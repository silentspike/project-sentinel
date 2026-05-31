//! `sentinel-ctl` — Operator-CLI fuer Sentinel / Gaia (#437, Epic #436).
//!
//! CLI statt MCP (DEV-008): Gaia laeuft lokal als `claude -p` und ruft dieses CLI via Bash.
//! Jeder Subcommand mappt auf einen vorhandenen Operator-API-/Gateway-Pfad (lokale HTTP-Calls).
//! Mutierende/hochriskante Subcommands laufen durch ein Policy-Gate (Risiko-Tag + Bestaetigung);
//! `--json` liefert maschinenlesbare Ausgabe fuer Gaia.

use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use serde_json::{json, Value};

const DEFAULT_OPERATOR_URL: &str = "http://127.0.0.1:8084";
const DEFAULT_GATEWAY_URL: &str = "http://127.0.0.1:8081";
const OPERATOR_KEY_HEADER: &str = "x-sentinel-operator-key";

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
#[command(name = "sentinel-ctl", version, about = "Operator CLI for Sentinel / Gaia (#437)")]
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
    /// Welt aus Snapshot wiederherstellen.
    Restore { snapshot_id: String },
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

    match execute(&call) {
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
        Commands::Restore { snapshot_id } => c(
            Method::Post,
            false,
            "/operator/restore",
            Some(json!({"snapshot_id": snapshot_id})),
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
                } => json!({"action":"create","title":title,"assigned_to":assigned_to,"parent_task":parent,"description":description}),
                TaskAction::Assign {
                    task_id,
                    assigned_to,
                    by,
                } => json!({"action":"assign","task_id":task_id,"assigned_to":assigned_to,"assigned_by":by}),
                TaskAction::Status { task_id, status } => {
                    json!({"action":"update_status","task_id":task_id,"status":status})
                }
                TaskAction::Complete { task_id, result } => {
                    json!({"action":"complete","task_id":task_id,"result":result})
                }
            };
            c(Method::Post, false, "/operator/task", Some(body), Risk::Mutate)
        }
        Commands::GatewayReload => {
            c(Method::Post, true, "/control/reload", None, Risk::Mutate)
        }
        Commands::Platform { action } => match action {
            PlatformAction::Analyze => {
                c(Method::Post, false, "/operator/platform-analyze", None, Risk::Mutate)
            }
            PlatformAction::Reconcile => c(
                Method::Post,
                false,
                "/operator/runtime/reconcile",
                Some(json!({})),
                Risk::High,
            ),
            PlatformAction::State => {
                c(Method::Get, false, "/operator/platform-state", None, Risk::Read)
            }
            PlatformAction::RuntimeHealth => {
                c(Method::Get, false, "/operator/runtime-health", None, Risk::Read)
            }
        },
        Commands::Observe { what } => match what {
            ObserveWhat::Snapshots => {
                c(Method::Get, false, "/operator/snapshots", None, Risk::Read)
            }
            ObserveWhat::RuntimeHealth => {
                c(Method::Get, false, "/operator/runtime-health", None, Risk::Read)
            }
            ObserveWhat::PlatformState => {
                c(Method::Get, false, "/operator/platform-state", None, Risk::Read)
            }
            ObserveWhat::FsStats => {
                c(Method::Get, false, "/operator/security/fs-stats", None, Risk::Read)
            }
        },
    })
}

fn execute(call: &Call) -> Result<Value, String> {
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
    if let Ok(key) = std::env::var("SENTINEL_OPERATOR_API_KEY") {
        if !key.is_empty() {
            req = req.header(OPERATOR_KEY_HEADER, key);
        }
    }
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
        assert!(gate(call.risk, false, false).is_err(), "deny without confirm");
        assert!(gate(call.risk, true, false).is_ok(), "allow with --confirm");
        assert!(gate(call.risk, false, true).is_ok(), "allow with assume_yes");
    }

    #[test]
    fn high_risk_apply_and_restore_are_gated() {
        let restore = resolve_call(&Commands::Restore {
            snapshot_id: "abc".into(),
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
}
