//! Sentinel Projection Worker — CQRS Read-Model Service.
//!
//! Long-running service that polls the EventStore and maintains
//! materialized read models (agent_live_view, room_live_view, kpi_1m).
//! Started via systemd, runs continuously until stopped.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use tracing::{error, info};

use sentinel_limbo::EventStore;
use sentinel_projection::{ProjectionConfig, ProjectionWorker};

/// Sentinel Projection Worker — CQRS Read-Model Service.
#[derive(Parser, Debug)]
#[command(name = "sentinel-projection", version, about)]
struct Cli {
    /// Pfad zur EventStore-Datenbank.
    #[arg(long, default_value = "/opt/sentinel/data/events.db")]
    event_store: String,

    /// Pfad zur Projection-Datenbank (wird erstellt falls nicht vorhanden).
    #[arg(long, default_value = "/opt/sentinel/data/projection.db")]
    projection_db: String,

    /// Poll-Intervall in Millisekunden.
    #[arg(long, default_value = "50")]
    poll_interval_ms: u64,

    /// Batch-Groesse pro Poll-Zyklus.
    #[arg(long, default_value = "100")]
    batch_size: usize,

    /// Full Rebuild: Alle Views loeschen und alle Events neu verarbeiten.
    #[arg(long)]
    rebuild: bool,
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!("Projection worker failed: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    info!(
        event_store = %cli.event_store,
        projection_db = %cli.projection_db,
        poll_interval_ms = cli.poll_interval_ms,
        batch_size = cli.batch_size,
        rebuild = cli.rebuild,
        "Starting sentinel-projection worker"
    );

    let event_store = Arc::new(
        EventStore::open(&cli.event_store)
            .with_context(|| format!("Failed to open EventStore: {}", cli.event_store))?,
    );

    let config = ProjectionConfig {
        poll_interval: std::time::Duration::from_millis(cli.poll_interval_ms),
        batch_size: cli.batch_size,
        db_path: cli.projection_db.clone(),
        rebuild_request_path: Path::new(&cli.projection_db)
            .parent()
            .unwrap_or_else(|| Path::new("/opt/sentinel/data"))
            .join(".projection-rebuild-request")
            .to_string_lossy()
            .to_string(),
        rebuild_request_poll_interval: std::time::Duration::from_secs(1),
    };

    let worker =
        ProjectionWorker::new(event_store, config).context("Failed to create ProjectionWorker")?;

    if cli.rebuild {
        info!("Running full rebuild...");
        let count = worker.rebuild().context("Rebuild failed")?;
        info!(events = count, "Rebuild complete");
        return Ok(());
    }

    info!("Entering live poll loop");
    worker.run().context("Poll loop failed")?;

    Ok(())
}
