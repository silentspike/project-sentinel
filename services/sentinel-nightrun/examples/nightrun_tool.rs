//! Nightrun Dev Tool — Episode Seeding + Narrative Verification.
//!
//! Test-Fixture fuer Issue #17 VM-Verifikation.
//! Wird NICHT deployed, nur fuer Verifikation genutzt.
//!
//! Usage:
//!   nightrun-tool seed --db data/hippocampus.redb --agents "Thomas Mueller,Lisa Brenner" --count 10
//!   nightrun-tool check --db data/hippocampus.redb --agent "Thomas Mueller"
//!   nightrun-tool list --db data/hippocampus.redb

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use sentinel_hippocampus::{Episode, HippocampusService};

#[derive(Parser)]
#[command(name = "nightrun-tool", about = "Nightrun Dev Tool")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Seed test episodes into hippocampus.redb.
    Seed {
        /// Path to hippocampus.redb.
        #[arg(long)]
        db: String,
        /// Comma-separated agent names (must match TOML identity.name).
        #[arg(long)]
        agents: String,
        /// Episodes per agent.
        #[arg(long, default_value = "10")]
        count: usize,
    },
    /// Check narrative + episode state for an agent after consolidation.
    Check {
        /// Path to hippocampus.redb.
        #[arg(long)]
        db: String,
        /// Agent name to check.
        #[arg(long)]
        agent: String,
    },
    /// List all agents with pending episodes.
    List {
        /// Path to hippocampus.redb.
        #[arg(long)]
        db: String,
    },
}

/// Realistic episode summaries for seeding.
const SUMMARIES: &[&str] = &[
    "Teammeeting ueber Projektfortschritt und naechste Meilensteine",
    "Code-Review mit Kollegen, konstruktives Feedback gegeben",
    "Kunde hat Design-Entwurf abgenommen, positive Rueckmeldung",
    "Kaffeepause mit Gespraech ueber Wochenendplaene",
    "Technisches Problem beim Deployment geloest",
    "Praesentation der Sprint-Ergebnisse vor dem Team",
    "Telefonat mit externem Partner ueber API-Integration",
    "Dokumentation fuer neues Feature geschrieben",
    "Konflikt im Team ueber Architektur-Entscheidung besprochen",
    "Mittagessen mit Kollegen in der Kueche",
    "Bug in Production entdeckt und Hotfix eingespielt",
    "Neuen Mitarbeiter eingearbeitet und Tooling erklaert",
    "Retrospektive: Verbesserungsvorschlaege gesammelt",
    "Performance-Optimierung der Datenbank-Queries",
    "Workshop zu neuer Technologie besucht",
    "Deadlines fuer naechste Woche mit PM abgestimmt",
    "Pair-Programming Session mit Junior-Entwickler",
    "Feedback-Gespraech mit Teamleitung gefuehrt",
    "Pull Request fuer neues Feature erstellt und gemergt",
    "Standup: Blocker identifiziert und Loesung besprochen",
];

/// Realistic tags for episodes.
const TAG_SETS: &[&[&str]] = &[
    &["meeting", "team"],
    &["code-review", "feedback"],
    &["client", "design"],
    &["social", "break"],
    &["technical", "ops"],
    &["presentation", "sprint"],
    &["external", "integration"],
    &["documentation"],
    &["conflict", "architecture"],
    &["social", "lunch"],
    &["bugfix", "production"],
    &["onboarding"],
    &["retrospective"],
    &["performance", "database"],
    &["workshop", "learning"],
    &["planning", "deadline"],
    &["pair-programming"],
    &["feedback", "management"],
    &["pull-request", "feature"],
    &["standup", "blocker"],
];

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Command::Seed { db, agents, count } => cmd_seed(&db, &agents, count),
        Command::Check { db, agent } => cmd_check(&db, &agent),
        Command::List { db } => cmd_list(&db),
    }
}

fn cmd_seed(db: &str, agents_csv: &str, count: usize) -> ExitCode {
    let hc = match HippocampusService::open(db) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("Failed to open hippocampus DB at {db}: {e}");
            return ExitCode::from(2);
        }
    };

    let agents: Vec<&str> = agents_csv.split(',').map(|s| s.trim()).collect();
    let mut total = 0usize;

    for agent in &agents {
        if agent.is_empty() {
            continue;
        }

        let episodes: Vec<Episode> = (0..count)
            .map(|i| {
                let summary_idx = i % SUMMARIES.len();
                let tag_idx = i % TAG_SETS.len();

                // Vary relevance and emotion realistically
                let relevance = 0.3 + (i as f64 * 0.07) % 0.6;
                let emotion = 0.2 + (i as f64 * 0.09) % 0.7;
                let repetitions = 1 + (i % 3) as u32;
                let hours_ago = 0.5 + i as f64 * 0.8;

                Episode {
                    id: (total + i) as u64,
                    agent_name: agent.to_string(),
                    summary: SUMMARIES[summary_idx].to_string(),
                    relevance,
                    emotion,
                    repetitions,
                    hours_ago,
                    participants: vec![],
                    tags: TAG_SETS[tag_idx].iter().map(|s| s.to_string()).collect(),
                }
            })
            .collect();

        match hc.record_episodes(agent, &episodes) {
            Ok(()) => {
                println!("Seeded {count} episodes for \"{agent}\"");
                total += count;
            }
            Err(e) => {
                eprintln!("Failed to seed episodes for \"{agent}\": {e}");
                return ExitCode::from(1);
            }
        }
    }

    println!("Total: {total} episodes seeded for {} agents", agents.len());
    ExitCode::SUCCESS
}

fn cmd_check(db: &str, agent: &str) -> ExitCode {
    let hc = match HippocampusService::open(db) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("Failed to open hippocampus DB at {db}: {e}");
            return ExitCode::from(2);
        }
    };

    // Check narrative
    match hc.get_narrative(agent) {
        Ok(Some(narrative)) => {
            println!("Narrative for \"{agent}\":");
            println!("  Length: {} chars", narrative.len());
            let preview = if narrative.len() > 200 {
                format!("{}...", &narrative[..200])
            } else {
                narrative
            };
            println!("  Content: {preview}");
        }
        Ok(None) => {
            println!("No narrative found for \"{agent}\"");
        }
        Err(e) => {
            eprintln!("Error reading narrative for \"{agent}\": {e}");
            return ExitCode::from(1);
        }
    }

    // Check remaining episodes
    match hc.store().load_episodes(agent) {
        Ok(episodes) => {
            println!("Pending episodes: {}", episodes.len());
            if !episodes.is_empty() {
                println!("  (episodes should be 0 after consolidation)");
            }
        }
        Err(e) => {
            eprintln!("Error loading episodes for \"{agent}\": {e}");
            return ExitCode::from(1);
        }
    }

    ExitCode::SUCCESS
}

fn cmd_list(db: &str) -> ExitCode {
    let hc = match HippocampusService::open(db) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("Failed to open hippocampus DB at {db}: {e}");
            return ExitCode::from(2);
        }
    };

    match hc.store().list_agents_with_episodes() {
        Ok(agents) => {
            if agents.is_empty() {
                println!("No agents with pending episodes");
            } else {
                println!("Agents with pending episodes ({}):", agents.len());
                for agent in &agents {
                    let count = hc
                        .store()
                        .load_episodes(agent)
                        .map(|e| e.len())
                        .unwrap_or(0);
                    println!("  {agent}: {count} episodes");
                }
            }
        }
        Err(e) => {
            eprintln!("Error listing agents: {e}");
            return ExitCode::from(1);
        }
    }

    ExitCode::SUCCESS
}
