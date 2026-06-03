//! Sentinel Daemon — ECS Orchestrator Binary.
//!
//! Composition Root: Fuegt alle 15 Library-Crates zu einem
//! laufenden Daemon zusammen. Tick-basierte ECS-Simulation
//! mit async I/O Bridge (Zenoh, Limbo, redb).

use std::path::Path;
use std::process::ExitCode;

use anyhow::Context;
use clap::Parser;
use tracing::{error, info, info_span};

use sentinel_daemon::config::DaemonConfig;

/// Sentinel Daemon — ECS Orchestrator fuer die Agent-Simulation.
#[derive(Parser, Debug)]
#[command(name = "sentinel-daemon", version, about)]
struct Cli {
    /// Pfad zur Daemon-Konfigurationsdatei.
    #[arg(long, default_value = "config/daemon.toml")]
    config: String,

    /// Dry-Run: Config laden, Agents parsen, Schicht erkennen, dann beenden.
    #[arg(long)]
    dry_run: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Runtime::new().context("Tokio Runtime erstellen") {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("Daemon fehlgeschlagen: {e:#}");
            return ExitCode::from(1);
        }
    };
    let observability = match runtime
        .block_on(async { sentinel_telemetry::init_observability("sentinel-daemon") })
    {
        Ok(guard) => guard,
        Err(e) => {
            eprintln!("Observability-Initialisierung fehlgeschlagen: {e}");
            return ExitCode::from(1);
        }
    };
    let otlp_enabled = observability.otlp_enabled();

    {
        let span = info_span!(
            "sentinel_daemon.bootstrap",
            service = "sentinel-daemon",
            otlp_enabled
        );
        let _entered = span.enter();
        info!("Observability initialisiert");
    }

    match run(cli, &runtime) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!(error = %e, "Daemon fehlgeschlagen");
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli, runtime: &tokio::runtime::Runtime) -> anyhow::Result<()> {
    let config = {
        let span = info_span!("sentinel_daemon.config_load", config_path = %cli.config);
        let _entered = span.enter();
        DaemonConfig::load(Path::new(&cli.config))
            .with_context(|| format!("Config laden: {}", cli.config))?
    };

    info!(
        config_dir = %config.config_dir.display(),
        data_dir = %config.data_dir.display(),
        tick_rate_ms = config.tick_rate_ms,
        max_agents = config.max_agents,
        "Konfiguration geladen"
    );

    if cli.dry_run {
        return dry_run(&config);
    }

    runtime.block_on(sentinel_daemon::orchestrator::run(config))
}

/// Dry-Run: Config validieren, Agents laden, Schicht erkennen, beenden.
fn dry_run(config: &DaemonConfig) -> anyhow::Result<()> {
    use sentinel_common::agent_config::load_all_agents_with_validation;
    use sentinel_daemon::shift::{agents_for_shift, detect_current_shift};

    let agents_dir = config.config_dir.join("agents");
    let validation = config.agent_config_validation()?;
    let all_agents = load_all_agents_with_validation(&agents_dir, validation)
        .with_context(|| format!("Agents laden: {}", agents_dir.display()))?;

    let current_shift = detect_current_shift();
    let active = agents_for_shift(&all_agents, current_shift);

    info!(
        total_agents = all_agents.len(),
        current_shift = current_shift,
        active_agents = active.len(),
        "Dry-Run abgeschlossen"
    );

    for agent in &active {
        info!(
            id = agent.identity.id,
            name = %agent.identity.name,
            role = %agent.identity.role,
            shift_set = agent.identity.shift_set,
            "Agent aktiv"
        );
    }

    Ok(())
}
