use super::*;
use crate::{AdaptiveWorkFundingLimitsV1, AdaptiveWorkFundingRequestV1};

const FUNDING_KIND: &str = "adaptive_work_funding";
const FUNDING_EVENT: &str = "adaptive_work_funding_issued";

fn funding_draft(f: &Fixture) -> AdaptiveWorkFundingRequestV1 {
    f.store
        .adaptive_work_funding_draft(
            &f.operator,
            &f.project.project_id,
            f.session.grant.session_id,
            Uuid::new_v4(),
            "issue-856.additional-work",
            AdaptiveWorkFundingLimitsV1 {
                additional_model_calls: 4,
                additional_tool_calls: 4,
                additional_reviews: 8,
                additional_windows: 4,
                max_window_ms: ADAPTIVE_RESUME_MAX_WINDOW_MS,
                max_call_duration_ms: crate::ADAPTIVE_LEADERSHIP_MAX_DURATION_MS,
                dispatch_margin_ms: ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
                expires_at_unix_ms: EXPIRY,
            },
            NOW,
        )
        .unwrap()
}

fn funding_row_counts(f: &Fixture) -> (i64, i64) {
    let connection = f.store.connection.lock().unwrap();
    (
        connection
            .query_row(
                "SELECT COUNT(*) FROM company_entities WHERE entity_kind=?1",
                [FUNDING_KIND],
                |row| row.get(0),
            )
            .unwrap(),
        connection
            .query_row(
                "SELECT COUNT(*) FROM company_events WHERE event_type=?1",
                [FUNDING_EVENT],
                |row| row.get(0),
            )
            .unwrap(),
    )
}

#[test]
fn corrupted_session_field_cannot_hide_a_pending_receipt_from_new_issuance() {
    let f = fixture(false);
    let request = funding_draft(&f);
    f.store
        .authorize_adaptive_work_funding(&f.operator, &request, NOW)
        .unwrap();
    f.store
        .connection
        .lock()
        .unwrap()
        .execute(
            "UPDATE company_entities SET payload=json_set(payload,
         '$.request.source.resume_source.session_id',?1) WHERE entity_kind=?2",
            params![Uuid::new_v4().to_string(), FUNDING_KIND],
        )
        .unwrap();
    assert!(f
        .store
        .adaptive_work_funding_draft(
            &f.operator,
            &f.project.project_id,
            f.session.grant.session_id,
            Uuid::new_v4(),
            &request.reason_ref,
            request.limits.clone(),
            NOW
        )
        .is_err());
    let mut second = request.clone();
    second.operation_id = Uuid::new_v4();
    assert!(f
        .store
        .authorize_adaptive_work_funding(&f.operator, &second, NOW)
        .is_err());
    assert!(f
        .store
        .adaptive_work_funding_for_review(&f.project.tenant_id, f.session.grant.session_id, NOW)
        .is_err());
    assert_eq!(funding_row_counts(&f), (1, 1));
}

#[test]
fn corrupted_orphan_event_session_cannot_hide_a_pending_receipt() {
    let f = fixture(false);
    let request = funding_draft(&f);
    f.store
        .authorize_adaptive_work_funding(&f.operator, &request, NOW)
        .unwrap();
    {
        let connection = f.store.connection.lock().unwrap();
        connection
            .execute(
                "DELETE FROM company_entities WHERE entity_kind=?1",
                [FUNDING_KIND],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE company_events SET payload=json_set(payload,
            '$.request.source.resume_source.session_id',NULL) WHERE event_type=?1",
                [FUNDING_EVENT],
            )
            .unwrap();
    }
    assert!(f
        .store
        .adaptive_work_funding_draft(
            &f.operator,
            &f.project.project_id,
            f.session.grant.session_id,
            Uuid::new_v4(),
            &request.reason_ref,
            request.limits.clone(),
            NOW
        )
        .is_err());
    let mut second = request.clone();
    second.operation_id = Uuid::new_v4();
    assert!(f
        .store
        .authorize_adaptive_work_funding(&f.operator, &second, NOW)
        .is_err());
    assert_eq!(funding_row_counts(&f), (0, 1));
}

#[test]
fn funding_issuance_changes_only_receipt_and_event_not_execution_or_old_policy() {
    let f = fixture(false);
    let old_policy = issue(&f);
    let request = funding_draft(&f);
    assert_eq!(funding_row_counts(&f), (0, 0));
    let (replayed, receipt) = f
        .store
        .authorize_adaptive_work_funding(&f.operator, &request, NOW)
        .unwrap();
    assert!(!replayed);
    assert_eq!(funding_row_counts(&f), (1, 1));
    assert_eq!(
        f.store
            .adaptive_session(f.session.grant.session_id, &f.session.grant.authority)
            .unwrap(),
        Some(f.session.clone())
    );
    assert_eq!(
        f.store
            .company_project(&f.project.tenant_id, &f.project.project_id)
            .unwrap(),
        Some(f.project.clone())
    );
    assert_eq!(
        f.store
            .adaptive_resume_policy(&f.project.tenant_id, f.session.grant.session_id)
            .unwrap(),
        Some(old_policy)
    );
    let reopened = WorkflowStore::open(&f.path).unwrap();
    assert_eq!(
        reopened
            .adaptive_work_funding(
                &f.project.tenant_id,
                f.session.grant.session_id,
                request.operation_id
            )
            .unwrap(),
        Some(receipt)
    );
}

#[test]
fn funding_replay_precedes_expiry_and_currentness_without_new_rows() {
    let f = fixture(false);
    let request = funding_draft(&f);
    let (_, receipt) = f
        .store
        .authorize_adaptive_work_funding(&f.operator, &request, NOW)
        .unwrap();
    f.store
        .advance_adaptive_session(
            f.session.grant.session_id,
            f.session.version,
            Uuid::new_v4(),
            &AdaptiveTransitionV1::Cancel,
            &f.session.grant.authority,
            NOW + 1,
        )
        .unwrap();
    assert_eq!(
        f.store
            .authorize_adaptive_work_funding(&f.operator, &request, EXPIRY + 1)
            .unwrap(),
        (true, receipt)
    );
    assert_eq!(
        f.store
            .adaptive_work_funding_draft(
                &f.operator,
                &f.project.project_id,
                f.session.grant.session_id,
                request.operation_id,
                &request.reason_ref,
                request.limits.clone(),
                EXPIRY + 1
            )
            .unwrap(),
        request
    );
    assert_eq!(funding_row_counts(&f), (1, 1));
}

#[test]
fn funding_rejects_each_substituted_source_before_any_write() {
    let f = fixture(false);
    let request = funding_draft(&f);
    for index in 0..10 {
        let mut changed = request.clone();
        match index {
            0 => changed.source.resume_source.expected_project_version += 1,
            1 => changed.source.resume_source.expected_session_version += 1,
            2 => changed.source.resume_source.project_payload_digest = "b".repeat(64),
            3 => changed.source.resume_source.root_entry_digest = "b".repeat(64),
            4 => changed.source.resume_source.head_entry_digest = "b".repeat(64),
            5 => changed.source.resume_source.continuation_history_digest = "b".repeat(64),
            6 => changed.source.resume_source.review_history_digest = "b".repeat(64),
            7 => changed.source.original_model_call_ceiling += 1,
            8 => changed.source.original_tool_call_ceiling += 1,
            _ => changed.limits.max_call_duration_ms -= 1,
        }
        assert!(f
            .store
            .authorize_adaptive_work_funding(&f.operator, &changed, NOW)
            .is_err());
        assert_eq!(funding_row_counts(&f), (0, 0));
    }
}

#[test]
fn funding_rejects_wrong_principal_and_changed_replay() {
    let f = fixture(false);
    let request = funding_draft(&f);
    for mut principal in [f.leader.clone(), f.operator.clone()] {
        if principal == f.operator {
            principal.role = CompanyRoleV1::Developer;
        }
        assert!(f
            .store
            .authorize_adaptive_work_funding(&principal, &request, NOW)
            .is_err());
    }
    let mut foreign = f.operator.clone();
    foreign.tenant_id = TenantId::parse("foreign-tenant").unwrap();
    assert!(f
        .store
        .authorize_adaptive_work_funding(&foreign, &request, NOW)
        .is_err());
    f.store
        .authorize_adaptive_work_funding(&f.operator, &request, NOW)
        .unwrap();
    let mut other_operator = f.operator.clone();
    other_operator.principal_id = "other-operator".into();
    assert!(f
        .store
        .authorize_adaptive_work_funding(&other_operator, &request, NOW)
        .is_err());
    let mut changed = request.clone();
    changed.limits.additional_model_calls += 1;
    assert!(f
        .store
        .authorize_adaptive_work_funding(&f.operator, &changed, NOW)
        .is_err());
    assert_eq!(funding_row_counts(&f), (1, 1));
}

#[test]
fn funding_concurrent_exact_issuance_commits_one_receipt_and_one_event() {
    let f = fixture(false);
    let request = funding_draft(&f);
    let barrier = Arc::new(Barrier::new(2));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let path = f.path.clone();
        let principal = f.operator.clone();
        let request = request.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            let store = WorkflowStore::open(&path).unwrap();
            barrier.wait();
            store
                .authorize_adaptive_work_funding(&principal, &request, NOW)
                .unwrap()
        }));
    }
    let left = workers.remove(0).join().unwrap();
    let right = workers.remove(0).join().unwrap();
    assert_ne!(left.0, right.0);
    assert_eq!(left.1, right.1);
    assert_eq!(funding_row_counts(&f), (1, 1));
}

#[test]
fn funding_cannot_issue_second_unadopted_epoch_or_trust_claimed_predecessor() {
    let f = fixture(false);
    let request = funding_draft(&f);
    let mut forged = request.clone();
    forged.source.predecessor_receipt_digest = Some(DIGEST.into());
    forged.source.current_model_call_ceiling += 1;
    forged.validate_shape().unwrap();
    assert!(f
        .store
        .authorize_adaptive_work_funding(&f.operator, &forged, NOW)
        .is_err());
    f.store
        .authorize_adaptive_work_funding(&f.operator, &request, NOW)
        .unwrap();
    let mut second = request.clone();
    second.operation_id = Uuid::new_v4();
    assert!(f
        .store
        .authorize_adaptive_work_funding(&f.operator, &second, NOW)
        .is_err());
    assert_eq!(funding_row_counts(&f), (1, 1));
}

#[test]
fn funding_rejects_unknown_effect_without_authorizing_its_retry() {
    let f = fixture(true);
    assert!(f
        .store
        .adaptive_work_funding_draft(
            &f.operator,
            &f.project.project_id,
            f.session.grant.session_id,
            Uuid::new_v4(),
            "issue-856.additional-work",
            AdaptiveWorkFundingLimitsV1 {
                additional_model_calls: 4,
                additional_tool_calls: 4,
                additional_reviews: 8,
                additional_windows: 4,
                max_window_ms: ADAPTIVE_RESUME_MAX_WINDOW_MS,
                max_call_duration_ms: crate::ADAPTIVE_LEADERSHIP_MAX_DURATION_MS,
                dispatch_margin_ms: ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
                expires_at_unix_ms: EXPIRY,
            },
            NOW
        )
        .is_err());
    assert_eq!(funding_row_counts(&f), (0, 0));
    assert_eq!(
        f.store
            .adaptive_session(f.session.grant.session_id, &f.session.grant.authority)
            .unwrap(),
        Some(f.session)
    );
}

#[test]
fn funding_event_failure_rolls_back_receipt_then_same_request_can_succeed() {
    let f = fixture(false);
    let request = funding_draft(&f);
    f.store.connection.lock().unwrap().execute_batch("CREATE TEMP TRIGGER fail_funding_event BEFORE INSERT ON company_events WHEN NEW.event_type='adaptive_work_funding_issued' BEGIN SELECT RAISE(ABORT,'funding event injection'); END;").unwrap();
    assert!(f
        .store
        .authorize_adaptive_work_funding(&f.operator, &request, NOW)
        .is_err());
    assert_eq!(funding_row_counts(&f), (0, 0));
    f.store
        .connection
        .lock()
        .unwrap()
        .execute_batch("DROP TRIGGER fail_funding_event")
        .unwrap();
    assert!(
        !f.store
            .authorize_adaptive_work_funding(&f.operator, &request, NOW)
            .unwrap()
            .0
    );
    assert_eq!(funding_row_counts(&f), (1, 1));
}

#[test]
fn funding_missing_or_mutated_event_and_orphaned_receipt_fail_closed() {
    for corruption in 0..3 {
        let f = fixture(false);
        let request = funding_draft(&f);
        f.store
            .authorize_adaptive_work_funding(&f.operator, &request, NOW)
            .unwrap();
        {
            let connection = f.store.connection.lock().unwrap();
            match corruption {
                0 => connection
                    .execute(
                        "DELETE FROM company_events WHERE event_type=?1",
                        [FUNDING_EVENT],
                    )
                    .unwrap(),
                1 => connection
                    .execute(
                        "UPDATE company_events SET principal_id='other-issuer' WHERE event_type=?1",
                        [FUNDING_EVENT],
                    )
                    .unwrap(),
                _ => connection
                    .execute(
                        "DELETE FROM company_entities WHERE entity_kind=?1",
                        [FUNDING_KIND],
                    )
                    .unwrap(),
            };
        }
        let reopened = WorkflowStore::open(&f.path).unwrap();
        assert!(reopened
            .adaptive_work_funding(
                &f.project.tenant_id,
                f.session.grant.session_id,
                request.operation_id
            )
            .is_err());
        assert!(reopened
            .authorize_adaptive_work_funding(&f.operator, &request, NOW)
            .is_err());
    }
}

#[test]
fn funding_duplicate_id_under_other_operation_rejects_lookup_replay_and_reopen() {
    let f = fixture(false);
    let request = funding_draft(&f);
    let (_, receipt) = f
        .store
        .authorize_adaptive_work_funding(&f.operator, &request, NOW)
        .unwrap();
    {
        let mut connection = f.store.connection.lock().unwrap();
        let transaction = connection.transaction().unwrap();
        append_event(
            &transaction,
            &f.operator,
            Uuid::new_v4(),
            &request.canonical_digest().unwrap(),
            Some(&f.project.project_id),
            FUNDING_EVENT,
            &receipt,
            NOW,
        )
        .unwrap();
        transaction.commit().unwrap();
    }
    assert!(f
        .store
        .adaptive_work_funding(
            &f.project.tenant_id,
            f.session.grant.session_id,
            request.operation_id
        )
        .is_err());
    assert!(f
        .store
        .authorize_adaptive_work_funding(&f.operator, &request, NOW)
        .is_err());
    let reopened = WorkflowStore::open(&f.path).unwrap();
    assert!(reopened
        .adaptive_work_funding(
            &f.project.tenant_id,
            f.session.grant.session_id,
            request.operation_id
        )
        .is_err());
}

#[test]
fn funding_lookup_during_other_connection_commit_sees_one_consistent_snapshot() {
    let f = fixture(false);
    let request = funding_draft(&f);
    let connection = f.store.connection.lock().unwrap();
    let value = crate::domain_store::adaptive_work_funding::read_during_issuance(
        &connection,
        &f.project.tenant_id,
        f.session.grant.session_id,
        request.operation_id,
        || {
            let path = f.path.clone();
            let principal = f.operator.clone();
            let request = request.clone();
            std::thread::spawn(move || {
                WorkflowStore::open(&path)?
                    .authorize_adaptive_work_funding(&principal, &request, NOW)?;
                Ok(())
            })
            .join()
            .unwrap()
        },
    )
    .unwrap();
    assert!(value.is_none());
    drop(connection);
    assert!(f
        .store
        .adaptive_work_funding(
            &f.project.tenant_id,
            f.session.grant.session_id,
            request.operation_id
        )
        .unwrap()
        .is_some());
}

#[test]
fn funding_can_be_proposed_at_spent_root_without_refunding_a_single_call() {
    let mut f = fixture(false);
    let tool = sentinel_common::WorkbenchTool::InspectFile {
        path: "source.txt".into(),
        max_bytes: 32,
    };
    let tool_digest = crate::adaptive_tool_digest(&tool).unwrap();
    let mut now = 20;
    for _ in 0..f.session.grant.max_model_calls {
        let model_effect = AdaptiveEffectV1 {
            id: Uuid::new_v4(),
            request_digest: DIGEST.into(),
        };
        let tool_effect = AdaptiveEffectV1 {
            id: Uuid::new_v4(),
            request_digest: DIGEST.into(),
        };
        let commands = [
            AdaptiveTransitionV1::ClaimModel {
                effect: model_effect.clone(),
                previous_observation_digest: f
                    .session
                    .last_observation
                    .as_ref()
                    .map(|value| value.observation_digest.clone()),
            },
            AdaptiveTransitionV1::ResolveModel {
                effect: model_effect,
                result_digest: DIGEST.into(),
                decision: crate::AdaptiveModelDecisionV1::Tool {
                    tool: tool.clone(),
                    tool_digest: tool_digest.clone(),
                },
            },
            AdaptiveTransitionV1::ClaimTool {
                effect: tool_effect.clone(),
                tool_digest: tool_digest.clone(),
            },
            AdaptiveTransitionV1::ObserveTool {
                observation: crate::AdaptiveObservationRefV1 {
                    effect: tool_effect,
                    observation_digest: DIGEST.into(),
                },
            },
        ];
        for command in commands {
            f.session = f
                .store
                .advance_adaptive_session(
                    f.session.grant.session_id,
                    f.session.version,
                    Uuid::new_v4(),
                    &command,
                    &f.session.grant.authority,
                    now,
                )
                .unwrap()
                .1;
            now += 1;
        }
    }
    assert_eq!(f.session.model_calls, f.session.grant.max_model_calls);
    assert_eq!(f.session.tool_calls, f.session.grant.max_tool_calls);
    assert!(f
        .store
        .adaptive_resume_policy_draft(
            &f.operator,
            &f.project.project_id,
            f.session.grant.session_id,
            Uuid::new_v4(),
            "old-policy",
            EXPIRY,
            NOW
        )
        .is_err());
    let request = funding_draft(&f);
    let mut unusable = request.clone();
    unusable.limits.additional_model_calls = 0;
    assert!(f
        .store
        .authorize_adaptive_work_funding(&f.operator, &unusable, NOW)
        .is_err());
    assert_eq!(funding_row_counts(&f), (0, 0));
    f.store
        .authorize_adaptive_work_funding(&f.operator, &request, NOW)
        .unwrap();
    assert_eq!(
        f.store
            .adaptive_session(f.session.grant.session_id, &f.session.grant.authority)
            .unwrap(),
        Some(f.session)
    );
}
