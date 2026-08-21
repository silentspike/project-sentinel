use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sentinel_common::{
    AgentId, WorkbenchCommand, WorkbenchMessage, WorkbenchOutcome, WorkbenchRequest,
    WorkbenchResourceLimits, WorkbenchTool, WORKBENCH_AGENT_RUNTIME_VERSION,
    WORKBENCH_RUNTIME_BWRAP, WORKBENCH_SCHEMA_VERSION,
};
use serde::Deserialize;

use agent_runtime::WorkbenchExecutor;

const STARTUP_ATTESTATION_SCHEMA_VERSION: u16 = 1;
const STARTUP_ATTESTATION_MAX_BYTES: u64 = 4 * 1024;
const TEST_LANDLOCK_ABI: u8 = 1;
const MAX_WORKBENCH_FRAME_BYTES: usize = 1024 * 1024;
static ATTESTATION_NONCE_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Deserialize)]
struct StartupAttestation {
    schema_version: u16,
    nonce: String,
    wrapper_version: String,
    runtime_version: String,
    landlock_abi: u8,
    host_pid: u32,
}

struct AttestationCleanup(Option<PathBuf>);

impl Drop for AttestationCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}

fn next_attestation_nonce() -> String {
    let counter = ATTESTATION_NONCE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let entropy = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
        ^ (u64::from(std::process::id()) << 16)
        ^ counter;
    format!(
        "018f3f32-4f01-4f2c-a6c1-{:012x}",
        entropy & 0xffff_ffff_ffff
    )
}

fn spawn_attested_runtime(
    workspace: &Path,
    artifacts: &Path,
) -> (Child, ChildStdin, BufReader<ChildStdout>) {
    let nonce = next_attestation_nonce();
    let wrapper_version = env!("CARGO_PKG_VERSION");
    let attestation_path =
        PathBuf::from(format!("/tmp/.sentinel-workbench-attestation-{nonce}.json"));
    let mut cleanup = AttestationCleanup(Some(attestation_path.clone()));
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-runtime"))
        .env("SENTINEL_WORKSPACE_ROOT", workspace)
        .env("SENTINEL_ARTIFACT_ROOT", artifacts)
        .env("SENTINEL_WORKBENCH_ATTESTATION_NONCE", &nonce)
        .env("SENTINEL_WORKBENCH_WRAPPER_VERSION", wrapper_version)
        .env(
            "SENTINEL_WORKBENCH_LANDLOCK_ABI",
            TEST_LANDLOCK_ABI.to_string(),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let bytes = loop {
        match fs::symlink_metadata(&attestation_path) {
            Ok(metadata) => {
                assert!(metadata.is_file() && !metadata.file_type().is_symlink());
                assert_eq!(metadata.nlink(), 1);
                assert_eq!(metadata.permissions().mode() & 0o7777, 0o600);
                assert!(metadata.len() <= STARTUP_ATTESTATION_MAX_BYTES);
                if metadata.len() == 0 {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("agent-runtime startup attestation remained empty");
                    }
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
                break fs::read(&attestation_path).unwrap();
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("agent-runtime startup attestation timed out");
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("inspect agent-runtime startup attestation: {error}"),
        }
    };
    let attestation: StartupAttestation = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        attestation.schema_version,
        STARTUP_ATTESTATION_SCHEMA_VERSION
    );
    assert_eq!(attestation.nonce, nonce);
    assert_eq!(attestation.wrapper_version, wrapper_version);
    assert_eq!(attestation.runtime_version, WORKBENCH_AGENT_RUNTIME_VERSION);
    assert_eq!(attestation.landlock_abi, TEST_LANDLOCK_ABI);
    assert_eq!(attestation.host_pid, child.id());
    fs::remove_file(&attestation_path).unwrap();
    cleanup.0 = None;
    let input = child.stdin.take().unwrap();
    let output = BufReader::new(child.stdout.take().unwrap());
    (child, input, output)
}

fn read_runtime_line(child: &mut Child, output: &mut BufReader<ChildStdout>, line: &mut String) {
    line.clear();
    if output.read_line(line).unwrap() > 0 {
        return;
    }
    let status = match child.try_wait().unwrap() {
        Some(status) => status,
        None => {
            let _ = child.kill();
            child.wait().unwrap()
        }
    };
    let mut diagnostics = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        stderr.read_to_string(&mut diagnostics).unwrap();
    }
    panic!("agent-runtime closed stdout unexpectedly ({status}): {diagnostics}");
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap()
}

fn write_request() -> WorkbenchRequest {
    WorkbenchRequest {
        schema_version: WORKBENCH_SCHEMA_VERSION,
        invocation_id: "018f3f32-4f01-7f2c-a6c1-f6f4a81b2901".to_string(),
        agent_id: AgentId(7),
        project_id: "project-01".to_string(),
        work_item_id: "work-04".to_string(),
        workspace_id: "project-01:work-04".to_string(),
        caller_id: "AGENT-07".to_string(),
        caller_role: "developer".to_string(),
        assignment_version: 2,
        credential_generation: 3,
        policy_digest: "a".repeat(64),
        tool_profile: "web-authoring-v1".to_string(),
        tool_profile_digest: "b".repeat(64),
        runtime_key: WORKBENCH_RUNTIME_BWRAP.to_string(),
        capabilities: BTreeSet::from(["file.write".to_string()]),
        output_artifact_kinds: BTreeSet::from(["source_tree".to_string()]),
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
            path: "src/index.html".to_string(),
            content: "<!doctype html>".to_string(),
            expected_sha256: None,
        },
        input_digest: String::new(),
    }
    .bind_digest()
    .unwrap()
}

fn cancellable_command_request() -> WorkbenchRequest {
    let mut request = write_request();
    request.invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2902".to_string();
    request.capabilities = BTreeSet::from(["command.run_allowlisted".to_string()]);
    request.command_policy = vec![sentinel_common::CommandRule {
        program: "sleep".to_string(),
        required_arg_prefix: Vec::new(),
        max_args: 1,
    }];
    request.tool = WorkbenchTool::RunCommand {
        program: "sleep".to_string(),
        args: vec!["5".to_string()],
    };
    request.input_digest = request.canonical_digest().unwrap();
    request
}

fn prepare_completion_receipt_crash_state(
    workspace: &Path,
    artifacts: &Path,
) -> (WorkbenchRequest, PathBuf, PathBuf) {
    let request = write_request();
    let executor = WorkbenchExecutor::new(workspace, artifacts);
    let result = executor.execute(request.clone(), Arc::new(AtomicBool::new(false)));
    assert!(matches!(
        result,
        WorkbenchMessage::Result {
            outcome: WorkbenchOutcome::Succeeded,
            ..
        }
    ));
    executor.persist_completion_receipt(&result).unwrap();
    let receipt_directory = artifacts.join(".workbench-receipts");
    let receipt = receipt_directory.join(format!("{}.json", request.invocation_id));
    let temporary = receipt_directory.join(format!(".{}.deadbeef.tmp", request.invocation_id));
    fs::hard_link(&receipt, &temporary).unwrap();
    (request, receipt, temporary)
}

#[test]
fn startup_reconciles_root_receipt_hardlink_before_recover_and_readiness() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    let artifacts = directory.path().join("artifacts");
    let (request, receipt, temporary) =
        prepare_completion_receipt_crash_state(&workspace, &artifacts);
    assert_eq!(fs::metadata(&receipt).unwrap().nlink(), 2);

    let (mut child, mut input, mut output) = spawn_attested_runtime(&workspace, &artifacts);
    assert!(!temporary.exists());
    assert_eq!(fs::metadata(&receipt).unwrap().nlink(), 1);
    writeln!(
        input,
        "{}",
        serde_json::to_string(&WorkbenchCommand::Recover {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: request.invocation_id.clone(),
            input_digest: request.input_digest.clone(),
        })
        .unwrap()
    )
    .unwrap();
    input.flush().unwrap();

    let mut recovered = false;
    let mut completed = false;
    while !completed {
        let mut line = String::new();
        read_runtime_line(&mut child, &mut output, &mut line);
        match serde_json::from_str::<WorkbenchMessage>(&line).unwrap() {
            WorkbenchMessage::Result {
                invocation_id,
                input_digest,
                outcome: WorkbenchOutcome::Succeeded,
                resources,
                output,
                ..
            } if invocation_id == request.invocation_id => {
                assert_eq!(input_digest, request.input_digest);
                assert_eq!(resources.bytes_written, 15);
                assert!(output.is_empty());
                recovered = true;
            }
            WorkbenchMessage::Progress {
                invocation_id,
                stage: sentinel_common::WorkbenchProgressStage::Completed,
                ..
            } if invocation_id == request.invocation_id => completed = true,
            _ => {}
        }
    }
    assert!(recovered);
    drop(input);
    assert!(child.wait().unwrap().success());
    assert!(receipt.exists());
}

#[test]
fn conflicting_root_receipt_temp_keeps_runtime_unavailable_and_preserves_evidence() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    let artifacts = directory.path().join("artifacts");
    let (_request, receipt, temporary) =
        prepare_completion_receipt_crash_state(&workspace, &artifacts);
    fs::remove_file(&temporary).unwrap();
    let conflict = artifacts
        .join(".workbench-receipts")
        .join("conflicting-evidence");
    fs::write(&conflict, b"conflicting receipt evidence").unwrap();
    fs::set_permissions(&conflict, fs::Permissions::from_mode(0o600)).unwrap();
    fs::hard_link(&conflict, &temporary).unwrap();
    let receipt_identity = (
        fs::metadata(&receipt).unwrap().dev(),
        fs::metadata(&receipt).unwrap().ino(),
    );
    let temporary_identity = (
        fs::metadata(&temporary).unwrap().dev(),
        fs::metadata(&temporary).unwrap().ino(),
    );

    let nonce = next_attestation_nonce();
    let attestation_path =
        PathBuf::from(format!("/tmp/.sentinel-workbench-attestation-{nonce}.json"));
    let output = Command::new(env!("CARGO_BIN_EXE_agent-runtime"))
        .env("SENTINEL_WORKSPACE_ROOT", &workspace)
        .env("SENTINEL_ARTIFACT_ROOT", &artifacts)
        .env("SENTINEL_WORKBENCH_ATTESTATION_NONCE", &nonce)
        .env(
            "SENTINEL_WORKBENCH_WRAPPER_VERSION",
            env!("CARGO_PKG_VERSION"),
        )
        .env(
            "SENTINEL_WORKBENCH_LANDLOCK_ABI",
            TEST_LANDLOCK_ABI.to_string(),
        )
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(126));
    assert!(output.stdout.is_empty());
    assert!(!attestation_path.exists());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("startup completion receipt recovery failed"));
    assert_eq!(
        (
            fs::metadata(&receipt).unwrap().dev(),
            fs::metadata(&receipt).unwrap().ino()
        ),
        receipt_identity
    );
    assert_eq!(
        (
            fs::metadata(&temporary).unwrap().dev(),
            fs::metadata(&temporary).unwrap().ino(),
        ),
        temporary_identity
    );
    assert!(conflict.exists());
}

#[test]
fn runtime_without_workbench_attestation_uses_general_agent_mode() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-runtime"))
        .env_remove("SENTINEL_WORKBENCH_ATTESTATION_NONCE")
        .env_remove("SENTINEL_WORKBENCH_WRAPPER_VERSION")
        .env_remove("SENTINEL_WORKBENCH_LANDLOCK_ABI")
        .env_remove("SENTINEL_WORKSPACE_ROOT")
        .env_remove("SENTINEL_ARTIFACT_ROOT")
        .env_remove("SENTINEL_INPUT_ROOT")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();

    thread::sleep(Duration::from_millis(150));
    assert!(
        child.try_wait().unwrap().is_none(),
        "general agent runtime must remain alive while stdin is open"
    );
    writeln!(input, "shutdown").unwrap();
    input.flush().unwrap();
    drop(input);

    let deadline = Instant::now() + Duration::from_secs(2);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let status = child.wait().unwrap();
            panic!("general agent runtime did not stop after shutdown: {status}");
        }
        thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success());

    let mut diagnostics = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut diagnostics)
        .unwrap();
    assert!(diagnostics.contains("agent-runtime: started"));
    assert!(diagnostics.contains("agent-runtime: shutting down"));
    assert!(!diagnostics.contains("workbench started"));
}

#[test]
fn partial_workbench_attestation_cannot_downgrade_to_general_agent_mode() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    let artifacts = directory.path().join("artifacts");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&artifacts).unwrap();
    let nonce = next_attestation_nonce();
    let attestation_path =
        PathBuf::from(format!("/tmp/.sentinel-workbench-attestation-{nonce}.json"));

    let output = Command::new(env!("CARGO_BIN_EXE_agent-runtime"))
        .env("SENTINEL_WORKSPACE_ROOT", workspace)
        .env("SENTINEL_ARTIFACT_ROOT", artifacts)
        .env("SENTINEL_WORKBENCH_ATTESTATION_NONCE", &nonce)
        .env_remove("SENTINEL_WORKBENCH_WRAPPER_VERSION")
        .env_remove("SENTINEL_WORKBENCH_LANDLOCK_ABI")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(126));
    assert!(output.stdout.is_empty());
    assert!(!attestation_path.exists());
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostics.contains("startup isolation attestation failed"));
    assert!(!diagnostics.contains("agent-runtime: started"));
}

#[test]
fn jsonl_process_handles_health_rejection_and_execution() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    let artifacts = directory.path().join("artifacts");
    let (mut child, mut input, mut output) = spawn_attested_runtime(&workspace, &artifacts);

    writeln!(input, "{{\"kind\":\"ambient_shell\"}}").unwrap();
    writeln!(
        input,
        "{}",
        serde_json::to_string(&WorkbenchCommand::Health {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            request_id: "health-1".to_string(),
        })
        .unwrap()
    )
    .unwrap();
    let request = write_request();
    writeln!(
        input,
        "{}",
        serde_json::to_string(&WorkbenchCommand::Execute {
            request: Box::new(request.clone()),
        })
        .unwrap()
    )
    .unwrap();
    input.flush().unwrap();

    let mut malformed_rejected = false;
    let mut healthy = false;
    let mut succeeded = false;
    let mut immediate_output = None;
    let mut immediate_resources = None;
    let mut completed = false;
    while !completed {
        let mut line = String::new();
        read_runtime_line(&mut child, &mut output, &mut line);
        match serde_json::from_str::<WorkbenchMessage>(&line).unwrap() {
            WorkbenchMessage::Error { error, .. } if error.code == "malformed_message" => {
                malformed_rejected = true;
            }
            WorkbenchMessage::Health {
                request_id,
                healthy: true,
                ..
            } if request_id == "health-1" => healthy = true,
            WorkbenchMessage::Result {
                outcome: WorkbenchOutcome::Succeeded,
                resources,
                output,
                ..
            } => {
                assert!(!output.values().any(|value| value.contains("secret")));
                immediate_output = Some(output);
                immediate_resources = Some(resources);
                succeeded = true;
            }
            WorkbenchMessage::Progress {
                stage: sentinel_common::WorkbenchProgressStage::Completed,
                ..
            } => {
                completed = true;
            }
            _ => {}
        }
    }
    writeln!(
        input,
        "{}",
        serde_json::to_string(&WorkbenchCommand::Recover {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: request.invocation_id.clone(),
            input_digest: request.input_digest.clone(),
        })
        .unwrap()
    )
    .unwrap();
    input.flush().unwrap();
    let mut recovered = false;
    let mut recovery_completed = false;
    while !recovery_completed {
        let mut line = String::new();
        read_runtime_line(&mut child, &mut output, &mut line);
        match serde_json::from_str::<WorkbenchMessage>(&line).unwrap() {
            WorkbenchMessage::Result {
                invocation_id,
                input_digest,
                outcome: WorkbenchOutcome::Succeeded,
                resources,
                output,
                ..
            } if invocation_id == request.invocation_id => {
                assert_eq!(input_digest, request.input_digest);
                assert!(immediate_output
                    .as_ref()
                    .is_some_and(|output| !output.is_empty()));
                assert_eq!(Some(resources), immediate_resources);
                assert!(output.is_empty());
                recovered = true;
            }
            WorkbenchMessage::Progress {
                invocation_id,
                stage: sentinel_common::WorkbenchProgressStage::Completed,
                ..
            } if invocation_id == request.invocation_id => recovery_completed = true,
            _ => {}
        }
    }
    drop(input);
    assert!(child.wait().unwrap().success());
    assert!(malformed_rejected && healthy && succeeded && recovered);
    assert_eq!(
        std::fs::read_to_string(
            workspace
                .join("project-01")
                .join("work-04")
                .join("src/index.html"),
        )
        .unwrap(),
        "<!doctype html>"
    );
    let mut diagnostics = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut diagnostics)
        .unwrap();
    assert!(!diagnostics.contains("<!doctype html>"));
}

#[test]
fn cancel_waits_for_the_receipted_result_instead_of_acknowledging_early() {
    let directory = tempfile::tempdir().unwrap();
    let (mut child, mut input, mut output) = spawn_attested_runtime(
        &directory.path().join("workspace"),
        &directory.path().join("artifacts"),
    );
    let request = cancellable_command_request();
    writeln!(
        input,
        "{}",
        serde_json::to_string(&WorkbenchCommand::Execute {
            request: Box::new(request.clone()),
        })
        .unwrap()
    )
    .unwrap();
    input.flush().unwrap();

    let mut cancel_sent = false;
    let mut cancelled_result = false;
    let mut completed = false;
    while !completed {
        let mut line = String::new();
        read_runtime_line(&mut child, &mut output, &mut line);
        match serde_json::from_str::<WorkbenchMessage>(&line).unwrap() {
            WorkbenchMessage::Progress {
                stage: sentinel_common::WorkbenchProgressStage::Executing,
                ..
            } if !cancel_sent => {
                writeln!(
                    input,
                    "{}",
                    serde_json::to_string(&WorkbenchCommand::Cancel {
                        schema_version: WORKBENCH_SCHEMA_VERSION,
                        invocation_id: request.invocation_id.clone(),
                        reason: "test_cancel".to_string(),
                    })
                    .unwrap()
                )
                .unwrap();
                input.flush().unwrap();
                cancel_sent = true;
            }
            WorkbenchMessage::Result {
                outcome: WorkbenchOutcome::Cancelled,
                ..
            } => cancelled_result = true,
            WorkbenchMessage::Progress {
                stage: sentinel_common::WorkbenchProgressStage::Completed,
                ..
            } => completed = true,
            WorkbenchMessage::Cancelled { .. } => {
                panic!("cancel must not acknowledge before the receipted result")
            }
            _ => {}
        }
    }
    assert!(cancel_sent && cancelled_result);
    drop(input);
    assert!(child.wait().unwrap().success());
}

#[test]
fn adapter_deadline_cancel_is_receipted_as_timed_out() {
    let directory = tempfile::tempdir().unwrap();
    let (mut child, mut input, mut output) = spawn_attested_runtime(
        &directory.path().join("workspace"),
        &directory.path().join("artifacts"),
    );
    let request = cancellable_command_request();
    writeln!(
        input,
        "{}",
        serde_json::to_string(&WorkbenchCommand::Execute {
            request: Box::new(request.clone()),
        })
        .unwrap()
    )
    .unwrap();
    input.flush().unwrap();

    let mut cancel_sent = false;
    let mut timed_out = false;
    let mut completed = false;
    while !completed {
        let mut line = String::new();
        read_runtime_line(&mut child, &mut output, &mut line);
        match serde_json::from_str::<WorkbenchMessage>(&line).unwrap() {
            WorkbenchMessage::Progress {
                stage: sentinel_common::WorkbenchProgressStage::Executing,
                ..
            } if !cancel_sent => {
                writeln!(
                    input,
                    "{}",
                    serde_json::to_string(&WorkbenchCommand::Cancel {
                        schema_version: WORKBENCH_SCHEMA_VERSION,
                        invocation_id: request.invocation_id.clone(),
                        reason: "deadline_expired".to_string(),
                    })
                    .unwrap()
                )
                .unwrap();
                input.flush().unwrap();
                cancel_sent = true;
            }
            WorkbenchMessage::Result {
                outcome: WorkbenchOutcome::TimedOut,
                error: Some(error),
                ..
            } => {
                assert_eq!(error.code, "deadline_expired");
                timed_out = true;
            }
            WorkbenchMessage::Progress {
                stage: sentinel_common::WorkbenchProgressStage::Completed,
                ..
            } => completed = true,
            WorkbenchMessage::Cancelled { .. } => {
                panic!("deadline cancel must be acknowledged by its receipted result")
            }
            _ => {}
        }
    }
    assert!(cancel_sent && timed_out);
    drop(input);
    assert!(child.wait().unwrap().success());
}

#[test]
fn receipt_failure_is_one_terminal_error_without_completed_after_it() {
    let directory = tempfile::tempdir().unwrap();
    let artifact_root = directory.path().join("artifacts");
    fs::create_dir(&artifact_root).unwrap();
    let (mut child, mut input, mut output) =
        spawn_attested_runtime(&directory.path().join("workspace"), &artifact_root);
    fs::remove_dir(&artifact_root).unwrap();
    fs::write(&artifact_root, "not a directory").unwrap();
    let request = write_request();
    writeln!(
        input,
        "{}",
        serde_json::to_string(&WorkbenchCommand::Execute {
            request: Box::new(request),
        })
        .unwrap()
    )
    .unwrap();
    input.flush().unwrap();

    let mut saw_terminal_error = false;
    while !saw_terminal_error {
        let mut line = String::new();
        read_runtime_line(&mut child, &mut output, &mut line);
        if let WorkbenchMessage::Error { error, .. } =
            serde_json::from_str::<WorkbenchMessage>(&line).unwrap()
        {
            assert_eq!(error.code, "completion_receipt_io_failed");
            saw_terminal_error = true;
        }
    }
    drop(input);
    let mut remaining = String::new();
    output.read_to_string(&mut remaining).unwrap();
    assert!(child.wait().unwrap().success());
    assert!(!remaining.contains("\"stage\":\"completed\""));
}

#[test]
fn oversized_jsonl_frame_is_rejected_without_poisoning_following_health() {
    let directory = tempfile::tempdir().unwrap();
    let (mut child, mut input, mut output) = spawn_attested_runtime(
        &directory.path().join("workspace"),
        &directory.path().join("artifacts"),
    );
    input
        .write_all(&vec![b'x'; MAX_WORKBENCH_FRAME_BYTES + 1])
        .unwrap();
    input.write_all(b"\n").unwrap();
    writeln!(
        input,
        "{}",
        serde_json::to_string(&WorkbenchCommand::Health {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            request_id: "after-overflow".to_string(),
        })
        .unwrap()
    )
    .unwrap();
    input.flush().unwrap();

    let mut oversized_rejected = false;
    let mut healthy_after_overflow = false;
    while !oversized_rejected || !healthy_after_overflow {
        let mut line = String::new();
        read_runtime_line(&mut child, &mut output, &mut line);
        match serde_json::from_str::<WorkbenchMessage>(&line).unwrap() {
            WorkbenchMessage::Error { error, .. } if error.code == "frame_too_large" => {
                oversized_rejected = true;
            }
            WorkbenchMessage::Health {
                request_id,
                healthy: true,
                ..
            } if request_id == "after-overflow" => healthy_after_overflow = true,
            _ => {}
        }
    }
    drop(input);
    assert!(child.wait().unwrap().success());
}
