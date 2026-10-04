//! Exact reauthorization and final dispatch must not reschedule an employee's queue.

use super::adaptive_leadership_review::tests::discovery_state;
use super::model_execution::{
    AdaptiveProviderAuthority, ModelExecutionContext, ProviderExecutionAuthority,
};
use super::model_work::{assign_test_work_from_at, configured_adaptive_test_api};
use super::*;
use crate::llm_bridge::bridge::ProviderUsageAuthorityResolver;

fn two_project_fixture(
    path: &Path,
    events: &Path,
) -> (
    WorkflowApi,
    AdaptiveProviderAuthority,
    AdaptiveProviderAuthority,
) {
    let (api, a, _) = configured_adaptive_test_api(path, events);
    // Both projects use real accepted proposals, assignments and independent grants.
    // A is strictly older, so fresh B cannot win an equal-priority queue tie.
    let work_b = assign_test_work_from_at(
        &api,
        Some(8),
        100,
        now_unix_ms().max(a.grant.created_at_ms + 1),
    );
    let projects = api.store.company_projects().unwrap();
    assert_eq!(projects.len(), 2);
    let selected_b =
        select_provider_usage_binding(&projects, work_b.agent_id, Some(&work_b.reservation_id))
            .unwrap()
            .unwrap();
    assert_eq!(selected_b.project_id, work_b.project_id);
    let (b, reconciled) = api
        .adaptive_provider_authority_from_binding(selected_b, false)
        .unwrap();
    assert!(!reconciled);
    let b = b.unwrap();
    assert_eq!(a.grant.authority.agent_id, b.grant.authority.agent_id);
    assert_eq!(a.grant.authority.tenant_id, b.grant.authority.tenant_id);
    assert_ne!(a.grant.authority.project_id, b.grant.authority.project_id);
    assert_ne!(a.grant.provider_allowance_id, b.grant.provider_allowance_id);
    assert_ne!(a.grant.session_id, b.grant.session_id);
    assert_ne!(a.effect_id, b.effect_id);
    assert!(a.grant.created_at_ms < b.grant.created_at_ms);
    (api, a, b)
}

fn session(api: &WorkflowApi, binding: &AdaptiveProviderAuthority) -> AdaptiveSessionV1 {
    api.core
        .adaptive_session(binding.grant.session_id, &binding.grant.authority)
        .unwrap()
        .unwrap()
}

#[test]
fn adaptive_queue_rotates_equal_priority_projects_after_durable_progress() {
    use sentinel_workflow::{
        adaptive_tool_digest, AdaptiveModelDecisionV1, AdaptiveObservationRefV1,
        AdaptiveTransitionV1,
    };
    let temp = tempfile::tempdir().unwrap();
    let (api, a, b) = two_project_fixture(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    let base = now_unix_ms().max(b.grant.created_at_ms) + 1;
    let finish_inspect = |binding: &AdaptiveProviderAuthority, at: u64| {
        let tool = WorkbenchTool::InspectFile {
            path: "src/main.rs".into(),
            max_bytes: 16,
        };
        let tool_digest = adaptive_tool_digest(&tool).unwrap();
        let effect = AdaptiveEffectV1 {
            id: Uuid::new_v4(),
            request_digest: "a".repeat(64),
        };
        let tool_effect = AdaptiveEffectV1 {
            id: Uuid::new_v4(),
            request_digest: "b".repeat(64),
        };
        for (offset, command) in [
            AdaptiveTransitionV1::ClaimModel {
                effect: effect.clone(),
                previous_observation_digest: None,
            },
            AdaptiveTransitionV1::ResolveModel {
                effect,
                result_digest: "c".repeat(64),
                decision: AdaptiveModelDecisionV1::Tool {
                    tool,
                    tool_digest: tool_digest.clone(),
                },
            },
            AdaptiveTransitionV1::ClaimTool {
                effect: tool_effect.clone(),
                tool_digest,
            },
            AdaptiveTransitionV1::ObserveTool {
                observation: AdaptiveObservationRefV1 {
                    effect: tool_effect,
                    observation_digest: "d".repeat(64),
                },
            },
        ]
        .into_iter()
        .enumerate()
        {
            let current = session(&api, binding);
            api.store
                .advance_adaptive_session(
                    binding.grant.session_id,
                    current.version,
                    Uuid::new_v4(),
                    &command,
                    &binding.grant.authority,
                    at + offset as u64,
                )
                .unwrap();
        }
    };
    finish_inspect(&a, base);
    finish_inspect(&b, base + 10);
    let selected = api
        .provider_usage_binding_for_agent(a.grant.authority.agent_id)
        .unwrap()
        .unwrap();
    assert_eq!(selected.reservation_id, a.grant.provider_allowance_id);
    let old = session(&api, &a);
    let effect = AdaptiveEffectV1 {
        id: Uuid::new_v4(),
        request_digest: "e".repeat(64),
    };
    api.store
        .advance_adaptive_session(
            a.grant.session_id,
            old.version,
            Uuid::new_v4(),
            &AdaptiveTransitionV1::ClaimModel {
                effect,
                previous_observation_digest: old
                    .last_observation
                    .as_ref()
                    .map(|value| value.observation_digest.clone()),
            },
            &a.grant.authority,
            base + 20,
        )
        .unwrap();
    let selected = api
        .provider_usage_binding_for_agent(a.grant.authority.agent_id)
        .unwrap()
        .unwrap();
    assert_eq!(selected.reservation_id, b.grant.provider_allowance_id);
    assert_eq!(
        api.adaptive_subscription_queue_order(&selected).unwrap(),
        Some((1, session(&api, &b).updated_at_ms))
    );
    // Restart reconstructs the same order from existing journal data, not RAM.
    let reopened = super::model_work::configured_test_api(&temp.path().join("company.sqlite"));
    assert_eq!(
        reopened
            .provider_usage_binding_for_agent(a.grant.authority.agent_id)
            .unwrap()
            .unwrap()
            .reservation_id,
        b.grant.provider_allowance_id
    );
}

fn reserved_adaptive_request(
    api: &WorkflowApi,
    binding: &AdaptiveProviderAuthority,
) -> serde_json::Value {
    let prepared = api.prepare_adaptive_model(binding).unwrap();
    assert!(prepared.working_memory.is_some());
    let context = ModelExecutionContext::Adaptive(Box::new(prepared));
    context.validate_dispatch(now_unix_ms()).unwrap();
    let context_digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&context).unwrap())
    );
    let request_id = binding.request_id();
    let request_digest = "d".repeat(64);
    let events = api.event_store.as_ref().unwrap();
    assert!(events
        .reserve_llm_request(
            &request_id,
            &request_digest,
            &binding.grant.authority.agent_id.to_string(),
        )
        .unwrap());
    let reserved = events.get_llm_completion(&request_id).unwrap().unwrap();
    assert_eq!(reserved.request_digest, request_digest);
    assert_eq!(reserved.status, "provider_in_flight");
    assert!(reserved.payload.is_empty());
    let owner_scope = sentinel_common::StateTransferScope::for_agent(
        binding.grant.authority.agent_id.to_string(),
    );
    assert_eq!(reserved.owner_scope, owner_scope);
    serde_json::json!({
        "schema_version": 3,
        "allowance_id": binding.grant.provider_allowance_id,
        "agent_id": binding.grant.authority.agent_id.0,
        "request_id": request_id,
        "request_digest": request_digest,
        "context_digest": context_digest,
        "provider": binding.grant.provider,
        "model": binding.grant.model,
        "catalog_digest": binding.grant.catalog_digest,
        "subject": {
            "kind": "adaptive_session",
            "session_id": binding.grant.session_id,
            "effect_id": binding.effect_id,
            "session_version": binding.session_version,
        },
    })
}

#[test]
fn exact_adaptive_b_reauthorizes_and_dispatches_once_while_global_queue_selects_a() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("company.sqlite");
    let events = temp.path().join("events.sqlite");
    let (api, a, b) = two_project_fixture(&path, &events);
    let request = reserved_adaptive_request(&api, &b);
    let before = discovery_state(&path, &events);
    let before_a = session(&api, &a);
    let before_b = session(&api, &b);
    assert_eq!(before_a.cursor, AdaptiveCursorV1::ReadyForModel);
    assert_eq!(before_b.cursor, AdaptiveCursorV1::ReadyForModel);
    assert_eq!(before_b.model_calls, 0);

    let global = api
        .adaptive_provider_authority_for_claim(b.grant.authority.agent_id)
        .unwrap()
        .unwrap();
    assert_eq!(global, a);
    assert_ne!(
        global, b,
        "the old global-queue reauthorization must select another project",
    );
    let expected = ProviderExecutionAuthority::Adaptive(Box::new(b.clone()));
    assert_eq!(
        api.resolve_provider_usage_authority(b.grant.authority.agent_id)
            .unwrap(),
        Some(ProviderExecutionAuthority::Adaptive(Box::new(a.clone()))),
    );
    assert!(api.validate_provider_usage_authority(&expected).unwrap());
    assert_eq!(
        api.adaptive_provider_authority_for_exact_binding(&b)
            .unwrap(),
        Some(b.clone()),
    );
    assert_eq!(
        api.adaptive_provider_authority_for_reserved_session(
            b.grant.authority.agent_id,
            b.grant.session_id,
            &b.grant.provider_allowance_id,
            b.session_version,
            b.effect_id,
        )
        .unwrap(),
        Some(b.clone()),
    );
    assert_eq!(discovery_state(&path, &events), before);

    let body = serde_json::to_vec(&request).unwrap();
    let response = api.subscription_dispatch(&body);
    assert_eq!(response.status, 200);
    let response: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(response["schema_version"], request["schema_version"]);
    assert_eq!(response["allowance_id"], request["allowance_id"]);
    assert_eq!(response["request_id"], request["request_id"]);
    assert_eq!(response["request_digest"], request["request_digest"]);
    let claimed = session(&api, &b);
    assert_eq!(claimed.version, before_b.version + 1);
    assert_eq!(claimed.model_calls, before_b.model_calls + 1);
    assert_eq!(claimed.tool_calls, before_b.tool_calls);
    assert_eq!(claimed.grant, before_b.grant);
    assert_eq!(claimed.continuation, before_b.continuation);
    assert_eq!(claimed.last_observation, before_b.last_observation);
    assert_eq!(
        claimed.cursor,
        AdaptiveCursorV1::ModelPending {
            effect: AdaptiveEffectV1 {
                id: b.effect_id,
                request_digest: request["request_digest"].as_str().unwrap().to_owned(),
            },
        },
    );
    assert_eq!(
        response["deadline_unix_ms"].as_u64().unwrap(),
        claimed
            .grant
            .deadline_ms
            .min(claimed.updated_at_ms + claimed.grant.max_call_duration_ms),
    );
    assert_eq!(session(&api, &a), before_a);
    let after = discovery_state(&path, &events);
    assert_ne!(after, before);
    assert_eq!(&after[..3], &before[..3]);
    assert_eq!(&after[5..], &before[5..]);
    // Reauthorization retains the same pending effect, but never grants another claim.
    assert!(api.validate_provider_usage_authority(&expected).unwrap());
    assert_eq!(api.subscription_dispatch(&body).status, 403);
    assert_eq!(session(&api, &b), claimed);
    assert_eq!(session(&api, &a), before_a);
    assert_eq!(discovery_state(&path, &events), after);
}

#[test]
fn exact_adaptive_reauthorization_rejects_tampered_scope_and_budgets_without_writes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("company.sqlite");
    let events = temp.path().join("events.sqlite");
    let (api, a, b) = two_project_fixture(&path, &events);
    let before = discovery_state(&path, &events);
    let before_a = session(&api, &a);
    let before_b = session(&api, &b);
    let expected = ProviderExecutionAuthority::Adaptive(Box::new(b.clone()));
    assert!(api.validate_provider_usage_authority(&expected).unwrap());

    for field in [
        "tenant",
        "project",
        "assignment_id",
        "assignment_version",
        "assignment_digest",
        "profile_id",
        "profile_generation",
        "profile_digest",
        "session",
        "version",
        "effect",
        "allowance",
        "model_budget",
        "tool_budget",
        "duration_budget",
        "output_budget",
        "deadline",
    ] {
        let mut changed = b.clone();
        match field {
            "tenant" => {
                changed.grant.authority.tenant_id = TenantId::parse("tenant-other").unwrap()
            }
            "project" => changed.grant.authority.project_id = a.grant.authority.project_id.clone(),
            "assignment_id" => changed.assignment_id = a.assignment_id.clone(),
            "assignment_version" => changed.grant.authority.assignment_version += 1,
            "assignment_digest" => changed.grant.authority.assignment_digest = "0".repeat(64),
            "profile_id" => changed.grant.authority.profile_id = "python-coding-v1".into(),
            "profile_generation" => changed.grant.authority.profile_generation += 1,
            "profile_digest" => changed.grant.authority.profile_digest = "0".repeat(64),
            "session" => changed.grant.session_id = a.grant.session_id,
            "version" => changed.session_version += 1,
            "effect" => changed.effect_id = a.effect_id,
            "allowance" => {
                changed.grant.provider_allowance_id = a.grant.provider_allowance_id.clone()
            }
            "model_budget" => changed.grant.max_model_calls += 1,
            "tool_budget" => changed.grant.max_tool_calls += 1,
            "duration_budget" => changed.grant.max_call_duration_ms += 1,
            "output_budget" => changed.grant.max_output_tokens += 1,
            "deadline" => changed.grant.deadline_ms += 1,
            _ => unreachable!(),
        }
        assert_ne!(changed, b, "{field}");
        assert_eq!(
            api.adaptive_provider_authority_for_exact_binding(&changed),
            if field == "tenant" {
                Err("adaptive exact project missing")
            } else {
                Ok(None)
            },
            "{field}: exact resolution must not substitute another binding",
        );
        assert_eq!(
            api.validate_provider_usage_authority(&ProviderExecutionAuthority::Adaptive(Box::new(
                changed,
            ))),
            if field == "tenant" {
                Err("adaptive exact project missing")
            } else {
                Ok(false)
            },
            "{field}: resolver reauthorization must deny the changed binding",
        );
        assert_eq!(discovery_state(&path, &events), before, "{field}");
        assert_eq!(session(&api, &a), before_a, "{field}");
        assert_eq!(session(&api, &b), before_b, "{field}");
    }
}

#[test]
fn exact_adaptive_dispatch_rejects_tampered_reserved_identity_without_writes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("company.sqlite");
    let events = temp.path().join("events.sqlite");
    let (api, a, b) = two_project_fixture(&path, &events);
    let request = reserved_adaptive_request(&api, &b);
    let before = discovery_state(&path, &events);
    let before_a = session(&api, &a);
    let before_b = session(&api, &b);
    for (pointer, replacement) in [
        ("/subject/session_id", serde_json::json!(a.grant.session_id)),
        (
            "/subject/session_version",
            serde_json::json!(b.session_version + 1),
        ),
        ("/subject/effect_id", serde_json::json!(a.effect_id)),
        (
            "/allowance_id",
            serde_json::json!(a.grant.provider_allowance_id),
        ),
        ("/agent_id", serde_json::json!(7)),
        ("/request_id", serde_json::json!(a.request_id())),
        ("/request_digest", serde_json::json!("0".repeat(64))),
        ("/context_digest", serde_json::json!("0".repeat(64))),
        ("/provider", serde_json::json!("local-loop")),
        ("/model", serde_json::json!("other-model")),
        ("/catalog_digest", serde_json::json!("0".repeat(64))),
    ] {
        let mut changed = request.clone();
        *changed.pointer_mut(pointer).unwrap() = replacement;
        assert_ne!(changed, request, "{pointer}");
        assert_eq!(
            api.subscription_dispatch(&serde_json::to_vec(&changed).unwrap())
                .status,
            403,
            "{pointer}",
        );
        assert_eq!(discovery_state(&path, &events), before, "{pointer}");
        assert_eq!(session(&api, &a), before_a, "{pointer}");
        assert_eq!(session(&api, &b), before_b, "{pointer}");
    }
    assert_eq!(
        api.subscription_dispatch(&serde_json::to_vec(&request).unwrap())
            .status,
        200,
        "denied callbacks must not consume the valid reserved effect",
    );
}

#[test]
fn exact_adaptive_reauthorization_and_dispatch_never_create_a_missing_session() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("company.sqlite");
    let events = temp.path().join("events.sqlite");
    let (api, a, _) = configured_adaptive_test_api(&path, &events);
    let work_b = assign_test_work_from_at(
        &api,
        Some(8),
        100,
        now_unix_ms().max(a.grant.created_at_ms + 1),
    );
    let authority_b = api
        .authority
        .as_ref()
        .unwrap()
        .snapshot_for_admission(
            &TenantId::parse(&work_b.tenant_id).unwrap(),
            &ProjectId::parse(&work_b.project_id).unwrap(),
            &WorkItemId::parse(&work_b.work_item_id).unwrap(),
            work_b.agent_id,
            false,
        )
        .unwrap();
    assert!(api
        .store
        .adaptive_session_for_authority(&authority_b)
        .unwrap()
        .is_none());
    // Reconstruct B's first effect from its real grant and current authority,
    // without calling scheduling/discovery, which would create the missing head.
    let project_b = api
        .store
        .company_project(&authority_b.tenant_id, &authority_b.project_id)
        .unwrap()
        .unwrap();
    let allowance_b = project_b.subscription_call.as_ref().unwrap();
    assert_eq!(allowance_b.allowance_id, work_b.reservation_id);
    assert_eq!(
        allowance_b.grant,
        *work_b.subscription_grant.as_ref().unwrap()
    );
    let authority_digest = authority_b.canonical_digest().unwrap();
    let session_id = stable_operation_id(
        "sentinel.workflow.adaptive-session.v1",
        &format!("{}:{authority_digest}", allowance_b.allowance_id),
        allowance_b.grant.assignment_version,
    );
    let provider_bytes = serde_json::to_vec(&(allowance_b, &authority_digest)).unwrap();
    let expected_b = AdaptiveProviderAuthority {
        schema_version: 3,
        grant: AdaptiveSessionGrantV1 {
            session_id,
            authority: authority_b.clone(),
            provider_allowance_id: allowance_b.allowance_id.clone(),
            provider_authority_digest: domain_digest(
                "sentinel.workflow.adaptive-provider-authority.v1",
                &[&provider_bytes],
            ),
            created_at_ms: allowance_b.created_at_unix_ms,
            deadline_ms: allowance_b.grant.expires_at_unix_ms,
            ..a.grant.clone()
        },
        session_version: 1,
        effect_id: stable_operation_id(
            "sentinel.workflow.adaptive-model-effect.v1",
            &session_id.to_string(),
            1,
        ),
        assignment_id: work_b.assignment_id,
        previous_observation: None,
    };
    let mut request = reserved_adaptive_request(&api, &a);
    request["allowance_id"] = serde_json::json!(work_b.reservation_id);
    let before = discovery_state(&path, &events);
    let before_a = session(&api, &a);
    assert_eq!(
        api.adaptive_provider_authority_for_exact_binding(&expected_b)
            .unwrap(),
        None,
    );
    assert!(!api
        .validate_provider_usage_authority(&ProviderExecutionAuthority::Adaptive(Box::new(
            expected_b.clone(),
        )))
        .unwrap());
    assert_eq!(
        api.adaptive_provider_authority_for_reserved_session(
            work_b.agent_id,
            expected_b.grant.session_id,
            &work_b.reservation_id,
            expected_b.session_version,
            expected_b.effect_id,
        )
        .unwrap(),
        None,
    );
    assert_eq!(
        api.subscription_dispatch(&serde_json::to_vec(&request).unwrap())
            .status,
        403,
    );
    assert!(api
        .store
        .adaptive_session_for_authority(&authority_b)
        .unwrap()
        .is_none());
    assert_eq!(session(&api, &a), before_a);
    assert_eq!(discovery_state(&path, &events), before);

    // Only normal scheduling may create B. Its result also proves that the
    // missing-session negative supplied the exact valid first-effect binding.
    let projects = api.store.company_projects().unwrap();
    let selected_b =
        select_provider_usage_binding(&projects, work_b.agent_id, Some(&work_b.reservation_id))
            .unwrap()
            .unwrap();
    let (created, reconciled) = api
        .adaptive_provider_authority_from_binding(selected_b, false)
        .unwrap();
    assert!(!reconciled);
    assert_eq!(created, Some(expected_b.clone()));
    assert_eq!(
        session(&api, &expected_b).cursor,
        AdaptiveCursorV1::ReadyForModel,
    );
    assert_eq!(session(&api, &expected_b).model_calls, 0);
}
