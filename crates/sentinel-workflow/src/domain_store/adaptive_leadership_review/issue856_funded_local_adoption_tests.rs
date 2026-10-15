use super::*;
use crate::{
    AdaptiveLeadershipLocalAdoptionFundingBindingV2, AdaptiveLeadershipLocalAdoptionRequestV1,
    AdaptiveLeadershipLocalAdoptionV1,
};

const SAVED_CALLS: u16 = 8;
const LOCAL_WINDOW: u64 = 300_000;

fn saved_result(
    call: &AdaptiveLeadershipReviewCallV1,
    issued: u64,
) -> CompleteAdaptiveLeadershipReviewCallV1 {
    let mut result = continued(call, issued, SAVED_CALLS);
    let AdaptiveLeadershipReviewDecisionKindV1::Continue { window_ms, .. } =
        &mut result.decision.decision else { panic!("Continue"); };
    *window_ms = LOCAL_WINDOW;
    let resolution = adaptive_leadership_continuation_audit_id(
        call.grant.review_id, &result.request_digest, &result.model_response_digest, &result.decision,
    ).unwrap();
    let allowance = call.continuation_allowance(issued, issued + LOCAL_WINDOW, SAVED_CALLS).unwrap();
    result.resolution_event_id = Some(resolution);
    let authorization = result.continuation.as_mut().unwrap();
    authorization.resolution_event_id = resolution;
    authorization.deadline_ms = issued + LOCAL_WINDOW;
    authorization.provider_authority_digest = adaptive_leadership_continuation_provider_authority_digest(
        &allowance, &call.grant.assignee_authority,
    ).unwrap();
    result
}

fn local_fixture() -> (
    Fixture,
    AdaptiveLeadershipReviewCallV1,
    AdaptiveLeadershipLocalAdoptionRequestV1,
    u64,
) {
    let mut f = budget_fixture(2);
    let now = CONTINUATION_AT + 7;
    let operator = funding_operator(&f);
    let draft = f.store.adaptive_work_funding_draft(
        &operator, &f.grant.project_id, f.grant.session_id, Uuid::new_v4(),
        "funded-local-adoption-fixture", crate::AdaptiveWorkFundingLimitsV1 {
            additional_model_calls: SAVED_CALLS,
            additional_tool_calls: SAVED_CALLS,
            additional_reviews: 3,
            additional_windows: 3,
            max_window_ms: LOCAL_WINDOW,
            max_call_duration_ms: f.context.source_session.grant.max_call_duration_ms,
            dispatch_margin_ms: crate::ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
            expires_at_unix_ms: now + 3_600_000,
        }, now,
    ).unwrap();
    let receipt = f.store.authorize_adaptive_work_funding(&operator, &draft, now).unwrap().1;
    bind(&mut f, &receipt, receipt.request.source.resume_source.base_review_count + 1);
    let call = dispatch(&f, now);
    let issued = call.grant.expires_at_unix_ms + 1;
    let result = saved_result(&call, issued);
    let request = AdaptiveLeadershipLocalAdoptionRequestV1 {
        schema_version: 2,
        operation_id: Uuid::new_v4(),
        tenant_id: f.leader.tenant_id.clone(),
        project_id: call.grant.project_id.clone(),
        work_item_id: call.grant.work_item_id.clone(),
        session_id: call.grant.session_id,
        review_id: call.grant.review_id,
        epoch_digest: None,
        funded_binding: Some(AdaptiveLeadershipLocalAdoptionFundingBindingV2::from_epoch(
            call.grant.work_funding.as_deref().unwrap(),
        ).unwrap()),
        original_call_digest: crate::adaptive_leadership_local_adoption_source_call_digest(&call)
            .unwrap(),
        project_digest: crate::adaptive_leadership_recovery_project_digest(
            &call.context.source_project,
        ).unwrap(),
        session_digest: crate::adaptive_leadership_recovery_session_digest(
            &call.context.source_session,
        ).unwrap(),
        session_head_digest: f.store.adaptive_session_head_digest(
            call.grant.session_id, &call.grant.assignee_authority,
        ).unwrap().unwrap(),
        request_id: call.request_id(),
        request_digest: call.dispatch.as_ref().unwrap().request_digest.clone(),
        context_digest: call.context_digest().unwrap(),
        payload_digest: "a".repeat(64),
        model_response_digest: result.model_response_digest,
        usage_event_digest: "b".repeat(64),
        original_completion_error: "work funding current reviews unavailable".into(),
        completion_attempts: 5,
        release: crate::AdaptiveRecoveryReleaseV1 {
            schema_version: 1,
            source_git_sha: "c".repeat(40),
            release_manifest_digest: "d".repeat(64),
            gateway_binary_digest: "e".repeat(64),
        },
        repair_digest: "f".repeat(64),
        decision: result.decision,
        expires_at_unix_ms: issued + 1_000,
    };
    (f, call, request, issued)
}

fn authority(
    f: &Fixture,
    request: &AdaptiveLeadershipLocalAdoptionRequestV1,
    issued: u64,
) -> AdaptiveLeadershipLocalAdoptionV1 {
    let operator = funding_operator(f);
    AdaptiveLeadershipLocalAdoptionV1 {
        request: request.clone(),
        issuer_authority: PrincipalAuthorityV1 {
            schema_version: 1,
            principal_id: operator.principal_id.clone(),
            principal_generation: operator.authority_generation,
            authority_digest: operator.authority_digest.clone(),
        },
        issuer_principal: operator,
        issued_at_unix_ms: issued,
        continuation_deadline_ms: issued + LOCAL_WINDOW,
        adoption_key: request.key().unwrap(),
    }
}

fn local_result(
    call: &AdaptiveLeadershipReviewCallV1,
    record: &AdaptiveLeadershipLocalAdoptionV1,
) -> CompleteAdaptiveLeadershipReviewCallV1 {
    let mut result = saved_result(call, record.issued_at_unix_ms);
    result.continuation.as_mut().unwrap().local_adoption = Some(Box::new(record.clone()));
    result
}

fn sealed_audit(
    call: &AdaptiveLeadershipReviewCallV1,
    result: &CompleteAdaptiveLeadershipReviewCallV1,
    appended_at: u64,
) -> sentinel_common::EventEnvelopeV2 {
    let proposal = WorkflowStore::local_adoption_continuation_audit_proposal(call, result).unwrap();
    // Fixture-only append positions and seals are not runtime durability evidence.
    let mut event = sentinel_common::EventEnvelopeV2 {
        event_id: proposal.requested_event_id.clone().unwrap(),
        event_truth_generation: 1,
        stream_namespace: proposal.causal_context.authority_scope_digest().unwrap(),
        stream_revision: 1,
        global_position: 1,
        event_type: proposal.event_type.clone(),
        schema_version: proposal.schema_version,
        payload_codec: proposal.payload_codec,
        payload_digest: proposal.payload_digest.clone(),
        payload: proposal.payload.clone(),
        causal_context: proposal.causal_context.clone(),
        producer: proposal.producer.clone(),
        owner_term: proposal.owner_term.clone(),
        tick: proposal.tick,
        appended_at_ms: i64::try_from(appended_at).unwrap(),
        durability: proposal.requested_durability,
        canonical_request_digest: proposal.canonical_request_digest().unwrap(),
        append_receipt_digest: String::new(),
        sealed_envelope_digest: String::new(),
    };
    event.append_receipt_digest = event.expected_append_receipt_digest().unwrap();
    event.sealed_envelope_digest = event.expected_sealed_envelope_digest().unwrap();
    event.validate_seals().unwrap();
    event
}

#[test]
fn funded_binding_constructor_covers_receipt_and_complete_ordinal_binding() {
    let (_, call, request, _) = local_fixture();
    let epoch = call.grant.work_funding.as_ref().unwrap();
    let binding = request.funded_binding.unwrap();
    binding.validate_epoch(epoch).unwrap();
    assert_eq!(binding.funding_id, epoch.receipt.funding_id);
    assert_eq!(binding.receipt_digest, epoch.receipt.receipt_digest().unwrap());
    assert_eq!(binding.binding_digest, canonical_sha256(
        "sentinel.workflow.leadership-local-adoption-funding-binding.v2",
        &epoch.binding,
    ).unwrap());
    let mut other = epoch.clone();
    other.binding.ordinal += 1;
    other.validate().unwrap();
    assert!(binding.validate_epoch(&other).is_err());
}

#[test]
fn funded_continuation_requires_explicit_v2_and_exact_epoch_attachment() {
    let (f, call, request, issued) = local_fixture();
    let record = authority(&f, &request, issued);
    let original = local_result(&call, &record).continuation.unwrap();
    original.validate().unwrap();
    for target in ["missing", "wrong-binding", "legacy", "changed-window"] {
        let mut changed = original.clone();
        match target {
            "missing" => changed.work_funding = None,
            "wrong-binding" => changed.local_adoption.as_mut().unwrap().request
                .funded_binding.as_mut().unwrap().binding_digest = "0".repeat(64),
            "legacy" => {
                let adoption = changed.local_adoption.as_mut().unwrap();
                adoption.request.schema_version = 1;
                adoption.request.epoch_digest = Some("a".repeat(64));
                adoption.request.funded_binding = None;
                adoption.request.original_completion_error = "continuation audit invalid".into();
                adoption.validate().unwrap();
            }
            _ => changed.deadline_ms += 1,
        }
        assert!(changed.validate().is_err(), "{target}");
    }
}

#[test]
fn funded_local_authority_is_required_for_late_review_adoption_and_survives_journal_reopen() {
    let (f, call, request, issued) = local_fixture();
    let record = authority(&f, &request, issued);
    record.validate_funded_against(&call).unwrap();
    let result = local_result(&call, &record);
    let before = rows(&f.store);
    assert!(f.store.complete_adaptive_leadership_review_call(&f.leader, &result, issued).is_err());
    assert_eq!(rows(&f.store), before);
    let (replayed, stored) = f.store.authorize_adaptive_leadership_local_adoption(
        &funding_operator(&f), &request, issued,
    ).unwrap();
    assert!(!replayed);
    assert_eq!(stored, record);
    let proposal = WorkflowStore::local_adoption_continuation_audit_proposal(&call, &result).unwrap();
    let audit: serde_json::Value = serde_json::from_slice(&proposal.payload).unwrap();
    assert_eq!(audit["schema_version"], 3);
    assert_eq!(proposal.schema_version, 1);
    let completed = f.store.complete_adaptive_leadership_review_call(&f.leader, &result, issued)
        .unwrap();
    assert_eq!(completed.decision.as_ref(), Some(&request.decision));
    assert_eq!(completed.model_response_digest.as_ref(), Some(&request.model_response_digest));
    let after = session(&f);
    assert_eq!(after.version, call.context.source_session.version + 1);
    assert_eq!(after.model_calls, call.context.source_session.model_calls);
    assert_eq!(after.tool_calls, call.context.source_session.tool_calls);
    assert_eq!(after.active_work_funding(), call.grant.work_funding.as_deref());
    let committed = rows(&f.store);
    assert_eq!(f.store.complete_adaptive_leadership_review_call(
        &f.leader, &result, record.continuation_deadline_ms + 1,
    ).unwrap(), completed);
    assert_eq!(f.store.authorize_adaptive_leadership_local_adoption(
        &funding_operator(&f), &request, record.continuation_deadline_ms + 1,
    ).unwrap(), (true, record));
    assert_eq!(rows(&f.store), committed);
    let reopened = WorkflowStore::open(&f.path).unwrap();
    assert_eq!(reopened.adaptive_session(call.grant.session_id, &call.grant.assignee_authority)
        .unwrap(), Some(after));
    f.store.connection.lock().unwrap().execute(
        "DELETE FROM company_entities WHERE entity_kind='adaptive_leadership_local_adoption'", [],
    ).unwrap();
    assert!(reopened.adaptive_session(call.grant.session_id, &call.grant.assignee_authority).is_err());
}

#[test]
fn funded_local_request_rejects_changed_source_head_and_funding_provenance_without_writes() {
    for target in ["head", "context", "binding", "error", "attempts", "event"] {
        let (f, _, mut request, issued) = local_fixture();
        match target {
            "head" => request.session_head_digest = "0".repeat(64),
            "context" => request.context_digest = "0".repeat(64),
            "binding" => request.funded_binding.as_mut().unwrap().binding_digest = "0".repeat(64),
            "error" => request.original_completion_error = "continuation audit invalid".into(),
            "attempts" => request.completion_attempts = 4,
            _ => {
                f.store.connection.lock().unwrap().execute(
                    "DELETE FROM company_events WHERE event_type='adaptive_work_funding_issued'", [],
                ).unwrap();
            }
        }
        let before = rows(&f.store);
        assert!(f.store.authorize_adaptive_leadership_local_adoption(
            &funding_operator(&f), &request, issued,
        ).is_err(), "{target}");
        assert_eq!(rows(&f.store), before, "{target}");
    }
}

#[test]
fn funded_local_limits_keep_exact_decision_and_original_funding_expiry() {
    let (f, call, request, issued) = local_fixture();
    let record = authority(&f, &request, issued);
    for target in ["expiry", "calls", "window", "schema", "missing-funding"] {
        let mut changed = record.clone();
        let mut review = call.clone();
        match target {
            "expiry" => {
                let expires = call.grant.work_funding.as_ref().unwrap().binding.limits.expires_at_unix_ms;
                changed.issued_at_unix_ms = expires - LOCAL_WINDOW + 1;
                changed.continuation_deadline_ms = expires + 1;
                changed.request.expires_at_unix_ms = changed.issued_at_unix_ms + 1_000;
            }
            "calls" => {
                let AdaptiveLeadershipReviewDecisionKindV1::Continue { additional_model_calls, .. } =
                    &mut changed.request.decision.decision else { panic!("Continue"); };
                *additional_model_calls = 64;
            }
            "window" => changed.continuation_deadline_ms += 1,
            "schema" => review.grant.schema_version = 6,
            _ => review.grant.work_funding = None,
        }
        assert!(changed.validate_funded_against(&review).is_err(), "{target}");
    }
    let expires = call.grant.work_funding.as_ref().unwrap().binding.limits.expires_at_unix_ms;
    let mut boundary = record;
    boundary.issued_at_unix_ms = expires - LOCAL_WINDOW;
    boundary.continuation_deadline_ms = expires;
    boundary.request.expires_at_unix_ms = boundary.issued_at_unix_ms + 1_000;
    boundary.validate_funded_against(&call).unwrap();
}

#[test]
fn funded_local_replay_conflicts_do_not_reauthorize_or_renew_expired_admission() {
    let (f, _, request, issued) = local_fixture();
    let operator = funding_operator(&f);
    let (_, record) = f.store.authorize_adaptive_leadership_local_adoption(
        &operator, &request, issued,
    ).unwrap();
    let before = rows(&f.store);
    let later = record.continuation_deadline_ms + 1;
    assert_eq!(f.store.authorize_adaptive_leadership_local_adoption(
        &operator, &request, later,
    ).unwrap(), (true, record));
    let mut changed = request.clone();
    changed.expires_at_unix_ms += 1;
    assert_eq!(f.store.authorize_adaptive_leadership_local_adoption(
        &operator, &changed, later,
    ).unwrap_err().code, WorkflowErrorCode::IdempotencyConflict);
    let mut changed_issuer = operator;
    changed_issuer.role = if changed_issuer.role == CompanyRoleV1::ProjectManager {
        CompanyRoleV1::TechnicalLead
    } else { CompanyRoleV1::ProjectManager };
    assert_eq!(f.store.authorize_adaptive_leadership_local_adoption(
        &changed_issuer, &request, later,
    ).unwrap_err().code, WorkflowErrorCode::IdempotencyConflict);
    assert_eq!(rows(&f.store), before);
}

#[test]
fn funded_local_schema3_audit_can_finish_after_admission_but_not_fixed_deadline() {
    let (f, call, request, issued) = local_fixture();
    let (_, record) = f.store.authorize_adaptive_leadership_local_adoption(
        &funding_operator(&f), &request, issued,
    ).unwrap();
    let result = local_result(&call, &record);
    let audit = sealed_audit(&call, &result, issued + 1);
    let later = request.expires_at_unix_ms + 1;
    let before = rows(&f.store);
    assert!(f.store.complete_adaptive_leadership_review_call(&f.leader, &result, later).is_err());
    assert_eq!(rows(&f.store), before);
    for event in [
        sealed_audit(&call, &result, request.expires_at_unix_ms),
        sealed_audit(&call, &result, issued - 1),
    ] {
        assert!(f.store.complete_adaptive_leadership_review_call_with_local_adoption_audit(
            &f.leader, &result, &event, later,
        ).is_err());
        assert_eq!(rows(&f.store), before);
    }
    let mut wrong_schema = audit.clone();
    let mut payload: serde_json::Value = serde_json::from_slice(&wrong_schema.payload).unwrap();
    payload["schema_version"] = serde_json::json!(2);
    wrong_schema.payload = sentinel_common::canonical_json(&payload).unwrap();
    wrong_schema.payload_digest = sentinel_common::sha256_hex(&wrong_schema.payload);
    wrong_schema.append_receipt_digest = wrong_schema.expected_append_receipt_digest().unwrap();
    wrong_schema.sealed_envelope_digest = wrong_schema.expected_sealed_envelope_digest().unwrap();
    assert!(f.store.complete_adaptive_leadership_review_call_with_local_adoption_audit(
        &f.leader, &result, &wrong_schema, later,
    ).is_err());
    assert_eq!(rows(&f.store), before);
    assert!(f.store.complete_adaptive_leadership_review_call_with_local_adoption_audit(
        &f.leader, &result, &audit, record.continuation_deadline_ms,
    ).is_err());
    assert_eq!(rows(&f.store), before);
    let completed = f.store.complete_adaptive_leadership_review_call_with_local_adoption_audit(
        &f.leader, &result, &audit, later,
    ).unwrap();
    assert_eq!(completed.continuation.as_ref().unwrap().deadline_ms, record.continuation_deadline_ms);
    assert_eq!(f.store.complete_adaptive_leadership_review_call_with_local_adoption_audit(
        &f.leader, &result, &audit, record.continuation_deadline_ms + 1,
    ).unwrap(), completed);
}

#[test]
fn funded_local_sealed_adoption_inventory_rejects_missing_authority_provenance() {
    let (f, call, request, issued) = local_fixture();
    let (_, record) = f.store.authorize_adaptive_leadership_local_adoption(
        &funding_operator(&f), &request, issued,
    ).unwrap();
    let result = local_result(&call, &record);
    f.store.complete_adaptive_leadership_review_call(&f.leader, &result, issued).unwrap();
    let connection = f.store.connection.lock().unwrap();
    connection.execute(
        "DELETE FROM company_events WHERE event_type='adaptive_leadership_local_adoption_authorized'",
        [],
    ).unwrap();
    assert_eq!(crate::domain_store::adaptive_work_funding::require_pre_recovery_funding_history(
        &connection, call.grant.work_funding.as_deref().unwrap(), None,
    ).unwrap_err().code, WorkflowErrorCode::CorruptStore);
}
