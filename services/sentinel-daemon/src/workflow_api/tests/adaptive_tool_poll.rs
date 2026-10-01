use super::*;
use std::cell::Cell;
use std::collections::BTreeMap;

use crate::workbench::{
    ReservationOutcome, WorkbenchCoordinator, WorkbenchCoordinatorUpdate, WorkbenchInvocationStore,
    WorkbenchRuntimeClient, WorkbenchRuntimeExchange,
};
use sentinel_common::{
    NanoExecRequest, NanoExecResult, WorkbenchErrorClass, WorkbenchErrorInfo, WorkbenchMessage,
    WorkbenchOutcome, WorkbenchResourceUsage, WORKBENCH_RETAIN_OBSERVATION,
};

const RESERVED_AT: u64 = 1_900_000_000_000;

struct ToolFixture {
    store: WorkbenchInvocationStore,
    request: WorkbenchRequest,
    effect: AdaptiveEffectV1,
    profile: WorkbenchProfile,
    authority: WorkbenchAuthoritySnapshot,
    reserved: WorkbenchInvocationRecord,
    _directory: tempfile::TempDir,
}

impl ToolFixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let store =
            WorkbenchInvocationStore::open(directory.path().join("workbench.redb")).unwrap();
        let request = WorkbenchRequest {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: Uuid::new_v4().to_string(),
            agent_id: AgentId(7),
            project_id: "project-tool-poll".into(),
            work_item_id: "work-tool-poll".into(),
            workspace_id: "project-tool-poll:work-tool-poll".into(),
            caller_id: "AGENT-07".into(),
            caller_role: "developer".into(),
            assignment_version: 2,
            credential_generation: 3,
            policy_digest: "a".repeat(64),
            tool_profile: "web-authoring-v1".into(),
            tool_profile_digest: "b".repeat(64),
            runtime_key: WORKBENCH_RUNTIME_BWRAP.into(),
            capabilities: BTreeSet::from([
                "file.write".into(),
                WORKBENCH_RETAIN_OBSERVATION.into(),
            ]),
            output_artifact_kinds: BTreeSet::from(["source_tree".into()]),
            inputs: Vec::new(),
            command_policy: Vec::new(),
            resource_limits: WorkbenchResourceLimits {
                wall_time_ms: 30_000,
                cpu_time_ms: 10_000,
                memory_bytes: 128 * 1024 * 1024,
                process_count: 16,
                file_bytes: 1024 * 1024,
                stdout_bytes: 64 * 1024,
                stderr_bytes: 64 * 1024,
            },
            deadline_unix_ms: RESERVED_AT + 60_000,
            attempt: 1,
            tool: WorkbenchTool::WriteFile {
                path: "src/main.py".into(),
                content: "print('original')".into(),
                expected_sha256: None,
            },
            input_digest: String::new(),
        }
        .bind_digest()
        .unwrap();
        let profile = WorkbenchProfile {
            schema_version: 1,
            id: request.tool_profile.clone(),
            runtime_key: request.runtime_key.clone(),
            network: "none".into(),
            environment: BTreeMap::new(),
            capabilities: request.capabilities.clone(),
            output_artifact_kinds: request.output_artifact_kinds.clone(),
            resource_ceilings: request.resource_limits.clone(),
            command_rules: Vec::new(),
            test_suites: Vec::new(),
        };
        let granted = request.capabilities.clone();
        let authority = WorkbenchAuthoritySnapshot {
            agent_id: request.agent_id,
            caller_id: request.caller_id.clone(),
            caller_role: request.caller_role.clone(),
            project_id: request.project_id.clone(),
            work_item_id: request.work_item_id.clone(),
            assignment_version: request.assignment_version,
            credential_generation: request.credential_generation,
            policy_digest: request.policy_digest.clone(),
            tool_profile: request.tool_profile.clone(),
            tool_profile_digest: request.tool_profile_digest.clone(),
            runtime_key: request.runtime_key.clone(),
            assignment_active: true,
            agent_capabilities: granted.clone(),
            role_capabilities: granted.clone(),
            assignment_capabilities: granted.clone(),
            project_capabilities: granted.clone(),
            profile_capabilities: granted,
        };
        let ReservationOutcome::Reserved(reserved) = store.reserve(&request, RESERVED_AT).unwrap()
        else {
            panic!("fresh fixture invocation must reserve once");
        };
        let effect = AdaptiveEffectV1 {
            id: Uuid::parse_str(&request.invocation_id).unwrap(),
            request_digest: request.input_digest.clone(),
        };
        Self {
            store,
            request,
            effect,
            profile,
            authority,
            reserved,
            _directory: directory,
        }
    }

    fn coordinator(&self) -> WorkbenchCoordinator<'_> {
        WorkbenchCoordinator::new(
            &self.store,
            &self.profile,
            &self.request.tool_profile_digest,
        )
    }

    fn executing(&self) -> WorkbenchInvocationRecord {
        self.store
            .mark_executing(
                &self.request.invocation_id,
                &self.request.input_digest,
                RESERVED_AT + 1,
            )
            .unwrap()
    }

    fn result(&self, outcome: WorkbenchOutcome) -> WorkbenchMessage {
        WorkbenchMessage::Result {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: self.request.invocation_id.clone(),
            input_digest: self.request.input_digest.clone(),
            outcome,
            resources: WorkbenchResourceUsage::default(),
            artifacts: Vec::new(),
            output: BTreeMap::from([("content".into(), "retained tool observation".into())]),
            error: (outcome != WorkbenchOutcome::Succeeded).then(|| WorkbenchErrorInfo {
                class: WorkbenchErrorClass::Tool,
                code: "tool_failed".into(),
                safe_message: "tool failed".into(),
                retryable: false,
            }),
        }
    }

    fn load(&self) -> WorkbenchInvocationRecord {
        self.store
            .load(&self.request.invocation_id)
            .unwrap()
            .unwrap()
    }
}

fn update(record: WorkbenchInvocationRecord) -> WorkbenchCoordinatorUpdate {
    WorkbenchCoordinatorUpdate {
        records: vec![record],
        runtime_state: None,
        replayed: true,
        caller_result: None,
    }
}

#[test]
fn executing_polls_once_and_returns_succeeded_with_retained_observation() {
    let fixture = ToolFixture::new();
    let executing = fixture.executing();
    let polls = Cell::new(0);
    let completed =
        poll_executing_adaptive_tool(update(executing.clone()), &fixture.effect, || {
            polls.set(polls.get() + 1);
            let result = fixture.result(WorkbenchOutcome::Succeeded);
            let succeeded = fixture
                .store
                .accept_result(&result, RESERVED_AT + 2)
                .unwrap();
            let mut polled = update(succeeded);
            polled.records.insert(0, executing.clone());
            polled.runtime_state = Some("completed".into());
            polled.caller_result = Some(result);
            Ok(polled)
        })
        .unwrap();
    assert_eq!(polls.get(), 1);
    let record = completed.records.last().unwrap();
    assert_eq!(record.state, WorkbenchInvocationState::Succeeded);
    assert_eq!(record.invocation_id, fixture.request.invocation_id);
    assert_eq!(record.request_digest, fixture.effect.request_digest);
    assert_eq!(record.attempt, executing.attempt);
    assert_eq!(fixture.load(), *record);
    let observation = fixture
        .coordinator()
        .private_observation(&record.invocation_id, &fixture.authority)
        .unwrap()
        .unwrap();
    observation
        .validate(&record.invocation_id, &record.request_digest)
        .unwrap();
    assert_eq!(
        record.observation_digest.as_deref(),
        Some(observation.digest())
    );
    assert_eq!(
        observation.output().get("content").unwrap(),
        "retained tool observation"
    );
}

#[test]
fn pending_poll_preserves_exact_record_counters_identity_and_update() {
    let fixture = ToolFixture::new();
    let executing = fixture.executing();
    let mut polled = update(executing.clone());
    polled.runtime_state = Some("pending".into());
    polled.replayed = false;
    let polls = Cell::new(0);
    for _ in 0..2 {
        let result =
            poll_executing_adaptive_tool(update(executing.clone()), &fixture.effect, || {
                polls.set(polls.get() + 1);
                Ok(polled.clone())
            })
            .unwrap();
        assert!(result == polled);
        assert_eq!(fixture.load(), executing);
        assert_eq!(fixture.effect.id.to_string(), executing.invocation_id);
        assert_eq!(fixture.effect.request_digest, executing.request_digest);
    }
    assert_eq!(polls.get(), 2, "each reconciliation performs only one poll");
}

#[test]
fn reserved_and_terminal_replays_never_poll_or_change_durable_records() {
    let fixture = ToolFixture::new();
    let reserved = update(fixture.reserved.clone());
    let result = poll_executing_adaptive_tool(reserved.clone(), &fixture.effect, || {
        panic!("Reserved must not poll");
    })
    .unwrap();
    assert!(result == reserved);
    assert_eq!(fixture.load(), fixture.reserved);
    for outcome in [
        WorkbenchOutcome::Succeeded,
        WorkbenchOutcome::Failed,
        WorkbenchOutcome::Cancelled,
        WorkbenchOutcome::TimedOut,
    ] {
        let fixture = ToolFixture::new();
        fixture.executing();
        let terminal = fixture
            .store
            .accept_result(&fixture.result(outcome), RESERVED_AT + 2)
            .unwrap();
        let mut runtime = TestRuntime {
            calls: Vec::new(),
            response: None,
        };
        for _ in 0..2 {
            let replay = fixture
                .coordinator()
                .submit(
                    &mut runtime,
                    &fixture.request,
                    &fixture.authority,
                    fixture.request.deadline_unix_ms + 1,
                )
                .unwrap();
            assert!(replay.replayed);
            assert_eq!(replay.records, vec![terminal.clone()]);
            let result = poll_executing_adaptive_tool(replay.clone(), &fixture.effect, || {
                panic!("terminal replay must never poll");
            })
            .unwrap();
            assert!(result == replay);
            assert!(runtime.calls.is_empty());
            assert_eq!(fixture.load(), terminal);
        }
    }
}

#[test]
fn mismatched_initial_identity_rejects_before_poll_including_terminal_replay() {
    for terminal in [false, true] {
        let fixture = ToolFixture::new();
        let executing = fixture.executing();
        let original = if terminal {
            fixture
                .store
                .accept_result(
                    &fixture.result(WorkbenchOutcome::Succeeded),
                    RESERVED_AT + 2,
                )
                .unwrap()
        } else {
            executing
        };
        for wrong_id in [false, true] {
            let mut wrong = original.clone();
            if wrong_id {
                wrong.invocation_id = Uuid::new_v4().to_string();
            } else {
                wrong.request_digest = "c".repeat(64);
            }
            let mut initial = update(original.clone());
            initial.records.push(wrong);
            let result = poll_executing_adaptive_tool(initial, &fixture.effect, || {
                panic!("identity mismatch must reject before polling");
            });
            assert!(matches!(result, Err(WorkflowPortError::AuthorityConflict)));
            assert_eq!(fixture.load(), original);
        }
    }
}

#[test]
fn mismatched_final_identity_is_not_adopted_after_exactly_one_poll() {
    let fixture = ToolFixture::new();
    let executing = fixture.executing();
    for wrong_id in [false, true] {
        let foreign = ToolFixture::new();
        foreign.executing();
        let mut wrong = foreign
            .store
            .accept_result(
                &foreign.result(WorkbenchOutcome::Succeeded),
                RESERVED_AT + 2,
            )
            .unwrap();
        if wrong_id {
            wrong.request_digest = fixture.effect.request_digest.clone();
        } else {
            wrong.invocation_id = fixture.effect.id.to_string();
        }
        let polls = Cell::new(0);
        let result =
            poll_executing_adaptive_tool(update(executing.clone()), &fixture.effect, || {
                polls.set(polls.get() + 1);
                let mut polled = update(executing.clone());
                polled.records.push(wrong);
                Ok(polled)
            });
        assert_eq!(polls.get(), 1);
        assert!(matches!(result, Err(WorkflowPortError::AuthorityConflict)));
        assert_eq!(fixture.load(), executing);
        assert!(fixture.load().observation_digest.is_none());
    }
}

#[test]
fn empty_initial_or_polled_records_cannot_be_adopted() {
    let fixture = ToolFixture::new();
    let executing = fixture.executing();
    let mut empty = update(executing.clone());
    empty.records.clear();
    let result = poll_executing_adaptive_tool(empty.clone(), &fixture.effect, || {
        panic!("missing initial identity must reject before polling");
    });
    assert!(matches!(result, Err(WorkflowPortError::UnknownOutcome)));
    let polls = Cell::new(0);
    let result = poll_executing_adaptive_tool(update(executing.clone()), &fixture.effect, || {
        polls.set(polls.get() + 1);
        Ok(empty)
    });
    assert_eq!(polls.get(), 1);
    assert!(matches!(result, Err(WorkflowPortError::UnknownOutcome)));
    assert_eq!(fixture.load(), executing);
}

#[test]
fn poll_errors_are_propagated_without_changing_the_existing_effect() {
    let fixture = ToolFixture::new();
    let executing = fixture.executing();
    for error in [
        WorkflowPortError::Unavailable,
        WorkflowPortError::AuthorityConflict,
        WorkflowPortError::Rejected,
        WorkflowPortError::TimedOut,
        WorkflowPortError::UnknownOutcome,
    ] {
        let polls = Cell::new(0);
        let result =
            poll_executing_adaptive_tool(update(executing.clone()), &fixture.effect, || {
                polls.set(polls.get() + 1);
                Err(error.clone())
            });
        assert_eq!(polls.get(), 1);
        assert!(matches!(result, Err(actual) if actual == error));
        assert_eq!(fixture.load(), executing);
    }
}

struct TestRuntime {
    calls: Vec<(AgentId, NanoExecRequest)>,
    response: Option<NanoExecResult>,
}

impl WorkbenchRuntimeClient for TestRuntime {
    fn exchange(
        &mut self,
        agent_id: AgentId,
        request: NanoExecRequest,
    ) -> anyhow::Result<WorkbenchRuntimeExchange<'_>> {
        self.calls.push((agent_id, request));
        let result = self.response.take().expect("unexpected runtime exchange");
        Ok(WorkbenchRuntimeExchange::new(result, || Ok(())))
    }
}

#[test]
fn coordinator_rejects_expired_fresh_and_reserved_submit_without_start() {
    let fixture = ToolFixture::new();
    let coordinator = fixture.coordinator();
    let mut runtime = TestRuntime {
        calls: Vec::new(),
        response: None,
    };
    for now in [
        fixture.request.deadline_unix_ms,
        fixture.request.deadline_unix_ms + 1,
    ] {
        let mut fresh = fixture.request.clone();
        fresh.invocation_id = Uuid::new_v4().to_string();
        fresh = fresh.bind_digest().unwrap();
        assert!(coordinator
            .submit(&mut runtime, &fresh, &fixture.authority, now)
            .is_err());
        assert!(fixture.store.load(&fresh.invocation_id).unwrap().is_none());
        assert!(coordinator
            .submit(&mut runtime, &fixture.request, &fixture.authority, now)
            .is_err());
        assert!(runtime.calls.is_empty());
        assert_eq!(fixture.load(), fixture.reserved);
    }
}

#[test]
fn coordinator_replays_expired_executing_then_helper_polls_only_same_effect() {
    let fixture = ToolFixture::new();
    let executing = fixture.executing();
    let coordinator = fixture.coordinator();
    let mut runtime = TestRuntime {
        calls: Vec::new(),
        response: Some(NanoExecResult {
            runtime_key: WORKBENCH_RUNTIME_BWRAP.into(),
            workload_id: "AGENT-07".into(),
            success: true,
            output: serde_json::json!({
                "schema_version": WORKBENCH_SCHEMA_VERSION,
                "invocation_id": fixture.request.invocation_id,
                "state": "pending", "messages": []
            })
            .to_string(),
        }),
    };
    let now = fixture.request.deadline_unix_ms + 1;
    let submitted = coordinator
        .submit(&mut runtime, &fixture.request, &fixture.authority, now)
        .unwrap();
    assert!(
        runtime.calls.is_empty(),
        "Executing replay must not Start again"
    );
    assert!(submitted.replayed);
    assert_eq!(submitted.records, vec![executing.clone()]);
    assert_eq!(fixture.load(), executing);
    let polled = poll_executing_adaptive_tool(submitted, &fixture.effect, || {
        coordinator
            .poll(
                &mut runtime,
                &fixture.effect.id.to_string(),
                &fixture.authority,
                now,
            )
            .map_err(|_| WorkflowPortError::UnknownOutcome)
    })
    .unwrap();
    assert_eq!(runtime.calls.len(), 1);
    let (agent, request) = &runtime.calls[0];
    assert_eq!(*agent, fixture.request.agent_id);
    assert_eq!(request.operation, "workbench_poll");
    let frame: serde_json::Value = serde_json::from_str(&request.input).unwrap();
    assert_eq!(
        frame,
        serde_json::json!({
            "kind": "poll", "schema_version": WORKBENCH_SCHEMA_VERSION,
            "invocation_id": fixture.effect.id.to_string()
        })
    );
    assert_eq!(polled.records.last().unwrap(), &executing);
    assert_eq!(polled.runtime_state.as_deref(), Some("pending"));
    assert_eq!(fixture.load(), executing);
}

#[test]
fn unknown_outcome_never_polls_again_through_submit_helper() {
    let fixture = ToolFixture::new();
    fixture.executing();
    let mut runtime = TestRuntime {
        calls: Vec::new(),
        response: Some(NanoExecResult {
            runtime_key: WORKBENCH_RUNTIME_BWRAP.into(),
            workload_id: "AGENT-07".into(),
            success: true,
            output: "{}".into(),
        }),
    };
    // A rejected runtime response creates UnknownOutcome through the real
    // coordinator/store path; it is not a fabricated persisted state.
    let unknown = fixture
        .coordinator()
        .poll(
            &mut runtime,
            &fixture.request.invocation_id,
            &fixture.authority,
            RESERVED_AT + 2,
        )
        .unwrap();
    assert_eq!(runtime.calls.len(), 1);
    let record = fixture.load();
    assert_eq!(record.state, WorkbenchInvocationState::UnknownOutcome);
    let result = poll_executing_adaptive_tool(unknown.clone(), &fixture.effect, || {
        panic!("UnknownOutcome remains on the Recover path, not this Poll helper");
    })
    .unwrap();
    assert!(result == unknown);
    assert_eq!(fixture.load(), record);
    assert_eq!(runtime.calls.len(), 1);
}

#[test]
fn invocation_status_missing_is_read_only_and_does_not_reserve() {
    let fixture = ToolFixture::new();
    let missing = Uuid::new_v4().to_string();
    let path = fixture._directory.path().join("workbench.redb");
    let before = fs::read(&path).unwrap();
    for _ in 0..2 {
        assert!(fixture
            .coordinator()
            .invocation_status(&missing, &fixture.authority)
            .unwrap()
            .is_none());
        assert!(fixture.store.load(&missing).unwrap().is_none());
        assert_eq!(fixture.load(), fixture.reserved);
    }
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn invocation_status_preserves_reserved_executing_and_terminal_records() {
    let fixture = ToolFixture::new();
    let path = fixture._directory.path().join("workbench.redb");
    for state in [
        WorkbenchInvocationState::Reserved,
        WorkbenchInvocationState::Executing,
        WorkbenchInvocationState::Succeeded,
    ] {
        let expected = match state {
            WorkbenchInvocationState::Reserved => fixture.reserved.clone(),
            WorkbenchInvocationState::Executing => fixture.executing(),
            WorkbenchInvocationState::Succeeded => fixture
                .store
                .accept_result(
                    &fixture.result(WorkbenchOutcome::Succeeded),
                    RESERVED_AT + 2,
                )
                .unwrap(),
            _ => unreachable!(),
        };
        let before = fs::read(&path).unwrap();
        for _ in 0..2 {
            let status = fixture
                .coordinator()
                .invocation_status(&fixture.request.invocation_id, &fixture.authority)
                .unwrap()
                .unwrap();
            assert_eq!(status.state, state);
            assert_eq!(status, expected);
            assert_eq!(fixture.load(), expected);
        }
        // Status has no runtime or event output. Also check the entire durable
        // store bytes, not just the selected record, remain unchanged by reads.
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}

#[test]
fn invocation_status_rejects_wrong_authority_and_profile_without_writes() {
    let fixture = ToolFixture::new();
    let expected = fixture.executing();
    let path = fixture._directory.path().join("workbench.redb");
    let before = fs::read(&path).unwrap();
    let mut wrong_project = fixture.authority.clone();
    wrong_project.project_id = "project-foreign".into();
    let mut wrong_work = fixture.authority.clone();
    wrong_work.work_item_id = "work-foreign".into();
    let mut wrong_agent = fixture.authority.clone();
    wrong_agent.agent_id = AgentId(8);
    let mut stale_assignment = fixture.authority.clone();
    stale_assignment.assignment_version += 1;
    let mut wrong_digest = fixture.authority.clone();
    wrong_digest.tool_profile_digest = "c".repeat(64);
    let mut inactive = fixture.authority.clone();
    inactive.assignment_active = false;
    let mut revoked = fixture.authority.clone();
    revoked.assignment_capabilities.clear();
    for authority in [
        wrong_project,
        wrong_work,
        wrong_agent,
        stale_assignment,
        wrong_digest,
        inactive,
        revoked,
    ] {
        assert!(fixture
            .coordinator()
            .invocation_status(&fixture.request.invocation_id, &authority)
            .is_err());
        assert_eq!(fixture.load(), expected);
    }
    let mut wrong_profile = fixture.profile.clone();
    wrong_profile.id = "web-review-v1".into();
    let wrong_profile_digest = "c".repeat(64);
    for (profile, digest) in [
        (&wrong_profile, fixture.request.tool_profile_digest.as_str()),
        (&fixture.profile, wrong_profile_digest.as_str()),
    ] {
        let coordinator = WorkbenchCoordinator::new(&fixture.store, profile, digest);
        assert!(coordinator
            .invocation_status(&fixture.request.invocation_id, &fixture.authority)
            .is_err());
        assert_eq!(fixture.load(), expected);
    }
    assert_eq!(fs::read(&path).unwrap(), before);
}
