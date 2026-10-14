use super::super::model_execution::{ModelExecutionCompletion, ModelExecutionContext};
use super::*;
use crate::llm_bridge::bridge::ProviderUsageAuthorityResolver;

std::thread_local! {
    static MEMBERSHIP_VERIFICATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(super) fn record_membership_verification() {
    MEMBERSHIP_VERIFICATIONS.with(|count| count.set(count.get() + 1));
}

fn take_membership_verifications() -> usize {
    MEMBERSHIP_VERIFICATIONS.with(|count| count.replace(0))
}

fn admitted_root_fixture() -> (
    tempfile::TempDir,
    WorkflowApi,
    sentinel_workflow::AdaptiveSessionV1,
) {
    let (temp, api, session) =
        adaptive_resume_policy::tests::unreviewed_policy_root_with_limits(120_000, false, 4, 4);
    assert!(api.enabled && api.model_work_enabled);
    assert!(api.active_provider_requests.lock().unwrap().is_empty());
    *api.provider_admission.write().unwrap() = true;
    (temp, api, session)
}

// Synthetic source facts for pure proof/template tests, not issued authority.
pub(crate) fn source_fixture() -> AdaptiveFundedReviewRecoveryVerificationSourceV1 {
    let (context, _, _) = adaptive_leadership_review::tests::funded_prompt_fixture();
    let grant = context.binding.grant.clone();
    let funding = grant.work_funding.as_ref().unwrap().receipt.clone();
    let ordinal = grant.work_funding.as_ref().unwrap().binding.ordinal + 1;
    let call = AdaptiveLeadershipReviewCallV1 {
        schema_version: 1,
        review_key: "synthetic-funded-review".into(),
        allowance_id: context.binding.allowance_id.clone(),
        operation_id: Uuid::new_v4(),
        version: 4,
        created_at_unix_ms: context.binding.issued_at_ms,
        grant_issued_at_unix_ms: context.binding.issued_at_ms,
        updated_at_unix_ms: grant.expires_at_unix_ms,
        retired_at_unix_ms: Some(grant.expires_at_unix_ms),
        dispatch: None,
        decision: None,
        model_response_digest: None,
        resolution_event_id: None,
        continuation: None,
        grant: grant.clone(),
        context: context.source.clone(),
    };
    let source = sentinel_workflow::AdaptiveFundedReviewRecoverySourceV1 {
        schema_version: 1,
        tenant_id: context.source.source_project.tenant_id.clone(),
        project_id: context.source.source_project.project_id.clone(),
        work_item_id: context
            .source
            .source_session
            .grant
            .authority
            .work_item_id
            .clone(),
        session_id: context.source.source_session.grant.session_id,
        expected_project_version: context.source.source_project.version,
        expected_session_version: context.source.source_session.version,
        project_digest: "a".repeat(64),
        session_digest: "b".repeat(64),
        session_head_digest: "c".repeat(64),
        funding_receipt_digest: funding.receipt_digest().unwrap(),
        review_history_digest: "d".repeat(64),
        review_count: 1,
        retired_review_id: grant.review_id,
        ordinal,
    };
    AdaptiveFundedReviewRecoveryVerificationSourceV1 {
        source,
        project: Box::new(context.source.source_project),
        session: Box::new(context.source.source_session),
        funding: Box::new(funding),
        calls: vec![call],
    }
}

fn request_fixture() -> (
    AdaptiveFundedReviewRecoveryVerificationSourceV1,
    AdaptiveFundedReviewRecoveryRequestV1,
) {
    let source = source_fixture();
    let now = source.calls[0].grant.expires_at_unix_ms + 1;
    let request = AdaptiveFundedReviewRecoveryRequestV1 {
        schema_version: 1,
        operation_id: Uuid::new_v4(),
        release: sentinel_workflow::AdaptiveRecoveryReleaseV1 {
            schema_version: 1,
            source_git_sha: "a".repeat(40),
            release_manifest_digest: "b".repeat(64),
            gateway_binary_digest: "c".repeat(64),
        },
        proof_digest: "d".repeat(64),
        reason_ref: "synthetic-funded-recovery".into(),
        expires_at_unix_ms: now + 200_000,
        source_digest: source.source.canonical_digest().unwrap(),
        source: Box::new(source.source.clone()),
    };
    (source, request)
}

#[test]
fn funded_retry_template_preserves_epoch_source_limits_and_model_freedom() {
    let (source, request) = request_fixture();
    let old = &source.calls[0];
    let (grant, context) = template(&source, &request).unwrap();
    assert_ne!(grant.review_id, old.grant.review_id);
    assert_eq!(grant.schema_version, 5);
    assert!(grant.funded_review_recovery.is_none());
    assert!(grant
        .work_funding
        .as_ref()
        .unwrap()
        .same_epoch(old.grant.work_funding.as_ref().unwrap()));
    assert_eq!(
        grant.work_funding.as_ref().unwrap().binding.ordinal,
        request.source.ordinal
    );
    assert_eq!(grant.max_duration_ms, old.grant.max_duration_ms);
    assert_eq!(grant.leadership_principal, old.grant.leadership_principal);
    assert_eq!(grant.leadership_authority, old.grant.leadership_authority);
    assert_eq!(grant.assignee_authority, old.grant.assignee_authority);
    assert_eq!(grant.subject, old.grant.subject);
    assert_eq!(grant.provider, old.grant.provider);
    assert_eq!(grant.model, old.grant.model);
    assert_eq!(context.source_project, old.context.source_project);
    assert_eq!(context.source_session, old.context.source_session);
    assert_eq!(context.tool_catalog, old.context.tool_catalog);
    assert_eq!(
        context
            .evidence_refs
            .iter()
            .filter(|reference| reference.starts_with("adaptive-work-funding:"))
            .count(),
        1
    );
    context.validate(&grant).unwrap();
    assert_eq!(template(&source, &request).unwrap(), (grant, context));
}

#[test]
fn funded_retry_template_changes_identity_for_changed_exact_request() {
    let (source, mut request) = request_fixture();
    let first = template(&source, &request).unwrap().0.review_id;
    request.operation_id = Uuid::new_v4();
    assert_ne!(template(&source, &request).unwrap().0.review_id, first);
}

#[test]
fn funded_retry_final_clock_rejects_expiry_funding_and_original_window_overflow() {
    let (source, mut request) = request_fixture();
    let now = source.calls[0].grant.expires_at_unix_ms + 1;
    assert!(validate_lifetime(&source, &request, now).is_ok());
    assert!(validate_lifetime(&source, &request, request.expires_at_unix_ms).is_err());
    request.expires_at_unix_ms = source.funding.request.limits.expires_at_unix_ms + 1;
    assert!(validate_lifetime(&source, &request, now).is_err());
    request.expires_at_unix_ms = now + source.funding.request.limits.max_window_ms + 1;
    assert!(validate_lifetime(&source, &request, now).is_err());
    request.expires_at_unix_ms = now + source.calls[0].grant.max_duration_ms;
    assert!(validate_lifetime(&source, &request, now).is_err());
}

#[test]
fn funded_retry_no_io_denies_live_reserved_and_unknown() {
    let (_temp, api, _) = admitted_root_fixture();
    let source = source_fixture();
    let id = source.calls[0].request_id();
    let events = api.event_store.as_ref().unwrap();
    let guard = api.provider_recovery_fence().unwrap();
    assert!(api.validate_funded_recovery_no_io(&source, &guard).is_ok());
    events
        .reserve_llm_request(
            &id,
            &"a".repeat(64),
            &source.calls[0]
                .grant
                .leadership_principal
                .agent_id
                .unwrap()
                .to_string(),
        )
        .unwrap();
    assert!(api.validate_funded_recovery_no_io(&source, &guard).is_err());
    events
        .release_undispatched_llm_request(&id, &"a".repeat(64))
        .unwrap();
    assert!(
        api.validate_funded_recovery_no_io(&source, &guard).is_ok(),
        "released row passes veto only; protected proof is still required"
    );
    events
        .reserve_llm_request(
            &id,
            &"a".repeat(64),
            &source.calls[0]
                .grant
                .leadership_principal
                .agent_id
                .unwrap()
                .to_string(),
        )
        .unwrap();
    events
        .mark_llm_provider_outcome_unknown(
            &id,
            &"a".repeat(64),
            "UnknownOutcome: provider_transport_deadline_elapsed",
        )
        .unwrap();
    assert!(api.validate_funded_recovery_no_io(&source, &guard).is_err());
    events
        .resolve_llm_completion_terminal(&id, &"a".repeat(64), "synthetic operator abandonment")
        .unwrap();
    assert!(events.get_llm_completion(&id).unwrap().is_none());
    assert!(
        api.validate_funded_recovery_no_io(&source, &guard).is_err(),
        "resolution history is not no-I/O proof"
    );
    drop(guard);
    api.active_provider_requests
        .lock()
        .unwrap()
        .insert("synthetic-active".into(), 1);
    let guard = api.provider_recovery_fence().unwrap();
    assert!(api
        .validate_funded_request_no_io("synthetic-active", &guard)
        .is_err());
}

#[test]
fn funded_retry_route_rejects_employee_and_routes_before_legacy_mutation_lock() {
    let (_temp, api, _) = admitted_root_fixture();
    let employee = &api.principals.principal("pm").unwrap();
    assert_eq!(
        api.funded_review_recovery_http(employee, "GET", "?mode=funded_undispatched_retry", b"")
            .status,
        403
    );
    let operator = &api.principals.principal("operator").unwrap();
    // Missing query fields reach the new helper; acquiring the old lock first would return busy.
    assert_eq!(
        api.review_recovery_epoch(operator, "GET", "?mode=funded_undispatched_retry", b"")
            .status,
        400
    );
}

#[test]
fn funded_retry_public_request_is_bounded_metadata_without_private_contexts() {
    let (_, request) = request_fixture();
    let bytes = serde_json::to_vec(&request).unwrap();
    assert!(bytes.len() < 4_096);
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    for key in [
        "project",
        "session",
        "calls",
        "funding",
        "context",
        "tool_catalog",
        "private_observation",
    ] {
        assert!(value["source"].get(key).is_none(), "{key}");
    }
    assert_eq!(
        value["source_digest"],
        request.source.canonical_digest().unwrap()
    );
}

#[test]
fn funded_retry_no_io_denies_usage_after_completion_row_cleanup() {
    let (_temp, api, _) = admitted_root_fixture();
    let source = source_fixture();
    let id = source.calls[0].request_id();
    let agent = source.calls[0]
        .grant
        .leadership_principal
        .agent_id
        .unwrap()
        .to_string();
    let digest = "a".repeat(64);
    let events = api.event_store.as_ref().unwrap();
    events.reserve_llm_request(&id, &digest, &agent).unwrap();
    events.enqueue_llm_completion(&id, &digest, "{}").unwrap();
    // Any retained usage trace is a veto; this is not fabricated accounting evidence.
    let event = sentinel_common::DomainEvent::new("agent_llm_usage", &agent, "{}", &id, 0)
        .with_operation_id(&format!("llm_usage_{id}"));
    events
        .persist_llm_completion_usage(&id, &digest, &event)
        .unwrap();
    events.claim_llm_completion_actions(&id, &digest).unwrap();
    events
        .complete_llm_completion_actions(&id, &digest)
        .unwrap();
    assert!(events.get_llm_completion(&id).unwrap().is_none());
    let guard = api.provider_recovery_fence().unwrap();
    assert!(api.validate_funded_recovery_no_io(&source, &guard).is_err());
}

fn live_fixture() -> (
    tempfile::TempDir,
    WorkflowApi,
    AdaptiveFundedReviewRecoveryVerificationSourceV1,
) {
    let (temp, api, source, _) = live_fixture_with_release(false);
    (temp, api, source)
}

fn live_fixture_with_release(
    released: bool,
) -> (
    tempfile::TempDir,
    WorkflowApi,
    AdaptiveFundedReviewRecoveryVerificationSourceV1,
    Option<(String, u64)>,
) {
    let (temp, api, session) = admitted_root_fixture();
    let now = now_unix_ms();
    // The unfunded root expires by deadline, not by the funded dispatch margin.
    let issued = session.grant.deadline_ms;
    assert!(session.grant.created_at_ms < issued && issued < now);
    assert!(session.model_window_exhausted_at(issued));
    let operator = &api.principals.principal("operator").unwrap();
    let request = api
        .store
        .adaptive_work_funding_draft(
            &operator.principal,
            &session.grant.authority.project_id,
            session.grant.session_id,
            Uuid::new_v4(),
            "synthetic-funded-retry-api",
            sentinel_workflow::AdaptiveWorkFundingLimitsV1 {
                additional_model_calls: 4,
                additional_tool_calls: 4,
                additional_reviews: 4,
                additional_windows: 4,
                max_window_ms: 300_000,
                max_call_duration_ms: 120_000,
                dispatch_margin_ms: sentinel_workflow::ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
                expires_at_unix_ms: now + 600_000,
            },
            issued,
        )
        .unwrap();
    api.store
        .authorize_adaptive_work_funding(&operator.principal, &request, issued)
        .unwrap();
    let project = api
        .store
        .company_project(
            &operator.principal.tenant_id,
            &session.grant.authority.project_id,
        )
        .unwrap()
        .unwrap();
    adaptive_leadership_review::tests::reconcile_review_at(&api, &project, issued);
    let calls = api
        .store
        .adaptive_leadership_review_calls(&project.tenant_id, session.grant.session_id)
        .unwrap();
    let old = calls
        .iter()
        .find(|call| call.grant.schema_version == 5)
        .unwrap();
    assert!(old.grant.expires_at_unix_ms < now);
    let release = if released {
        let digest = "a".repeat(64);
        let events = api.event_store.as_ref().unwrap();
        events
            .reserve_llm_request(
                &old.request_id(),
                &digest,
                &old.grant.leadership_principal.agent_id.unwrap().to_string(),
            )
            .unwrap();
        events
            .release_undispatched_llm_request(&old.request_id(), &digest)
            .unwrap();
        Some((digest, now_unix_ms()))
    } else {
        None
    };
    let retired_at = now_unix_ms();
    api.store
        .expire_adaptive_leadership_review_call(
            &old.grant.leadership_principal,
            old.grant.review_id,
            old.version,
            retired_at,
        )
        .unwrap();
    let source = api
        .store
        .adaptive_funded_review_recovery_verification_source(
            &operator.principal,
            &project.project_id,
            session.grant.session_id,
            old.grant.review_id,
            retired_at,
        )
        .unwrap();
    (temp, api, source, release)
}

#[test]
fn funded_retry_api_issues_once_and_replays_before_closed_admission_or_replaced_proof() {
    for released in [false, true] {
        let (_temp, api, source, release) = live_fixture_with_release(released);
        let proof = adaptive_recovery_release::TestFundedRecoveryGuard::fixture(&source);
        if let Some((digest, released_at)) = release {
            assert!(released_at <= source.calls[0].retired_at_unix_ms.unwrap());
            proof.release_before_io(&digest, released_at);
        }
        let operator = &api.principals.principal("operator").unwrap();
        let path = format!("{ADAPTIVE_REVIEW_EPOCH_PATH}?mode=funded_undispatched_retry&project_id={}&session_id={}&retired_review_id={}",
        source.source.project_id, source.source.session_id, source.source.retired_review_id);
        let draft = api.review_recovery_epoch(operator, "GET", &path, b"");
        assert_eq!(
            draft.status,
            200,
            "{}",
            String::from_utf8_lossy(&draft.body)
        );
        let draft: serde_json::Value = serde_json::from_slice(&draft.body).unwrap();
        let request: AdaptiveFundedReviewRecoveryRequestV1 =
            serde_json::from_value(draft["request"].clone()).unwrap();
        let bytes = serde_json::to_vec(&request).unwrap();
        assert!(bytes.len() < 4_096);
        let issued = api.review_recovery_epoch(operator, "POST", &path, &bytes);
        assert_eq!(
            issued.status,
            200,
            "{}",
            String::from_utf8_lossy(&issued.body)
        );
        let key = request.source.receipt_key().unwrap();
        let stored = api
            .store
            .adaptive_funded_review_recovery(&operator.principal.tenant_id, &key)
            .unwrap()
            .unwrap();
        assert_eq!(
            matches!(
                &stored.evidence.no_io,
                sentinel_workflow::AdaptiveFundedReviewNoIoV1::ReleasedBeforeIo { .. }
            ),
            released
        );
        let grant = stored.bound_grant().unwrap();
        assert_eq!(grant.schema_version, 6);
        assert_eq!(
            grant.work_funding.as_ref().unwrap().receipt,
            *source.funding
        );
        let call = api
            .store
            .adaptive_leadership_review_call(&operator.principal.tenant_id, grant.review_id)
            .unwrap()
            .unwrap();
        api.verify_funded_review_recovery_membership(&call).unwrap();
        api.validate_work_funding_review(&call).unwrap();
        assert_eq!(
            api.store
                .adaptive_leadership_review_calls(
                    &operator.principal.tenant_id,
                    source.source.session_id
                )
                .unwrap()
                .len(),
            source.calls.len() + 1
        );
        assert_eq!(
            api.store
                .adaptive_funded_review_recovery_historical_verification_source(
                    &operator.principal.tenant_id,
                    &key
                )
                .unwrap()
                .unwrap()
                .source,
            source.source
        );
        proof.reject();
        assert!(api.verify_funded_review_recovery_membership(&call).is_err());
        assert!(api.validate_work_funding_review(&call).is_err());
        take_membership_verifications();
        assert!(api
            .prepare_leadership_review(&adaptive_leadership_review::LeadershipAuthority::from_call(
                &call
            ))
            .is_err());
        assert_eq!(
            take_membership_verifications(),
            1,
            "changed proof must fail the first prepare pair check"
        );
        *api.provider_admission.write().unwrap() = false;
        let replay = api.review_recovery_epoch(operator, "POST", &path, &bytes);
        assert_eq!(replay.status, 200);
        let replay: serde_json::Value = serde_json::from_slice(&replay.body).unwrap();
        assert_eq!(replay["replayed"], true);
        assert_eq!(replay["expires_at_unix_ms"], request.expires_at_unix_ms);
        let mut changed = request;
        changed.expires_at_unix_ms += 1;
        assert_eq!(
            api.review_recovery_epoch(
                operator,
                "POST",
                &path,
                &serde_json::to_vec(&changed).unwrap()
            )
            .status,
            409
        );
        assert_eq!(
            api.store
                .adaptive_funded_review_recovery(&operator.principal.tenant_id, &key)
                .unwrap()
                .unwrap(),
            stored
        );
    }
}

fn retry_path(source: &AdaptiveFundedReviewRecoveryVerificationSourceV1) -> String {
    format!("{ADAPTIVE_REVIEW_EPOCH_PATH}?mode=funded_undispatched_retry&project_id={}&session_id={}&retired_review_id={}",
        source.source.project_id, source.source.session_id, source.source.retired_review_id)
}

fn historical_receipt_path(request: &AdaptiveFundedReviewRecoveryRequestV1) -> String {
    format!("{ADAPTIVE_REVIEW_EPOCH_PATH}?mode=funded_undispatched_retry&receipt_only=true&receipt_key={}&operation_id={}&project_id={}&session_id={}&retired_review_id={}",
        request.source.receipt_key().unwrap(), request.operation_id,
        request.source.project_id, request.source.session_id, request.source.retired_review_id)
}

#[test]
fn funded_retry_historical_receipt_survives_closed_admission_expiry_stale_scope_and_proof() {
    let (temp, api, source) = live_fixture();
    let proof = adaptive_recovery_release::TestFundedRecoveryGuard::fixture(&source);
    let request = issue_retry(&api, &source);
    let operator = api.principals.principal("operator").unwrap();
    let bytes = serde_json::to_vec(&request).unwrap();
    let replay = api.review_recovery_epoch(&operator, "POST", &retry_path(&source), &bytes);
    assert_eq!(replay.status, 200);
    let mut expected: serde_json::Value = serde_json::from_slice(&replay.body).unwrap();
    expected["historical_lookup"] = serde_json::json!(true);
    let path = historical_receipt_path(&request);
    let initial = api.review_recovery_epoch(&operator, "GET", &path, b"");
    assert_eq!(initial.status, 200);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&initial.body).unwrap(),
        expected
    );
    drop(proof);
    let invalid_new_release = adaptive_recovery_release::TestFundedRecoveryGuard::fixture(&source);
    invalid_new_release.reject();
    *api.provider_admission.write().unwrap() = false;
    let authority = &source.session.grant.authority;
    let old = source
        .calls
        .iter()
        .find(|call| call.grant.review_id == source.source.retired_review_id)
        .unwrap();
    api.store
        .apply_company_command(
            &old.grant.leadership_principal,
            Uuid::new_v4(),
            &CompanyWorkflowCommandV1::ReassignWork {
                project_id: authority.project_id.clone(),
                expected_version: source.project.version,
                work_item_id: authority.work_item_id.clone(),
                expected_assignment_version: authority.assignment_version,
                agent_id: authority.agent_id,
                organization_generation: authority.organization_generation,
                organization_digest: authority.organization_digest.clone(),
                reason_ref: "synthetic historical receipt stale assignment".into(),
            },
            now_unix_ms(),
        )
        .unwrap();
    let before = adaptive_leadership_review::tests::discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    let _mutation = api.mutation_fence.write().unwrap();
    let clock_reads = std::cell::Cell::new(0);
    let historical = api.funded_review_recovery_http_with_checks(
        &operator,
        "GET",
        &path,
        b"",
        || {
            clock_reads.set(clock_reads.get() + 1);
            request.expires_at_unix_ms + 1
        },
        |_| panic!("historical receipt lookup must not consult current release proof"),
    );
    assert_eq!(
        historical.status,
        200,
        "{}",
        String::from_utf8_lossy(&historical.body)
    );
    assert_eq!(
        clock_reads.get(),
        0,
        "immutable lookup has no current expiry check"
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&historical.body).unwrap(),
        expected
    );
    assert_eq!(
        adaptive_leadership_review::tests::discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite")
        ),
        before
    );
}

#[test]
fn funded_retry_historical_receipt_rejects_wrong_identity_flags_and_issuer_without_writes() {
    let (temp, mut api, source) = live_fixture();
    let _proof = adaptive_recovery_release::TestFundedRecoveryGuard::fixture(&source);
    let request = issue_retry(&api, &source);
    let operator = api.principals.principal("operator").unwrap();
    let mut other = operator.clone();
    other.principal.principal_id = "operator-other".into();
    other.execution_authority =
        PrincipalAuthorityV1::derive("operator-other", 1, &[9; 32]).unwrap();
    let mut foreign = operator.clone();
    foreign.principal.principal_id = "operator-foreign".into();
    foreign.principal.tenant_id = TenantId::parse("tenant-foreign").unwrap();
    foreign.execution_authority =
        PrincipalAuthorityV1::derive("operator-foreign", 1, &[8; 32]).unwrap();
    let mut principals = PrincipalAuthenticator {
        by_principal_id: api.principals.by_principal_id.clone(),
        by_credential_digest: api.principals.by_credential_digest.clone(),
    };
    principals
        .by_principal_id
        .insert(other.principal.principal_id.clone(), other.clone());
    principals
        .by_principal_id
        .insert(foreign.principal.principal_id.clone(), foreign.clone());
    api.principals = Arc::new(principals);
    assert!(api.validate_work_funding_operator(&other).is_ok());
    assert!(api.validate_work_funding_operator(&foreign).is_ok());
    let path = historical_receipt_path(&request);
    let key = request.source.receipt_key().unwrap();
    let missing_key = format!("funded-review-recovery-{}", "f".repeat(64));
    let before = adaptive_leadership_review::tests::discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    *api.provider_admission.write().unwrap() = false;
    for invalid in [
        path.replace(&key, &missing_key),
        path.replace(
            &request.operation_id.to_string(),
            &Uuid::new_v4().to_string(),
        ),
        path.replace(
            &format!("project_id={}", request.source.project_id),
            "project_id=wrong-project",
        ),
        path.replace(
            &format!("session_id={}", request.source.session_id),
            &format!("session_id={}", Uuid::new_v4()),
        ),
        path.replace(
            &format!("retired_review_id={}", request.source.retired_review_id),
            &format!("retired_review_id={}", Uuid::new_v4()),
        ),
        path.replace(&format!("&receipt_key={key}"), ""),
        path.replace(&format!("&operation_id={}", request.operation_id), ""),
        path.replace("receipt_only=true", "receipt_only=false"),
        path.replace("receipt_only=true", "receipt_only="),
        path.replace("receipt_only=true", "receipt_only"),
        path.replace("receipt_only=true", "receipt_only=1"),
        format!("{path}&receipt_only=true"),
        format!("{path}&source_only=true"),
        format!("{path}&operation_id={}", request.operation_id),
        format!("{path}&receipt_key={key}"),
        format!("{path}&unrecognized=true"),
        path.replace(&request.operation_id.to_string(), &Uuid::nil().to_string()),
    ] {
        let response = api.review_recovery_epoch(&operator, "GET", &invalid, b"");
        assert!(
            matches!(response.status, 400 | 409),
            "{invalid}: {}",
            response.status
        );
    }
    assert_eq!(
        api.review_recovery_epoch(&other, "GET", &path, b"").status,
        409
    );
    assert_eq!(
        api.review_recovery_epoch(&foreign, "GET", &path, b"")
            .status,
        409
    );
    assert_eq!(
        api.review_recovery_epoch(&api.principals.principal("pm").unwrap(), "GET", &path, b"")
            .status,
        403
    );
    assert_eq!(
        api.review_recovery_epoch(
            &operator,
            "POST",
            &path,
            &serde_json::to_vec(&request).unwrap()
        )
        .status,
        400
    );
    assert_eq!(
        adaptive_leadership_review::tests::discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite")
        ),
        before
    );
}

#[test]
fn funded_retry_historical_receipt_rejects_corrupt_or_orphaned_receipt_without_fallback() {
    for orphan in [false, true] {
        let (temp, api, source) = live_fixture();
        let _proof = adaptive_recovery_release::TestFundedRecoveryGuard::fixture(&source);
        let request = issue_retry(&api, &source);
        let key = request.source.receipt_key().unwrap();
        let connection =
            sentinel_limbo::rusqlite::Connection::open(temp.path().join("company.sqlite")).unwrap();
        let sql = if orphan {
            "DELETE FROM company_entities WHERE entity_kind='adaptive_funded_review_recovery' AND entity_id=?1"
        } else {
            "UPDATE company_entities SET payload_digest='corrupt' WHERE entity_kind='adaptive_funded_review_recovery' AND entity_id=?1"
        };
        assert_eq!(
            connection
                .execute(sql, sentinel_limbo::rusqlite::params![key])
                .unwrap(),
            1
        );
        let before = adaptive_leadership_review::tests::discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
        );
        *api.provider_admission.write().unwrap() = false;
        let response = api.review_recovery_epoch(
            &api.principals.principal("operator").unwrap(),
            "GET",
            &historical_receipt_path(&request),
            b"",
        );
        assert_ne!(response.status, 200);
        assert_ne!(
            response.status, 503,
            "must reject historical corruption before admission"
        );
        assert_eq!(
            adaptive_leadership_review::tests::discovery_state(
                &temp.path().join("company.sqlite"),
                &temp.path().join("events.sqlite")
            ),
            before
        );
    }
}

#[test]
fn funded_retry_source_metadata_is_bounded_and_not_no_io_authority() {
    let (_temp, api, source) = live_fixture();
    let operator = api.principals.principal("operator").unwrap();
    let path = retry_path(&source);
    // Obtain a valid immutable submission, then invalidate the protected proof.
    let proof = adaptive_recovery_release::TestFundedRecoveryGuard::fixture(&source);
    let draft = api.review_recovery_epoch(&operator, "GET", &path, b"");
    assert_eq!(draft.status, 200);
    let draft: serde_json::Value = serde_json::from_slice(&draft.body).unwrap();
    let request = serde_json::to_vec(&draft["request"]).unwrap();
    drop(proof);
    let invalid = adaptive_recovery_release::TestFundedRecoveryGuard::fixture(&source);
    invalid.reject();
    let events = api.event_store.as_ref().unwrap();
    let before = serde_json::to_value(events.get_all_events().unwrap()).unwrap();
    let workflow_before = serde_json::to_value(
        api.store
            .company_project_events_since(&source.source.tenant_id, 0, 100)
            .unwrap(),
    )
    .unwrap();
    let exported =
        api.review_recovery_epoch(&operator, "GET", &format!("{path}&source_only=true"), b"");
    assert_eq!(
        exported.status,
        200,
        "{}",
        String::from_utf8_lossy(&exported.body)
    );
    assert!(exported.body.len() < 4_096);
    let public: serde_json::Value = serde_json::from_slice(&exported.body).unwrap();
    assert_eq!(public.as_object().unwrap().len(), 8);
    assert_eq!(public["schema_version"], 1);
    assert_eq!(public["authority"], "unverified_source_metadata");
    assert_eq!(public["authority_issued"], false);
    assert_eq!(public["model_decision_recorded"], false);
    assert_eq!(public["receipt_key"], source.source.receipt_key().unwrap());
    assert_eq!(
        public["source"],
        serde_json::to_value(&source.source).unwrap()
    );
    assert_eq!(
        public["source_digest"],
        source.source.canonical_digest().unwrap()
    );
    let old = source
        .calls
        .iter()
        .find(|call| call.grant.review_id == source.source.retired_review_id)
        .unwrap();
    assert_eq!(
        public["retired_review"],
        serde_json::to_value(adaptive_recovery_release::retired_admission_evidence(old).unwrap())
            .unwrap()
    );
    assert_eq!(
        api.review_recovery_epoch(&operator, "GET", &path, b"")
            .status,
        409
    );
    assert_eq!(
        api.review_recovery_epoch(&operator, "POST", &path, &request)
            .status,
        409
    );
    assert_eq!(
        serde_json::to_value(events.get_all_events().unwrap()).unwrap(),
        before
    );
    assert_eq!(
        serde_json::to_value(
            api.store
                .company_project_events_since(&source.source.tenant_id, 0, 100)
                .unwrap()
        )
        .unwrap(),
        workflow_before
    );
    assert!(events
        .get_llm_completion(&old.request_id())
        .unwrap()
        .is_none());
    assert!(api
        .store
        .adaptive_funded_review_recovery(
            &source.source.tenant_id,
            &source.source.receipt_key().unwrap()
        )
        .unwrap()
        .is_none());
    // Only an independently protected proof unlocks the ordinary draft/POST flow.
    drop(invalid);
    let _protected = adaptive_recovery_release::TestFundedRecoveryGuard::fixture(&source);
    let issued = issue_retry(&api, &source);
    assert_eq!(public["source_digest"], issued.source_digest);
    assert_ne!(
        api.review_recovery_epoch(&operator, "GET", &format!("{path}&source_only=true"), b"")
            .status,
        200,
        "consumed source metadata must not remain eligible for a fresh retry"
    );
}

#[test]
fn funded_retry_source_metadata_rejects_ambiguous_flags_post_and_employee() {
    let (_temp, api, source) = live_fixture();
    let operator = api.principals.principal("operator").unwrap();
    let path = retry_path(&source);
    for suffix in [
        "source_only",
        "source_only=",
        "source_only=false",
        "source_only=1",
        "source_only=true&source_only=true",
        "source_only=true&source_only=false",
        "source_only=true&session_id=ambiguous",
    ] {
        assert_eq!(
            api.review_recovery_epoch(&operator, "GET", &format!("{path}&{suffix}"), b"")
                .status,
            400
        );
    }
    let export_path = format!("{path}&source_only=true");
    assert_eq!(
        api.review_recovery_epoch(&operator, "POST", &export_path, b"{}")
            .status,
        400
    );
    assert_eq!(
        api.review_recovery_epoch(
            &api.principals.principal("pm").unwrap(),
            "GET",
            &export_path,
            b""
        )
        .status,
        403
    );
    *api.provider_admission.write().unwrap() = false;
    assert_eq!(
        api.review_recovery_epoch(&operator, "GET", &export_path, b"")
            .status,
        503
    );
}

#[test]
fn funded_retry_source_metadata_rechecks_changed_assignment_and_corrupt_head() {
    for corrupt in [false, true] {
        let (temp, api, source) = live_fixture();
        let operator = api.principals.principal("operator").unwrap();
        let path = retry_path(&source);
        let proof = adaptive_recovery_release::TestFundedRecoveryGuard::fixture(&source);
        let exported =
            api.review_recovery_epoch(&operator, "GET", &format!("{path}&source_only=true"), b"");
        assert_eq!(exported.status, 200);
        let draft = api.review_recovery_epoch(&operator, "GET", &path, b"");
        assert_eq!(draft.status, 200);
        let draft: serde_json::Value = serde_json::from_slice(&draft.body).unwrap();
        let request = serde_json::to_vec(&draft["request"]).unwrap();
        if corrupt {
            let connection =
                sentinel_limbo::rusqlite::Connection::open(temp.path().join("company.sqlite"))
                    .unwrap();
            assert_eq!(
                connection
                    .execute(
                        "UPDATE workflow_adaptive_heads SET version=version+1 WHERE session_id=?1",
                        sentinel_limbo::rusqlite::params![source.source.session_id.to_string()]
                    )
                    .unwrap(),
                1
            );
        } else {
            let authority = &source.session.grant.authority;
            let old = source
                .calls
                .iter()
                .find(|call| call.grant.review_id == source.source.retired_review_id)
                .unwrap();
            api.store
                .apply_company_command(
                    &old.grant.leadership_principal,
                    Uuid::new_v4(),
                    &CompanyWorkflowCommandV1::ReassignWork {
                        project_id: authority.project_id.clone(),
                        expected_version: source.project.version,
                        work_item_id: authority.work_item_id.clone(),
                        expected_assignment_version: authority.assignment_version,
                        agent_id: authority.agent_id,
                        organization_generation: authority.organization_generation,
                        organization_digest: authority.organization_digest.clone(),
                        reason_ref: "synthetic source-export assignment revocation".into(),
                    },
                    now_unix_ms(),
                )
                .unwrap();
        }
        let before =
            serde_json::to_value(api.event_store.as_ref().unwrap().get_all_events().unwrap())
                .unwrap();
        assert_ne!(
            api.review_recovery_epoch(&operator, "GET", &format!("{path}&source_only=true"), b"")
                .status,
            200
        );
        assert_ne!(
            api.review_recovery_epoch(&operator, "POST", &path, &request)
                .status,
            200
        );
        assert_eq!(
            serde_json::to_value(api.event_store.as_ref().unwrap().get_all_events().unwrap())
                .unwrap(),
            before
        );
        assert!(api
            .store
            .adaptive_funded_review_recovery(
                &source.source.tenant_id,
                &source.source.receipt_key().unwrap()
            )
            .unwrap()
            .is_none());
        drop(proof);
    }
}

fn issue_retry(
    api: &WorkflowApi,
    source: &AdaptiveFundedReviewRecoveryVerificationSourceV1,
) -> AdaptiveFundedReviewRecoveryRequestV1 {
    let operator = &api.principals.principal("operator").unwrap();
    let path = retry_path(source);
    let draft = api.review_recovery_epoch(operator, "GET", &path, b"");
    assert_eq!(
        draft.status,
        200,
        "{}",
        String::from_utf8_lossy(&draft.body)
    );
    let draft: serde_json::Value = serde_json::from_slice(&draft.body).unwrap();
    let request: AdaptiveFundedReviewRecoveryRequestV1 =
        serde_json::from_value(draft["request"].clone()).unwrap();
    let issued = api.review_recovery_epoch(
        operator,
        "POST",
        &path,
        &serde_json::to_vec(&request).unwrap(),
    );
    assert_eq!(
        issued.status,
        200,
        "{}",
        String::from_utf8_lossy(&issued.body)
    );
    request
}

#[test]
fn funded_retry_normal_pipeline_continue_and_defer_commit_once() {
    // Fixture model responses exercise production selection/claim/admission, not live inference.
    for continued in [true, false] {
        let (_temp, api, source) = live_fixture();
        let _proof = adaptive_recovery_release::TestFundedRecoveryGuard::fixture(&source);
        let request = issue_retry(&api, &source);
        let before = api
            .store
            .adaptive_session_for_authority(&source.session.grant.authority)
            .unwrap()
            .unwrap();
        assert_eq!(
            before, *source.session,
            "issuance cannot authorize employee continuation"
        );
        let agent = source.calls[0].grant.leadership_principal.agent_id.unwrap();
        let candidate = api
            .prepare_provider_usage_candidate(agent)
            .unwrap()
            .unwrap();
        let authority = candidate.authority.unwrap();
        let model = api.model_work_context(&authority).unwrap().unwrap();
        assert_eq!(
            api.validate_prepared_provider_model_context(&authority, &model)
                .unwrap(),
            Some(())
        );
        let ModelExecutionContext::AdaptiveLeadershipReview(context) = model else {
            panic!("normal selection must choose the issued leadership retry");
        };
        assert_eq!(context.binding.schema_version, 5);
        assert_eq!(context.binding.grant.schema_version, 6);
        take_membership_verifications();
        assert_eq!(
            api.prepare_leadership_review(&context.binding).unwrap(),
            *context
        );
        assert_eq!(
            take_membership_verifications(),
            1,
            "one full proof per prepare validation pair"
        );
        let selected_call = api
            .store
            .adaptive_leadership_review_call(
                &source.source.tenant_id,
                context.binding.grant.review_id,
            )
            .unwrap()
            .unwrap();
        api.verify_recovery_review_release(&selected_call).unwrap();
        assert_eq!(
            take_membership_verifications(),
            1,
            "standalone verification must remain fresh"
        );
        assert!(context
            .prompt()
            .unwrap()
            .contains("Defer remains admissible"));
        assert!(context
            .validate_dispatch(context.binding.grant.expires_at_unix_ms)
            .is_err());
        let id = authority.request_id();
        let digest = "c".repeat(64);
        let events = api.event_store.as_ref().unwrap();
        events
            .reserve_llm_request(&id, &digest, &agent.to_string())
            .unwrap();
        let claim_body = serde_json::to_vec(&serde_json::json!({
            "schema_version":5, "allowance_id":context.binding.allowance_id,
            "agent_id":agent.0, "request_id":id, "request_digest":digest,
            "context_digest":context.context_digest, "provider":context.binding.grant.provider,
            "model":context.binding.grant.model, "catalog_digest":context.binding.grant.catalog_digest,
            "subject":{"kind":"adaptive_leadership_review", "review_id":context.binding.grant.review_id,
                "review_kind":"funded_undispatched_recovery"}
        })).unwrap();
        let claim = api.subscription_dispatch(&claim_body);
        assert_eq!(
            claim.status,
            200,
            "{}",
            String::from_utf8_lossy(&claim.body)
        );
        let claimed = api
            .store
            .adaptive_leadership_review_call(
                &source.source.tenant_id,
                context.binding.grant.review_id,
            )
            .unwrap()
            .unwrap();
        assert!(claimed.dispatch.is_some());
        assert_eq!(
            api.subscription_dispatch(&claim_body).status,
            403,
            "dispatch replay must not mint a second permission"
        );
        assert_eq!(
            api.store
                .adaptive_leadership_review_call(
                    &source.source.tenant_id,
                    context.binding.grant.review_id
                )
                .unwrap()
                .unwrap(),
            claimed
        );
        assert_eq!(
            api.store
                .adaptive_session_for_authority(&source.session.grant.authority)
                .unwrap()
                .unwrap(),
            before
        );
        let decision = if continued {
            serde_json::json!({"kind":"continue", "additional_model_calls":4, "window_ms":180_000,
                "rationale":"Synthetic bounded inspection and productive continuation.",
                "evidence_refs":[context.source.evidence_refs[0]]})
        } else {
            serde_json::json!({"kind":"defer_budget", "rationale":"Synthetic independent refusal.",
                "evidence_refs":[context.source.evidence_refs[0]]})
        };
        let completion = ModelExecutionCompletion {
            context: ModelExecutionContext::AdaptiveLeadershipReview(context.clone()),
            content: serde_json::json!({"schema_version":6, "decision":decision}).to_string(),
            admissible: true,
        };
        adaptive_leadership_review::tests::persist(&api, &completion, &context, &id, &digest, true);
        take_membership_verifications();
        api.admit_model_work(&completion, &id, &digest).unwrap();
        assert_eq!(
            take_membership_verifications(),
            if continued { 3 } else { 1 },
            "completion runs once; Continue retains pre-audit and audit-append authority checks"
        );
        let completed = api
            .store
            .adaptive_leadership_review_call(
                &source.source.tenant_id,
                context.binding.grant.review_id,
            )
            .unwrap()
            .unwrap();
        assert!(completed.decision.is_some());
        assert!(completed.model_response_digest.is_some());
        let after = api
            .store
            .adaptive_session_for_authority(&source.session.grant.authority)
            .unwrap()
            .unwrap();
        assert_eq!(after.grant, before.grant);
        assert_eq!(
            (after.model_calls, after.tool_calls),
            (before.model_calls, before.tool_calls)
        );
        if continued {
            assert_eq!(after.version, before.version + 1);
            assert_eq!(
                after.active_work_funding().unwrap().receipt,
                *source.funding
            );
            assert!(completed.continuation.is_some());
        } else {
            assert_eq!(after, before);
            assert!(completed.continuation.is_none());
            assert!(after.active_work_funding().is_none());
        }
        let retained = events.get_llm_completion(&id).unwrap().unwrap();
        let audit = serde_json::to_value(events.get_all_events().unwrap()).unwrap();
        api.admit_model_work(&completion, &id, &digest).unwrap();
        assert_eq!(
            api.store
                .adaptive_session_for_authority(&source.session.grant.authority)
                .unwrap()
                .unwrap(),
            after
        );
        assert_eq!(
            api.store
                .adaptive_leadership_review_call(
                    &source.source.tenant_id,
                    context.binding.grant.review_id
                )
                .unwrap()
                .unwrap(),
            completed
        );
        assert_eq!(events.get_llm_completion(&id).unwrap().unwrap(), retained);
        assert_eq!(
            serde_json::to_value(events.get_all_events().unwrap()).unwrap(),
            audit
        );
        assert!(api.leadership_review_for_agent(agent).unwrap().is_none());
        let operator = &api.principals.principal("operator").unwrap();
        assert_eq!(
            api.review_recovery_epoch(
                operator,
                "POST",
                &retry_path(&source),
                &serde_json::to_vec(&request).unwrap()
            )
            .status,
            200
        );
        let historical =
            api.review_recovery_epoch(operator, "GET", &historical_receipt_path(&request), b"");
        assert_eq!(
            historical.status, 200,
            "Continue advances the head, not the immutable receipt"
        );
        let historical: serde_json::Value = serde_json::from_slice(&historical.body).unwrap();
        assert_eq!(historical["historical_lookup"], true);
        assert_eq!(historical["replayed"], true);
        assert_eq!(historical["expires_at_unix_ms"], request.expires_at_unix_ms);
        assert_eq!(historical["operation_id"], request.operation_id.to_string());
        assert_eq!(events.get_llm_completion(&id).unwrap().unwrap(), retained);
        assert_eq!(
            serde_json::to_value(events.get_all_events().unwrap()).unwrap(),
            audit
        );
    }
}

#[test]
fn funded_retry_draft_clock_is_sampled_after_delayed_real_verification() {
    for delay in [90_000, 450_000] {
        let (_temp, api, source) = live_fixture();
        let _proof = adaptive_recovery_release::TestFundedRecoveryGuard::fixture(&source);
        let initial = now_unix_ms();
        let clock = std::cell::Cell::new(initial);
        let draft = api.funded_review_recovery_http_with_checks(
            &api.principals.principal("operator").unwrap(),
            "GET",
            &retry_path(&source),
            b"",
            || clock.get(),
            |fresh| {
                let evidence =
                    adaptive_recovery_release::verified_current_funded_review_recovery(fresh)?;
                clock.set(initial + delay);
                Ok(evidence)
            },
        );
        assert_eq!(
            draft.status,
            200,
            "{}",
            String::from_utf8_lossy(&draft.body)
        );
        let draft: serde_json::Value = serde_json::from_slice(&draft.body).unwrap();
        let request: AdaptiveFundedReviewRecoveryRequestV1 =
            serde_json::from_value(draft["request"].clone()).unwrap();
        let old = &source.calls[0];
        let window = (old.grant.expires_at_unix_ms - old.grant_issued_at_unix_ms)
            .min(source.funding.request.limits.max_window_ms);
        assert_eq!(
            request.expires_at_unix_ms,
            (initial + delay + window).min(source.funding.request.limits.expires_at_unix_ms)
        );
        assert!(api
            .store
            .adaptive_funded_review_recovery(
                &source.source.tenant_id,
                &request.source.receipt_key().unwrap()
            )
            .unwrap()
            .is_none());
    }
}

#[test]
fn funded_retry_released_after_retirement_is_rejected_without_issuance() {
    let (_temp, api, source) = live_fixture();
    let proof = adaptive_recovery_release::TestFundedRecoveryGuard::fixture(&source);
    let old = &source.calls[0];
    std::thread::sleep(std::time::Duration::from_millis(2));
    let events = api.event_store.as_ref().unwrap();
    let digest = "a".repeat(64);
    events
        .reserve_llm_request(
            &old.request_id(),
            &digest,
            &old.grant.leadership_principal.agent_id.unwrap().to_string(),
        )
        .unwrap();
    events
        .release_undispatched_llm_request(&old.request_id(), &digest)
        .unwrap();
    let released_at = now_unix_ms();
    assert!(released_at > old.retired_at_unix_ms.unwrap());
    proof.release_before_io(&digest, released_at);
    assert!(adaptive_recovery_release::verified_current_funded_review_recovery(&source).is_err());
    assert_eq!(
        api.review_recovery_epoch(
            &api.principals.principal("operator").unwrap(),
            "GET",
            &retry_path(&source),
            b""
        )
        .status,
        409
    );
    assert!(api
        .store
        .adaptive_funded_review_recovery(
            &source.source.tenant_id,
            &source.source.receipt_key().unwrap()
        )
        .unwrap()
        .is_none());
}
