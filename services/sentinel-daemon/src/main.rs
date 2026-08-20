//! Sentinel Daemon — ECS Orchestrator Binary.
//!
//! Composition Root: Fuegt alle 15 Library-Crates zu einem
//! laufenden Daemon zusammen. Tick-basierte ECS-Simulation
//! mit async I/O Bridge (Zenoh, Limbo, redb).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;
use clap::{Parser, Subcommand};
use tracing::{error, info};

use sentinel_daemon::config::{
    DaemonConfig, CREDENTIALS_DIRECTORY_ENV, OPERATOR_CREDENTIAL_FILE_ENV,
};

/// Sentinel Daemon — ECS Orchestrator fuer die Agent-Simulation.
#[derive(Parser, Debug)]
#[command(name = "sentinel-daemon", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Pfad zur Daemon-Konfigurationsdatei.
    #[arg(long, default_value = "config/daemon.toml")]
    config: String,

    /// Dry-Run: Config laden, Agents parsen, Schicht erkennen, dann beenden.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Generate or read the persistent QUIC control identity and print its fingerprint.
    GenerateControlIdentity {
        #[arg(long)]
        alias: String,
        #[arg(long)]
        cert: PathBuf,
        #[arg(long)]
        key: PathBuf,
    },
    /// Explicitly initialize or validate durable episode projection control.
    InitializeEpisodeProjection,
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
            error!(error = ?e, "Daemon fehlgeschlagen");
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    if let Some(Command::GenerateControlIdentity { alias, cert, key }) = &cli.command {
        if let Some(parent) = cert.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Some(parent) = key.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let identity =
            sentinel_cluster_control::NodeCertificate::load_or_generate(cert, key, alias)?;
        println!("{}", identity.fingerprint());
        return Ok(());
    }

    let credential_path = std::env::var_os(OPERATOR_CREDENTIAL_FILE_ENV).map(PathBuf::from);
    let credentials_directory = std::env::var_os(CREDENTIALS_DIRECTORY_ENV).map(PathBuf::from);
    let config = load_runtime_config(
        Path::new(&cli.config),
        credentials_directory.as_deref(),
        credential_path.as_deref(),
    )
    .with_context(|| format!("Config laden: {}", cli.config))?;

    info!(
        config_dir = %config.config_dir.display(),
        data_dir = %config.data_dir.display(),
        tick_rate_ms = config.tick_rate_ms,
        max_agents = config.max_agents,
        "Konfiguration geladen"
    );

    if matches!(cli.command, Some(Command::InitializeEpisodeProjection)) {
        let receipt = initialize_episode_projection(&config)?;
        println!("{}", serde_json::to_string(&receipt)?);
        return Ok(());
    }

    // TOGAF Cluster 12 (#495): optionale Cluster-Identität. Ohne [daemon.cluster]
    // bleibt der Daemon im Single-Node-Modus (Verhalten unverändert).
    match &config.cluster {
        Some(cluster) => {
            let identity = sentinel_common::NodeIdentity::from_config(cluster);
            info!(
                node_id = %identity.node_id,
                alias = %identity.alias,
                cluster_id = %cluster.cluster_id,
                role = ?cluster.role(),
                lifecycle = ?cluster.initial_lifecycle(),
                boot_id = %identity.boot_id,
                "Cluster 12: Node-Identität geladen"
            );
        }
        None => info!("Cluster 12: keine [daemon.cluster] Section — Single-Node-Modus"),
    }

    if cli.dry_run {
        return dry_run(&config);
    }

    // Tokio Runtime fuer async I/O
    let runtime = tokio::runtime::Runtime::new().context("Tokio Runtime erstellen")?;
    runtime.block_on(sentinel_daemon::orchestrator::run(config))
}

fn initialize_episode_projection(
    config: &DaemonConfig,
) -> anyhow::Result<sentinel_daemon::episode_producer::EpisodeProjectionBootstrapReceipt> {
    use sentinel_common::agent_config::load_all_agents_with_validation;

    let agents_dir = config.config_dir.join("agents");
    let all_agents =
        load_all_agents_with_validation(&agents_dir, config.agent_config_validation()?)
            .with_context(|| format!("Agents laden: {}", agents_dir.display()))?;
    let agents = all_agents
        .iter()
        .map(|agent| (agent.identity.id, agent.identity.name.clone()))
        .collect::<Vec<_>>();
    let events_path = config.data_dir.join("events.db");
    let event_store = sentinel_limbo::EventStore::open(
        events_path.to_str().context("events.db Pfad nicht UTF-8")?,
    )
    .context("EventStore fuer Episode-Projection-Bootstrap oeffnen")?;
    let hippocampus_path = config.data_dir.join("hippocampus.redb");
    let hippocampus = sentinel_hippocampus::HippocampusService::open(
        hippocampus_path
            .to_str()
            .context("hippocampus.redb Pfad nicht UTF-8")?,
    )
    .context("Hippocampus fuer Episode-Projection-Bootstrap oeffnen")?;

    sentinel_daemon::episode_producer::initialize_episode_projection_bootstrap(
        hippocampus,
        &agents,
        &event_store,
        config.operator_api.shared_secret.as_deref(),
        config.tick_rate_ms,
    )
    .context("Episode-Projection explizit initialisieren")
}

fn load_runtime_config(
    config_path: &Path,
    credentials_directory: Option<&Path>,
    credential_path: Option<&Path>,
) -> anyhow::Result<DaemonConfig> {
    let mut config = DaemonConfig::load(config_path)?;
    config
        .bind_operator_credential(credentials_directory, credential_path)
        .context("Operator-API Credential validieren")?;
    Ok(config)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn safe_tempdir() -> tempfile::TempDir {
        let root = std::env::var_os("RUNNER_TEMP")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/work/tmp/project-sentinel"));
        fs::create_dir_all(&root).unwrap();
        tempfile::Builder::new()
            .prefix("operator-credential-main-")
            .tempdir_in(root)
            .unwrap()
    }

    #[test]
    fn runtime_config_fails_before_dry_run_or_start_without_operator_credential() {
        let directory = safe_tempdir();
        let config_path = directory.path().join("daemon.toml");
        fs::write(
            &config_path,
            "[daemon]\nconfig_dir = \"/opt/sentinel/config\"\ndata_dir = \"/opt/sentinel/data\"\n",
        )
        .unwrap();

        let error = load_runtime_config(&config_path, None, None).unwrap_err();
        assert!(error.to_string().contains("Credential validieren"));

        let credential_path = directory.path().join("operator-api");
        fs::write(&credential_path, b"0123456789abcdef0123456789abcdef").unwrap();
        fs::set_permissions(&credential_path, fs::Permissions::from_mode(0o400)).unwrap();
        let config =
            load_runtime_config(&config_path, Some(directory.path()), Some(&credential_path))
                .unwrap();
        assert!(config.operator_api.shared_secret.is_some());
    }
}
