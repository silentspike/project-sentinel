use super::*;
use crate::{
    adaptive_leadership_recovery_history_digest, adaptive_leadership_recovery_project_digest,
    adaptive_leadership_recovery_session_digest,
    AdaptiveLeadershipRecoveryEpochV1, AdaptiveLeadershipRecoveryRequestV1,
    AdaptiveRecoveryReleaseV1,
};

fn recovery_source() -> (Fixture, Vec<AdaptiveLeadershipReviewCallV1>, u64) {
    let mut f = continuation_fixture(true, false);
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
    let AdaptiveLeadershipReviewSubjectV2::UnknownModel {
        effect, sealed_unknown_proof_digest,
    } = f.grant.subject.as_ref().unwrap() else {
        panic!("unknown model fixture");
    };
    let (_, head_digest) = crate::store::adaptive::load(
        &f.store.connection.lock().unwrap(), source.grant.session_id,
    ).unwrap().unwrap();
    let request = AdaptiveLeadershipRecoveryRequestV1 {
        schema_version: 1,
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
        unknown_effect: effect.clone(),
        sealed_unknown_proof_digest: sealed_unknown_proof_digest.clone(),
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
            "effect" => { request.unknown_effect.id = Uuid::new_v4(); now },
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
    assert!(continued.is_abandoned_model_effect(&request.unknown_effect));
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
        effect: request.unknown_effect.clone(),
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
