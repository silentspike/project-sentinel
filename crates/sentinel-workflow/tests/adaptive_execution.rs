use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use sentinel_common::WorkbenchTool;
use sentinel_workflow::{
    adaptive_tool_digest, AdaptiveCursorV1, AdaptiveEffectV1, AdaptiveModelDecisionV1,
    AdaptiveModelObservationV1, AdaptiveModelPort, AdaptiveObservationRefV1,
    AdaptiveSessionGrantV1, AdaptiveToolObservationV1, AdaptiveToolPort, AdaptiveTransitionV1,
    AdaptiveWorkflowCore, AgentId, DependencyReadiness, OrganizationRuntimePort,
    PrincipalAuthorityV1, ProjectId, RuntimeAuthoritySnapshotV1, TenantId, WorkItemId,
    WorkflowErrorCode, WorkflowPortError, WorkflowStore, WORKFLOW_SCHEMA_VERSION,
};
use tempfile::tempdir;
use uuid::Uuid;

const NOW: u64 = 1_900_000_000_000;

fn authority() -> RuntimeAuthoritySnapshotV1 {
    RuntimeAuthoritySnapshotV1 {
        schema_version: WORKFLOW_SCHEMA_VERSION,
        tenant_id: TenantId::parse("tenant-01").unwrap(),
        project_id: ProjectId::parse("project-01").unwrap(),
        work_item_id: WorkItemId::parse("work-01").unwrap(),
        agent_id: AgentId(7),
        assignment_version: 3,
        assignment_digest: "1".repeat(64),
        organization_generation: 9,
        organization_digest: "2".repeat(64),
        principal: PrincipalAuthorityV1::derive("agent-07", 4, &[0x5a; 32]).unwrap(),
        profile_id: "coding-agent-v1".to_owned(),
        profile_generation: 2,
        profile_digest: "3".repeat(64),
        runtime_key: "bwrap-coding-v1".to_owned(),
        runtime_generation: 2,
        runtime_digest: "4".repeat(64),
        policy_generation: 6,
        policy_digest: "5".repeat(64),
        active: true,
        capabilities: BTreeSet::from([
            "file.inspect".to_owned(),
            "observation.retain_private".to_owned(),
        ]),
    }
}

fn grant(authority: RuntimeAuthoritySnapshotV1, model_calls: u16) -> AdaptiveSessionGrantV1 {
    AdaptiveSessionGrantV1 {
        schema_version: 1,
        session_id: Uuid::parse_str("01991c34-e03c-70c2-b97e-0591f4be2211").unwrap(),
        authority,
        provider_allowance_id: "subscription-adaptive".to_owned(),
        provider_authority_digest: "6".repeat(64),
        provider: "codex-cli".to_owned(),
        model: "gpt-5.6-luna".to_owned(),
        catalog_digest: "7".repeat(64),
        max_output_tokens: 4096,
        max_call_duration_ms: 120_000,
        max_model_calls: model_calls,
        max_tool_calls: 2,
        created_at_ms: NOW,
        deadline_ms: NOW + 60_000,
    }
}

fn effect(id: &str, digest: char) -> AdaptiveEffectV1 {
    AdaptiveEffectV1 {
        id: Uuid::parse_str(id).unwrap(),
        request_digest: digest.to_string().repeat(64),
    }
}

fn inspect_tool() -> WorkbenchTool {
    WorkbenchTool::InspectFile {
        path: "src/main.rs".to_owned(),
        max_bytes: 16 * 1024,
    }
}

#[derive(Clone)]
struct Organization {
    authority: Arc<Mutex<RuntimeAuthoritySnapshotV1>>,
}

impl OrganizationRuntimePort for Organization {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Ready
    }

    fn authority_snapshot(
        &self,
        _tenant_id: &TenantId,
        _project_id: &ProjectId,
        _work_item_id: &WorkItemId,
        _agent_id: AgentId,
    ) -> Result<RuntimeAuthoritySnapshotV1, WorkflowPortError> {
        Ok(self.authority.lock().unwrap().clone())
    }
}

struct Model {
    authority: Arc<Mutex<RuntimeAuthoritySnapshotV1>>,
    rotate_during_io: bool,
}

impl AdaptiveModelPort for Model {
    fn reconcile_model(
        &self,
        _session: &sentinel_workflow::AdaptiveSessionV1,
        _effect: &AdaptiveEffectV1,
    ) -> Result<AdaptiveModelObservationV1, WorkflowPortError> {
        if self.rotate_during_io {
            self.authority.lock().unwrap().policy_generation += 1;
        }
        Ok(AdaptiveModelObservationV1::Completed {
            result_digest: "8".repeat(64),
            decision: AdaptiveModelDecisionV1::Tool {
                tool: inspect_tool(),
                tool_digest: adaptive_tool_digest(&inspect_tool()).unwrap(),
            },
        })
    }
}

struct Tool;

impl AdaptiveToolPort for Tool {
    fn reconcile_tool(
        &self,
        _session: &sentinel_workflow::AdaptiveSessionV1,
        _effect: &AdaptiveEffectV1,
        tool: &WorkbenchTool,
        tool_digest: &str,
    ) -> Result<AdaptiveToolObservationV1, WorkflowPortError> {
        assert_eq!(tool, &inspect_tool());
        assert_eq!(tool_digest, adaptive_tool_digest(tool).unwrap());
        Ok(AdaptiveToolObservationV1::Completed {
            observation_digest: "a".repeat(64),
        })
    }
}

#[test]
fn failed_tool_observation_drives_a_second_model_round_and_survives_reopen() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    let auth = authority();
    let grant = grant(auth.clone(), 2);
    let store = WorkflowStore::open(&database).unwrap();
    let (replayed, mut session) = store.begin_adaptive_session(&grant, &auth, NOW).unwrap();
    assert!(!replayed);

    let model_one = effect("01991c34-e03c-70c2-b97e-0591f4be2212", '7');
    let tool_one = effect("01991c34-e03c-70c2-b97e-0591f4be2213", '8');
    let operations = [
        AdaptiveTransitionV1::ClaimModel {
            effect: model_one.clone(),
            previous_observation_digest: None,
        },
        AdaptiveTransitionV1::ResolveModel {
            effect: model_one,
            result_digest: "9".repeat(64),
            decision: AdaptiveModelDecisionV1::Tool {
                tool: inspect_tool(),
                tool_digest: adaptive_tool_digest(&inspect_tool()).unwrap(),
            },
        },
        AdaptiveTransitionV1::ClaimTool {
            effect: tool_one.clone(),
            tool_digest: adaptive_tool_digest(&inspect_tool()).unwrap(),
        },
        AdaptiveTransitionV1::ObserveTool {
            observation: AdaptiveObservationRefV1 {
                effect: tool_one,
                // The observation may contain a nonzero command status. It is still feedback.
                observation_digest: "b".repeat(64),
            },
        },
    ];
    for (index, command) in operations.iter().enumerate() {
        let operation_id = Uuid::from_u128(0x01991c34e03c70c2b97e0591f4be2300 + index as u128);
        let (was_replay, next) = store
            .advance_adaptive_session(
                grant.session_id,
                session.version,
                operation_id,
                command,
                &auth,
                NOW + 1 + index as u64,
            )
            .unwrap();
        assert!(!was_replay);
        session = next;
    }
    assert_eq!(session.model_calls, 1);
    assert_eq!(session.tool_calls, 1);
    assert!(matches!(session.cursor, AdaptiveCursorV1::ReadyForModel));
    let observation = session.last_observation.clone().unwrap();

    drop(store);
    let reopened = WorkflowStore::open(&database).unwrap();
    assert_eq!(
        reopened.adaptive_session(grant.session_id, &auth).unwrap(),
        Some(session.clone())
    );
    assert_eq!(
        reopened.adaptive_session_for_authority(&auth).unwrap(),
        Some(session.clone())
    );
    let (_, continued) = reopened
        .advance_adaptive_session(
            grant.session_id,
            session.version,
            Uuid::parse_str("01991c34-e03c-70c2-b97e-0591f4be2310").unwrap(),
            &AdaptiveTransitionV1::ClaimModel {
                effect: effect("01991c34-e03c-70c2-b97e-0591f4be2311", 'c'),
                previous_observation_digest: Some(observation.observation_digest),
            },
            &auth,
            NOW + 10,
        )
        .unwrap();
    assert_eq!(continued.model_calls, 2);
    assert!(matches!(
        continued.cursor,
        AdaptiveCursorV1::ModelPending { .. }
    ));
}

#[test]
fn authority_head_is_unique_atomic_and_detects_tampering() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    let auth = authority();
    let grant = grant(auth.clone(), 2);
    let store = WorkflowStore::open(&database).unwrap();
    let (_, initial) = store.begin_adaptive_session(&grant, &auth, NOW).unwrap();

    let mut conflicting = grant.clone();
    conflicting.session_id = Uuid::parse_str("01991c34-e03c-70c2-b97e-0591f4be2611").unwrap();
    assert_eq!(
        store
            .begin_adaptive_session(&conflicting, &auth, NOW)
            .unwrap_err()
            .code,
        WorkflowErrorCode::IdempotencyConflict
    );
    assert_eq!(
        store.adaptive_session_for_authority(&auth).unwrap(),
        Some(initial)
    );
    drop(store);

    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute("UPDATE workflow_adaptive_heads SET version=99", [])
        .unwrap();
    drop(connection);
    let reopened = WorkflowStore::open(&database).unwrap();
    assert_eq!(
        reopened
            .adaptive_session_for_authority(&auth)
            .unwrap_err()
            .code,
        WorkflowErrorCode::CorruptStore
    );
}

#[test]
fn replay_is_exact_but_revocation_limits_and_unknown_effects_fail_closed() {
    let directory = tempdir().unwrap();
    let store = WorkflowStore::open(directory.path().join("workflow.sqlite")).unwrap();
    let auth = authority();
    let grant = grant(auth.clone(), 1);
    let (_, initial) = store.begin_adaptive_session(&grant, &auth, NOW).unwrap();
    let model = effect("01991c34-e03c-70c2-b97e-0591f4be2412", 'd');
    let operation = Uuid::parse_str("01991c34-e03c-70c2-b97e-0591f4be2413").unwrap();
    let command = AdaptiveTransitionV1::ClaimModel {
        effect: model.clone(),
        previous_observation_digest: None,
    };
    let (_, pending) = store
        .advance_adaptive_session(
            grant.session_id,
            initial.version,
            operation,
            &command,
            &auth,
            NOW + 1,
        )
        .unwrap();
    let (replayed, same) = store
        .advance_adaptive_session(
            grant.session_id,
            initial.version,
            operation,
            &command,
            &auth,
            NOW + 1,
        )
        .unwrap();
    assert!(replayed);
    assert_eq!(same, pending);

    let (_, unknown) = store
        .advance_adaptive_session(
            grant.session_id,
            pending.version,
            Uuid::parse_str("01991c34-e03c-70c2-b97e-0591f4be2414").unwrap(),
            &AdaptiveTransitionV1::MarkUnknown {
                effect: model.clone(),
            },
            &auth,
            NOW + 2,
        )
        .unwrap();
    assert!(matches!(
        unknown.cursor,
        AdaptiveCursorV1::ModelUnknown { .. }
    ));
    let duplicate = store.advance_adaptive_session(
        grant.session_id,
        unknown.version,
        Uuid::parse_str("01991c34-e03c-70c2-b97e-0591f4be2415").unwrap(),
        &AdaptiveTransitionV1::ClaimModel {
            effect: model,
            previous_observation_digest: None,
        },
        &auth,
        NOW + 3,
    );
    assert_eq!(
        duplicate.unwrap_err().code,
        WorkflowErrorCode::InvalidTransition
    );

    let mut revoked = auth.clone();
    revoked.active = false;
    assert_eq!(
        store
            .adaptive_session(grant.session_id, &revoked)
            .unwrap_err()
            .code,
        WorkflowErrorCode::AuthorityConflict
    );

    let resolved = store
        .advance_adaptive_session(
            grant.session_id,
            unknown.version,
            Uuid::parse_str("01991c34-e03c-70c2-b97e-0591f4be2416").unwrap(),
            &AdaptiveTransitionV1::ResolveModel {
                effect: match &unknown.cursor {
                    AdaptiveCursorV1::ModelUnknown { effect } => effect.clone(),
                    _ => unreachable!(),
                },
                result_digest: "e".repeat(64),
                decision: AdaptiveModelDecisionV1::Tool {
                    tool: inspect_tool(),
                    tool_digest: adaptive_tool_digest(&inspect_tool()).unwrap(),
                },
            },
            &auth,
            NOW + 4,
        )
        .unwrap()
        .1;
    let tool = effect("01991c34-e03c-70c2-b97e-0591f4be2418", 'a');
    let (_, tool_pending) = store
        .advance_adaptive_session(
            grant.session_id,
            resolved.version,
            Uuid::parse_str("01991c34-e03c-70c2-b97e-0591f4be2417").unwrap(),
            &AdaptiveTransitionV1::ClaimTool {
                effect: tool.clone(),
                tool_digest: adaptive_tool_digest(&inspect_tool()).unwrap(),
            },
            &auth,
            NOW + 5,
        )
        .unwrap();
    let (_, ready_again) = store
        .advance_adaptive_session(
            grant.session_id,
            tool_pending.version,
            Uuid::parse_str("01991c34-e03c-70c2-b97e-0591f4be2419").unwrap(),
            &AdaptiveTransitionV1::ObserveTool {
                observation: AdaptiveObservationRefV1 {
                    effect: tool,
                    observation_digest: "b".repeat(64),
                },
            },
            &auth,
            NOW + 6,
        )
        .unwrap();
    let limit = store.advance_adaptive_session(
        grant.session_id,
        ready_again.version,
        Uuid::parse_str("01991c34-e03c-70c2-b97e-0591f4be2420").unwrap(),
        &AdaptiveTransitionV1::ClaimModel {
            effect: effect("01991c34-e03c-70c2-b97e-0591f4be2421", 'c'),
            previous_observation_digest: Some("b".repeat(64)),
        },
        &auth,
        NOW + 7,
    );
    assert_eq!(
        limit.unwrap_err().code,
        WorkflowErrorCode::InvalidTransition
    );
}

#[test]
fn adaptive_core_rechecks_authority_around_io_and_preserves_pending_effect() {
    let directory = tempdir().unwrap();
    let store = Arc::new(WorkflowStore::open(directory.path().join("workflow.sqlite")).unwrap());
    let auth = authority();
    let grant = grant(auth.clone(), 2);
    let (_, initial) = store.begin_adaptive_session(&grant, &auth, NOW).unwrap();
    let model_effect = effect("01991c34-e03c-70c2-b97e-0591f4be2512", '7');
    let (_, pending) = store
        .advance_adaptive_session(
            grant.session_id,
            initial.version,
            Uuid::parse_str("01991c34-e03c-70c2-b97e-0591f4be2513").unwrap(),
            &AdaptiveTransitionV1::ClaimModel {
                effect: model_effect,
                previous_observation_digest: None,
            },
            &auth,
            NOW + 1,
        )
        .unwrap();
    let current = Arc::new(Mutex::new(auth.clone()));
    let core = AdaptiveWorkflowCore::new(
        Arc::clone(&store),
        Organization {
            authority: Arc::clone(&current),
        },
        Model {
            authority: Arc::clone(&current),
            rotate_during_io: true,
        },
        Tool,
    );
    let error = core
        .reconcile_model(
            grant.session_id,
            &auth,
            Uuid::parse_str("01991c34-e03c-70c2-b97e-0591f4be2514").unwrap(),
            NOW + 2,
        )
        .unwrap_err();
    assert_eq!(error.code, WorkflowErrorCode::AuthorityConflict);
    *current.lock().unwrap() = auth.clone();
    assert_eq!(
        store.adaptive_session(grant.session_id, &auth).unwrap(),
        Some(pending)
    );
}
