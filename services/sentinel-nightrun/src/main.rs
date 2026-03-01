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
}

fn main() -> ExitCode {
    // Logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match run(cli) {
        Ok(result) => {
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
                    duration_ms = result.duration_ms,
                    "Nightrun erfolgreich abgeschlossen"
                );
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            error!(error = %e, "Nightrun fehlgeschlagen");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> Result<sentinel_nightrun::runner::NightrunResult> {
    // Config
    let config = NightrunConfig::load(Path::new(&cli.config))
        .with_context(|| format!("Config laden fehlgeschlagen: {}", cli.config))?;
    let settings = config.nightrun;

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
            return Ok(NightrunResult {
                run_id: uuid::Uuid::new_v4().to_string(),
                agents_consolidated: 0,
                agents_failed: 0,
                agents_skipped: 0,
                total_episodes: 0,
                duration_ms: 0,
                hash_chain_final: String::new(),
            });
        }
    };

    let event_store =
        EventStore::open(&settings.event_store_db).context("Failed to open EventStore")?;

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

    runner.run(trigger_shift_set)
}
