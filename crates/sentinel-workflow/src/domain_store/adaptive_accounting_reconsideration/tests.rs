use super::*;
use crate::{
    adaptive_accounting_projection, AdaptiveAccountingReconsiderationReceiptV1,
    AdaptiveAccountingReconsiderationRequestV1, AdaptiveAccountingReconsiderationSourceV1,
};

struct Correction {
    f: Fixture,
    refused: AdaptiveLeadershipReviewCallV1,
    source: AdaptiveAccountingReconsiderationSourceV1,
    request: AdaptiveAccountingReconsiderationRequestV1,
    review_operation: Uuid,
    allowance: String,
    now: u64,
}

fn refused_fixture(slots: u16) -> (Fixture, AdaptiveLeadershipReviewCallV1, u64) {
    let (mut f, now) = reported_head(false);
    let policy = amend(&f, now, slots);
    bind(&mut f, &policy, policy.request.source.base_review_count + 1);
    let issued = issue(&f, now);
    let dispatched = f.store.claim_adaptive_leadership_review_call(
        &f.leader, &claim(&issued), now + 1,
    ).unwrap();
    let refused = f.store.complete_adaptive_leadership_review_call(
        &f.leader, &defer(&dispatched), now + 2,
    ).unwrap();
    let now = now + 3;
    (f, refused, now)
}

fn correction() -> Correction {
    let (mut f, refused, now) = refused_fixture(3);
    let policy = f.store.adaptive_resume_policy(&f.leader.tenant_id, f.grant.session_id).unwrap().unwrap();
    let source = f.store.adaptive_accounting_reconsideration_source(
        &operator(&f), &f.grant.project_id, f.grant.session_id, refused.grant.review_id,
        &f.grant.assignee_authority, &f.leader, &f.grant.leadership_authority, now,
    ).unwrap();
    let request = AdaptiveAccountingReconsiderationRequestV1 {
        schema_version: 1, operation_id: Uuid::new_v4(), project_id: f.grant.project_id.clone(),
        session_id: f.grant.session_id, refused_review_id: refused.grant.review_id,
        source_digest: source.source_digest.clone(), reason_ref: "explicit-accounting-correction".into(),
        expires_at_unix_ms: now + 120_000,
    };
    bind(&mut f, &policy, source.next_ordinal);
    f.context.evidence_refs.extend(source.evidence_refs.clone());
    f.grant.expires_at_unix_ms = request.expires_at_unix_ms;
    if let Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }) = &mut f.grant.subject {
        budget.observed_at_ms = now;
    }
    rebind_budget_evidence(&mut f);
    let allowance = format!("accounting-review-{}", f.grant.review_id);
    Correction { f, refused, source, request, review_operation: Uuid::new_v4(), allowance, now }
}

fn defer(call: &AdaptiveLeadershipReviewCallV1) -> CompleteAdaptiveLeadershipReviewCallV1 {
    let mut response = completion(call, false);
    response.decision.schema_version = 3;
    response.decision.decision = AdaptiveLeadershipReviewDecisionKindV1::DeferBudget {
        rationale: "The remaining accounting needs an explicit operator review".into(),
        evidence_refs: vec![call.context.evidence_refs[0].clone()],
    };
    response
}

fn post(c: &Correction, now: u64) -> Result<(bool, AdaptiveAccountingReconsiderationReceiptV1), WorkflowError> {
    c.f.store.authorize_adaptive_accounting_reconsideration(
        &operator(&c.f), &c.request, &c.f.grant.assignee_authority,
        &c.f.grant.leadership_authority, c.review_operation, &c.allowance,
        &c.f.grant, &c.f.context, now,
    )
}

#[test]
fn atomic_issuance_preserves_refusal_root_policy_and_charges_one_ordinary_review() {
    let c = correction();
    let original = session(&c.f);
    let original_journal = journal(&c.f);
    let old_policy = c.source.policy.clone();
    assert_eq!(c.source.accounting.model_calls_spent, 2);
    assert_eq!(c.source.accounting.root_model_calls_remaining, 14);
    assert_eq!(c.source.accounting.active_window_model_calls_remaining, 1);
    assert_eq!(c.source.accounting.issued_windows, 3);
    assert_ne!(c.source.accounting.review_ordinal, c.source.accounting.issued_windows);
    let (replayed, receipt) = post(&c, c.now).unwrap();
    assert!(!replayed);
    assert_eq!(receipt.source, c.source);
    assert_eq!(receipt.evidence_ref().unwrap(), c.source.evidence_ref().unwrap());
    assert_eq!(session(&c.f), original);
    assert_eq!(journal(&c.f), original_journal);
    assert_eq!(c.f.store.adaptive_resume_policy(&c.f.leader.tenant_id, c.request.session_id).unwrap(), Some(old_policy));
    assert_eq!(c.f.store.adaptive_leadership_review_call(&c.f.leader.tenant_id, c.refused.grant.review_id).unwrap(), Some(c.refused.clone()));
    let designated = c.f.store.adaptive_leadership_review_call(&c.f.leader.tenant_id, receipt.review_id).unwrap().unwrap();
    assert!(designated.decision.is_none());
    assert!(designated.continuation.is_none());
    assert_eq!(designated.grant.resume_policy.as_ref().unwrap().ordinal, c.source.next_ordinal);
    assert_eq!(c.f.store.adaptive_leadership_review_calls(&c.f.leader.tenant_id, c.request.session_id).unwrap().len(), usize::from(c.source.next_ordinal));
    let dispatched = c.f.store.claim_adaptive_leadership_review_call(&c.f.leader, &claim(&designated), c.now + 1).unwrap();
    assert_eq!(dispatched.grant, c.f.grant);
}

#[test]
fn reopen_and_historical_exact_replay_do_not_renew_or_write() {
    let mut c = correction();
    let receipt = post(&c, c.now).unwrap().1;
    c.f.store = WorkflowStore::open(&c.f.path).unwrap();
    let before = rows(&c.f.store);
    assert_eq!(post(&c, c.source.policy.request.limits.expires_at_unix_ms + 1).unwrap(), (true, receipt.clone()));
    assert_eq!(rows(&c.f.store), before);
    assert_eq!(c.f.store.adaptive_accounting_reconsideration(&c.f.leader.tenant_id, c.request.session_id).unwrap(), Some(receipt));
}

#[test]
fn direct_ordinary_authorization_cannot_bypass_the_original_refusal_or_forge_receipt() {
    let mut c = correction();
    let before = rows(&c.f.store);
    assert!(c.f.store.authorize_resume_leadership_review(&c.f.leader, c.review_operation, &c.allowance,
        &c.f.grant, &c.f.context, c.now).is_err());
    c.f.context.evidence_refs.retain(|r| !r.starts_with("adaptive-accounting-correction:"));
    rebind_budget_evidence(&mut c.f);
    assert!(c.f.store.authorize_resume_leadership_review(&c.f.leader, c.review_operation, &c.allowance,
        &c.f.grant, &c.f.context, c.now).is_err());
    assert_eq!(rows(&c.f.store), before);
}

#[test]
fn stale_request_expiry_authority_and_context_fail_without_writes() {
    for case in 0..15 {
        let mut c = correction();
        let before = rows(&c.f.store);
        let mut now = c.now;
        match case {
            0 => c.request.source_digest = "b".repeat(64),
            1 => c.request.refused_review_id = Uuid::new_v4(),
            2 => c.request.expires_at_unix_ms = now,
            3 => c.request.expires_at_unix_ms = now + crate::ADAPTIVE_ACCOUNTING_RECONSIDERATION_MAX_MS + 1,
            4 => now = c.source.policy.request.limits.expires_at_unix_ms,
            5 => c.f.grant.assignee_authority.runtime_generation += 1,
            6 => c.f.grant.leadership_authority.principal_generation += 1,
            7 => c.f.grant.leadership_principal.authority_generation += 1,
            8 => c.f.context.source_session.model_calls += 1,
            9 => c.f.context.source_project.version += 1,
            10 => {
                c.f.context.evidence_refs.retain(|r| !r.starts_with("adaptive-accounting-projection:"));
                rebind_budget_evidence(&mut c.f);
            }
            11 => c.f.grant.expires_at_unix_ms = c.request.expires_at_unix_ms + 1,
            12 => c.f.grant.resume_policy.as_mut().unwrap().ordinal += 1,
            13 => c.review_operation = c.request.operation_id,
            14 => c.f.grant.resume_policy.as_mut().unwrap().receipt_digest = "b".repeat(64),
            _ => unreachable!(),
        }
        assert!(post(&c, now).is_err(), "accepted case {case}");
        assert_eq!(rows(&c.f.store), before, "wrote case {case}");
    }
}

#[test]
fn non_operator_and_no_completed_refusal_are_denied() {
    let c = correction();
    let before = rows(&c.f.store);
    for principal in [c.f.leader.clone(), {
        let mut p = operator(&c.f); p.role = CompanyRoleV1::Developer; p
    }] {
        assert!(c.f.store.authorize_adaptive_accounting_reconsideration(&principal, &c.request,
            &c.f.grant.assignee_authority, &c.f.grant.leadership_authority,
            c.review_operation, &c.allowance, &c.f.grant, &c.f.context, c.now).is_err());
    }
    let prior = c.f.store.adaptive_leadership_review_calls(&c.f.leader.tenant_id, c.request.session_id).unwrap()
        .into_iter().find(|call| call.continuation.is_some()).unwrap();
    assert!(c.f.store.adaptive_accounting_reconsideration_source(&operator(&c.f), &c.request.project_id,
        c.request.session_id, prior.grant.review_id, &c.f.grant.assignee_authority, &c.f.leader,
        &c.f.grant.leadership_authority, c.now).is_err());
    assert_eq!(rows(&c.f.store), before);
}

#[test]
fn expired_or_deferred_correction_never_releases_second_slot() {
    for deferred in [false, true] {
        let mut c = correction();
        let receipt = post(&c, c.now).unwrap().1;
        let designated = c.f.store.adaptive_leadership_review_call(&c.f.leader.tenant_id, receipt.review_id).unwrap().unwrap();
        let terminal = if deferred {
            let dispatched = c.f.store.claim_adaptive_leadership_review_call(&c.f.leader, &claim(&designated), c.now + 1).unwrap();
            c.f.store.complete_adaptive_leadership_review_call(&c.f.leader, &defer(&dispatched), c.now + 2).unwrap()
        } else {
            c.f.store.expire_adaptive_leadership_review_call(&c.f.leader, designated.grant.review_id,
                designated.version, designated.grant.expires_at_unix_ms).unwrap()
        };
        assert_eq!(terminal.grant.resume_policy.as_ref().unwrap().ordinal, c.source.next_ordinal);
        let before = rows(&c.f.store);
        assert!(c.f.store.adaptive_accounting_reconsideration_source(&operator(&c.f), &c.request.project_id,
            c.request.session_id, c.refused.grant.review_id, &c.f.grant.assignee_authority, &c.f.leader,
            &c.f.grant.leadership_authority, c.now + 3).is_err());
        c.request.operation_id = Uuid::new_v4();
        assert!(post(&c, c.now + 3).is_err());
        assert_eq!(rows(&c.f.store), before);
        change_project(&c.f, c.now + 4);
        budget_context(&mut c.f, c.now + 5, "changed-project-no-second-accounting-review");
        c.f.context.evidence_refs.retain(|r| !r.starts_with("adaptive-accounting-correction:") && !r.starts_with("adaptive-accounting-projection:"));
        bind(&mut c.f, &receipt.source.policy, c.source.next_ordinal + 1);
        let after_project_change = rows(&c.f.store);
        assert!(c.f.store.authorize_resume_leadership_review(&c.f.leader, Uuid::new_v4(), "forbidden-next-review",
            &c.f.grant, &c.f.context, c.now + 5).is_err());
        assert_eq!(rows(&c.f.store), after_project_change);
        assert_eq!(c.f.store.adaptive_leadership_review_call(&c.f.leader.tenant_id, c.refused.grant.review_id).unwrap(), Some(c.refused));
    }
}

#[test]
fn accounting_write_boundary_starts_fresh_bounded_proofs_inside_the_same_transaction() {
    let c = correction();
    let original_journal = journal(&c.f);
    let (result, scopes) = validation_scope::with_completed_validations(|| post(&c, c.now));
    assert!(!result.unwrap().0);
    assert_eq!(scopes.len(), 2);
    for scope in scopes {
        assert_eq!(scope.get("review-calls"), Some(&1));
        assert_eq!(scope.get("adaptive-journal"), Some(&1));
    }
    assert_eq!(journal(&c.f), original_journal);
}

#[test]
fn event_failure_rolls_back_receipt_membership_and_review_together() {
    for event in ["adaptive_accounting_reconsideration_authorized", "adaptive_resume_review_membership_issued", "adaptive_leadership_review_authorized"] {
        let c = correction();
        c.f.store.connection.lock().unwrap().execute_batch(&format!(
            "CREATE TRIGGER fail_accounting_event BEFORE INSERT ON company_events WHEN NEW.event_type='{event}' BEGIN SELECT RAISE(ABORT, 'test rollback'); END;",
        )).unwrap();
        let before = rows(&c.f.store);
        assert!(post(&c, c.now).is_err(), "event {event} did not fail");
        assert_eq!(rows(&c.f.store), before);
        assert!(c.f.store.adaptive_accounting_reconsideration(&c.f.leader.tenant_id, c.request.session_id).unwrap().is_none());
        c.f.store.connection.lock().unwrap().execute_batch("DROP TRIGGER fail_accounting_event").unwrap();
        assert!(!post(&c, c.now).unwrap().0);
    }
}

#[test]
fn competing_operation_ids_across_connections_issue_only_once() {
    let c = correction();
    let stores = [WorkflowStore::open(&c.f.path).unwrap(), WorkflowStore::open(&c.f.path).unwrap()];
    let barrier = Arc::new(Barrier::new(2));
    let results = std::thread::scope(|scope| {
        let handles: Vec<_> = stores.into_iter().map(|store| {
            let barrier = barrier.clone();
            let mut request = c.request.clone();
            request.operation_id = Uuid::new_v4();
            let c = &c;
            scope.spawn(move || {
                barrier.wait();
                store.authorize_adaptive_accounting_reconsideration(&operator(&c.f), &request,
                    &c.f.grant.assignee_authority, &c.f.grant.leadership_authority,
                    c.review_operation, &c.allowance, &c.f.grant, &c.f.context, c.now)
            })
        }).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect::<Vec<_>>()
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(c.f.store.adaptive_leadership_review_calls(&c.f.leader.tenant_id, c.request.session_id).unwrap().len(), usize::from(c.source.next_ordinal));
}

#[test]
fn historical_receipt_event_tamper_or_orphan_fails_closed_without_cycles() {
    for orphan in [false, true] {
        let c = correction();
        let receipt = post(&c, c.now).unwrap().1;
        let connection = c.f.store.connection.lock().unwrap();
        if orphan {
            connection.execute("DELETE FROM company_entities WHERE tenant_id=?1 AND entity_kind='adaptive_accounting_reconsideration'",
                [&c.f.leader.tenant_id.0]).unwrap();
        } else {
            connection.execute("UPDATE company_events SET payload=X'00' WHERE tenant_id=?1 AND event_type='adaptive_accounting_reconsideration_authorized'",
                [&c.f.leader.tenant_id.0]).unwrap();
        }
        drop(connection);
        assert!(c.f.store.adaptive_accounting_reconsideration(&c.f.leader.tenant_id, c.request.session_id).is_err());
        assert!(c.f.store.adaptive_leadership_review_call(&c.f.leader.tenant_id, receipt.review_id).is_err());
    }
}

#[test]
fn accounting_marker_is_pure_and_rejects_forged_counter_projection() {
    let mut c = correction();
    let projection = adaptive_accounting_projection(&c.f.context.source_session, c.f.grant.resume_policy.as_deref().unwrap()).unwrap();
    assert_eq!(projection, c.source.accounting);
    assert!(c.f.context.evidence_refs.contains(&projection.evidence_ref().unwrap()));
    c.f.context.evidence_refs.retain(|r| !r.starts_with("adaptive-accounting-projection:"));
    c.f.context.evidence_refs.push(format!("adaptive-accounting-projection:{}", "b".repeat(64)));
    rebind_budget_evidence(&mut c.f);
    let before = rows(&c.f.store);
    assert!(post(&c, c.now).is_err());
    assert_eq!(rows(&c.f.store), before);
}

#[test]
fn expired_original_refusal_is_historical_but_new_issuance_has_a_fresh_bounded_expiry() {
    let mut c = correction();
    c.now = c.refused.grant.expires_at_unix_ms + 1;
    c.request.expires_at_unix_ms = c.now + 120_000;
    c.f.grant.expires_at_unix_ms = c.request.expires_at_unix_ms;
    if let Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }) = &mut c.f.grant.subject {
        budget.observed_at_ms = c.now;
    }
    assert!(!post(&c, c.now).unwrap().0);
    assert_eq!(c.f.store.adaptive_leadership_review_call(&c.f.leader.tenant_id, c.refused.grant.review_id).unwrap(), Some(c.refused));
}

#[test]
fn exhausted_policy_and_actual_project_drift_deny_a_new_source() {
    let (f, refused, now) = refused_fixture(1);
    let before = rows(&f.store);
    assert!(f.store.adaptive_accounting_reconsideration_source(&operator(&f), &f.grant.project_id,
        f.grant.session_id, refused.grant.review_id, &f.grant.assignee_authority, &f.leader,
        &f.grant.leadership_authority, now).is_err());
    assert_eq!(rows(&f.store), before);
    let c = correction();
    change_project(&c.f, c.now);
    let before = rows(&c.f.store);
    assert!(post(&c, c.now + 1).is_err());
    assert_eq!(rows(&c.f.store), before);
}

#[test]
fn dispatched_unresolved_correction_is_spent_and_cannot_buy_a_retry() {
    let mut c = correction();
    let receipt = post(&c, c.now).unwrap().1;
    let call = c.f.store.adaptive_leadership_review_call(&c.f.leader.tenant_id, receipt.review_id).unwrap().unwrap();
    c.f.store.claim_adaptive_leadership_review_call(&c.f.leader, &claim(&call), c.now + 1).unwrap();
    let before = rows(&c.f.store);
    c.request.operation_id = Uuid::new_v4();
    assert!(post(&c, c.now + 2).is_err());
    assert_eq!(rows(&c.f.store), before);
    assert_eq!(session(&c.f), c.source.source_session);
}

#[test]
fn refusal_payload_or_policy_replacement_without_matching_sealed_event_is_rejected() {
    for policy in [false, true] {
        let c = correction();
        let connection = c.f.store.connection.lock().unwrap();
        let (kind, key, payload) = if policy {
            let mut changed = c.source.policy.clone();
            changed.request.reason_ref = "replacement-policy".into();
            ("adaptive_resume_policy", changed.policy_id.clone(), encode(&changed).unwrap())
        } else {
            let mut changed = c.refused.clone();
            changed.model_response_digest = Some("b".repeat(64));
            ("adaptive_leadership_review_call", changed.review_key.clone(), encode(&changed).unwrap())
        };
        let digest = bytes_digest("sentinel.workflow.company-entity-row.v1", &payload).unwrap();
        connection.execute("UPDATE company_entities SET payload=?1,payload_digest=?2 WHERE tenant_id=?3 AND entity_kind=?4 AND entity_id=?5",
            params![payload, digest, c.f.leader.tenant_id.0, kind, key]).unwrap();
        drop(connection);
        let before = rows(&c.f.store);
        assert!(post(&c, c.now).is_err());
        assert_eq!(rows(&c.f.store), before);
    }
}

#[test]
fn replay_requires_exact_request_designation_authority_and_context_without_writes() {
    let mut c = correction();
    post(&c, c.now).unwrap();
    let request = c.request.clone();
    let grant = c.f.grant.clone();
    let context = c.f.context.clone();
    let review_operation = c.review_operation;
    let allowance = c.allowance.clone();
    let before = rows(&c.f.store);
    for case in 0..9 {
        c.request = request.clone();
        c.f.grant = grant.clone();
        c.f.context = context.clone();
        c.review_operation = review_operation;
        c.allowance = allowance.clone();
        match case {
            0 => c.request.operation_id = Uuid::new_v4(),
            1 => c.request.reason_ref = "different-reason".into(),
            2 => c.request.expires_at_unix_ms += 1,
            3 => c.review_operation = Uuid::new_v4(),
            4 => c.allowance = "different-allowance".into(),
            5 => c.f.grant.expires_at_unix_ms += 1,
            6 => c.f.context.evidence_refs.push("additional-evidence".into()),
            7 => c.f.grant.assignee_authority.runtime_generation += 1,
            8 => c.f.grant.leadership_authority.principal_generation += 1,
            _ => unreachable!(),
        }
        assert!(post(&c, c.now + 1).is_err(), "accepted replay change {case}");
        assert_eq!(rows(&c.f.store), before);
    }
}

#[test]
fn exact_replay_survives_designated_defer_expiry_and_continue_with_an_evolved_head() {
    for outcome in 0..3 {
        let c = correction();
        let receipt = post(&c, c.now).unwrap().1;
        let issued = c.f.store.adaptive_leadership_review_call(&c.f.leader.tenant_id, receipt.review_id).unwrap().unwrap();
        if outcome == 0 {
            c.f.store.expire_adaptive_leadership_review_call(&c.f.leader, issued.grant.review_id,
                issued.version, issued.grant.expires_at_unix_ms).unwrap();
        } else {
            let call = c.f.store.claim_adaptive_leadership_review_call(&c.f.leader, &claim(&issued), c.now + 1).unwrap();
            let response = if outcome == 1 { defer(&call) } else { result(&call, c.now + 2, 2) };
            c.f.store.complete_adaptive_leadership_review_call(&c.f.leader, &response, c.now + 2).unwrap();
            if outcome == 2 {
                observe_budget_inspection(&c.f, session(&c.f), c.now + 3);
                assert!(session(&c.f).version > c.source.source_session.version);
            }
        }
        let before = rows(&c.f.store);
        assert_eq!(post(&c, c.source.policy.request.limits.expires_at_unix_ms + 1).unwrap(), (true, receipt));
        assert_eq!(rows(&c.f.store), before);
    }
}
