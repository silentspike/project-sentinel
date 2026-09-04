//! Sentinel Nightrun — Schichtwechsel-Konsolidierung.
//!
//! Run-to-completion Service, getriggert via systemd Timer bei Schichtwechsel.
//! Konsolidiert episodische Erinnerungen der abgehenden Agents per NMDA-Scoring.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{error, info, warn};

use sentinel_nightrun::config::NightrunConfig;
use sentinel_nightrun::job_queue::JobQueue;
use sentinel_nightrun::replay::ReplayEngine;
use sentinel_nightrun::runner::NightrunSelectionMetrics;
use sentinel_nightrun::runner::{NightrunResult, NightrunRunner};
use sentinel_nightrun::shift::{current_shift_set, outgoing_shift_set};

use sentinel_hippocampus::HippocampusService;
use sentinel_limbo::EventStore;

/// Sentinel Nightrun — Schichtwechsel-Konsolidierung.
#[derive(Parser, Debug)]
#[command(name = "sentinel-nightrun", version, about)]
struct Cli {
    /// Pfad zur Konfigurations-Datei.
    #[arg(long, default_value = "config/nightrun.toml")]
    config: String,

    /// Expliziter Shift-Set (1=Frueh, 2=Mittel, 3=Spaet). Auto-detect wenn nicht gesetzt.
    #[arg(long)]
    shift: Option<u8>,

    /// Dry-Run: Agents auflisten aber nicht konsolidieren.
    #[arg(long)]
    dry_run: bool,

    /// Unvollstaendigen Run fortsetzen statt neuen zu starten.
    #[arg(long)]
    resume: bool,

    /// Einen abgeschlossenen Run read-only replayen statt neu zu konsolidieren.
    #[arg(long)]
    replay_run_id: Option<String>,

    /// Erwarteter Hash fuer Replay. Wenn nicht gesetzt, wird nightrun_completed.hash_chain genutzt.
    #[arg(long)]
    expected_hash: Option<String>,

    /// Hash-Seed fuer Replay. Default ist die Run-ID.
    #[arg(long)]
    seed: Option<String>,

    /// Ergebnis als JSON auf stdout ausgeben.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, serde::Serialize)]
#[serde(tag = "mode", content = "result")]
enum CommandOutcome {
    Nightrun(NightrunResult),
    Replay(sentinel_nightrun::replay::ReplayResult),
}

fn main() -> ExitCode {
    // Logging
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let json_output = cli.json;

    match run(cli) {
        Ok(CommandOutcome::Nightrun(result)) => {
            if json_output {
                if let Err(e) = print_json(&CommandOutcome::Nightrun(result.clone())) {
                    error!(error = %e, "JSON-Ausgabe fehlgeschlagen");
                    return ExitCode::from(2);
                }
            }

            if result.agents_failed > 0 {
                error!(
                    run_id = %result.run_id,
                    failed = result.agents_failed,
                    consolidated = result.agents_consolidated,
                    "Nightrun mit Fehlern abgeschlossen"
                );
                ExitCode::from(1)
            } else {
                info!(
                    run_id = %result.run_id,
                    consolidated = result.agents_consolidated,
                    skipped = result.agents_skipped,
                    total_episodes = result.total_episodes,
                    total_episodes_consolidated = result.total_episodes_consolidated,
                    selection_rate = format!("{:.3}", result.selection.selection_rate),
                    duration_ms = result.duration_ms,
                    "Nightrun erfolgreich abgeschlossen"
                );
                ExitCode::SUCCESS
            }
        }
        Ok(CommandOutcome::Replay(result)) => {
            if json_output {
                if let Err(e) = print_json(&CommandOutcome::Replay(result.clone())) {
                    error!(error = %e, "JSON-Ausgabe fehlgeschlagen");
                    return ExitCode::from(2);
                }
            }

            if result.hash_chain_valid {
                info!(
                    run_id = %result.run_id,
                    events_loaded = result.events_loaded,
                    events_replayed = result.events_replayed,
                    hash = %result.final_hash,
                    "Replay erfolgreich"
                );
                ExitCode::SUCCESS
            } else {
                error!(
                    run_id = %result.run_id,
                    expected = %result.expected_hash,
                    actual = %result.final_hash,
                    "Replay-Hash stimmt nicht ueberein"
                );
                ExitCode::from(1)
            }
        }
        Err(e) => {
            error!(error = %e, "Nightrun fehlgeschlagen");
            ExitCode::from(2)
        }
    }
}

fn print_json(outcome: &CommandOutcome) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(outcome)?);
    Ok(())
}

fn run(cli: Cli) -> Result<CommandOutcome> {
    // Config
    let config = NightrunConfig::load(Path::new(&cli.config))
        .with_context(|| format!("Config laden fehlgeschlagen: {}", cli.config))?;
    let settings = config.nightrun;

    if let Some(run_id) = cli.replay_run_id.as_deref() {
        let event_store = EventStore::open_compatible(&settings.event_store_db)
            .context("Failed to open EventStore")?;
        let replay = ReplayEngine::new(&event_store);
        let expected_hash = match cli.expected_hash {
            Some(hash) => hash,
            None => replay.expected_hash_from_completed(run_id)?,
        };
        let seed = cli.seed.as_deref().unwrap_or(run_id);
        return replay
            .replay(run_id, seed, &expected_hash)
            .map(CommandOutcome::Replay);
    }

    // Runtime Feature Flags (Issue #233 AC-4)
    let flags = sentinel_common::feature_flags::RuntimeFlags::init();
    if !flags.nightrun_enabled {
        warn!("SENTINEL_NIGHTRUN_ENABLED=false — Nightrun uebersprungen");
        return Ok(CommandOutcome::Nightrun(NightrunResult {
            run_id: uuid::Uuid::new_v4().to_string(),
            agents_consolidated: 0,
            agents_failed: 0,
            agents_skipped: 0,
            total_episodes: 0,
            total_episodes_consolidated: 0,
            selection: NightrunSelectionMetrics::empty(),
            duration_ms: 0,
            hash_chain_final: String::new(),
        }));
    }

    // Shift-Set bestimmen
    let current = current_shift_set();
    let trigger_shift_set = cli.shift.unwrap_or_else(|| outgoing_shift_set(current));

    info!(
        current_shift = current,
        trigger_shift = trigger_shift_set,
        explicit = cli.shift.is_some(),
        "Schicht-Erkennung"
    );

    // Services oeffnen — HippocampusService kann fehlschlagen wenn der Daemon
    // den redb Lock haelt. In dem Fall ueberspringt Night-Run die
    // Hippocampus-Konsolidierung (Daemon erledigt sie beim Schichtwechsel).
    let hippocampus = match HippocampusService::open(&settings.hippocampus_db) {
        Ok(h) => h,
        Err(e) => {
            warn!(
                error = %e,
                "HippocampusService nicht oeffenbar (Daemon haelt Lock?) — \
                 Konsolidierung wird vom Daemon beim Schichtwechsel durchgefuehrt"
            );
            info!("Nightrun beendet (Daemon-Konsolidierung aktiv)");
            return Ok(CommandOutcome::Nightrun(NightrunResult {
                run_id: uuid::Uuid::new_v4().to_string(),
                agents_consolidated: 0,
                agents_failed: 0,
                agents_skipped: 0,
                total_episodes: 0,
                total_episodes_consolidated: 0,
                selection: NightrunSelectionMetrics::empty(),
                duration_ms: 0,
                hash_chain_final: String::new(),
            }));
        }
    };

    let event_store = EventStore::open_compatible(&settings.event_store_db)
        .context("Failed to open EventStore")?;

    let job_queue = JobQueue::open(&settings.job_queue_path).context("Failed to open JobQueue")?;

    // Run-ID bestimmen (resume oder neu)
    let run_id = if cli.resume {
        match job_queue.get_incomplete_run()? {
            Some(id) => {
                info!(run_id = %id, "Fortsetze unvollstaendigen Run");
                id
            }
            None => {
                info!("Kein unvollstaendiger Run gefunden, starte neuen");
                uuid::Uuid::new_v4().to_string()
            }
        }
    } else {
        uuid::Uuid::new_v4().to_string()
    };

    info!(run_id = %run_id, dry_run = cli.dry_run, "Nightrun initialisiert");

    // Runner ausfuehren
    let runner = NightrunRunner::new(
        hippocampus,
        event_store,
        job_queue,
        settings,
        run_id,
        cli.dry_run,
    );

    runner.run(trigger_shift_set).map(CommandOutcome::Nightrun)
}
