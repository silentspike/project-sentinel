use anyhow::Result;
use clap::{Parser, Subcommand};
use sentinel_gaia_loop::config::GaiaLoopConfig;
use sentinel_gaia_loop::readiness;
use sentinel_gaia_loop::session::{ClaudeSessionRunner, GaiaSessionRequest};

#[derive(Debug, Parser)]
#[command(
    name = "sentinel-gaia-loop",
    version,
    about = "Reactive Gaia Console readiness loop and Claude Code session bridge (#442)"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Print the effective Gaia Console runtime config.
    Config {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Scan events.db once for operator escalations and persist console alerts.
    ScanOnce {
        /// Maximum number of events to read after the stored cursor.
        #[arg(long, default_value_t = 1000)]
        limit: usize,
    },
    /// Run the token-light readiness watcher. This never spawns Claude.
    Serve,
    /// Start one explicit deep Claude Code session for an operator task.
    Deep {
        /// Operator task prompt for this single Claude turn.
        #[arg(long)]
        prompt: String,
        /// Existing Claude session id for a follow-up turn.
        #[arg(long)]
        resume: Option<String>,
    },
    /// Start one explicit setup-interview Claude Code session.
    SetupInterview {
        /// Setup request prompt for this single Claude turn.
        #[arg(long)]
        prompt: String,
        /// Existing Claude session id for a follow-up turn.
        #[arg(long)]
        resume: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Commands::Config { json } => print_config(json),
        Commands::ScanOnce { limit } => scan_once(limit),
        Commands::Serve => serve().await,
        Commands::Deep { prompt, resume } => {
            run_session(GaiaSessionRequest::deep(prompt, resume)).await
        }
        Commands::SetupInterview { prompt, resume } => {
            run_session(GaiaSessionRequest::setup_interview(prompt, resume)).await
        }
    }
}

fn print_config(json: bool) -> Result<()> {
    let cfg = GaiaLoopConfig::from_env()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&cfg)?);
    } else {
        println!("console_dir={}", cfg.console_dir.display());
        println!("events_db={}", cfg.events_db.display());
        println!("nats_url={}", cfg.nats_url);
        println!("http_bind={}", cfg.http_bind);
        println!("claude_bin={}", cfg.claude_bin.display());
        println!("sentinel_ctl_bin={}", cfg.sentinel_ctl_bin.display());
        println!("sentinel_gaia_bin={}", cfg.sentinel_gaia_bin.display());
        println!("model={}", cfg.model.as_deref().unwrap_or(""));
        println!("max_budget_usd={}", cfg.max_budget_usd);
        println!("session_timeout_secs={}", cfg.session_timeout_secs);
        println!("max_turns={}", cfg.max_turns);
        println!(
            "readiness_scan_interval_secs={}",
            cfg.readiness_scan_interval_secs
        );
    }
    Ok(())
}

fn scan_once(limit: usize) -> Result<()> {
    let cfg = GaiaLoopConfig::from_env()?;
    let summary = readiness::scan_once(&cfg, limit)?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

async fn serve() -> Result<()> {
    let cfg = GaiaLoopConfig::from_env()?;
    readiness::run_readiness_loop(cfg).await
}

async fn run_session(request: GaiaSessionRequest) -> Result<()> {
    let cfg = GaiaLoopConfig::from_env()?;
    let runner = ClaudeSessionRunner::new(cfg);
    let run = runner.run(request).await?;
    println!("{}", serde_json::to_string_pretty(&run)?);
    Ok(())
}
