//! Thin CLI entrypoint for Gaia Console Memory.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{json, Value};

use crate::graph::{
    Entity, EntityId, FactObject, FactQuery, FactSource, FactWrite, GaiaConsoleMemoryStore,
};
use crate::memory_file::{GaiaConsoleMemoryFile, MemorySection};
use crate::GRAPH_FILE_NAME;

const ASSUME_YES_ENV: &str = "SENTINEL_GAIA_MEMORY_ASSUME_YES";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Risk {
    Read,
    Mutate,
}

impl Risk {
    fn label(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Mutate => "mutate",
        }
    }

    fn needs_confirmation(self) -> bool {
        matches!(self, Self::Mutate)
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "sentinel-gaia-memory",
    version,
    about = "Gaia Console Memory graph and Markdown memory CLI"
)]
struct Cli {
    /// Print machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,
    /// Confirm mutating actions. Alternative: SENTINEL_GAIA_MEMORY_ASSUME_YES=1.
    #[arg(long, global = true)]
    confirm: bool,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Markdown memory file commands.
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
    },
    /// Entity commands for the Gaia Console Memory graph.
    Entity {
        #[command(subcommand)]
        action: EntityAction,
    },
    /// Fact commands for the Gaia Console Memory graph.
    Fact {
        #[command(subcommand)]
        action: FactAction,
    },
}

#[derive(Subcommand, Debug)]
enum MemoryAction {
    /// Read the Markdown memory file.
    Read {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long, default_value_t = 16_384)]
        max_bytes: usize,
    },
    /// Append an entry to one Markdown memory section.
    Append {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long, value_enum)]
        section: SectionArg,
        #[arg(long)]
        timestamp_ms: u64,
        #[arg(long)]
        text: String,
    },
}

#[derive(Subcommand, Debug)]
enum EntityAction {
    /// Create or update an entity.
    Upsert {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        id: String,
        #[arg(long)]
        kind: String,
        #[arg(long)]
        label: String,
        #[arg(long)]
        created_tx_ms: u64,
    },
}

#[derive(Subcommand, Debug)]
enum FactAction {
    /// Insert a new fact without closing prior facts.
    Insert(FactWriteArgs),
    /// Insert a new fact and close current facts for the same subject and relation.
    Supersede(FactWriteArgs),
    /// Query facts by optional subject/relation and bi-temporal coordinates.
    Query {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        subject: Option<String>,
        #[arg(long)]
        relation: Option<String>,
        #[arg(long)]
        valid_at_ms: Option<u64>,
        #[arg(long)]
        as_of_tx_ms: Option<u64>,
        #[arg(long)]
        current_only: bool,
        #[arg(long)]
        include_stale: bool,
    },
}

#[derive(clap::Args, Debug)]
struct FactWriteArgs {
    #[arg(long)]
    data_dir: PathBuf,
    #[arg(long)]
    subject: String,
    #[arg(long)]
    relation: String,
    #[arg(
        long,
        conflicts_with = "object_entity",
        required_unless_present = "object_entity"
    )]
    literal: Option<String>,
    #[arg(long = "object-entity", conflicts_with = "literal")]
    object_entity: Option<String>,
    #[arg(long)]
    valid_from_ms: u64,
    #[arg(long)]
    tx_ms: u64,
    #[arg(long, default_value_t = 1.0)]
    confidence: f32,
    #[arg(long)]
    note: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SectionArg {
    SetupDecisions,
    OpenTasks,
    UserPreferences,
    Notes,
}

impl From<SectionArg> for MemorySection {
    fn from(value: SectionArg) -> Self {
        match value {
            SectionArg::SetupDecisions => Self::SetupDecisions,
            SectionArg::OpenTasks => Self::OpenTasks,
            SectionArg::UserPreferences => Self::UserPreferences,
            SectionArg::Notes => Self::Notes,
        }
    }
}

pub fn run() -> ExitCode {
    let cli = Cli::parse();
    let assume_yes = std::env::var(ASSUME_YES_ENV)
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    match execute_cli(&cli, assume_yes) {
        Ok(value) => {
            if cli.json {
                println!("{}", serde_json::to_string(&value).unwrap_or_default());
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value).unwrap_or_default()
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => fail(&cli, &error),
    }
}

fn execute_cli(cli: &Cli, assume_yes: bool) -> Result<Value, String> {
    let risk = command_risk(&cli.command);
    gate(risk, cli.confirm, assume_yes)?;
    execute_command(&cli.command).map_err(|error| error.to_string())
}

fn command_risk(command: &Commands) -> Risk {
    match command {
        Commands::Memory { action } => match action {
            MemoryAction::Read { .. } => Risk::Read,
            MemoryAction::Append { .. } => Risk::Mutate,
        },
        Commands::Entity { .. } => Risk::Mutate,
        Commands::Fact { action } => match action {
            FactAction::Insert(_) | FactAction::Supersede(_) => Risk::Mutate,
            FactAction::Query { .. } => Risk::Read,
        },
    }
}

fn gate(risk: Risk, confirm: bool, assume_yes: bool) -> Result<(), String> {
    if risk.needs_confirmation() && !(confirm || assume_yes) {
        return Err(format!(
            "policy: '{}' action requires confirmation; pass --confirm or set {ASSUME_YES_ENV}=1 (no mutation performed)",
            risk.label()
        ));
    }
    Ok(())
}

fn execute_command(command: &Commands) -> anyhow::Result<Value> {
    match command {
        Commands::Memory { action } => execute_memory(action),
        Commands::Entity { action } => execute_entity(action),
        Commands::Fact { action } => execute_fact(action),
    }
}

fn execute_memory(action: &MemoryAction) -> anyhow::Result<Value> {
    match action {
        MemoryAction::Read {
            data_dir,
            max_bytes,
        } => {
            let file = GaiaConsoleMemoryFile::open_or_create(data_dir)?;
            let contents = file.read_condensed(*max_bytes)?;
            Ok(json!({
                "ok": true,
                "action": "memory.read",
                "path": file.path().display().to_string(),
                "bytes": contents.len(),
                "contents": contents,
            }))
        }
        MemoryAction::Append {
            data_dir,
            section,
            timestamp_ms,
            text,
        } => {
            let file = GaiaConsoleMemoryFile::open_or_create(data_dir)?;
            let entry = file.append_entry((*section).into(), *timestamp_ms, text)?;
            Ok(json!({
                "ok": true,
                "action": "memory.append",
                "path": file.path().display().to_string(),
                "section": entry.section.slug(),
                "timestamp_ms": entry.timestamp_ms,
            }))
        }
    }
}

fn execute_entity(action: &EntityAction) -> anyhow::Result<Value> {
    match action {
        EntityAction::Upsert {
            data_dir,
            id,
            kind,
            label,
            created_tx_ms,
        } => {
            let store = open_store(data_dir)?;
            let entity = Entity::new(id.as_str(), kind.as_str(), label.as_str(), *created_tx_ms);
            store.upsert_entity(&entity)?;
            Ok(json!({
                "ok": true,
                "action": "entity.upsert",
                "entity": entity,
            }))
        }
    }
}

fn execute_fact(action: &FactAction) -> anyhow::Result<Value> {
    match action {
        FactAction::Insert(args) => {
            let store = open_store(&args.data_dir)?;
            let fact = store.insert_fact(fact_write_from_args(args)?)?;
            Ok(json!({
                "ok": true,
                "action": "fact.insert",
                "fact": fact,
            }))
        }
        FactAction::Supersede(args) => {
            let store = open_store(&args.data_dir)?;
            let fact = store.supersede_fact(fact_write_from_args(args)?)?;
            Ok(json!({
                "ok": true,
                "action": "fact.supersede",
                "fact": fact,
            }))
        }
        FactAction::Query {
            data_dir,
            subject,
            relation,
            valid_at_ms,
            as_of_tx_ms,
            current_only,
            include_stale,
        } => {
            let store = open_store(data_dir)?;
            let facts = store.query_facts(FactQuery {
                subject: subject.as_ref().map(|value| EntityId::new(value.clone())),
                relation: relation.clone(),
                valid_at_ms: *valid_at_ms,
                as_of_tx_ms: *as_of_tx_ms,
                current_only: *current_only,
                include_stale: *include_stale,
            })?;
            Ok(json!({
                "ok": true,
                "action": "fact.query",
                "count": facts.len(),
                "facts": facts,
            }))
        }
    }
}

fn fact_write_from_args(args: &FactWriteArgs) -> anyhow::Result<FactWrite> {
    let object = match (&args.literal, &args.object_entity) {
        (Some(value), None) => FactObject::Literal(value.clone()),
        (None, Some(value)) => FactObject::Entity(EntityId::new(value.clone())),
        _ => anyhow::bail!("exactly one of --literal or --object-entity is required"),
    };

    Ok(FactWrite {
        subject: EntityId::new(args.subject.clone()),
        relation: args.relation.clone(),
        object,
        valid_from_ms: args.valid_from_ms,
        tx_ms: args.tx_ms,
        source: FactSource::manual(),
        confidence: args.confidence,
        note: args.note.clone(),
    })
}

fn open_store(data_dir: &Path) -> anyhow::Result<GaiaConsoleMemoryStore> {
    std::fs::create_dir_all(data_dir)?;
    GaiaConsoleMemoryStore::open(data_dir.join(GRAPH_FILE_NAME))
}

fn fail(cli: &Cli, message: &str) -> ExitCode {
    if cli.json {
        eprintln!("{}", json!({"ok": false, "error": message}));
    } else {
        eprintln!("ERROR: {message}");
    }
    ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_read_commands_need_no_confirmation() {
        let command = Commands::Memory {
            action: MemoryAction::Read {
                data_dir: PathBuf::from("/tmp/gaia-console-memory-test"),
                max_bytes: 128,
            },
        };

        assert_eq!(command_risk(&command), Risk::Read);
        assert!(gate(command_risk(&command), false, false).is_ok());
    }

    #[test]
    fn cli_mutating_commands_require_confirmation() {
        let command = Commands::Memory {
            action: MemoryAction::Append {
                data_dir: PathBuf::from("/tmp/gaia-console-memory-test"),
                section: SectionArg::Notes,
                timestamp_ms: 1,
                text: "hello".to_string(),
            },
        };

        assert_eq!(command_risk(&command), Risk::Mutate);
        assert!(gate(command_risk(&command), false, false).is_err());
        assert!(gate(command_risk(&command), true, false).is_ok());
        assert!(gate(command_risk(&command), false, true).is_ok());
    }

    #[test]
    fn cli_appends_memory_entry_with_confirm() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().to_path_buf();
        let cli = Cli {
            json: true,
            confirm: true,
            command: Commands::Memory {
                action: MemoryAction::Append {
                    data_dir: data_dir.clone(),
                    section: SectionArg::SetupDecisions,
                    timestamp_ms: 100,
                    text: "use crate-local backup path".to_string(),
                },
            },
        };

        let value = execute_cli(&cli, false).unwrap();
        assert_eq!(value["ok"], true);
        let contents = GaiaConsoleMemoryFile::open_or_create(&data_dir)
            .unwrap()
            .read_full()
            .unwrap();
        assert!(contents.contains("use crate-local backup path"));
    }

    #[test]
    fn cli_fact_insert_and_query_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().to_path_buf();
        let insert = Cli {
            json: true,
            confirm: true,
            command: Commands::Fact {
                action: FactAction::Insert(FactWriteArgs {
                    data_dir: data_dir.clone(),
                    subject: "company:sentinel".to_string(),
                    relation: "operating_mode".to_string(),
                    literal: Some("production-grade".to_string()),
                    object_entity: None,
                    valid_from_ms: 1_000,
                    tx_ms: 1_000,
                    confidence: 1.0,
                    note: None,
                }),
            },
        };
        execute_cli(&insert, false).unwrap();

        let query = Cli {
            json: true,
            confirm: false,
            command: Commands::Fact {
                action: FactAction::Query {
                    data_dir,
                    subject: Some("company:sentinel".to_string()),
                    relation: Some("operating_mode".to_string()),
                    valid_at_ms: None,
                    as_of_tx_ms: None,
                    current_only: true,
                    include_stale: false,
                },
            },
        };

        let value = execute_cli(&query, false).unwrap();
        assert_eq!(value["count"], 1);
        assert_eq!(
            value["facts"][0]["fact"]["object"]["value"],
            "production-grade"
        );
    }
}
