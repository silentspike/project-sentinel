//! Synthetic offline replacement fixtures, never live provider evidence.
use super::super::adaptive_leadership_review::tests::{
    discovery_state, persist, reconcile_review_at,
};
use super::super::adaptive_leadership_review::LeadershipAuthority;
use super::super::model_execution::{ModelExecutionCompletion, ModelExecutionContext};
use super::*;
use crate::llm_bridge::bridge::ProviderUsageAuthorityResolver;

fn limits(expires: u64) -> AdaptiveWorkFundingLimitsV1 {
    AdaptiveWorkFundingLimitsV1 {
        additional_model_calls: 4,
        additional_tool_calls: 4,
        additional_reviews: 4,
        additional_windows: 4,
        max_window_ms: 300_000,
        max_call_duration_ms: 120_000,
        dispatch_margin_ms: sentinel_workflow::ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
        expires_at_unix_ms: expires,
    }
}

fn fixture() -> (
    tempfile::TempDir,
    WorkflowApi,
    AdaptiveWorkFundingExpiredReplacementVerificationSourceV1,
    LeadershipAuthority,
) {
    let (temp, api, session) =
        super::super::adaptive_resume_policy::tests::unreviewed_policy_root_with_limits(
            120_000, false, 4, 4,
        );
    *api.provider_admission.write().unwrap() = true;
    let issued = session.grant.deadline_ms;
    assert!(session.model_window_exhausted_at(issued));
    let operator = api.principals.principal("operator").unwrap();
    let request = api
        .store
        .adaptive_work_funding_draft(
            &operator.principal,
            &session.grant.authority.project_id,
            session.grant.session_id,
            Uuid::new_v4(),
            "synthetic-reviewed-expired-proposal",
            limits(issued + 180_000),
            issued,
        )
        .unwrap();
    let (_, old) = api
        .store
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
    assert!(reconcile_review_at(&api, &project, issued));
    let call = api
        .store
        .adaptive_leadership_review_calls(&project.tenant_id, session.grant.session_id)
        .unwrap()
        .into_iter()
        .find(|call| {
            call.grant
                .work_funding
                .as_ref()
                .is_some_and(|epoch| epoch.receipt == old)
        })
        .unwrap();
    let queued = LeadershipAuthority::from_call(&call);
    assert!(call.grant.expires_at_unix_ms < now_unix_ms());
    api.store
        .expire_adaptive_leadership_review_call(
            &call.grant.leadership_principal,
            call.grant.review_id,
            call.version,
            now_unix_ms(),
        )
        .unwrap();
    let source = api
        .store
        .adaptive_work_funding_expired_replacement_verification_source(
            &operator.principal,
            &project.project_id,
            session.grant.session_id,
            &old.receipt_digest().unwrap(),
            now_unix_ms(),
        )
        .unwrap();
    (temp, api, source, queued)
}

fn path(source: &AdaptiveWorkFundingExpiredReplacementVerificationSourceV1) -> String {
    format!("{ADAPTIVE_WORK_FUNDING_PATH}?mode={EXPIRED_FUNDING_REPLACEMENT_MODE}&project_id={}&session_id={}&old_funding_receipt_digest={}",
        source.source.project_id, source.source.session_id, source.source.old_funding_receipt_digest)
}

fn draft_path(
    source: &AdaptiveWorkFundingExpiredReplacementVerificationSourceV1,
    limits: &AdaptiveWorkFundingLimitsV1,
    op: Uuid,
) -> String {
    format!("{}&operation_id={op}&reason_ref=synthetic-finite-replacement&additional_model_calls={}&additional_tool_calls={}&additional_reviews={}&additional_windows={}&max_window_ms={}&max_call_duration_ms={}&dispatch_margin_ms={}&expires_at_unix_ms={}",
        path(source), limits.additional_model_calls, limits.additional_tool_calls, limits.additional_reviews,
        limits.additional_windows, limits.max_window_ms, limits.max_call_duration_ms,
        limits.dispatch_margin_ms, limits.expires_at_unix_ms)
}

fn ok(response: WorkflowHttpResponse) -> serde_json::Value {
    assert_eq!(
        response.status,
        200,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    serde_json::from_slice(&response.body).unwrap()
}

fn draft(
    api: &WorkflowApi,
    source: &AdaptiveWorkFundingExpiredReplacementVerificationSourceV1,
) -> ReplacementSubmission {
    let public = ok(api.adaptive_work_funding_http(
        &api.principals.principal("operator").unwrap(),
        "GET",
        &draft_path(source, &limits(now_unix_ms() + 600_000), Uuid::new_v4()),
        b"",
    ));
    assert_eq!(public["requires_explicit_submission"], true);
    assert_eq!(public["grant_schema_version"], 5);
    serde_json::from_value(public["request"].clone()).unwrap()
}

fn post(api: &WorkflowApi, submission: &ReplacementSubmission) -> WorkflowHttpResponse {
    api.adaptive_work_funding_http(
        &api.principals.principal("operator").unwrap(),
        "POST",
        &format!("{ADAPTIVE_WORK_FUNDING_PATH}?mode={EXPIRED_FUNDING_REPLACEMENT_MODE}"),
        &serde_json::to_vec(submission).unwrap(),
    )
}

fn historical(
    source: &AdaptiveWorkFundingExpiredReplacementVerificationSourceV1,
    value: &ReplacementSubmission,
) -> String {
    format!(
        "{}&receipt_only=true&operation_id={}&replacement_key={}",
        path(source),
        value.operation_id,
        source.source.replacement_key().unwrap()
    )
}

#[test]
fn expired_funding_metadata_after_expiry_is_not_proof_and_consumed_history_is_readable() {
    let (temp, api, source, _) = fixture();
    let operator = api.principals.principal("operator").unwrap();
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    let exported = ok(api.adaptive_work_funding_http(
        &operator,
        "GET",
        &format!("{}&source_only=true", path(&source)),
        b"",
    ));
    assert_eq!(
        exported["source_digest"],
        source.source.canonical_digest().unwrap()
    );
    assert_eq!(
        exported["replacement_key"],
        source.source.replacement_key().unwrap()
    );
    assert_eq!(exported["authority"], "unverified_source_metadata");
    assert_eq!(exported["authority_issued"], false);
    assert_eq!(
        exported["issuer"],
        serde_json::to_value(&operator.principal).unwrap()
    );
    assert!(exported.get("no_io").is_none());
    assert!(exported.get("expires_at_unix_ms").is_none());
    assert!(exported.get("operation_id").is_none());
    assert!(serde_json::to_vec(&exported).unwrap().len() < MAX_WORKFLOW_BODY_BYTES);
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite")
        ),
        before
    );
    let proof = adaptive_recovery_release::TestExpiredFundingGuard::fixture(&source);
    proof.reject();
    assert_ne!(
        api.adaptive_work_funding_http(
            &operator,
            "GET",
            &draft_path(&source, &limits(now_unix_ms() + 600_000), Uuid::new_v4()),
            b""
        )
        .status,
        200
    );
    drop(proof);
    let proof = adaptive_recovery_release::TestExpiredFundingGuard::fixture(&source);
    let submission = draft(&api, &source);
    ok(post(&api, &submission));
    proof.reject();
    let historical_export = ok(api.adaptive_work_funding_http(
        &operator,
        "GET",
        &format!("{}&source_only=true", path(&source)),
        b"",
    ));
    assert_eq!(historical_export["source"], exported["source"]);
    assert_eq!(
        historical_export["retired_reviews"],
        exported["retired_reviews"]
    );
    assert_eq!(
        historical_export["source_digest"],
        exported["source_digest"]
    );
    assert_eq!(historical_export["issuer"], exported["issuer"]);
    assert_eq!(historical_export["historical_inspection"], true);
    assert_eq!(historical_export["eligibility"], "historical_noneligible");
    assert!(
        api.store
            .adaptive_work_funding_expired_replacement_verification_source(
                &operator.principal,
                &source.source.project_id,
                source.source.session_id,
                &source.source.old_funding_receipt_digest,
                now_unix_ms()
            )
            .is_err(),
        "consumed metadata cannot authorize another proposal"
    );
}

#[test]
fn expired_funding_gate_replacement_is_finite_once_and_deferred_old_preparation_stays_retired() {
    let (_temp, api, source, queued) = fixture();
    let proof = adaptive_recovery_release::TestExpiredFundingGuard::fixture(&source);
    let old = source
        .calls
        .iter()
        .find(|call| call.grant.review_id == queued.grant.review_id)
        .unwrap();
    let events = api.event_store.as_ref().unwrap();
    events
        .reserve_llm_request(
            &old.request_id(),
            &"a".repeat(64),
            &queued
                .grant
                .leadership_principal
                .agent_id
                .unwrap()
                .to_string(),
        )
        .unwrap();
    events
        .release_undispatched_llm_request(&old.request_id(), &"a".repeat(64))
        .unwrap();
    assert!(events
        .get_llm_completion(&old.request_id())
        .unwrap()
        .is_none());
    let queued_preparation = || api.prepare_leadership_review(&queued);
    assert!(
        api.active_provider_requests.lock().unwrap().is_empty(),
        "queued selection is not live HTTP ownership"
    );
    let value = draft(&api, &source);
    let issued = ok(post(&api, &value));
    assert_eq!(issued["developer_window_created"], false);
    assert_eq!(issued["model_decision_recorded"], false);
    assert_eq!(
        issued["expires_at_unix_ms"],
        value.limits.expires_at_unix_ms
    );
    assert_eq!(queued_preparation().unwrap_err(), "leadership call retired");
    assert_eq!(
        api.store
            .adaptive_session_for_authority(&source.session.grant.authority)
            .unwrap()
            .unwrap(),
        *source.session
    );
    assert_eq!(
        api.store
            .adaptive_work_funding(
                &source.source.tenant_id,
                source.source.session_id,
                source.old_funding.request.operation_id
            )
            .unwrap()
            .unwrap(),
        *source.old_funding
    );
    proof.reject();
    *api.provider_admission.write().unwrap() = false;
    let replay = ok(post(&api, &value));
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["issued_at_unix_ms"], issued["issued_at_unix_ms"]);
    assert_eq!(replay["expires_at_unix_ms"], issued["expires_at_unix_ms"]);
    let operator = api.principals.principal("operator").unwrap();
    let historical =
        ok(api.adaptive_work_funding_http(&operator, "GET", &historical(&source, &value), b""));
    assert_eq!(historical["historical_lookup"], true);
    assert_eq!(historical["new_funding_id"], issued["new_funding_id"]);
    assert_eq!(
        historical["original_submission"],
        serde_json::to_value(&value).unwrap()
    );
    assert_eq!(
        historical["issuer"],
        serde_json::to_value(&operator.principal).unwrap()
    );
    let mut changed = value;
    changed.limits.expires_at_unix_ms += 1;
    assert_ne!(post(&api, &changed).status, 200);
}

#[test]
fn expired_funding_normal_schema5_continue_and_defer_commit_and_replay() {
    for continued in [true, false] {
        let (_temp, api, source, _) = fixture();
        let _proof = adaptive_recovery_release::TestExpiredFundingGuard::fixture(&source);
        let value = draft(&api, &source);
        ok(post(&api, &value));
        let agent = source.calls[0].grant.leadership_principal.agent_id.unwrap();
        // Funding issuance does not issue a review. Drive the ordinary reconciler;
        // stale unrelated calls may require a separate pass before review issuance.
        let mut candidate = None;
        for _ in 0..4 {
            let project = api
                .store
                .company_project(&source.source.tenant_id, &source.source.project_id)
                .unwrap()
                .unwrap();
            reconcile_review_at(&api, &project, now_unix_ms());
            candidate = api.prepare_provider_usage_candidate(agent).unwrap();
            if candidate.is_some() {
                break;
            }
        }
        let authority = candidate
            .expect("ordinary reconciliation must select the replacement funding review")
            .authority
            .unwrap();
        let model = api.model_work_context(&authority).unwrap().unwrap();
        assert_eq!(
            api.validate_prepared_provider_model_context(&authority, &model)
                .unwrap(),
            Some(())
        );
        let ModelExecutionContext::AdaptiveLeadershipReview(context) = model else {
            panic!("normal leadership selection required");
        };
        assert_eq!(context.binding.schema_version, 5);
        assert_eq!(context.binding.grant.schema_version, 5);
        assert!(context.binding.grant.funded_review_recovery.is_none());
        assert_eq!(
            context
                .binding
                .grant
                .work_funding
                .as_ref()
                .unwrap()
                .receipt
                .request
                .operation_id,
            value.operation_id
        );
        let (id, digest) =
            super::super::adaptive_resume_policy::tests::claim_review(&api, &context);
        let decision = if continued {
            serde_json::json!({"kind":"continue", "additional_model_calls":4, "window_ms":180_000,
                "rationale":"synthetic independent continuation", "evidence_refs":[context.source.evidence_refs[0]]})
        } else {
            serde_json::json!({"kind":"defer_budget", "rationale":"synthetic independent refusal",
                "evidence_refs":[context.source.evidence_refs[0]]})
        };
        let completion = ModelExecutionCompletion {
            context: ModelExecutionContext::AdaptiveLeadershipReview(context.clone()),
            content: serde_json::json!({"schema_version":5, "decision":decision}).to_string(),
            admissible: true,
        };
        persist(&api, &completion, &context, &id, &digest, true);
        api.admit_model_work(&completion, &id, &digest).unwrap();
        let after = api
            .store
            .adaptive_session_for_authority(&source.session.grant.authority)
            .unwrap()
            .unwrap();
        assert_eq!(after.grant, source.session.grant);
        assert_eq!(
            (after.model_calls, after.tool_calls),
            (source.session.model_calls, source.session.tool_calls)
        );
        assert_eq!(after.active_work_funding().is_some(), continued);
        let events = api.event_store.as_ref().unwrap();
        let before = serde_json::to_value(events.get_all_events().unwrap()).unwrap();
        let retained = events.get_llm_completion(&id).unwrap();
        api.admit_model_work(&completion, &id, &digest).unwrap();
        assert_eq!(
            serde_json::to_value(events.get_all_events().unwrap()).unwrap(),
            before
        );
        assert_eq!(events.get_llm_completion(&id).unwrap(), retained);
        assert_eq!(
            api.store
                .adaptive_session_for_authority(&source.session.grant.authority)
                .unwrap()
                .unwrap(),
            after
        );
        ok(api.adaptive_work_funding_http(
            &api.principals.principal("operator").unwrap(),
            "GET",
            &historical(&source, &value),
            b"",
        ));
        assert!(
            api.leadership_review_for_agent(agent).unwrap().is_none(),
            "neither Defer nor same-head retirement rerolls"
        );
    }
}

#[test]
fn expired_funding_member_proof_and_current_provider_traces_deny_without_writes() {
    for case in [
        "source",
        "missing_gate",
        "omitted_member",
        "duplicate_member",
        "wrong_request",
        "release_claim",
        "oversize",
        "ordering_cycle",
        "reserved",
        "unknown",
        "completion",
        "usage",
        "history",
        "live",
    ] {
        let (temp, api, source, _) = fixture();
        let proof = adaptive_recovery_release::TestExpiredFundingGuard::fixture(&source);
        let old = &source.calls[0];
        let id = old.request_id();
        let digest = "a".repeat(64);
        let agent = old.grant.leadership_principal.agent_id.unwrap().to_string();
        let events = api.event_store.as_ref().unwrap();
        match case {
            "live" => {
                api.active_provider_requests
                    .lock()
                    .unwrap()
                    .insert(id.clone(), 1);
            }
            "reserved" | "unknown" | "completion" | "usage" | "history" => {
                events.reserve_llm_request(&id, &digest, &agent).unwrap();
                if matches!(case, "unknown" | "history") {
                    events
                        .mark_llm_provider_outcome_unknown(
                            &id,
                            &digest,
                            "UnknownOutcome: provider_transport_deadline_elapsed",
                        )
                        .unwrap();
                    if case == "history" {
                        events
                            .resolve_llm_completion_terminal(&id, &digest, "synthetic abandonment")
                            .unwrap();
                    }
                }
                if matches!(case, "completion" | "usage") {
                    events.enqueue_llm_completion(&id, &digest, "{}").unwrap();
                    if case == "usage" {
                        let event = sentinel_common::DomainEvent::new(
                            "agent_llm_usage",
                            &agent,
                            "{}",
                            &id,
                            0,
                        )
                        .with_operation_id(&format!("llm_usage_{id}"));
                        events
                            .persist_llm_completion_usage(&id, &digest, &event)
                            .unwrap();
                        events.claim_llm_completion_actions(&id, &digest).unwrap();
                        events
                            .complete_llm_completion_actions(&id, &digest)
                            .unwrap();
                    }
                }
            }
            _ => proof.mutate(case),
        }
        let before = discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
        );
        let result = api.adaptive_work_funding_http(
            &api.principals.principal("operator").unwrap(),
            "GET",
            &draft_path(&source, &limits(now_unix_ms() + 600_000), Uuid::new_v4()),
            b"",
        );
        assert_ne!(result.status, 200, "{case}");
        if matches!(
            case,
            "reserved" | "unknown" | "completion" | "usage" | "history" | "live"
        ) {
            assert_ne!(
                api.adaptive_work_funding_http(
                    &api.principals.principal("operator").unwrap(),
                    "GET",
                    &format!("{}&source_only=true", path(&source)),
                    b""
                )
                .status,
                200,
                "unconsumed metadata must still veto {case}"
            );
        }
        assert_eq!(
            discovery_state(
                &temp.path().join("company.sqlite"),
                &temp.path().join("events.sqlite")
            ),
            before,
            "{case}"
        );
    }
}

#[test]
fn expired_funding_shared_reference_bound_denies_without_partial_evidence() {
    let (_temp, _api, source, _) = fixture();
    // Parser-only denial fixture; these additional calls are never persisted or authorized.
    let mut many = source.clone();
    let old = many.calls[0].clone();
    many.calls.clear();
    many.review_members.clear();
    for _ in 0..7 {
        let mut call = old.clone();
        call.grant.review_id = Uuid::new_v4();
        many.review_members
            .push(sentinel_workflow::AdaptiveFundedReviewRecoveryMemberV1 {
                review_id: call.grant.review_id,
                version: call.version,
                digest: "a".repeat(64),
            });
        many.calls.push(call);
    }
    many.review_members.sort_by_key(|member| member.review_id);
    many.source.prior_review_count = 7;
    many.source.current_resume_source.base_review_count = 7;
    let proof = adaptive_recovery_release::TestExpiredFundingGuard::fixture(&many);
    proof.mutate("reference_bound");
    assert_eq!(
        adaptive_recovery_release::verified_current_expired_funding_replacement(&many).unwrap_err(),
        "expired funding reference bound exceeded"
    );
}

#[test]
fn expired_funding_consumed_inspection_survives_assignment_change_but_not_fresh_issuance() {
    let (temp, api, source, _) = fixture();
    let proof = adaptive_recovery_release::TestExpiredFundingGuard::fixture(&source);
    let value = draft(&api, &source);
    ok(post(&api, &value));
    let authority = &source.session.grant.authority;
    let leader = &source.calls[0].grant.leadership_principal;
    api.store
        .apply_company_command(
            leader,
            Uuid::new_v4(),
            &CompanyWorkflowCommandV1::ReassignWork {
                project_id: authority.project_id.clone(),
                expected_version: source.project.version,
                work_item_id: authority.work_item_id.clone(),
                expected_assignment_version: authority.assignment_version,
                agent_id: authority.agent_id,
                organization_generation: authority.organization_generation,
                organization_digest: authority.organization_digest.clone(),
                reason_ref: "synthetic-revoke-after-consumption".into(),
            },
            now_unix_ms(),
        )
        .unwrap();
    proof.reject();
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    let operator = api.principals.principal("operator").unwrap();
    let exported = ok(api.adaptive_work_funding_http(
        &operator,
        "GET",
        &format!("{}&source_only=true", path(&source)),
        b"",
    ));
    assert_eq!(
        exported["source_digest"],
        source.source.canonical_digest().unwrap()
    );
    assert_eq!(exported["historical_inspection"], true);
    assert_eq!(exported["eligibility"], "historical_noneligible");
    assert_eq!(exported["authority_issued"], false);
    assert!(exported.get("no_io").is_none());
    assert_ne!(
        api.adaptive_work_funding_http(
            &operator,
            "GET",
            &draft_path(&source, &limits(now_unix_ms() + 600_000), Uuid::new_v4()),
            b""
        )
        .status,
        200
    );
    ok(api.adaptive_work_funding_http(&operator, "GET", &historical(&source, &value), b""));
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite")
        ),
        before
    );
}

#[test]
fn expired_funding_routing_rejects_ignored_post_identity_unknown_mode_and_mixed_contracts() {
    let (temp, api, source, _) = fixture();
    let _proof = adaptive_recovery_release::TestExpiredFundingGuard::fixture(&source);
    let value = draft(&api, &source);
    let bytes = serde_json::to_vec(&value).unwrap();
    let operator = api.principals.principal("operator").unwrap();
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    assert_eq!(
        api.adaptive_work_funding_http(
            &operator,
            "GET",
            &format!(
                "{}&replacement_key=foreign",
                draft_path(&source, &value.limits, value.operation_id)
            ),
            b""
        )
        .status,
        400
    );
    for suffix in [
        "project_id=foreign",
        "operation_id=00000000-0000-0000-0000-000000000001",
        "reason_ref=ignored",
        "old_funding_receipt_digest=foreign",
        "expires_at_unix_ms=1",
    ] {
        assert_eq!(
            api.adaptive_work_funding_http(
                &operator,
                "POST",
                &format!(
                    "{ADAPTIVE_WORK_FUNDING_PATH}?mode={EXPIRED_FUNDING_REPLACEMENT_MODE}&{suffix}"
                ),
                &bytes
            )
            .status,
            400
        );
    }
    for query in [
        "mode=unknown",
        "mode=unknown&mode=expired_unadopted_replacement",
        "mode=expired_unadopted_replacement&mode=expired_unadopted_replacement",
        "source_only=true",
        "receipt_only=true",
        "old_funding_receipt_digest=foreign",
        "mode=expired_unadopted_replacement&supersedes_unused_receipt_digest=foreign",
    ] {
        assert_eq!(
            api.adaptive_work_funding_http(
                &operator,
                "GET",
                &format!("{ADAPTIVE_WORK_FUNDING_PATH}?{query}"),
                b""
            )
            .status,
            400,
            "{query}"
        );
    }
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite")
        ),
        before
    );
}

#[test]
fn expired_funding_historical_original_submission_and_issuer_are_exact_and_missing_never_drafts() {
    let (temp, mut api, source, _) = fixture();
    let _proof = adaptive_recovery_release::TestExpiredFundingGuard::fixture(&source);
    let value = draft(&api, &source);
    let issued = ok(post(&api, &value));
    let operator = api.principals.principal("operator").unwrap();
    let mut other = operator.clone();
    other.principal.principal_id = "operator-other".into();
    other.execution_authority =
        PrincipalAuthorityV1::derive("operator-other", 1, &[9; 32]).unwrap();
    let mut registry = PrincipalAuthenticator {
        by_principal_id: api.principals.by_principal_id.clone(),
        by_credential_digest: api.principals.by_credential_digest.clone(),
    };
    registry
        .by_principal_id
        .insert(other.principal.principal_id.clone(), other.clone());
    api.principals = Arc::new(registry);
    *api.provider_admission.write().unwrap() = false;
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    let query = historical(&source, &value);
    let found = ok(api.adaptive_work_funding_http(&operator, "GET", &query, b""));
    assert_eq!(
        found["original_submission"],
        serde_json::to_value(&value).unwrap()
    );
    assert_eq!(
        found["issuer"],
        serde_json::to_value(&operator.principal).unwrap()
    );
    assert_eq!(found["expires_at_unix_ms"], issued["expires_at_unix_ms"]);
    assert_ne!(
        api.adaptive_work_funding_http(&other, "GET", &query, b"")
            .status,
        200
    );
    for changed in [
        query.replace(&value.operation_id.to_string(), &Uuid::new_v4().to_string()),
        query.replace(&value.project_id.0, "foreign-project"),
        query.replace(&value.session_id.to_string(), &Uuid::new_v4().to_string()),
        query.replace(&value.old_funding_receipt_digest, &"f".repeat(64)),
        format!("{query}&operation_id={}", value.operation_id),
    ] {
        assert_ne!(
            api.adaptive_work_funding_http(&operator, "GET", &changed, b"")
                .status,
            200
        );
    }
    let mut changed = value.clone();
    changed.source_digest = "f".repeat(64);
    assert_ne!(post(&api, &changed).status, 200);
    changed = value.clone();
    changed.reason_ref.push_str("-changed");
    assert_ne!(post(&api, &changed).status, 200);
    changed = value.clone();
    changed.proof_digest = "f".repeat(64);
    assert_ne!(post(&api, &changed).status, 200);
    changed = value;
    changed.release.gateway_binary_digest = "f".repeat(64);
    assert_ne!(post(&api, &changed).status, 200);
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite")
        ),
        before
    );
}

#[test]
fn expired_funding_final_callback_rechecks_new_reservation_and_rolls_back_domain() {
    let (temp, api, source, _) = fixture();
    let _proof = adaptive_recovery_release::TestExpiredFundingGuard::fixture(&source);
    let value = draft(&api, &source);
    let reads = std::cell::Cell::new(0);
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    let result = api.expired_funding_replacement_http_with_checks(
        &api.principals.principal("operator").unwrap(),
        "POST",
        &format!("?mode={EXPIRED_FUNDING_REPLACEMENT_MODE}"),
        &serde_json::to_vec(&value).unwrap(),
        now_unix_ms,
        |fresh| {
            let evidence =
                adaptive_recovery_release::verified_current_expired_funding_replacement(fresh)?;
            reads.set(reads.get() + 1);
            if reads.get() == 2 {
                let call = &fresh.calls[0];
                api.event_store
                    .as_ref()
                    .unwrap()
                    .reserve_llm_request(
                        &call.request_id(),
                        &"a".repeat(64),
                        &call
                            .grant
                            .leadership_principal
                            .agent_id
                            .unwrap()
                            .to_string(),
                    )
                    .unwrap();
            }
            Ok(evidence)
        },
    );
    assert_ne!(result.status, 200);
    assert_eq!(
        reads.get(),
        2,
        "independent initial and writer-boundary proof checks"
    );
    let after = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    assert_eq!(
        &after[..5],
        &before[..5],
        "injected EventStore row cannot leave a partial domain proposal/relation"
    );
}

#[test]
fn expired_funding_draft_samples_final_clock_and_keeps_explicit_deadline_and_caps() {
    let (temp, api, source, _) = fixture();
    let _proof = adaptive_recovery_release::TestExpiredFundingGuard::fixture(&source);
    let initial = now_unix_ms();
    let clock = std::cell::Cell::new(initial);
    let bound = limits(initial + 180_000);
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    let result = api.expired_funding_replacement_http_with_checks(
        &api.principals.principal("operator").unwrap(),
        "GET",
        &draft_path(&source, &bound, Uuid::new_v4()),
        b"",
        || clock.get(),
        |fresh| {
            let evidence =
                adaptive_recovery_release::verified_current_expired_funding_replacement(fresh)?;
            clock.set(initial + 180_000);
            Ok(evidence)
        },
    );
    assert_ne!(
        result.status, 200,
        "verification cannot extend the submitted new expiry"
    );
    let mut excessive = limits(initial + 600_000);
    excessive.additional_reviews = u16::MAX;
    assert_ne!(
        api.adaptive_work_funding_http(
            &api.principals.principal("operator").unwrap(),
            "GET",
            &draft_path(&source, &excessive, Uuid::new_v4()),
            b""
        )
        .status,
        200
    );
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite")
        ),
        before
    );
}

#[test]
fn expired_funding_metadata_flags_are_exact_get_only_and_unambiguous() {
    assert_eq!(
        metadata_modes("?mode=expired_unadopted_replacement", "GET"),
        Ok((false, false))
    );
    assert_eq!(
        metadata_modes("?source_only=true", "GET"),
        Ok((true, false))
    );
    assert_eq!(
        metadata_modes("?receipt_only=true", "GET"),
        Ok((false, true))
    );
    for query in [
        "?source_only=",
        "?source_only",
        "?source_only=false",
        "?receipt_only=1",
        "?source_only=true&source_only=true",
        "?source_only=true&receipt_only=true",
        "?session_id=one&session_id=two",
        "?supersedes_unused_receipt_digest=old",
        "?unknown=true",
    ] {
        assert!(metadata_modes(query, "GET").is_err(), "{query}");
    }
    assert!(metadata_modes("?receipt_only=true", "POST").is_err());
    assert!(metadata_modes("?source_only=true", "POST").is_err());
}
