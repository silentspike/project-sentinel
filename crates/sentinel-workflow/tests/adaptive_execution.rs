use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use sentinel_common::WorkbenchTool;
use sentinel_workflow::{
    adaptive_collaboration_digest, adaptive_tool_digest, AdaptiveCollaborationActionV1,
    AdaptiveCursorV1, AdaptiveEffectV1, AdaptiveModelDecisionV1, AdaptiveModelObservationV1,
    AdaptiveModelPort, AdaptiveObservationRefV1, AdaptiveSessionGrantV1, AdaptiveToolObservationV1,
    AdaptiveToolPort, AdaptiveTransitionV1, AdaptiveWorkflowCore, AgentId, DependencyReadiness,
    OrganizationRuntimePort, PrincipalAuthorityV1, ProjectId, RuntimeAuthoritySnapshotV1, TenantId,
    UnavailableCompletionEvidencePort, UnavailableGateEvidencePort,
    UnavailableOrganizationRuntimePort, UnavailableWorkExecutionPort, WorkItemId, WorkflowCore,
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

fn advance_replay_fixture(
    store: &WorkflowStore,
    grant: &AdaptiveSessionGrantV1,
    version: u64,
    command: &AdaptiveTransitionV1,
) -> sentinel_workflow::AdaptiveSessionV1 {
    let (replayed, session) = store
        .advance_adaptive_session(
            grant.session_id,
            version,
            Uuid::from_u128(0x01991c34e03c70c2b97e0591f4be2800 + u128::from(version)),
            command,
            &grant.authority,
            NOW + version + 1,
        )
        .unwrap();
    assert!(!replayed);
    session
}

fn persisted_adaptive_rows(database: &std::path::Path) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    let connection = rusqlite::Connection::open(database).unwrap();
    [
        "SELECT * FROM workflow_operations ORDER BY operation_namespace,operation_id",
        "SELECT * FROM workflow_adaptive_heads ORDER BY session_id",
    ]
    .iter()
    .map(|sql| {
        let mut statement = connection.prepare(sql).unwrap();
        let columns = statement.column_count();
        let rows = statement
            .query_map([], |row| {
                (0..columns)
                    .map(|column| row.get::<_, rusqlite::types::Value>(column))
                    .collect::<Result<Vec<_>, _>>()
            })
            .unwrap();
        rows.collect::<Result<Vec<_>, _>>().unwrap()
    })
    .collect()
}

fn renewed_grant(previous: &AdaptiveSessionGrantV1) -> AdaptiveSessionGrantV1 {
    AdaptiveSessionGrantV1 {
        session_id: Uuid::new_v4(),
        provider_allowance_id: Uuid::new_v4().to_string(),
        created_at_ms: previous.deadline_ms,
        deadline_ms: previous.deadline_ms + 60_000,
        ..previous.clone()
    }
}

fn advance_at(
    store: &WorkflowStore,
    grant: &AdaptiveSessionGrantV1,
    version: u64,
    command: &AdaptiveTransitionV1,
    now_ms: u64,
) -> sentinel_workflow::AdaptiveSessionV1 {
    store
        .advance_adaptive_session(
            grant.session_id,
            version,
            Uuid::new_v4(),
            command,
            &grant.authority,
            now_ms,
        )
        .unwrap()
        .1
}

fn journal_entry(database: &std::path::Path, id: Uuid, version: u64) -> serde_json::Value {
    let connection = rusqlite::Connection::open(database).unwrap();
    let bytes: Vec<u8> = connection
        .query_row(
            "SELECT response FROM workflow_operations WHERE operation_namespace=?1 AND operation_id=?2",
            (format!("adaptive-session-v1:{id}"), format!("{version:020}")),
            |row| row.get(0),
        )
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn reject_first_model_fixture(
    store: &WorkflowStore,
    grant: &AdaptiveSessionGrantV1,
) -> sentinel_workflow::AdaptiveSessionV1 {
    let model = effect(&Uuid::new_v4().to_string(), 'a');
    let claimed = advance_at(
        store,
        grant,
        1,
        &AdaptiveTransitionV1::ClaimModel {
            effect: model.clone(),
            previous_observation_digest: None,
        },
        grant.created_at_ms + 1,
    );
    advance_at(
        store,
        grant,
        claimed.version,
        &AdaptiveTransitionV1::RejectModel {
            effect: model,
            resolution_event_id: Uuid::new_v4().to_string(),
            reason_code: "schema_error".into(),
        },
        grant.created_at_ms + 2,
    )
}

#[test]
fn durable_model_adoption_is_exact_read_only_and_survives_later_results() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    let auth = authority();
    let grant = grant(auth.clone(), 2);
    let store = WorkflowStore::open(&database).unwrap();
    let (_, initial) = store.begin_adaptive_session(&grant, &auth, NOW).unwrap();
    let model = effect("01991c34-e03c-70c2-b97e-0591f4be2811", '7');
    let result_digest = "9".repeat(64);
    let decision = AdaptiveModelDecisionV1::Tool {
        tool: inspect_tool(),
        tool_digest: adaptive_tool_digest(&inspect_tool()).unwrap(),
    };
    let before = persisted_adaptive_rows(&database);
    assert!(!store
        .adaptive_model_result_is_adopted(&grant, &model, &result_digest, &decision)
        .unwrap());
    let mut absent_grant = grant.clone();
    absent_grant.session_id = Uuid::parse_str("01991c34-e03c-70c2-b97e-0591f4be2814").unwrap();
    assert_eq!(
        store
            .adaptive_model_result_is_adopted(&absent_grant, &model, &result_digest, &decision)
            .unwrap_err()
            .code,
        WorkflowErrorCode::NotFound
    );
    assert_eq!(persisted_adaptive_rows(&database), before);
    let tool = effect("01991c34-e03c-70c2-b97e-0591f4be2812", '8');
    let second_model = effect("01991c34-e03c-70c2-b97e-0591f4be2813", 'a');
    let resolution = AdaptiveTransitionV1::ResolveModel {
        effect: model.clone(),
        result_digest: result_digest.clone(),
        decision: decision.clone(),
    };
    let commands = [
        AdaptiveTransitionV1::ClaimModel {
            effect: model.clone(),
            previous_observation_digest: None,
        },
        resolution.clone(),
        AdaptiveTransitionV1::ClaimTool {
            effect: tool.clone(),
            tool_digest: adaptive_tool_digest(&inspect_tool()).unwrap(),
        },
        AdaptiveTransitionV1::ObserveTool {
            observation: AdaptiveObservationRefV1 {
                effect: tool,
                observation_digest: "b".repeat(64),
            },
        },
        AdaptiveTransitionV1::ClaimModel {
            effect: second_model.clone(),
            previous_observation_digest: Some("b".repeat(64)),
        },
        AdaptiveTransitionV1::ResolveModel {
            effect: second_model.clone(),
            result_digest: "c".repeat(64),
            decision: AdaptiveModelDecisionV1::Blocked {
                reason_code: "needs_input".to_owned(),
            },
        },
    ];
    let mut session = initial;
    let mut resolved = None;
    for (index, command) in commands.iter().enumerate() {
        session = advance_replay_fixture(&store, &grant, session.version, command);
        if index == 1 {
            resolved = Some(session.clone());
        }
    }
    drop(store);
    let reopened = WorkflowStore::open(&database).unwrap();
    let before = persisted_adaptive_rows(&database);
    assert!(reopened
        .adaptive_model_result_is_adopted(&grant, &model, &result_digest, &decision)
        .unwrap());
    assert!(!reopened
        .adaptive_model_result_is_adopted(&grant, &model, &"d".repeat(64), &decision)
        .unwrap());
    for altered_effect in [
        second_model,
        AdaptiveEffectV1 {
            request_digest: "d".repeat(64),
            ..model.clone()
        },
        AdaptiveEffectV1 {
            id: Uuid::nil(),
            ..model.clone()
        },
    ] {
        assert!(!reopened
            .adaptive_model_result_is_adopted(&grant, &altered_effect, &result_digest, &decision)
            .unwrap());
    }
    let changed_tool = WorkbenchTool::InspectFile {
        path: "src/other.rs".to_owned(),
        max_bytes: 16 * 1024,
    };
    for altered_decision in [
        AdaptiveModelDecisionV1::Tool {
            tool_digest: adaptive_tool_digest(&changed_tool).unwrap(),
            tool: changed_tool,
        },
        AdaptiveModelDecisionV1::Tool {
            tool: inspect_tool(),
            tool_digest: "d".repeat(64),
        },
        AdaptiveModelDecisionV1::Blocked {
            reason_code: "needs_input".to_owned(),
        },
    ] {
        assert!(!reopened
            .adaptive_model_result_is_adopted(&grant, &model, &result_digest, &altered_decision)
            .unwrap());
    }
    let mut altered_limit = grant.clone();
    altered_limit.max_model_calls += 1;
    let mut altered_provider = grant.clone();
    altered_provider.provider_authority_digest = "d".repeat(64);
    let mut altered_deadline = grant.clone();
    altered_deadline.deadline_ms += 1;
    for altered_grant in [altered_limit, altered_provider, altered_deadline] {
        assert_eq!(
            reopened
                .adaptive_model_result_is_adopted(&altered_grant, &model, &result_digest, &decision)
                .unwrap_err()
                .code,
            WorkflowErrorCode::IdempotencyConflict
        );
    }
    let mut altered_grant = grant.clone();
    altered_grant.authority.policy_generation += 1;
    assert_eq!(
        reopened
            .adaptive_model_result_is_adopted(&altered_grant, &model, &result_digest, &decision)
            .unwrap_err()
            .code,
        WorkflowErrorCode::AuthorityConflict
    );
    altered_grant.authority.active = false;
    assert!(reopened
        .adaptive_model_result_is_adopted(&altered_grant, &model, &result_digest, &decision)
        .is_err());
    assert_eq!(
        reopened.adaptive_session(grant.session_id, &auth).unwrap(),
        Some(session.clone())
    );
    // The new query leaves the existing operation replay contract untouched.
    let (replayed, response) = reopened
        .advance_adaptive_session(
            grant.session_id,
            2,
            Uuid::from_u128(0x01991c34e03c70c2b97e0591f4be2802),
            &resolution,
            &auth,
            grant.deadline_ms + 1,
        )
        .unwrap();
    assert!(replayed);
    assert_eq!(response, resolved.unwrap());
    assert_eq!(persisted_adaptive_rows(&database), before);
}

#[test]
fn terminal_non_collaboration_resolutions_are_adopted_on_reopen() {
    for decision in [
        AdaptiveModelDecisionV1::ProposeCompletion {
            artifact_digest: "a".repeat(64),
        },
        AdaptiveModelDecisionV1::Blocked {
            reason_code: "needs_input".to_owned(),
        },
    ] {
        let directory = tempdir().unwrap();
        let database = directory.path().join("workflow.sqlite");
        let auth = authority();
        let grant = grant(auth.clone(), 1);
        let store = WorkflowStore::open(&database).unwrap();
        store.begin_adaptive_session(&grant, &auth, NOW).unwrap();
        let model = effect("01991c34-e03c-70c2-b97e-0591f4be2821", '7');
        advance_replay_fixture(
            &store,
            &grant,
            1,
            &AdaptiveTransitionV1::ClaimModel {
                effect: model.clone(),
                previous_observation_digest: None,
            },
        );
        advance_replay_fixture(
            &store,
            &grant,
            2,
            &AdaptiveTransitionV1::ResolveModel {
                effect: model.clone(),
                result_digest: "9".repeat(64),
                decision: decision.clone(),
            },
        );
        drop(store);
        let reopened = WorkflowStore::open(&database).unwrap();
        let before = persisted_adaptive_rows(&database);
        assert!(reopened
            .adaptive_model_result_is_adopted(&grant, &model, &"9".repeat(64), &decision)
            .unwrap());
        assert_eq!(persisted_adaptive_rows(&database), before);
    }
}

#[test]
fn collaboration_adoption_requires_the_exact_later_commit() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    let auth = authority();
    let grant = grant(auth.clone(), 2);
    let store = WorkflowStore::open(&database).unwrap();
    store.begin_adaptive_session(&grant, &auth, NOW).unwrap();
    let model = effect("01991c34-e03c-70c2-b97e-0591f4be2831", '7');
    let action = AdaptiveCollaborationActionV1::AskQuestion {
        question_ref: "question-01".to_owned(),
    };
    let decision = AdaptiveModelDecisionV1::Collaborate {
        action: action.clone(),
    };
    advance_replay_fixture(
        &store,
        &grant,
        1,
        &AdaptiveTransitionV1::ClaimModel {
            effect: model.clone(),
            previous_observation_digest: None,
        },
    );
    let proposed = advance_replay_fixture(
        &store,
        &grant,
        2,
        &AdaptiveTransitionV1::ResolveModel {
            effect: model.clone(),
            result_digest: "9".repeat(64),
            decision: decision.clone(),
        },
    );
    drop(store);
    let reopened = WorkflowStore::open(&database).unwrap();
    let before = persisted_adaptive_rows(&database);
    assert!(!reopened
        .adaptive_model_result_is_adopted(&grant, &model, &"9".repeat(64), &decision)
        .unwrap());
    assert_eq!(
        reopened.adaptive_session(grant.session_id, &auth).unwrap(),
        Some(proposed.clone())
    );
    for invalid_commit in [
        AdaptiveTransitionV1::CommitCollaboration {
            effect: model.clone(),
            action_digest: "b".repeat(64),
        },
        AdaptiveTransitionV1::CommitCollaboration {
            effect: AdaptiveEffectV1 {
                request_digest: "b".repeat(64),
                ..model.clone()
            },
            action_digest: adaptive_collaboration_digest(&action).unwrap(),
        },
    ] {
        assert_eq!(
            reopened
                .advance_adaptive_session(
                    grant.session_id,
                    proposed.version,
                    Uuid::from_u128(0x01991c34e03c70c2b97e0591f4be2803),
                    &invalid_commit,
                    &auth,
                    NOW + 4,
                )
                .unwrap_err()
                .code,
            WorkflowErrorCode::InvalidTransition
        );
        assert!(!reopened
            .adaptive_model_result_is_adopted(&grant, &model, &"9".repeat(64), &decision)
            .unwrap());
    }
    assert_eq!(persisted_adaptive_rows(&database), before);
    let commit = AdaptiveTransitionV1::CommitCollaboration {
        effect: model.clone(),
        action_digest: adaptive_collaboration_digest(&action).unwrap(),
    };
    let committed = advance_replay_fixture(&reopened, &grant, proposed.version, &commit);
    let advanced = advance_replay_fixture(
        &reopened,
        &grant,
        committed.version,
        &AdaptiveTransitionV1::ClaimModel {
            effect: effect("01991c34-e03c-70c2-b97e-0591f4be2832", 'a'),
            previous_observation_digest: None,
        },
    );
    drop(reopened);
    let reopened = WorkflowStore::open(&database).unwrap();
    let before = persisted_adaptive_rows(&database);
    assert!(reopened
        .adaptive_model_result_is_adopted(&grant, &model, &"9".repeat(64), &decision)
        .unwrap());
    let altered_decision = AdaptiveModelDecisionV1::Collaborate {
        action: AdaptiveCollaborationActionV1::AskQuestion {
            question_ref: "question-02".to_owned(),
        },
    };
    assert!(!reopened
        .adaptive_model_result_is_adopted(&grant, &model, &"9".repeat(64), &altered_decision)
        .unwrap());
    assert!(!reopened
        .adaptive_model_result_is_adopted(&grant, &model, &"b".repeat(64), &decision)
        .unwrap());
    let altered_effect = AdaptiveEffectV1 {
        request_digest: "b".repeat(64),
        ..model.clone()
    };
    assert!(!reopened
        .adaptive_model_result_is_adopted(&grant, &altered_effect, &"9".repeat(64), &decision)
        .unwrap());
    assert_eq!(
        reopened.adaptive_session(grant.session_id, &auth).unwrap(),
        Some(advanced)
    );
    assert_eq!(persisted_adaptive_rows(&database), before);
}

#[test]
fn adoption_query_validates_the_entire_journal_and_current_head() {
    for tampering in [
        "UPDATE workflow_adaptive_heads SET version=99",
        "DELETE FROM workflow_adaptive_heads",
        "UPDATE workflow_operations SET request_digest='invalid' WHERE operation_id='00000000000000000001'",
        "UPDATE workflow_operations SET request_digest='invalid' WHERE operation_id='00000000000000000003'",
        "UPDATE workflow_operations SET request_digest='invalid' WHERE operation_id='00000000000000000004'",
    ] {
        let directory = tempdir().unwrap();
        let database = directory.path().join("workflow.sqlite");
        let auth = authority();
        let grant = grant(auth.clone(), 1);
        let store = WorkflowStore::open(&database).unwrap();
        store.begin_adaptive_session(&grant, &auth, NOW).unwrap();
        let model = effect("01991c34-e03c-70c2-b97e-0591f4be2841", '7');
        let decision = AdaptiveModelDecisionV1::Tool {
            tool: inspect_tool(),
            tool_digest: adaptive_tool_digest(&inspect_tool()).unwrap(),
        };
        advance_replay_fixture(
            &store,
            &grant,
            1,
            &AdaptiveTransitionV1::ClaimModel {
                effect: model.clone(),
                previous_observation_digest: None,
            },
        );
        advance_replay_fixture(
            &store,
            &grant,
            2,
            &AdaptiveTransitionV1::ResolveModel {
                effect: model.clone(),
                result_digest: "9".repeat(64),
                decision: decision.clone(),
            },
        );
        advance_replay_fixture(&store, &grant, 3, &AdaptiveTransitionV1::Cancel);
        drop(store);
        let connection = rusqlite::Connection::open(&database).unwrap();
        assert!(connection.execute(tampering, []).unwrap() > 0);
        drop(connection);
        let reopened = WorkflowStore::open(&database).unwrap();
        let before = persisted_adaptive_rows(&database);
        assert_eq!(
            reopened
                .adaptive_model_result_is_adopted(&grant, &model, &"9".repeat(64), &decision)
                .unwrap_err()
                .code,
            WorkflowErrorCode::CorruptStore,
            "{tampering}"
        );
        assert_eq!(persisted_adaptive_rows(&database), before);
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
    assert_eq!(
        reopened.adaptive_recovery_feedback(&auth).unwrap_err().code,
        WorkflowErrorCode::CorruptStore
    );
}

#[test]
fn expired_effect_free_session_rolls_to_renewed_grant_atomically() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    let auth = authority();
    let old = grant(auth.clone(), 2);
    let store = WorkflowStore::open(&database).unwrap();
    let (_, initial) = store.begin_adaptive_session(&old, &auth, NOW).unwrap();
    let mut renewed = old.clone();
    renewed.session_id = Uuid::from_u128(0x01991c34e03c70c2b97e0591f4be2212);
    renewed.provider_allowance_id = "subscription-renewed".into();
    renewed.provider_authority_digest = "8".repeat(64);
    renewed.created_at_ms = old.deadline_ms;
    renewed.deadline_ms = renewed.created_at_ms + 60_000;

    let (replayed, next) = store
        .begin_adaptive_session(&renewed, &auth, renewed.created_at_ms)
        .unwrap();
    assert!(!replayed);
    assert_eq!(next.version, 1);
    assert_eq!(
        store.adaptive_session_for_authority(&auth).unwrap(),
        Some(next.clone())
    );
    assert_eq!(
        store
            .begin_adaptive_session(&renewed, &auth, renewed.created_at_ms)
            .unwrap(),
        (true, next.clone())
    );
    assert_eq!(
        store
            .begin_adaptive_session(&old, &auth, NOW)
            .unwrap_err()
            .code,
        WorkflowErrorCode::IdempotencyConflict
    );
    let connection = rusqlite::Connection::open(&database).unwrap();
    let cancelled: Vec<u8> = connection
        .query_row(
            "SELECT response FROM workflow_operations WHERE operation_namespace=?1 AND operation_id=?2",
            (format!("adaptive-session-v1:{}", old.session_id), "00000000000000000002"),
            |row| row.get(0),
        )
        .unwrap();
    let record: serde_json::Value = serde_json::from_slice(&cancelled).unwrap();
    assert_eq!(record["session"]["cursor"]["kind"], "cancelled");
    assert_eq!(initial.model_calls, 0);
    drop(connection);
    drop(store);
    let reopened = WorkflowStore::open(&database).unwrap();
    assert_eq!(
        reopened.adaptive_session_for_authority(&auth).unwrap(),
        Some(next)
    );
}

#[test]
fn renewal_never_replaces_claimed_or_blocked_session() {
    for claimed in [true, false] {
        let directory = tempdir().unwrap();
        let database = directory.path().join("workflow.sqlite");
        let auth = authority();
        let old = grant(auth.clone(), 2);
        let store = WorkflowStore::open(&database).unwrap();
        let (_, initial) = store.begin_adaptive_session(&old, &auth, NOW).unwrap();
        let pending = store
            .advance_adaptive_session(
                old.session_id,
                initial.version,
                Uuid::from_u128(0x01991c34e03c70c2b97e0591f4be2213),
                &AdaptiveTransitionV1::ClaimModel {
                    effect: effect("01991c34-e03c-70c2-b97e-0591f4be2214", 'a'),
                    previous_observation_digest: None,
                },
                &auth,
                NOW + 1,
            )
            .unwrap()
            .1;
        let current = if claimed {
            pending
        } else {
            let AdaptiveCursorV1::ModelPending { effect } = &pending.cursor else {
                panic!("expected pending model");
            };
            store
                .advance_adaptive_session(
                    old.session_id,
                    pending.version,
                    Uuid::from_u128(0x01991c34e03c70c2b97e0591f4be2215),
                    &AdaptiveTransitionV1::ResolveModel {
                        effect: effect.clone(),
                        result_digest: "b".repeat(64),
                        decision: AdaptiveModelDecisionV1::Blocked {
                            reason_code: "needs_input".into(),
                        },
                    },
                    &auth,
                    NOW + 2,
                )
                .unwrap()
                .1
        };
        let before = persisted_adaptive_rows(&database);
        let mut renewed = old.clone();
        renewed.session_id = Uuid::from_u128(0x01991c34e03c70c2b97e0591f4be2216);
        renewed.provider_allowance_id = "subscription-renewed".into();
        renewed.created_at_ms = old.deadline_ms;
        renewed.deadline_ms = renewed.created_at_ms + 60_000;
        assert_eq!(
            store
                .begin_adaptive_session(&renewed, &auth, renewed.created_at_ms)
                .unwrap_err()
                .code,
            WorkflowErrorCode::IdempotencyConflict
        );
        assert_eq!(persisted_adaptive_rows(&database), before);
        assert_eq!(
            store.adaptive_session_for_authority(&auth).unwrap(),
            Some(current)
        );
    }
}

#[test]
fn rejected_first_model_rolls_only_after_durable_rejection_and_expiry() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    let auth = authority();
    let old = grant(auth.clone(), 2);
    let store = WorkflowStore::open(&database).unwrap();
    let (_, initial) = store.begin_adaptive_session(&old, &auth, NOW).unwrap();
    let model = effect("01991c34-e03c-70c2-b97e-0591f4be2311", 'a');
    let claimed = store
        .advance_adaptive_session(
            old.session_id,
            initial.version,
            Uuid::from_u128(0x01991c34e03c70c2b97e0591f4be2312),
            &AdaptiveTransitionV1::ClaimModel {
                effect: model.clone(),
                previous_observation_digest: None,
            },
            &auth,
            NOW + 1,
        )
        .unwrap()
        .1;
    let mut renewed = old.clone();
    renewed.session_id = Uuid::from_u128(0x01991c34e03c70c2b97e0591f4be2313);
    renewed.provider_allowance_id = "subscription-renewed".into();
    renewed.provider_authority_digest = "8".repeat(64);
    renewed.created_at_ms = old.deadline_ms;
    renewed.deadline_ms = renewed.created_at_ms + 60_000;
    assert!(store
        .begin_adaptive_session(&renewed, &auth, renewed.created_at_ms)
        .is_err());
    let rejection = AdaptiveTransitionV1::RejectModel {
        effect: model.clone(),
        resolution_event_id: Uuid::from_u128(0x01991c34e03c70c2b97e0591f4be2314).to_string(),
        reason_code: "adaptive_tool_schema".into(),
    };
    assert!(store
        .advance_adaptive_session(
            old.session_id,
            claimed.version,
            Uuid::from_u128(0x01991c34e03c70c2b97e0591f4be2315),
            &rejection,
            &auth,
            NOW + 2,
        )
        .is_ok());
    assert!(store
        .begin_adaptive_session(&renewed, &auth, old.deadline_ms - 1)
        .is_err());
    let (_, next) = store
        .begin_adaptive_session(&renewed, &auth, renewed.created_at_ms)
        .unwrap();
    assert_eq!(next.version, 1);
    assert!(matches!(next.cursor, AdaptiveCursorV1::ReadyForModel));
    drop(store);
    let reopened = WorkflowStore::open(&database).unwrap();
    assert_eq!(
        reopened.adaptive_session_for_authority(&auth).unwrap(),
        Some(next)
    );
}

#[test]
fn renewed_grant_predating_rejection_keeps_both_journals_monotonic() {
    for use_admission_time in [false, true] {
        let directory = tempdir().unwrap();
        let database = directory.path().join("workflow.sqlite");
        let auth = authority();
        let old = grant(auth.clone(), 2);
        let store = WorkflowStore::open(&database).unwrap();
        store.begin_adaptive_session(&old, &auth, NOW).unwrap();
        let model = AdaptiveEffectV1 {
            id: Uuid::new_v4(),
            request_digest: "a".repeat(64),
        };
        let claimed = advance_at(
            &store,
            &old,
            1,
            &AdaptiveTransitionV1::ClaimModel {
                effect: model.clone(),
                previous_observation_digest: None,
            },
            NOW + 1,
        );
        let renewed = renewed_grant(&old);
        let rejection_time = renewed.created_at_ms + 10;
        let rejected = advance_at(
            &store,
            &old,
            claimed.version,
            &AdaptiveTransitionV1::RejectModel {
                effect: model,
                resolution_event_id: Uuid::new_v4().to_string(),
                reason_code: "adaptive_tool_schema".into(),
            },
            rejection_time,
        );
        let supplied_time = if use_admission_time {
            rejection_time + 1
        } else {
            renewed.created_at_ms
        };
        let unusable = AdaptiveSessionGrantV1 {
            deadline_ms: rejection_time,
            ..renewed.clone()
        };
        let before = persisted_adaptive_rows(&database);
        assert_eq!(
            store
                .begin_adaptive_session(&unusable, &auth, renewed.created_at_ms)
                .unwrap_err()
                .code,
            WorkflowErrorCode::AuthorityConflict
        );
        assert_eq!(persisted_adaptive_rows(&database), before);
        let (_, next) = store
            .begin_adaptive_session(&renewed, &auth, supplied_time)
            .unwrap();
        assert_eq!(next.grant, renewed);
        assert_eq!(next.updated_at_ms, supplied_time.max(rejection_time));
        let cancelled = journal_entry(&database, old.session_id, rejected.version + 1);
        assert_eq!(cancelled["session"]["cursor"]["kind"], "cancelled");
        assert_eq!(cancelled["session"]["updated_at_ms"], next.updated_at_ms);
        assert_eq!(cancelled["session"]["grant"]["created_at_ms"], NOW);
        assert_eq!(
            journal_entry(&database, renewed.session_id, 1)["session"]["grant"]["created_at_ms"],
            renewed.created_at_ms
        );
        let before = persisted_adaptive_rows(&database);
        assert!(store
            .advance_adaptive_session(
                renewed.session_id,
                1,
                Uuid::new_v4(),
                &AdaptiveTransitionV1::ClaimModel {
                    effect: AdaptiveEffectV1 {
                        id: Uuid::new_v4(),
                        request_digest: "b".repeat(64),
                    },
                    previous_observation_digest: None,
                },
                &auth,
                rejection_time - 1,
            )
            .is_err());
        assert_eq!(persisted_adaptive_rows(&database), before);
        drop(store);
        let reopened = WorkflowStore::open(&database).unwrap();
        assert_eq!(
            reopened.adaptive_session_for_authority(&auth).unwrap(),
            Some(next.clone())
        );
        assert_eq!(
            reopened
                .begin_adaptive_session(&renewed, &auth, renewed.deadline_ms + 1)
                .unwrap(),
            (true, next)
        );
        assert!(reopened.adaptive_session(old.session_id, &auth).is_err());
    }
}

#[test]
fn schema_corrections_are_bounded_across_restart_expiry_and_idle_renewal() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    let auth = authority();
    let mut current_grant = grant(auth.clone(), 2);
    let mut previous_feedback: Option<sentinel_workflow::AdaptiveRecoveryFeedbackV1> = None;
    for count in 0..=2 {
        let store = WorkflowStore::open(&database).unwrap();
        let (_, mut session) = store
            .begin_adaptive_session(&current_grant, &auth, current_grant.created_at_ms)
            .unwrap();
        assert_eq!(
            store.adaptive_recovery_feedback(&auth).unwrap(),
            previous_feedback
        );
        if count == 1 {
            // An unused expired allowance cannot erase the previous correction.
            let idle_renewal = renewed_grant(&current_grant);
            session = store
                .begin_adaptive_session(&idle_renewal, &auth, idle_renewal.created_at_ms)
                .unwrap()
                .1;
            current_grant = idle_renewal;
            assert_eq!(
                store.adaptive_recovery_feedback(&auth).unwrap(),
                previous_feedback
            );
        }
        let model = effect(&Uuid::new_v4().to_string(), 'a');
        session = advance_at(
            &store,
            &current_grant,
            session.version,
            &AdaptiveTransitionV1::ClaimModel {
                effect: model.clone(),
                previous_observation_digest: None,
            },
            session.updated_at_ms + 1,
        );
        if count == 0 {
            let before = persisted_adaptive_rows(&database);
            for command in [
                AdaptiveTransitionV1::RejectModel {
                    effect: effect(&Uuid::new_v4().to_string(), 'a'),
                    resolution_event_id: Uuid::new_v4().to_string(),
                    reason_code: "schema_error".into(),
                },
                AdaptiveTransitionV1::RejectModel {
                    effect: model.clone(),
                    resolution_event_id: Uuid::nil().to_string(),
                    reason_code: "schema_error".into(),
                },
                AdaptiveTransitionV1::RejectModel {
                    effect: model.clone(),
                    resolution_event_id: Uuid::new_v4().to_string(),
                    reason_code: "invalid reason".into(),
                },
            ] {
                assert!(store
                    .advance_adaptive_session(
                        current_grant.session_id,
                        session.version,
                        Uuid::new_v4(),
                        &command,
                        &auth,
                        session.updated_at_ms + 1,
                    )
                    .is_err());
            }
            assert_eq!(store.adaptive_recovery_feedback(&auth).unwrap(), None);
            assert_eq!(persisted_adaptive_rows(&database), before);
        }
        let operation_id = Uuid::new_v4();
        let expected_version = session.version;
        let rejection = AdaptiveTransitionV1::RejectModel {
            effect: model,
            resolution_event_id: Uuid::new_v4().to_string(),
            reason_code: format!("schema_rejection_{count}"),
        };
        let rejected = store
            .advance_adaptive_session(
                current_grant.session_id,
                expected_version,
                operation_id,
                &rejection,
                &auth,
                current_grant.deadline_ms + 10,
            )
            .unwrap()
            .1;
        let feedback = store.adaptive_recovery_feedback(&auth).unwrap().unwrap();
        assert_eq!(feedback.count, count);
        assert_eq!(feedback.reason_code, format!("schema_rejection_{count}"));
        assert_eq!(feedback.previous_session_id, current_grant.session_id);
        drop(store);
        let store = WorkflowStore::open(&database).unwrap();
        let before = persisted_adaptive_rows(&database);
        assert_eq!(
            store.adaptive_recovery_feedback(&auth).unwrap(),
            Some(feedback.clone())
        );
        assert_eq!(
            store
                .advance_adaptive_session(
                    current_grant.session_id,
                    expected_version,
                    operation_id,
                    &rejection,
                    &auth,
                    current_grant.deadline_ms + 100,
                )
                .unwrap(),
            (true, rejected.clone())
        );
        assert_eq!(persisted_adaptive_rows(&database), before);
        let next_grant = renewed_grant(&current_grant);
        if count < 2 {
            let (_, next) = store
                .begin_adaptive_session(&next_grant, &auth, next_grant.created_at_ms)
                .unwrap();
            let mut expected = feedback;
            expected.count += 1;
            assert_eq!(
                store.adaptive_recovery_feedback(&auth).unwrap(),
                Some(expected.clone())
            );
            let before = persisted_adaptive_rows(&database);
            assert_eq!(
                store
                    .begin_adaptive_session(&next_grant, &auth, next_grant.deadline_ms + 1)
                    .unwrap(),
                (true, next)
            );
            assert_eq!(persisted_adaptive_rows(&database), before);
            previous_feedback = Some(expected);
            current_grant = next_grant;
        } else {
            for later in [next_grant.created_at_ms, next_grant.created_at_ms + 100] {
                assert_eq!(
                    store
                        .begin_adaptive_session(&next_grant, &auth, later)
                        .unwrap_err()
                        .code,
                    WorkflowErrorCode::IdempotencyConflict
                );
                assert_eq!(persisted_adaptive_rows(&database), before);
            }
            assert_eq!(
                store.adaptive_session_for_authority(&auth).unwrap(),
                Some(rejected)
            );
            let mut reassigned = auth.clone();
            reassigned.assignment_version += 1;
            assert_eq!(store.adaptive_recovery_feedback(&reassigned).unwrap(), None);
            let reassigned_grant = AdaptiveSessionGrantV1 {
                authority: reassigned.clone(),
                ..next_grant
            };
            store
                .begin_adaptive_session(
                    &reassigned_grant,
                    &reassigned,
                    reassigned_grant.created_at_ms,
                )
                .unwrap();
            assert_eq!(store.adaptive_recovery_feedback(&reassigned).unwrap(), None);
            assert_eq!(
                store.adaptive_recovery_feedback(&auth).unwrap(),
                Some(feedback)
            );
        }
    }
}

#[test]
fn failed_rollover_rolls_back_cancellation_head_and_feedback_together() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    let auth = authority();
    let old = grant(auth.clone(), 2);
    let store = WorkflowStore::open(&database).unwrap();
    store.begin_adaptive_session(&old, &auth, NOW).unwrap();
    let rejected = reject_first_model_fixture(&store, &old);
    let renewed = renewed_grant(&old);
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER deny_renewal BEFORE INSERT ON workflow_operations \
             WHEN NEW.operation_namespace='adaptive-session-v1:{}' \
             BEGIN SELECT RAISE(ABORT,'injected rollback'); END;",
            renewed.session_id
        ))
        .unwrap();
    let before = persisted_adaptive_rows(&database);
    assert!(store
        .begin_adaptive_session(&renewed, &auth, renewed.created_at_ms)
        .is_err());
    assert_eq!(persisted_adaptive_rows(&database), before);
    assert_eq!(
        store.adaptive_session_for_authority(&auth).unwrap(),
        Some(rejected)
    );
    assert_eq!(
        store
            .adaptive_recovery_feedback(&auth)
            .unwrap()
            .unwrap()
            .count,
        0
    );
    connection
        .execute_batch("DROP TRIGGER deny_renewal")
        .unwrap();
    store
        .begin_adaptive_session(&renewed, &auth, renewed.created_at_ms)
        .unwrap();
    assert_eq!(
        store
            .adaptive_recovery_feedback(&auth)
            .unwrap()
            .unwrap()
            .count,
        1
    );
}

#[test]
fn explicit_blocked_resolution_preserves_history_and_correction_lineage_on_rollover() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    let auth = authority();
    let original = grant(auth.clone(), 2);
    let store = WorkflowStore::open(&database).unwrap();
    store.begin_adaptive_session(&original, &auth, NOW).unwrap();
    reject_first_model_fixture(&store, &original);
    let current = renewed_grant(&original);
    let (_, mut session) = store
        .begin_adaptive_session(&current, &auth, current.created_at_ms)
        .unwrap();
    let feedback = store.adaptive_recovery_feedback(&auth).unwrap().unwrap();
    assert_eq!(feedback.count, 1);
    let model = effect(&Uuid::new_v4().to_string(), 'b');
    let tool = effect(&Uuid::new_v4().to_string(), 'c');
    let second_model = effect(&Uuid::new_v4().to_string(), 'd');
    let commands = [
        AdaptiveTransitionV1::ClaimModel {
            effect: model.clone(),
            previous_observation_digest: None,
        },
        AdaptiveTransitionV1::ResolveModel {
            effect: model,
            result_digest: "e".repeat(64),
            decision: AdaptiveModelDecisionV1::Tool {
                tool: inspect_tool(),
                tool_digest: adaptive_tool_digest(&inspect_tool()).unwrap(),
            },
        },
        AdaptiveTransitionV1::ClaimTool {
            effect: tool.clone(),
            tool_digest: adaptive_tool_digest(&inspect_tool()).unwrap(),
        },
        AdaptiveTransitionV1::ObserveTool {
            observation: AdaptiveObservationRefV1 {
                effect: tool,
                observation_digest: "f".repeat(64),
            },
        },
        AdaptiveTransitionV1::ClaimModel {
            effect: second_model.clone(),
            previous_observation_digest: Some("f".repeat(64)),
        },
        AdaptiveTransitionV1::ResolveModel {
            effect: second_model,
            result_digest: "1".repeat(64),
            decision: AdaptiveModelDecisionV1::Blocked {
                reason_code: "needs_input".into(),
            },
        },
    ];
    for command in commands {
        session = advance_at(
            &store,
            &current,
            session.version,
            &command,
            session.updated_at_ms + 1,
        );
    }
    let next_grant = renewed_grant(&current);
    let before = persisted_adaptive_rows(&database);
    assert!(store
        .begin_adaptive_session(&next_grant, &auth, next_grant.created_at_ms)
        .is_err());
    for (reason, resolution) in [
        ("different_reason", Uuid::new_v4().to_string()),
        ("needs_input", "not-a-uuid".to_owned()),
        ("needs_input", Uuid::nil().to_string()),
    ] {
        assert_eq!(
            store
                .advance_adaptive_session(
                    current.session_id,
                    session.version,
                    Uuid::new_v4(),
                    &AdaptiveTransitionV1::ResolveBlocked {
                        expected_reason_code: reason.into(),
                        resolution_event_id: resolution,
                    },
                    &auth,
                    current.deadline_ms + 1,
                )
                .unwrap_err()
                .code,
            WorkflowErrorCode::InvalidTransition
        );
    }
    assert_eq!(persisted_adaptive_rows(&database), before);
    let resolution = AdaptiveTransitionV1::ResolveBlocked {
        expected_reason_code: "needs_input".into(),
        resolution_event_id: Uuid::new_v4().to_string(),
    };
    let operation_id = Uuid::new_v4();
    let resolution_time = current.deadline_ms + 20;
    let (_, resolved) = store
        .advance_adaptive_session(
            current.session_id,
            session.version,
            operation_id,
            &resolution,
            &auth,
            resolution_time,
        )
        .unwrap();
    let mut expected = session.clone();
    expected.version += 1;
    expected.updated_at_ms = resolution_time;
    let AdaptiveTransitionV1::ResolveBlocked {
        resolution_event_id,
        ..
    } = &resolution
    else {
        unreachable!();
    };
    expected.cursor = AdaptiveCursorV1::BlockedResolved {
        reason_code: "needs_input".into(),
        resolution_event_id: resolution_event_id.clone(),
    };
    assert_eq!(resolved, expected); // Includes all effect IDs, results and observations.
    let historical = (1..=resolved.version)
        .map(|version| journal_entry(&database, current.session_id, version))
        .collect::<Vec<_>>();
    drop(store);
    let store = WorkflowStore::open(&database).unwrap();
    let before = persisted_adaptive_rows(&database);
    assert_eq!(
        store.adaptive_session_for_authority(&auth).unwrap(),
        Some(resolved.clone())
    );
    assert_eq!(
        store
            .advance_adaptive_session(
                current.session_id,
                session.version,
                operation_id,
                &resolution,
                &auth,
                resolution_time + 1,
            )
            .unwrap(),
        (true, resolved.clone())
    );
    assert!(store
        .advance_adaptive_session(
            current.session_id,
            resolved.version,
            Uuid::new_v4(),
            &resolution,
            &auth,
            resolution_time + 1,
        )
        .is_err());
    assert_eq!(persisted_adaptive_rows(&database), before);
    let (_, fresh) = store
        .begin_adaptive_session(&next_grant, &auth, next_grant.created_at_ms)
        .unwrap();
    assert_eq!(fresh.updated_at_ms, resolution_time);
    assert_eq!(fresh.model_calls, 0);
    assert_eq!(fresh.tool_calls, 0);
    assert!(matches!(fresh.cursor, AdaptiveCursorV1::ReadyForModel));
    assert_eq!(
        store.adaptive_recovery_feedback(&auth).unwrap(),
        Some(feedback.clone())
    );
    for (index, entry) in historical.iter().enumerate() {
        assert_eq!(
            &journal_entry(&database, current.session_id, index as u64 + 1),
            entry
        );
    }
    let cancelled = journal_entry(&database, current.session_id, resolved.version + 1);
    assert_eq!(cancelled["session"]["model_calls"], 2);
    assert_eq!(cancelled["session"]["tool_calls"], 1);
    assert_eq!(
        cancelled["session"]["last_observation"],
        historical.last().unwrap()["session"]["last_observation"]
    );
    drop(store);
    let store = WorkflowStore::open(&database).unwrap();
    assert_eq!(
        store.adaptive_recovery_feedback(&auth).unwrap(),
        Some(feedback)
    );
    assert_eq!(
        store.adaptive_session_for_authority(&auth).unwrap(),
        Some(fresh)
    );
}

#[test]
fn resolved_blocked_session_still_requires_expiry_and_distinct_allowance() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    let auth = authority();
    let old = grant(auth.clone(), 1);
    let store = WorkflowStore::open(&database).unwrap();
    store.begin_adaptive_session(&old, &auth, NOW).unwrap();
    let model = effect(&Uuid::new_v4().to_string(), 'a');
    advance_at(
        &store,
        &old,
        1,
        &AdaptiveTransitionV1::ClaimModel {
            effect: model.clone(),
            previous_observation_digest: None,
        },
        NOW + 1,
    );
    advance_at(
        &store,
        &old,
        2,
        &AdaptiveTransitionV1::ResolveModel {
            effect: model,
            result_digest: "b".repeat(64),
            decision: AdaptiveModelDecisionV1::Blocked {
                reason_code: "needs_input".into(),
            },
        },
        NOW + 2,
    );
    let resolved = advance_at(
        &store,
        &old,
        3,
        &AdaptiveTransitionV1::ResolveBlocked {
            expected_reason_code: "needs_input".into(),
            resolution_event_id: Uuid::new_v4().to_string(),
        },
        NOW + 3,
    );
    let mut early = renewed_grant(&old);
    early.created_at_ms = NOW + 4;
    let mut same_allowance = renewed_grant(&old);
    same_allowance.provider_allowance_id = old.provider_allowance_id.clone();
    let before = persisted_adaptive_rows(&database);
    for candidate in [early, same_allowance] {
        assert_eq!(
            store
                .begin_adaptive_session(&candidate, &auth, candidate.created_at_ms)
                .unwrap_err()
                .code,
            WorkflowErrorCode::IdempotencyConflict
        );
        assert_eq!(persisted_adaptive_rows(&database), before);
    }
    let renewed = renewed_grant(&old);
    let mut revoked = auth.clone();
    revoked.policy_generation += 1;
    assert!(store
        .begin_adaptive_session(&renewed, &revoked, renewed.created_at_ms)
        .is_err());
    assert_eq!(persisted_adaptive_rows(&database), before);
    assert_eq!(
        store.adaptive_session_for_authority(&auth).unwrap(),
        Some(resolved)
    );
    store
        .begin_adaptive_session(&renewed, &auth, renewed.created_at_ms)
        .unwrap();
    assert_eq!(store.adaptive_recovery_feedback(&auth).unwrap(), None);
}

#[test]
fn explicit_blocked_resolution_never_clears_pending_or_unknown_effects() {
    for tool_effect in [false, true] {
        for unknown in [false, true] {
            let directory = tempdir().unwrap();
            let database = directory.path().join("workflow.sqlite");
            let auth = authority();
            let old = grant(auth.clone(), 2);
            let store = WorkflowStore::open(&database).unwrap();
            store.begin_adaptive_session(&old, &auth, NOW).unwrap();
            let model = effect(&Uuid::new_v4().to_string(), 'a');
            let mut session = advance_at(
                &store,
                &old,
                1,
                &AdaptiveTransitionV1::ClaimModel {
                    effect: model.clone(),
                    previous_observation_digest: None,
                },
                NOW + 1,
            );
            let inflight = if tool_effect {
                session = advance_at(
                    &store,
                    &old,
                    session.version,
                    &AdaptiveTransitionV1::ResolveModel {
                        effect: model.clone(),
                        result_digest: "b".repeat(64),
                        decision: AdaptiveModelDecisionV1::Tool {
                            tool: inspect_tool(),
                            tool_digest: adaptive_tool_digest(&inspect_tool()).unwrap(),
                        },
                    },
                    session.updated_at_ms + 1,
                );
                let tool = effect(&Uuid::new_v4().to_string(), 'c');
                session = advance_at(
                    &store,
                    &old,
                    session.version,
                    &AdaptiveTransitionV1::ClaimTool {
                        effect: tool.clone(),
                        tool_digest: adaptive_tool_digest(&inspect_tool()).unwrap(),
                    },
                    session.updated_at_ms + 1,
                );
                tool
            } else {
                model
            };
            if unknown {
                session = advance_at(
                    &store,
                    &old,
                    session.version,
                    &AdaptiveTransitionV1::MarkUnknown { effect: inflight },
                    session.updated_at_ms + 1,
                );
            }
            drop(store);
            let store = WorkflowStore::open(&database).unwrap();
            let before = persisted_adaptive_rows(&database);
            for command in [
                AdaptiveTransitionV1::ResolveBlocked {
                    expected_reason_code: "needs_input".into(),
                    resolution_event_id: Uuid::new_v4().to_string(),
                },
                AdaptiveTransitionV1::Cancel,
            ] {
                assert_eq!(
                    store
                        .advance_adaptive_session(
                            old.session_id,
                            session.version,
                            Uuid::new_v4(),
                            &command,
                            &auth,
                            old.deadline_ms + 1,
                        )
                        .unwrap_err()
                        .code,
                    WorkflowErrorCode::InvalidTransition
                );
            }
            let renewed = renewed_grant(&old);
            assert!(store
                .begin_adaptive_session(&renewed, &auth, renewed.created_at_ms)
                .is_err());
            assert_eq!(persisted_adaptive_rows(&database), before);
            assert_eq!(
                store.adaptive_session_for_authority(&auth).unwrap(),
                Some(session)
            );
        }
    }
}

#[test]
fn recovery_feedback_core_fences_stale_authority_and_unready_organization() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    let auth = authority();
    let current = Arc::new(Mutex::new(auth.clone()));
    let store = Arc::new(WorkflowStore::open(&database).unwrap());
    let old = grant(auth.clone(), 1);
    store.begin_adaptive_session(&old, &auth, NOW).unwrap();
    reject_first_model_fixture(&store, &old);
    let core = WorkflowCore::new(
        Arc::clone(&store),
        Organization {
            authority: Arc::clone(&current),
        },
        UnavailableWorkExecutionPort,
        UnavailableCompletionEvidencePort,
        UnavailableGateEvidencePort,
    );
    let before = persisted_adaptive_rows(&database);
    assert_eq!(
        core.adaptive_recovery_feedback(&auth).unwrap(),
        store.adaptive_recovery_feedback(&auth).unwrap()
    );
    current.lock().unwrap().assignment_version += 1;
    assert_eq!(
        core.adaptive_recovery_feedback(&auth).unwrap_err().code,
        WorkflowErrorCode::AuthorityConflict
    );
    let refreshed = current.lock().unwrap().clone();
    assert_eq!(core.adaptive_recovery_feedback(&refreshed).unwrap(), None);
    let unready = WorkflowCore::new(
        store,
        UnavailableOrganizationRuntimePort,
        UnavailableWorkExecutionPort,
        UnavailableCompletionEvidencePort,
        UnavailableGateEvidencePort,
    );
    assert_eq!(
        unready.adaptive_recovery_feedback(&auth).unwrap_err().code,
        WorkflowErrorCode::OrganizationUnavailable
    );
    assert_eq!(persisted_adaptive_rows(&database), before);
}

#[test]
fn replay_is_exact_but_revocation_limits_and_unknown_effects_fail_closed() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    let store = WorkflowStore::open(&database).unwrap();
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
    assert_eq!(
        unknown.cursor,
        AdaptiveCursorV1::ModelUnknown {
            effect: model.clone()
        }
    );
    assert_eq!(unknown.model_calls, pending.model_calls);
    assert_eq!(unknown.tool_calls, pending.tool_calls);
    assert_eq!(unknown.model_calls, 1);
    assert_eq!(unknown.tool_calls, 0);
    let before = persisted_adaptive_rows(&database);
    let feedback = store.adaptive_recovery_feedback(&auth).unwrap();
    assert_eq!(feedback, None);
    for now_ms in [NOW + 3, grant.deadline_ms + 1] {
        for (command, expected_code) in [
            (
                AdaptiveTransitionV1::ClaimModel {
                    effect: model.clone(),
                    previous_observation_digest: None,
                },
                WorkflowErrorCode::AuthorityConflict,
            ),
            (
                AdaptiveTransitionV1::ResolveModel {
                    effect: model.clone(),
                    result_digest: "e".repeat(64),
                    decision: AdaptiveModelDecisionV1::Tool {
                        tool: inspect_tool(),
                        tool_digest: adaptive_tool_digest(&inspect_tool()).unwrap(),
                    },
                },
                WorkflowErrorCode::AuthorityConflict,
            ),
            (
                AdaptiveTransitionV1::RejectModel {
                    effect: model.clone(),
                    resolution_event_id: Uuid::new_v4().to_string(),
                    reason_code: "schema_error".into(),
                },
                WorkflowErrorCode::AuthorityConflict,
            ),
            (
                AdaptiveTransitionV1::ClaimModel {
                    effect: effect("01991c34-e03c-70c2-b97e-0591f4be2415", 'f'),
                    previous_observation_digest: None,
                },
                WorkflowErrorCode::InvalidTransition,
            ),
        ] {
            assert_eq!(
                store
                    .advance_adaptive_session(
                        grant.session_id,
                        unknown.version,
                        Uuid::new_v4(),
                        &command,
                        &auth,
                        now_ms,
                    )
                    .unwrap_err()
                    .code,
                expected_code
            );
            assert_eq!(persisted_adaptive_rows(&database), before);
            assert_eq!(
                store.adaptive_session(grant.session_id, &auth).unwrap(),
                Some(unknown.clone())
            );
            assert_eq!(store.adaptive_recovery_feedback(&auth).unwrap(), feedback);
        }
    }

    let mut revoked = auth.clone();
    revoked.active = false;
    assert_eq!(
        store
            .adaptive_session(grant.session_id, &revoked)
            .unwrap_err()
            .code,
        WorkflowErrorCode::AuthorityConflict
    );
    assert_eq!(persisted_adaptive_rows(&database), before);
    drop(store);
    let reopened = WorkflowStore::open(&database).unwrap();
    assert_eq!(
        reopened.adaptive_session(grant.session_id, &auth).unwrap(),
        Some(unknown.clone())
    );
    assert_eq!(
        reopened.adaptive_recovery_feedback(&auth).unwrap(),
        feedback
    );
    assert_eq!(persisted_adaptive_rows(&database), before);

    // Known completion exercises its own pending head, never the sealed unknown head.
    let known_database = directory.path().join("known-completion.sqlite");
    let store = WorkflowStore::open(&known_database).unwrap();
    let (_, initial) = store.begin_adaptive_session(&grant, &auth, NOW).unwrap();
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

    let resolved = store
        .advance_adaptive_session(
            grant.session_id,
            pending.version,
            Uuid::parse_str("01991c34-e03c-70c2-b97e-0591f4be2416").unwrap(),
            &AdaptiveTransitionV1::ResolveModel {
                effect: match &pending.cursor {
                    AdaptiveCursorV1::ModelPending { effect } => effect.clone(),
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
    let known_before = persisted_adaptive_rows(&known_database);
    let known_feedback = store.adaptive_recovery_feedback(&auth).unwrap();
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
    assert_eq!(persisted_adaptive_rows(&known_database), known_before);
    assert_eq!(
        store.adaptive_session(grant.session_id, &auth).unwrap(),
        Some(ready_again)
    );
    assert_eq!(
        store.adaptive_recovery_feedback(&auth).unwrap(),
        known_feedback
    );
    assert_eq!(persisted_adaptive_rows(&database), before);
    assert_eq!(
        reopened.adaptive_session(grant.session_id, &auth).unwrap(),
        Some(unknown)
    );
    assert_eq!(
        reopened.adaptive_recovery_feedback(&auth).unwrap(),
        feedback
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
