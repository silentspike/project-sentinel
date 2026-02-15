//! Acceptance tests for Issue #23: sentinel-hippocampus persistence.
//!
//! AC-1: Data survives process restart (write → drop → reopen → read)
//! AC-2: Night-run consolidation produces persistent narratives
//! AC-3: Retrieval returns episodes sorted by NMDA score
//! AC-4: No in-memory-only fallback — HippocampusService requires DB path

use sentinel_hippocampus::{Episode, HippocampusService, HippocampusStore, NarrativeState};

fn make_episode(
    id: u64,
    agent: &str,
    summary: &str,
    relevance: f64,
    emotion: f64,
    repetitions: u32,
    hours_ago: f64,
) -> Episode {
    Episode {
        id,
        agent_name: agent.to_string(),
        summary: summary.to_string(),
        relevance,
        emotion,
        repetitions,
        hours_ago,
        participants: vec![],
        tags: vec![],
    }
}

/// AC-1: Data persists across process restarts.
///
/// Writes episodes, facts, narratives, and cache state to redb,
/// drops the store, reopens, and verifies all data is intact.
#[test]
fn ac_23_01_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ac01.redb");
    let path_str = path.to_str().unwrap();

    // Phase 1: Write data
    {
        let store = HippocampusStore::open(path_str).unwrap();
        store
            .store_episodes(
                "Thomas",
                &[
                    make_episode(1, "Thomas", "Wichtiges Meeting", 0.9, 0.8, 2, 1.0),
                    make_episode(2, "Thomas", "Kundengespraech", 0.7, 0.6, 1, 2.0),
                ],
            )
            .unwrap();
        store
            .store_fact("facts/projects/aurora", "Projekt Aurora: Webseite")
            .unwrap();
        store
            .store_narrative(
                "Thomas",
                &NarrativeState {
                    agent_name: "Thomas".to_string(),
                    summary: "- Meeting mit Kunde (Score: 0.56)".to_string(),
                    episode_count: 2,
                },
            )
            .unwrap();
        store.store_cache_state("Thomas", true).unwrap();
    } // Store dropped — simulates process exit

    // Phase 2: Reopen and verify
    {
        let store = HippocampusStore::open(path_str).unwrap();

        // Episodes persist
        let episodes = store.load_episodes("Thomas").unwrap();
        assert_eq!(episodes.len(), 2, "AC-1: Episodes must survive restart");
        assert_eq!(episodes[0].summary, "Wichtiges Meeting");
        assert_eq!(episodes[1].summary, "Kundengespraech");

        // Facts persist
        let fact = store.load_fact("facts/projects/aurora").unwrap();
        assert_eq!(
            fact.unwrap(),
            "Projekt Aurora: Webseite",
            "AC-1: Facts must survive restart"
        );

        // Narrative persists
        let narrative = store.load_narrative("Thomas").unwrap().unwrap();
        assert_eq!(narrative.episode_count, 2, "AC-1: Narrative must survive");
        assert!(narrative.summary.contains("Meeting"));

        // Cache state persists
        assert_eq!(
            store.load_cache_state("Thomas").unwrap(),
            Some(true),
            "AC-1: Cache state must survive"
        );
    }
}

/// AC-2: Night-run consolidation produces persistent narratives.
///
/// Records episodes via HippocampusService, runs consolidation,
/// drops service, reopens, and verifies narrative was persisted.
#[test]
fn ac_23_02_consolidation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ac02.redb");
    let path_str = path.to_str().unwrap();

    // Phase 1: Record and consolidate
    let consolidated_count;
    {
        let service = HippocampusService::open(path_str).unwrap();

        // Record a day's worth of episodes
        let episodes = vec![
            make_episode(1, "Thomas", "Wichtiges Strategiemeeting", 0.95, 0.9, 2, 0.5),
            make_episode(2, "Thomas", "Kaffee in der Kueche", 0.1, 0.05, 1, 3.0),
            make_episode(3, "Thomas", "Konflikt mit Lieferant", 0.9, 0.85, 1, 1.0),
            make_episode(4, "Thomas", "E-Mail beantwortet", 0.2, 0.1, 1, 4.0),
        ];
        service.record_episodes("Thomas", &episodes).unwrap();

        // Run night consolidation
        let result = service.consolidate_agent("Thomas").unwrap();
        assert_eq!(result.episodes_processed, 4);
        assert!(
            result.episodes_consolidated > 0,
            "AC-2: Must consolidate at least one episode"
        );
        consolidated_count = result.episodes_consolidated;

        // Episodes should be cleared after consolidation
        let remaining = service.store().load_episodes("Thomas").unwrap();
        assert!(
            remaining.is_empty(),
            "AC-2: Episodes must be cleared after consolidation"
        );
    } // Service dropped

    // Phase 2: Reopen and verify narrative persisted
    {
        let service = HippocampusService::open(path_str).unwrap();
        let narrative = service
            .get_narrative("Thomas")
            .unwrap()
            .expect("AC-2: Narrative must be persisted after consolidation");

        assert!(
            !narrative.is_empty(),
            "AC-2: Narrative must contain content"
        );
        assert!(
            narrative.contains("Score:"),
            "AC-2: Narrative must contain scores"
        );

        // Verify via store directly
        let state = service.store().load_narrative("Thomas").unwrap().unwrap();
        assert_eq!(
            state.episode_count, consolidated_count,
            "AC-2: Episode count must match consolidation result"
        );
    }
}

/// AC-3: Retrieval returns episodes sorted by NMDA score (highest first).
#[test]
fn ac_23_03_retrieval_priority() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ac03.redb");
    let service = HippocampusService::open(path.to_str().unwrap()).unwrap();

    // Record episodes with known different scores
    let episodes = vec![
        // Low: 0.1 * 0.1 * 1 * (1/6) = 0.00166...
        make_episode(1, "Thomas", "Routine Kaffee", 0.1, 0.1, 1, 5.0),
        // High: 0.95 * 0.9 * 3 * (1/1.1) = 2.331...
        make_episode(2, "Thomas", "Kritischer Vorfall", 0.95, 0.9, 3, 0.1),
        // Medium: 0.5 * 0.5 * 1 * (1/2) = 0.125
        make_episode(3, "Thomas", "Normales Meeting", 0.5, 0.5, 1, 1.0),
    ];
    service.record_episodes("Thomas", &episodes).unwrap();

    // Retrieve with priority ordering
    let memories = service.retrieve_memories("Thomas", 10).unwrap();

    assert_eq!(memories.len(), 3, "AC-3: All episodes should be returned");
    assert_eq!(
        memories[0].0.summary, "Kritischer Vorfall",
        "AC-3: Highest score must be first"
    );
    assert_eq!(
        memories[2].0.summary, "Routine Kaffee",
        "AC-3: Lowest score must be last"
    );

    // Verify strict descending order
    for i in 0..memories.len() - 1 {
        assert!(
            memories[i].1 >= memories[i + 1].1,
            "AC-3: Scores must be in descending order: {} >= {} failed",
            memories[i].1,
            memories[i + 1].1
        );
    }
}

/// AC-4: No in-memory-only fallback — service requires a DB path.
///
/// HippocampusService::open() requires a valid path.
/// There is no constructor that works without persistence.
#[test]
fn ac_23_04_no_inmemory_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ac04.redb");
    let path_str = path.to_str().unwrap();

    // Phase 1: Service MUST be created with a DB path, write data
    {
        let service = HippocampusService::open(path_str).unwrap();

        // Record + retrieve to prove it's using the DB
        let ep = make_episode(1, "Thomas", "Persistence test", 0.9, 0.8, 1, 0.5);
        service.record_episode(ep).unwrap();

        // Verify data is on disk (not just in memory)
        assert!(
            path.exists(),
            "AC-4: Database file must exist on disk at {:?}",
            path
        );

        let file_size = std::fs::metadata(&path).unwrap().len();
        assert!(
            file_size > 0,
            "AC-4: Database file must have content (size: {file_size})"
        );
    } // service dropped, releasing file lock

    // Phase 2: Reopen independently to prove it's not in-memory
    {
        let store2 = HippocampusStore::open(path_str).unwrap();
        let loaded = store2.load_episodes("Thomas").unwrap();
        assert_eq!(
            loaded.len(),
            1,
            "AC-4: Data must be readable from independent store instance"
        );
    }
}
