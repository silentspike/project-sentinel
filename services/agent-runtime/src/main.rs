//! Isolated agent workbench process.
//!
//! The daemon owns durable invocation state and sends newline-delimited JSON
//! commands over stdin. This process validates every effect-bearing request,
//! executes it inside the already-selected sandbox, and returns structured JSON
//! messages over stdout. Human diagnostics on stderr never include request data.

use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use agent_runtime::WorkbenchExecutor;
use sentinel_common::{
    WorkbenchCommand, WorkbenchErrorClass, WorkbenchErrorInfo, WorkbenchMessage,
    WorkbenchProgressStage, WORKBENCH_SCHEMA_VERSION,
};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const INPUT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

type ActiveInvocations = Arc<Mutex<BTreeMap<String, Arc<AtomicBool>>>>;

enum ReaderEvent {
    Command(WorkbenchCommand),
    Malformed,
    Eof,
}

fn main() {
    eprintln!(
        "agent-runtime: workbench started (pid={})",
        std::process::id()
    );

    let workspace_root = std::env::var_os("SENTINEL_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/workspace"));
    let artifact_root = std::env::var_os("SENTINEL_ARTIFACT_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/artifacts"));
    let executor = Arc::new(WorkbenchExecutor::new(workspace_root, artifact_root));
    let active: ActiveInvocations = Arc::new(Mutex::new(BTreeMap::new()));
    let output_lock = Arc::new(Mutex::new(()));
    let running = Arc::new(AtomicBool::new(true));
    let (sender, receiver) = mpsc::channel();

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

fn read_commands(sender: mpsc::Sender<ReaderEvent>) {
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let event = match line {
            Ok(line) if !line.trim().is_empty() => {
                match serde_json::from_str::<WorkbenchCommand>(&line) {
                    Ok(command) => ReaderEvent::Command(command),
                    Err(_) => ReaderEvent::Malformed,
                }
            }
            Ok(_) => ReaderEvent::Malformed,
            Err(_) => break,
        };
        if sender.send(event).is_err() {
            return;
        }
    }
    let _ = sender.send(ReaderEvent::Eof);
}

fn handle_command(
    command: WorkbenchCommand,
    executor: Arc<WorkbenchExecutor>,
    active: ActiveInvocations,
    output_lock: Arc<Mutex<()>>,
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
                    active_guard.insert(invocation_id.clone(), cancellation.clone());
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
                let result = executor.execute(*request, cancellation);
                match executor.persist_completion_receipt(&result) {
                    Ok(()) => emit(&output_lock, &result),
                    Err(error) => emit(
                        &output_lock,
                        &WorkbenchMessage::Error {
                            schema_version: WORKBENCH_SCHEMA_VERSION,
                            invocation_id: Some(invocation_id.clone()),
                            error,
                        },
                    ),
                };
                let mut active_guard = active.lock().unwrap_or_else(|error| error.into_inner());
                active_guard.remove(&invocation_id);
                drop(active_guard);
                emit_progress(
                    &output_lock,
                    &invocation_id,
                    WorkbenchProgressStage::Completed,
                    elapsed_ms(started),
                );
            });
        }
        WorkbenchCommand::Cancel {
            schema_version,
            invocation_id,
            reason: _,
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
            match cancellation {
                Some(cancellation) => {
                    cancellation.store(true, Ordering::Release);
                    emit(
                        &output_lock,
                        &WorkbenchMessage::Cancelled {
                            schema_version: WORKBENCH_SCHEMA_VERSION,
                            invocation_id,
                        },
                    );
                }
                None => emit(
                    &output_lock,
                    &WorkbenchMessage::Error {
                        schema_version: WORKBENCH_SCHEMA_VERSION,
                        invocation_id: Some(invocation_id),
                        error: protocol_error(
                            "invocation_not_active",
                            "the invocation is not active in this runtime",
                        ),
                    },
                ),
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
            cancellation.store(true, Ordering::Release);
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

fn emit_progress(
    output_lock: &Arc<Mutex<()>>,
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

fn emit(output_lock: &Arc<Mutex<()>>, message: &WorkbenchMessage) {
    let _guard = output_lock
        .lock()
        .unwrap_or_else(|error| error.into_inner());
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
