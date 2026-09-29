use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use sentinel_common::WorkbenchTool;
use sentinel_workflow::{
    adaptive_collaboration_digest, adaptive_tool_digest, AdaptiveCollaborationActionV1,
    AdaptiveCursorV1, AdaptiveEffectV1, AdaptiveModelDecisionV1, AdaptiveModelObservationV1,
    AdaptiveModelPort, AdaptiveObservationRefV1, AdaptiveSessionGrantV1, AdaptiveToolObservationV1,
    AdaptiveToolPort, AdaptiveTransitionV1, AdaptiveWorkflowCore, AgentId, DependencyReadiness,
    OrganizationRuntimePort, PrincipalAuthorityV1, ProjectId, RuntimeAuthoritySnapshotV1, TenantId,
    WorkItemId, WorkflowErrorCode, WorkflowPortError, WorkflowStore, WORKFLOW_SCHEMA_VERSION,
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
