//! `sentinel-ctl` — Operator-CLI fuer Sentinel / Gaia (#437, Epic #436).
//!
//! CLI statt MCP (DEV-008): Gaia laeuft lokal als `claude -p` und ruft dieses CLI via Bash.
//! Jeder Subcommand mappt auf einen vorhandenen Operator-API-/Gateway-Pfad (lokale HTTP-Calls).
//! Mutierende/hochriskante Subcommands laufen durch ein Policy-Gate (Risiko-Tag + Bestaetigung);
//! `--json` liefert maschinenlesbare Ausgabe fuer Gaia.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const DEFAULT_OPERATOR_URL: &str = "http://127.0.0.1:8084";
const DEFAULT_GATEWAY_URL: &str = "http://127.0.0.1:8081";
const OPERATOR_KEY_HEADER: &str = "x-sentinel-operator-key";
const OPERATOR_KEY_FILE_ENV: &str = "SENTINEL_OPERATOR_API_KEY_FILE";
const CREDENTIALS_DIRECTORY_ENV: &str = "CREDENTIALS_DIRECTORY";
const OPERATOR_BROKER_SOCKET_ENV: &str = "SENTINEL_GAIA_OPERATOR_BROKER_SOCKET";
const OPERATOR_BROKER_SESSION_ENV: &str = "SENTINEL_GAIA_OPERATOR_BROKER_SESSION";
const OPERATOR_BROKER_CAPABILITY_ENV: &str = "SENTINEL_GAIA_OPERATOR_BROKER_CAPABILITY";
const OPERATOR_CREDENTIAL_NAME: &str = "operator-api";
const CREDENTIAL_MIN_BYTES: u64 = 32;
const CREDENTIAL_MAX_BYTES: u64 = 4096;
const O_DIRECTORY: i32 = 0o200000;
const O_NOFOLLOW: i32 = 0o400000;
const O_NONBLOCK: i32 = 0o4000;
const BROKER_MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CredentialIdentity {
    device: u64,
    inode: u64,
    owner: u32,
    group: u32,
    mode: u32,
    links: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl CredentialIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            owner: metadata.uid(),
            group: metadata.gid(),
            mode: metadata.mode() & 0o7777,
            links: metadata.nlink(),
            size: metadata.size(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CredentialDirectoryIdentity {
    device: u64,
    inode: u64,
    owner: u32,
    group: u32,
    mode: u32,
    is_directory: bool,
}

impl CredentialDirectoryIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            owner: metadata.uid(),
            group: metadata.gid(),
            mode: metadata.mode() & 0o7777,
            is_directory: metadata.is_dir(),
        }
    }
}

struct OpenCredential {
    file: std::fs::File,
    identity: CredentialIdentity,
    directories: Vec<CredentialDirectoryIdentity>,
}

fn operator_credential_path() -> Result<PathBuf, String> {
    if std::env::var_os("SENTINEL_OPERATOR_API_KEY").is_some() {
        return Err("direct operator credentials are not allowed".into());
    }
    let path = std::env::var_os(OPERATOR_KEY_FILE_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| format!("{OPERATOR_KEY_FILE_ENV} is required"))?;
    let directory = std::env::var_os(CREDENTIALS_DIRECTORY_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| format!("{CREDENTIALS_DIRECTORY_ENV} is required"))?;
    if !path.is_absolute()
        || !directory.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        || directory
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        || path != directory.join(OPERATOR_CREDENTIAL_NAME)
    {
        return Err("operator credential path must be the canonical systemd credential leaf".into());
    }
    Ok(path)
}

fn effective_identity() -> Result<(u32, u32), String> {
    let metadata = std::fs::metadata("/proc/self")
        .map_err(|error| format!("operator credential owner authority unavailable: {error}"))?;
    Ok((metadata.uid(), metadata.gid()))
}

fn validate_credential_metadata(
    metadata: &std::fs::Metadata,
    expected_owner: u32,
    expected_group: u32,
) -> Result<CredentialIdentity, String> {
    let identity = CredentialIdentity::from_metadata(metadata);
    let regular = metadata.file_type().is_file();
    let root_systemd = identity.owner == 0
        && identity.group == 0
        && matches!(identity.mode, 0o400 | 0o440);
    let service_owned = identity.owner == expected_owner
        && identity.group == expected_group
        && matches!(identity.mode, 0o400 | 0o600);
    if !regular
        || identity.links != 1
        || identity.size < CREDENTIAL_MIN_BYTES
        || identity.size > CREDENTIAL_MAX_BYTES
        || (!root_systemd && !service_owned)
    {
        return Err("operator credential metadata is invalid".into());
    }
    Ok(identity)
}

fn validate_credential_directory(
    metadata: &std::fs::Metadata,
    expected_owner: u32,
) -> Result<CredentialDirectoryIdentity, String> {
    let identity = CredentialDirectoryIdentity::from_metadata(metadata);
    if !identity.is_directory
        || (identity.owner != 0 && identity.owner != expected_owner)
        || identity.mode & 0o7022 != 0
    {
        return Err("operator credential directory metadata is invalid".into());
    }
    Ok(identity)
}

fn open_operator_credential(
    path: &Path,
    expected_owner: u32,
    expected_group: u32,
) -> Result<OpenCredential, String> {
    if !path.is_absolute() {
        return Err("operator credential path must be absolute".into());
    }
    let components = path
        .components()
        .map(|component| match component {
            Component::RootDir => Ok(None),
            Component::Normal(value) => Ok(Some(value.to_os_string())),
            _ => Err("operator credential path is invalid".to_string()),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let names = components.into_iter().flatten().collect::<Vec<_>>();
    let (file_name, parents) = names
        .split_last()
        .ok_or_else(|| "operator credential path is invalid".to_string())?;

    let mut directory = OpenOptions::new()
        .read(true)
        .custom_flags(O_DIRECTORY | O_NOFOLLOW)
        .open("/")
        .map_err(|error| format!("open operator credential root: {error}"))?;
    let mut directories = vec![validate_credential_directory(
        &directory
            .metadata()
            .map_err(|error| format!("stat operator credential root: {error}"))?,
        expected_owner,
    )?];
    for component in parents {
        let candidate =
            PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())).join(component);
        let next = OpenOptions::new()
            .read(true)
            .custom_flags(O_DIRECTORY | O_NOFOLLOW)
            .open(candidate)
            .map_err(|error| format!("open operator credential directory: {error}"))?;
        directories.push(validate_credential_directory(
            &next
                .metadata()
                .map_err(|error| format!("stat operator credential directory: {error}"))?,
            expected_owner,
        )?);
        directory = next;
    }

    let candidate =
        PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())).join(file_name);
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW | O_NONBLOCK)
        .open(candidate)
        .map_err(|error| format!("open operator credential: {error}"))?;
    let identity = validate_credential_metadata(
        &file
            .metadata()
            .map_err(|error| format!("stat operator credential: {error}"))?,
        expected_owner,
        expected_group,
    )?;
    Ok(OpenCredential {
        file,
        identity,
        directories,
    })
}

fn read_operator_credential_with_hook(
    path: &Path,
    expected_owner: u32,
    expected_group: u32,
    after_open: impl FnOnce() -> Result<(), String>,
) -> Result<String, String> {
    let OpenCredential {
        mut file,
        identity: before,
        directories,
    } = open_operator_credential(path, expected_owner, expected_group)?;
    after_open()?;
    let mut bytes = Vec::with_capacity(before.size as usize);
    file.by_ref()
        .take(CREDENTIAL_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read operator credential: {error}"))?;
    if bytes.len() < CREDENTIAL_MIN_BYTES as usize || bytes.len() > CREDENTIAL_MAX_BYTES as usize {
        return Err("operator credential length is invalid".into());
    }
    let after = validate_credential_metadata(
        &file
            .metadata()
            .map_err(|error| format!("recheck operator credential metadata: {error}"))?,
        expected_owner,
        expected_group,
    )?;
    let reopened = open_operator_credential(path, expected_owner, expected_group)?;
    if before != after
        || before != reopened.identity
        || directories != reopened.directories
        || bytes.len() as u64 != before.size
    {
        return Err("operator credential identity changed while reading".into());
    }
    let value = String::from_utf8(bytes)
        .map_err(|_| "operator credential encoding is invalid".to_string())?;
    if value.trim() != value || value.chars().any(char::is_control) {
        return Err("operator credential content is invalid".into());
    }
    Ok(value)
}

fn read_operator_credential() -> Result<String, String> {
    let path = operator_credential_path()?;
    let (owner, group) = effective_identity()?;
    read_operator_credential_with_hook(&path, owner, group, || Ok(()))
}

/// Risiko-Klassifikation eines Subcommands (Policy-as-Code, operator-seitig).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Risk {
    /// Nur-Lesen — kein Gate.
    Read,
    /// Mutierend — Gate (Bestaetigung noetig).
    Mutate,
    /// Hochriskant (Welt-Reset/Restore/Config-Apply) — Gate (Bestaetigung noetig).
    High,
}

impl Risk {
    fn label(self) -> &'static str {
        match self {
            Risk::Read => "read",
            Risk::Mutate => "mutate",
            Risk::High => "high-risk",
        }
    }
    /// Braucht dieser Risiko-Level eine explizite Bestaetigung?
    fn needs_confirmation(self) -> bool {
        !matches!(self, Risk::Read)
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "sentinel-ctl",
    version,
    about = "Operator CLI for Sentinel / Gaia (#437)"
)]
struct Cli {
    /// Maschinenlesbare JSON-Ausgabe (fuer Gaia).
    #[arg(long, global = true)]
    json: bool,
    /// Bestaetigt mutierende/hochriskante Aktionen (alternativ: SENTINEL_CTL_ASSUME_YES=1).
    #[arg(long, global = true)]
    confirm: bool,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Nachricht in einen Raum (Operator-Chat).
    ChatToRoom {
        room_id: String,
        message: String,
        #[arg(long, default_value = "Operator")]
        sender: String,
    },
    /// 1:1-Direktnachricht an einen Agent.
    DmAgent {
        agent_id: u16,
        message: String,
        #[arg(long, default_value = "Operator")]
        sender: String,
    },
    /// System-weite Durchsage.
    Broadcast {
        message: String,
        #[arg(long = "type", default_value = "info")]
        broadcast_type: String,
    },
    /// Voice of Gaia: Gedanken-Infusion an einen Agent.
    VoiceOfGaia { agent_id: u16, thought: String },
    /// Firmen-Config zur Laufzeit anwenden (#425). `file` = JSON {mode,agents[],building}.
    ApplyConfig { file: String },
    /// Welt wiederherstellen: genau EINES von snapshot_id (Positional) / --target-tick /
    /// --target-event-id (#491 TM-3: Tick/Event-Ziel via Anchor-Snapshot + bounded Replay).
    Restore {
        snapshot_id: Option<String>,
        #[arg(long)]
        target_tick: Option<u64>,
        #[arg(long)]
        target_event_id: Option<i64>,
    },
    /// Manuellen Snapshot ausloesen.
    Snapshot {
        #[arg(long)]
        tier: Option<String>,
    },
    /// Nightrun-Konsolidierung ausloesen.
    Nightrun {
        #[arg(long)]
        shift_set: Option<u8>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Task-/Auftrags-Verwaltung (#438).
    Task {
        #[command(subcommand)]
        action: TaskAction,
    },
    /// Gateway: company-context + Agent-DNA hot-reloaden (#440).
    GatewayReload,
    /// Platform-Control-Plane.
    Platform {
        #[command(subcommand)]
        action: PlatformAction,
    },
    /// Read-only Beobachtung vorhandener Telemetrie/Events.
    Observe {
        #[command(subcommand)]
        what: ObserveWhat,
    },
}

#[derive(Subcommand, Debug)]
enum TaskAction {
    Create {
        title: String,
        assigned_to: u16,
        #[arg(long)]
        parent: Option<u32>,
        #[arg(long)]
        description: Option<String>,
    },
    Assign {
        task_id: u32,
        assigned_to: u16,
        #[arg(long)]
        by: Option<u16>,
    },
    Status {
        task_id: u32,
        status: String,
    },
    Complete {
        task_id: u32,
        #[arg(long)]
        result: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum PlatformAction {
    /// Platform-Analyse anstossen (mutierend).
    Analyze,
    /// Runtime-Reconcile anstossen (hochriskant).
    Reconcile,
    /// Platform-State lesen.
    State,
    /// Runtime-Health lesen.
    RuntimeHealth,
}

#[derive(Subcommand, Debug)]
enum ObserveWhat {
    Snapshots,
    RuntimeHealth,
    PlatformState,
    FsStats,
}

/// Ein aufgeloester HTTP-Call: Methode, Ziel-URL-Pfad, optionaler Body.
struct Call {
    method: Method,
    /// true = Gateway-Control-Plane (8081), false = Operator-API (8084).
    gateway: bool,
    path: String,
    body: Option<Value>,
    risk: Risk,
}

#[derive(Clone, Copy)]
enum Method {
    Get,
    Post,
}

#[derive(Serialize)]
struct GaiaBrokerRequest<'a> {
    schema_version: u8,
    session_id: &'a str,
    capability: &'a str,
    operation_id: String,
    method: &'static str,
    gateway: bool,
    path: &'a str,
    body: &'a Option<Value>,
    risk: &'static str,
    confirmed: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GaiaBrokerResponse {
    ok: bool,
    value: Option<Value>,
    error: Option<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let assume_yes = std::env::var("SENTINEL_CTL_ASSUME_YES")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let call = match resolve_call(&cli.command) {
        Ok(c) => c,
        Err(e) => return fail(&cli, &e),
    };

    // Policy-Gate (#437 AC-2): mutierend/hochriskant braucht Bestaetigung, sonst KEIN Request.
    if let Err(e) = gate(call.risk, cli.confirm, assume_yes) {
        return fail(&cli, &e);
    }

    let confirmed = cli.confirm || assume_yes;
    match execute(&call, confirmed) {
        Ok(value) => {
            if cli.json {
                println!("{}", serde_json::to_string(&value).unwrap_or_default());
            } else {
                println!(
                    "OK [{}] {} {}\n{}",
                    call.risk.label(),
                    method_str(call.method),
                    call.path,
                    serde_json::to_string_pretty(&value).unwrap_or_default()
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(&cli, &format!("request failed: {e}")),
    }
}

/// Policy-Gate: verweigert mutierende/hochriskante Aktionen ohne Bestaetigung.
fn gate(risk: Risk, confirm: bool, assume_yes: bool) -> Result<(), String> {
    if risk.needs_confirmation() && !(confirm || assume_yes) {
        return Err(format!(
            "policy: '{}' action requires confirmation — pass --confirm or set SENTINEL_CTL_ASSUME_YES=1 (no request sent)",
            risk.label()
        ));
    }
    Ok(())
}

fn method_str(m: Method) -> &'static str {
    match m {
        Method::Get => "GET",
        Method::Post => "POST",
    }
}

/// Mappt einen Subcommand auf einen konkreten Operator-API-/Gateway-Pfad + Risiko (#437 AC-1).
fn resolve_call(cmd: &Commands) -> Result<Call, String> {
    let c = |method, gateway, path: &str, body, risk| Call {
        method,
        gateway,
        path: path.to_string(),
        body,
        risk,
    };
    Ok(match cmd {
        Commands::ChatToRoom {
            room_id,
            message,
            sender,
        } => c(
            Method::Post,
            false,
            "/operator/chat",
            Some(json!({"room_id": room_id, "message": message, "sender_name": sender})),
            Risk::Mutate,
        ),
        Commands::DmAgent {
            agent_id,
            message,
            sender,
        } => c(
            Method::Post,
            false,
            "/operator/dm",
            Some(json!({"target_agent_id": agent_id, "message": message, "sender_name": sender})),
            Risk::Mutate,
        ),
        Commands::Broadcast {
            message,
            broadcast_type,
        } => c(
            Method::Post,
            false,
            "/operator/broadcast",
            Some(json!({"message": message, "type": broadcast_type})),
            Risk::Mutate,
        ),
        Commands::VoiceOfGaia { agent_id, thought } => c(
            Method::Post,
            false,
            "/operator/gaia",
            Some(json!({"target_agent_id": agent_id, "thought": thought})),
            Risk::Mutate,
        ),
        Commands::ApplyConfig { file } => {
            let raw = std::fs::read_to_string(file)
                .map_err(|e| format!("read config file {file}: {e}"))?;
            let value: Value = serde_json::from_str(&raw)
                .map_err(|e| format!("config file is not valid JSON: {e}"))?;
            c(
                Method::Post,
                false,
                "/operator/config/apply",
                Some(value),
                Risk::High,
            )
        }
        Commands::Restore {
            snapshot_id,
            target_tick,
            target_event_id,
        } => c(
            Method::Post,
            false,
            "/operator/restore",
            // Nicht gesetzte Felder werden zu JSON null -> serde(default) liest sie als None.
            // Der Daemon validiert "genau eines gesetzt" und antwortet sonst 400.
            Some(json!({
                "snapshot_id": snapshot_id,
                "target_tick": target_tick,
                "target_event_id": target_event_id,
            })),
            Risk::High,
        ),
        Commands::Snapshot { tier } => c(
            Method::Post,
            false,
            "/operator/snapshot",
            Some(json!({"tier": tier})),
            Risk::Mutate,
        ),
        Commands::Nightrun { shift_set, dry_run } => c(
            Method::Post,
            false,
            "/operator/nightrun",
            Some(json!({"shift_set": shift_set, "dry_run": dry_run})),
            Risk::Mutate,
        ),
        Commands::Task { action } => {
            let body = match action {
                TaskAction::Create {
                    title,
                    assigned_to,
                    parent,
                    description,
                } => {
                    json!({"action":"create","title":title,"assigned_to":assigned_to,"parent_task":parent,"description":description})
                }
                TaskAction::Assign {
                    task_id,
                    assigned_to,
                    by,
                } => {
                    json!({"action":"assign","task_id":task_id,"assigned_to":assigned_to,"assigned_by":by})
                }
                TaskAction::Status { task_id, status } => {
                    json!({"action":"update_status","task_id":task_id,"status":status})
                }
                TaskAction::Complete { task_id, result } => {
                    json!({"action":"complete","task_id":task_id,"result":result})
                }
            };
            c(
                Method::Post,
                false,
                "/operator/task",
                Some(body),
                Risk::Mutate,
            )
        }
        Commands::GatewayReload => c(Method::Post, true, "/control/reload", None, Risk::Mutate),
        Commands::Platform { action } => match action {
            PlatformAction::Analyze => c(
                Method::Post,
                false,
                "/operator/platform-analyze",
                None,
                Risk::Mutate,
            ),
            PlatformAction::Reconcile => c(
                Method::Post,
                false,
                "/operator/runtime/reconcile",
                Some(json!({})),
                Risk::High,
            ),
            PlatformAction::State => c(
                Method::Get,
                false,
                "/operator/platform-state",
                None,
                Risk::Read,
            ),
            PlatformAction::RuntimeHealth => c(
                Method::Get,
                false,
                "/operator/runtime-health",
                None,
                Risk::Read,
            ),
        },
        Commands::Observe { what } => match what {
            ObserveWhat::Snapshots => {
                c(Method::Get, false, "/operator/snapshots", None, Risk::Read)
            }
            ObserveWhat::RuntimeHealth => c(
                Method::Get,
                false,
                "/operator/runtime-health",
                None,
                Risk::Read,
            ),
            ObserveWhat::PlatformState => c(
                Method::Get,
                false,
                "/operator/platform-state",
                None,
                Risk::Read,
            ),
            ObserveWhat::FsStats => c(
                Method::Get,
                false,
                "/operator/security/fs-stats",
                None,
                Risk::Read,
            ),
        },
    })
}

fn execute(call: &Call, confirmed: bool) -> Result<Value, String> {
    let broker_values = [
        std::env::var(OPERATOR_BROKER_SOCKET_ENV).ok(),
        std::env::var(OPERATOR_BROKER_SESSION_ENV).ok(),
        std::env::var(OPERATOR_BROKER_CAPABILITY_ENV).ok(),
    ];
    if broker_values.iter().any(Option::is_some) {
        if std::env::var_os(OPERATOR_KEY_FILE_ENV).is_some()
            || std::env::var_os("SENTINEL_OPERATOR_API_KEY").is_some()
        {
            return Err("brokered Gaia execution rejects direct credential authority".into());
        }
        let [Some(socket), Some(session_id), Some(capability)] = broker_values else {
            return Err("Gaia operator broker capability is incomplete".into());
        };
        if call.method != Method::Get
            || call.gateway
            || call.body.is_some()
            || call.risk != Risk::Read
        {
            return Err("Gaia operator broker permits only read observations".into());
        }
        return execute_via_broker(call, confirmed, &socket, &session_id, &capability);
    }
    execute_direct(call)
}

fn execute_via_broker(
    call: &Call,
    confirmed: bool,
    socket: &str,
    session_id: &str,
    capability: &str,
) -> Result<Value, String> {
    if !Path::new(socket).is_absolute()
        || session_id.len() < 16
        || capability.len() < 32
        || !session_id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        || !capability
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err("Gaia operator broker capability is invalid".into());
    }
    let operation_id = format!(
        "ctl-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "operator broker clock is invalid")?
            .as_nanos()
    );
    let request = GaiaBrokerRequest {
        schema_version: 1,
        session_id,
        capability,
        operation_id,
        method: method_str(call.method),
        gateway: call.gateway,
        path: &call.path,
        body: &call.body,
        risk: call.risk.label(),
        confirmed,
    };
    let mut wire = serde_json::to_vec(&request).map_err(|error| error.to_string())?;
    wire.push(b'\n');
    let mut stream = UnixStream::connect(socket)
        .map_err(|error| format!("connect Gaia operator broker: {error}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .map_err(|error| error.to_string())?;
    stream.write_all(&wire).map_err(|error| error.to_string())?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|error| error.to_string())?;
    let mut response = Vec::new();
    stream
        .take(BROKER_MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut response)
        .map_err(|error| error.to_string())?;
    if response.len() > BROKER_MAX_RESPONSE_BYTES as usize {
        return Err("Gaia operator broker response is too large".into());
    }
    let response: GaiaBrokerResponse =
        serde_json::from_slice(&response).map_err(|_| "Gaia operator broker response is invalid")?;
    match (response.ok, response.value, response.error) {
        (true, Some(value), None) => Ok(value),
        (false, None, Some(error)) if !error.is_empty() => Err(error),
        _ => Err("Gaia operator broker response is inconsistent".into()),
    }
}

fn execute_direct(call: &Call) -> Result<Value, String> {
    let operator_credential = read_operator_credential()?;
    let base = if call.gateway {
        std::env::var("CORTEX_GATEWAY_URL").unwrap_or_else(|_| DEFAULT_GATEWAY_URL.to_string())
    } else {
        std::env::var("SENTINEL_OPERATOR_API_URL")
            .unwrap_or_else(|_| DEFAULT_OPERATOR_URL.to_string())
    };
    let url = format!("{base}{}", call.path);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;

    let mut req = match call.method {
        Method::Get => client.get(&url),
        Method::Post => client.post(&url),
    };
    req = req.header(OPERATOR_KEY_HEADER, operator_credential);
    if let Some(body) = &call.body {
        req = req.json(body);
    }
    let resp = req.send().map_err(|e| e.to_string())?;
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
    if status.is_success() {
        Ok(json!({"status": status.as_u16(), "body": parsed}))
    } else {
        Err(format!("HTTP {} — {}", status.as_u16(), parsed))
    }
}

/// Einheitliche Fehlerausgabe + non-zero Exit-Code-Marker (#437 AC-2).
fn fail(cli: &Cli, msg: &str) -> ExitCode {
    if cli.json {
        eprintln!("{}", json!({"ok": false, "error": msg}));
    } else {
        eprintln!("ERROR: {msg}");
    }
    ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Mutex, OnceLock};
    use std::thread;

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    fn credential_case(name: &str) -> (PathBuf, PathBuf) {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join("sentinel-ctl-credential-tests")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join(OPERATOR_CREDENTIAL_NAME);
        std::fs::write(&path, b"0123456789abcdef0123456789abcdef").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        (root, path)
    }

    fn set_credential_environment(root: &Path, path: &Path) {
        assert_eq!(path.parent(), Some(root));
        std::env::set_var(CREDENTIALS_DIRECTORY_ENV, root);
        std::env::set_var(OPERATOR_KEY_FILE_ENV, path);
        std::env::remove_var("SENTINEL_OPERATOR_API_KEY");
        std::env::remove_var(OPERATOR_BROKER_SOCKET_ENV);
        std::env::remove_var(OPERATOR_BROKER_SESSION_ENV);
        std::env::remove_var(OPERATOR_BROKER_CAPABILITY_ENV);
    }

    fn read_test_call() -> Call {
        Call {
            method: Method::Get,
            gateway: false,
            path: "/operator/runtime-health".into(),
            body: None,
            risk: Risk::Read,
        }
    }

    #[test]
    fn read_commands_need_no_confirmation() {
        let call = resolve_call(&Commands::Observe {
            what: ObserveWhat::Snapshots,
        })
        .unwrap();
        assert_eq!(call.risk, Risk::Read);
        assert!(gate(call.risk, false, false).is_ok());
    }

    #[test]
    fn mutating_command_denied_without_confirmation() {
        let call = resolve_call(&Commands::Broadcast {
            message: "hi".into(),
            broadcast_type: "info".into(),
        })
        .unwrap();
        assert_eq!(call.risk, Risk::Mutate);
        assert!(
            gate(call.risk, false, false).is_err(),
            "deny without confirm"
        );
        assert!(gate(call.risk, true, false).is_ok(), "allow with --confirm");
        assert!(
            gate(call.risk, false, true).is_ok(),
            "allow with assume_yes"
        );
    }

    #[test]
    fn high_risk_apply_and_restore_are_gated() {
        let restore = resolve_call(&Commands::Restore {
            snapshot_id: Some("abc".into()),
            target_tick: None,
            target_event_id: None,
        })
        .unwrap();
        assert_eq!(restore.risk, Risk::High);
        assert!(gate(restore.risk, false, false).is_err());
    }

    #[test]
    fn subcommands_map_to_expected_paths() {
        // #437 AC-1: jeder Subcommand mappt auf einen konkreten Pfad.
        assert_eq!(
            resolve_call(&Commands::DmAgent {
                agent_id: 5,
                message: "x".into(),
                sender: "Op".into()
            })
            .unwrap()
            .path,
            "/operator/dm"
        );
        assert_eq!(
            resolve_call(&Commands::VoiceOfGaia {
                agent_id: 5,
                thought: "x".into()
            })
            .unwrap()
            .path,
            "/operator/gaia"
        );
        let reload = resolve_call(&Commands::GatewayReload).unwrap();
        assert_eq!(reload.path, "/control/reload");
        assert!(reload.gateway, "gateway-reload hits the control plane");
        assert_eq!(
            resolve_call(&Commands::Task {
                action: TaskAction::Create {
                    title: "t".into(),
                    assigned_to: 3,
                    parent: None,
                    description: None
                }
            })
            .unwrap()
            .path,
            "/operator/task"
        );
    }

    #[test]
    fn execute_sends_operator_header_loaded_from_file() {
        let _guard = env_lock();
        let (root, path) = credential_case("header");
        set_credential_environment(&root, &path);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::env::set_var("SENTINEL_OPERATOR_API_URL", format!("http://{address}"));
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let count = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..count]);
            assert!(request.contains(
                "x-sentinel-operator-key: 0123456789abcdef0123456789abcdef"
            ));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Type: application/json\r\n\r\n{}")
                .unwrap();
        });

        assert!(execute(&read_test_call(), false).is_ok());
        server.join().unwrap();
        std::env::remove_var("SENTINEL_OPERATOR_API_URL");
        std::env::remove_var("SENTINEL_OPERATOR_API_KEY");
        std::env::remove_var(OPERATOR_KEY_FILE_ENV);
        std::env::remove_var(CREDENTIALS_DIRECTORY_ENV);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_or_tampered_credential_makes_zero_http_calls() {
        let _guard = env_lock();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        std::env::set_var("SENTINEL_OPERATOR_API_URL", format!("http://{address}"));

        std::env::remove_var(OPERATOR_KEY_FILE_ENV);
        assert!(execute(&read_test_call(), false).is_err());
        assert!(listener.accept().is_err());

        let (root, path) = credential_case("tampered");
        set_credential_environment(&root, &path);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(execute(&read_test_call(), false).is_err());
        assert!(listener.accept().is_err());
        std::env::remove_var("SENTINEL_OPERATOR_API_URL");
        std::env::remove_var(OPERATOR_KEY_FILE_ENV);
        std::env::remove_var(CREDENTIALS_DIRECTORY_ENV);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn direct_plaintext_authority_makes_zero_http_calls() {
        let _guard = env_lock();
        let (root, path) = credential_case("plaintext");
        set_credential_environment(&root, &path);
        std::env::set_var("SENTINEL_OPERATOR_API_KEY", "must-not-be-used");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        std::env::set_var(
            "SENTINEL_OPERATOR_API_URL",
            format!("http://{}", listener.local_addr().unwrap()),
        );
        assert!(execute(&read_test_call(), false).is_err());
        assert!(listener.accept().is_err());
        std::env::remove_var("SENTINEL_OPERATOR_API_URL");
        std::env::remove_var("SENTINEL_OPERATOR_API_KEY");
        std::env::remove_var(OPERATOR_KEY_FILE_ENV);
        std::env::remove_var(CREDENTIALS_DIRECTORY_ENV);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn broker_mode_sends_only_structured_scoped_request() {
        let _guard = env_lock();
        let (root, socket_path) = credential_case("broker");
        std::fs::remove_file(&socket_path).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
        let session = "gaia-broker-session-1";
        let capability = "opaque-capability-0123456789abcdef";
        std::env::set_var(OPERATOR_BROKER_SOCKET_ENV, &socket_path);
        std::env::set_var(OPERATOR_BROKER_SESSION_ENV, session);
        std::env::set_var(OPERATOR_BROKER_CAPABILITY_ENV, capability);
        std::env::remove_var(OPERATOR_KEY_FILE_ENV);
        std::env::remove_var(CREDENTIALS_DIRECTORY_ENV);
        std::env::remove_var("SENTINEL_OPERATOR_API_KEY");

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            stream.read_to_string(&mut request).unwrap();
            let request: Value = serde_json::from_str(&request).unwrap();
            assert_eq!(request["schema_version"], 1);
            assert_eq!(request["session_id"], session);
            assert_eq!(request["capability"], capability);
            assert_eq!(request["method"], "GET");
            assert_eq!(request["path"], "/operator/runtime-health");
            assert_eq!(request["risk"], "read");
            assert_eq!(request["confirmed"], false);
            assert!(request.get("operator_key").is_none());
            stream
                .write_all(b"{\"ok\":true,\"value\":{\"status\":200,\"body\":{}},\"error\":null}")
                .unwrap();
        });
        assert!(execute(&read_test_call(), false).is_ok());
        server.join().unwrap();
        std::env::remove_var(OPERATOR_BROKER_SOCKET_ENV);
        std::env::remove_var(OPERATOR_BROKER_SESSION_ENV);
        std::env::remove_var(OPERATOR_BROKER_CAPABILITY_ENV);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn broker_mode_rejects_mutation_before_socket_io() {
        let _guard = env_lock();
        let (root, socket_path) = credential_case("broker-mutation");
        std::fs::remove_file(&socket_path).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
        listener.set_nonblocking(true).unwrap();
        std::env::set_var(OPERATOR_BROKER_SOCKET_ENV, &socket_path);
        std::env::set_var(OPERATOR_BROKER_SESSION_ENV, "gaia-broker-session-1");
        std::env::set_var(
            OPERATOR_BROKER_CAPABILITY_ENV,
            "opaque-capability-0123456789abcdef",
        );
        std::env::remove_var(OPERATOR_KEY_FILE_ENV);
        std::env::remove_var(CREDENTIALS_DIRECTORY_ENV);
        std::env::remove_var("SENTINEL_OPERATOR_API_KEY");

        let mutation = Call {
            method: Method::Post,
            gateway: false,
            path: "/operator/snapshot".to_string(),
            body: Some(json!({"tier": "manual"})),
            risk: Risk::Mutate,
        };
        assert!(execute(&mutation, true).is_err());
        assert!(listener.accept().is_err());

        std::env::remove_var(OPERATOR_BROKER_SOCKET_ENV);
        std::env::remove_var(OPERATOR_BROKER_SESSION_ENV);
        std::env::remove_var(OPERATOR_BROKER_CAPABILITY_ENV);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn credential_reader_rejects_parent_symlink_hardlink_and_replacement() {
        let _guard = env_lock();
        let (root, path) = credential_case("identity");
        let metadata = std::fs::metadata(&path).unwrap();
        let owner = metadata.uid();
        let group = metadata.gid();

        let hardlink = root.join("operator-api.hardlink");
        std::fs::hard_link(&path, &hardlink).unwrap();
        assert!(
            read_operator_credential_with_hook(&path, owner, group, || Ok(())).is_err()
        );
        std::fs::remove_file(&hardlink).unwrap();

        let parent = root.join("credentials");
        std::fs::create_dir(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        let parent_path = parent.join(OPERATOR_CREDENTIAL_NAME);
        std::fs::rename(&path, &parent_path).unwrap();
        let linked_parent = root.join("credentials.link");
        std::os::unix::fs::symlink(&parent, &linked_parent).unwrap();
        assert!(read_operator_credential_with_hook(
            &linked_parent.join(OPERATOR_CREDENTIAL_NAME),
            owner,
            group,
            || Ok(())
        )
        .is_err());

        let original = parent.join("operator-api.original");
        assert!(read_operator_credential_with_hook(&parent_path, owner, group, || {
            std::fs::rename(&parent_path, &original).map_err(|error| error.to_string())?;
            std::fs::write(&parent_path, b"fedcba9876543210fedcba9876543210")
                .map_err(|error| error.to_string())?;
            std::fs::set_permissions(&parent_path, std::fs::Permissions::from_mode(0o600))
                .map_err(|error| error.to_string())?;
            Ok(())
        })
        .is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}
