use super::*;
use crate::{
    adaptive_leadership_recovery_history_digest, adaptive_leadership_recovery_project_digest,
    adaptive_leadership_recovery_session_digest,
    AdaptiveLeadershipRecoveryBlockedSubjectV1, AdaptiveLeadershipRecoveryEpochV1,
    AdaptiveLeadershipRecoveryRequestV1,
    AdaptiveLeadershipLocalAdoptionRequestV1, AdaptiveLeadershipLocalAdoptionV1,
    AdaptiveRecoveryReleaseV1,
};
use crate::{
    ExecutionPlanV1, ExecutionReconcileState, ExecutionResourceBoundsV1, ExecutionStepV1,
    ExecutionToolV1, GateExpectationV1, OutputExpectationV1, WorkTransitionReceiptV1,
};

fn recovery_source() -> (Fixture, Vec<AdaptiveLeadershipReviewCallV1>, u64) {
    recovery_source_with_subject(true)
}

fn recovery_source_with_subject(unknown: bool) -> (Fixture, Vec<AdaptiveLeadershipReviewCallV1>, u64) {
    recovery_source_from_fixture(continuation_fixture(unknown, false))
}

fn recovery_source_from_fixture(mut f: Fixture) -> (Fixture, Vec<AdaptiveLeadershipReviewCallV1>, u64) {
    let source = f.context.source_session.clone();
    let project = f.context.source_project.clone();
    let mut retired = Vec::new();
    let mut now = CONTINUATION_AT;
    for index in 0..ADAPTIVE_LEADERSHIP_MAX_REVIEWS {
        f.grant.expires_at_unix_ms = now + 1_000;
        refresh_recovery_review(&mut f);
        let call = f.store.authorize_adaptive_leadership_review_call(
            &f.leader,
            Uuid::new_v4(),
            &format!("recovery-exhausted-{index}"),
            &f.grant,
            &f.context,
            now,
        ).unwrap();
        let dispatched = f.store.claim_adaptive_leadership_review_call(
            &f.leader, &claim(&call), now + 1,
        ).unwrap();
        let expired = f.store.expire_adaptive_leadership_review_call(
            &f.leader, call.grant.review_id, dispatched.version, now + 1_000,
        ).unwrap();
        assert!(expired.decision.is_none());
        f.context.evidence_refs.push(format!(
            "leadership-review-retired:{}:{}",
            expired.grant.review_id, expired.retired_at_unix_ms.unwrap(),
        ));
        retired.push(expired);
        now += 1_001;
    }
    assert_eq!(session(&f), source);
    assert_eq!(f.store.company_project(&f.leader.tenant_id, &f.grant.project_id).unwrap(), Some(project));
    f.grant.expires_at_unix_ms = now + 120_000;
    refresh_recovery_review(&mut f);
    (f, retired, now)
}

fn refresh_recovery_review(f: &mut Fixture) {
    f.grant.evidence_fingerprint = adaptive_leadership_evidence_fingerprint(
        &f.context.tool_catalog, &f.context.evidence_refs,
    ).unwrap();
    f.grant.review_id = adaptive_leadership_review_id(
        f.grant.session_id, f.grant.expected_session_version, &f.grant.evidence_fingerprint,
    ).unwrap();
}

fn recovery_operator(tenant: &TenantId) -> AuthenticatedCompanyPrincipalV1 {
    let authority = PrincipalAuthorityV1::derive("recovery-operator", 1, &[9; 32]).unwrap();
    AuthenticatedCompanyPrincipalV1 {
        schema_version: 1,
        tenant_id: tenant.clone(),
        principal_id: authority.principal_id,
        kind: CompanyPrincipalKindV1::Operator,
        role: CompanyRoleV1::ProjectManager,
        customer_id: None,
        agent_id: None,
        authority_generation: authority.principal_generation,
        authority_digest: authority.authority_digest,
    }
}

fn recovery_request(
    f: &mut Fixture,
    retired: &[AdaptiveLeadershipReviewCallV1],
) -> AdaptiveLeadershipRecoveryRequestV1 {
    let source = &f.context.source_session;
    let (schema_version, unknown_effect, sealed_unknown_proof_digest, blocked_subject) =
        match f.grant.subject.as_ref().unwrap() {
            AdaptiveLeadershipReviewSubjectV2::UnknownModel { effect, sealed_unknown_proof_digest } => {
                (1, Some(effect.clone()), Some(sealed_unknown_proof_digest.clone()), None)
            }
            AdaptiveLeadershipReviewSubjectV2::BlockedContinuation { reason_code, resolution_event_id: None } => {
                (2, None, None, Some(AdaptiveLeadershipRecoveryBlockedSubjectV1 {
                    reason_code: reason_code.clone(),
                    model_response_digest: source.last_model_result_digest.clone().unwrap(),
                }))
            }
            _ => panic!("unsupported recovery fixture"),
    };
    let (_, head_digest) = crate::store::adaptive::load(
        &f.store.connection.lock().unwrap(), source.grant.session_id,
    ).unwrap().unwrap();
    let request = AdaptiveLeadershipRecoveryRequestV1 {
        schema_version,
        operation_id: Uuid::new_v4(),
        tenant_id: f.leader.tenant_id.clone(),
        project_id: f.grant.project_id.clone(),
        work_item_id: f.grant.work_item_id.clone(),
        session_id: f.grant.session_id,
        expected_project_version: f.grant.expected_project_version,
        expected_session_version: f.grant.expected_session_version,
        session_head_digest: head_digest,
        session_digest: adaptive_leadership_recovery_session_digest(source).unwrap(),
        project_digest: adaptive_leadership_recovery_project_digest(&f.context.source_project).unwrap(),
        unknown_effect,
        sealed_unknown_proof_digest,
        blocked_subject,
        admission_repair: None,
        prior_review_history_digest: adaptive_leadership_recovery_history_digest(retired).unwrap(),
        repair_digest: "b".repeat(64),
        release: AdaptiveRecoveryReleaseV1 {
            schema_version: 1,
            source_git_sha: "c".repeat(40),
            release_manifest_digest: "d".repeat(64),
            gateway_binary_digest: "e".repeat(64),
        },
        reason_ref: "issue-856-verified-repair".into(),
        expires_at_unix_ms: f.grant.expires_at_unix_ms,
        max_additional_model_calls: 1,
        max_window_ms: 120_000,
    };
    f.context.evidence_refs.push(format!("recovery-request:{}", request.canonical_digest().unwrap()));
    f.context.evidence_refs.sort();
    refresh_recovery_review(f);
    request
}

fn recovery_call(f: &Fixture, epoch: &AdaptiveLeadershipRecoveryEpochV1) -> AdaptiveLeadershipReviewCallV1 {
    f.store.adaptive_leadership_review_call(&f.leader.tenant_id, epoch.review_id).unwrap().unwrap()
}

fn authorize_recovery(
    f: &Fixture,
    request: &AdaptiveLeadershipRecoveryRequestV1,
    now: u64,
) -> AdaptiveLeadershipRecoveryEpochV1 {
    let operator = recovery_operator(&f.leader.tenant_id);
    let (replayed, epoch) = f.store.authorize_adaptive_leadership_recovery_epoch(
        &operator, request, &f.grant, &f.context, now,
    ).unwrap();
    assert!(!replayed);
    epoch
}

#[test]
fn blocked_recovery_preserves_original_session_history_and_permanent_slot() {
    let (mut f, retired, now) = recovery_source_with_subject(false);
    let source = session(&f);
    let mut request = recovery_request(&mut f, &retired);
    request.max_additional_model_calls = source.grant.max_model_calls - source.model_calls;
    f.context.evidence_refs.retain(|reference| !reference.starts_with("recovery-request:"));
    f.context.evidence_refs.push(format!("recovery-request:{}", request.canonical_digest().unwrap()));
    f.context.evidence_refs.sort();
    refresh_recovery_review(&mut f);
    assert_eq!(request.schema_version, 2);
    assert!(request.unknown_effect.is_none());
    assert!(request.sealed_unknown_proof_digest.is_none());
    assert_eq!(request.blocked_subject.as_ref().unwrap().model_response_digest,
        source.last_model_result_digest.clone().unwrap());
    let epoch = authorize_recovery(&f, &request, now);
    epoch.validate_history(&retired).unwrap();
    let call = recovery_call(&f, &epoch);
    let dispatched = f.store.claim_adaptive_leadership_review_call(
        &f.leader, &claim(&call), now + 1,
    ).unwrap();
    let result = recovery_continue_result_with_calls(&dispatched, now + 2, 2);
    let completed = f.store.complete_adaptive_leadership_review_call(
        &f.leader, &result, now + 2,
    ).unwrap();
    let continued = session(&f);
    assert_eq!(continued.grant, source.grant);
    assert_eq!((continued.model_calls, continued.tool_calls), (source.model_calls, source.tool_calls));
    assert_eq!(continued.last_model_result_digest, source.last_model_result_digest);
    assert_eq!(continued.effect_ids, source.effect_ids);
    assert!(continued.requires_fresh_observation());
    assert_eq!(continued.active_model_ceiling(), source.model_calls + 2);
    assert!(completed.continuation.as_ref().unwrap().abandoned_model_effect.is_none());
    let calls = f.store.adaptive_leadership_review_calls(&request.tenant_id, request.session_id).unwrap();
    assert_eq!(calls.len(), 4);
    for prior in retired {
        assert!(calls.contains(&prior));
    }
    let before = rows(&f.store);
    let operator = recovery_operator(&request.tenant_id);
    assert_eq!(f.store.authorize_adaptive_leadership_recovery_epoch(
        &operator, &request, &f.grant, &f.context, request.expires_at_unix_ms + 1,
    ).unwrap(), (true, epoch));
    let mut other_subject = request.clone();
    other_subject.schema_version = 1;
    other_subject.unknown_effect = Some(AdaptiveEffectV1 { id: Uuid::new_v4(), request_digest: DIGEST.into() });
    other_subject.sealed_unknown_proof_digest = Some(DIGEST.into());
    other_subject.blocked_subject = None;
    assert_eq!(f.store.authorize_adaptive_leadership_recovery_epoch(
        &operator, &other_subject, &f.grant, &f.context, request.expires_at_unix_ms + 1,
    ).unwrap_err().code, WorkflowErrorCode::IdempotencyConflict);
    assert_eq!(rows(&f.store), before);
}

#[test]
fn blocked_recovery_subject_and_original_root_caps_cannot_be_rebound() {
    for field in ["reason", "result", "calls", "window", "subject", "resolved", "clock"] {
        let (mut f, retired, now) = recovery_source_with_subject(false);
        let source = session(&f);
        let mut request = recovery_request(&mut f, &retired);
        let issued = match field {
            "reason" => { request.blocked_subject.as_mut().unwrap().reason_code = "other_blocked".into(); now },
            "result" => { request.blocked_subject.as_mut().unwrap().model_response_digest = "f".repeat(64); now },
            "calls" => { request.max_additional_model_calls = source.grant.max_model_calls - source.model_calls + 1; now },
            "window" => { request.max_window_ms = source.grant.deadline_ms - source.grant.created_at_ms + 1; now },
            "subject" => {
                request.schema_version = 1;
                request.unknown_effect = Some(AdaptiveEffectV1 { id: Uuid::new_v4(), request_digest: DIGEST.into() });
                request.sealed_unknown_proof_digest = Some(DIGEST.into());
                request.blocked_subject = None;
                now
            }
            "resolved" => {
                f.grant.subject = Some(AdaptiveLeadershipReviewSubjectV2::BlockedContinuation {
                    reason_code: REASON.into(), resolution_event_id: Some(Uuid::new_v4().to_string()),
                });
                now
            }
            "clock" => source.active_deadline_ms() - 1,
            _ => unreachable!(),
        };
        f.context.evidence_refs.retain(|reference| !reference.starts_with("recovery-request:"));
        f.context.evidence_refs.push(format!("recovery-request:{}", request.canonical_digest().unwrap()));
        f.context.evidence_refs.sort();
        refresh_recovery_review(&mut f);
        let before = rows(&f.store);
        assert!(f.store.authorize_adaptive_leadership_recovery_epoch(
            &recovery_operator(&request.tenant_id), &request, &f.grant, &f.context, issued,
        ).is_err(), "accepted invalid blocked {field}");
        assert_eq!(rows(&f.store), before);
        assert_eq!(session(&f), source);
    }
}

#[test]
fn unknown_recovery_cannot_use_a_blocked_request_or_mixed_retired_history() {
    let (mut f, retired, now) = recovery_source();
    let mut request = recovery_request(&mut f, &retired);
    request.schema_version = 2;
    request.unknown_effect = None;
    request.sealed_unknown_proof_digest = None;
    request.blocked_subject = Some(AdaptiveLeadershipRecoveryBlockedSubjectV1 {
        reason_code: REASON.into(), model_response_digest: DIGEST.into(),
    });
    f.context.evidence_refs.retain(|reference| !reference.starts_with("recovery-request:"));
    f.context.evidence_refs.push(format!("recovery-request:{}", request.canonical_digest().unwrap()));
    f.context.evidence_refs.sort();
    refresh_recovery_review(&mut f);
    let before = rows(&f.store);
    assert!(f.store.authorize_adaptive_leadership_recovery_epoch(
        &recovery_operator(&request.tenant_id), &request, &f.grant, &f.context, now,
    ).is_err());
    assert_eq!(rows(&f.store), before);

    let (mut blocked, blocked_history, blocked_now) = recovery_source_with_subject(false);
    let mut mixed = blocked_history.clone();
    mixed[0] = retired[0].clone();
    let request = recovery_request(&mut blocked, &mixed);
    let before = rows(&blocked.store);
    assert!(blocked.store.authorize_adaptive_leadership_recovery_epoch(
        &recovery_operator(&request.tenant_id), &request, &blocked.grant, &blocked.context, blocked_now,
    ).is_err());
    assert_eq!(rows(&blocked.store), before);
}

#[test]
fn recovery_epoch_preserves_original_count_source_and_single_slot() {
    let (mut f, retired, now) = recovery_source();
    let source = session(&f);
    let project = f.context.source_project.clone();
    let request = recovery_request(&mut f, &retired);
    let operator = recovery_operator(&f.leader.tenant_id);
    let before = rows(&f.store);
    assert!(f.store.authorize_adaptive_leadership_review_call(
        &f.leader, request.operation_id, "normal-cap-bypass", &f.grant, &f.context, now,
    ).is_err());
    assert_eq!(rows(&f.store), before);
    let epoch = authorize_recovery(&f, &request, now);
    let call = recovery_call(&f, &epoch);
    assert_eq!(call.grant.recovery_epoch, Some(epoch.binding().unwrap()));
    assert_eq!(call.operation_id, request.operation_id);
    assert_eq!(session(&f), source);
    assert_eq!(f.store.company_project(&f.leader.tenant_id, &f.grant.project_id).unwrap(), Some(project));
    let calls = f.store.adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id).unwrap();
    assert_eq!(calls.len(), ADAPTIVE_LEADERSHIP_MAX_REVIEWS + 1);
    assert_eq!(calls.iter().filter(|call| call.grant.recovery_epoch.is_none()).count(), 3);
    for prior in &retired {
        assert!(calls.contains(prior));
    }
    let before = rows(&f.store);
    let mut second = request.clone();
    second.operation_id = Uuid::new_v4();
    assert!(f.store.authorize_adaptive_leadership_recovery_epoch(
        &operator, &second, &f.grant, &f.context, now + 1,
    ).is_err());
    let mut second_grant = call.grant.clone();
    second_grant.review_id = Uuid::new_v4();
    assert!(f.store.authorize_adaptive_leadership_review_call(
        &f.leader, Uuid::new_v4(), "epoch-second-slot", &second_grant, &f.context, now + 1,
    ).is_err());
    assert_eq!(rows(&f.store), before);
}

#[test]
fn recovery_epoch_reopens_and_replays_before_expiry_and_project_currentness() {
    let (mut f, retired, now) = recovery_source();
    let request = recovery_request(&mut f, &retired);
    let operator = recovery_operator(&f.leader.tenant_id);
    let epoch = authorize_recovery(&f, &request, now);
    change_project(&f, now + 1);
    let reopened = WorkflowStore::open(&f.path).unwrap();
    let before = rows(&reopened);
    assert_eq!(reopened.authorize_adaptive_leadership_recovery_epoch(
        &operator, &request, &f.grant, &f.context, request.expires_at_unix_ms + 1,
    ).unwrap(), (true, epoch.clone()));
    assert_eq!(reopened.adaptive_leadership_recovery_epoch(
        &operator.tenant_id, request.session_id,
    ).unwrap(), Some(epoch.clone()));
    let call = recovery_call(&f, &epoch);
    assert!(reopened.claim_adaptive_leadership_review_call(&f.leader, &claim(&call), now + 2).is_err());
    assert_eq!(rows(&reopened), before);
}

#[test]
fn recovery_epoch_changed_payload_issuer_and_cross_tenant_never_rebind() {
    let (mut f, retired, now) = recovery_source();
    let request = recovery_request(&mut f, &retired);
    let operator = recovery_operator(&f.leader.tenant_id);
    authorize_recovery(&f, &request, now);
    let before = rows(&f.store);
    let mut changed_requests = Vec::new();
    let mut changed = request.clone();
    changed.operation_id = Uuid::new_v4();
    changed_requests.push(changed);
    let mut changed = request.clone();
    changed.repair_digest = "f".repeat(64);
    changed_requests.push(changed);
    let mut changed = request.clone();
    changed.expected_session_version += 1;
    changed_requests.push(changed);
    let mut changed = request.clone();
    changed.release.gateway_binary_digest = "f".repeat(64);
    changed_requests.push(changed);
    let mut changed = request.clone();
    changed.expires_at_unix_ms += 1;
    changed_requests.push(changed);
    for changed in changed_requests {
        assert_eq!(f.store.authorize_adaptive_leadership_recovery_epoch(
            &operator, &changed, &f.grant, &f.context, now + 1,
        ).unwrap_err().code, WorkflowErrorCode::IdempotencyConflict);
    }
    let mut changed_operator = operator.clone();
    changed_operator.authority_generation += 1;
    assert!(f.store.authorize_adaptive_leadership_recovery_epoch(
        &changed_operator, &request, &f.grant, &f.context, now + 1,
    ).is_err());
    let foreign = recovery_operator(&TenantId::parse("tenant-foreign").unwrap());
    assert!(f.store.authorize_adaptive_leadership_recovery_epoch(
        &foreign, &request, &f.grant, &f.context, now + 1,
    ).is_err());
    assert!(f.store.adaptive_leadership_recovery_epoch(&foreign.tenant_id, request.session_id).unwrap().is_none());
    assert_eq!(rows(&f.store), before);
}

#[test]
fn recovery_epoch_concurrent_issuance_has_one_durable_winner() {
    let (mut f, retired, now) = recovery_source();
    let request = recovery_request(&mut f, &retired);
    let operator = recovery_operator(&f.leader.tenant_id);
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2).map(|_| {
        let store = WorkflowStore::open(&f.path).unwrap();
        let barrier = barrier.clone();
        let operator = operator.clone();
        let request = request.clone();
        let grant = f.grant.clone();
        let context = f.context.clone();
        std::thread::spawn(move || {
            barrier.wait();
            store.authorize_adaptive_leadership_recovery_epoch(&operator, &request, &grant, &context, now)
        })
    }).collect();
    let results: Vec<_> = handles.into_iter().map(|handle| handle.join().unwrap().unwrap()).collect();
    assert_ne!(results[0].0, results[1].0);
    assert_eq!(results[0].1, results[1].1);
    assert_eq!(f.store.adaptive_leadership_review_calls(&operator.tenant_id, request.session_id).unwrap().len(), 4);
    let connection = f.store.connection.lock().unwrap();
    let events: i64 = connection.query_row(
        "SELECT COUNT(*) FROM company_events WHERE event_type='adaptive_leadership_recovery_epoch_authorized'",
        [], |row| row.get(0),
    ).unwrap();
    assert_eq!(events, 1);
}

#[test]
fn recovery_epoch_invalid_head_history_and_time_do_not_consume_authority() {
    for changed_field in ["head", "history", "effect", "time", "project"] {
        let (mut f, retired, now) = recovery_source();
        let mut request = recovery_request(&mut f, &retired);
        let operator = recovery_operator(&f.leader.tenant_id);
        let issued_at = match changed_field {
            "head" => { request.session_head_digest = "f".repeat(64); now },
            "history" => { request.prior_review_history_digest = "f".repeat(64); now },
            "effect" => { request.unknown_effect.as_mut().unwrap().id = Uuid::new_v4(); now },
            "time" => request.expires_at_unix_ms,
            "project" => { change_project(&f, now + 1); now + 2 },
            _ => unreachable!(),
        };
        f.context.evidence_refs.retain(|reference| !reference.starts_with("recovery-request:"));
        f.context.evidence_refs.push(format!("recovery-request:{}", request.canonical_digest().unwrap()));
        f.context.evidence_refs.sort();
        refresh_recovery_review(&mut f);
        let before = rows(&f.store);
        assert!(f.store.authorize_adaptive_leadership_recovery_epoch(
            &operator, &request, &f.grant, &f.context, issued_at,
        ).is_err(), "accepted invalid {changed_field}");
        assert_eq!(rows(&f.store), before);
        assert!(f.store.adaptive_leadership_recovery_epoch(&operator.tenant_id, request.session_id).unwrap().is_none());
    }
}

#[test]
fn recovery_epoch_checksum_valid_tamper_fails_historical_read_and_dispatch() {
    let (mut f, retired, now) = recovery_source();
    let request = recovery_request(&mut f, &retired);
    let mut epoch = authorize_recovery(&f, &request, now);
    let call = recovery_call(&f, &epoch);
    epoch.issuer_principal.authority_generation += 1;
    epoch.issuer_authority.principal_generation += 1;
    persist_entity(&f.store, &epoch);
    let before = rows(&f.store);
    assert!(f.store.adaptive_leadership_recovery_epoch(&request.tenant_id, request.session_id).is_err());
    assert!(f.store.adaptive_leadership_review_call(&request.tenant_id, epoch.review_id).is_err());
    assert!(f.store.claim_adaptive_leadership_review_call(&f.leader, &claim(&call), now + 1).is_err());
    assert_eq!(rows(&f.store), before);
}

#[test]
fn recovery_epoch_expiry_retires_without_restoring_slot_or_decision() {
    let (mut f, retired, now) = recovery_source();
    let request = recovery_request(&mut f, &retired);
    let epoch = authorize_recovery(&f, &request, now);
    let call = recovery_call(&f, &epoch);
    let dispatched = f.store.claim_adaptive_leadership_review_call(&f.leader, &claim(&call), now + 1).unwrap();
    let source = session(&f);
    let retired = f.store.expire_adaptive_leadership_review_call(
        &f.leader, epoch.review_id, dispatched.version, request.expires_at_unix_ms,
    ).unwrap();
    assert!(retired.decision.is_none());
    assert_eq!(session(&f), source);
    let before = rows(&f.store);
    let operator = recovery_operator(&request.tenant_id);
    assert_eq!(f.store.authorize_adaptive_leadership_recovery_epoch(
        &operator, &request, &f.grant, &f.context, request.expires_at_unix_ms + 1,
    ).unwrap(), (true, epoch));
    let mut second = request;
    second.operation_id = Uuid::new_v4();
    assert!(f.store.authorize_adaptive_leadership_recovery_epoch(
        &operator, &second, &f.grant, &f.context, second.expires_at_unix_ms + 1,
    ).is_err());
    assert_eq!(rows(&f.store), before);
}

fn recovery_continue_result(
    call: &AdaptiveLeadershipReviewCallV1,
    issued_at: u64,
) -> CompleteAdaptiveLeadershipReviewCallV1 {
    recovery_continue_result_with_calls(call, issued_at, 1)
}

fn recovery_continue_result_with_calls(
    call: &AdaptiveLeadershipReviewCallV1,
    issued_at: u64,
    calls: u16,
) -> CompleteAdaptiveLeadershipReviewCallV1 {
    let mut result = continue_result(call);
    if let AdaptiveLeadershipReviewDecisionKindV1::Continue { additional_model_calls, .. } = &mut result.decision.decision {
        *additional_model_calls = calls;
    }
    let audit = adaptive_leadership_continuation_audit_id(
        call.grant.review_id, &result.request_digest, &result.model_response_digest, &result.decision,
    ).unwrap();
    result.resolution_event_id = Some(audit);
    let allowance = call.continuation_allowance(issued_at, issued_at + 120_000, calls).unwrap();
    let authorization = result.continuation.as_mut().unwrap();
    authorization.resolution_event_id = audit;
    authorization.additional_model_calls = calls;
    authorization.issued_at_ms = issued_at;
    authorization.deadline_ms = issued_at + 120_000;
    authorization.provider_allowance_id = allowance.allowance_id.clone();
    authorization.provider_authority_digest = adaptive_leadership_continuation_provider_authority_digest(
        &allowance, &call.grant.assignee_authority,
    ).unwrap();
    result
}

fn local_adoption_source(unknown: bool) -> (
    Fixture,
    AdaptiveLeadershipRecoveryEpochV1,
    AdaptiveLeadershipReviewCallV1,
    CompleteAdaptiveLeadershipReviewCallV1,
    AdaptiveLeadershipLocalAdoptionRequestV1,
    u64,
) {
    let (mut f, retired, now) = recovery_source_with_subject(unknown);
    let recovery = recovery_request(&mut f, &retired);
    let epoch = authorize_recovery(&f, &recovery, now);
    let call = recovery_call(&f, &epoch);
    let dispatched = f.store.claim_adaptive_leadership_review_call(
        &f.leader, &claim(&call), now + 1,
    ).unwrap();
    // Retain the known response before expiry; only local authority gets a new clock.
    let known = recovery_continue_result(&dispatched, now + 2);
    let issued = dispatched.grant.expires_at_unix_ms + 1;
    let request = AdaptiveLeadershipLocalAdoptionRequestV1 {
        schema_version: 1,
        operation_id: Uuid::new_v4(),
        tenant_id: recovery.tenant_id.clone(),
        project_id: recovery.project_id.clone(),
        work_item_id: recovery.work_item_id.clone(),
        session_id: recovery.session_id,
        review_id: epoch.review_id,
        epoch_digest: epoch.canonical_digest().unwrap(),
        original_call_digest: crate::adaptive_leadership_local_adoption_source_call_digest(&dispatched).unwrap(),
        project_digest: recovery.project_digest.clone(),
        session_digest: recovery.session_digest.clone(),
        session_head_digest: recovery.session_head_digest.clone(),
        request_id: dispatched.request_id(),
        request_digest: known.request_digest.clone(),
        context_digest: dispatched.context_digest().unwrap(),
        payload_digest: "1".repeat(64),
        model_response_digest: known.model_response_digest.clone(),
        usage_event_digest: "2".repeat(64),
        original_completion_error: "continuation audit invalid".into(),
        completion_attempts: 5,
        release: AdaptiveRecoveryReleaseV1 {
            schema_version: 1,
            source_git_sha: "3".repeat(40),
            release_manifest_digest: "4".repeat(64),
            gateway_binary_digest: "5".repeat(64),
        },
        repair_digest: "6".repeat(64),
        decision: known.decision.clone(),
        expires_at_unix_ms: issued + 60_000,
    };
    (f, epoch, dispatched, known, request, issued)
}

fn local_adoption_result(
    call: &AdaptiveLeadershipReviewCallV1,
    known: &CompleteAdaptiveLeadershipReviewCallV1,
    adoption: &AdaptiveLeadershipLocalAdoptionV1,
) -> CompleteAdaptiveLeadershipReviewCallV1 {
    let mut result = known.clone();
    let authorization = result.continuation.as_mut().unwrap();
    let allowance = call.continuation_allowance(
        adoption.issued_at_unix_ms, adoption.continuation_deadline_ms,
        authorization.additional_model_calls,
    ).unwrap();
    authorization.issued_at_ms = adoption.issued_at_unix_ms;
    authorization.deadline_ms = adoption.continuation_deadline_ms;
    authorization.provider_authority_digest = adaptive_leadership_continuation_provider_authority_digest(
        &allowance, &call.grant.assignee_authority,
    ).unwrap();
    authorization.local_adoption = Some(Box::new(adoption.clone()));
    result
}

#[test]
fn funded_review_preserves_completed_recovery_and_local_adoption_history() {
    for (unknown, local) in [(false, false), (false, true), (true, false), (true, true)] {
        let (mut f, epoch, dispatched, known, request, issued) = local_adoption_source(unknown);
        let completed = if local {
            let (_, adoption) = f.store.authorize_adaptive_leadership_local_adoption(
                &recovery_operator(&request.tenant_id), &request, issued,
            ).unwrap();
            let result = local_adoption_result(&dispatched, &known, &adoption);
            f.store.complete_adaptive_leadership_review_call(
                &f.leader, &result, issued + 1,
            ).unwrap()
        } else {
            f.store.complete_adaptive_leadership_review_call(
                &f.leader, &known, known.continuation.as_ref().unwrap().issued_at_ms,
            ).unwrap()
        };
        let continued = session(&f);
        let observed_at = continued.continuation.as_ref().unwrap()
            .authorizations.last().unwrap().issued_at_ms + 1;
        observe_budget_inspection(&f, continued, observed_at);
        let now = session(&f).active_deadline_ms() + 1;
        budget_context(&mut f, now, "fund-after-completed-recovery");
        let source = session(&f);
        let history = f.store.adaptive_leadership_review_calls(
            &f.leader.tenant_id, f.grant.session_id,
        ).unwrap();
        let funding = super::work_funding_tests::fund(&f, now);
        super::work_funding_tests::bind(
            &mut f, &funding, funding.request.source.resume_source.base_review_count + 1,
        );
        let call = super::work_funding_tests::dispatch(&f, now);
        let result = super::work_funding_tests::continued(&call, now + 2, 1);
        f.store.complete_adaptive_leadership_review_call(&f.leader, &result, now + 2).unwrap();
        let reopened = WorkflowStore::open(&f.path).unwrap();
        assert_eq!(reopened.adaptive_leadership_recovery_epoch(
            &request.tenant_id, request.session_id,
        ).unwrap(), Some(epoch));
        assert_eq!(reopened.adaptive_leadership_review_call(
            &f.leader.tenant_id, completed.grant.review_id,
        ).unwrap(), Some(completed));
        let calls = reopened.adaptive_leadership_review_calls(
            &f.leader.tenant_id, f.grant.session_id,
        ).unwrap();
        assert_eq!(calls.len(), history.len() + 1);
        assert!(history.iter().all(|prior| calls.contains(prior)));
        let adopted = session(&f);
        assert_eq!(adopted.grant, source.grant);
        assert_eq!((adopted.model_calls, adopted.tool_calls),
            (source.model_calls, source.tool_calls));
        assert_eq!(adopted.active_work_funding(), call.grant.work_funding.as_deref());
        assert_eq!(adopted.continuation.as_ref().unwrap().authorizations.len(), 2);
    }
}

#[test]
fn funding_rejects_missing_or_corrupt_completed_local_adoption_proof_without_writes() {
    for corruption in ["missing_entity", "missing_event", "corrupt_event"] {
        let (mut f, _, dispatched, known, request, issued) = local_adoption_source(false);
        let (_, adoption) = f.store.authorize_adaptive_leadership_local_adoption(
            &recovery_operator(&request.tenant_id), &request, issued,
        ).unwrap();
        let result = local_adoption_result(&dispatched, &known, &adoption);
        f.store.complete_adaptive_leadership_review_call(
            &f.leader, &result, issued + 1,
        ).unwrap();
        let continued = session(&f);
        let observed_at = continued.continuation.as_ref().unwrap()
            .authorizations.last().unwrap().issued_at_ms + 1;
        observe_budget_inspection(&f, continued, observed_at);
        let now = session(&f).active_deadline_ms() + 1;
        budget_context(&mut f, now, "fund-after-damaged-local-adoption");
        let operator = recovery_operator(&request.tenant_id);
        let draft = f.store.adaptive_work_funding_draft(
            &operator, &f.grant.project_id, f.grant.session_id, Uuid::new_v4(),
            "require-completed-local-adoption-proof", crate::AdaptiveWorkFundingLimitsV1 {
                additional_model_calls: 6,
                additional_tool_calls: 6,
                additional_reviews: 3,
                additional_windows: 3,
                max_window_ms: 180_000,
                max_call_duration_ms: f.context.source_session.grant.max_call_duration_ms,
                dispatch_margin_ms: crate::ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
                expires_at_unix_ms: now + 3_600_000,
            }, now,
        ).unwrap();
        {
            let connection = f.store.connection.lock().unwrap();
            let changed = match corruption {
                "missing_entity" => connection.execute(
                    "DELETE FROM company_entities WHERE tenant_id=?1
                     AND entity_kind='adaptive_leadership_local_adoption' AND entity_id=?2",
                    params![request.tenant_id.0, adoption.adoption_key],
                ),
                "missing_event" => connection.execute(
                    "DELETE FROM company_events WHERE tenant_id=?1
                     AND event_type='adaptive_leadership_local_adoption_authorized' AND operation_id=?2",
                    params![request.tenant_id.0, request.operation_id.to_string()],
                ),
                "corrupt_event" => connection.execute(
                    "UPDATE company_events SET payload=X'00' WHERE tenant_id=?1
                     AND event_type='adaptive_leadership_local_adoption_authorized' AND operation_id=?2",
                    params![request.tenant_id.0, request.operation_id.to_string()],
                ),
                _ => unreachable!(),
            }.unwrap();
            assert_eq!(changed, 1);
        }
        let before = rows(&f.store);
        let reopened = WorkflowStore::open(&f.path).unwrap();
        assert!(reopened.adaptive_leadership_review_call(
            &request.tenant_id, request.review_id,
        ).is_err());
        assert!(reopened.authorize_adaptive_work_funding(&operator, &draft, now).is_err());
        assert_eq!(rows(&reopened), before);
    }
}

#[test]
fn funding_rejects_unresolved_recovery_even_after_review_expiry_without_writes() {
    for unknown in [false, true] {
        let (f, _, pending, _, request, issued) = local_adoption_source(unknown);
        assert!(issued > pending.grant.expires_at_unix_ms);
        assert!(pending.decision.is_none() && pending.retired_at_unix_ms.is_none());
        let before = rows(&f.store);
        let limits = crate::AdaptiveWorkFundingLimitsV1 {
            additional_model_calls: 6,
            additional_tool_calls: 6,
            additional_reviews: 3,
            additional_windows: 3,
            max_window_ms: 180_000,
            max_call_duration_ms: f.context.source_session.grant.max_call_duration_ms,
            dispatch_margin_ms: crate::ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
            expires_at_unix_ms: issued + 3_600_000,
        };
        assert!(f.store.adaptive_work_funding_draft(
            &recovery_operator(&request.tenant_id), &f.grant.project_id,
            f.grant.session_id, Uuid::new_v4(), "no-unresolved-recovery-bypass",
            limits, issued,
        ).is_err());
        assert_eq!(rows(&f.store), before);
    }
}

#[test]
fn local_adoption_persisted_continue_completes_both_subjects_and_replays_after_reopen() {
    for unknown in [true, false] {
        let (f, epoch, call, known, request, issued) = local_adoption_source(unknown);
        let source = session(&f);
        let operator = recovery_operator(&request.tenant_id);
        assert!(issued > call.grant.expires_at_unix_ms);
        assert_ne!(request.release, epoch.request.release);
        let (replayed, adoption) = f.store.authorize_adaptive_leadership_local_adoption(
            &operator, &request, issued,
        ).unwrap();
        assert!(!replayed);
        assert_eq!(adoption.issued_at_unix_ms, issued);
        assert_eq!(adoption.continuation_deadline_ms, issued + 120_000);
        assert_eq!(session(&f), source);
        assert_eq!(recovery_call(&f, &epoch), call);
        let before = rows(&f.store);
        let reopened = WorkflowStore::open(&f.path).unwrap();
        assert_eq!(reopened.adaptive_leadership_local_adoption(
            &request.tenant_id, request.review_id,
        ).unwrap(), Some(adoption.clone()));
        assert_eq!(reopened.authorize_adaptive_leadership_local_adoption(
            &operator, &request, issued + 10,
        ).unwrap(), (true, adoption.clone()));
        assert_eq!(rows(&reopened), before);
        let result = local_adoption_result(&call, &known, &adoption);
        assert_eq!(result.decision, known.decision);
        assert_eq!(result.model_response_digest, known.model_response_digest);
        assert_eq!(result.resolution_event_id, known.resolution_event_id);
        let completed = reopened.complete_adaptive_leadership_review_call(
            &f.leader, &result, issued + 11,
        ).unwrap();
        assert_eq!(completed.version, 3);
        assert_eq!(completed.decision, Some(request.decision.clone()));
        assert_eq!(completed.model_response_digest, Some(request.model_response_digest.clone()));
        let continued = session(&f);
        assert_eq!(continued.version, source.version + 1);
        assert_eq!(continued.grant, source.grant);
        assert_eq!(continued.model_calls, source.model_calls);
        assert_eq!(continued.tool_calls, source.tool_calls);
        let authorization = continued.continuation.as_ref().unwrap().authorizations.last().unwrap();
        assert_eq!(authorization, result.continuation.as_ref().unwrap());
        if let Some(effect) = &epoch.request.unknown_effect {
            assert!(continued.is_abandoned_model_effect(effect));
        } else {
            assert!(authorization.abandoned_model_effect.is_none());
        }
        assert_eq!(reopened.adaptive_leadership_review_calls(
            &request.tenant_id, request.session_id,
        ).unwrap().len(), ADAPTIVE_LEADERSHIP_MAX_REVIEWS + 1);
        change_project(&f, request.expires_at_unix_ms + 1);
        let before = rows(&f.store);
        let reopened = WorkflowStore::open(&f.path).unwrap();
        assert_eq!(reopened.authorize_adaptive_leadership_local_adoption(
            &operator, &request, adoption.continuation_deadline_ms + 1,
        ).unwrap(), (true, adoption));
        assert_eq!(reopened.complete_adaptive_leadership_review_call(
            &f.leader, &result, issued + 120_001,
        ).unwrap(), completed);
        assert_eq!(reopened.adaptive_leadership_review_call(
            &request.tenant_id, request.review_id,
        ).unwrap(), Some(completed));
        assert_eq!(rows(&reopened), before);
        assert_eq!(session(&f), continued);
        let audits: i64 = reopened.connection.lock().unwrap().query_row(
            "SELECT COUNT(*) FROM company_events
             WHERE event_type='adaptive_leadership_local_adoption_authorized' AND operation_id=?1",
            [request.operation_id.to_string()], |row| row.get(0),
        ).unwrap();
        assert_eq!(audits, 1);
    }
}

#[test]
fn local_adoption_expiry_rejects_fresh_issuance_and_completion_without_writes() {
    for unknown in [true, false] {
        let (f, epoch, call, known, request, issued) = local_adoption_source(unknown);
        let operator = recovery_operator(&request.tenant_id);
        let source = session(&f);
        let before = rows(&f.store);
        assert!(f.store.authorize_adaptive_leadership_local_adoption(
            &operator, &request, request.expires_at_unix_ms,
        ).is_err());
        assert_eq!(rows(&f.store), before);
        assert!(f.store.adaptive_leadership_local_adoption(
            &request.tenant_id, request.review_id,
        ).unwrap().is_none());
        let (_, adoption) = f.store.authorize_adaptive_leadership_local_adoption(
            &operator, &request, issued,
        ).unwrap();
        let result = local_adoption_result(&call, &known, &adoption);
        let before = rows(&f.store);
        let reopened = WorkflowStore::open(&f.path).unwrap();
        for now in [issued - 1, request.expires_at_unix_ms, adoption.continuation_deadline_ms] {
            assert!(reopened.complete_adaptive_leadership_review_call(&f.leader, &result, now).is_err());
            assert_eq!(rows(&reopened), before);
        }
        assert_eq!(reopened.authorize_adaptive_leadership_local_adoption(
            &operator, &request, request.expires_at_unix_ms + 1,
        ).unwrap(), (true, adoption));
        assert_eq!(rows(&reopened), before);
        assert_eq!(session(&f), source);
        assert_eq!(recovery_call(&f, &epoch), call);
    }
}

fn sealed_local_adoption_audit(
    call: &AdaptiveLeadershipReviewCallV1,
    result: &CompleteAdaptiveLeadershipReviewCallV1,
    appended_at: u64,
) -> sentinel_common::EventEnvelopeV2 {
    let proposal = local_adoption_audit_proposal(call, result).unwrap();
    // Synthetic store-owned fields are test fixtures, never runtime durability evidence.
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
    reseal_local_adoption_audit(&mut event);
    event
}

fn reseal_local_adoption_audit(event: &mut sentinel_common::EventEnvelopeV2) {
    event.payload_digest = sentinel_common::sha256_hex(&event.payload);
    event.stream_namespace = event.causal_context.authority_scope_digest().unwrap();
    event.append_receipt_digest = event.expected_append_receipt_digest().unwrap();
    event.sealed_envelope_digest = event.expected_sealed_envelope_digest().unwrap();
    event.validate_seals().unwrap();
}

#[test]
fn local_adoption_sealed_audit_replays_after_admission_before_fixed_deadline() {
    for unknown in [true, false] {
        let (f, epoch, call, known, request, issued) = local_adoption_source(unknown);
        let (_, adoption) = f.store.authorize_adaptive_leadership_local_adoption(
            &recovery_operator(&request.tenant_id), &request, issued,
        ).unwrap();
        let result = local_adoption_result(&call, &known, &adoption);
        let event = sealed_local_adoption_audit(&call, &result, request.expires_at_unix_ms - 1);
        let proposal = WorkflowStore::local_adoption_continuation_audit_proposal(&call, &result).unwrap();
        assert_eq!(proposal.payload, event.payload);
        assert_eq!(proposal.causal_context, event.causal_context);
        assert_eq!(proposal.canonical_request_digest().unwrap(), event.canonical_request_digest);
        let mut invalid = result.clone();
        invalid.allowance_id.push_str("-other");
        assert!(WorkflowStore::local_adoption_continuation_audit_proposal(&call, &invalid).is_err());
        let now = request.expires_at_unix_ms + 1;
        assert!(now < adoption.continuation_deadline_ms);
        let before = rows(&f.store);
        let reopened = WorkflowStore::open(&f.path).unwrap();
        assert!(reopened.complete_adaptive_leadership_review_call(&f.leader, &result, now).is_err());
        assert_eq!(rows(&reopened), before);
        let completed = reopened.complete_adaptive_leadership_review_call_with_local_adoption_audit(
            &f.leader, &result, &event, now,
        ).unwrap();
        assert_eq!(completed.version, 3);
        assert_eq!(completed.continuation, result.continuation);
        assert_eq!(recovery_call(&f, &epoch), completed);
        assert_eq!(session(&f).continuation.unwrap().authorizations.last(), result.continuation.as_ref());
        let before = rows(&reopened);
        assert!(reopened.retire_expired_adaptive_local_adoption(
            &f.leader, request.review_id, completed.version, now,
        ).is_err());
        assert_eq!(reopened.complete_adaptive_leadership_review_call_with_local_adoption_audit(
            &f.leader, &result, &event, now + 1,
        ).unwrap(), completed);
        assert_eq!(rows(&reopened), before);
    }
}

#[test]
fn local_adoption_audit_rejects_invalid_seals_payload_bindings_and_clocks_without_writes() {
    for unknown in [true, false] {
        let (f, epoch, call, known, request, issued) = local_adoption_source(unknown);
        let (_, adoption) = f.store.authorize_adaptive_leadership_local_adoption(
            &recovery_operator(&request.tenant_id), &request, issued,
        ).unwrap();
        let result = local_adoption_result(&call, &known, &adoption);
        let event = sealed_local_adoption_audit(&call, &result, issued + 1);
        let now = request.expires_at_unix_ms + 1;
        let before = rows(&f.store);
        for field in 0..18 {
            let mut changed = event.clone();
            match field {
                0 => changed.sealed_envelope_digest = "f".repeat(64),
                1 => changed.append_receipt_digest = "f".repeat(64),
                2 => changed.payload.push(b' '),
                3 => changed.event_type.push_str("_other"),
                4 => changed.producer.push_str("_other"),
                5 => changed.schema_version = 2,
                6 => changed.payload_codec = sentinel_common::EventPayloadCodec::DeterministicCbor,
                7 => changed.event_id = Uuid::now_v7().to_string(),
                8 => changed.causal_context.request_id.push_str("-other"),
                9 => changed.causal_context.operation_id = Uuid::now_v7().to_string(),
                10 => changed.causal_context.correlation_id = Uuid::new_v4().to_string(),
                11 => changed.durability = sentinel_common::EventDurability::DurableOperational,
                12 => changed.appended_at_ms = i64::try_from(issued - 1).unwrap(),
                13 => changed.appended_at_ms = i64::try_from(request.expires_at_unix_ms).unwrap(),
                14 => changed.appended_at_ms = i64::try_from(now + 1).unwrap(),
                15 => {
                    let mut payload: serde_json::Value = serde_json::from_slice(&changed.payload).unwrap();
                    payload["call"]["updated_at_unix_ms"] = serde_json::json!(call.updated_at_unix_ms + 1);
                    changed.payload = sentinel_common::canonical_json(&payload).unwrap();
                }
                16 => {
                    let mut payload: serde_json::Value = serde_json::from_slice(&changed.payload).unwrap();
                    payload["result"]["continuation"]["additional_model_calls"] = serde_json::json!(2);
                    changed.payload = sentinel_common::canonical_json(&payload).unwrap();
                }
                _ => changed.canonical_request_digest = "f".repeat(64),
            }
            if field >= 3 {
                reseal_local_adoption_audit(&mut changed);
            }
            assert!(f.store.complete_adaptive_leadership_review_call_with_local_adoption_audit(
                &f.leader, &result, &changed, now,
            ).is_err(), "accepted changed audit field {field}");
            assert_eq!(rows(&f.store), before);
        }
        assert!(f.store.complete_adaptive_leadership_review_call_with_local_adoption_audit(
            &f.leader, &result, &event, issued,
        ).is_err());
        let mut noncanonical = event.clone();
        noncanonical.payload.push(b' ');
        reseal_local_adoption_audit(&mut noncanonical);
        assert!(f.store.complete_adaptive_leadership_review_call_with_local_adoption_audit(
            &f.leader, &result, &noncanonical, now,
        ).is_err());
        assert!(f.store.complete_adaptive_leadership_review_call_with_local_adoption_audit(
            &f.leader, &result, &event, adoption.continuation_deadline_ms,
        ).is_err());
        assert_eq!(rows(&f.store), before);
        assert_eq!(session(&f), call.context.source_session);
        assert_eq!(recovery_call(&f, &epoch), call);
    }
}

#[test]
fn local_adoption_retirement_requires_expired_admission_and_replays_terminal_unchanged() {
    for unknown in [true, false] {
        let (f, epoch, call, _, request, issued) = local_adoption_source(unknown);
        let before = rows(&f.store);
        assert!(f.store.retire_expired_adaptive_local_adoption(
            &f.leader, request.review_id, call.version, request.expires_at_unix_ms,
        ).is_err());
        assert_eq!(rows(&f.store), before);
        let (_, adoption) = f.store.authorize_adaptive_leadership_local_adoption(
            &recovery_operator(&request.tenant_id), &request, issued,
        ).unwrap();
        let before = rows(&f.store);
        for (version, now) in [(call.version, request.expires_at_unix_ms - 1),
            (call.version + 1, request.expires_at_unix_ms)] {
            assert!(f.store.retire_expired_adaptive_local_adoption(
                &f.leader, request.review_id, version, now,
            ).is_err());
            assert_eq!(rows(&f.store), before);
        }
        f.store.connection.lock().unwrap().execute_batch(
            "CREATE TRIGGER reject_local_adoption_retirement BEFORE INSERT ON company_events
             WHEN NEW.event_type='adaptive_leadership_review_retired'
             BEGIN SELECT RAISE(ABORT, 'test retirement audit failure'); END;",
        ).unwrap();
        assert!(f.store.retire_expired_adaptive_local_adoption(
            &f.leader, request.review_id, call.version, request.expires_at_unix_ms,
        ).is_err());
        assert_eq!(rows(&f.store), before);
        assert_eq!(recovery_call(&f, &epoch), call);
        f.store.connection.lock().unwrap().execute_batch(
            "DROP TRIGGER reject_local_adoption_retirement;",
        ).unwrap();
        let retired = f.store.retire_expired_adaptive_local_adoption(
            &f.leader, request.review_id, call.version, request.expires_at_unix_ms,
        ).unwrap();
        assert_eq!(retired.version, 4);
        assert_eq!(retired.retired_at_unix_ms, Some(request.expires_at_unix_ms));
        assert!(retired.decision.is_none());
        assert!(retired.continuation.is_none());
        assert_eq!(session(&f), call.context.source_session);
        assert_eq!(recovery_call(&f, &epoch), retired);
        let before = rows(&f.store);
        let reopened = WorkflowStore::open(&f.path).unwrap();
        for version in [call.version, 4, call.version] {
            assert_eq!(reopened.retire_expired_adaptive_local_adoption(
                &f.leader, request.review_id, version, adoption.continuation_deadline_ms + 1,
            ).unwrap(), retired);
            assert_eq!(rows(&reopened), before);
        }
        assert_eq!(reopened.adaptive_leadership_local_adoption(
            &request.tenant_id, request.review_id,
        ).unwrap(), Some(adoption));
    }
}

#[test]
fn local_adoption_conflicting_request_issuer_and_attached_authority_never_write() {
    for unknown in [true, false] {
        let (f, epoch, call, known, request, issued) = local_adoption_source(unknown);
        let operator = recovery_operator(&request.tenant_id);
        let (_, adoption) = f.store.authorize_adaptive_leadership_local_adoption(
            &operator, &request, issued,
        ).unwrap();
        let result = local_adoption_result(&call, &known, &adoption);
        let before = rows(&f.store);
        for field in ["operation", "response", "decision", "expiry", "repair"] {
            let mut changed = request.clone();
            match field {
                "operation" => changed.operation_id = Uuid::new_v4(),
                "response" => changed.model_response_digest = "f".repeat(64),
                "decision" => {
                    if let AdaptiveLeadershipReviewDecisionKindV1::Continue { rationale, .. } = &mut changed.decision.decision {
                        *rationale = "A different response cannot replace the retained decision".into();
                    }
                }
                "expiry" => changed.expires_at_unix_ms += 1,
                _ => changed.repair_digest = "f".repeat(64),
            }
            assert_eq!(f.store.authorize_adaptive_leadership_local_adoption(
                &operator, &changed, issued + 1,
            ).unwrap_err().code, WorkflowErrorCode::IdempotencyConflict);
            assert_eq!(rows(&f.store), before);
        }
        let mut other_issuer = operator.clone();
        other_issuer.authority_generation += 1;
        assert_eq!(f.store.authorize_adaptive_leadership_local_adoption(
            &other_issuer, &request, issued + 1,
        ).unwrap_err().code, WorkflowErrorCode::IdempotencyConflict);
        let mut forged = adoption.clone();
        forged.request.repair_digest = "f".repeat(64);
        let forged_result = local_adoption_result(&call, &known, &forged);
        call.validate_completion_proposal(&forged_result).unwrap();
        assert!(f.store.complete_adaptive_leadership_review_call(
            &f.leader, &forged_result, issued + 1,
        ).is_err());
        let mut changed_response = result.clone();
        changed_response.model_response_digest = "f".repeat(64);
        assert!(f.store.complete_adaptive_leadership_review_call(
            &f.leader, &changed_response, issued + 1,
        ).is_err());
        assert_eq!(rows(&f.store), before);
        assert_eq!(session(&f), call.context.source_session);
        assert_eq!(recovery_call(&f, &epoch), call);
        assert_eq!(f.store.adaptive_leadership_local_adoption(
            &request.tenant_id, request.review_id,
        ).unwrap(), Some(adoption));
    }
}

#[test]
fn recovery_epoch_multiple_employee_calls_use_only_unspent_root_budget() {
    let (mut f, retired, now) = recovery_source();
    let source = session(&f);
    let remaining = source.grant.max_model_calls - source.model_calls;
    assert!(remaining > 1);
    let mut request = recovery_request(&mut f, &retired);
    request.max_additional_model_calls = remaining;
    f.context.evidence_refs.retain(|reference| !reference.starts_with("recovery-request:"));
    f.context.evidence_refs.push(format!("recovery-request:{}", request.canonical_digest().unwrap()));
    f.context.evidence_refs.sort();
    refresh_recovery_review(&mut f);
    let epoch = authorize_recovery(&f, &request, now);
    assert_eq!(epoch.binding().unwrap().max_additional_model_calls, remaining);
    let call = recovery_call(&f, &epoch);
    assert!(call.continuation_allowance(now + 2, now + 120_002, remaining + 1).is_err());
    let dispatched = f.store.claim_adaptive_leadership_review_call(&f.leader, &claim(&call), now + 1).unwrap();
    let result = recovery_continue_result_with_calls(&dispatched, now + 2, remaining);
    f.store.complete_adaptive_leadership_review_call(&f.leader, &result, now + 2).unwrap();
    let continued = session(&f);
    assert_eq!(continued.grant, source.grant);
    assert_eq!(continued.model_calls, source.model_calls);
    assert_eq!(continued.tool_calls, source.tool_calls);
    assert_eq!(continued.active_model_ceiling(), source.grant.max_model_calls);
    assert_eq!(continued.continuation.unwrap().authorizations.last().unwrap().additional_model_calls, remaining);
    assert_eq!(f.store.adaptive_leadership_review_calls(&request.tenant_id, request.session_id).unwrap().len(), 4);
}

#[test]
fn recovery_epoch_cannot_authorize_more_than_unspent_root_budget() {
    let (mut f, retired, now) = recovery_source();
    let source = session(&f);
    let mut request = recovery_request(&mut f, &retired);
    request.max_additional_model_calls = source.grant.max_model_calls - source.model_calls + 1;
    f.context.evidence_refs.retain(|reference| !reference.starts_with("recovery-request:"));
    f.context.evidence_refs.push(format!("recovery-request:{}", request.canonical_digest().unwrap()));
    f.context.evidence_refs.sort();
    refresh_recovery_review(&mut f);
    let before = rows(&f.store);
    assert!(f.store.authorize_adaptive_leadership_recovery_epoch(
        &recovery_operator(&request.tenant_id), &request, &f.grant, &f.context, now,
    ).is_err());
    assert_eq!(rows(&f.store), before);
    assert_eq!(session(&f), source);
}

#[test]
fn recovery_epoch_continuation_keeps_identity_and_historical_replay_after_head_change() {
    let (mut f, retired, now) = recovery_source();
    let request = recovery_request(&mut f, &retired);
    let epoch = authorize_recovery(&f, &request, now);
    let source = session(&f);
    let call = recovery_call(&f, &epoch);
    let dispatched = f.store.claim_adaptive_leadership_review_call(&f.leader, &claim(&call), now + 1).unwrap();
    let result = recovery_continue_result(&dispatched, now + 2);
    let completed = f.store.complete_adaptive_leadership_review_call(&f.leader, &result, now + 2).unwrap();
    let continued = session(&f);
    assert_eq!(continued.grant, source.grant);
    assert_eq!(continued.model_calls, source.model_calls);
    assert_eq!(continued.tool_calls, source.tool_calls);
    assert_eq!(continued.version, source.version + 1);
    assert!(continued.is_abandoned_model_effect(request.unknown_effect.as_ref().unwrap()));
    let before = rows(&f.store);
    let reopened = WorkflowStore::open(&f.path).unwrap();
    let operator = recovery_operator(&request.tenant_id);
    assert_eq!(reopened.authorize_adaptive_leadership_recovery_epoch(
        &operator, &request, &f.grant, &f.context, request.expires_at_unix_ms + 1,
    ).unwrap(), (true, epoch));
    assert_eq!(reopened.complete_adaptive_leadership_review_call(
        &f.leader, &result, request.expires_at_unix_ms + 1,
    ).unwrap(), completed);
    assert_eq!(rows(&reopened), before);
}

#[test]
fn recovery_epoch_rejects_window_expansion_and_expired_fresh_completion() {
    let (mut f, retired, now) = recovery_source();
    let mut request = recovery_request(&mut f, &retired);
    request.max_window_ms = 1_000;
    f.context.evidence_refs.retain(|reference| !reference.starts_with("recovery-request:"));
    f.context.evidence_refs.push(format!("recovery-request:{}", request.canonical_digest().unwrap()));
    f.context.evidence_refs.sort();
    refresh_recovery_review(&mut f);
    let epoch = authorize_recovery(&f, &request, now);
    let call = recovery_call(&f, &epoch);
    let dispatched = f.store.claim_adaptive_leadership_review_call(&f.leader, &claim(&call), now + 1).unwrap();
    assert!(dispatched.continuation_allowance(now + 2, now + 120_002, 1).is_err());
    let mut expanded = completion(&dispatched, false);
    expanded.decision = AdaptiveLeadershipReviewDecisionV1 {
        schema_version: 2,
        decision: AdaptiveLeadershipReviewDecisionKindV1::Continue {
            additional_model_calls: 1,
            window_ms: 120_000,
            rationale: "Reject expansion beyond the exact recovery window".into(),
            evidence_refs: vec![dispatched.context.evidence_refs[0].clone()],
        },
    };
    assert!(f.store.validate_adaptive_leadership_recovery_decision(&dispatched, &expanded.decision).is_err());
    let before = rows(&f.store);
    assert!(f.store.complete_adaptive_leadership_review_call(&f.leader, &expanded, now + 2).is_err());
    let mut keep = completion(&dispatched, false);
    keep.decision = AdaptiveLeadershipReviewDecisionV1 {
        schema_version: 2,
        decision: AdaptiveLeadershipReviewDecisionKindV1::KeepUnknown {
            rationale: "Preserve the exact unresolved effect".into(),
            evidence_refs: vec![dispatched.context.evidence_refs[0].clone()],
        },
    };
    assert!(f.store.complete_adaptive_leadership_review_call(
        &f.leader, &keep, request.expires_at_unix_ms,
    ).is_err());
    assert_eq!(rows(&f.store), before);
}

#[test]
fn recovery_epoch_stripped_binding_cannot_dispatch_as_an_ordinary_review() {
    let (mut f, retired, now) = recovery_source();
    let request = recovery_request(&mut f, &retired);
    let epoch = authorize_recovery(&f, &request, now);
    let mut call = recovery_call(&f, &epoch);
    call.grant.recovery_epoch = None;
    persist_entity(&f.store, &call);
    let before = rows(&f.store);
    assert!(f.store.adaptive_leadership_review_call(&request.tenant_id, epoch.review_id).is_err());
    assert!(f.store.claim_adaptive_leadership_review_call(&f.leader, &claim(&call), now + 1).is_err());
    assert_eq!(rows(&f.store), before);
}

#[test]
fn recovery_epoch_review_write_failure_rolls_back_authority_and_audits() {
    let (mut f, retired, now) = recovery_source();
    let request = recovery_request(&mut f, &retired);
    f.store.connection.lock().unwrap().execute_batch(
        "CREATE TRIGGER reject_recovery_review BEFORE INSERT ON company_entities
         WHEN NEW.entity_kind='adaptive_leadership_review_call'
         BEGIN SELECT RAISE(ABORT, 'injected recovery review failure'); END;",
    ).unwrap();
    let before = rows(&f.store);
    let operator = recovery_operator(&request.tenant_id);
    assert!(f.store.authorize_adaptive_leadership_recovery_epoch(
        &operator, &request, &f.grant, &f.context, now,
    ).is_err());
    assert_eq!(rows(&f.store), before);
    assert!(f.store.adaptive_leadership_recovery_epoch(&request.tenant_id, request.session_id).unwrap().is_none());
    f.store.connection.lock().unwrap().execute_batch("DROP TRIGGER reject_recovery_review;").unwrap();
    authorize_recovery(&f, &request, now);
}

#[test]
fn recovery_epoch_rejects_non_leadership_operator_and_unknown_tool_source() {
    let (mut f, retired, now) = recovery_source();
    let mut request = recovery_request(&mut f, &retired);
    let operator = recovery_operator(&request.tenant_id);
    let mut non_leadership = operator.clone();
    non_leadership.role = CompanyRoleV1::Gaia;
    let before = rows(&f.store);
    assert!(f.store.authorize_adaptive_leadership_recovery_epoch(
        &non_leadership, &request, &f.grant, &f.context, now,
    ).is_err());
    assert_eq!(rows(&f.store), before);
    let tool: sentinel_common::WorkbenchTool = serde_json::from_value(serde_json::json!({
        "tool": "list_directory", "path": ".", "after": null, "max_entries": 16,
    })).unwrap();
    f.context.source_session.cursor = AdaptiveCursorV1::ToolUnknown {
        effect: request.unknown_effect.clone().unwrap(),
        tool_digest: crate::adaptive_tool_digest(&tool).unwrap(),
        tool,
    };
    request.session_digest = adaptive_leadership_recovery_session_digest(&f.context.source_session).unwrap();
    f.context.evidence_refs.retain(|reference| !reference.starts_with("recovery-request:"));
    f.context.evidence_refs.push(format!("recovery-request:{}", request.canonical_digest().unwrap()));
    f.context.evidence_refs.sort();
    refresh_recovery_review(&mut f);
    assert!(f.store.authorize_adaptive_leadership_recovery_epoch(
        &operator, &request, &f.grant, &f.context, now,
    ).is_err());
    assert_eq!(rows(&f.store), before);
}

#[test]
fn recovery_epoch_non_leadership_operator_cannot_replay_exact_request() {
    let (mut f, retired, now) = recovery_source();
    let request = recovery_request(&mut f, &retired);
    let epoch = authorize_recovery(&f, &request, now);
    let operator = recovery_operator(&request.tenant_id);
    let before = rows(&f.store);
    for role in [CompanyRoleV1::Gaia, CompanyRoleV1::Developer, CompanyRoleV1::Qa] {
        let mut denied = operator.clone();
        denied.role = role;
        assert_eq!(f.store.authorize_adaptive_leadership_recovery_epoch(
            &denied, &request, &f.grant, &f.context, request.expires_at_unix_ms + 1,
        ).unwrap_err().code, WorkflowErrorCode::AuthorityConflict);
    }
    assert_eq!(f.store.authorize_adaptive_leadership_recovery_epoch(
        &operator, &request, &f.grant, &f.context, request.expires_at_unix_ms + 1,
    ).unwrap(), (true, epoch));
    assert_eq!(rows(&f.store), before);
}

#[test]
fn recovery_epoch_decision_preflight_is_read_only_and_historical() {
    let (mut f, retired, now) = recovery_source();
    let mut request = recovery_request(&mut f, &retired);
    request.max_window_ms = 1_000;
    f.context.evidence_refs.retain(|reference| !reference.starts_with("recovery-request:"));
    f.context.evidence_refs.push(format!("recovery-request:{}", request.canonical_digest().unwrap()));
    f.context.evidence_refs.sort();
    refresh_recovery_review(&mut f);
    let epoch = authorize_recovery(&f, &request, now);
    let call = recovery_call(&f, &epoch);
    let allowed = AdaptiveLeadershipReviewDecisionV1 {
        schema_version: 2,
        decision: AdaptiveLeadershipReviewDecisionKindV1::Continue {
            additional_model_calls: 1,
            window_ms: 1_000,
            rationale: "Use only the explicitly authorized bounded continuation".into(),
            evidence_refs: vec![call.context.evidence_refs[0].clone()],
        },
    };
    let mut expanded_calls = allowed.clone();
    if let AdaptiveLeadershipReviewDecisionKindV1::Continue { additional_model_calls, .. } = &mut expanded_calls.decision {
        *additional_model_calls = 2;
    }
    let mut expanded_window = allowed.clone();
    if let AdaptiveLeadershipReviewDecisionKindV1::Continue { window_ms, .. } = &mut expanded_window.decision {
        *window_ms = 1_001;
    }
    change_project(&f, request.expires_at_unix_ms + 1);
    let before = rows(&f.store);
    assert!(f.store.validate_adaptive_leadership_recovery_decision(&call, &allowed).is_ok());
    assert!(f.store.validate_adaptive_leadership_recovery_decision(&call, &expanded_calls).is_err());
    assert!(f.store.validate_adaptive_leadership_recovery_decision(&call, &expanded_window).is_err());
    assert_eq!(rows(&f.store), before);
}

fn admission_repair_fixture(mixed_head: bool) -> (Fixture, u64) {
    admission_repair_fixture_with_retained_allowance(mixed_head, false)
}

fn admission_repair_fixture_with_retained_allowance(
    mixed_head: bool,
    retain_allowance: bool,
) -> (Fixture, u64) {
    let (mut f, retired, mut now) = recovery_source_from_fixture(
        continuation_fixture_with_calls(false, false, 16),
    );
    let request = recovery_request(&mut f, &retired);
    let epoch = authorize_recovery(&f, &request, now);
    let call = recovery_call(&f, &epoch);
    let dispatched = f.store.claim_adaptive_leadership_review_call(&f.leader, &claim(&call), now + 1).unwrap();
    let result = continue_result_with_window_at(&dispatched, 120_000, now + 2);
    f.store.complete_adaptive_leadership_review_call(&f.leader, &result, now + 2).unwrap();
    observe_budget_inspection(&f, session(&f), now + 3);
    now += 7;
    if retain_allowance {
        retain_discovery_allowance_in_correction(&f, now);
        now += 4;
    }
    if mixed_head {
        budget_context(&mut f, now, "repair-prior-head-completed");
        let call = dispatch_budget(&f, now);
        let result = budget_result(&call, 2, now + 2);
        f.store.complete_adaptive_leadership_review_call(&f.leader, &result, now + 2).unwrap();
        now = result.continuation.as_ref().unwrap().deadline_ms;
    }
    let operator = recovery_operator(&f.leader.tenant_id);
    for round in 0..3 {
        let count = if round == 0 && mixed_head { 2 } else { 3 };
        for index in 0..count {
            budget_context(&mut f, now, &format!("repair-retired-{round}-{index}"));
            let call = f.store.authorize_adaptive_leadership_review_call(
                &f.leader, Uuid::new_v4(), &format!("repair-review-{round}-{index}"),
                &f.grant, &f.context, now,
            ).unwrap();
            now = call.grant.expires_at_unix_ms;
            f.store.expire_adaptive_leadership_review_call(&f.leader, call.grant.review_id, 1, now).unwrap();
            now += 1;
        }
        if round < 2 {
            budget_context(&mut f, now, "repair-exhausted-extension");
            if round == 0 {
                f.store.record_adaptive_budget_window_limit(&f.leader, &f.grant, &f.context, now).unwrap();
            }
            let request = if round == 0 {
                f.store.budget_review_extension_draft(&operator, &f.grant.project_id,
                    f.grant.session_id, Uuid::new_v4(), 3, "repair-original-extension", now + 3_600_000, now)
            } else {
                f.store.budget_review_extension_successor_draft(&operator, &f.grant.project_id,
                    f.grant.session_id, Uuid::new_v4(), 3, "repair-successor-extension", now + 3_600_000, now)
            }.unwrap();
            f.store.authorize_budget_review_extension(&operator, &request, now).unwrap();
            now += 1;
        }
    }
    (f, now)
}

fn retain_discovery_allowance_in_correction(f: &Fixture, now: u64) {
    let authority = &f.grant.assignee_authority;
    let mut project = f
        .store
        .company_project(&f.leader.tenant_id, &f.grant.project_id)
        .unwrap()
        .unwrap();
    let prior = project.subscription_call.clone().unwrap();
    let workspace_id = format!("{}:{}", project.project_id.0, f.grant.work_item_id.0);
    let plan = ExecutionPlanV1 {
        schema_version: 1,
        plan_id: Uuid::new_v4(),
        tenant_id: authority.tenant_id.clone(),
        project_id: authority.project_id.clone(),
        work_item_id: authority.work_item_id.clone(),
        agent_id: authority.agent_id,
        workspace_id: workspace_id.clone(),
        assignment_version: authority.assignment_version,
        assignment_digest: authority.assignment_digest.clone(),
        organization_generation: authority.organization_generation,
        organization_digest: authority.organization_digest.clone(),
        principal: authority.principal.clone(),
        profile_id: authority.profile_id.clone(),
        profile_generation: authority.profile_generation,
        profile_digest: authority.profile_digest.clone(),
        runtime_key: authority.runtime_key.clone(),
        runtime_generation: authority.runtime_generation,
        runtime_digest: authority.runtime_digest.clone(),
        policy_generation: authority.policy_generation,
        policy_digest: authority.policy_digest.clone(),
        created_at_unix_ms: now,
        deadline_unix_ms: now + 1_000,
        request_digest: String::new(),
        steps: vec![ExecutionStepV1 {
            step_id: Uuid::new_v4(),
            invocation_id: Uuid::new_v4(),
            ordinal: 0,
            workspace_id,
            capabilities: BTreeSet::from(["file.inspect".into()]),
            inputs: vec![],
            command_policy: vec![],
            tool: ExecutionToolV1::InspectFile {
                path: "index.html".into(),
                max_bytes: 1_024,
            },
            outputs: vec![OutputExpectationV1 {
                name: "source".into(),
                kind: "source_tree".into(),
                required: true,
                digest_algorithm: "sha256".into(),
            }],
            artifacts: vec![],
            gate_expectation: GateExpectationV1 {
                profile_id: "web-work-item-qa-v1".into(),
                profile_generation: 1,
                profile_digest: DIGEST.into(),
                required_checks: BTreeSet::from(["check".into()]),
            },
            resource_bounds: ExecutionResourceBoundsV1 {
                wall_time_ms: 1_000,
                cpu_time_ms: 1_000,
                memory_bytes: 1024 * 1024,
                process_count: 1,
                file_bytes: 1_024,
                stdout_bytes: 1_024,
                stderr_bytes: 1_024,
            },
            deadline_unix_ms: now + 1_000,
        }],
    }
    .bind_digest()
    .unwrap();
    f.store.admit_plan(&plan, authority, now).unwrap();
    let pending = f.store.pending_executions(1).unwrap().remove(0);
    let failed = f
        .store
        .record_execution_observation(
            &pending,
            ExecutionReconcileState::Failed,
            authority,
            now + 1,
        )
        .unwrap();
    let work = &project.work_items[&f.grant.work_item_id];
    let blocked = CompanyWorkflowCommandV1::ApplyWorkTransition {
        project_id: project.project_id.clone(),
        expected_version: project.version,
        receipt: WorkTransitionReceiptV1 {
            schema_version: 1,
            project_id: project.project_id.clone(),
            work_item_id: f.grant.work_item_id.clone(),
            expected_project_version: project.version,
            expected_work_version: work.version,
            expected_assignment_version: authority.assignment_version,
            from_state: CompanyWorkStateV1::Assigned,
            to_state: CompanyWorkStateV1::Blocked,
            output_receipts: vec![],
            gate_receipt: None,
            phase_a_evidence_digest: DIGEST.into(),
            reason_ref: "discovery-fixture-execution-failed".into(),
            occurred_at_unix_ms: now + 2,
        },
    };
    let response = f
        .store
        .apply_company_command(&f.leader, Uuid::new_v4(), &blocked, now + 2)
        .unwrap();
    let CompanyWorkflowResponseV1::Project(updated) = response.response else {
        panic!("expected blocked project");
    };
    project = *updated;
    let correction = CompanyWorkflowCommandV1::RequestWorkCorrection {
        project_id: project.project_id.clone(),
        expected_version: project.version,
        work_item_id: f.grant.work_item_id.clone(),
        expected_work_version: project.work_items[&f.grant.work_item_id].version,
        execution_revision: crate::ExecutionRevisionV1::from_completed_work(&failed, DIGEST.into())
            .unwrap(),
        feedback_ref: "discovery-fixture-correction".into(),
        feedback: None,
        next_subscription_grant: None,
    };
    let response = f
        .store
        .apply_company_command(&f.leader, Uuid::new_v4(), &correction, now + 3)
        .unwrap();
    let CompanyWorkflowResponseV1::Project(updated) = response.response else {
        panic!("expected corrected project");
    };
    assert_eq!(updated.subscription_call.as_ref(), Some(&prior));
    assert_eq!(
        updated.work_corrections[0].previous_subscription_call.as_ref(),
        Some(&prior)
    );
}

fn admission_repair_request(f: &mut Fixture, now: u64) -> AdaptiveLeadershipRecoveryRequestV1 {
    let source = f.store.adaptive_leadership_admission_repair_source(
        &f.leader.tenant_id, f.grant.session_id,
    ).unwrap();
    budget_context(f, now, "explicit-admission-repair");
    let request = AdaptiveLeadershipRecoveryRequestV1 {
        schema_version: 3, operation_id: Uuid::new_v4(), tenant_id: f.leader.tenant_id.clone(),
        project_id: f.grant.project_id.clone(), work_item_id: f.grant.work_item_id.clone(),
        session_id: f.grant.session_id, expected_project_version: source.project.version,
        expected_session_version: source.session.version, session_head_digest: source.session_head_digest.clone(),
        session_digest: adaptive_leadership_recovery_session_digest(&source.session).unwrap(),
        project_digest: adaptive_leadership_recovery_project_digest(&source.project).unwrap(),
        unknown_effect: None, sealed_unknown_proof_digest: None, blocked_subject: None,
        admission_repair: Some(crate::AdaptiveLeadershipAdmissionRepairV1 {
            schema_version: 1, source_digest: source.canonical_digest().unwrap(),
            // A test callback attestation, not a claim that mixed history had zero provider I/O.
            disposition_digest: "a".repeat(64),
            failed_release: source.legacy_epoch.request.release.clone(),
        }),
        prior_review_history_digest: crate::adaptive_leadership_admission_repair_history_digest(&source.calls).unwrap(),
        repair_digest: "b".repeat(64),
        release: AdaptiveRecoveryReleaseV1 { schema_version: 1, source_git_sha: "f".repeat(40),
            release_manifest_digest: "1".repeat(64), gateway_binary_digest: "2".repeat(64) },
        reason_ref: "explicit-once-admission-repair".into(), expires_at_unix_ms: now + 120_000,
        max_additional_model_calls: 2, max_window_ms: 120_000,
    };
    f.grant.schema_version = 4;
    f.context.evidence_refs.push(format!("recovery-request:{}", request.canonical_digest().unwrap()));
    f.context.evidence_refs.sort();
    rebind_budget_evidence(f);
    request
}

fn admission_repair_evidence(
    source: &crate::AdaptiveLeadershipAdmissionRepairSourceV1,
    request: &AdaptiveLeadershipRecoveryRequestV1,
) -> Result<crate::AdaptiveLeadershipAdmissionRepairEvidenceV1, WorkflowError> {
    let repair = request.admission_repair.as_ref().unwrap();
    assert_eq!(source.calls.len(), 13);
    assert_eq!(source.calls.iter().filter(|call| call.grant.schema_version == 3).count(), 9);
    assert_eq!(source.calls.iter().filter(|call| call.grant.schema_version == 2).count(), 4);
    assert!(source.calls.iter().any(|call| call.dispatch.is_some() && call.continuation.is_some()));
    Ok(crate::AdaptiveLeadershipAdmissionRepairEvidenceV1 {
        source_digest: source.canonical_digest()?, disposition_digest: repair.disposition_digest.clone(),
        repair_digest: request.repair_digest.clone(), release: request.release.clone(),
        failed_release: repair.failed_release.clone(),
        attested_review_ids: vec![source.qualifying_review_ids[0]],
    })
}

fn authorize_admission_repair(
    f: &Fixture, request: &AdaptiveLeadershipRecoveryRequestV1, now: u64,
) -> crate::AdaptiveLeadershipAdmissionRepairEpochV1 {
    let (replayed, receipt) = f.store.authorize_adaptive_leadership_admission_repair(
        &recovery_operator(&f.leader.tenant_id), request, &f.grant, &f.context, now, admission_repair_evidence,
    ).unwrap();
    assert!(!replayed);
    receipt
}

fn assert_admission_repair_heap_layout_preserves_wire(
    receipt: &crate::AdaptiveLeadershipAdmissionRepairEpochV1,
) {
    #[derive(serde::Serialize)]
    struct InlineSource<'a> {
        schema_version: u16,
        session: &'a crate::AdaptiveSessionV1,
        session_head_digest: &'a str,
        project: &'a ProjectV1,
        calls: &'a [AdaptiveLeadershipReviewCallV1],
        original_extension: &'a crate::AdaptiveBudgetReviewExtensionReceiptV1,
        successor_extension: &'a crate::AdaptiveBudgetReviewExtensionReceiptV1,
        legacy_epoch: &'a AdaptiveLeadershipRecoveryEpochV1,
        qualifying_review_ids: &'a [Uuid],
        continuation_reviews: &'a [AdaptiveLeadershipReviewCallV1],
    }
    #[derive(serde::Serialize)]
    struct InlineReceipt<'a> {
        epoch: &'a AdaptiveLeadershipRecoveryEpochV1,
        source: InlineSource<'a>,
        evidence: &'a crate::AdaptiveLeadershipAdmissionRepairEvidenceV1,
    }
    let source = &receipt.source;
    let inline = InlineReceipt {
        epoch: &receipt.epoch,
        source: InlineSource {
            schema_version: source.schema_version,
            session: &source.session,
            session_head_digest: &source.session_head_digest,
            project: &source.project,
            calls: &source.calls,
            original_extension: &source.original_extension,
            successor_extension: &source.successor_extension,
            legacy_epoch: &source.legacy_epoch,
            qualifying_review_ids: &source.qualifying_review_ids,
            continuation_reviews: &source.continuation_reviews,
        },
        evidence: &receipt.evidence,
    };
    assert!(std::mem::size_of::<crate::AdaptiveLeadershipAdmissionRepairEpochV1>() < 1024);
    assert_eq!(serde_json::to_vec(receipt).unwrap(), serde_json::to_vec(&inline).unwrap());
    assert_eq!(source.canonical_digest().unwrap(), canonical_sha256(
        "sentinel.workflow.adaptive-leadership-admission-repair-source.v1", &inline.source,
    ).unwrap());
    assert_eq!(serde_json::from_slice::<crate::AdaptiveLeadershipAdmissionRepairEpochV1>(
        &serde_json::to_vec(&inline).unwrap(),
    ).unwrap(), *receipt);
}

#[test]
fn admission_repair_preserves_mixed_inventory_occupied_epoch_and_root_without_refund() {
    for mixed_head in [false, true] {
        let (mut f, now) = admission_repair_fixture(mixed_head);
        let request = admission_repair_request(&mut f, now);
        let before_session = session(&f);
        let before_project = f.context.source_project.clone();
        let legacy = f.store.adaptive_leadership_recovery_epoch(&f.leader.tenant_id, f.grant.session_id).unwrap().unwrap();
        let receipt = authorize_admission_repair(&f, &request, now);
        assert_admission_repair_heap_layout_preserves_wire(&receipt);
        assert_eq!(receipt.source.qualifying_review_ids.len(), if mixed_head { 8 } else { 9 });
        assert_eq!(receipt.source.continuation_reviews.len(), if mixed_head { 2 } else { 1 });
        assert_eq!(session(&f), before_session);
        assert_eq!(session(&f).grant.max_model_calls, 16);
        assert_eq!(session(&f).model_calls, 2);
        assert!(f.store.company_project(&f.leader.tenant_id, &f.grant.project_id).unwrap() == Some(before_project));
        assert_eq!(f.store.adaptive_leadership_recovery_epoch(&f.leader.tenant_id, f.grant.session_id).unwrap(), Some(legacy));
        assert_ne!(receipt.epoch.epoch_key, receipt.source.legacy_epoch.epoch_key);
        let calls = f.store.adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id).unwrap();
        assert_eq!(calls.len(), 14);
        assert_eq!(calls.iter().filter(|call| call.grant.schema_version == 3).count(), 9);
        assert_eq!(calls.iter().filter(|call| call.grant.schema_version == 4).count(), 1);
        assert_eq!(budget_review_extension::review_counts(&calls, session(&f).version),
            (9, if mixed_head { 8 } else { 9 }));
        assert!(receipt.source.calls.iter().all(|prior| calls.contains(prior)));
        assert!(f.store.authorize_adaptive_leadership_recovery_epoch(
            &recovery_operator(&f.leader.tenant_id), &request, &f.grant, &f.context, now,
        ).is_err());
        assert!(f.store.authorize_adaptive_leadership_review_call(
            &f.leader, request.operation_id, "ordinary-cannot-issue-repair", &f.grant, &f.context, now,
        ).is_err());
    }
}

#[test]
fn admission_repair_exact_replay_after_expiry_reopen_and_release_change_never_renews() {
    let (mut f, now) = admission_repair_fixture(false);
    let request = admission_repair_request(&mut f, now);
    let receipt = authorize_admission_repair(&f, &request, now);
    let call = f.store.adaptive_leadership_review_call(&f.leader.tenant_id, receipt.epoch.review_id).unwrap().unwrap();
    f.store.expire_adaptive_leadership_review_call(&f.leader, call.grant.review_id, 1, request.expires_at_unix_ms).unwrap();
    change_project(&f, request.expires_at_unix_ms + 1);
    let reopened = WorkflowStore::open(&f.path).unwrap();
    let before = rows(&reopened);
    let (replayed, prior) = reopened.authorize_adaptive_leadership_admission_repair(
        &recovery_operator(&f.leader.tenant_id), &request, &f.grant, &f.context,
        request.expires_at_unix_ms + 2, |_, _| panic!("historical replay must not verify or grant again"),
    ).unwrap();
    assert!(replayed && prior == receipt);
    assert_eq!(rows(&reopened), before);
    let mut changed = request.clone();
    changed.release.source_git_sha = "9".repeat(40);
    assert!(reopened.authorize_adaptive_leadership_admission_repair(
        &recovery_operator(&f.leader.tenant_id), &changed, &f.grant, &f.context,
        request.expires_at_unix_ms + 2, |_, _| panic!("occupied slot must conflict before callback"),
    ).is_err());
    let mut denied = recovery_operator(&f.leader.tenant_id);
    denied.role = CompanyRoleV1::Sales;
    assert!(reopened.authorize_adaptive_leadership_admission_repair(
        &denied, &request, &f.grant, &f.context, now,
        |_, _| panic!("invalid operator must not reach callback"),
    ).is_err());
    assert_eq!(rows(&reopened), before);
}

#[test]
fn admission_repair_failed_or_mismatched_trusted_verification_writes_nothing() {
    let (mut f, now) = admission_repair_fixture(true);
    let request = admission_repair_request(&mut f, now);
    let before = rows(&f.store);
    for mismatch in 0..9 {
        let result = f.store.authorize_adaptive_leadership_admission_repair(
            &recovery_operator(&f.leader.tenant_id), &request, &f.grant, &f.context, now,
            |source, request| {
                if mismatch == 0 { return Err(transition()); }
                let mut evidence = admission_repair_evidence(source, request)?;
                match mismatch {
                    1 => evidence.source_digest = "8".repeat(64),
                    2 => evidence.disposition_digest = "8".repeat(64),
                    3 => evidence.repair_digest = "8".repeat(64),
                    4 => evidence.release.gateway_binary_digest = "8".repeat(64),
                    5 => evidence.failed_release.source_git_sha = "8".repeat(40),
                    6 => evidence.attested_review_ids.clear(),
                    7 => evidence.attested_review_ids = vec![source.legacy_epoch.review_id],
                    _ => evidence.attested_review_ids.push(evidence.attested_review_ids[0]),
                }
                Ok(evidence)
            },
        );
        assert!(result.is_err());
        assert_eq!(rows(&f.store), before);
    }
    authorize_admission_repair(&f, &request, now);
}

#[test]
fn admission_repair_dispatch_defer_and_expired_continuation_retirement_preserve_slot() {
    for retire_proposal in [false, true] {
        let (mut f, now) = admission_repair_fixture(false);
        let request = admission_repair_request(&mut f, now);
        let receipt = authorize_admission_repair(&f, &request, now);
        let call = f.store.adaptive_leadership_review_call(&f.leader.tenant_id, receipt.epoch.review_id).unwrap().unwrap();
        let dispatched = f.store.claim_adaptive_leadership_review_call(&f.leader, &claim(&call), now + 1).unwrap();
        let source = session(&f);
        if retire_proposal {
            let mut result = budget_result(&dispatched, 2, now + 2);
            result.decision.schema_version = 4;
            let audit = adaptive_leadership_continuation_audit_id(dispatched.grant.review_id,
                &result.request_digest, &result.model_response_digest, &result.decision).unwrap();
            result.resolution_event_id = Some(audit);
            result.continuation.as_mut().unwrap().resolution_event_id = audit;
            let retired = f.store.retire_expired_adaptive_continuation_call(
                &f.leader, &result, now + 120_002,
            ).unwrap();
            assert_eq!(retired.version, 4);
            assert!(retired.decision.is_none() && retired.continuation.is_none());
        } else {
            let mut result = completion(&dispatched, false);
            result.decision = AdaptiveLeadershipReviewDecisionV1 {
                schema_version: 4,
                decision: AdaptiveLeadershipReviewDecisionKindV1::DeferBudget {
                    rationale: "Explicit repair review declined further spending".into(),
                    evidence_refs: vec![dispatched.context.evidence_refs[0].clone()],
                },
            };
            let completed = f.store.complete_adaptive_leadership_review_call(&f.leader, &result, now + 2).unwrap();
            assert_eq!(completed.version, 3);
            assert!(completed.continuation.is_none());
        }
        assert_eq!(session(&f), source);
        assert!(f.store.adaptive_leadership_admission_repair_epoch(&f.leader.tenant_id, f.grant.session_id).unwrap() == Some(receipt));
    }
}

#[test]
fn admission_repair_only_real_model_continuation_changes_window_and_reopens() {
    let (mut f, now) = admission_repair_fixture(false);
    let request = admission_repair_request(&mut f, now);
    let receipt = authorize_admission_repair(&f, &request, now);
    let call = f.store.adaptive_leadership_review_call(&f.leader.tenant_id, receipt.epoch.review_id).unwrap().unwrap();
    let dispatched = f.store.claim_adaptive_leadership_review_call(&f.leader, &claim(&call), now + 1).unwrap();
    let source = session(&f);
    let mut result = budget_result(&dispatched, 2, now + 2);
    result.decision.schema_version = 4;
    let audit = adaptive_leadership_continuation_audit_id(dispatched.grant.review_id,
        &result.request_digest, &result.model_response_digest, &result.decision).unwrap();
    result.resolution_event_id = Some(audit);
    result.continuation.as_mut().unwrap().resolution_event_id = audit;
    let completed = f.store.complete_adaptive_leadership_review_call(&f.leader, &result, now + 2).unwrap();
    let continued = session(&f);
    assert_eq!(continued.grant, source.grant);
    assert_eq!(continued.model_calls, source.model_calls);
    assert_eq!(continued.tool_calls, source.tool_calls);
    assert_eq!(continued.active_model_ceiling(), source.model_calls + 2);
    assert_eq!(continued.continuation.as_ref().unwrap().authorizations.len(), 2);
    let reopened = WorkflowStore::open(&f.path).unwrap();
    assert_eq!(reopened.adaptive_leadership_review_call(&f.leader.tenant_id, call.grant.review_id).unwrap(), Some(completed));
    assert!(reopened.company_project(&f.leader.tenant_id, &f.grant.project_id).is_ok());
    assert!(reopened.adaptive_leadership_admission_repair_epoch(&f.leader.tenant_id, f.grant.session_id).unwrap() == Some(receipt));
}

fn company_discovery_fixture() -> (Fixture, ProjectV1, AdaptiveLeadershipReviewCallV1) {
    let (mut f, now) = admission_repair_fixture_with_retained_allowance(true, true);
    let request = admission_repair_request(&mut f, now);
    let receipt = authorize_admission_repair(&f, &request, now);
    let call = f
        .store
        .adaptive_leadership_review_call(&f.leader.tenant_id, receipt.epoch.review_id)
        .unwrap()
        .unwrap();
    let dispatched = f
        .store
        .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), now + 1)
        .unwrap();
    let mut result = budget_result(&dispatched, 2, now + 2);
    result.decision.schema_version = 4;
    let audit = adaptive_leadership_continuation_audit_id(
        dispatched.grant.review_id,
        &result.request_digest,
        &result.model_response_digest,
        &result.decision,
    )
    .unwrap();
    result.resolution_event_id = Some(audit);
    result.continuation.as_mut().unwrap().resolution_event_id = audit;
    f.store
        .complete_adaptive_leadership_review_call(&f.leader, &result, now + 2)
        .unwrap();
    assert_eq!(
        session(&f).continuation.as_ref().unwrap().authorizations.len(),
        3
    );
    let project = f
        .store
        .company_project(&f.leader.tenant_id, &f.grant.project_id)
        .unwrap()
        .unwrap();
    let retained = project.work_corrections[0]
        .previous_subscription_call
        .as_ref()
        .unwrap();
    assert_ne!(
        retained.allowance_id,
        project.subscription_call.as_ref().unwrap().allowance_id
    );
    let archived = receipt
        .source
        .continuation_reviews
        .iter()
        .find(|call| {
            call.continuation.as_ref().unwrap().provider_allowance_id == retained.allowance_id
        })
        .unwrap()
        .clone();
    (f, project, archived)
}

fn assert_company_discovery_proofs(store: &WorkflowStore, expected: &ProjectV1) {
    let before = rows(store);
    // Do not supply an outer scope: discovery must establish its own project arena.
    let (projects, scopes) =
        validation_scope::with_completed_validations(|| store.company_projects());
    assert_eq!(projects.unwrap(), vec![expected.clone()]);
    assert_eq!(scopes.len(), 1);
    assert_eq!(scopes[0].get("adaptive-journal"), Some(&1));
    assert_eq!(scopes[0].get("historical-project"), Some(&1));
    assert_eq!(scopes[0].get("governed-allowance"), Some(&2));
    assert_eq!(validation_scope::validations("adaptive-journal"), 0);
    assert!(store.connection.lock().unwrap().is_autocommit());
    assert_eq!(rows(store), before);
}

#[test]
fn company_projects_reuses_multi_allowance_proofs_only_after_exact_input_recheck() {
    let (f, project, _) = company_discovery_fixture();
    assert_company_discovery_proofs(&f.store, &project);
    let (projects, scopes) =
        validation_scope::with_completed_validations(|| f.store.company_projects());
    assert_eq!(projects.unwrap(), vec![project.clone()]);
    assert!(scopes.is_empty());
    assert!(f.store.connection.lock().unwrap().is_autocommit());
    let reopened = WorkflowStore::open(&f.path).unwrap();
    assert_company_discovery_proofs(&reopened, &project);
}

#[test]
fn company_project_cached_reads_keep_exact_tenant_project_and_absent_bindings() {
    let (f, project, _) = company_discovery_fixture();
    for _ in 0..2 {
        assert_eq!(f.store.company_project(&project.tenant_id, &project.project_id).unwrap(), Some(project.clone()));
        assert!(f.store.company_project(&TenantId("foreign-tenant".into()), &project.project_id).unwrap().is_none());
        assert!(f.store.company_project(&project.tenant_id, &ProjectId("missing-project".into())).unwrap().is_none());
    }
    assert!(f.store.connection.lock().unwrap().is_autocommit());
}

#[test]
fn leadership_history_cached_reads_keep_tenant_session_and_reject_resealed_corruption() {
    let (f, _, archived) = company_discovery_fixture();
    let tenant = &f.leader.tenant_id;
    let session_id = archived.grant.session_id;
    let missing_session_id = Uuid::new_v4();
    let expected = f.store.adaptive_leadership_review_calls(tenant, session_id).unwrap();
    assert!(!expected.is_empty());
    for _ in 0..2 {
        assert_eq!(f.store.adaptive_leadership_review_calls(tenant, session_id).unwrap(), expected);
        assert!(f.store.adaptive_leadership_review_calls(&TenantId("foreign-tenant".into()), session_id).unwrap().is_empty());
        assert!(f.store.adaptive_leadership_review_calls(tenant, missing_session_id).unwrap().is_empty());
    }
    let mut unbound = archived.clone();
    assert!(unbound.grant.recovery_epoch.take().is_some());
    assert!(unbound.validate_entity().is_err());
    {
        let mut connection = f.store.connection.lock().unwrap();
        let transaction = connection.transaction().unwrap();
        put_entity(&transaction, tenant, KIND, &unbound.review_key, unbound.version, &unbound).unwrap();
        transaction.commit().unwrap();
    }
    let before = rows(&f.store);
    let reopened = WorkflowStore::open(&f.path).unwrap();
    for store in [&f.store, &reopened] {
        assert!(store.adaptive_leadership_review_calls(tenant, session_id).is_err());
        assert!(store.adaptive_leadership_review_calls(tenant, missing_session_id).is_err());
        assert!(store.connection.lock().unwrap().is_autocommit());
        assert_eq!(rows(store), before);
    }
}

#[test]
fn leadership_history_cached_reads_preserve_outer_transaction_changes_and_rollback() {
    let (f, _, archived) = company_discovery_fixture();
    let tenant = &f.leader.tenant_id;
    let session_id = archived.grant.session_id;
    let expected = f.store.adaptive_leadership_review_calls(tenant, session_id).unwrap();
    let before = rows(&f.store);
    let mut unbound = archived.clone();
    assert!(unbound.grant.recovery_epoch.take().is_some());
    let payload = encode(&unbound).unwrap();
    let payload_digest = bytes_digest("sentinel.workflow.company-entity-row.v1", &payload).unwrap();
    for (begin, rollback) in [
        ("BEGIN", "ROLLBACK"),
        ("SAVEPOINT caller", "ROLLBACK TO caller; RELEASE caller"),
    ] {
        {
            let connection = f.store.connection.lock().unwrap();
            connection.execute_batch(begin).unwrap();
            assert_eq!(connection.execute(
                "UPDATE company_entities SET payload=?1,payload_digest=?2 WHERE tenant_id=?3 AND entity_kind=?4 AND entity_id=?5",
                params![payload, payload_digest, tenant.0, KIND, unbound.review_key],
            ).unwrap(), 1);
        }
        assert!(f.store.adaptive_leadership_review_calls(tenant, session_id).is_err());
        {
            let connection = f.store.connection.lock().unwrap();
            assert!(!connection.is_autocommit());
            connection.execute_batch(rollback).unwrap();
        }
        assert_eq!(rows(&f.store), before);
        for _ in 0..2 {
            assert_eq!(f.store.adaptive_leadership_review_calls(tenant, session_id).unwrap(), expected);
        }
    }
}

#[test]
fn company_projects_rejects_resealed_archived_receipt_after_warm_read_and_reopen() {
    let (f, project, archived) = company_discovery_fixture();
    assert_company_discovery_proofs(&f.store, &project);
    let mut unbound = archived.clone();
    assert!(unbound.grant.recovery_epoch.take().is_some());
    assert!(unbound.validate_entity().is_err());
    {
        let mut connection = f.store.connection.lock().unwrap();
        let transaction = connection.transaction().unwrap();
        put_entity(
            &transaction,
            &f.leader.tenant_id,
            KIND,
            &unbound.review_key,
            unbound.version,
            &unbound,
        )
        .unwrap();
        transaction.commit().unwrap();
    }
    let before = rows(&f.store);
    let reopened = WorkflowStore::open(&f.path).unwrap();
    for store in [&f.store, &reopened] {
        let (projects, scopes) =
            validation_scope::with_completed_validations(|| store.company_projects());
        assert!(projects.is_err());
        assert!(scopes.is_empty());
        assert_eq!(validation_scope::validations("adaptive-journal"), 0);
        assert!(store.connection.lock().unwrap().is_autocommit());
        assert_eq!(rows(store), before);
    }
    {
        let mut connection = f.store.connection.lock().unwrap();
        let transaction = connection.transaction().unwrap();
        put_entity(
            &transaction,
            &f.leader.tenant_id,
            KIND,
            &archived.review_key,
            archived.version,
            &archived,
        )
        .unwrap();
        transaction.commit().unwrap();
    }
    assert_company_discovery_proofs(&f.store, &project);
    assert_company_discovery_proofs(&reopened, &project);
}

#[test]
fn admission_repair_concurrent_same_request_consumes_one_permanent_slot_and_verifies_once() {
    let (mut f, now) = admission_repair_fixture(true);
    let request = admission_repair_request(&mut f, now);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let verified = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let workers: Vec<_> = (0..2).map(|_| {
        let store = WorkflowStore::open(&f.path).unwrap();
        let operator = recovery_operator(&f.leader.tenant_id);
        let request = request.clone();
        let grant = f.grant.clone();
        let context = f.context.clone();
        let barrier = barrier.clone();
        let verified = verified.clone();
        std::thread::spawn(move || {
            barrier.wait();
            store.authorize_adaptive_leadership_admission_repair(
                &operator, &request, &grant, &context, now, |source, request| {
                    verified.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    admission_repair_evidence(source, request)
                },
            ).unwrap()
        })
    }).collect();
    let results: Vec<_> = workers.into_iter().map(|worker| worker.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|(replayed, _)| !replayed).count(), 1);
    assert_eq!(verified.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(results[0].1 == results[1].1);
    let calls = f.store.adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id).unwrap();
    assert_eq!(calls.len(), 14);
    assert_eq!(calls.iter().filter(|call| call.grant.schema_version == 4).count(), 1);
}

#[test]
fn admission_repair_event_failure_rolls_back_and_fresh_retry_verifies_again() {
    let (mut f, now) = admission_repair_fixture(false);
    let request = admission_repair_request(&mut f, now);
    let before = rows(&f.store);
    f.store.connection.lock().unwrap().execute_batch(
        "CREATE TRIGGER reject_admission_repair BEFORE INSERT ON company_events
         WHEN NEW.event_type='adaptive_leadership_admission_repair_authorized'
         BEGIN SELECT RAISE(ABORT,'repair event rejected'); END;",
    ).unwrap();
    assert!(f.store.authorize_adaptive_leadership_admission_repair(
        &recovery_operator(&f.leader.tenant_id), &request, &f.grant, &f.context, now, admission_repair_evidence,
    ).is_err());
    assert_eq!(rows(&f.store), before);
    assert!(f.store.adaptive_leadership_admission_repair_epoch(&f.leader.tenant_id, f.grant.session_id).unwrap().is_none());
    f.store.connection.lock().unwrap().execute_batch("DROP TRIGGER reject_admission_repair").unwrap();
    authorize_admission_repair(&f, &request, now);
}

#[test]
fn admission_repair_disposition_gate_permits_nonretired_decision_absent_continuation() {
    let (f, _) = admission_repair_fixture(true);
    let source = f.store.adaptive_leadership_admission_repair_source(
        &f.leader.tenant_id, f.grant.session_id,
    ).unwrap();
    let mut call = source.continuation_reviews.iter()
        .find(|call| call.grant.schema_version == 3).unwrap().clone();
    assert!(call.retired_at_unix_ms.is_none());
    call.decision = None;
    assert!(super::super::recovery::has_repair_disposition(&call));
    // This gate cannot substitute for the typed receipt's independent model proof.
    assert!(call.validate_entity().is_err());
    call.continuation = None;
    assert!(!super::super::recovery::has_repair_disposition(&call));
}

#[test]
fn admission_repair_corrupted_continuation_denied_before_trusted_verifier() {
    let (mut f, now) = admission_repair_fixture(true);
    let request = admission_repair_request(&mut f, now);
    let source = f.store.adaptive_leadership_admission_repair_source(
        &f.leader.tenant_id, f.grant.session_id,
    ).unwrap();
    let original = source.continuation_reviews.iter()
        .find(|call| call.grant.schema_version == 3).unwrap();
    let mut changed = original.clone();
    changed.continuation.as_mut().unwrap().provider_authority_digest = "0".repeat(64);
    {
        let mut connection = f.store.connection.lock().unwrap();
        let transaction = connection.transaction().unwrap();
        put_entity(&transaction, &f.leader.tenant_id, KIND, &original.review_key,
            original.version, &changed).unwrap();
        transaction.commit().unwrap();
    }
    let before = rows(&f.store);
    assert!(f.store.adaptive_leadership_admission_repair_source(
        &f.leader.tenant_id, f.grant.session_id,
    ).is_err());
    assert!(f.store.authorize_adaptive_leadership_admission_repair(
        &recovery_operator(&f.leader.tenant_id), &request, &f.grant, &f.context, now,
        |_, _| panic!("corrupt continuation must fail before verifier"),
    ).is_err());
    assert_eq!(rows(&f.store), before);
    {
        let mut connection = f.store.connection.lock().unwrap();
        let transaction = connection.transaction().unwrap();
        put_entity(&transaction, &f.leader.tenant_id, KIND, &original.review_key,
            original.version, original).unwrap();
        transaction.commit().unwrap();
    }
    let reopened = WorkflowStore::open(&f.path).unwrap();
    assert!(reopened.adaptive_leadership_admission_repair_source(
        &f.leader.tenant_id, f.grant.session_id,
    ).unwrap() == source);
}

#[test]
fn admission_repair_retained_continuation_corruption_fails_closed_without_cached_proof() {
    let (mut f, now) = admission_repair_fixture(true);
    let request = admission_repair_request(&mut f, now);
    let receipt = authorize_admission_repair(&f, &request, now);
    let completed = receipt.source.continuation_reviews.iter().find(|call| call.grant.schema_version == 3).unwrap();
    let digest: String = f.store.connection.lock().unwrap().query_row(
        "SELECT payload_digest FROM company_entities WHERE tenant_id=?1 AND entity_kind=?2 AND entity_id=?3",
        params![f.leader.tenant_id.0, KIND, completed.review_key], |row| row.get(0),
    ).unwrap();
    f.store.connection.lock().unwrap().execute(
        "UPDATE company_entities SET payload_digest='tampered' WHERE tenant_id=?1 AND entity_kind=?2 AND entity_id=?3",
        params![f.leader.tenant_id.0, KIND, completed.review_key],
    ).unwrap();
    let before = rows(&f.store);
    assert!(f.store.adaptive_leadership_admission_repair_epoch(&f.leader.tenant_id, f.grant.session_id).is_err());
    assert!(f.store.authorize_adaptive_leadership_admission_repair(
        &recovery_operator(&f.leader.tenant_id), &request, &f.grant, &f.context, now,
        |_, _| panic!("corrupt historical proof must fail before verifier"),
    ).is_err());
    assert_eq!(rows(&f.store), before);
    f.store.connection.lock().unwrap().execute(
        "UPDATE company_entities SET payload_digest=?4 WHERE tenant_id=?1 AND entity_kind=?2 AND entity_id=?3",
        params![f.leader.tenant_id.0, KIND, completed.review_key, digest],
    ).unwrap();
    let original = f.store.adaptive_leadership_review_call(&f.leader.tenant_id, receipt.epoch.review_id).unwrap().unwrap();
    for downgrade in [false, true] {
        let mut unbound = original.clone();
        unbound.grant.recovery_epoch = None;
        if downgrade {
            unbound.schema_version = 3;
            unbound.grant.schema_version = 3;
        }
        {
            let mut connection = f.store.connection.lock().unwrap();
            let transaction = connection.transaction().unwrap();
            put_entity(&transaction, &f.leader.tenant_id, KIND, &original.review_key, original.version, &unbound).unwrap();
            transaction.commit().unwrap();
        }
        assert!(f.store.adaptive_leadership_review_call(&f.leader.tenant_id, original.grant.review_id).is_err());
    }
    {
        let mut connection = f.store.connection.lock().unwrap();
        let transaction = connection.transaction().unwrap();
        put_entity(&transaction, &f.leader.tenant_id, KIND, &original.review_key, original.version, &original).unwrap();
        transaction.commit().unwrap();
    }
    let reopened = WorkflowStore::open(&f.path).unwrap();
    assert!(reopened.adaptive_leadership_admission_repair_epoch(&f.leader.tenant_id, f.grant.session_id).unwrap() == Some(receipt));
}
