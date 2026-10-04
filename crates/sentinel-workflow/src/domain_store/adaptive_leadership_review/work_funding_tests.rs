use super::*;
use crate::{AdaptiveWorkFundingEpochV1, AdaptiveWorkFundingLimitsV1, AdaptiveWorkFundingReceiptV1};

const WINDOW_MS: u64 = 180_000;

pub(super) fn fund(f: &Fixture, now: u64) -> AdaptiveWorkFundingReceiptV1 {
    fund_with_windows(f, now, 3)
}

fn fund_with_windows(f: &Fixture, now: u64, additional_windows: u16) -> AdaptiveWorkFundingReceiptV1 {
    let mut operator = f.leader.clone();
    operator.kind = CompanyPrincipalKindV1::Operator;
    operator.agent_id = None;
    let request = f.store.adaptive_work_funding_draft(
        &operator, &f.grant.project_id, f.grant.session_id, Uuid::new_v4(),
        "explicit-additional-work", AdaptiveWorkFundingLimitsV1 {
            additional_model_calls: 6,
            additional_tool_calls: 6,
            additional_reviews: 3,
            additional_windows,
            max_window_ms: WINDOW_MS,
            max_call_duration_ms: f.context.source_session.grant.max_call_duration_ms,
            dispatch_margin_ms: crate::ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
            expires_at_unix_ms: now + 3_600_000,
        }, now,
    ).unwrap();
    f.store.authorize_adaptive_work_funding(&operator, &request, now).unwrap().1
}

pub(super) fn bind(f: &mut Fixture, receipt: &AdaptiveWorkFundingReceiptV1, ordinal: u16) {
    let epoch = AdaptiveWorkFundingEpochV1 {
        receipt: receipt.clone(), binding: receipt.binding(ordinal).unwrap(),
    };
    f.context.evidence_refs.retain(|reference| !reference.starts_with("adaptive-work-funding:"));
    f.context.evidence_refs.push(epoch.evidence_ref().unwrap());
    f.grant.schema_version = 5;
    f.grant.work_funding = Some(Box::new(epoch));
    rebind_budget_evidence(f);
}

fn issue(f: &Fixture, now: u64) -> Result<AdaptiveLeadershipReviewCallV1, WorkflowError> {
    f.store.authorize_adaptive_leadership_review_call(
        &f.leader, Uuid::new_v4(), &format!("funded-review-{}", f.grant.review_id),
        &f.grant, &f.context, now,
    )
}

pub(super) fn dispatch(f: &Fixture, now: u64) -> AdaptiveLeadershipReviewCallV1 {
    let call = issue(f, now).unwrap();
    f.store.claim_adaptive_leadership_review_call(&f.leader, &claim(&call), now + 1).unwrap()
}

fn defer(call: &AdaptiveLeadershipReviewCallV1) -> CompleteAdaptiveLeadershipReviewCallV1 {
    let mut result = completion(call, false);
    result.decision = AdaptiveLeadershipReviewDecisionV1 {
        schema_version: call.grant.schema_version,
        decision: AdaptiveLeadershipReviewDecisionKindV1::DeferBudget {
            rationale: "Do not continue this reviewed source".into(),
            evidence_refs: vec![call.context.evidence_refs[0].clone()],
        },
    };
    result
}

pub(super) fn continued(call: &AdaptiveLeadershipReviewCallV1, now: u64, calls: u16)
    -> CompleteAdaptiveLeadershipReviewCallV1
{
    let mut result = completion(call, false);
    result.decision = AdaptiveLeadershipReviewDecisionV1 {
        schema_version: call.grant.schema_version,
        decision: AdaptiveLeadershipReviewDecisionKindV1::Continue {
            additional_model_calls: calls, window_ms: WINDOW_MS,
            rationale: "Continue only within the explicitly funded epoch".into(),
            evidence_refs: vec![call.grant.work_funding.as_ref().map_or_else(
                || call.context.evidence_refs[0].clone(), |epoch| epoch.evidence_ref().unwrap(),
            )],
        },
    };
    let resolution = adaptive_leadership_continuation_audit_id(
        call.grant.review_id, &result.request_digest, &result.model_response_digest, &result.decision,
    ).unwrap();
    let allowance = call.continuation_allowance(now, now + WINDOW_MS, calls).unwrap();
    let Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }) = &call.grant.subject
    else { panic!("budget source"); };
    result.resolution_event_id = Some(resolution);
    result.continuation = Some(crate::AdaptiveContinuationAuthorizationV1 {
        schema_version: 1, operation_id: call.operation_id, review_id: call.grant.review_id,
        resolution_event_id: resolution, session_id: call.grant.session_id,
        source_session_version: call.grant.expected_session_version,
        source: crate::AdaptiveContinuationSourceV1::BudgetWindowExhausted {
            active_allowance_digest: budget.active_allowance_digest.clone(),
            continuation_history_digest: budget.continuation_history_digest.clone(),
        },
        abandoned_model_effect: None, provider_allowance_id: allowance.allowance_id.clone(),
        provider_authority_digest: adaptive_leadership_continuation_provider_authority_digest(
            &allowance, &call.grant.assignee_authority,
        ).unwrap(),
        issued_at_ms: now, deadline_ms: now + WINDOW_MS, additional_model_calls: calls,
        local_adoption: None, resume_policy: call.grant.resume_policy.clone(),
        work_funding: call.grant.work_funding.clone(),
    });
    result
}

fn funded_fixture() -> (Fixture, AdaptiveWorkFundingReceiptV1, u64) {
    let mut f = budget_fixture(2);
    let now = CONTINUATION_AT + 7;
    let receipt = fund(&f, now);
    bind(&mut f, &receipt, receipt.request.source.resume_source.base_review_count + 1);
    (f, receipt, now)
}

fn funding_operator(f: &Fixture) -> AuthenticatedCompanyPrincipalV1 {
    let mut operator = f.leader.clone();
    operator.kind = CompanyPrincipalKindV1::Operator;
    operator.agent_id = None;
    operator
}

fn supersession_draft(
    f: &Fixture,
    receipt: &AdaptiveWorkFundingReceiptV1,
    operation: Uuid,
    now: u64,
) -> Result<crate::AdaptiveWorkFundingRequestV1, WorkflowError> {
    let mut limits = receipt.request.limits.clone();
    limits.expires_at_unix_ms = now + 3_600_000;
    f.store.adaptive_work_funding_draft_with_supersession(
        &funding_operator(f), &f.grant.project_id, f.grant.session_id, operation,
        "replace-expired-unused-proposal", limits, now, Some(&receipt.receipt_digest()?),
    )
}

#[test]
fn expired_unused_funding_replacement_is_explicit_atomic_and_preserves_replay_and_spend() {
    let (mut f, old, _) = funded_fixture();
    let now = old.request.limits.expires_at_unix_ms;
    let original = session(&f);
    let operator = funding_operator(&f);
    assert_eq!(f.store.adaptive_work_funding_for_review(
        &operator.tenant_id, f.grant.session_id, now,
    ).unwrap(), None);
    let mut limits = old.request.limits.clone();
    limits.expires_at_unix_ms = now + 3_600_000;
    assert!(f.store.adaptive_work_funding_draft(
        &operator, &f.grant.project_id, f.grant.session_id, Uuid::new_v4(),
        "no-implicit-replacement", limits, now,
    ).is_err());
    let replacement = supersession_draft(&f, &old, Uuid::new_v4(), now).unwrap();
    assert_eq!(replacement.source.current_model_call_ceiling, original.grant.max_model_calls);
    assert!(replacement.source.predecessor_receipt_digest.is_none());
    assert_eq!(replacement.source.supersedes_unused_receipt_digest.as_deref(),
        Some(old.receipt_digest().unwrap().as_str()));
    let before = rows(&f.store);
    let (_, fresh) = f.store.authorize_adaptive_work_funding(&operator, &replacement, now).unwrap();
    assert_eq!(session(&f), original);
    assert_eq!(fresh.resulting_model_call_ceiling().unwrap(), old.resulting_model_call_ceiling().unwrap());
    assert_eq!(fresh.resulting_tool_call_ceiling().unwrap(), old.resulting_tool_call_ceiling().unwrap());
    let after = rows(&f.store);
    assert_ne!(after, before);
    let reopened = WorkflowStore::open(&f.path).unwrap();
    assert!(reopened.authorize_adaptive_work_funding(&operator, &old.request, now + 1).unwrap().1 == old);
    assert!(reopened.authorize_adaptive_work_funding(&operator, &replacement, now + 1).unwrap().1 == fresh);
    assert_eq!(rows(&reopened), after);
    assert_eq!(reopened.adaptive_work_funding_for_review(
        &operator.tenant_id, f.grant.session_id, now,
    ).unwrap().unwrap().receipt, fresh);
    assert!(supersession_draft(&f, &old, Uuid::new_v4(), now + 1).is_err());
    // A caller cannot backdate a manual old-epoch review after replacement.
    let old_time = old.issued_at_unix_ms + 1;
    assert!(issue(&f, old_time).is_err());
    assert_eq!(rows(&f.store), after);
    budget_context(&mut f, now, "new-explicit-proposal");
    bind(&mut f, &fresh, fresh.request.source.resume_source.base_review_count + 1);
    let call = dispatch(&f, now);
    f.store.complete_adaptive_leadership_review_call(
        &f.leader, &continued(&call, now + 2, 1), now + 2,
    ).unwrap();
    assert_eq!(session(&f).grant, original.grant);
    assert_eq!((session(&f).model_calls, session(&f).tool_calls),
        (original.model_calls, original.tool_calls));
    assert!(f.store.adaptive_work_funding(&operator.tenant_id,
        f.grant.session_id, old.request.operation_id).unwrap().unwrap() == old);
}

#[test]
fn unused_funding_supersession_rejects_unexpired_wrong_digest_and_changed_source_without_writes() {
    let (f, old, issued) = funded_fixture();
    let before = rows(&f.store);
    assert!(supersession_draft(&f, &old, Uuid::new_v4(), issued + 1).is_err());
    let now = old.request.limits.expires_at_unix_ms;
    let operator = funding_operator(&f);
    let mut draft = supersession_draft(&f, &old, Uuid::new_v4(), now).unwrap();
    draft.source.supersedes_unused_receipt_digest = Some("f".repeat(64));
    assert!(f.store.authorize_adaptive_work_funding(&operator, &draft, now).is_err());
    assert_eq!(rows(&f.store), before);
    let draft = supersession_draft(&f, &old, Uuid::new_v4(), now).unwrap();
    change_project(&f, now);
    let changed = rows(&f.store);
    assert!(f.store.authorize_adaptive_work_funding(&operator, &draft, now).is_err());
    assert_eq!(rows(&f.store), changed);
}

#[test]
fn issued_funding_reviews_permanently_veto_unused_supersession_in_all_states() {
    for state in ["authorized", "dispatched", "retired", "defer", "continue", "orphan_event"] {
        let (f, receipt, now) = funded_fixture();
        let call = issue(&f, now).unwrap();
        let dispatched = if state != "authorized" && state != "orphan_event" {
            Some(f.store.claim_adaptive_leadership_review_call(&f.leader, &claim(&call), now + 1).unwrap())
        } else { None };
        match state {
            "retired" => { f.store.expire_adaptive_leadership_review_call(
                &f.leader, call.grant.review_id, dispatched.as_ref().unwrap().version,
                call.grant.expires_at_unix_ms,
            ).unwrap(); },
            "defer" => { f.store.complete_adaptive_leadership_review_call(
                &f.leader, &defer(dispatched.as_ref().unwrap()), now + 2,
            ).unwrap(); },
            "continue" => { f.store.complete_adaptive_leadership_review_call(
                &f.leader, &continued(dispatched.as_ref().unwrap(), now + 2, 1), now + 2,
            ).unwrap(); },
            "orphan_event" => {
                let connection = f.store.connection.lock().unwrap();
                connection.execute("DELETE FROM company_entities WHERE entity_kind IN
                    ('adaptive_work_funding_review','adaptive_leadership_review_call')", []).unwrap();
            },
            _ => {},
        }
        let before = rows(&f.store);
        assert!(supersession_draft(&f, &receipt, Uuid::new_v4(),
            receipt.request.limits.expires_at_unix_ms).is_err());
        assert_eq!(rows(&f.store), before);
    }
}

#[test]
fn missing_or_corrupt_unused_funding_proof_rejects_supersession_without_writes() {
    for damage in ["entity", "event", "corrupt_event"] {
        let (f, old, _) = funded_fixture();
        let now = old.request.limits.expires_at_unix_ms;
        let request = supersession_draft(&f, &old, Uuid::new_v4(), now).unwrap();
        {
            let connection = f.store.connection.lock().unwrap();
            let sql = match damage {
                "entity" => "DELETE FROM company_entities WHERE entity_kind='adaptive_work_funding'",
                "event" => "DELETE FROM company_events WHERE event_type='adaptive_work_funding_issued'",
                _ => "UPDATE company_events SET payload=X'00' WHERE event_type='adaptive_work_funding_issued'",
            };
            assert_eq!(connection.execute(sql, []).unwrap(), 1);
        }
        let before = rows(&f.store);
        let reopened = WorkflowStore::open(&f.path).unwrap();
        assert!(reopened.authorize_adaptive_work_funding(&funding_operator(&f), &request, now).is_err());
        assert_eq!(rows(&reopened), before);
    }
}

#[test]
fn valid_json_cannot_hide_issued_review_traces_from_unused_supersession() {
    for damage in ["event_payload", "orphan_membership_payload"] {
        let (f, old, issued) = funded_fixture();
        let now = old.request.limits.expires_at_unix_ms;
        let request = supersession_draft(&f, &old, Uuid::new_v4(), now).unwrap();
        issue(&f, issued).unwrap();
        {
            let connection = f.store.connection.lock().unwrap();
            assert_eq!(connection.execute("DELETE FROM company_entities
                WHERE entity_kind='adaptive_leadership_review_call'
                AND json_extract(payload,'$.grant.work_funding.receipt.funding_id')=?1",
                [&old.funding_id]).unwrap(), 1);
            if damage == "event_payload" {
                assert_eq!(connection.execute("DELETE FROM company_entities
                    WHERE entity_kind='adaptive_work_funding_review'
                    AND json_extract(payload,'$.grant.work_funding.receipt.funding_id')=?1",
                    [&old.funding_id]).unwrap(), 1);
                assert_eq!(connection.execute("UPDATE company_events SET payload='{}'
                    WHERE event_type IN ('adaptive_work_funding_review_issued','adaptive_leadership_review_authorized')
                    AND json_extract(payload,'$.grant.work_funding.receipt.funding_id')=?1",
                    [&old.funding_id]).unwrap(), 2);
            } else {
                assert_eq!(connection.execute("DELETE FROM company_events
                    WHERE event_type IN ('adaptive_work_funding_review_issued','adaptive_leadership_review_authorized')
                    AND json_extract(payload,'$.grant.work_funding.receipt.funding_id')=?1",
                    [&old.funding_id]).unwrap(), 2);
                assert_eq!(connection.execute("UPDATE company_entities SET payload='{}'
                    WHERE entity_kind='adaptive_work_funding_review'
                    AND json_extract(payload,'$.grant.work_funding.receipt.funding_id')=?1",
                    [&old.funding_id]).unwrap(), 1);
            }
            let visible: i64 = connection.query_row("SELECT count(*) FROM company_events
                WHERE event_type IN ('adaptive_work_funding_review_issued','adaptive_leadership_review_authorized')
                AND json_extract(payload,'$.grant.work_funding.receipt.funding_id')=?1",
                [&old.funding_id], |row| row.get(0)).unwrap();
            assert_eq!(visible, 0);
        }
        let before = rows(&f.store);
        let reopened = WorkflowStore::open(&f.path).unwrap();
        assert!(supersession_draft(&f, &old, Uuid::new_v4(), now).is_err());
        assert!(reopened.authorize_adaptive_work_funding(&funding_operator(&f), &request, now).is_err());
        assert_eq!(rows(&reopened), before);
    }
}

#[test]
fn unused_expiry_history_counts_toward_the_permanent_issuance_cap() {
    let (f, mut receipt, _) = funded_fixture();
    let operator = funding_operator(&f);
    for _ in 1..128 {
        let now = receipt.request.limits.expires_at_unix_ms;
        let request = supersession_draft(&f, &receipt, Uuid::new_v4(), now).unwrap();
        receipt = f.store.authorize_adaptive_work_funding(&operator, &request, now).unwrap().1;
    }
    let before = rows(&f.store);
    assert!(supersession_draft(&f, &receipt, Uuid::new_v4(),
        receipt.request.limits.expires_at_unix_ms).is_err());
    assert_eq!(rows(&f.store), before);
}

#[test]
fn concurrent_unused_funding_replacements_have_exactly_one_successor() {
    let (f, old, _) = funded_fixture();
    let now = old.request.limits.expires_at_unix_ms;
    let requests = [supersession_draft(&f, &old, Uuid::new_v4(), now).unwrap(),
        supersession_draft(&f, &old, Uuid::new_v4(), now).unwrap()];
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut jobs = Vec::new();
    for request in requests {
        let store = WorkflowStore::open(&f.path).unwrap();
        let operator = funding_operator(&f);
        let barrier = barrier.clone();
        jobs.push(std::thread::spawn(move || {
            barrier.wait();
            store.authorize_adaptive_work_funding(&operator, &request, now)
        }));
    }
    let results = jobs.into_iter().map(|job| job.join().unwrap()).collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let connection = f.store.connection.lock().unwrap();
    let count: i64 = connection.query_row("SELECT count(*) FROM company_entities
        WHERE entity_kind='adaptive_work_funding'", [], |row| row.get(0)).unwrap();
    assert_eq!(count, 2);
}

#[test]
fn funded_continue_is_atomic_preserves_source_spend_and_replays_historically() {
    let mut f = budget_fixture(2);
    let now = CONTINUATION_AT + 7;
    let source = session(&f);
    let project = f.context.source_project.clone();
    let legacy_bytes = encode(&f.grant).unwrap();
    assert!(!String::from_utf8(legacy_bytes.clone()).unwrap().contains("work_funding"));
    let receipt = fund(&f, now);
    assert_eq!(session(&f), source);
    assert_eq!(f.store.company_project(&f.leader.tenant_id, &f.grant.project_id).unwrap().unwrap(), project);
    assert_eq!(encode(&f.grant).unwrap(), legacy_bytes);
    assert!(source.active_work_funding().is_none());
    bind(&mut f, &receipt, receipt.request.source.resume_source.base_review_count + 1);
    assert_eq!(f.store.adaptive_work_funding_for_review(
        &f.leader.tenant_id, f.grant.session_id, now,
    ).unwrap().as_ref(), f.grant.work_funding.as_deref());
    let call = dispatch(&f, now);
    assert_eq!(session(&f), source);
    let before = rows(&f.store);
    assert!(f.store.advance_adaptive_session(
        source.grant.session_id, source.version, Uuid::new_v4(),
        &AdaptiveTransitionV1::ClaimModel {
            effect: AdaptiveEffectV1 { id: Uuid::new_v4(), request_digest: DIGEST.into() },
            previous_observation_digest: source.last_observation.as_ref().map(|observation| observation.observation_digest.clone()),
        }, &f.grant.assignee_authority, now + 1,
    ).is_err());
    assert_eq!(rows(&f.store), before);
    let result = continued(&call, now + 2, 5);
    let completed = f.store.complete_adaptive_leadership_review_call(&f.leader, &result, now + 2).unwrap();
    let adopted = session(&f);
    assert_eq!(adopted.grant, source.grant);
    assert_eq!((adopted.model_calls, adopted.tool_calls), (source.model_calls, source.tool_calls));
    assert_eq!(adopted.funded_model_call_ceiling(), 8);
    assert_eq!(adopted.funded_tool_call_ceiling(), 8);
    assert_eq!(adopted.active_work_funding(), call.grant.work_funding.as_deref());
    assert_eq!(adopted.continuation.as_ref().unwrap().authorizations.len(), 2);
    assert!(adopted.requires_fresh_observation());
    let final_project = f.store.company_project(&f.leader.tenant_id, &f.grant.project_id).unwrap().unwrap();
    assert_eq!(final_project.subscription_call.as_ref().unwrap().grant.max_calls, 5);
    assert!(final_project.subscription_call.as_ref().unwrap().dispatch.is_none());
    let before = rows(&f.store);
    assert_eq!(f.store.complete_adaptive_leadership_review_call(
        &f.leader, &result, receipt.request.limits.expires_at_unix_ms + 1,
    ).unwrap(), completed);
    assert_eq!(rows(&f.store), before);
    assert_eq!(f.store.adaptive_leadership_review_call(
        &f.leader.tenant_id, call.grant.review_id,
    ).unwrap(), Some(completed.clone()));
    let reopened = WorkflowStore::open(&f.path).unwrap();
    assert_eq!(reopened.adaptive_leadership_review_call(
        &f.leader.tenant_id, call.grant.review_id,
    ).unwrap(), Some(completed));
    assert_eq!(reopened.adaptive_session(f.grant.session_id, &f.grant.assignee_authority).unwrap().unwrap(), adopted);
    assert_eq!(reopened.company_project(&f.leader.tenant_id, &f.grant.project_id).unwrap().unwrap(), final_project);
    let mut changed = result.clone();
    changed.model_response_digest = "b".repeat(64);
    assert!(f.store.complete_adaptive_leadership_review_call(&f.leader, &changed, now + 3).is_err());
    assert_eq!(rows(&f.store), before);
}

#[test]
fn completed_funded_defer_is_not_a_successor_or_replenishment_trigger() {
    let (mut f, receipt, now) = funded_fixture();
    let source = session(&f);
    let call = dispatch(&f, now);
    let result = defer(&call);
    let refused = f.store.complete_adaptive_leadership_review_call(&f.leader, &result, now + 2).unwrap();
    assert_eq!(session(&f), source);
    assert!(session(&f).active_work_funding().is_none());
    assert_eq!(f.store.adaptive_work_funding_for_review(
        &f.leader.tenant_id, f.grant.session_id, now + 3,
    ).unwrap(), None);
    let before = rows(&f.store);
    assert_eq!(f.store.complete_adaptive_leadership_review_call(&f.leader, &result, now + 3).unwrap(), refused);
    f.context.evidence_refs.push("new-explanation-same-head".into());
    bind(&mut f, &receipt, receipt.request.source.resume_source.base_review_count + 2);
    assert!(issue(&f, now + 3).is_err());
    assert_eq!(rows(&f.store), before);
    change_project(&f, now + 4);
    budget_context(&mut f, now + 5, "project-only-change");
    bind(&mut f, &receipt, receipt.request.source.resume_source.base_review_count + 2);
    let before = rows(&f.store);
    assert!(issue(&f, now + 5).is_err());
    assert_eq!(rows(&f.store), before);
    assert_eq!(session(&f), source);
}

#[test]
fn new_explicit_funding_can_review_a_retained_unfunded_defer_without_rewriting_it() {
    let mut f = budget_fixture(4);
    let now = CONTINUATION_AT + 7;
    let old = dispatch_budget(&f, now);
    let refused = f.store.complete_adaptive_leadership_review_call(&f.leader, &defer(&old), now + 2).unwrap();
    let source = session(&f);
    let receipt = fund(&f, now + 3);
    bind(&mut f, &receipt, receipt.request.source.resume_source.base_review_count + 1);
    let new = dispatch(&f, now + 3);
    assert_ne!(new.grant.review_id, refused.grant.review_id);
    assert_eq!(session(&f), source);
    assert_eq!(f.store.adaptive_leadership_review_call(&f.leader.tenant_id, refused.grant.review_id).unwrap(), Some(refused));
    assert_eq!(new.grant.work_funding.as_ref().unwrap().binding.ordinal, 3);
}

#[test]
fn historical_v1_resume_policy_reviews_and_authorizations_coexist_with_funding() {
    let mut f = budget_fixture(4);
    let now = CONTINUATION_AT + 7;
    let mut operator = f.leader.clone();
    operator.kind = CompanyPrincipalKindV1::Operator;
    operator.agent_id = None;
    let request = f.store.adaptive_resume_policy_draft(
        &operator, &f.grant.project_id, f.grant.session_id, Uuid::new_v4(),
        "original-v1-policy", now + 3_600_000, now,
    ).unwrap();
    let policy = f.store.authorize_adaptive_resume_policy_with_unknown_proof(
        &operator, &request, now, |_, _| Ok(DIGEST.into()),
    ).unwrap().1;
    let binding = policy.binding(policy.request.source.base_review_count + 1).unwrap();
    f.context.evidence_refs.push(format!("adaptive-resume-policy:{}:{}", binding.receipt_digest, binding.ordinal));
    f.grant.resume_policy = Some(Box::new(binding));
    rebind_budget_evidence(&mut f);
    let old = dispatch(&f, now);
    let old_result = continued(&old, now + 2, 1);
    let old_review = f.store.complete_adaptive_leadership_review_call(&f.leader, &old_result, now + 2).unwrap();
    let source = observe_budget_inspection(&f, session(&f), now + 3);
    let old_authorizations = source.continuation.as_ref().unwrap().authorizations.clone();
    assert!(old_authorizations.last().unwrap().resume_policy.is_some());
    budget_context(&mut f, now + 7, "funding-after-v1-policy");
    f.grant.resume_policy = None;
    let receipt = fund(&f, now + 7);
    bind(&mut f, &receipt, receipt.request.source.resume_source.base_review_count + 1);
    let new = dispatch(&f, now + 7);
    f.store.complete_adaptive_leadership_review_call(&f.leader, &continued(&new, now + 9, 5), now + 9).unwrap();
    let adopted = session(&f);
    assert_eq!(adopted.grant, source.grant);
    assert_eq!((adopted.model_calls, adopted.tool_calls), (source.model_calls, source.tool_calls));
    assert!(adopted.continuation.as_ref().unwrap().authorizations.starts_with(&old_authorizations));
    assert_eq!(adopted.funded_model_call_ceiling(), 10);
    assert_eq!(f.store.adaptive_leadership_review_call(&f.leader.tenant_id, old.grant.review_id).unwrap(), Some(old_review));
}

#[test]
fn unresolved_invalid_expired_and_stale_funded_reviews_never_reroll_the_same_head() {
    for stale in [false, true] {
        let (mut f, receipt, now) = funded_fixture();
        let call = dispatch(&f, now);
        let mut invalid = defer(&call);
        invalid.decision.schema_version = 3;
        let before = rows(&f.store);
        assert!(f.store.complete_adaptive_leadership_review_call(&f.leader, &invalid, now + 2).is_err());
        assert_eq!(rows(&f.store), before);
        bind(&mut f, &receipt, receipt.request.source.resume_source.base_review_count + 2);
        assert!(issue(&f, now + 2).is_err());
        assert_eq!(rows(&f.store), before);
        let at = if stale { now + 3 } else { call.grant.expires_at_unix_ms };
        if stale {
            change_project(&f, at);
            f.store.retire_stale_adaptive_leadership_review_call(
                &f.leader, call.grant.review_id, call.version, at,
            ).unwrap();
        } else {
            f.store.expire_adaptive_leadership_review_call(
                &f.leader, call.grant.review_id, call.version, at,
            ).unwrap();
        }
        budget_context(&mut f, at + 1, "no-automatic-successor");
        bind(&mut f, &receipt, receipt.request.source.resume_source.base_review_count + 2);
        let before = rows(&f.store);
        assert!(issue(&f, at + 1).is_err());
        assert_eq!(rows(&f.store), before);
        assert!(session(&f).active_work_funding().is_none());
        assert_eq!(f.store.adaptive_work_funding_for_review(
            &f.leader.tenant_id, f.grant.session_id, at + 1,
        ).unwrap(), None);
    }
}

#[test]
fn productive_heads_get_bounded_successors_with_global_ordinals_and_exact_totals() {
    let (mut f, receipt, now) = funded_fixture();
    let first = dispatch(&f, now);
    f.store.complete_adaptive_leadership_review_call(&f.leader, &continued(&first, now + 2, 5), now + 2).unwrap();
    let observed = observe_budget_inspection(&f, session(&f), now + 3);
    assert_eq!(observed.model_calls, 3);
    assert_eq!(observed.tool_calls, 2);
    let next = observed.active_deadline_ms();
    budget_context(&mut f, next, "productive-successor");
    bind(&mut f, &receipt, receipt.request.source.resume_source.base_review_count + 2);
    assert_eq!(f.store.adaptive_work_funding_for_review(
        &f.leader.tenant_id, f.grant.session_id, next,
    ).unwrap().as_ref(), f.grant.work_funding.as_deref());
    let second = dispatch(&f, next);
    assert_eq!(second.grant.work_funding.as_ref().unwrap().binding.ordinal, 3);
    assert!(second.continuation_allowance(next + 2, next + 2 + WINDOW_MS, 5).is_ok());
    assert!(second.continuation_allowance(next + 2, next + 2 + WINDOW_MS, 6).is_err());
    f.store.complete_adaptive_leadership_review_call(&f.leader, &continued(&second, next + 2, 5), next + 2).unwrap();
    let source = session(&f);
    assert_eq!(source.grant.max_model_calls, 2);
    assert_eq!(source.funded_model_call_ceiling(), 8);
    assert_eq!(source.continuation.as_ref().unwrap().authorizations.len(), 3);
    let expiry = source.active_deadline_ms();
    budget_context(&mut f, expiry, "adoption-only-head-is-not-progress");
    bind(&mut f, &receipt, receipt.request.source.resume_source.base_review_count + 3);
    let before = rows(&f.store);
    assert!(issue(&f, expiry).is_err());
    assert_eq!(rows(&f.store), before);
    assert_eq!(f.store.adaptive_work_funding_for_review(
        &f.leader.tenant_id, f.grant.session_id, expiry,
    ).unwrap(), None);
}

#[test]
fn adopted_successor_epoch_derives_prior_proof_and_cumulative_authenticated_totals() {
    let (mut f, first_receipt, now) = funded_fixture();
    let first = dispatch(&f, now);
    let first_result = continued(&first, now + 2, 5);
    let first_completed = f.store.complete_adaptive_leadership_review_call(
        &f.leader, &first_result, now + 2,
    ).unwrap();
    let progressed = observe_budget_inspection(&f, session(&f), now + 3);
    let prior_authorizations = progressed.continuation.as_ref().unwrap().authorizations.clone();
    assert_eq!((progressed.model_calls, progressed.tool_calls), (3, 2));
    assert_eq!(progressed.funded_model_call_ceiling(), 8);
    assert_eq!(progressed.funded_tool_call_ceiling(), 8);
    let next = progressed.active_deadline_ms();
    budget_context(&mut f, next, "explicit-successor-epoch-source");
    let second_receipt = fund(&f, next);
    assert_eq!(session(&f), progressed);
    let proposal = &second_receipt.request.source;
    let anchor = &proposal.resume_source;
    assert_eq!(proposal.predecessor_receipt_digest, Some(first_receipt.receipt_digest().unwrap()));
    assert_eq!(proposal.original_model_call_ceiling, progressed.grant.max_model_calls);
    assert_eq!(proposal.original_tool_call_ceiling, progressed.grant.max_tool_calls);
    assert_eq!(proposal.current_model_call_ceiling, progressed.funded_model_call_ceiling());
    assert_eq!(proposal.current_tool_call_ceiling, progressed.funded_tool_call_ceiling());
    assert_eq!((anchor.base_model_calls, anchor.base_tool_calls), (3, 2));
    assert_eq!((anchor.base_review_count, anchor.base_window_count), (2, 2));
    assert_eq!(anchor.expected_session_version, progressed.version);
    assert_eq!(anchor.continuation_history_digest, crate::adaptive_budget_history_digest(&progressed.continuation).unwrap());
    assert_eq!(second_receipt.resulting_model_call_ceiling().unwrap(), 14);
    assert_eq!(second_receipt.resulting_tool_call_ceiling().unwrap(), 14);
    bind(&mut f, &second_receipt, anchor.base_review_count + 1);
    assert_eq!(f.store.adaptive_work_funding_for_review(
        &f.leader.tenant_id, f.grant.session_id, next,
    ).unwrap().as_ref(), f.grant.work_funding.as_deref());
    let second = dispatch(&f, next);
    assert_eq!(session(&f), progressed);
    assert_eq!(second.grant.work_funding.as_ref().unwrap().binding.ordinal, 3);
    let second_result = continued(&second, next + 2, 6);
    f.store.complete_adaptive_leadership_review_call(&f.leader, &second_result, next + 2).unwrap();
    let adopted = session(&f);
    assert_eq!(adopted.grant, progressed.grant);
    assert_eq!((adopted.model_calls, adopted.tool_calls), (progressed.model_calls, progressed.tool_calls));
    assert_eq!(adopted.funded_model_call_ceiling(), 14);
    assert_eq!(adopted.funded_tool_call_ceiling(), 14);
    assert_eq!(adopted.active_work_funding(), second.grant.work_funding.as_deref());
    let history = &adopted.continuation.as_ref().unwrap().authorizations;
    assert!(history.starts_with(&prior_authorizations));
    assert_eq!(history.len(), 3);
    assert_eq!(history.last(), second_result.continuation.as_ref());
    assert!(history.iter().any(|authorization| {
        authorization.work_funding.as_ref().is_some_and(|epoch| epoch.receipt == first_receipt)
    }));
    assert_eq!(f.store.adaptive_work_funding(
        &f.leader.tenant_id, f.grant.session_id, first_receipt.request.operation_id,
    ).unwrap(), Some(first_receipt));
    assert_eq!(f.store.adaptive_leadership_review_call(
        &f.leader.tenant_id, first.grant.review_id,
    ).unwrap(), Some(first_completed));
    let reopened = WorkflowStore::open(&f.path).unwrap();
    assert_eq!(reopened.adaptive_session(
        f.grant.session_id, &f.grant.assignee_authority,
    ).unwrap().unwrap(), adopted);
}

#[test]
fn adopted_window_cap_returns_no_selector_even_with_unspent_calls_and_review_slot() {
    let mut f = budget_fixture(2);
    let now = CONTINUATION_AT + 7;
    let receipt = fund_with_windows(&f, now, 2);
    let base = receipt.request.source.resume_source.base_review_count;
    bind(&mut f, &receipt, base + 1);
    let first = dispatch(&f, now);
    f.store.complete_adaptive_leadership_review_call(
        &f.leader, &continued(&first, now + 2, 2), now + 2,
    ).unwrap();
    let observed = observe_budget_inspection(&f, session(&f), now + 3);
    let next = observed.active_deadline_ms();
    budget_context(&mut f, next, "last-funded-window");
    bind(&mut f, &receipt, base + 2);
    let second = dispatch(&f, next);
    f.store.complete_adaptive_leadership_review_call(
        &f.leader, &continued(&second, next + 2, 2), next + 2,
    ).unwrap();
    let observed = observe_budget_inspection(&f, session(&f), next + 3);
    let epoch = observed.active_work_funding().unwrap();
    assert_eq!(observed.continuation.as_ref().unwrap().authorizations.len(),
        usize::from(epoch.binding.limits.total_window_ceiling));
    let reviews = f.store.adaptive_leadership_review_calls(
        &f.leader.tenant_id, f.grant.session_id,
    ).unwrap();
    assert!(reviews.len() < usize::from(epoch.binding.limits.total_review_ceiling));
    assert!(observed.model_calls < epoch.binding.limits.total_model_call_ceiling);
    assert!(observed.tool_calls < epoch.binding.limits.total_tool_call_ceiling);
    let exhausted = observed.active_deadline_ms();
    assert!(observed.model_window_exhausted_at(exhausted));
    let before = rows(&f.store);
    assert_eq!(f.store.adaptive_work_funding_for_review(
        &f.leader.tenant_id, f.grant.session_id, exhausted,
    ).unwrap(), None);
    assert_eq!(rows(&f.store), before);
    budget_context(&mut f, exhausted, "no-window-replenishment");
    bind(&mut f, &receipt, base + 3);
    assert!(issue(&f, exhausted).is_err());
    assert_eq!(rows(&f.store), before);
    assert_eq!(session(&f), observed);
}

#[test]
fn manufactured_epoch_and_forged_review_membership_are_not_stored_authority() {
    let (mut f, mut receipt, now) = funded_fixture();
    receipt.request.operation_id = Uuid::new_v4();
    receipt.funding_id = crate::adaptive_work_funding_id(
        &f.leader.tenant_id, f.grant.session_id, receipt.request.operation_id,
    ).unwrap();
    bind(&mut f, &receipt, receipt.request.source.resume_source.base_review_count + 1);
    f.grant.work_funding.as_ref().unwrap().validate().unwrap();
    f.context.validate(&f.grant).unwrap();
    let before = rows(&f.store);
    assert!(issue(&f, now).is_err());
    assert_eq!(rows(&f.store), before);

    let (f, receipt, now) = funded_fixture();
    let call = dispatch(&f, now);
    let mut forged = call.clone();
    forged.grant.work_funding.as_mut().unwrap().binding = receipt.binding(
        receipt.request.source.resume_source.base_review_count + 2,
    ).unwrap();
    let connection = f.store.connection.lock().unwrap();
    assert!(require_funding_review_membership(
        &connection, &forged.grant, &forged.context_digest().unwrap(), forged.operation_id,
    ).is_err());
    let result = continued(&call, now + 2, 5);
    assert!(require_funding_authorization_membership(
        &connection, result.continuation.as_ref().unwrap(), &call.grant.assignee_authority,
    ).is_err());
}

#[test]
fn missing_adoption_or_its_event_corrupts_completed_receipt_and_reopened_journal() {
    for target in ["adoption", "adoption-event"] {
        let (f, _, now) = funded_fixture();
        let call = dispatch(&f, now);
        let result = continued(&call, now + 2, 5);
        f.store.complete_adaptive_leadership_review_call(&f.leader, &result, now + 2).unwrap();
        let connection = f.store.connection.lock().unwrap();
        let changed = match target {
            "adoption" => connection.execute(
                "DELETE FROM company_entities WHERE entity_kind='adaptive_work_funding_adoption'", [],
            ).unwrap(),
            "adoption-event" => connection.execute(
                "DELETE FROM company_events WHERE event_type='adaptive_work_funding_adopted'", [],
            ).unwrap(),
            _ => unreachable!(),
        };
        assert_eq!(changed, 1);
        drop(connection);
        let before = rows(&f.store);
        assert!(f.store.adaptive_leadership_review_call(&f.leader.tenant_id, call.grant.review_id).is_err());
        assert!(f.store.complete_adaptive_leadership_review_call(&f.leader, &result, now + 3).is_err());
        let reopened = WorkflowStore::open(&f.path).unwrap();
        assert!(reopened.adaptive_session(f.grant.session_id, &f.grant.assignee_authority).is_err());
        assert_eq!(rows(&f.store), before);
    }
}

#[test]
fn funded_completion_event_failure_rolls_back_journal_adoption_allowance_and_receipt() {
    let (f, _, now) = funded_fixture();
    let call = dispatch(&f, now);
    let result = continued(&call, now + 2, 5);
    f.store.connection.lock().unwrap().execute_batch(
        "CREATE TRIGGER reject_funded_completion BEFORE INSERT ON company_events
         WHEN NEW.event_type='adaptive_leadership_review_completed'
         BEGIN SELECT RAISE(ABORT, 'test funded completion failure'); END;",
    ).unwrap();
    let before = rows(&f.store);
    assert!(f.store.complete_adaptive_leadership_review_call(&f.leader, &result, now + 2).is_err());
    assert_eq!(rows(&f.store), before);
    assert_eq!(session(&f), call.context.source_session);
    f.store.connection.lock().unwrap().execute_batch("DROP TRIGGER reject_funded_completion").unwrap();
    assert!(f.store.complete_adaptive_leadership_review_call(&f.leader, &result, now + 2).is_ok());
}

#[test]
fn funded_review_event_failure_rolls_back_its_membership_without_adopting_source() {
    let (f, _, now) = funded_fixture();
    let source = session(&f);
    f.store.connection.lock().unwrap().execute_batch(
        "CREATE TRIGGER reject_funded_review BEFORE INSERT ON company_events
         WHEN NEW.event_type='adaptive_leadership_review_authorized'
         BEGIN SELECT RAISE(ABORT, 'test funded review failure'); END;",
    ).unwrap();
    let before = rows(&f.store);
    assert!(issue(&f, now).is_err());
    assert_eq!(rows(&f.store), before);
    assert_eq!(session(&f), source);
    f.store.connection.lock().unwrap().execute_batch("DROP TRIGGER reject_funded_review").unwrap();
    let call = issue(&f, now).unwrap();
    assert_eq!(session(&f), source);
    require_funding_review_membership(
        &f.store.connection.lock().unwrap(), &call.grant, &call.context_digest().unwrap(), call.operation_id,
    ).unwrap();
}

#[test]
fn funded_completion_rejects_missing_or_different_authorization_epoch_without_writes() {
    let (f, receipt, now) = funded_fixture();
    let call = dispatch(&f, now);
    let result = continued(&call, now + 2, 5);
    let before = rows(&f.store);
    for ordinal in [None, Some(receipt.request.source.resume_source.base_review_count + 2)] {
        let mut changed = result.clone();
        changed.continuation.as_mut().unwrap().work_funding = ordinal.map(|ordinal| Box::new(AdaptiveWorkFundingEpochV1 {
            receipt: receipt.clone(), binding: receipt.binding(ordinal).unwrap(),
        }));
        assert!(f.store.complete_adaptive_leadership_review_call(&f.leader, &changed, now + 2).is_err());
        assert_eq!(rows(&f.store), before);
    }
}

#[test]
fn funded_persisted_and_completion_paths_reject_corrupt_receipt_event_and_membership() {
    for target in ["receipt", "receipt-event", "membership"] {
        let (f, _, now) = funded_fixture();
        let call = dispatch(&f, now);
        let result = continued(&call, now + 2, 5);
        let connection = f.store.connection.lock().unwrap();
        let changed = match target {
            "receipt" => connection.execute(
                "UPDATE company_entities SET payload=X'00' WHERE entity_kind='adaptive_work_funding'", [],
            ).unwrap(),
            "receipt-event" => connection.execute(
                "UPDATE company_events SET payload=X'00' WHERE event_type='adaptive_work_funding_issued'", [],
            ).unwrap(),
            "membership" => connection.execute(
                "DELETE FROM company_entities WHERE entity_kind='adaptive_work_funding_review' AND json_extract(payload,'$.grant.review_id')=?1",
                [call.grant.review_id.to_string()],
            ).unwrap(),
            _ => unreachable!(),
        };
        assert!(changed > 0, "missing corruption target {target}");
        drop(connection);
        let before = rows(&f.store);
        assert!(f.store.adaptive_leadership_review_call(&f.leader.tenant_id, call.grant.review_id).is_err());
        assert!(f.store.complete_adaptive_leadership_review_call(&f.leader, &result, now + 2).is_err());
        assert_eq!(rows(&f.store), before);
    }
}

#[test]
fn funded_scope_reuses_tenant_inventories_and_session_supersession_across_reviews() {
    let (mut f, receipt, now) = funded_fixture();
    let first = dispatch(&f, now);
    f.store.complete_adaptive_leadership_review_call(
        &f.leader, &continued(&first, now + 2, 5), now + 2,
    ).unwrap();
    let observed = observe_budget_inspection(&f, session(&f), now + 3);
    let next = observed.active_deadline_ms();
    budget_context(&mut f, next, "scoped-proof-successor");
    bind(&mut f, &receipt, receipt.request.source.resume_source.base_review_count + 2);
    let second = issue(&f, next).unwrap();
    let expected = f.store.adaptive_leadership_review_calls(
        &f.leader.tenant_id, f.grant.session_id,
    ).unwrap();
    assert!(expected.iter().any(|call| call.grant.review_id == first.grant.review_id));
    assert!(expected.iter().any(|call| call == &second));
    let before = rows(&f.store);
    let connection = f.store.connection.lock().unwrap();
    for _ in 0..2 {
        validation_scope::with_scope(&connection, || {
            let actual = calls_for_session(&connection, &f.leader.tenant_id, f.grant.session_id)?;
            assert_eq!(encode(&actual)?, encode(&expected)?);
            let entities = validation_scope::validations("entity");
            for _ in 0..3 {
                assert_eq!(calls_for_session(
                    &connection, &f.leader.tenant_id, Uuid::from_u128(9001),
                )?, Vec::new());
                assert_eq!(calls_for_session(
                    &connection, &f.leader.tenant_id, f.grant.session_id,
                )?, expected);
                assert_eq!(validation_scope::validations("entity"), entities);
                assert_eq!(validation_scope::validations("review-inventory"), 1);
                assert_eq!(validation_scope::validations("funding-inventory"), 1);
                assert_eq!(validation_scope::validations("funding-supersession"), 1);
                assert_eq!(validation_scope::validations("adaptive-journal"), 1);
            }
            assert!(calls_for_session(
                &connection, &TenantId::parse("other-tenant")?, f.grant.session_id,
            )?.is_empty());
            assert_eq!(validation_scope::validations("review-inventory"), 2);
            assert_eq!(validation_scope::validations("funding-inventory"), 1);
            Ok(())
        }).unwrap();
        assert_eq!(validation_scope::validations("funding-inventory"), 0);
    }
    drop(connection);
    assert_eq!(rows(&f.store), before);
}

#[test]
fn funded_supersession_scope_reuses_only_stored_history_after_explicit_replacement() {
    let (mut f, old, _) = funded_fixture();
    let now = old.request.limits.expires_at_unix_ms;
    let draft = supersession_draft(&f, &old, Uuid::new_v4(), now).unwrap();
    let fresh = f.store.authorize_adaptive_work_funding(
        &funding_operator(&f), &draft, now,
    ).unwrap().1;
    budget_context(&mut f, now, "scoped-unused-replacement");
    bind(&mut f, &fresh, fresh.request.source.resume_source.base_review_count + 1);
    let call = issue(&f, now).unwrap();
    let connection = f.store.connection.lock().unwrap();
    validation_scope::with_scope(&connection, || {
        for _ in 0..3 {
            let stored: AdaptiveLeadershipReviewCallV1 = get_entity(
                &connection, &f.leader.tenant_id, KIND, &call.review_key,
            )?.unwrap();
            assert_eq!(stored, call);
            assert_eq!(validation_scope::validations("funding-inventory"), 1);
            assert_eq!(validation_scope::validations("funding-supersession"), 1);
            assert_eq!(validation_scope::validations("unused-funding-review-traces"), 1);
        }
        Ok(())
    }).unwrap();
    drop(connection);
    assert!(supersession_draft(&f, &fresh, Uuid::new_v4(),
        fresh.request.limits.expires_at_unix_ms).is_err());
}

#[test]
fn funded_inventory_scope_pins_external_writes_and_rejects_them_in_the_next_operation() {
    let (f, _, now) = funded_fixture();
    issue(&f, now).unwrap();
    let expected = f.store.adaptive_leadership_review_calls(
        &f.leader.tenant_id, f.grant.session_id,
    ).unwrap();
    let connection = f.store.connection.lock().unwrap();
    let peer = Connection::open(&f.path).unwrap();
    validation_scope::with_scope(&connection, || {
        assert_eq!(calls_for_session(
            &connection, &f.leader.tenant_id, f.grant.session_id,
        )?, expected);
        peer.execute("INSERT INTO company_entities VALUES(?1,'adaptive_work_funding',
            'unrelated-malformed-funding',1,X'00','invalid')", [&f.leader.tenant_id.0])?;
        assert_eq!(calls_for_session(
            &connection, &f.leader.tenant_id, Uuid::from_u128(9002),
        )?, Vec::new());
        assert_eq!(calls_for_session(
            &connection, &f.leader.tenant_id, f.grant.session_id,
        )?, expected);
        assert_eq!(validation_scope::validations("review-inventory"), 1);
        assert_eq!(validation_scope::validations("funding-inventory"), 1);
        Ok(())
    }).unwrap();
    for session_id in [f.grant.session_id, Uuid::from_u128(9002)] {
        assert_eq!(calls_for_session(&connection, &f.leader.tenant_id, session_id)
            .unwrap_err().code, WorkflowErrorCode::CorruptStore);
    }
    peer.execute("DELETE FROM company_entities WHERE entity_id='unrelated-malformed-funding'", []).unwrap();
    assert_eq!(calls_for_session(
        &connection, &f.leader.tenant_id, f.grant.session_id,
    ).unwrap(), expected);
    assert!(connection.is_autocommit());
}

#[test]
fn funded_inventory_same_connection_writes_invalidate_proofs_and_errors_do_not_escape_rollback() {
    for announced in [false, true] {
        let (f, _, now) = funded_fixture();
        issue(&f, now).unwrap();
        let expected = f.store.adaptive_leadership_review_calls(
            &f.leader.tenant_id, f.grant.session_id,
        ).unwrap();
        let before = rows(&f.store);
        let mut connection = f.store.connection.lock().unwrap();
        let transaction = connection.transaction().unwrap();
        let failed = validation_scope::with_scope(&transaction, || {
            assert_eq!(calls_for_session(
                &transaction, &f.leader.tenant_id, f.grant.session_id,
            )?, expected);
            if announced {
                validation_scope::before_write(&transaction)?;
            }
            transaction.execute("UPDATE company_entities SET payload=X'00'
                WHERE entity_kind='adaptive_work_funding'", [])?;
            for _ in 0..2 {
                assert_eq!(calls_for_session(
                    &transaction, &f.leader.tenant_id, Uuid::from_u128(9003),
                ).unwrap_err().code, WorkflowErrorCode::CorruptStore);
            }
            assert_eq!(validation_scope::validations("review-inventory"), 3);
            Err::<(), _>(corrupt())
        });
        assert!(failed.is_err());
        assert!(!transaction.is_autocommit());
        assert_eq!(validation_scope::validations("review-inventory"), 0);
        assert_eq!(calls_for_session(
            &transaction, &f.leader.tenant_id, f.grant.session_id,
        ).unwrap(), expected);
        transaction.rollback().unwrap();
        assert!(connection.is_autocommit());
        drop(connection);
        assert_eq!(rows(&f.store), before);
    }
}

#[test]
fn funded_inventory_unwind_discards_proofs_and_preserves_the_outer_transaction() {
    let (f, _, now) = funded_fixture();
    issue(&f, now).unwrap();
    let mut connection = f.store.connection.lock().unwrap();
    let transaction = connection.transaction().unwrap();
    let expected = calls_for_session(&transaction, &f.leader.tenant_id, f.grant.session_id).unwrap();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _scope = validation_scope::enter(&transaction).unwrap();
        assert_eq!(calls_for_session(
            &transaction, &f.leader.tenant_id, f.grant.session_id,
        ).unwrap(), expected);
        transaction.execute("UPDATE company_entities SET payload=X'00'
            WHERE entity_kind='adaptive_work_funding'", []).unwrap();
        panic!("abandon the owned proof scope");
    }));
    assert!(panic.is_err());
    assert!(!transaction.is_autocommit());
    assert_eq!(validation_scope::validations("funding-inventory"), 0);
    assert_eq!(calls_for_session(
        &transaction, &f.leader.tenant_id, f.grant.session_id,
    ).unwrap(), expected);
    transaction.rollback().unwrap();
}

#[test]
fn funded_inventory_new_operations_reject_corruption_orphans_duplicates_and_unrelated_rows() {
    for damage in ["funding", "funding-event", "orphan", "duplicate", "review",
        "membership-event", "membership", "adoption", "adoption-event", "unrelated-review"]
    {
        let (f, _, now) = funded_fixture();
        let call = dispatch(&f, now);
        f.store.complete_adaptive_leadership_review_call(
            &f.leader, &continued(&call, now + 2, 5), now + 2,
        ).unwrap();
        let connection = f.store.connection.lock().unwrap();
        calls_for_session(&connection, &f.leader.tenant_id, f.grant.session_id).unwrap();
        let sql = match damage {
            "funding" => "UPDATE company_entities SET payload=X'00' WHERE entity_kind='adaptive_work_funding'",
            "funding-event" => "UPDATE company_events SET payload=X'00' WHERE event_type='adaptive_work_funding_issued'",
            "orphan" => "DELETE FROM company_entities WHERE entity_kind='adaptive_work_funding'",
            "duplicate" => "INSERT INTO company_events(event_id,tenant_id,project_id,event_type,operation_id,operation_digest,principal_id,principal_kind,principal_role,agent_id,customer_id,authority_generation,authority_digest,authority_binding_digest,payload,payload_digest,created_at_ms)
                SELECT 'duplicate-funding-event',tenant_id,project_id,event_type,operation_id,operation_digest,principal_id,principal_kind,principal_role,agent_id,customer_id,authority_generation,authority_digest,authority_binding_digest,payload,payload_digest,created_at_ms
                FROM company_events WHERE event_type='adaptive_work_funding_issued'",
            "review" => "UPDATE company_entities SET payload=X'00' WHERE entity_kind='adaptive_leadership_review_call'",
            "membership-event" => "UPDATE company_events SET payload=X'00' WHERE event_type='adaptive_work_funding_review_issued'",
            "membership" => "DELETE FROM company_entities WHERE entity_kind='adaptive_work_funding_review'",
            "adoption" => "DELETE FROM company_entities WHERE entity_kind='adaptive_work_funding_adoption'",
            "adoption-event" => "DELETE FROM company_events WHERE event_type='adaptive_work_funding_adopted'",
            _ => "INSERT INTO company_entities(tenant_id,entity_kind,entity_id,version,payload,payload_digest)
                SELECT tenant_id,'adaptive_leadership_review_call','unrelated-malformed-review',1,X'00','invalid'
                FROM company_entities WHERE entity_kind='adaptive_work_funding' LIMIT 1",
        };
        assert!(connection.execute(sql, []).unwrap() > 0, "{damage}");
        for _ in 0..2 {
            assert_eq!(calls_for_session(
                &connection, &f.leader.tenant_id, Uuid::from_u128(9004),
            ).unwrap_err().code, WorkflowErrorCode::CorruptStore, "{damage}");
            assert_eq!(validation_scope::validations("review-inventory"), 0);
        }
    }
}

#[test]
fn funded_inventory_scopes_keep_tenant_and_byte_bounds() {
    for kind in ["adaptive_work_funding", KIND] {
        let (f, _, now) = funded_fixture();
        issue(&f, now).unwrap();
        let connection = f.store.connection.lock().unwrap();
        connection.execute("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<4097)
            INSERT INTO company_entities(tenant_id,entity_kind,entity_id,version,payload,payload_digest)
            SELECT ?1,?2,'overflow-'||i,1,X'00','invalid' FROM n",
            params![f.leader.tenant_id.0, kind]).unwrap();
        assert_eq!(calls_for_session(&connection, &f.leader.tenant_id, f.grant.session_id)
            .unwrap_err().code, WorkflowErrorCode::CorruptStore);
    }
    let (f, _, now) = funded_fixture();
    issue(&f, now).unwrap();
    let connection = f.store.connection.lock().unwrap();
    let failed = validation_scope::with_scope(&connection, || {
        validation_scope::charge_bytes(&connection, 64 * 1024 * 1024)?;
        calls_for_session(&connection, &f.leader.tenant_id, f.grant.session_id)
    });
    assert_eq!(failed.unwrap_err().code, WorkflowErrorCode::CorruptStore);
    assert!(calls_for_session(&connection, &f.leader.tenant_id, f.grant.session_id).is_ok());
    assert!(connection.is_autocommit());
}

#[test]
fn governed_journal_predicate_reuses_only_exact_inputs_and_invalidates_on_write() {
    let f = budget_fixture(2);
    let allowance = f.context.source_project.subscription_call.as_ref().unwrap();
    let mut connection = f.store.connection.lock().unwrap();
    let transaction = connection.transaction().unwrap();
    let predicate = |value: &SubscriptionCallAllowanceV1| {
        crate::store::adaptive::allowance_is_governed_in_journal(
            &transaction, &f.leader.tenant_id, &f.grant.project_id, value,
        )
    };
    let failed = validation_scope::with_scope(&transaction, || {
        for _ in 0..3 {
            assert!(predicate(allowance)?);
            assert_eq!(validation_scope::validations("governed-journal-predicate"), 1);
            assert_eq!(validation_scope::validations("adaptive-journal"), 1);
            assert_eq!(validation_scope::validations("governed-journal-locators"), 1);
        }
        let mut changed = allowance.clone();
        changed.grant.max_calls += 1;
        assert!(predicate(&changed)?);
        changed.allowance_id.push_str("-absent");
        assert!(!predicate(&changed)?);
        assert!(!predicate(&changed)?);
        assert_eq!(validation_scope::validations("governed-journal-predicate"), 3);
        assert_eq!(validation_scope::validations("adaptive-journal"), 1);
        assert_eq!(validation_scope::validations("governed-journal-locators"), 1);
        assert!(!crate::store::adaptive::allowance_is_governed_in_journal(
            &transaction, &TenantId::parse("other-tenant")?, &f.grant.project_id, allowance,
        )?);
        assert!(!crate::store::adaptive::allowance_is_governed_in_journal(
            &transaction, &f.leader.tenant_id, &ProjectId::parse("other-project")?, allowance,
        )?);
        assert_eq!(validation_scope::validations("governed-journal-predicate"), 5);
        transaction.execute("UPDATE workflow_operations SET request_digest='invalid'
            WHERE operation_namespace=?1 AND operation_id='00000000000000000001'",
            [format!("adaptive-session-v1:{}", f.grant.session_id)])?;
        assert_eq!(predicate(allowance).unwrap_err().code, WorkflowErrorCode::CorruptStore);
        assert_eq!(predicate(allowance).unwrap_err().code, WorkflowErrorCode::CorruptStore);
        assert_eq!(validation_scope::validations("governed-journal-predicate"), 7);
        Err::<(), _>(corrupt())
    });
    assert!(failed.is_err());
    validation_scope::with_scope(&transaction, || {
        assert!(predicate(allowance)?);
        assert_eq!(validation_scope::validations("governed-journal-predicate"), 1);
        Ok(())
    }).unwrap();
    transaction.rollback().unwrap();
}

#[test]
fn project_subscription_reuses_exact_proofs_only_in_the_current_snapshot() {
    let f = budget_fixture(2);
    let project = f.store.company_project(&f.leader.tenant_id, &f.grant.project_id)
        .unwrap().unwrap();
    let connection = f.store.connection.lock().unwrap();
    validation_scope::with_scope(&connection, || {
        for _ in 0..3 {
            subscription::validate_persisted(&connection, &project)?;
            assert_eq!(validation_scope::validations("project-subscription"), 1);
        }
        let mut later = project.clone();
        later.updated_at_unix_ms += 1;
        subscription::validate_persisted(&connection, &later)?;
        assert_eq!(validation_scope::validations("project-subscription"), 2);
        Ok(())
    }).unwrap();
    let (review_id, digest): (String, String) = connection.query_row(
        "SELECT entity_id,payload_digest FROM company_entities WHERE entity_kind=?1 AND json_extract(payload,'$.continuation.provider_allowance_id')=?2",
        params![KIND, project.subscription_call.as_ref().unwrap().allowance_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    connection.execute(
        "UPDATE company_entities SET payload_digest='invalid' WHERE entity_kind=?1 AND entity_id=?2",
        params![KIND, review_id],
    ).unwrap();
    assert!(subscription::validate_persisted(&connection, &project).is_err());
    connection.execute(
        "UPDATE company_entities SET payload_digest=?1 WHERE entity_kind=?2 AND entity_id=?3",
        params![digest, KIND, review_id],
    ).unwrap();
    subscription::validate_persisted(&connection, &project).unwrap();
    assert!(connection.is_autocommit());
}

#[test]
fn funded_working_memory_outer_scope_preserves_ready_and_claimed_bytes() {
    use sha2::Digest;

    let (f, _, now) = funded_fixture();
    let call = dispatch(&f, now);
    f.store.complete_adaptive_leadership_review_call(
        &f.leader, &continued(&call, now + 2, 5), now + 2,
    ).unwrap();
    let source = session(&f);
    let digest = sha2::Sha256::digest(format!(
        "sentinel.workflow.adaptive-model-effect.v1:{}:{}",
        source.grant.session_id, source.version,
    ).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let effect_id = Uuid::from_bytes(bytes);
    let before = rows(&f.store);
    let (ready, scopes) = validation_scope::with_completed_validations(|| {
        f.store.adaptive_working_memory_source(
            source.grant.session_id, source.version, effect_id, &source.grant.authority,
        )
    });
    let ready = ready.unwrap().unwrap();
    assert_eq!(scopes.len(), 1);
    assert_eq!(scopes[0].get("adaptive-journal"), Some(&1));
    assert_eq!(scopes[0].get("funding-inventory"), Some(&1));
    assert_eq!(scopes[0].get("funding-supersession"), Some(&1));
    assert_eq!(ready.work_funding.as_deref(), source.active_work_funding());
    assert_eq!(ready.root_model_ceiling, source.grant.max_model_calls);
    assert_eq!(ready.root_tool_ceiling, source.grant.max_tool_calls);
    assert_eq!(rows(&f.store), before);
    f.store.advance_adaptive_session(
        source.grant.session_id, source.version, Uuid::new_v4(),
        &AdaptiveTransitionV1::ClaimModel {
            effect: AdaptiveEffectV1 { id: effect_id, request_digest: DIGEST.into() },
            previous_observation_digest: source.last_observation.as_ref()
                .map(|observation| observation.observation_digest.clone()),
        }, &source.grant.authority, now + 3,
    ).unwrap();
    let claimed_rows = rows(&f.store);
    let (claimed, scopes) = validation_scope::with_completed_validations(|| {
        f.store.adaptive_working_memory_source(
            source.grant.session_id, source.version, effect_id, &source.grant.authority,
        )
    });
    assert_eq!(encode(&claimed.unwrap().unwrap()).unwrap(), encode(&ready).unwrap());
    assert_eq!(scopes.len(), 1);
    assert_eq!(scopes[0].get("adaptive-journal"), Some(&1));
    assert_eq!(scopes[0].get("funding-inventory"), Some(&1));
    assert_eq!(scopes[0].get("funding-supersession"), Some(&1));
    assert_eq!(rows(&f.store), claimed_rows);
}
