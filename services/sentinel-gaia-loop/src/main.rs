use anyhow::Result;
use clap::{Parser, Subcommand};
use sentinel_gaia_loop::config::GaiaLoopConfig;

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
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Config { json } => print_config(json),
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
    }
    Ok(())
}
