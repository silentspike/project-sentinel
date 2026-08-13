use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use sentinel_common::{
    AgentId, WorkbenchCommand, WorkbenchMessage, WorkbenchOutcome, WorkbenchRequest,
    WorkbenchResourceLimits, WorkbenchTool, WORKBENCH_RUNTIME_BWRAP, WORKBENCH_SCHEMA_VERSION,
};

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

#[test]
fn jsonl_process_handles_health_rejection_and_execution() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    let artifacts = directory.path().join("artifacts");
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-runtime"))
        .env("SENTINEL_WORKSPACE_ROOT", &workspace)
        .env("SENTINEL_ARTIFACT_ROOT", &artifacts)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());

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
    let mut completed = false;
    while !completed {
        let mut line = String::new();
        assert!(output.read_line(&mut line).unwrap() > 0);
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
                output,
                ..
            } => {
                assert!(!output.values().any(|value| value.contains("secret")));
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
        assert!(output.read_line(&mut line).unwrap() > 0);
        match serde_json::from_str::<WorkbenchMessage>(&line).unwrap() {
            WorkbenchMessage::Result {
                invocation_id,
                input_digest,
                outcome: WorkbenchOutcome::Succeeded,
                output,
                ..
            } if invocation_id == request.invocation_id => {
                assert_eq!(input_digest, request.input_digest);
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
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-runtime"))
        .env("SENTINEL_WORKSPACE_ROOT", directory.path().join("workspace"))
        .env("SENTINEL_ARTIFACT_ROOT", directory.path().join("artifacts"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
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
        assert!(output.read_line(&mut line).unwrap() > 0);
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
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-runtime"))
        .env("SENTINEL_WORKSPACE_ROOT", directory.path().join("workspace"))
        .env("SENTINEL_ARTIFACT_ROOT", directory.path().join("artifacts"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
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
        assert!(output.read_line(&mut line).unwrap() > 0);
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
    let artifact_file = directory.path().join("artifact-root-is-a-file");
    std::fs::write(&artifact_file, "not a directory").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-runtime"))
        .env("SENTINEL_WORKSPACE_ROOT", directory.path().join("workspace"))
        .env("SENTINEL_ARTIFACT_ROOT", &artifact_file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
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
        assert!(output.read_line(&mut line).unwrap() > 0);
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
