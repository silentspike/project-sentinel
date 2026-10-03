//! Offline synthetic fixtures only; these are not live funding or provider evidence.
use super::super::adaptive_leadership_review::tests::{
    discovery_state, persist, reconcile_review_at, stop_review_agent,
};
use super::super::adaptive_leadership_review::{LeadershipAuthority, LeadershipContext};
use super::super::adaptive_resume_policy::tests::{
    claim_review, unreviewed_policy_root_with_limits,
};
use super::super::model_execution::{ModelExecutionCompletion, ModelExecutionContext};
use super::*;

fn fixture() -> (tempfile::TempDir, WorkflowApi, AdaptiveSessionV1) {
    unreviewed_policy_root_with_limits(120_000, false, 4, 4)
}

fn limits() -> AdaptiveWorkFundingLimitsV1 {
    AdaptiveWorkFundingLimitsV1 {
        additional_model_calls: 4,
        additional_tool_calls: 4,
        additional_reviews: 4,
        additional_windows: 4,
        max_window_ms: 300_000,
        max_call_duration_ms: 120_000,
        dispatch_margin_ms: sentinel_workflow::ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
        expires_at_unix_ms: now_unix_ms() + 600_000,
    }
}

fn request(api: &WorkflowApi, source: &AdaptiveSessionV1) -> AdaptiveWorkFundingRequestV1 {
    api.store
        .adaptive_work_funding_draft(
            &api.principals.principal("operator").unwrap().principal,
            &source.grant.authority.project_id,
            source.grant.session_id,
            Uuid::new_v4(),
            "explicit-productive-work",
            limits(),
            now_unix_ms(),
        )
        .unwrap()
}

fn query(request: &AdaptiveWorkFundingRequestV1) -> String {
    let source = &request.source.resume_source;
    let limits = &request.limits;
    format!("{ADAPTIVE_WORK_FUNDING_PATH}?project_id={}&session_id={}&operation_id={}&reason_ref={}&additional_model_calls={}&additional_tool_calls={}&additional_reviews={}&additional_windows={}&max_window_ms={}&max_call_duration_ms={}&dispatch_margin_ms={}&expires_at_unix_ms={}",
        source.project_id, source.session_id, request.operation_id, request.reason_ref,
        limits.additional_model_calls, limits.additional_tool_calls, limits.additional_reviews,
        limits.additional_windows, limits.max_window_ms, limits.max_call_duration_ms,
        limits.dispatch_margin_ms, limits.expires_at_unix_ms)
}

fn post<T: Serialize>(api: &WorkflowApi, value: &T) -> WorkflowHttpResponse {
    api.adaptive_work_funding_http(
        &api.principals.principal("operator").unwrap(),
        "POST",
        ADAPTIVE_WORK_FUNDING_PATH,
        &serde_json::to_vec(value).unwrap(),
    )
}

fn assert_ok(response: &WorkflowHttpResponse) -> serde_json::Value {
    assert_eq!(
        response.status,
        200,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    serde_json::from_slice(&response.body).unwrap()
}

fn project(api: &WorkflowApi, source: &AdaptiveSessionV1) -> sentinel_workflow::ProjectV1 {
    api.store
        .company_project(
            &source.grant.authority.tenant_id,
            &source.grant.authority.project_id,
        )
        .unwrap()
        .unwrap()
}

fn selected(api: &WorkflowApi) -> LeadershipContext {
    let agent = api
        .principals
        .principal("pm")
        .unwrap()
        .principal
        .agent_id
        .unwrap();
    let call = api.leadership_review_for_agent(agent).unwrap().unwrap();
    api.prepare_leadership_review(&LeadershipAuthority::from_call(&call))
        .unwrap()
}

fn decide(api: &WorkflowApi, context: &LeadershipContext, continued: bool) {
    let (id, digest) = claim_review(api, context);
    let decision = if continued {
        serde_json::json!({"kind":"continue", "additional_model_calls":4, "window_ms":180_000,
            "rationale":"Synthetic inspection and productive work window.",
            "evidence_refs":[context.source.evidence_refs[0]]})
    } else {
        serde_json::json!({"kind":"defer_budget", "rationale":"Synthetic independent refusal.",
            "evidence_refs":[context.source.evidence_refs[0]]})
    };
    let completion = ModelExecutionCompletion {
        context: ModelExecutionContext::AdaptiveLeadershipReview(Box::new(context.clone())),
        content: serde_json::json!({"schema_version":5, "decision":decision}).to_string(),
        admissible: true,
    };
    persist(api, &completion, context, &id, &digest, true);
    api.accept_leadership_review_at(&completion, context, &id, &digest, now_unix_ms())
        .unwrap();
}

#[test]
fn draft_authorize_read_are_redacted_explicit_and_do_not_dispatch_or_adopt() {
    let (temp, api, source) = fixture();
    let request = request(&api, &source);
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    let operator = api.principals.principal("operator").unwrap();
    let draft = assert_ok(&api.adaptive_work_funding_http(&operator, "GET", &query(&request), &[]));
    assert!(draft["request"].get("source").is_none());
    assert_eq!(draft["requires_explicit_submission"], true);
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite")
        ),
        before
    );
    let issued = assert_ok(&post(&api, &draft["request"]));
    assert_eq!(issued["replayed"], false);
    assert_eq!(issued["model_decision_recorded"], false);
    assert_eq!(issued["developer_window_created"], false);
    for key in [
        "source",
        "request",
        "issuer_principal",
        "private_observation",
        "prompt",
    ] {
        assert!(issued.get(key).is_none(), "{key}");
    }
    assert_eq!(
        api.store
            .adaptive_session_for_authority(&source.grant.authority)
            .unwrap(),
        Some(source.clone())
    );
    assert!(api
        .store
        .adaptive_leadership_review_calls(
            &source.grant.authority.tenant_id,
            source.grant.session_id
        )
        .unwrap()
        .is_empty());
    let sealed = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    assert_eq!(assert_ok(&post(&api, &request))["replayed"], true);
    assert_eq!(
        assert_ok(&api.adaptive_work_funding_http(&operator, "GET", &query(&request), &[])),
        assert_ok(&post(&api, &request))
    );
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite")
        ),
        sealed
    );
}

#[test]
fn registered_exact_operator_identity_authority_tenant_and_source_are_required() {
    let (temp, api, source) = fixture();
    let request = request(&api, &source);
    assert!(is_workflow_path(ADAPTIVE_WORK_FUNDING_PATH));
    assert_eq!(
        api.handle("POST", ADAPTIVE_WORK_FUNDING_PATH, &HashMap::new(), &[])
            .unwrap()
            .status,
        401
    );
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    for bound in api
        .principals
        .by_principal_id
        .values()
        .filter(|bound| bound.principal.kind != CompanyPrincipalKindV1::Operator)
    {
        assert_eq!(
            api.adaptive_work_funding_http(
                bound,
                "POST",
                ADAPTIVE_WORK_FUNDING_PATH,
                &serde_json::to_vec(&request).unwrap()
            )
            .status,
            403
        );
    }
    let operator = api.principals.principal("operator").unwrap();
    for field in ["identity", "generation", "authority", "role", "tenant"] {
        let mut forged = operator.clone();
        match field {
            "identity" => forged.principal.principal_id = "unregistered-operator".into(),
            "generation" => forged.principal.authority_generation += 1,
            "authority" => forged.execution_authority.authority_digest = "f".repeat(64),
            "role" => forged.principal.role = CompanyRoleV1::Developer,
            "tenant" => forged.principal.tenant_id = TenantId::parse("foreign").unwrap(),
            _ => unreachable!(),
        }
        assert_eq!(
            api.adaptive_work_funding_http(
                &forged,
                "POST",
                ADAPTIVE_WORK_FUNDING_PATH,
                &serde_json::to_vec(&request).unwrap()
            )
            .status,
            403,
            "{field}"
        );
    }
    for field in ["head", "root", "current", "tenant", "predecessor", "expiry"] {
        let mut forged = request.clone();
        match field {
            "head" => forged.source.resume_source.head_entry_digest = "f".repeat(64),
            "root" => forged.source.original_model_call_ceiling += 1,
            "current" => forged.source.current_tool_call_ceiling += 1,
            "tenant" => forged.source.resume_source.tenant_id = TenantId::parse("foreign").unwrap(),
            "predecessor" => forged.source.predecessor_receipt_digest = Some("f".repeat(64)),
            "expiry" => forged.limits.expires_at_unix_ms = now_unix_ms(),
            _ => unreachable!(),
        }
        assert_ne!(post(&api, &forged).status, 200, "{field}");
    }
    let mut compact = serde_json::to_value(submission(&request).unwrap()).unwrap();
    compact["source_digest"] = serde_json::json!("f".repeat(64));
    assert_ne!(post(&api, &compact).status, 200);
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite")
        ),
        before
    );
}

#[test]
fn disabled_malformed_and_unsupported_http_requests_are_read_only() {
    let (temp, mut api, source) = fixture();
    let request = request(&api, &source);
    let operator = api.principals.principal("operator").unwrap();
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    assert_eq!(
        api.adaptive_work_funding_http(&operator, "PUT", ADAPTIVE_WORK_FUNDING_PATH, &[])
            .status,
        405
    );
    assert_eq!(
        api.adaptive_work_funding_http(&operator, "GET", ADAPTIVE_WORK_FUNDING_PATH, &[])
            .status,
        400
    );
    let mut spoofed = serde_json::to_value(submission(&request).unwrap()).unwrap();
    spoofed["issuer_principal"] = serde_json::json!(operator.principal);
    assert_ne!(post(&api, &spoofed).status, 200);
    api.model_work_enabled = false;
    assert_eq!(post(&api, &request).status, 503);
    api.model_work_enabled = true;
    api.enabled = false;
    assert_eq!(post(&api, &request).status, 503);
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite")
        ),
        before
    );
}

#[test]
fn unknown_or_nonexhausted_source_and_unavailable_employee_cannot_issue() {
    let (_temp, api, unknown) = unreviewed_policy_root_with_limits(30_000, true, 4, 4);
    assert!(api
        .work_funding_current_source(
            &api.principals.principal("operator").unwrap(),
            &unknown.grant.authority.project_id,
            unknown.grant.session_id,
            now_unix_ms()
        )
        .is_err());
    assert!(api
        .store
        .adaptive_work_funding_draft(
            &api.principals.principal("operator").unwrap().principal,
            &unknown.grant.authority.project_id,
            unknown.grant.session_id,
            Uuid::new_v4(),
            "explicit-productive-work",
            limits(),
            now_unix_ms()
        )
        .is_err());
    let (_temp, api, ready) = fixture();
    assert!(api
        .work_funding_current_source(
            &api.principals.principal("operator").unwrap(),
            &ready.grant.authority.project_id,
            ready.grant.session_id,
            ready.grant.created_at_ms
        )
        .is_err());
    let request = request(&api, &ready);
    stop_review_agent(&api, ready.grant.authority.agent_id, true);
    assert_ne!(post(&api, &request).status, 200);
}

#[test]
fn explicit_receipt_uses_one_existing_lane_and_seals_epoch_before_fingerprint() {
    let (temp, api, source) = fixture();
    let baseline = project(&api, &source);
    assert!(reconcile_review_at(&api, &baseline, now_unix_ms()));
    // This unissued baseline must never fabricate a funding epoch.
    assert!(api
        .store
        .adaptive_leadership_review_calls(&baseline.tenant_id, source.grant.session_id)
        .unwrap()
        .iter()
        .all(|call| call.grant.work_funding.is_none()));
    drop(temp);

    let (temp, api, source) = fixture();
    let request = request(&api, &source);
    assert_ok(&post(&api, &request));
    let project = project(&api, &source);
    assert!(reconcile_review_at(&api, &project, now_unix_ms()));
    let context = selected(&api);
    let epoch = context.binding.grant.work_funding.as_ref().unwrap();
    assert_eq!(context.binding.grant.schema_version, 5);
    assert!(context.binding.grant.resume_policy.is_none());
    assert!(context.binding.grant.recovery_epoch.is_none());
    assert_eq!(epoch.receipt.request, request);
    assert_eq!(context.source.source_session, source);
    assert_eq!(
        context
            .source
            .evidence_refs
            .iter()
            .filter(|reference| reference.starts_with("adaptive-work-funding:"))
            .count(),
        1
    );
    assert!(context
        .source
        .evidence_refs
        .contains(&epoch.evidence_ref().unwrap()));
    assert_eq!(
        context.binding.grant.evidence_fingerprint,
        sentinel_workflow::adaptive_leadership_evidence_fingerprint(
            &context.source.tool_catalog,
            &context.source.evidence_refs
        )
        .unwrap()
    );
    assert!(context
        .prompt()
        .unwrap()
        .contains("Original ROOT model/tool ceilings remain 4/4"));
    let prompt = context.prompt().unwrap();
    assert!(prompt.contains("assignment within remaining explicitly funded limits"));
    assert!(prompt.contains("enforces the remaining explicitly funded budget"));
    assert!(!prompt.contains("assignment within remaining root limits"));
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    assert!(reconcile_review_at(&api, &project, now_unix_ms()));
    assert_eq!(selected(&api), context);
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite")
        ),
        before
    );
    let receipt = &epoch.receipt;
    assert!(api
        .store
        .adaptive_work_funding_for_review(
            &project.tenant_id,
            source.grant.session_id,
            receipt.issued_at_unix_ms
        )
        .unwrap()
        .is_none());
}

#[test]
fn funding_context_rejects_marker_epoch_subject_and_legacy_combinations_without_writes() {
    let (temp, api, source) = fixture();
    assert_ok(&post(&api, &request(&api, &source)));
    reconcile_review_at(&api, &project(&api, &source), now_unix_ms());
    let context = selected(&api);
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    for field in [
        "marker",
        "epoch",
        "source",
        "ordinal",
        "schema",
        "subject",
        "unknown",
        "recovery",
        "v1_accounting",
    ] {
        let mut changed = context.clone();
        match field {
            "marker" => changed
                .source
                .evidence_refs
                .retain(|r| !r.starts_with("adaptive-work-funding:")),
            "epoch" => {
                changed
                    .binding
                    .grant
                    .work_funding
                    .as_mut()
                    .unwrap()
                    .receipt
                    .funding_id = "fabricated".into()
            }
            "source" => {
                changed
                    .binding
                    .grant
                    .work_funding
                    .as_mut()
                    .unwrap()
                    .receipt
                    .request
                    .source
                    .original_tool_call_ceiling += 1
            }
            "ordinal" => {
                changed
                    .binding
                    .grant
                    .work_funding
                    .as_mut()
                    .unwrap()
                    .binding
                    .ordinal += 1
            }
            "schema" => changed.binding.grant.schema_version = 3,
            "subject" => changed.binding.grant.subject = None,
            "unknown" => {
                changed.binding.grant.subject = Some(
                    sentinel_workflow::AdaptiveLeadershipReviewSubjectV2::UnknownModel {
                        effect: sentinel_workflow::AdaptiveEffectV1 {
                            id: Uuid::new_v4(),
                            request_digest: "f".repeat(64),
                        },
                        sealed_unknown_proof_digest: "f".repeat(64),
                    },
                )
            }
            "recovery" => {
                changed.binding.grant.recovery_epoch =
                    Some(sentinel_workflow::AdaptiveLeadershipRecoveryBindingV1 {
                        schema_version: 1,
                        epoch_key: "fabricated-recovery".into(),
                        epoch_digest: "f".repeat(64),
                        review_id: changed.binding.grant.review_id,
                        max_window_ms: 120_000,
                        max_additional_model_calls: 4,
                    })
            }
            "v1_accounting" => changed
                .source
                .evidence_refs
                .push(format!("adaptive-accounting-projection:{}", "f".repeat(64))),
            _ => unreachable!(),
        }
        assert!(
            changed
                .validate_dispatch(context.binding.issued_at_ms)
                .is_err(),
            "{field}"
        );
        assert!(
            api.prepare_leadership_review(&changed.binding).is_err(),
            "{field}"
        );
    }
    let mut changed = context.clone();
    changed.binding.grant.work_funding = None;
    assert!(changed
        .validate_dispatch(context.binding.issued_at_ms)
        .is_err());
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite")
        ),
        before
    );
}

#[test]
fn missing_typed_funding_membership_cannot_append_a_continuation_audit() {
    let (temp, api, source) = fixture();
    assert_ok(&post(&api, &request(&api, &source)));
    reconcile_review_at(&api, &project(&api, &source), now_unix_ms());
    let context = selected(&api);
    let (id, digest) = claim_review(&api, &context);
    let completion = ModelExecutionCompletion {
        context: ModelExecutionContext::AdaptiveLeadershipReview(Box::new(context.clone())),
        content: serde_json::json!({"schema_version":5, "decision":{
            "kind":"continue", "additional_model_calls":4, "window_ms":180_000,
            "rationale":"Synthetic missing membership regression.",
            "evidence_refs":[context.source.evidence_refs[0]],
        }})
        .to_string(),
        admissible: true,
    };
    serde_json::from_str::<sentinel_workflow::AdaptiveLeadershipReviewDecisionV1>(
        &completion.content,
    )
    .unwrap()
    .validate_subject(&context.binding.grant)
    .unwrap();
    persist(&api, &completion, &context, &id, &digest, true);
    let db =
        sentinel_limbo::rusqlite::Connection::open(temp.path().join("company.sqlite")).unwrap();
    assert_eq!(
        db.execute(
            "DELETE FROM company_entities WHERE entity_kind='adaptive_work_funding_review'",
            []
        )
        .unwrap(),
        1
    );
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    assert!(api
        .accept_leadership_review_at(&completion, &context, &id, &digest, now_unix_ms())
        .is_err());
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite")
        ),
        before
    );
}

#[test]
fn funded_subscription_callback_keeps_one_lane_one_claim_and_exact_selection() {
    let (temp, api, source) = fixture();
    assert_ok(&post(&api, &request(&api, &source)));
    reconcile_review_at(&api, &project(&api, &source), now_unix_ms());
    let context = selected(&api);
    let grant = &context.binding.grant;
    let agent = grant.leadership_principal.agent_id.unwrap();
    let id = format!("company-leadership-{}", grant.review_id);
    let digest = "c".repeat(64);
    api.event_store
        .as_ref()
        .unwrap()
        .reserve_llm_request(&id, &digest, &agent.to_string())
        .unwrap();
    let callback = serde_json::json!({
        "schema_version":5, "allowance_id":context.binding.allowance_id,
        "agent_id":agent.0, "request_id":id, "request_digest":digest,
        "context_digest":context.context_digest, "provider":grant.provider,
        "model":grant.model, "catalog_digest":grant.catalog_digest,
        "subject":{"kind":"adaptive_leadership_review", "review_id":grant.review_id,
            "review_kind":"work_funding"},
    });
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    for field in [
        "kind",
        "legacy_kind",
        "review",
        "context",
        "agent",
        "allowance",
        "schema",
    ] {
        let mut changed = callback.clone();
        match field {
            "kind" => changed["subject"]["review_kind"] = serde_json::json!("admission_repair"),
            "legacy_kind" => {
                changed["subject"]["review_kind"] = serde_json::json!("budget_window_exhausted")
            }
            "review" => changed["subject"]["review_id"] = serde_json::json!(Uuid::new_v4()),
            "context" => changed["context_digest"] = serde_json::json!("f".repeat(64)),
            "agent" => changed["agent_id"] = serde_json::json!(source.grant.authority.agent_id.0),
            "allowance" => changed["allowance_id"] = serde_json::json!("wrong-allowance"),
            "schema" => changed["schema_version"] = serde_json::json!(3),
            _ => unreachable!(),
        }
        assert_eq!(
            api.subscription_dispatch(&serde_json::to_vec(&changed).unwrap())
                .status,
            403,
            "{field}"
        );
    }
    assert!(api
        .leadership_review_for_dispatch_with_context(agent, Uuid::new_v4())
        .is_err());
    assert!(api
        .leadership_review_for_dispatch_with_context(
            source.grant.authority.agent_id,
            grant.review_id
        )
        .is_err());
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite")
        ),
        before
    );
    assert_ok(&api.subscription_dispatch(&serde_json::to_vec(&callback).unwrap()));
    assert_eq!(
        api.subscription_dispatch(&serde_json::to_vec(&callback).unwrap())
            .status,
        403
    );
    assert!(api.leadership_review_for_agent(agent).unwrap().is_none());
    assert!(api
        .adaptive_provider_authority(source.grant.authority.agent_id)
        .unwrap()
        .is_none());
}

#[test]
fn defer_and_undispatched_expiry_are_terminal_for_same_head_without_rescheduling() {
    for defer in [true, false] {
        let (temp, api, source) = fixture();
        let request = request(&api, &source);
        assert_ok(&post(&api, &request));
        reconcile_review_at(&api, &project(&api, &source), now_unix_ms());
        let context = selected(&api);
        let now = if defer {
            decide(&api, &context, false);
            now_unix_ms()
        } else {
            context.binding.grant.expires_at_unix_ms + 1
        };
        let current = project(&api, &source);
        reconcile_review_at(&api, &current, now);
        let before = discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
        );
        for offset in 1..=3 {
            reconcile_review_at(&api, &current, now + offset);
        }
        assert_eq!(
            discovery_state(
                &temp.path().join("company.sqlite"),
                &temp.path().join("events.sqlite")
            ),
            before
        );
        assert_eq!(
            api.store
                .adaptive_leadership_review_calls(&current.tenant_id, source.grant.session_id)
                .unwrap()
                .len(),
            1
        );
        assert!(api
            .store
            .adaptive_work_funding_for_review(&current.tenant_id, source.grant.session_id, now)
            .unwrap()
            .is_none());
        assert!(api
            .leadership_review_for_agent(
                context.binding.grant.leadership_principal.agent_id.unwrap()
            )
            .unwrap()
            .is_none());
        assert_eq!(assert_ok(&post(&api, &request))["replayed"], true);
    }
}

#[test]
fn invalid_retained_funded_decision_retires_once_without_a_same_head_reroll() {
    let (temp, api, source) = fixture();
    assert_ok(&post(&api, &request(&api, &source)));
    reconcile_review_at(&api, &project(&api, &source), now_unix_ms());
    let context = selected(&api);
    let (id, digest) = claim_review(&api, &context);
    let completion = ModelExecutionCompletion {
        context: ModelExecutionContext::AdaptiveLeadershipReview(Box::new(context.clone())),
        content: serde_json::json!({"schema_version":5, "decision":{
            "kind":"defer_budget", "rationale":"Synthetic invalid membership.",
            "evidence_refs":["invented-evidence"],
        }})
        .to_string(),
        admissible: true,
    };
    persist(&api, &completion, &context, &id, &digest, true);
    api.event_store
        .as_ref()
        .unwrap()
        .record_llm_completion_failure(&id, &digest, "leadership decision evidence invalid", 1)
        .unwrap();
    let expired = context.binding.grant.expires_at_unix_ms + 1;
    let current = project(&api, &source);
    reconcile_review_at(&api, &current, expired);
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    for offset in 1..=3 {
        reconcile_review_at(&api, &current, expired + offset);
    }
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite")
        ),
        before
    );
    let calls = api
        .store
        .adaptive_leadership_review_calls(&current.tenant_id, source.grant.session_id)
        .unwrap();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].retired_at_unix_ms.is_some());
    assert!(calls[0].decision.is_none());
    assert!(api
        .store
        .adaptive_work_funding_for_review(&current.tenant_id, source.grant.session_id, expired + 3)
        .unwrap()
        .is_none());
}

#[test]
fn second_funded_ordinal_on_pending_same_head_is_rejected_without_a_second_lane() {
    let (temp, api, source) = fixture();
    assert_ok(&post(&api, &request(&api, &source)));
    reconcile_review_at(&api, &project(&api, &source), now_unix_ms());
    let context = selected(&api);
    let mut grant = context.binding.grant.clone();
    let epoch = grant.work_funding.as_mut().unwrap();
    let old_ref = epoch.evidence_ref().unwrap();
    epoch.binding = epoch.receipt.binding(2).unwrap();
    let new_ref = epoch.evidence_ref().unwrap();
    let mut source_context = context.source.clone();
    for reference in &mut source_context.evidence_refs {
        if *reference == old_ref {
            *reference = new_ref.clone();
        }
    }
    grant.evidence_fingerprint = sentinel_workflow::adaptive_leadership_evidence_fingerprint(
        &source_context.tool_catalog,
        &source_context.evidence_refs,
    )
    .unwrap();
    grant.review_id = sentinel_workflow::adaptive_leadership_review_id(
        source.grant.session_id,
        source.version,
        &grant.evidence_fingerprint,
    )
    .unwrap();
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    assert!(api
        .store
        .authorize_adaptive_leadership_review_call(
            &grant.leadership_principal,
            Uuid::new_v4(),
            "fabricated-second-lane",
            &grant,
            &source_context,
            now_unix_ms()
        )
        .is_err());
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite")
        ),
        before
    );
    assert_eq!(selected(&api), context);
}

#[test]
fn expired_epoch_before_review_does_not_create_a_funded_review() {
    let (_temp, api, source) = fixture();
    let request = request(&api, &source);
    assert_ok(&post(&api, &request));
    assert!(api
        .store
        .adaptive_work_funding_for_review(
            &source.grant.authority.tenant_id,
            source.grant.session_id,
            request.limits.expires_at_unix_ms
        )
        .unwrap()
        .is_none());
    reconcile_review_at(
        &api,
        &project(&api, &source),
        request.limits.expires_at_unix_ms,
    );
    assert!(api
        .store
        .adaptive_leadership_review_calls(
            &source.grant.authority.tenant_id,
            source.grant.session_id
        )
        .unwrap()
        .iter()
        .all(|call| call.grant.work_funding.is_none()));
}

#[test]
fn expired_historical_v1_policy_stays_valid_but_cannot_override_a_new_funding_epoch() {
    let (_temp, api, source) = fixture();
    let operator = api.principals.principal("operator").unwrap();
    let old_now = now_unix_ms() - 200_000;
    let policy = api
        .store
        .adaptive_resume_policy_draft(
            &operator.principal,
            &source.grant.authority.project_id,
            source.grant.session_id,
            Uuid::new_v4(),
            "historical-finite-policy",
            old_now + 180_000,
            old_now,
        )
        .unwrap();
    let (_, old_receipt) = api
        .store
        .authorize_adaptive_resume_policy(&operator.principal, &policy, old_now)
        .unwrap();
    let current = project(&api, &source);
    reconcile_review_at(&api, &current, old_now);
    let old = api
        .store
        .adaptive_leadership_review_calls(&current.tenant_id, source.grant.session_id)
        .unwrap()
        .pop()
        .unwrap();
    let (accounting, accounting_correction) = api.leadership_accounting_fields(&old).unwrap();
    let retained = LeadershipContext {
        binding: LeadershipAuthority::from_call(&old),
        source: old.context.clone(),
        context_digest: old.context_digest().unwrap(),
        private_observation: None,
        accounting,
        accounting_correction,
    };
    let old_prompt = retained.prompt().unwrap();
    let old_bytes = serde_json::to_vec(&retained).unwrap();
    api.store
        .expire_adaptive_leadership_review_call(
            &old.grant.leadership_principal,
            old.grant.review_id,
            old.version,
            now_unix_ms(),
        )
        .unwrap();
    let request = request(&api, &source);
    assert_ok(&post(&api, &request));
    reconcile_review_at(&api, &current, now_unix_ms());
    let context = selected(&api);
    assert_eq!(
        context
            .binding
            .grant
            .work_funding
            .as_ref()
            .unwrap()
            .binding
            .ordinal,
        2
    );
    assert!(context.accounting.is_none());
    assert!(context.accounting_correction.is_none());
    assert!(context.binding.grant.resume_policy.is_none());
    assert!(!context
        .prompt()
        .unwrap()
        .contains("Validated immutable accounting:"));
    let mut mixed = context.clone();
    mixed.binding.grant.resume_policy = old.grant.resume_policy.clone();
    assert!(mixed.validate_dispatch(now_unix_ms()).is_err());
    assert_eq!(retained.prompt().unwrap(), old_prompt);
    assert_eq!(serde_json::to_vec(&retained).unwrap(), old_bytes);
    retained
        .validate_dispatch(old.grant_issued_at_unix_ms)
        .unwrap();
    assert_eq!(
        api.store
            .adaptive_resume_policy(&current.tenant_id, source.grant.session_id)
            .unwrap(),
        Some(old_receipt)
    );
}

#[test]
fn genuine_continue_adopts_same_session_and_preserves_root_memory_with_exact_effective_caps() {
    let (_temp, api, source) = fixture();
    let request = request(&api, &source);
    assert_ok(&post(&api, &request));
    reconcile_review_at(&api, &project(&api, &source), now_unix_ms());
    let context = selected(&api);
    decide(&api, &context, true);
    let adopted = api
        .store
        .adaptive_session_for_authority(&source.grant.authority)
        .unwrap()
        .unwrap();
    assert_eq!(adopted.grant, source.grant);
    assert_eq!(adopted.model_calls, source.model_calls);
    assert_eq!(adopted.tool_calls, source.tool_calls);
    assert_eq!(
        adopted.active_work_funding(),
        context.binding.grant.work_funding.as_deref()
    );
    assert_eq!(adopted.funded_model_call_ceiling(), 8);
    assert_eq!(adopted.funded_tool_call_ceiling(), 8);
    assert_eq!(
        adopted.effective_grant().max_model_calls,
        adopted.active_model_ceiling()
    );
    assert_eq!(adopted.effective_grant().max_tool_calls, 8);
    let binding = api
        .adaptive_provider_authority(source.grant.authority.agent_id)
        .unwrap()
        .unwrap();
    let model = api.prepare_adaptive_model(&binding).unwrap();
    let memory = model.working_memory.as_ref().unwrap();
    assert_eq!(memory.source.root_model_ceiling, 4);
    assert_eq!(memory.source.root_tool_ceiling, 4);
    assert_eq!(
        memory.source.work_funding.as_deref(),
        adopted.active_work_funding()
    );
    assert_eq!(
        memory.source.active_model_ceiling,
        binding.grant.max_model_calls
    );
    assert!(model.fresh_observation_required);
    assert!(model
        .prompt()
        .unwrap()
        .contains("Separately verified adopted work-funding epoch"));
    let mut wrong = model.clone();
    wrong.binding.grant.max_tool_calls = 4;
    assert!(wrong.validate_dispatch(now_unix_ms()).is_err());
    let mut wrong = model.clone();
    wrong
        .working_memory
        .as_mut()
        .unwrap()
        .source
        .root_model_ceiling = 8;
    assert!(wrong.validate_dispatch(now_unix_ms()).is_err());
    let mut wrong = model.clone();
    wrong.working_memory.as_mut().unwrap().source.work_funding = None;
    assert!(wrong.validate_dispatch(now_unix_ms()).is_err());
    // Shape-only private prompt regression: spending under adopted funding can
    // exceed the original root without applying v1 checked-sub accounting.
    let mut over_root = model.clone();
    over_root.binding.session_version = 3;
    over_root.binding.grant.max_model_calls = 8;
    let memory = over_root.working_memory.as_mut().unwrap();
    memory.source.provider_version = 3;
    memory.source.head_version = 3;
    memory.source.model_calls = 5;
    memory.source.tool_calls = 5;
    memory.source.active_model_ceiling = 8;
    memory.source.continuation_windows = 2;
    over_root.validate_dispatch(now_unix_ms()).unwrap();
    assert!(over_root
        .prompt()
        .unwrap()
        .contains("original remaining model/tool calls 0/0"));
    assert_eq!(
        over_root
            .working_memory
            .as_ref()
            .unwrap()
            .source
            .root_model_ceiling,
        4
    );
    // Synthetic journal inspection progress, never a live Workbench claim.
    let effect = sentinel_workflow::AdaptiveEffectV1 {
        id: Uuid::new_v4(),
        request_digest: "a".repeat(64),
    };
    let tool_effect = sentinel_workflow::AdaptiveEffectV1 {
        id: Uuid::new_v4(),
        request_digest: "c".repeat(64),
    };
    let tool = sentinel_common::WorkbenchTool::InspectFile {
        path: "src/main.rs".into(),
        max_bytes: 1024,
    };
    let tool_digest = sentinel_workflow::adaptive_tool_digest(&tool).unwrap();
    let mut progressed = adopted.clone();
    for command in [
        AdaptiveTransitionV1::ClaimModel {
            effect: effect.clone(),
            previous_observation_digest: None,
        },
        AdaptiveTransitionV1::ResolveModel {
            effect,
            result_digest: "b".repeat(64),
            decision: sentinel_workflow::AdaptiveModelDecisionV1::Tool {
                tool,
                tool_digest: tool_digest.clone(),
            },
        },
        AdaptiveTransitionV1::ClaimTool {
            effect: tool_effect.clone(),
            tool_digest,
        },
        AdaptiveTransitionV1::ObserveTool {
            observation: sentinel_workflow::AdaptiveObservationRefV1 {
                effect: tool_effect,
                observation_digest: "d".repeat(64),
            },
        },
    ] {
        progressed = api
            .store
            .advance_adaptive_session(
                source.grant.session_id,
                progressed.version,
                Uuid::new_v4(),
                &command,
                &source.grant.authority,
                now_unix_ms(),
            )
            .unwrap()
            .1;
    }
    // A normally progressed adopted head can use the finite epoch's next global ordinal.
    reconcile_review_at(
        &api,
        &project(&api, &source),
        adopted.active_deadline_ms() + 1,
    );
    let calls = api
        .store
        .adaptive_leadership_review_calls(
            &source.grant.authority.tenant_id,
            source.grant.session_id,
        )
        .unwrap();
    assert_eq!(calls.len(), 2);
    let next = calls
        .iter()
        .find(|call| call.grant.review_id != context.binding.grant.review_id)
        .unwrap();
    let epoch = next.grant.work_funding.as_ref().unwrap();
    assert!(epoch.same_epoch(context.binding.grant.work_funding.as_ref().unwrap()));
    assert_eq!(epoch.binding.ordinal, 2);
}

#[test]
fn public_events_and_projection_never_expose_funding_receipts_or_private_context() {
    let (_temp, api, source) = fixture();
    assert_ok(&post(&api, &request(&api, &source)));
    reconcile_review_at(&api, &project(&api, &source), now_unix_ms());
    let context = selected(&api);
    let call = api
        .store
        .adaptive_leadership_review_calls(
            &source.grant.authority.tenant_id,
            source.grant.session_id,
        )
        .unwrap()
        .pop()
        .unwrap();
    let public = assert_ok(&public_workflow_read(&serde_json::json!({
        "project":project(&api, &source), "review":context,
        "raw_review":call,
        "epoch":context.binding.grant.work_funding,
    })));
    assert!(public["raw_review"].get("context").is_none());
    let bytes = public.to_string();
    for key in [
        "issuer_principal",
        "resume_source",
        "private_observation",
        "leadership_principal",
        "leadership_authority",
        "assignee_authority",
        "prompt",
    ] {
        assert!(!bytes.contains(&format!("\"{key}\"")), "{key}");
    }
    for name in [
        "adaptive_work_funding_issued",
        "adaptive_work_funding_review_issued",
        "adaptive_work_funding_adopted",
    ] {
        assert!(private_work_funding_event(name));
    }
    assert!(!private_work_funding_event(
        "adaptive_leadership_review_issued"
    ));
    let operator = api.principals.principal("operator").unwrap();
    let events = assert_ok(&api.events(&operator, OPERATOR_EVENTS_PATH));
    assert!(events
        .as_array()
        .unwrap()
        .iter()
        .all(|event| !private_work_funding_event(event["event_type"].as_str().unwrap())));
    let unfunded = serde_json::json!({"legacy":"unchanged", "source":{"root_model_ceiling":4}});
    assert_eq!(
        public_workflow_read(&unfunded).body,
        json(200, &unfunded).body
    );
}
