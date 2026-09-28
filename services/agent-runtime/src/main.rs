//! Isolated agent workbench process.
//!
//! The daemon owns durable invocation state and sends newline-delimited JSON
//! commands over stdin. This process validates every effect-bearing request,
//! executes it inside the already-selected sandbox, and returns structured JSON
//! messages over stdout. Human diagnostics on stderr never include request data.

use std::collections::BTreeMap;
use std::env;
use std::io::{self, BufRead, Write};
use std::os::unix::fs::OpenOptionsExt as UnixOpenOptionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use agent_runtime::WorkbenchExecutor;
use sentinel_common::{
    WorkbenchCommand, WorkbenchErrorClass, WorkbenchErrorInfo, WorkbenchMessage,
    WorkbenchProgressStage, WORKBENCH_AGENT_RUNTIME_VERSION, WORKBENCH_SCHEMA_VERSION,
};
use serde::Serialize;

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const INPUT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
const MAX_WORKBENCH_FRAME_BYTES: usize = 1024 * 1024;
const MAX_PENDING_READER_EVENTS: usize = 1;
const STARTUP_ATTESTATION_SCHEMA_VERSION: u16 = 1;
const ATTESTATION_NONCE_ENV: &str = "SENTINEL_WORKBENCH_ATTESTATION_NONCE";
const ATTESTATION_WRAPPER_VERSION_ENV: &str = "SENTINEL_WORKBENCH_WRAPPER_VERSION";
const ATTESTATION_LANDLOCK_ABI_ENV: &str = "SENTINEL_WORKBENCH_LANDLOCK_ABI";

#[derive(Serialize)]
struct StartupAttestation<'a> {
    schema_version: u16,
    nonce: &'a str,
    wrapper_version: &'a str,
    runtime_version: &'static str,
    landlock_abi: u8,
    #[serde(rename = "host_pid")]
    namespace_pid: u32,
}

#[derive(Clone)]
struct ActiveInvocation {
    cancelled: Arc<AtomicBool>,
    deadline_cancelled: Arc<AtomicBool>,
}

type ActiveInvocations = Arc<Mutex<BTreeMap<String, ActiveInvocation>>>;

#[derive(Default)]
struct ProtocolOutput {
    lock: Mutex<()>,
    #[cfg(test)]
    test: Option<TestProtocolOutput>,
}

#[cfg(test)]
struct TestProtocolOutput {
    messages: mpsc::Sender<String>,
    resume_execution: Mutex<mpsc::Receiver<()>>,
}

enum ReaderEvent {
    Command(WorkbenchCommand),
    Malformed,
    Oversized,
    Eof,
}

enum BoundedJsonlRecord {
    Record(Vec<u8>),
    Malformed,
    Oversized,
    Eof,
}

fn main() {
    if env::args_os().nth(1).as_deref()
        == Some(std::ffi::OsStr::new(
            agent_runtime::command_sandbox::GATE_MODE,
        ))
    {
        if let Err(error) = agent_runtime::command_sandbox::run_gate(env::args_os().skip(2)) {
            eprintln!("agent-runtime: command membership or namespace setup failed: {error}");
            std::process::exit(126);
        }
    } else if env::args_os().nth(1).as_deref()
        == Some(std::ffi::OsStr::new(
            agent_runtime::command_sandbox::CHILD_MODE,
        ))
    {
        if let Err(error) = agent_runtime::command_sandbox::run_child(env::args_os().skip(2)) {
            eprintln!("agent-runtime: scoped command isolation or exec failed: {error}");
            std::process::exit(126);
        }
    } else if workbench_mode_requested() {
        run_workbench();
    } else {
        run_general_agent();
    }
}

fn workbench_mode_requested() -> bool {
    [
        ATTESTATION_NONCE_ENV,
        ATTESTATION_WRAPPER_VERSION_ENV,
        ATTESTATION_LANDLOCK_ABI_ENV,
    ]
    .into_iter()
    .any(|name| env::var_os(name).is_some())
}

fn run_general_agent() {
    eprintln!("agent-runtime: started (pid={})", std::process::id());

    let running = Arc::new(AtomicBool::new(true));
    let reader_running = running.clone();
    thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            match line {
                Ok(command) if command.trim() == "shutdown" => break,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        reader_running.store(false, Ordering::Release);
    });

    write_heartbeat();
    let mut last_heartbeat = Instant::now();
    while running.load(Ordering::Acquire) {
        if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
            write_heartbeat();
            last_heartbeat = Instant::now();
        }
        thread::sleep(INPUT_POLL_INTERVAL);
    }

    eprintln!("agent-runtime: shutting down");
}

fn run_workbench() {
    if nix::sys::prctl::set_dumpable(false).is_err() || nix::sys::prctl::get_dumpable() != Ok(false)
    {
        eprintln!("agent-runtime: trusted process memory protection failed");
        std::process::exit(126);
    }
    let runner = match std::env::current_exe() {
        Ok(path) => path,
        Err(_) => {
            eprintln!("agent-runtime: trusted command runner is unavailable");
            std::process::exit(126);
        }
    };
    let workspace_root = std::env::var_os("SENTINEL_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/workspace"));
    let artifact_root = std::env::var_os("SENTINEL_ARTIFACT_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/artifacts"));
    let input_root = std::env::var_os("SENTINEL_INPUT_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/workspace/.inputs"));
    let mut executor =
        WorkbenchExecutor::with_input_root(&workspace_root, artifact_root, input_root)
            .with_command_runner(runner.clone());
    // Optional trusted outer-enforcer configuration. Without it native command
    // execution fails closed; file tools/receipt recovery remain available.
    match (
        env::var_os("SENTINEL_COMMAND_CGROUP_ROOT"),
        env::var("SENTINEL_WORKSPACE_BUDGET_BYTES"),
    ) {
        (Some(root), Ok(budget)) => {
            let budget = match budget.parse::<u64>() {
                Ok(value) if value > 0 => value,
                _ => {
                    eprintln!("agent-runtime: invalid command workspace budget contract");
                    std::process::exit(126);
                }
            };
            let boundary = agent_runtime::command_sandbox::CommandBoundary {
                cgroup_root: PathBuf::from(root),
                workspace_budget_bytes: budget,
            };
            if agent_runtime::command_sandbox::verify_readiness(&runner, &workspace_root, &boundary)
                .is_err()
            {
                eprintln!("agent-runtime: native command isolation readiness failed");
                std::process::exit(126);
            }
            executor = executor.with_command_boundary(boundary);
        }
        (None, Err(env::VarError::NotPresent)) => {}
        _ => {
            eprintln!("agent-runtime: incomplete command boundary contract");
            std::process::exit(126);
        }
    }
    let executor = Arc::new(executor);
    if executor
        .reconcile_root_completion_receipts_before_serving()
        .is_err()
    {
        eprintln!("agent-runtime: startup completion receipt recovery failed");
        std::process::exit(126);
    }
    if write_startup_attestation().is_err() {
        eprintln!("agent-runtime: startup isolation attestation failed");
        std::process::exit(126);
    }
    eprintln!(
        "agent-runtime: workbench started (pid={})",
        std::process::id()
    );

    let active: ActiveInvocations = Arc::new(Mutex::new(BTreeMap::new()));
    let output_lock = Arc::new(ProtocolOutput::default());
    let running = Arc::new(AtomicBool::new(true));
    let (sender, receiver) = mpsc::sync_channel(MAX_PENDING_READER_EVENTS);

    thread::spawn(move || read_commands(sender));
    write_heartbeat();
    let mut last_heartbeat = Instant::now();

    while running.load(Ordering::Acquire) {
        match receiver.recv_timeout(INPUT_POLL_INTERVAL) {
            Ok(ReaderEvent::Command(command)) => handle_command(
                command,
                executor.clone(),
                active.clone(),
                output_lock.clone(),
            ),
            Ok(ReaderEvent::Malformed) => emit(
                &output_lock,
                &WorkbenchMessage::Error {
                    schema_version: WORKBENCH_SCHEMA_VERSION,
                    invocation_id: None,
                    error: protocol_error(
                        "malformed_message",
                        "the workbench command could not be parsed",
                    ),
                },
            ),
            Ok(ReaderEvent::Oversized) => emit(
                &output_lock,
                &WorkbenchMessage::Error {
                    schema_version: WORKBENCH_SCHEMA_VERSION,
                    invocation_id: None,
                    error: protocol_error(
                        "frame_too_large",
                        "the workbench command exceeded the 1 MiB frame boundary",
                    ),
                },
            ),
            Ok(ReaderEvent::Eof) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                running.store(false, Ordering::Release);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
            write_heartbeat();
            last_heartbeat = Instant::now();
        }
    }

    cancel_all_and_wait(&active);
    eprintln!("agent-runtime: workbench stopped");
}

fn write_startup_attestation() -> io::Result<()> {
    if env!("CARGO_PKG_VERSION") != WORKBENCH_AGENT_RUNTIME_VERSION {
        return Err(io::Error::other(
            "agent runtime version is outside the attestation contract",
        ));
    }
    let nonce = std::env::var(ATTESTATION_NONCE_ENV)
        .map_err(|_| io::Error::other("startup attestation nonce is unavailable"))?;
    if !valid_attestation_nonce(&nonce) {
        return Err(io::Error::other("startup attestation nonce is invalid"));
    }
    let wrapper_version = std::env::var(ATTESTATION_WRAPPER_VERSION_ENV)
        .map_err(|_| io::Error::other("startup wrapper version is unavailable"))?;
    let landlock_abi = std::env::var(ATTESTATION_LANDLOCK_ABI_ENV)
        .ok()
        .and_then(|value| value.parse::<u8>().ok())
        .filter(|abi| *abi > 0)
        .ok_or_else(|| io::Error::other("startup Landlock ABI is invalid"))?;
    let namespace_pid = namespace_pid_from_nspid()?;
    let attestation = StartupAttestation {
        schema_version: STARTUP_ATTESTATION_SCHEMA_VERSION,
        nonce: &nonce,
        wrapper_version: &wrapper_version,
        runtime_version: env!("CARGO_PKG_VERSION"),
        landlock_abi,
        namespace_pid,
    };
    let bytes = serde_json::to_vec(&attestation)
        .map_err(|_| io::Error::other("encode startup attestation"))?;
    let path = PathBuf::from(format!("/tmp/.sentinel-workbench-attestation-{nonce}.json"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(&bytes)?;
    file.sync_all()
}

fn valid_attestation_nonce(nonce: &str) -> bool {
    nonce.len() == 36
        && nonce.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()
            }
        })
}

fn namespace_pid_from_nspid() -> io::Result<u32> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    parse_namespace_pid_from_nspid(&status)
        .ok_or_else(|| io::Error::other("PID namespace identity is unavailable"))
}

fn parse_namespace_pid_from_nspid(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("NSpid:"))
        .and_then(|value| value.split_whitespace().last())
        .and_then(|value| value.parse().ok())
        .filter(|pid| *pid > 0)
}

fn read_commands(sender: mpsc::SyncSender<ReaderEvent>) {
    let stdin = io::stdin();
    read_commands_from(stdin.lock(), sender);
}

fn read_commands_from(mut input: impl BufRead, sender: mpsc::SyncSender<ReaderEvent>) {
    loop {
        let event = match read_bounded_jsonl_record(&mut input) {
            Ok(BoundedJsonlRecord::Record(bytes)) if !bytes.iter().all(u8::is_ascii_whitespace) => {
                match serde_json::from_slice::<WorkbenchCommand>(&bytes) {
                    Ok(command) => ReaderEvent::Command(command),
                    Err(_) => ReaderEvent::Malformed,
                }
            }
            Ok(BoundedJsonlRecord::Record(_) | BoundedJsonlRecord::Malformed) => {
                ReaderEvent::Malformed
            }
            Ok(BoundedJsonlRecord::Oversized) => ReaderEvent::Oversized,
            Ok(BoundedJsonlRecord::Eof) | Err(_) => break,
        };
        if sender.send(event).is_err() {
            return;
        }
    }
    let _ = sender.send(ReaderEvent::Eof);
}

fn read_bounded_jsonl_record(input: &mut impl BufRead) -> io::Result<BoundedJsonlRecord> {
    let mut record = Vec::with_capacity(4096);
    let mut oversized = false;
    loop {
        let buffer = input.fill_buf()?;
        if buffer.is_empty() {
            return Ok(if oversized {
                BoundedJsonlRecord::Oversized
            } else if record.is_empty() {
                BoundedJsonlRecord::Eof
            } else {
                BoundedJsonlRecord::Malformed
            });
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let take = newline.unwrap_or(buffer.len());
        if !oversized {
            if record.len().saturating_add(take) > MAX_WORKBENCH_FRAME_BYTES {
                oversized = true;
                record.clear();
            } else {
                record.extend_from_slice(&buffer[..take]);
            }
        }
        input.consume(take + usize::from(newline.is_some()));
        if newline.is_some() {
            return Ok(if oversized {
                BoundedJsonlRecord::Oversized
            } else {
                BoundedJsonlRecord::Record(record)
            });
        }
    }
}

fn handle_command(
    command: WorkbenchCommand,
    executor: Arc<WorkbenchExecutor>,
    active: ActiveInvocations,
    output_lock: Arc<ProtocolOutput>,
) {
    match command {
        WorkbenchCommand::Execute { request } => {
            let invocation_id = request.invocation_id.clone();
            if let Err(error) = request.validate_at(unix_time_ms()) {
                emit(
                    &output_lock,
                    &WorkbenchMessage::Error {
                        schema_version: WORKBENCH_SCHEMA_VERSION,
                        invocation_id: Some(invocation_id),
                        error: WorkbenchErrorInfo {
                            class: WorkbenchErrorClass::Authorization,
                            code: "request_rejected".to_string(),
                            safe_message: error.to_string(),
                            retryable: false,
                        },
                    },
                );
                return;
            }
            let cancellation = Arc::new(AtomicBool::new(false));
            let deadline_cancellation = Arc::new(AtomicBool::new(false));
            let active_conflict = {
                let mut active_guard = active.lock().unwrap_or_else(|error| error.into_inner());
                if active_guard.contains_key(&invocation_id) {
                    Some((
                        "invocation_already_active",
                        "the invocation is already executing",
                    ))
                } else if !active_guard.is_empty() {
                    Some((
                        "runtime_busy",
                        "the isolated agent runtime already has an active invocation",
                    ))
                } else {
                    active_guard.insert(
                        invocation_id.clone(),
                        ActiveInvocation {
                            cancelled: cancellation.clone(),
                            deadline_cancelled: deadline_cancellation.clone(),
                        },
                    );
                    None
                }
            };
            if let Some((code, message)) = active_conflict {
                emit(
                    &output_lock,
                    &WorkbenchMessage::Error {
                        schema_version: WORKBENCH_SCHEMA_VERSION,
                        invocation_id: Some(invocation_id),
                        error: protocol_error(code, message),
                    },
                );
                return;
            }
            emit_progress(
                &output_lock,
                &invocation_id,
                WorkbenchProgressStage::Validated,
                0,
            );
            thread::spawn(move || {
                let started = Instant::now();
                emit_progress(
                    &output_lock,
                    &invocation_id,
                    WorkbenchProgressStage::Executing,
                    0,
                );
                // Unit tests hold this boundary until Cancel has been handled;
                // the real executor still owns all effects and terminal results.
                #[cfg(test)]
                if let Some(test) = &output_lock.test {
                    test.resume_execution
                        .lock()
                        .unwrap()
                        .recv_timeout(SHUTDOWN_GRACE)
                        .expect("test must release execution after handling cancellation");
                }
                let mut result = executor.execute(*request.clone(), cancellation);
                apply_outer_deadline_outcome(&mut result, &deadline_cancellation);
                let receipt_persisted = match executor.persist_request_completion(&request, &result)
                {
                    Ok(()) => {
                        emit(&output_lock, &result);
                        true
                    }
                    Err(error) => {
                        emit(
                            &output_lock,
                            &WorkbenchMessage::Error {
                                schema_version: WORKBENCH_SCHEMA_VERSION,
                                invocation_id: Some(invocation_id.clone()),
                                error,
                            },
                        );
                        false
                    }
                };
                if receipt_persisted {
                    emit_progress(
                        &output_lock,
                        &invocation_id,
                        WorkbenchProgressStage::Completed,
                        elapsed_ms(started),
                    );
                }
                active
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .remove(&invocation_id);
            });
        }
        WorkbenchCommand::Cancel {
            schema_version,
            invocation_id,
            reason,
        } => {
            if schema_version != WORKBENCH_SCHEMA_VERSION {
                emit(
                    &output_lock,
                    &WorkbenchMessage::Error {
                        schema_version: WORKBENCH_SCHEMA_VERSION,
                        invocation_id: Some(invocation_id),
                        error: protocol_error(
                            "unsupported_version",
                            "the workbench command version is unsupported",
                        ),
                    },
                );
                return;
            }
            let cancellation = active
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get(&invocation_id)
                .cloned();
            if let Some(cancellation) = cancellation {
                // The result/receipt path owns the terminal acknowledgement.
                // Emitting `cancelled` here would let the adapter tear down the
                // process before the effect outcome and receipt are known.
                if reason == "deadline_expired" {
                    cancellation
                        .deadline_cancelled
                        .store(true, Ordering::Release);
                }
                cancellation.cancelled.store(true, Ordering::Release);
            }
        }
        WorkbenchCommand::Recover {
            schema_version,
            invocation_id,
            input_digest,
        } => {
            if schema_version != WORKBENCH_SCHEMA_VERSION {
                emit(
                    &output_lock,
                    &WorkbenchMessage::Error {
                        schema_version: WORKBENCH_SCHEMA_VERSION,
                        invocation_id: Some(invocation_id),
                        error: protocol_error(
                            "unsupported_version",
                            "the workbench command version is unsupported",
                        ),
                    },
                );
                return;
            }
            match executor.recover_completion(&invocation_id, &input_digest) {
                Ok(message) => {
                    emit(&output_lock, &message);
                    emit_progress(
                        &output_lock,
                        &invocation_id,
                        WorkbenchProgressStage::Completed,
                        0,
                    );
                }
                Err(error) => emit(
                    &output_lock,
                    &WorkbenchMessage::Error {
                        schema_version: WORKBENCH_SCHEMA_VERSION,
                        invocation_id: Some(invocation_id),
                        error,
                    },
                ),
            }
        }
        WorkbenchCommand::Health {
            schema_version,
            request_id,
        } => {
            if schema_version != WORKBENCH_SCHEMA_VERSION {
                emit(
                    &output_lock,
                    &WorkbenchMessage::Error {
                        schema_version: WORKBENCH_SCHEMA_VERSION,
                        invocation_id: None,
                        error: protocol_error(
                            "unsupported_version",
                            "the workbench command version is unsupported",
                        ),
                    },
                );
                return;
            }
            let active_invocations = active
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .len()
                .try_into()
                .unwrap_or(u32::MAX);
            emit(
                &output_lock,
                &WorkbenchMessage::Health {
                    schema_version: WORKBENCH_SCHEMA_VERSION,
                    request_id,
                    healthy: true,
                    active_invocations,
                },
            );
        }
    }
}

fn cancel_all_and_wait(active: &ActiveInvocations) {
    {
        let active_guard = active.lock().unwrap_or_else(|error| error.into_inner());
        for cancellation in active_guard.values() {
            cancellation.cancelled.store(true, Ordering::Release);
        }
    }
    let deadline = Instant::now() + SHUTDOWN_GRACE;
    while Instant::now() < deadline {
        if active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
        {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    eprintln!("agent-runtime: shutdown grace expired with active work");
}

fn apply_outer_deadline_outcome(message: &mut WorkbenchMessage, deadline_cancelled: &AtomicBool) {
    if !deadline_cancelled.load(Ordering::Acquire) {
        return;
    }
    let WorkbenchMessage::Result {
        outcome,
        error: Some(error),
        ..
    } = message
    else {
        return;
    };
    if *outcome == sentinel_common::WorkbenchOutcome::Cancelled {
        *outcome = sentinel_common::WorkbenchOutcome::TimedOut;
        error.class = WorkbenchErrorClass::Resource;
        error.code = "deadline_expired".to_string();
        error.safe_message = "invocation deadline expired".to_string();
        error.retryable = false;
    }
}

fn emit_progress(
    output_lock: &Arc<ProtocolOutput>,
    invocation_id: &str,
    stage: WorkbenchProgressStage,
    elapsed_ms: u64,
) {
    emit(
        output_lock,
        &WorkbenchMessage::Progress {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: invocation_id.to_string(),
            stage,
            elapsed_ms,
        },
    );
}

fn emit(output_lock: &Arc<ProtocolOutput>, message: &WorkbenchMessage) {
    let _guard = output_lock
        .lock
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    #[cfg(test)]
    if let Some(test) = &output_lock.test {
        test.messages
            .send(serde_json::to_string(message).unwrap())
            .unwrap();
        return;
    }
    let mut stdout = io::stdout().lock();
    if serde_json::to_writer(&mut stdout, message).is_err()
        || stdout.write_all(b"\n").is_err()
        || stdout.flush().is_err()
    {
        eprintln!("agent-runtime: protocol output failed");
    }
}

fn protocol_error(code: &str, safe_message: &str) -> WorkbenchErrorInfo {
    WorkbenchErrorInfo {
        class: WorkbenchErrorClass::Protocol,
        code: code.to_string(),
        safe_message: safe_message.to_string(),
        retryable: false,
    }
}

fn write_heartbeat() {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if OpenOptionsExt::write_heartbeat(timestamp).is_err() {
        eprintln!("agent-runtime: heartbeat write failed");
    }
}

struct OpenOptionsExt;

impl OpenOptionsExt {
    fn write_heartbeat(timestamp: u64) -> io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open("/tmp/heartbeat")?;
        writeln!(file, "{timestamp}")
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

#[cfg(test)]
mod protocol_cancellation_tests {
    use super::*;
    use sentinel_common::{
        AgentId, WorkbenchOutcome, WorkbenchRequest, WorkbenchResourceLimits, WorkbenchTool,
        WORKBENCH_RUNTIME_BWRAP,
    };
    use std::collections::BTreeSet;

    fn write_request() -> WorkbenchRequest {
        WorkbenchRequest {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: "018f3f32-4f01-7f2c-a6c1-f6f4a81b2902".into(),
            agent_id: AgentId(7),
            project_id: "project-01".into(),
            work_item_id: "work-04".into(),
            workspace_id: "project-01:work-04".into(),
            caller_id: "AGENT-07".into(),
            caller_role: "developer".into(),
            assignment_version: 2,
            credential_generation: 3,
            policy_digest: "a".repeat(64),
            tool_profile: "web-authoring-v1".into(),
            tool_profile_digest: "b".repeat(64),
            runtime_key: WORKBENCH_RUNTIME_BWRAP.into(),
            capabilities: BTreeSet::from(["file.write".into()]),
            output_artifact_kinds: BTreeSet::from(["source_tree".into()]),
            inputs: Vec::new(),
            command_policy: Vec::new(),
            resource_limits: WorkbenchResourceLimits {
                wall_time_ms: 10_000,
                cpu_time_ms: 5_000,
                memory_bytes: 128 * 1024 * 1024,
                process_count: 8,
                file_bytes: 1024 * 1024,
                stdout_bytes: 64 * 1024,
                stderr_bytes: 64 * 1024,
            },
            deadline_unix_ms: unix_time_ms() + 30_000,
            attempt: 1,
            tool: WorkbenchTool::WriteFile {
                path: "src/index.html".into(),
                content: "must not be written".into(),
                expected_sha256: None,
            },
            input_digest: String::new(),
        }
        .bind_digest()
        .unwrap()
    }

    fn receive(messages: &mpsc::Receiver<String>) -> WorkbenchMessage {
        serde_json::from_str(&messages.recv_timeout(SHUTDOWN_GRACE).unwrap()).unwrap()
    }

    fn assert_receipted_cancellation(reason: &str, expected: WorkbenchOutcome) {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let artifacts = directory.path().join("artifacts");
        let executor = Arc::new(WorkbenchExecutor::new(&workspace, &artifacts));
        let active: ActiveInvocations = Arc::new(Mutex::new(BTreeMap::new()));
        let (sender, messages) = mpsc::channel();
        let (resume, gate) = mpsc::channel();
        let output = Arc::new(ProtocolOutput {
            lock: Mutex::new(()),
            test: Some(TestProtocolOutput {
                messages: sender,
                resume_execution: Mutex::new(gate),
            }),
        });
        let request = write_request();
        let receipt = artifacts
            .join(".workbench-receipts")
            .join(format!("{}.json", request.invocation_id));
        handle_command(
            WorkbenchCommand::Execute {
                request: Box::new(request.clone()),
            },
            executor.clone(),
            active.clone(),
            output.clone(),
        );
        for expected_stage in [
            WorkbenchProgressStage::Validated,
            WorkbenchProgressStage::Executing,
        ] {
            assert!(matches!(
                receive(&messages),
                WorkbenchMessage::Progress { invocation_id, stage, .. }
                    if invocation_id == request.invocation_id && stage == expected_stage
            ));
        }
        handle_command(
            WorkbenchCommand::Cancel {
                schema_version: WORKBENCH_SCHEMA_VERSION,
                invocation_id: request.invocation_id.clone(),
                reason: reason.into(),
            },
            executor.clone(),
            active.clone(),
            output.clone(),
        );
        {
            let active_guard = active.lock().unwrap();
            let invocation = active_guard.get(&request.invocation_id).unwrap();
            assert!(invocation.cancelled.load(Ordering::Acquire));
            assert_eq!(
                invocation.deadline_cancelled.load(Ordering::Acquire),
                reason == "deadline_expired"
            );
        }
        assert!(matches!(messages.try_recv(), Err(mpsc::TryRecvError::Empty)));
        assert!(
            !receipt.exists(),
            "Cancel must not manufacture a receipt or acknowledgement"
        );
        resume.send(()).unwrap();

        let result = receive(&messages);
        let WorkbenchMessage::Result {
            invocation_id,
            input_digest,
            outcome,
            resources,
            artifacts: result_artifacts,
            output: feedback,
            error: Some(error),
            ..
        } = &result
        else {
            panic!("cancellation must terminate with a real executor result: {result:?}");
        };
        assert_eq!(invocation_id, &request.invocation_id);
        assert_eq!(input_digest, &request.input_digest);
        assert_eq!(*outcome, expected);
        assert_eq!(resources.bytes_written, 0);
        assert!(result_artifacts.is_empty() && feedback.is_empty());
        assert_eq!(
            error.code,
            if reason == "deadline_expired" {
                reason
            } else {
                "cancelled"
            }
        );
        assert_eq!(
            error.class,
            if reason == "deadline_expired" {
                WorkbenchErrorClass::Resource
            } else {
                WorkbenchErrorClass::Runtime
            }
        );
        assert!(!error.retryable);
        assert!(!workspace.join("project-01/work-04/src/index.html").exists());
        assert!(
            receipt.is_file(),
            "receipt must exist before the terminal result is emitted"
        );
        assert_eq!(
            executor
                .recover_completion(&request.invocation_id, &request.input_digest)
                .unwrap(),
            result
        );
        assert!(matches!(
            receive(&messages),
            WorkbenchMessage::Progress { invocation_id, stage: WorkbenchProgressStage::Completed, .. }
                if invocation_id == request.invocation_id
        ));
        cancel_all_and_wait(&active);
        assert!(active.lock().unwrap().is_empty());
        handle_command(
            WorkbenchCommand::Recover {
                schema_version: WORKBENCH_SCHEMA_VERSION,
                invocation_id: request.invocation_id.clone(),
                input_digest: request.input_digest.clone(),
            },
            Arc::new(WorkbenchExecutor::new(&workspace, &artifacts)),
            active,
            output,
        );
        assert_eq!(receive(&messages), result);
        assert!(matches!(
            receive(&messages),
            WorkbenchMessage::Progress { stage: WorkbenchProgressStage::Completed, .. }
        ));
        assert!(matches!(
            messages.recv_timeout(SHUTDOWN_GRACE),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
    }

    #[test]
    fn cancel_waits_for_the_receipted_result_instead_of_acknowledging_early() {
        assert_receipted_cancellation("test_cancel", WorkbenchOutcome::Cancelled);
    }

    #[test]
    fn adapter_deadline_cancel_is_receipted_as_timed_out() {
        assert_receipted_cancellation("deadline_expired", WorkbenchOutcome::TimedOut);
    }

    #[test]
    fn outer_deadline_does_not_overwrite_finished_success_or_isolation_failure() {
        let directory = tempfile::tempdir().unwrap();
        let executor = WorkbenchExecutor::new(
            directory.path().join("workspace"),
            directory.path().join("artifacts"),
        )
        .with_command_runner(std::env::current_exe().unwrap());
        let write = write_request();
        let mut command = write.clone();
        command.capabilities = BTreeSet::from(["command.run_allowlisted".into()]);
        command.command_policy = vec![sentinel_common::CommandRule {
            program: "sleep".into(),
            required_arg_prefix: Vec::new(),
            max_args: 1,
        }];
        command.tool = WorkbenchTool::RunCommand {
            program: "sleep".into(),
            args: vec!["5".into()],
        };
        command.input_digest = command.canonical_digest().unwrap();
        for (request, expected) in [
            (write, WorkbenchOutcome::Succeeded),
            (command, WorkbenchOutcome::Failed),
        ] {
            let original = executor.execute(request, Arc::new(AtomicBool::new(false)));
            assert!(matches!(
                &original,
                WorkbenchMessage::Result { outcome, .. } if *outcome == expected
            ));
            if expected == WorkbenchOutcome::Failed {
                assert!(matches!(
                    &original,
                    WorkbenchMessage::Result { error: Some(error), output, .. }
                        if error.code == "command_isolation_unavailable" && output.is_empty()
                ));
            }
            let mut result = original.clone();
            apply_outer_deadline_outcome(&mut result, &AtomicBool::new(true));
            assert_eq!(result, original);
        }
    }
}

#[cfg(test)]
mod startup_attestation_tests {
    use super::*;

    #[test]
    fn startup_attestation_nonce_and_namespace_pid_are_strict() {
        assert!(valid_attestation_nonce(
            "018f3f32-4f01-4f2c-a6c1-f6f4a81b2903"
        ));
        assert!(!valid_attestation_nonce(
            "018F3F32-4F01-4F2C-A6C1-F6F4A81B2903"
        ));
        assert!(!valid_attestation_nonce("../attestation"));
        assert_eq!(
            parse_namespace_pid_from_nspid("Name:\tagent-runtime\nNSpid:\t4242\t2\n"),
            Some(2)
        );
        assert_eq!(parse_namespace_pid_from_nspid("NSpid:\t0\n"), None);
        assert_eq!(
            parse_namespace_pid_from_nspid("Name:\tagent-runtime\n"),
            None
        );
    }

    #[test]
    fn bounded_jsonl_reader_discards_overflow_and_recovers_at_record_boundary() {
        let mut bytes = vec![b'x'; MAX_WORKBENCH_FRAME_BYTES + 1];
        bytes.push(b'\n');
        bytes.extend_from_slice(
            b"{\"kind\":\"health\",\"schema_version\":1,\"request_id\":\"next\"}\n",
        );
        let mut input = io::Cursor::new(bytes);

        assert!(matches!(
            read_bounded_jsonl_record(&mut input).unwrap(),
            BoundedJsonlRecord::Oversized
        ));
        let BoundedJsonlRecord::Record(record) = read_bounded_jsonl_record(&mut input).unwrap()
        else {
            panic!("the record after an oversized frame must remain readable");
        };
        assert!(matches!(
            serde_json::from_slice::<WorkbenchCommand>(&record).unwrap(),
            WorkbenchCommand::Health { request_id, .. } if request_id == "next"
        ));
        assert!(matches!(
            read_bounded_jsonl_record(&mut input).unwrap(),
            BoundedJsonlRecord::Eof
        ));
    }

    #[test]
    fn command_reader_applies_single_flight_backpressure_without_losing_order() {
        let mut flood = Vec::new();
        for index in 0..128 {
            writeln!(
                flood,
                "{{\"kind\":\"health\",\"schema_version\":1,\"request_id\":\"request-{index}\"}}"
            )
            .unwrap();
        }
        let input = io::Cursor::new(flood);
        let (sender, receiver) = mpsc::sync_channel(MAX_PENDING_READER_EVENTS);
        let reader = thread::spawn(move || read_commands_from(input, sender));

        for index in 0..128 {
            let ReaderEvent::Command(WorkbenchCommand::Health { request_id, .. }) =
                receiver.recv_timeout(Duration::from_secs(1)).unwrap()
            else {
                panic!("reader did not preserve the bounded command order");
            };
            assert_eq!(request_id, format!("request-{index}"));
        }
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            ReaderEvent::Eof
        ));
        reader.join().unwrap();
    }
}
