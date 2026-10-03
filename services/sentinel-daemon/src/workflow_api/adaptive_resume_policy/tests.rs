#![cfg(test)]
//! Synthetic policy/evidence fixtures, not live provider or leadership results.
use super::super::adaptive_continuation_tests::fixture_schema2_recovery_source;
use super::super::adaptive_leadership_review::tests::discovery_state;
use super::super::adaptive_leadership_review::tests::{exhaust_review_history, persist};
use super::super::adaptive_leadership_review::tests::{
    reconcile_review_at, seed_planning_receipt_with_catalog,
};
use super::super::adaptive_leadership_review::{LeadershipAuthority, LeadershipContext};
use super::super::model_execution::{ModelExecutionCompletion, ModelExecutionContext};
use super::*;
use sentinel_workflow::{
    adaptive_leadership_continuation_provider_authority_digest, AdaptiveEffectV1,
    AdaptiveLeadershipReviewDecisionKindV1, AdaptiveLeadershipReviewDecisionV1,
    AdaptiveSessionGrantV1, ClaimAdaptiveLeadershipReviewCallV1,
    CompleteAdaptiveLeadershipReviewCallV1,
};

fn unreviewed_policy_root(
    duration_ms: u64,
    unknown: bool,
) -> (tempfile::TempDir, WorkflowApi, AdaptiveSessionV1) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("company.sqlite");
    let mut api = super::super::model_work::configured_test_api(&path);
    let created = now_unix_ms() - 600_000;
    let binding = super::super::model_work::assign_test_work_from_at(&api, Some(64), 0, created);
    api.subscription_allowance_id = Some(binding.reservation_id);
    api.event_store = Some(
        sentinel_limbo::EventStore::open(temp.path().join("events.sqlite").to_str().unwrap())
            .unwrap(),
    );
    let tenant = TenantId::parse(&binding.tenant_id).unwrap();
    let project_id = ProjectId::parse(&binding.project_id).unwrap();
    let work_item_id = WorkItemId::parse(&binding.work_item_id).unwrap();
    let project = api
        .store
        .company_project(&tenant, &project_id)
        .unwrap()
        .unwrap();
    let allowance = project.subscription_call.as_ref().unwrap();
    let authority = api
        .authority
        .as_ref()
        .unwrap()
        .snapshot_for_admission(&tenant, &project_id, &work_item_id, binding.agent_id, false)
        .unwrap();
    // A synthetic narrowed session root; ordinary subscription admission remains 120s.
    let grant = AdaptiveSessionGrantV1 {
        schema_version: 1,
        session_id: Uuid::new_v4(),
        authority: authority.clone(),
        provider_allowance_id: allowance.allowance_id.clone(),
        provider_authority_digest: adaptive_leadership_continuation_provider_authority_digest(
            allowance, &authority,
        )
        .unwrap(),
        provider: allowance.grant.provider.clone(),
        model: allowance.grant.model.clone(),
        catalog_digest: allowance.grant.catalog_digest.clone(),
        max_output_tokens: 4_096,
        max_call_duration_ms: duration_ms,
        max_model_calls: 64,
        max_tool_calls: 64,
        created_at_ms: created,
        deadline_ms: allowance.grant.expires_at_unix_ms,
    };
    let mut session = api
        .store
        .begin_adaptive_session(&grant, &authority, created)
        .unwrap()
        .1;
    if unknown {
        use super::super::model_execution::{
            AdaptiveProviderAuthority, ProviderExecutionAuthority,
        };
        let effect = AdaptiveEffectV1 {
            id: Uuid::new_v4(),
            request_digest: "a".repeat(64),
        };
        let assignment = project.work_items[&work_item_id]
            .assignments
            .iter()
            .find(|assignment| assignment.active)
            .unwrap();
        let provider_authority =
            ProviderExecutionAuthority::Adaptive(Box::new(AdaptiveProviderAuthority {
                schema_version: 3,
                grant: grant.clone(),
                session_version: session.version,
                effect_id: effect.id,
                assignment_id: assignment.assignment_id.clone(),
                previous_observation: None,
            }));
        let request_id = provider_authority.request_id();
        let events = api.event_store.as_ref().unwrap();
        events
            .reserve_llm_request(
                &request_id,
                &effect.request_digest,
                &authority.agent_id.to_string(),
            )
            .unwrap();
        events
            .bind_llm_model_reservation(&sentinel_limbo::LlmModelReservationV1 {
                schema_version: 1,
                request_id: request_id.clone(),
                request_digest: effect.request_digest.clone(),
                owner_scope: sentinel_common::StateTransferScope::for_agent(
                    authority.agent_id.to_string(),
                ),
                subject: sentinel_limbo::LlmModelSubjectV1::Adaptive {
                    session_id: grant.session_id,
                    effect_id: effect.id,
                    session_version: session.version,
                },
                allowance_id: grant.provider_allowance_id.clone(),
                context_digest: "d".repeat(64),
                authority_digest: sentinel_common::sha256_hex(
                    &serde_json::to_vec(&provider_authority).unwrap(),
                ),
                usage_binding: sentinel_limbo::LlmModelUsageBindingV1 {
                    agent_id: authority.agent_id,
                    tenant_id: tenant.0,
                    project_id: project_id.0,
                    work_item_id: work_item_id.0,
                    reservation_id: grant.provider_allowance_id.clone(),
                    assignment_id: assignment.assignment_id.clone(),
                    assignment_version: authority.assignment_version,
                    provider: grant.provider.clone(),
                    model: grant.model.clone(),
                },
            })
            .unwrap();
        assert!(events
            .mark_llm_provider_outcome_unknown(
                &request_id,
                &effect.request_digest,
                "UnknownOutcome: provider_transport_deadline_elapsed"
            )
            .unwrap());
        for (offset, command) in [
            AdaptiveTransitionV1::ClaimModel {
                effect: effect.clone(),
                previous_observation_digest: None,
            },
            AdaptiveTransitionV1::MarkUnknown { effect },
        ]
        .iter()
        .enumerate()
        {
            session = api
                .store
                .advance_adaptive_session(
                    grant.session_id,
                    session.version,
                    Uuid::new_v4(),
                    command,
                    &authority,
                    created + 1 + offset as u64,
                )
                .unwrap()
                .1;
        }
    }
    seed_planning_receipt_with_catalog(&api, &path, &project, &grant.catalog_digest);
    (temp, api, session)
}

#[test]
fn policy_review_duration_is_capped_by_the_30_second_root_not_120_second_planning() {
    let (_temp, api, source) = unreviewed_policy_root(30_000, true);
    let request = draft(&api, &source);
    assert_eq!(request.limits.max_call_duration_ms, 30_000);
    assert_eq!(issue(&api, &request).status, 200);
    let project = api
        .store
        .company_project(
            &source.grant.authority.tenant_id,
            &source.grant.authority.project_id,
        )
        .unwrap()
        .unwrap();
    let planning = api
        .store
        .project_planning_call(&project.tenant_id, &project.project_id)
        .unwrap()
        .unwrap();
    assert_eq!(planning.grant.max_duration_ms, 120_000);
    assert!(reconcile_review_at(&api, &project, now_unix_ms()));
    let calls = api
        .store
        .adaptive_leadership_review_calls(&project.tenant_id, source.grant.session_id)
        .unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].grant.max_duration_ms, 30_000);
    api.validate_resume_policy_review(&calls[0]).unwrap();
    assert_eq!(
        api.store
            .adaptive_session_for_authority(&source.grant.authority)
            .unwrap(),
        Some(source)
    );
}

#[test]
fn policy_same_head_retirement_history_is_bounded_and_ordinals_remain_distinct() {
    use sha2::{Digest, Sha256};
    let (temp, api, source) = unreviewed_policy_root(120_000, false);
    let mut request = draft(&api, &source);
    request.limits.total_review_ceiling = 35;
    request.limits.total_window_ceiling = 35;
    assert_eq!(issue(&api, &request).status, 200);
    let project = api
        .store
        .company_project(
            &source.grant.authority.tenant_id,
            &source.grant.authority.project_id,
        )
        .unwrap()
        .unwrap();
    let leader = api.principals.principal("pm").unwrap();
    let mut now = now_unix_ms();
    let mut history = Vec::new();
    let mut ids = std::collections::BTreeSet::new();
    for ordinal in 1..=35 {
        assert!(reconcile_review_at(&api, &project, now));
        let calls = api
            .store
            .adaptive_leadership_review_calls(&project.tenant_id, source.grant.session_id)
            .unwrap();
        assert_eq!(calls.len(), usize::from(ordinal));
        let pending: Vec<_> = calls
            .iter()
            .filter(|call| call.decision.is_none() && call.retired_at_unix_ms.is_none())
            .collect();
        assert_eq!(pending.len(), 1);
        let call = pending[0];
        assert_eq!(call.grant.resume_policy.as_ref().unwrap().ordinal, ordinal);
        assert_eq!(call.context.source_session, source);
        assert!(ids.insert(call.grant.review_id));
        assert!(call.context.evidence_refs.len() <= 32);
        assert!(!call
            .context
            .evidence_refs
            .iter()
            .any(|reference| reference.starts_with("leadership-review-retired:")));
        let refs: Vec<_> = call
            .context
            .evidence_refs
            .iter()
            .filter(|reference| reference.starts_with("leadership-review-retirement-history:"))
            .collect();
        if history.is_empty() {
            assert!(refs.is_empty());
        } else {
            history.sort_unstable();
            let expected = format!(
                "leadership-review-retirement-history:{}:{:x}",
                history.len(),
                Sha256::digest(serde_json::to_vec(&history).unwrap())
            );
            assert_eq!(refs, vec![&expected]);
        }
        api.validate_resume_policy_review(call).unwrap();
        let before = discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
        );
        assert!(reconcile_review_at(&api, &project, now));
        assert_eq!(
            discovery_state(
                &temp.path().join("company.sqlite"),
                &temp.path().join("events.sqlite")
            ),
            before
        );
        now = call.grant.expires_at_unix_ms;
        let retired = api
            .store
            .expire_adaptive_leadership_review_call(
                &leader.principal,
                call.grant.review_id,
                call.version,
                now,
            )
            .unwrap();
        history.push((call.grant.review_id, retired.retired_at_unix_ms.unwrap()));
        now += 1;
    }
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    assert!(reconcile_review_at(&api, &project, now));
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite")
        ),
        before
    );
    assert_eq!(
        api.store
            .adaptive_session_for_authority(&source.grant.authority)
            .unwrap(),
        Some(source)
    );
}

#[test]
fn successor_policy_refusal_is_not_reopened_by_project_metadata() {
    let (temp, api, source) = continued_policy_fixture(false);
    let mut project = api
        .store
        .company_project(
            &source.grant.authority.tenant_id,
            &source.grant.authority.project_id,
        )
        .unwrap()
        .unwrap();
    let now = source.active_deadline_ms();
    assert!(reconcile_review_at(&api, &project, now));
    let calls = api
        .store
        .adaptive_leadership_review_calls(&project.tenant_id, source.grant.session_id)
        .unwrap();
    let call = calls
        .iter()
        .find(|call| call.retired_at_unix_ms.is_none() && call.decision.is_none())
        .unwrap();
    let receipt = api
        .store
        .adaptive_resume_policy(&project.tenant_id, source.grant.session_id)
        .unwrap()
        .unwrap();
    assert!(source.version > receipt.request.source.expected_session_version);
    let leader = api.principals.principal("pm").unwrap();
    let claimed = api
        .store
        .claim_adaptive_leadership_review_call(
            &leader.principal,
            &ClaimAdaptiveLeadershipReviewCallV1 {
                review_id: call.grant.review_id,
                allowance_id: call.allowance_id.clone(),
                request_id: call.request_id(),
                request_digest: "c".repeat(64),
                context_digest: call.context_digest().unwrap(),
            },
            now + 1,
        )
        .unwrap();
    api.store
        .complete_adaptive_leadership_review_call(
            &leader.principal,
            &CompleteAdaptiveLeadershipReviewCallV1 {
                review_id: claimed.grant.review_id,
                allowance_id: claimed.allowance_id.clone(),
                request_digest: "c".repeat(64),
                model_response_digest: "d".repeat(64),
                decision: AdaptiveLeadershipReviewDecisionV1 {
                    schema_version: 3,
                    decision: AdaptiveLeadershipReviewDecisionKindV1::DeferBudget {
                        rationale: "Fixture-only terminal successor refusal".into(),
                        evidence_refs: vec![claimed.context.evidence_refs[0].clone()],
                    },
                },
                resolution_event_id: None,
                continuation: None,
            },
            now + 2,
        )
        .unwrap();
    api.store
        .apply_company_command(
            &leader.principal,
            Uuid::new_v4(),
            &CompanyWorkflowCommandV1::RecordDecision {
                project_id: project.project_id.clone(),
                expected_version: project.version,
                work_item_id: Some(source.grant.authority.work_item_id.clone()),
                choice_ref: "metadata-only-change".into(),
                rationale_ref: "Same refused session".into(),
            },
            now + 3,
        )
        .unwrap();
    project = api
        .store
        .company_project(&project.tenant_id, &project.project_id)
        .unwrap()
        .unwrap();
    assert_ne!(project, claimed.context.source_project);
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    for _ in 0..2 {
        assert!(reconcile_review_at(&api, &project, now + 4));
        assert_eq!(
            discovery_state(
                &temp.path().join("company.sqlite"),
                &temp.path().join("events.sqlite")
            ),
            before
        );
    }
    assert_eq!(
        api.store
            .adaptive_session_for_authority(&source.grant.authority)
            .unwrap(),
        Some(source)
    );
}

fn fixture(unknown: bool) -> (tempfile::TempDir, WorkflowApi, AdaptiveSessionV1) {
    if !unknown {
        return super::super::budget_window_tests::exhausted_budget_review_fixture();
    }
    let temp = tempfile::tempdir().unwrap();
    let (api, context) = fixture_schema2_recovery_source(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
        true,
    );
    exhaust_review_history(&api, &context);
    (temp, api, context.source.source_session)
}

fn query(session: &AdaptiveSessionV1) -> String {
    format!(
        "{ADAPTIVE_RESUME_POLICY_PATH}?project_id={}&session_id={}",
        session.grant.authority.project_id, session.grant.session_id
    )
}

fn draft(api: &WorkflowApi, session: &AdaptiveSessionV1) -> AdaptiveResumePolicyRequestV1 {
    let operator = api.principals.principal("operator").unwrap();
    let response = api.adaptive_resume_policy_http(&operator, "GET", &query(session), &[]);
    assert_eq!(
        response.status,
        200,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    let body: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(body["requires_explicit_submission"], true);
    assert_eq!(body["model_decision_recorded"], false);
    assert_eq!(body["developer_window_created"], false);
    serde_json::from_value(body["request"].clone()).unwrap()
}

fn issue(api: &WorkflowApi, request: &AdaptiveResumePolicyRequestV1) -> WorkflowHttpResponse {
    api.adaptive_resume_policy_http(
        &api.principals.principal("operator").unwrap(),
        "POST",
        ADAPTIVE_RESUME_POLICY_PATH,
        &serde_json::to_vec(request).unwrap(),
    )
}

fn claim_review(api: &WorkflowApi, context: &LeadershipContext) -> (String, String) {
    let grant = &context.binding.grant;
    let id = format!("company-leadership-{}", grant.review_id);
    let digest = "c".repeat(64);
    api.event_store
        .as_ref()
        .unwrap()
        .reserve_llm_request(
            &id,
            &digest,
            &grant.leadership_principal.agent_id.unwrap().to_string(),
        )
        .unwrap();
    let kind = if grant.schema_version == 2 {
        "unknown_model"
    } else {
        "budget_window_exhausted"
    };
    let response = api.subscription_dispatch(&serde_json::to_vec(&serde_json::json!({
        "schema_version":5, "allowance_id":context.binding.allowance_id,
        "agent_id":grant.leadership_principal.agent_id.unwrap().0,
        "request_id":id, "request_digest":digest, "context_digest":context.context_digest,
        "provider":grant.provider, "model":grant.model, "catalog_digest":grant.catalog_digest,
        "subject":{"kind":"adaptive_leadership_review","review_id":grant.review_id,"review_kind":kind},
    })).unwrap());
    assert_eq!(
        response.status,
        200,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    (id, digest)
}

fn policy_decision_fixture(
    unknown: bool,
    continued: bool,
) -> (tempfile::TempDir, WorkflowApi, AdaptiveSessionV1) {
    let (temp, api, source) = fixture(unknown);
    let request = draft(&api, &source);
    let issued = issue(&api, &request);
    assert_eq!(
        issued.status,
        200,
        "{}",
        String::from_utf8_lossy(&issued.body)
    );
    let project = api
        .store
        .company_project(
            &source.grant.authority.tenant_id,
            &source.grant.authority.project_id,
        )
        .unwrap()
        .unwrap();
    {
        let _fence = api.mutation_fence.write().unwrap();
        assert!(api.reconcile_adaptive_leadership_reviews(&project).unwrap());
    }
    let leader = api.principals.principal("pm").unwrap();
    let call = api
        .leadership_review_for_agent(leader.principal.agent_id.unwrap())
        .unwrap()
        .unwrap();
    let binding = call.grant.resume_policy.as_ref().unwrap();
    assert_eq!(binding.ordinal, request.source.base_review_count + 1);
    assert_eq!(call.grant.schema_version, if unknown { 2 } else { 3 });
    let context = api
        .prepare_leadership_review(&LeadershipAuthority::from_call(&call))
        .unwrap();
    let prompt = context.prompt().unwrap();
    assert!(prompt.contains("immutable session-wide resume policy"));
    assert!(prompt.contains("Original model allowance:"));
    assert!(prompt.contains("Original tool allowance:"));
    assert!(prompt.contains("issuance and replay never reset or extend it"));
    let (id, digest) = claim_review(&api, &context);
    let decision = if continued {
        serde_json::json!({"kind":"continue","additional_model_calls":1,"window_ms":300_000,
            "rationale":"Fixture-only finite continuation.","evidence_refs":[context.source.evidence_refs[0]]})
    } else {
        serde_json::json!({"kind":if unknown {"keep_unknown"} else {"defer_budget"},
            "rationale":"Fixture-only refusal.","evidence_refs":[context.source.evidence_refs[0]]})
    };
    let completion = ModelExecutionCompletion {
        context: ModelExecutionContext::AdaptiveLeadershipReview(Box::new(context.clone())),
        content:
            serde_json::json!({"schema_version":call.grant.schema_version,"decision":decision})
                .to_string(),
        admissible: true,
    };
    persist(&api, &completion, &context, &id, &digest, true);
    api.accept_leadership_review_at(&completion, &context, &id, &digest, now_unix_ms())
        .unwrap();
    let next = api
        .store
        .adaptive_session_for_authority(&source.grant.authority)
        .unwrap()
        .unwrap();
    assert_eq!(next.grant, source.grant);
    assert_eq!(next.model_calls, source.model_calls);
    assert_eq!(next.tool_calls, source.tool_calls);
    let old_windows = source
        .continuation
        .as_ref()
        .map_or(0, |state| state.authorizations.len());
    assert_eq!(
        next.continuation
            .as_ref()
            .map_or(0, |state| state.authorizations.len()),
        old_windows + usize::from(continued)
    );
    if continued {
        let authorization = next
            .continuation
            .as_ref()
            .unwrap()
            .authorizations
            .last()
            .unwrap();
        assert_eq!(
            authorization.resume_policy.as_ref(),
            call.grant.resume_policy.as_ref()
        );
        assert_eq!(
            next.effective_grant().max_call_duration_ms,
            source.grant.max_call_duration_ms
        );
    }
    (temp, api, next)
}

pub(crate) fn continued_policy_fixture(
    unknown: bool,
) -> (tempfile::TempDir, WorkflowApi, AdaptiveSessionV1) {
    policy_decision_fixture(unknown, true)
}

#[test]
fn both_heads_issue_only_finite_policy_and_replay_without_source_or_provider_renewal() {
    for unknown in [false, true] {
        let (temp, mut api, source) = fixture(unknown);
        let company = temp.path().join("company.sqlite");
        let events = temp.path().join("events.sqlite");
        let before = discovery_state(&company, &events);
        let before_ms = now_unix_ms();
        let request = draft(&api, &source);
        assert!(
            request.limits.expires_at_unix_ms
                >= before_ms + sentinel_workflow::ADAPTIVE_RESUME_MAX_POLICY_MS
        );
        assert!(
            request.limits.expires_at_unix_ms
                <= now_unix_ms() + sentinel_workflow::ADAPTIVE_RESUME_MAX_POLICY_MS
        );
        assert_eq!(discovery_state(&company, &events), before);
        let issued = issue(&api, &request);
        assert_eq!(
            issued.status,
            200,
            "{}",
            String::from_utf8_lossy(&issued.body)
        );
        assert_eq!(
            api.store
                .adaptive_session_for_authority(&source.grant.authority)
                .unwrap(),
            Some(source.clone())
        );
        let sealed = discovery_state(&company, &events);
        api.event_store = None;
        let replay = issue(&api, &request);
        assert_eq!(replay.status, 200);
        let body: serde_json::Value = serde_json::from_slice(&replay.body).unwrap();
        assert_eq!(body["replayed"], true);
        assert_eq!(body["model_decision_recorded"], false);
        assert_eq!(body["developer_window_created"], false);
        assert_eq!(discovery_state(&company, &events), sealed);
        let mut changed = request.clone();
        changed.operation_id = Uuid::new_v4();
        assert_eq!(issue(&api, &changed).status, 409);
        let receipt = api
            .store
            .adaptive_resume_policy(&source.grant.authority.tenant_id, source.grant.session_id)
            .unwrap()
            .unwrap();
        let (replayed, expired_replay) = api
            .store
            .authorize_adaptive_resume_policy(
                &api.principals.principal("operator").unwrap().principal,
                &request,
                request.limits.expires_at_unix_ms + 1,
            )
            .unwrap();
        assert!(replayed);
        assert_eq!(expired_replay, receipt);
        assert_eq!(discovery_state(&company, &events), sealed);
    }
}

#[test]
fn policy_api_denies_roles_foreign_tenant_spoofed_fields_and_unknown_proof() {
    let (temp, mut api, source) = fixture(true);
    let request = draft(&api, &source);
    let company = temp.path().join("company.sqlite");
    let events = temp.path().join("events.sqlite");
    let before = discovery_state(&company, &events);
    for principal in api.principals.by_principal_id.values() {
        if principal.principal.kind != CompanyPrincipalKindV1::Operator {
            assert_eq!(
                api.adaptive_resume_policy_http(principal, "GET", &query(&source), &[])
                    .status,
                403
            );
            assert_eq!(
                api.adaptive_resume_policy_http(
                    principal,
                    "POST",
                    ADAPTIVE_RESUME_POLICY_PATH,
                    &serde_json::to_vec(&request).unwrap()
                )
                .status,
                403
            );
        }
    }
    let mut foreign = request.clone();
    foreign.source.tenant_id = TenantId::parse("foreign-tenant").unwrap();
    assert_eq!(issue(&api, &foreign).status, 403);
    let mut spoofed = serde_json::to_value(&request).unwrap();
    spoofed["issuer_principal"] = serde_json::json!("operator");
    assert_eq!(
        api.adaptive_resume_policy_http(
            &api.principals.principal("operator").unwrap(),
            "POST",
            ADAPTIVE_RESUME_POLICY_PATH,
            &serde_json::to_vec(&spoofed).unwrap()
        )
        .status,
        400
    );
    let mut forged = request.clone();
    let AdaptiveResumeSubjectV1::ModelUnknown {
        sealed_unknown_proof_digest,
        ..
    } = &mut forged.source.subject
    else {
        panic!("unknown source fixture");
    };
    *sealed_unknown_proof_digest = "f".repeat(64);
    assert_eq!(issue(&api, &forged).status, 403);
    api.model_work_enabled = false;
    assert_eq!(issue(&api, &request).status, 503);
    api.model_work_enabled = true;
    api.event_store = None;
    assert_eq!(issue(&api, &request).status, 403);
    assert_eq!(discovery_state(&company, &events), before);
}

#[test]
fn policy_operator_technical_lead_is_accepted_but_changed_registered_binding_is_not() {
    let (temp, mut api, session) = fixture(true);
    let mut principals = PrincipalAuthenticator {
        by_principal_id: api.principals.by_principal_id.clone(),
        by_credential_digest: api.principals.by_credential_digest.clone(),
    };
    for bound in principals
        .by_principal_id
        .values_mut()
        .chain(principals.by_credential_digest.values_mut())
        .filter(|bound| bound.principal.principal_id == "operator")
    {
        bound.principal.role = CompanyRoleV1::TechnicalLead;
    }
    api.principals = Arc::new(principals);
    let request = draft(&api, &session);
    let operator = api.principals.principal("operator").unwrap();
    let mut spoofed = operator.clone();
    spoofed.principal.authority_generation += 1;
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    assert_eq!(
        api.adaptive_resume_policy_http(
            &spoofed,
            "POST",
            ADAPTIVE_RESUME_POLICY_PATH,
            &serde_json::to_vec(&request).unwrap()
        )
        .status,
        403
    );
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite")
        ),
        before
    );
    assert_eq!(issue(&api, &request).status, 200);
    let receipt = api
        .store
        .adaptive_resume_policy(&session.grant.authority.tenant_id, session.grant.session_id)
        .unwrap()
        .unwrap();
    assert_eq!(receipt.issuer_principal.role, CompanyRoleV1::TechnicalLead);
}

#[test]
fn policy_post_rejects_changed_assignment_without_writing_or_renewing() {
    let (temp, mut api, session) = fixture(true);
    let request = draft(&api, &session);
    let call = api
        .store
        .adaptive_leadership_review_calls(
            &session.grant.authority.tenant_id,
            session.grant.session_id,
        )
        .unwrap()
        .pop()
        .unwrap();
    let context = LeadershipContext {
        binding: LeadershipAuthority::from_call(&call),
        source: call.context.clone(),
        context_digest: call.context_digest().unwrap(),
        private_observation: None,
    };
    super::super::adaptive_leadership_review::tests::change_review_assignee(
        &mut api,
        &context,
        "assignment",
    );
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    assert_eq!(issue(&api, &request).status, 403);
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite")
        ),
        before
    );
}

#[test]
fn installed_policy_keeps_terminal_legacy_history_but_rejects_fresh_legacy_slots() {
    let (temp, api, session) = fixture(true);
    let request = draft(&api, &session);
    assert_eq!(issue(&api, &request).status, 200);
    let call = api
        .store
        .adaptive_leadership_review_calls(
            &session.grant.authority.tenant_id,
            session.grant.session_id,
        )
        .unwrap()
        .pop()
        .unwrap();
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    api.store.company_projects().unwrap();
    assert!(call.retired_at_unix_ms.is_some());
    api.validate_resume_policy_review(&call).unwrap();
    let mut fresh = call.clone();
    fresh.retired_at_unix_ms = None;
    assert!(api.validate_resume_policy_review(&fresh).is_err());
    let mut completed = fresh.clone();
    completed.decision = Some(sentinel_workflow::AdaptiveLeadershipReviewDecisionV1 {
        schema_version: 2,
        decision: sentinel_workflow::AdaptiveLeadershipReviewDecisionKindV1::KeepUnknown {
            rationale: "Fixture-only historical refusal.".into(),
            evidence_refs: vec![call.context.evidence_refs[0].clone()],
        },
    });
    api.validate_resume_policy_review(&completed).unwrap();
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite")
        ),
        before
    );
}

#[test]
fn both_policy_heads_continue_only_after_real_fixture_decision() {
    for unknown in [false, true] {
        continued_policy_fixture(unknown);
    }
}

#[test]
fn policy_review_refusal_is_terminal_for_the_same_head() {
    for unknown in [false, true] {
        let (temp, api, session) = policy_decision_fixture(unknown, false);
        let project = api
            .store
            .company_project(
                &session.grant.authority.tenant_id,
                &session.grant.authority.project_id,
            )
            .unwrap()
            .unwrap();
        let before = discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
        );
        {
            let _fence = api.mutation_fence.write().unwrap();
            assert!(api.reconcile_adaptive_leadership_reviews(&project).unwrap());
        }
        assert!(api
            .leadership_review_for_agent(
                api.principals
                    .principal("pm")
                    .unwrap()
                    .principal
                    .agent_id
                    .unwrap()
            )
            .unwrap()
            .is_none());
        assert_eq!(
            discovery_state(
                &temp.path().join("company.sqlite"),
                &temp.path().join("events.sqlite")
            ),
            before
        );
    }
}

#[test]
fn transaction_safe_unknown_proof_matches_existing_evidence_without_mutations() {
    let temp = tempfile::tempdir().unwrap();
    let company = temp.path().join("company.sqlite");
    let events = temp.path().join("events.sqlite");
    let (api, context) = fixture_schema2_recovery_source(&company, &events, true);
    let project = &context.source.source_project;
    let session = &context.source.source_session;
    let AdaptiveCursorV1::ModelUnknown { effect } = &session.cursor else {
        panic!("fixture must retain the exact unknown model effect");
    };
    let before = discovery_state(&company, &events);
    let expected = api
        .read_only_unknown_model_proof_digest(project, session, effect)
        .unwrap()
        .unwrap();
    assert_eq!(
        api.adaptive_resume_policy_source_proof(project, session)
            .unwrap(),
        Some(expected),
    );
    assert_eq!(discovery_state(&company, &events), before);
}

#[test]
fn unknown_proof_rejects_changed_effect_tenant_assignment_and_missing_event_store() {
    let temp = tempfile::tempdir().unwrap();
    let company = temp.path().join("company.sqlite");
    let events = temp.path().join("events.sqlite");
    let (mut api, context) = fixture_schema2_recovery_source(&company, &events, true);
    let project = &context.source.source_project;
    let session = &context.source.source_session;
    let before = discovery_state(&company, &events);
    let mut changed = session.clone();
    let AdaptiveCursorV1::ModelUnknown { effect } = &mut changed.cursor else {
        panic!("fixture must retain the exact unknown model effect");
    };
    effect.id = Uuid::new_v4();
    assert!(api
        .adaptive_resume_policy_source_proof(project, &changed)
        .is_err());
    let mut foreign = project.clone();
    foreign.tenant_id = TenantId::parse("foreign-tenant").unwrap();
    assert!(api
        .adaptive_resume_policy_source_proof(&foreign, session)
        .is_err());
    let mut reassigned = project.clone();
    reassigned
        .work_items
        .get_mut(&session.grant.authority.work_item_id)
        .unwrap()
        .assignments
        .iter_mut()
        .find(|assignment| assignment.active)
        .unwrap()
        .assignment_id
        .push_str("-changed");
    assert!(api
        .adaptive_resume_policy_source_proof(&reassigned, session)
        .is_err());
    api.event_store = None;
    assert!(api
        .adaptive_resume_policy_source_proof(project, session)
        .is_err());
    assert_eq!(discovery_state(&company, &events), before);
}
