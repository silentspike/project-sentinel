use super::*;
use crate::{AdaptiveWorkFundingExpiredReplacementDraftV1,
    AdaptiveWorkFundingExpiredReplacementRequestV1, AdaptiveWorkFundingExpiredReplacementEvidenceV1,
    AdaptiveWorkFundingExpiredReplacementReviewEvidenceV1,
    AdaptiveWorkFundingExpiredReplacementVerificationSourceV1};

fn expired_fixture(with_retry: bool) -> (Fixture, AdaptiveWorkFundingReceiptV1, u64) {
    if with_retry {
        let (f, request, at) = recovery_fixture();
        let (_, retry) = f.store.authorize_adaptive_funded_review_recovery(&funding_operator(&f),
            &request, &f.grant, &f.context, at, recovery_evidence).unwrap();
        let call = f.store.adaptive_leadership_review_call(&f.leader.tenant_id,
            retry.bound_grant().unwrap().review_id).unwrap().unwrap();
        f.store.expire_adaptive_leadership_review_call(&f.leader, call.grant.review_id,
            call.version, call.grant.expires_at_unix_ms).unwrap();
        let old = call.grant.work_funding.unwrap().receipt;
        let now = old.request.limits.expires_at_unix_ms + 1;
        (f, old, now)
    } else {
        let (f, old, issued) = funded_fixture();
        let call = issue(&f, issued).unwrap();
        f.store.expire_adaptive_leadership_review_call(&f.leader, call.grant.review_id,
            call.version, call.grant.expires_at_unix_ms).unwrap();
        let now = old.request.limits.expires_at_unix_ms + 1;
        (f, old, now)
    }
}

fn draft_input(f: &Fixture, old: &AdaptiveWorkFundingReceiptV1, now: u64)
    -> AdaptiveWorkFundingExpiredReplacementDraftV1
{
    AdaptiveWorkFundingExpiredReplacementDraftV1 {
        old_funding_receipt_digest: old.receipt_digest().unwrap(), operation_id: Uuid::new_v4(),
        reason_ref: "explicit-expired-reviewed-no-io-replacement".into(),
        limits: AdaptiveWorkFundingLimitsV1 {
            additional_model_calls: 1, additional_tool_calls: 1, additional_reviews: 2,
            additional_windows: 1, max_window_ms: WINDOW_MS,
            max_call_duration_ms: f.context.source_session.grant.max_call_duration_ms,
            dispatch_margin_ms: crate::ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
            expires_at_unix_ms: now + 3_600_000,
        },
        release: crate::AdaptiveRecoveryReleaseV1 { schema_version: 1,
            source_git_sha: "a".repeat(40), release_manifest_digest: "b".repeat(64),
            gateway_binary_digest: "c".repeat(64) }, proof_digest: "d".repeat(64),
    }
}

fn draft(f: &Fixture, old: &AdaptiveWorkFundingReceiptV1, now: u64)
    -> AdaptiveWorkFundingExpiredReplacementRequestV1
{
    f.store.adaptive_work_funding_expired_replacement_draft(&funding_operator(f),
        &f.grant.project_id, f.grant.session_id, &draft_input(f, old, now), now).unwrap()
}

fn evidence(source: &AdaptiveWorkFundingExpiredReplacementVerificationSourceV1,
    request: &AdaptiveWorkFundingExpiredReplacementRequestV1)
    -> Result<AdaptiveWorkFundingExpiredReplacementEvidenceV1, WorkflowError>
{
    Ok(AdaptiveWorkFundingExpiredReplacementEvidenceV1 {
        source_digest: source.source.canonical_digest()?, proof_digest: request.proof_digest.clone(),
        verified_at_unix_ms: request.funding_request.limits.expires_at_unix_ms - 3_600_000,
        per_review: source.review_members.iter().map(|member| {
            let call = source.calls.iter().find(|call| call.grant.review_id == member.review_id).unwrap();
            AdaptiveWorkFundingExpiredReplacementReviewEvidenceV1 {
                member: member.clone(), proof_digest: "e".repeat(64), release: request.release.clone(),
                no_io: crate::AdaptiveFundedReviewNoIoV1::UndispatchedAtDurableProviderGate {
                    request_id: call.request_id(),
                },
            }
        }).collect(),
    })
}

#[test]
fn expired_reviewed_funding_replacement_issues_only_normal_pending_capacity() {
    let (f, old, now) = expired_fixture(false);
    let request = draft(&f, &old, now);
    let original = session(&f);
    let old_calls = f.store.adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id).unwrap();
    let (_, replacement) = f.store.authorize_adaptive_work_funding_expired_replacement(
        &funding_operator(&f), &request, now, evidence).unwrap();
    assert_eq!(session(&f), original);
    assert_eq!(f.store.adaptive_leadership_review_calls(&f.leader.tenant_id,
        f.grant.session_id).unwrap(), old_calls);
    let new = f.store.adaptive_work_funding(&f.leader.tenant_id, f.grant.session_id,
        request.funding_request.operation_id).unwrap().unwrap();
    assert_eq!(new.receipt_digest().unwrap(), replacement.new_funding_receipt_digest);
    assert_eq!(new.resulting_model_call_ceiling().unwrap(), original.funded_model_call_ceiling() + 1);
    assert_eq!(new.resulting_tool_call_ceiling().unwrap(), original.funded_tool_call_ceiling() + 1);
    assert!(new.resulting_model_call_ceiling().unwrap() < old.resulting_model_call_ceiling().unwrap());
    assert_eq!(f.store.adaptive_work_funding(&f.leader.tenant_id, f.grant.session_id,
        old.request.operation_id).unwrap(), Some(old));
    let selected = f.store.adaptive_work_funding_for_review(&f.leader.tenant_id,
        f.grant.session_id, now).unwrap().unwrap();
    assert_eq!(selected.receipt, new);
    assert_eq!(usize::from(selected.binding.ordinal), old_calls.len() + 1);
}

#[test]
fn expired_reviewed_funding_replacement_requires_proof_for_every_schema5_and_schema6_review() {
    let (f, old, now) = expired_fixture(true);
    let request = draft(&f, &old, now);
    assert_eq!(request.source.prior_review_count, 2);
    let before = rows(&f.store);
    for defect in ["missing", "duplicate", "extra", "foreign", "bad-proof", "bad-release", "bad-source"] {
        assert!(f.store.authorize_adaptive_work_funding_expired_replacement(&funding_operator(&f),
            &request, now, |source, request| {
                let mut value = evidence(source, request)?;
                match defect {
                    "missing" => { value.per_review.pop(); }
                    "duplicate" => value.per_review[1] = value.per_review[0].clone(),
                    "extra" => value.per_review.push(value.per_review[0].clone()),
                    "foreign" => value.per_review[1].no_io = crate::AdaptiveFundedReviewNoIoV1::NeverReserved {
                        request_id: "foreign-request".into(),
                    },
                    "bad-proof" => value.per_review[1].proof_digest = "invalid".into(),
                    "bad-release" => value.per_review[1].release.schema_version = 2,
                    _ => value.source_digest = "a".repeat(64),
                }
                Ok(value)
            }).is_err(), "accepted {defect}");
        assert_eq!(rows(&f.store), before);
    }
    let (_, receipt) = f.store.authorize_adaptive_work_funding_expired_replacement(
        &funding_operator(&f), &request, now, evidence).unwrap();
    assert_eq!(receipt.review_members.len(), 2);
    assert!(receipt.evidence.per_review.iter().all(|value|
        matches!(value.no_io, crate::AdaptiveFundedReviewNoIoV1::UndispatchedAtDurableProviderGate { .. })));
}

#[test]
fn expired_reviewed_funding_replacement_rejects_current_and_historical_effects() {
    for state in ["live", "dispatch", "defer", "continue"] {
        let (f, old, issued) = funded_fixture();
        let call = if state == "live" { issue(&f, issued).unwrap() } else { dispatch(&f, issued) };
        match state {
            "defer" => { f.store.complete_adaptive_leadership_review_call(&f.leader, &defer(&call), issued + 2).unwrap(); }
            "continue" => { f.store.complete_adaptive_leadership_review_call(&f.leader,
                &continued(&call, issued + 2, 1), issued + 2).unwrap(); }
            "dispatch" => { f.store.expire_adaptive_leadership_review_call(&f.leader,
                call.grant.review_id, call.version, call.grant.expires_at_unix_ms).unwrap(); }
            _ => {}
        }
        let now = old.request.limits.expires_at_unix_ms + 1;
        let before = rows(&f.store);
        assert!(f.store.adaptive_work_funding_expired_replacement_draft(&funding_operator(&f),
            &f.grant.project_id, f.grant.session_id, &draft_input(&f, &old, now), now).is_err(), "accepted {state}");
        assert_eq!(rows(&f.store), before);
    }
    let (f, old, now) = expired_fixture(false);
    let request = draft(&f, &old, now);
    let external = Connection::open(&f.path).unwrap();
    external.execute("UPDATE company_events SET payload=x'7b7d'
        WHERE event_type='adaptive_leadership_review_authorized'", []).unwrap();
    let before = rows(&f.store);
    assert!(f.store.authorize_adaptive_work_funding_expired_replacement(&funding_operator(&f),
        &request, now, |_, _| panic!("corrupt history must precede proof")).is_err());
    assert_eq!(rows(&f.store), before);
}

#[test]
fn expired_reviewed_funding_replacement_replay_preserves_bytes_after_expiry_and_adoption() {
    let (mut f, old, now) = expired_fixture(true);
    let request = draft(&f, &old, now);
    let operator = funding_operator(&f);
    let (_, receipt) = f.store.authorize_adaptive_work_funding_expired_replacement(&operator,
        &request, now, evidence).unwrap();
    let bytes = encode(&receipt).unwrap();
    let selected = f.store.adaptive_work_funding_for_review(&operator.tenant_id,
        f.grant.session_id, now).unwrap().unwrap();
    budget_context(&mut f, now, "new-expired-replacement-review");
    bind(&mut f, &selected.receipt, selected.binding.ordinal);
    let call = dispatch(&f, now);
    let result = continued(&call, now + 2, 1);
    let completed = f.store.complete_adaptive_leadership_review_call(&f.leader, &result, now + 2).unwrap();
    let reopened = WorkflowStore::open(&f.path).unwrap();
    let after_expiry = request.funding_request.limits.expires_at_unix_ms + 1;
    let (replayed, replay) = reopened.authorize_adaptive_work_funding_expired_replacement(&operator,
        &request, after_expiry, |_, _| panic!("immutable replay must not invoke proof")).unwrap();
    assert!(replayed);
    assert_eq!(encode(&replay).unwrap(), bytes);
    assert_eq!(reopened.adaptive_work_funding_expired_replacement(&operator.tenant_id,
        &receipt.replacement_key).unwrap(), Some(receipt));
    assert_eq!(reopened.authorize_adaptive_work_funding(&operator, &old.request, after_expiry).unwrap().1, old);
    assert_eq!(reopened.authorize_adaptive_work_funding(&operator, &selected.receipt.request,
        after_expiry).unwrap().1, selected.receipt);
    assert_eq!(reopened.complete_adaptive_leadership_review_call(&f.leader, &result, after_expiry).unwrap(), completed);
    let mut changed = request.clone();
    changed.funding_request.operation_id = Uuid::new_v4();
    assert!(reopened.authorize_adaptive_work_funding_expired_replacement(&operator, &changed,
        after_expiry, evidence).is_err());
    let mut other = operator;
    other.principal_id = "other-operator".into();
    assert!(reopened.authorize_adaptive_work_funding_expired_replacement(&other, &request,
        after_expiry, evidence).is_err());
}

#[test]
fn expired_reviewed_funding_replacement_keeps_old_historical_membership_valid() {
    let (f, old, now) = expired_fixture(true);
    let request = draft(&f, &old, now);
    let before_calls = f.store.adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id).unwrap();
    let target_count = before_calls.iter().filter(|call| call.grant.work_funding.as_ref()
        .is_some_and(|epoch| epoch.receipt == old)).count();
    assert_eq!(target_count, 2);
    f.store.authorize_adaptive_work_funding_expired_replacement(&funding_operator(&f), &request, now, evidence).unwrap();
    for call in &before_calls {
        assert_eq!(f.store.adaptive_leadership_review_call(&f.leader.tenant_id,
            call.grant.review_id).unwrap().as_ref(), Some(call));
        assert!(f.store.claim_adaptive_leadership_review_call(&f.leader, &claim(call), now).is_err());
        if let Some(epoch) = &call.grant.work_funding {
            let connection = f.store.connection.lock().unwrap();
            let scope = validation_scope::enter(&connection).unwrap();
            super::super::super::super::adaptive_work_funding::require_epoch_receipt(&connection, epoch).unwrap();
            assert!(super::super::super::super::adaptive_work_funding::require_fresh_epoch_eligibility(&connection, epoch).is_err());
            scope.finish().unwrap();
        }
    }
    assert_eq!(encode(&session(&f)).unwrap(), encode(&f.context.source_session).unwrap());
}

#[test]
fn expired_reviewed_funding_replacement_consumes_predecessor_permanently() {
    let (f, old, now) = expired_fixture(false);
    let request = draft(&f, &old, now);
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for index in 0..2 {
        let store = WorkflowStore::open(&f.path).unwrap();
        let operator = funding_operator(&f);
        let mut request = request.clone();
        if index == 1 { request.funding_request.operation_id = Uuid::new_v4(); }
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            store.authorize_adaptive_work_funding_expired_replacement(&operator, &request, now, evidence)
        }));
    }
    let results: Vec<_> = handles.into_iter().map(|handle| handle.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let reopened = WorkflowStore::open(&f.path).unwrap();
    let late = now + 4_000_000;
    assert!(reopened.adaptive_work_funding_expired_replacement_draft(&funding_operator(&f),
        &f.grant.project_id, f.grant.session_id, &draft_input(&f, &old, late), late).is_err());
    assert!(supersession_draft(&f, &old, Uuid::new_v4(), late).is_err());
}

#[test]
fn expired_reviewed_funding_replacement_never_refunds_spent_or_inherits_expired_capacity() {
    let (f, old, now) = expired_fixture(false);
    let before = rows(&f.store);
    for defect in ["zero", "model-cap", "tool-cap", "review-cap", "window-cap", "overflow"] {
        let mut input = draft_input(&f, &old, now);
        match defect {
            "zero" => { input.limits.additional_model_calls = 0; input.limits.additional_tool_calls = 0; }
            "model-cap" => input.limits.additional_model_calls = 64,
            "tool-cap" => input.limits.additional_tool_calls = 64,
            "review-cap" => input.limits.additional_reviews = 128,
            "window-cap" => input.limits.additional_windows = 128,
            _ => input.limits.expires_at_unix_ms = u64::MAX,
        }
        assert!(f.store.adaptive_work_funding_expired_replacement_draft(&funding_operator(&f),
            &f.grant.project_id, f.grant.session_id, &input, now).is_err(), "accepted {defect}");
        assert_eq!(rows(&f.store), before);
    }
    assert_eq!(session(&f).model_calls, f.context.source_session.model_calls);
    assert_eq!(session(&f).tool_calls, f.context.source_session.tool_calls);
}

#[test]
fn expired_reviewed_funding_replacement_delayed_verifier_cannot_extend_expiry() {
    let (f, old, now) = expired_fixture(false);
    let request = draft(&f, &old, now);
    let before = rows(&f.store);
    for time in [now - 1, request.funding_request.limits.expires_at_unix_ms,
        request.funding_request.limits.expires_at_unix_ms - request.funding_request.limits.max_call_duration_ms] {
        assert!(f.store.authorize_adaptive_work_funding_expired_replacement(&funding_operator(&f),
            &request, now, |source, request| { let mut value = evidence(source, request)?;
                value.verified_at_unix_ms = time; Ok(value) }).is_err());
        assert_eq!(rows(&f.store), before);
    }
    assert!(f.store.authorize_adaptive_work_funding_expired_replacement(&funding_operator(&f),
        &request, now, |_, _| Err(transition())).is_err());
    let mut stale = request.clone();
    stale.source.current_resume_source.head_entry_digest = "f".repeat(64);
    stale.source_digest = stale.source.canonical_digest().unwrap();
    stale.funding_request.source.resume_source = stale.source.current_resume_source.clone();
    stale.funding_request.source.expired_reviewed_replacement = Some(stale.source.binding().unwrap());
    assert!(f.store.authorize_adaptive_work_funding_expired_replacement(&funding_operator(&f),
        &stale, now, |_, _| panic!("stale source must precede proof")).is_err());
    assert_eq!(rows(&f.store), before);
}

#[test]
fn expired_reviewed_funding_replacement_all_write_failures_roll_back_relation_and_receipt() {
    for (table, condition) in [
        ("company_entities", "NEW.entity_kind='adaptive_work_funding'"),
        ("company_entities", "NEW.entity_kind='adaptive_work_funding_expired_replacement'"),
        ("company_events", "NEW.event_type='adaptive_work_funding_issued'"),
        ("company_events", "NEW.event_type='adaptive_work_funding_expired_replacement_issued'"),
    ] {
        let (f, old, now) = expired_fixture(false);
        let request = draft(&f, &old, now);
        let external = Connection::open(&f.path).unwrap();
        external.execute_batch(&format!("CREATE TRIGGER reject_replacement BEFORE INSERT ON {table}
            WHEN {condition} BEGIN SELECT RAISE(ABORT,'reject replacement'); END;")).unwrap();
        let before = rows(&f.store);
        assert!(f.store.authorize_adaptive_work_funding_expired_replacement(&funding_operator(&f),
            &request, now, evidence).is_err());
        assert_eq!(rows(&f.store), before);
        assert_eq!(session(&f), f.context.source_session);
        assert!(f.store.adaptive_work_funding_expired_replacement(&f.leader.tenant_id,
            &request.source.replacement_key().unwrap()).unwrap().is_none());
        external.execute_batch("DROP TRIGGER reject_replacement").unwrap();
        f.store.authorize_adaptive_work_funding_expired_replacement(&funding_operator(&f), &request, now, evidence).unwrap();
    }
}

#[test]
fn expired_reviewed_funding_replacement_metadata_outlives_old_expiry_without_authority() {
    let (f, old, now) = expired_fixture(false);
    let request = draft(&f, &old, now);
    let operator = funding_operator(&f);
    let source = f.store.adaptive_work_funding_expired_replacement_source(&operator, &f.grant.project_id,
        f.grant.session_id, &old.receipt_digest().unwrap(), now).unwrap();
    let value = serde_json::to_value(&source).unwrap();
    assert!(value.get("calls").is_none() && value.get("context").is_none());
    assert!(serde_json::to_vec(&source).unwrap().len() < 16_384);
    let (_, receipt) = f.store.authorize_adaptive_work_funding_expired_replacement(&operator,
        &request, now, evidence).unwrap();
    let later = request.funding_request.limits.expires_at_unix_ms + 1;
    assert_eq!(f.store.adaptive_work_funding_expired_replacement_source(&operator, &f.grant.project_id,
        f.grant.session_id, &old.receipt_digest().unwrap(), later).unwrap(), source);
    assert!(f.store.adaptive_work_funding_expired_replacement_verification_source(&operator,
        &f.grant.project_id, f.grant.session_id, &old.receipt_digest().unwrap(), later).is_err());
    assert!(f.store.adaptive_work_funding_expired_replacement_draft(&operator, &f.grant.project_id,
        f.grant.session_id, &draft_input(&f, &old, later), later).is_err());
    let mut different = request;
    different.funding_request.operation_id = Uuid::new_v4();
    assert!(f.store.authorize_adaptive_work_funding_expired_replacement(&operator, &different, later,
        |_, _| panic!("consumed metadata cannot authorize")).is_err());
    let external = Connection::open(&f.path).unwrap();
    external.execute("UPDATE company_entities SET payload=x'7b7d' WHERE entity_kind=?1",
        ["adaptive_work_funding_expired_replacement"]).unwrap();
    assert!(f.store.adaptive_work_funding_expired_replacement(&operator.tenant_id, &receipt.replacement_key).is_err());
}

#[test]
fn expired_reviewed_funding_replacement_optional_none_preserves_old_golden_digests() {
    let (f, old, now) = expired_fixture(false);
    // Typed legacy wire projection pins field order and omission, not merely
    // equality between two values serialized by the new implementation.
    #[derive(serde::Serialize)]
    struct PriorSource<'a> {
        resume_source: &'a crate::AdaptiveResumeSourceV1,
        original_model_call_ceiling: u16,
        original_tool_call_ceiling: u16,
        current_model_call_ceiling: u16,
        current_tool_call_ceiling: u16,
        #[serde(skip_serializing_if = "Option::is_none")]
        predecessor_receipt_digest: &'a Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        supersedes_unused_receipt_digest: &'a Option<String>,
    }
    #[derive(serde::Serialize)]
    struct PriorRequest<'a> {
        schema_version: u16,
        operation_id: Uuid,
        source: PriorSource<'a>,
        limits: &'a AdaptiveWorkFundingLimitsV1,
        reason_ref: &'a str,
    }
    #[derive(serde::Serialize)]
    struct PriorReceipt<'a> {
        schema_version: u16,
        funding_id: &'a str,
        request: PriorRequest<'a>,
        issuer_principal: &'a AuthenticatedCompanyPrincipalV1,
        issued_at_unix_ms: u64,
    }
    let source = &old.request.source;
    let prior = PriorReceipt {
        schema_version: old.schema_version, funding_id: &old.funding_id,
        request: PriorRequest {
            schema_version: old.request.schema_version, operation_id: old.request.operation_id,
            source: PriorSource {
                resume_source: &source.resume_source,
                original_model_call_ceiling: source.original_model_call_ceiling,
                original_tool_call_ceiling: source.original_tool_call_ceiling,
                current_model_call_ceiling: source.current_model_call_ceiling,
                current_tool_call_ceiling: source.current_tool_call_ceiling,
                predecessor_receipt_digest: &source.predecessor_receipt_digest,
                supersedes_unused_receipt_digest: &source.supersedes_unused_receipt_digest,
            },
            limits: &old.request.limits, reason_ref: &old.request.reason_ref,
        },
        issuer_principal: &old.issuer_principal, issued_at_unix_ms: old.issued_at_unix_ms,
    };
    assert_eq!(encode(&prior).unwrap(), encode(&old).unwrap());
    assert_eq!(canonical_sha256("sentinel.workflow.adaptive-work-funding-request.v1", &prior.request).unwrap(),
        old.request.canonical_digest().unwrap());
    assert_eq!(canonical_sha256("sentinel.workflow.adaptive-work-funding-receipt.v1", &prior).unwrap(),
        old.receipt_digest().unwrap());
    assert!(!String::from_utf8(encode(&old).unwrap()).unwrap().contains("expired_reviewed_replacement"));
    let mut json = serde_json::to_value(&old).unwrap();
    json["request"]["source"]["expired_reviewed_replacement"] = serde_json::Value::Null;
    let retained: AdaptiveWorkFundingReceiptV1 = serde_json::from_value(json).unwrap();
    assert_eq!(encode(&retained).unwrap(), encode(&old).unwrap());
    assert_eq!(retained.receipt_digest().unwrap(), old.receipt_digest().unwrap());
    assert_eq!(retained.request.canonical_digest().unwrap(), old.request.canonical_digest().unwrap());
    assert_eq!(retained.binding(2).unwrap(), old.binding(2).unwrap());
    let request = draft(&f, &old, now);
    let before = rows(&f.store);
    assert!(f.store.authorize_adaptive_work_funding(&funding_operator(&f),
        &request.funding_request, now).is_err());
    assert_eq!(rows(&f.store), before);
    let mut invalid = request;
    invalid.funding_request.source.supersedes_unused_receipt_digest = Some(old.receipt_digest().unwrap());
    assert!(invalid.validate_shape().is_err());
}

#[test]
fn expired_reviewed_funding_replacement_real_continue_and_defer_use_existing_adoption() {
    for continue_work in [false, true] {
        let (mut f, old, now) = expired_fixture(false);
        let request = draft(&f, &old, now);
        let original = session(&f);
        f.store.authorize_adaptive_work_funding_expired_replacement(&funding_operator(&f), &request, now, evidence).unwrap();
        let epoch = f.store.adaptive_work_funding_for_review(&f.leader.tenant_id, f.grant.session_id, now).unwrap().unwrap();
        budget_context(&mut f, now, "ordinary-replacement-decision");
        bind(&mut f, &epoch.receipt, epoch.binding.ordinal);
        let call = dispatch(&f, now);
        let result = if continue_work { continued(&call, now + 2, 1) } else { defer(&call) };
        f.store.complete_adaptive_leadership_review_call(&f.leader, &result, now + 2).unwrap();
        assert_eq!((session(&f).model_calls, session(&f).tool_calls), (original.model_calls, original.tool_calls));
        assert_eq!(session(&f).grant, original.grant);
        if continue_work {
            assert_eq!(session(&f).active_work_funding().unwrap().receipt, epoch.receipt);
        } else {
            assert_eq!(session(&f).funded_model_call_ceiling(), original.funded_model_call_ceiling());
            assert!(f.store.adaptive_work_funding_for_review(&f.leader.tenant_id, f.grant.session_id, now + 3).unwrap().is_none());
            let later = epoch.receipt.request.limits.expires_at_unix_ms + 1;
            assert!(f.store.adaptive_work_funding_expired_replacement_draft(&funding_operator(&f), &f.grant.project_id,
                f.grant.session_id, &draft_input(&f, &epoch.receipt, later), later).is_err());
        }
    }
}

#[test]
fn expired_reviewed_funding_replacement_uses_current_adopted_capacity_at_64() {
    let (mut f, adopted, at) = capped_renewal_fixture(5);
    let operator = funding_operator(&f);
    let renewed_request = zero_call_renewal_draft(&f, &adopted, at).unwrap();
    let old = f.store.authorize_adaptive_work_funding(&operator, &renewed_request, at).unwrap().1;
    bind(&mut f, &old, old.request.source.resume_source.base_review_count + 1);
    let call = issue(&f, at).unwrap();
    f.store.expire_adaptive_leadership_review_call(&f.leader, call.grant.review_id,
        call.version, call.grant.expires_at_unix_ms).unwrap();
    let now = old.request.limits.expires_at_unix_ms + 1;
    let mut input = draft_input(&f, &old, now);
    input.limits.additional_model_calls = 0;
    input.limits.additional_tool_calls = 0;
    let request = f.store.adaptive_work_funding_expired_replacement_draft(&operator,
        &f.grant.project_id, f.grant.session_id, &input, now).unwrap();
    let original = session(&f);
    assert_eq!(request.funding_request.source.predecessor_receipt_digest, Some(adopted.receipt_digest().unwrap()));
    assert_eq!(request.funding_request.source.current_model_call_ceiling, 64);
    let (_, relation) = f.store.authorize_adaptive_work_funding_expired_replacement(&operator,
        &request, now, evidence).unwrap();
    let new = f.store.adaptive_work_funding(&operator.tenant_id, f.grant.session_id,
        request.funding_request.operation_id).unwrap().unwrap();
    assert_eq!(new.resulting_model_call_ceiling().unwrap(), 64);
    assert_eq!(new.resulting_tool_call_ceiling().unwrap(), 64);
    assert_eq!(session(&f), original);
    assert!(original.model_calls < 64 && original.tool_calls < 64);
    assert_ne!(relation.request.source.old_funding_receipt_digest, adopted.receipt_digest().unwrap());
}

#[test]
fn expired_reviewed_funding_replacement_respects_128_funding_inventory_cap() {
    let (mut f, mut old, _) = funded_fixture();
    for _ in 1..128 {
        let at = old.request.limits.expires_at_unix_ms;
        let request = supersession_draft(&f, &old, Uuid::new_v4(), at).unwrap();
        old = f.store.authorize_adaptive_work_funding(&funding_operator(&f), &request, at).unwrap().1;
    }
    let at = old.issued_at_unix_ms;
    budget_context(&mut f, at, "last-permitted-funding-proposal");
    bind(&mut f, &old, old.request.source.resume_source.base_review_count + 1);
    let call = issue(&f, at).unwrap();
    f.store.expire_adaptive_leadership_review_call(&f.leader, call.grant.review_id,
        call.version, call.grant.expires_at_unix_ms).unwrap();
    let now = old.request.limits.expires_at_unix_ms + 1;
    let before = rows(&f.store);
    assert!(f.store.adaptive_work_funding_expired_replacement_source(&funding_operator(&f),
        &f.grant.project_id, f.grant.session_id, &old.receipt_digest().unwrap(), now).is_ok());
    assert!(f.store.adaptive_work_funding_expired_replacement_draft(&funding_operator(&f),
        &f.grant.project_id, f.grant.session_id, &draft_input(&f, &old, now), now).is_err());
    assert_eq!(rows(&f.store), before);
}

#[test]
fn expired_reviewed_funding_replacement_explicit_multi_link_requires_new_proof_and_key() {
    let (mut f, old, now) = expired_fixture(false);
    let request = draft(&f, &old, now);
    let (_, first) = f.store.authorize_adaptive_work_funding_expired_replacement(&funding_operator(&f),
        &request, now, evidence).unwrap();
    let epoch = f.store.adaptive_work_funding_for_review(&f.leader.tenant_id, f.grant.session_id, now).unwrap().unwrap();
    budget_context(&mut f, now, "explicit-replacement-second-link");
    bind(&mut f, &epoch.receipt, epoch.binding.ordinal);
    let call = issue(&f, now).unwrap();
    f.store.expire_adaptive_leadership_review_call(&f.leader, call.grant.review_id,
        call.version, call.grant.expires_at_unix_ms).unwrap();
    let later = epoch.receipt.request.limits.expires_at_unix_ms + 1;
    let second_request = draft(&f, &epoch.receipt, later);
    assert_ne!(first.replacement_key, second_request.source.replacement_key().unwrap());
    let before = rows(&f.store);
    assert!(f.store.authorize_adaptive_work_funding_expired_replacement(&funding_operator(&f),
        &second_request, later, |_, _| Ok(first.evidence.clone())).is_err());
    assert_eq!(rows(&f.store), before);
    let (_, second) = f.store.authorize_adaptive_work_funding_expired_replacement(&funding_operator(&f),
        &second_request, later, evidence).unwrap();
    assert_ne!(second.replacement_key, first.replacement_key);
    assert_eq!(second.review_members.len(), 1);
    assert_eq!(second.request.funding_request.source.current_model_call_ceiling,
        session(&f).funded_model_call_ceiling());
    assert_eq!(f.store.adaptive_work_funding_expired_replacement(&f.leader.tenant_id,
        &first.replacement_key).unwrap(), Some(first));
    assert!(f.store.adaptive_work_funding_expired_replacement_draft(&funding_operator(&f),
        &f.grant.project_id, f.grant.session_id, &draft_input(&f, &old, later), later).is_err());
}

#[test]
fn expired_reviewed_funding_replacement_requires_expired_reviewed_exact_digest() {
    let (f, old, issued) = funded_fixture();
    let call = issue(&f, issued).unwrap();
    let retired = call.grant.expires_at_unix_ms;
    f.store.expire_adaptive_leadership_review_call(&f.leader, call.grant.review_id, call.version, retired).unwrap();
    assert!(f.store.adaptive_work_funding_expired_replacement_draft(&funding_operator(&f),
        &f.grant.project_id, f.grant.session_id, &draft_input(&f, &old, retired), retired).is_err());
    let now = old.request.limits.expires_at_unix_ms + 1;
    let mut foreign = draft_input(&f, &old, now);
    foreign.old_funding_receipt_digest = "f".repeat(64);
    let before = rows(&f.store);
    assert!(f.store.adaptive_work_funding_expired_replacement_draft(&funding_operator(&f),
        &f.grant.project_id, f.grant.session_id, &foreign, now).is_err());
    assert_eq!(rows(&f.store), before);
    let (unused, funding, _) = funded_fixture();
    let now = funding.request.limits.expires_at_unix_ms + 1;
    assert!(unused.store.adaptive_work_funding_expired_replacement_draft(&funding_operator(&unused),
        &unused.grant.project_id, unused.grant.session_id, &draft_input(&unused, &funding, now), now).is_err());
}

#[test]
fn expired_reviewed_funding_replacement_corrupt_links_and_unfiltered_inventory_fail_closed() {
    for damage in ["relation-row", "relation-event", "normal-row", "normal-event", "old-member", "unrelated-orphan"] {
        let (f, old, now) = expired_fixture(true);
        let request = draft(&f, &old, now);
        let (_, receipt) = f.store.authorize_adaptive_work_funding_expired_replacement(&funding_operator(&f),
            &request, now, evidence).unwrap();
        let external = Connection::open(&f.path).unwrap();
        match damage {
            "relation-row" => { external.execute("DELETE FROM company_entities WHERE entity_kind=?1",
                ["adaptive_work_funding_expired_replacement"]).unwrap(); }
            "relation-event" => { external.execute("DELETE FROM company_events WHERE event_type=?1",
                ["adaptive_work_funding_expired_replacement_issued"]).unwrap(); }
            "normal-row" => { external.execute("DELETE FROM company_entities WHERE entity_kind='adaptive_work_funding' AND entity_id=?1",
                [&receipt.new_funding_id]).unwrap(); }
            "normal-event" => { external.execute("DELETE FROM company_events WHERE event_type='adaptive_work_funding_issued' AND operation_id=?1",
                [request.funding_request.operation_id.to_string()]).unwrap(); }
            "old-member" => { external.execute("UPDATE company_entities SET payload=x'7b7d'
                WHERE entity_kind='adaptive_leadership_review_call' AND entity_id=?1",
                [receipt.review_members[0].review_id.to_string()]).unwrap(); }
            _ => { external.execute("UPDATE company_events SET operation_id=?1 WHERE event_type='adaptive_work_funding_issued'
                AND operation_id=?2", params![Uuid::new_v4().to_string(), old.request.operation_id.to_string()]).unwrap(); }
        }
        assert!(f.store.adaptive_work_funding_for_review(&f.leader.tenant_id, f.grant.session_id, now).is_err(), "accepted {damage}");
        assert!(f.store.authorize_adaptive_work_funding_expired_replacement(&funding_operator(&f),
            &request, now + 1, |_, _| panic!("corrupt replay cannot invoke proof")).is_err(), "replayed {damage}");
    }
}

#[test]
fn expired_reviewed_funding_replacement_same_request_race_replays_one_immutable_receipt() {
    let (f, old, now) = expired_fixture(false);
    let request = draft(&f, &old, now);
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2).map(|_| {
        let store = WorkflowStore::open(&f.path).unwrap();
        let operator = funding_operator(&f);
        let request = request.clone();
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || { barrier.wait();
            store.authorize_adaptive_work_funding_expired_replacement(&operator, &request, now, evidence).unwrap()
        })
    }).collect();
    let results: Vec<_> = handles.into_iter().map(|handle| handle.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|result| !result.0).count(), 1);
    assert_eq!(encode(&results[0].1).unwrap(), encode(&results[1].1).unwrap());
}

#[test]
fn expired_reviewed_funding_replacement_unrelated_schema6_missing_dedicated_membership_denies_lookup_and_replay() {
    for damage in ["receipt", "event"] {
        let (mut f, old, now) = expired_fixture(false);
        let request = draft(&f, &old, now);
        let operator = funding_operator(&f);
        let (_, replacement) = f.store.authorize_adaptive_work_funding_expired_replacement(
            &operator, &request, now, evidence).unwrap();
        let epoch = f.store.adaptive_work_funding_for_review(&operator.tenant_id,
            f.grant.session_id, now).unwrap().unwrap();
        budget_context(&mut f, now, "unrelated-successor-review");
        bind(&mut f, &epoch.receipt, epoch.binding.ordinal);
        let call = issue(&f, now).unwrap();
        let at = call.grant.expires_at_unix_ms;
        f.store.expire_adaptive_leadership_review_call(&f.leader, call.grant.review_id, call.version, at).unwrap();
        let source = f.store.adaptive_funded_review_recovery_source(&operator, &f.grant.project_id,
            f.grant.session_id, call.grant.review_id, at).unwrap();
        budget_context(&mut f, at, "unrelated-schema6-successor");
        bind(&mut f, &epoch.receipt, source.ordinal);
        f.grant.expires_at_unix_ms = at + WINDOW_MS;
        let retry_request = crate::AdaptiveFundedReviewRecoveryRequestV1 {
            schema_version: 1, operation_id: Uuid::new_v4(), source_digest: source.canonical_digest().unwrap(),
            source: Box::new(source), release: request.release.clone(), proof_digest: "f".repeat(64),
            reason_ref: "exact-successor-no-io-recovery".into(), expires_at_unix_ms: f.grant.expires_at_unix_ms,
        };
        let (_, retry) = f.store.authorize_adaptive_funded_review_recovery(&operator,
            &retry_request, &f.grant, &f.context, at, recovery_evidence).unwrap();
        assert_ne!(retry.request.source.funding_receipt_digest, old.receipt_digest().unwrap());
        assert_eq!(f.store.adaptive_work_funding_expired_replacement(&operator.tenant_id,
            &replacement.replacement_key).unwrap(), Some(replacement.clone()));
        let external = Connection::open(&f.path).unwrap();
        if damage == "receipt" {
            external.execute("DELETE FROM company_entities WHERE entity_kind='adaptive_funded_review_recovery'
                AND entity_id=?1", [&retry.receipt_key]).unwrap();
        } else {
            external.execute("DELETE FROM company_events WHERE event_type='adaptive_funded_review_recovery_issued'
                AND operation_id=?1", [retry.request.operation_id.to_string()]).unwrap();
        }
        let ordinary_count: i64 = external.query_row("SELECT count(*) FROM company_entities WHERE entity_kind
            IN ('adaptive_leadership_review_call','adaptive_work_funding_review')", [], |row| row.get(0)).unwrap();
        assert!(ordinary_count >= 6);
        let before = rows(&f.store);
        assert!(f.store.adaptive_work_funding_expired_replacement(&operator.tenant_id,
            &replacement.replacement_key).is_err(), "lookup accepted missing {damage}");
        assert!(f.store.authorize_adaptive_work_funding_expired_replacement(&operator,
            &request, at, |_, _| panic!("damaged immutable replay cannot invoke proof")).is_err(),
            "replay accepted missing {damage}");
        assert_eq!(rows(&f.store), before);
    }
}

fn assert_missing_successor_publication_denied(delete_call: bool) {
    let (mut f, old, now) = expired_fixture(false);
    let request = draft(&f, &old, now);
    let operator = funding_operator(&f);
    let (_, replacement) = f.store.authorize_adaptive_work_funding_expired_replacement(
        &operator, &request, now, evidence).unwrap();
    let epoch = f.store.adaptive_work_funding_for_review(&operator.tenant_id,
        f.grant.session_id, now).unwrap().unwrap();
    budget_context(&mut f, now, "ordinary-successor-completeness");
    bind(&mut f, &epoch.receipt, epoch.binding.ordinal);
    let call = issue(&f, now).unwrap();
    assert_eq!(call.grant.schema_version, 5);
    assert_eq!(f.store.adaptive_work_funding_expired_replacement(&operator.tenant_id,
        &replacement.replacement_key).unwrap(), Some(replacement.clone()));
    assert!(f.store.authorize_adaptive_work_funding_expired_replacement(&operator,
        &request, now, |_, _| panic!("healthy immutable replay must not invoke proof")).unwrap().0);
    let membership_id = format!("{}-review-{:03}", epoch.binding.funding_id, epoch.binding.ordinal);
    let external = Connection::open(&f.path).unwrap();
    if delete_call {
        external.execute("DELETE FROM company_entities WHERE entity_kind='adaptive_leadership_review_call'
            AND entity_id=?1", [call.grant.review_id.to_string()]).unwrap();
    } else {
        external.execute("DELETE FROM company_entities WHERE entity_kind='adaptive_work_funding_review'
            AND entity_id=?1", [&membership_id]).unwrap();
    }
    let retained: i64 = if delete_call {
        external.query_row("SELECT count(*) FROM company_entities WHERE entity_kind='adaptive_work_funding_review'
            AND entity_id=?1", [&membership_id], |row| row.get(0)).unwrap()
    } else {
        external.query_row("SELECT count(*) FROM company_entities WHERE entity_kind='adaptive_leadership_review_call'
            AND entity_id=?1", [call.grant.review_id.to_string()], |row| row.get(0)).unwrap()
    };
    assert_eq!(retained, 1);
    let event_count: i64 = external.query_row("SELECT count(*) FROM company_events WHERE operation_id=?1
        AND event_type IN ('adaptive_work_funding_review_issued','adaptive_leadership_review_authorized')",
        [call.operation_id.to_string()], |row| row.get(0)).unwrap();
    assert_eq!(event_count, 2);
    let before = rows(&f.store);
    assert!(f.store.adaptive_work_funding_expired_replacement(&operator.tenant_id,
        &replacement.replacement_key).is_err());
    assert!(f.store.authorize_adaptive_work_funding_expired_replacement(&operator,
        &request, now + 1, |_, _| panic!("incomplete publication must deny before proof")).is_err());
    assert_eq!(rows(&f.store), before);
}

#[test]
fn expired_reviewed_funding_replacement_deleted_successor_call_denies_lookup_and_replay() {
    assert_missing_successor_publication_denied(true);
}

#[test]
fn expired_reviewed_funding_replacement_deleted_successor_review_leaf_denies_lookup_and_replay() {
    assert_missing_successor_publication_denied(false);
}

#[test]
fn expired_reviewed_funding_replacement_schema6_publication_error_and_unwind_clear_exception() {
    let (f, _, _) = expired_fixture(true);
    let call = f.store.adaptive_leadership_review_calls(&f.leader.tenant_id,
        f.grant.session_id).unwrap().into_iter().find(|call| call.grant.schema_version == 6).unwrap();
    let context_digest = call.context_digest().unwrap();
    let mut connection = f.store.connection.lock().unwrap();
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate).unwrap();
    assert!(super::super::super::super::adaptive_work_funding::with_recovery_publication(
        &transaction, &call.grant, &context_digest, call.operation_id, call.grant_issued_at_unix_ms,
        || Err::<(), _>(transition())).is_err());
    super::super::super::super::adaptive_work_funding::with_recovery_publication(
        &transaction, &call.grant, &context_digest, call.operation_id, call.grant_issued_at_unix_ms,
        || Ok(())).unwrap();
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _: Result<(), WorkflowError> = super::super::super::super::adaptive_work_funding::with_recovery_publication(
            &transaction, &call.grant, &context_digest, call.operation_id, call.grant_issued_at_unix_ms,
            || panic!("controlled publication unwind"));
    }));
    assert!(unwound.is_err());
    super::super::super::super::adaptive_work_funding::with_recovery_publication(
        &transaction, &call.grant, &context_digest, call.operation_id, call.grant_issued_at_unix_ms,
        || Ok(())).unwrap();
    transaction.rollback().unwrap();
}

#[test]
fn expired_reviewed_funding_replacement_completion_replays_adoption_without_recursive_current_journal() {
    for with_retry in [false, true] {
        let (mut f, old, now) = expired_fixture(with_retry);
        let request = draft(&f, &old, now);
        let operator = funding_operator(&f);
        let original = session(&f);
        let old_calls = f.store.adaptive_leadership_review_calls(&operator.tenant_id, f.grant.session_id).unwrap();
        let (_, replacement) = f.store.authorize_adaptive_work_funding_expired_replacement(
            &operator, &request, now, evidence).unwrap();
        let epoch = f.store.adaptive_work_funding_for_review(&operator.tenant_id,
            f.grant.session_id, now).unwrap().unwrap();
        budget_context(&mut f, now, "completion-cycle-regression");
        bind(&mut f, &epoch.receipt, epoch.binding.ordinal);
        let call = dispatch(&f, now);
        let result = continued(&call, now + 2, 1);
        let completed = f.store.complete_adaptive_leadership_review_call(&f.leader, &result, now + 2).unwrap();
        let adopted = session(&f);
        assert_eq!((adopted.model_calls, adopted.tool_calls), (original.model_calls, original.tool_calls));
        assert_eq!(adopted.grant, original.grant);
        assert_eq!(adopted.active_work_funding().unwrap().receipt, epoch.receipt);
        let reopened = WorkflowStore::open(&f.path).unwrap();
        assert_eq!(reopened.adaptive_session(f.grant.session_id, &f.grant.assignee_authority).unwrap().unwrap(), adopted);
        assert_eq!(reopened.adaptive_work_funding_expired_replacement(&operator.tenant_id,
            &replacement.replacement_key).unwrap(), Some(replacement.clone()));
        for old_call in old_calls {
            assert_eq!(reopened.adaptive_leadership_review_call(&operator.tenant_id,
                old_call.grant.review_id).unwrap(), Some(old_call));
        }
        let after_expiry = request.funding_request.limits.expires_at_unix_ms + 1;
        assert_eq!(reopened.complete_adaptive_leadership_review_call(&f.leader, &result,
            after_expiry).unwrap(), completed);
        assert_eq!(reopened.authorize_adaptive_work_funding_expired_replacement(&operator,
            &request, after_expiry, |_, _| panic!("adopted immutable replay cannot reverify")).unwrap(),
            (true, replacement));
    }
}

#[test]
fn expired_reviewed_funding_replacement_historical_prefix_corruption_denies_after_adoption() {
    let (mut f, old, now) = expired_fixture(true);
    let request = draft(&f, &old, now);
    let operator = funding_operator(&f);
    assert!(request.source.current_resume_source.expected_session_version > 2);
    let (_, replacement) = f.store.authorize_adaptive_work_funding_expired_replacement(
        &operator, &request, now, evidence).unwrap();
    let epoch = f.store.adaptive_work_funding_for_review(&operator.tenant_id,
        f.grant.session_id, now).unwrap().unwrap();
    budget_context(&mut f, now, "prefix-corruption-regression");
    bind(&mut f, &epoch.receipt, epoch.binding.ordinal);
    let call = dispatch(&f, now);
    f.store.complete_adaptive_leadership_review_call(&f.leader, &continued(&call, now + 2, 1), now + 2).unwrap();
    assert!(f.store.adaptive_work_funding_expired_replacement(&operator.tenant_id,
        &replacement.replacement_key).is_ok());
    let external = Connection::open(&f.path).unwrap();
    assert_eq!(external.execute("UPDATE workflow_operations SET request_digest=?1
        WHERE operation_namespace=?2 AND operation_id=?3",
        params!["f".repeat(64), format!("adaptive-session-v1:{}", f.grant.session_id), format!("{:020}", 2)]).unwrap(), 1);
    let before = rows(&f.store);
    assert!(f.store.adaptive_work_funding_expired_replacement(&operator.tenant_id,
        &replacement.replacement_key).is_err());
    assert!(f.store.authorize_adaptive_work_funding_expired_replacement(&operator,
        &request, now + 3, |_, _| panic!("corrupt prefix must deny before proof")).is_err());
    assert_eq!(rows(&f.store), before);
}

#[test]
fn expired_reviewed_funding_replacement_completion_failure_rolls_back_adoption_and_journal() {
    let (mut f, old, now) = expired_fixture(true);
    let request = draft(&f, &old, now);
    let operator = funding_operator(&f);
    let (_, replacement) = f.store.authorize_adaptive_work_funding_expired_replacement(
        &operator, &request, now, evidence).unwrap();
    let epoch = f.store.adaptive_work_funding_for_review(&operator.tenant_id,
        f.grant.session_id, now).unwrap().unwrap();
    budget_context(&mut f, now, "completion-rollback-regression");
    bind(&mut f, &epoch.receipt, epoch.binding.ordinal);
    let call = dispatch(&f, now);
    let result = continued(&call, now + 2, 1);
    let before = rows(&f.store);
    let original = session(&f);
    let external = Connection::open(&f.path).unwrap();
    external.execute_batch("CREATE TRIGGER reject_replacement_completion BEFORE INSERT ON company_events
        WHEN NEW.event_type='adaptive_leadership_review_completed'
        BEGIN SELECT RAISE(ABORT,'reject completion'); END;").unwrap();
    assert!(f.store.complete_adaptive_leadership_review_call(&f.leader, &result, now + 2).is_err());
    assert_eq!(rows(&f.store), before);
    assert_eq!(session(&f), original);
    assert_eq!(f.store.adaptive_work_funding_expired_replacement(&operator.tenant_id,
        &replacement.replacement_key).unwrap(), Some(replacement));
    external.execute_batch("DROP TRIGGER reject_replacement_completion").unwrap();
    f.store.complete_adaptive_leadership_review_call(&f.leader, &result, now + 2).unwrap();
    assert_eq!(session(&f).active_work_funding().unwrap().receipt, epoch.receipt);
}
