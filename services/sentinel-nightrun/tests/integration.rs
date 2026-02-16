//! Integration-Tests fuer den Nightrun-Runner.
//!
//! Testet die gesamte Pipeline end-to-end:
//! HippocampusService + EventStore + JobQueue + Runner

use sentinel_hippocampus::{Episode, HippocampusService};
use sentinel_limbo::EventStore;
use sentinel_nightrun::config::NightrunSettings;
use sentinel_nightrun::job_queue::JobQueue;
use sentinel_nightrun::runner::NightrunRunner;

fn make_episode(id: u64, agent: &str, summary: &str, relevance: f64, emotion: f64) -> Episode {
    Episode {
        id,
        agent_name: agent.to_string(),
        summary: summary.to_string(),
        relevance,
        emotion,
        repetitions: 1,
        hours_ago: 1.0,
        participants: vec![],
        tags: vec![],
    }
}

struct TestHarness {
    hippocampus: HippocampusService,
    event_store: EventStore,
    job_queue: JobQueue,
    settings: NightrunSettings,
    _dir: tempfile::TempDir,
}

impl TestHarness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();

        let hc_path = dir.path().join("hippocampus.redb");
        let es_path = dir.path().join("events.db");
        let jq_path = dir.path().join("nightrun-jobs.db");

        let hippocampus = HippocampusService::open(hc_path.to_str().unwrap()).unwrap();
        let event_store = EventStore::open(es_path.to_str().unwrap()).unwrap();
        let job_queue = JobQueue::open(jq_path.to_str().unwrap()).unwrap();

        let settings = NightrunSettings {
            hippocampus_db: hc_path.to_str().unwrap().to_string(),
            event_store_db: es_path.to_str().unwrap().to_string(),
            agent_config_dir: dir.path().to_str().unwrap().to_string(),
            job_queue_path: jq_path.to_str().unwrap().to_string(),
            timeout_per_agent_secs: 300,
            timeout_total_secs: 7200,
            max_episodes_per_agent: 1000,
        };

        Self {
            hippocampus,
            event_store,
            job_queue,
            settings,
            _dir: dir,
        }
    }

    fn seed_agent(&self, name: &str, episode_count: usize) {
        let episodes: Vec<Episode> = (0..episode_count)
            .map(|i| make_episode(i as u64, name, &format!("Event {i} von {name}"), 0.8, 0.7))
            .collect();
        self.hippocampus.record_episodes(name, &episodes).unwrap();
    }

    fn runner(self, run_id: &str, dry_run: bool) -> (NightrunRunner, EventStore, tempfile::TempDir) {
        let event_store_check = EventStore::open(&self.settings.event_store_db).unwrap();
        let runner = NightrunRunner::new(
            self.hippocampus,
            self.event_store,
            self.job_queue,
            self.settings,
            run_id.to_string(),
            dry_run,
        );
        (runner, event_store_check, self._dir)
    }
}

// === AC-1: Service startet robust bei Schichtwechsel ===

#[test]
fn ac1_consolidation_completes_for_all_agents() {
    let h = TestHarness::new();
    h.seed_agent("Thomas", 5);
    h.seed_agent("Lisa", 3);
    h.seed_agent("Max", 8);

    let (runner, _es, _dir) = h.runner("run-ac1", false);
    let result = runner.run(1).unwrap();

    assert_eq!(result.agents_consolidated, 3);
    assert_eq!(result.agents_failed, 0);
    assert_eq!(result.agents_skipped, 0);
    assert!(result.total_episodes > 0);
}

// === AC-2: Konsolidierte Daten persistent ===

#[test]
fn ac2_narratives_persist_after_consolidation() {
    let dir = tempfile::tempdir().unwrap();
    let hc_path = dir.path().join("hc.redb");

    // Phase 1: Consolidate
    {
        let hc = HippocampusService::open(hc_path.to_str().unwrap()).unwrap();
        let es = EventStore::open(dir.path().join("ev.db").to_str().unwrap()).unwrap();
        let jq = JobQueue::open(dir.path().join("jq.db").to_str().unwrap()).unwrap();
        let settings = NightrunSettings {
            hippocampus_db: hc_path.to_str().unwrap().to_string(),
            event_store_db: dir.path().join("ev.db").to_str().unwrap().to_string(),
            agent_config_dir: dir.path().to_str().unwrap().to_string(),
            job_queue_path: dir.path().join("jq.db").to_str().unwrap().to_string(),
            timeout_per_agent_secs: 300,
            timeout_total_secs: 7200,
            max_episodes_per_agent: 1000,
        };

        let eps = vec![
            make_episode(1, "Thomas", "Wichtiges Meeting", 0.9, 0.8),
            make_episode(2, "Thomas", "Konflikt geloest", 0.95, 0.9),
        ];
        hc.record_episodes("Thomas", &eps).unwrap();

        let runner = NightrunRunner::new(hc, es, jq, settings, "run-ac2".into(), false);
        runner.run(1).unwrap();
    }

    // Phase 2: Reopen und pruefen
    {
        let hc = HippocampusService::open(hc_path.to_str().unwrap()).unwrap();
        let narrative = hc.get_narrative("Thomas").unwrap();
        assert!(narrative.is_some(), "Narrative muss persistent sein");
        assert!(!narrative.unwrap().is_empty());

        // Episodes muessen geloescht sein
        let remaining = hc.store().load_episodes("Thomas").unwrap();
        assert!(remaining.is_empty(), "Episodes muessen nach Konsolidierung geloescht sein");
    }
}

// === AC-3: Keine Gewichtsdateien erzeugt (architekturbedingt) ===

#[test]
fn ac3_no_weight_files_created() {
    let h = TestHarness::new();

    h.seed_agent("Thomas", 5);
    let (runner, _es, dir) = h.runner("run-ac3", false);
    let dir_path = dir.path().to_path_buf();
    runner.run(1).unwrap();

    // Keine .bin, .safetensors, .pt, .gguf Dateien
    let weight_extensions = ["bin", "safetensors", "pt", "gguf", "onnx"];
    for entry in std::fs::read_dir(&dir_path).unwrap() {
        let path = entry.unwrap().path();
        if let Some(ext) = path.extension() {
            assert!(
                !weight_extensions.contains(&ext.to_str().unwrap_or("")),
                "Gewichtsdatei gefunden: {}", path.display()
            );
        }
    }
}

// === AC-4: Laufzeit-Guardrails (Timeout-Enforcement) ===

#[test]
fn ac4_backlog_skip_enforced() {
    let h = TestHarness::new();
    // Seed Agent mit mehr Episodes als max_episodes_per_agent
    let mut settings = h.settings.clone();
    settings.max_episodes_per_agent = 3; // Niedrig setzen

    let dir = tempfile::tempdir().unwrap();
    let hc_path = dir.path().join("hc.redb");
    let es_path = dir.path().join("ev.db");
    let jq_path = dir.path().join("jq.db");

    let hc = HippocampusService::open(hc_path.to_str().unwrap()).unwrap();
    let es = EventStore::open(es_path.to_str().unwrap()).unwrap();
    let jq = JobQueue::open(jq_path.to_str().unwrap()).unwrap();

    // 10 Episodes > max 3
    let episodes: Vec<Episode> = (0..10)
        .map(|i| make_episode(i, "Backlog-Agent", &format!("Event {i}"), 0.5, 0.5))
        .collect();
    hc.record_episodes("Backlog-Agent", &episodes).unwrap();

    settings.hippocampus_db = hc_path.to_str().unwrap().to_string();
    settings.event_store_db = es_path.to_str().unwrap().to_string();
    settings.agent_config_dir = dir.path().to_str().unwrap().to_string();
    settings.job_queue_path = jq_path.to_str().unwrap().to_string();

    let runner = NightrunRunner::new(hc, es, jq, settings, "run-ac4".into(), false);
    let result = runner.run(1).unwrap();

    assert_eq!(result.agents_skipped, 1, "Agent mit zu grossem Backlog muss uebersprungen werden");
    assert_eq!(result.agents_consolidated, 0);
}

// === Resume nach Crash ===

#[test]
fn resume_continues_partial_run() {
    let dir = tempfile::tempdir().unwrap();
    let hc_path = dir.path().join("hc.redb");
    let es_path = dir.path().join("ev.db");
    let jq_path = dir.path().join("jq.db");

    // Phase 1: Starte Run, konsolidiere nur einen Agent
    {
        let hc = HippocampusService::open(hc_path.to_str().unwrap()).unwrap();
        hc.record_episodes("A", &[make_episode(1, "A", "Event A", 0.8, 0.7)])
            .unwrap();
        hc.record_episodes("B", &[make_episode(2, "B", "Event B", 0.8, 0.7)])
            .unwrap();

        let jq = JobQueue::open(jq_path.to_str().unwrap()).unwrap();
        jq.create_run("run-resume", &["A".into(), "B".into()])
            .unwrap();
        // Simuliere: A wurde konsolidiert, B ist noch pending
        jq.mark_in_progress("run-resume", "A").unwrap();
        jq.mark_completed("run-resume", "A", 1, 1).unwrap();
    }

    // Phase 2: Resume — B sollte jetzt konsolidiert werden
    {
        let hc = HippocampusService::open(hc_path.to_str().unwrap()).unwrap();
        let es = EventStore::open(es_path.to_str().unwrap()).unwrap();
        let jq = JobQueue::open(jq_path.to_str().unwrap()).unwrap();

        // Prüfe dass incomplete run gefunden wird
        let incomplete = jq.get_incomplete_run().unwrap();
        assert_eq!(incomplete.as_deref(), Some("run-resume"));

        let settings = NightrunSettings {
            hippocampus_db: hc_path.to_str().unwrap().to_string(),
            event_store_db: es_path.to_str().unwrap().to_string(),
            agent_config_dir: dir.path().to_str().unwrap().to_string(),
            job_queue_path: jq_path.to_str().unwrap().to_string(),
            timeout_per_agent_secs: 300,
            timeout_total_secs: 7200,
            max_episodes_per_agent: 1000,
        };

        let runner = NightrunRunner::new(hc, es, jq, settings, "run-resume".into(), false);
        let result = runner.run(1).unwrap();

        // Nur B sollte konsolidiert worden sein (A war schon done)
        assert_eq!(result.agents_consolidated, 1);
        assert_eq!(result.agents_failed, 0);
    }
}

// === Dry-Run ===

#[test]
fn dry_run_skips_all_agents() {
    let h = TestHarness::new();
    h.seed_agent("Thomas", 5);
    h.seed_agent("Lisa", 3);

    let (runner, _es, _dir) = h.runner("run-dry", true);
    let result = runner.run(1).unwrap();

    assert_eq!(result.agents_consolidated, 0);
    assert_eq!(result.agents_skipped, 2);

    // Episodes muessen noch vorhanden sein
    // (Hippocampus wurde an den Runner uebergeben, also koennen wir hier nicht pruefen.
    // Aber dry_run soll nicht konsolidieren — das prueft der skipped count.)
}

// === Leerer Run (keine Episodes) ===

#[test]
fn empty_run_completes_gracefully() {
    let h = TestHarness::new();
    // Keine Episodes seeden
    let (runner, _es, _dir) = h.runner("run-empty", false);
    let result = runner.run(1).unwrap();

    assert_eq!(result.agents_consolidated, 0);
    assert_eq!(result.agents_failed, 0);
    assert_eq!(result.agents_skipped, 0);
    assert_eq!(result.total_episodes, 0);
}

// === Events werden emittiert ===

#[test]
fn events_emitted_to_event_store() {
    let h = TestHarness::new();
    h.seed_agent("Thomas", 5);

    let es_path = h.settings.event_store_db.clone();
    let (runner, _, _dir) = h.runner("run-events", false);
    runner.run(1).unwrap();

    // EventStore oeffnen und Events zaehlen
    let conn = rusqlite::Connection::open(&es_path).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE event_type LIKE 'nightrun_%' OR event_type LIKE 'agent_consol%'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    // Mindestens: NightRunStarted + AgentConsolidated + NightRunCompleted = 3
    assert!(count >= 3, "Mindestens 3 Nightrun-Events erwartet, gefunden: {count}");
}
