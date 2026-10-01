use super::*;
use crate::{
    adaptive_leadership_recovery_history_digest, adaptive_leadership_recovery_project_digest,
    adaptive_leadership_recovery_session_digest,
    AdaptiveLeadershipRecoveryBlockedSubjectV1, AdaptiveLeadershipRecoveryEpochV1,
    AdaptiveLeadershipRecoveryRequestV1,
    AdaptiveLeadershipLocalAdoptionRequestV1, AdaptiveLeadershipLocalAdoptionV1,
    AdaptiveRecoveryReleaseV1,
};

fn recovery_source() -> (Fixture, Vec<AdaptiveLeadershipReviewCallV1>, u64) {
    recovery_source_with_subject(true)
}

fn recovery_source_with_subject(unknown: bool) -> (Fixture, Vec<AdaptiveLeadershipReviewCallV1>, u64) {
    let mut f = continuation_fixture(unknown, false);
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
