//! Store-backed synthetic decisions only; no live provider or Workbench evidence.
use super::*;
use crate::workflow_api::adaptive_leadership_review::tests::{
    discovery_state, persist, reconcile_review_at,
};
use crate::workflow_api::adaptive_leadership_review::LeadershipAuthority;
use crate::workflow_api::adaptive_resume_policy::tests::{
    claim_review, continued_policy_fixture, unreviewed_policy_root_with_limits,
};
use sentinel_workflow::{
    AdaptiveModelAdmissionV1, AdaptiveObservationRefV1, AdaptiveWorkFundingLimitsV1,
};

fn funded_fixture() -> (tempfile::TempDir, WorkflowApi, AdaptiveSessionV1) {
    let (temp, api, source) = unreviewed_policy_root_with_limits(120_000, false, 4, 4);
    let now = now_unix_ms();
    let operator = api.principals.principal("operator").unwrap();
    let request = api
        .store
        .adaptive_work_funding_draft(
            &operator.principal,
            &source.grant.authority.project_id,
            source.grant.session_id,
            Uuid::new_v4(),
            "explicit-dispatch-slack-regression",
            AdaptiveWorkFundingLimitsV1 {
                additional_model_calls: 4,
                additional_tool_calls: 4,
                additional_reviews: 4,
                additional_windows: 4,
                max_window_ms: 300_000,
                max_call_duration_ms: 120_000,
                dispatch_margin_ms: sentinel_workflow::ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
                expires_at_unix_ms: now + 600_000,
            },
            now,
        )
        .unwrap();
    let (replayed, receipt) = api
        .store
        .authorize_adaptive_work_funding(&operator.principal, &request, now)
        .unwrap();
    assert!(!replayed);
    let project = api
        .store
        .company_project(
            &source.grant.authority.tenant_id,
            &source.grant.authority.project_id,
        )
        .unwrap()
        .unwrap();
    assert!(reconcile_review_at(&api, &project, now_unix_ms()));
    let pm = api
        .principals
        .principal("pm")
        .unwrap()
        .principal
        .agent_id
        .unwrap();
    let call = api.leadership_review_for_agent(pm).unwrap().unwrap();
    let context = api
        .prepare_leadership_review(&LeadershipAuthority::from_call(&call))
        .unwrap();
    assert_eq!(context.binding.grant.schema_version, 5);
    assert!(context.binding.grant.resume_policy.is_none());
    let (id, digest) = claim_review(&api, &context);
    let completion = ModelExecutionCompletion {
        context: ModelExecutionContext::AdaptiveLeadershipReview(Box::new(context.clone())),
        content: serde_json::json!({
            "schema_version": 5,
            "decision": {
                "kind": "continue",
                "additional_model_calls": 4,
                "window_ms": 180_000,
                "rationale": "Synthetic bounded inspection and work window.",
                "evidence_refs": [context.source.evidence_refs[0]],
            },
        })
        .to_string(),
        admissible: true,
    };
    persist(&api, &completion, &context, &id, &digest, true);
    api.accept_leadership_review_at(&completion, &context, &id, &digest, now_unix_ms())
        .unwrap();
    let adopted = api
        .store
        .adaptive_session_for_authority(&source.grant.authority)
        .unwrap()
        .unwrap();
    assert_eq!(adopted.grant, source.grant);
    assert_eq!(adopted.model_calls, source.model_calls);
    assert_eq!(adopted.tool_calls, source.tool_calls);
    assert_eq!(adopted.active_work_funding().unwrap().receipt, receipt);
    assert!(matches!(adopted.cursor, AdaptiveCursorV1::ReadyForModel));
    (temp, api, adopted)
}

fn cutoff(session: &AdaptiveSessionV1, margin_ms: u64) -> u64 {
    session.active_deadline_ms() - session.effective_call_duration_ms() - margin_ms
}

#[test]
fn funded_ready_discovery_matches_claim_slack_at_equality_and_one_ms_late() {
    let (temp, api, session) = funded_fixture();
    let binding = api
        .adaptive_provider_authority(session.grant.authority.agent_id)
        .unwrap()
        .unwrap();
    let epoch = session.active_work_funding().unwrap();
    let boundary = cutoff(&session, epoch.binding.limits.dispatch_margin_ms);
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    assert_eq!(
        session.model_admission_at(boundary),
        AdaptiveModelAdmissionV1::Admissible,
    );
    assert_eq!(
        api.adaptive_model_effect_at(&session, boundary).unwrap(),
        Some((binding.session_version, binding.effect_id)),
    );
    let claim = AdaptiveTransitionV1::ClaimModel {
        effect: AdaptiveEffectV1 {
            id: binding.effect_id,
            request_digest: "f".repeat(64),
        },
        previous_observation_digest: None,
    };
    for now in [
        boundary + 1,
        session.active_deadline_ms(),
        session.active_deadline_ms() + 1,
    ] {
        assert_ne!(
            session.model_admission_at(now),
            AdaptiveModelAdmissionV1::Admissible,
        );
        assert_eq!(api.adaptive_model_effect_at(&session, now).unwrap(), None);
        assert!(api
            .store
            .advance_company_adaptive_model_with_clock(
                session.grant.session_id,
                session.version,
                Uuid::new_v4(),
                &claim,
                &session.grant.authority,
                || now,
            )
            .is_err());
    }
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
        ),
        before,
    );
    let (replayed, pending) = api
        .store
        .advance_company_adaptive_model_with_clock(
            session.grant.session_id,
            session.version,
            Uuid::new_v4(),
            &claim,
            &session.grant.authority,
            || boundary,
        )
        .unwrap();
    assert!(!replayed);
    assert!(matches!(
        pending.cursor,
        AdaptiveCursorV1::ModelPending { .. }
    ));
    assert_eq!(pending.model_calls, session.model_calls + 1);
    assert_eq!(pending.tool_calls, session.tool_calls);
    assert_eq!(pending.continuation, session.continuation);
}

#[test]
fn root_discovery_is_unchanged_and_resume_policy_keeps_its_slack_guard() {
    let temp = tempfile::tempdir().unwrap();
    let (api, binding, _) = crate::workflow_api::model_work::configured_adaptive_test_api(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    let root = api
        .store
        .adaptive_session_for_authority(&binding.grant.authority)
        .unwrap()
        .unwrap();
    assert!(root.continuation.is_none());
    assert_eq!(
        api.adaptive_model_effect_at(&root, root.active_deadline_ms() + 1)
            .unwrap(),
        Some((binding.session_version, binding.effect_id)),
    );

    let (_temp, api, session) = continued_policy_fixture(false);
    assert!(session.active_work_funding().is_none());
    let policy = session
        .continuation
        .as_ref()
        .unwrap()
        .authorizations
        .last()
        .unwrap()
        .resume_policy
        .as_ref()
        .unwrap();
    let boundary = cutoff(&session, policy.limits.dispatch_margin_ms);
    assert_eq!(
        session.model_admission_at(boundary),
        AdaptiveModelAdmissionV1::Admissible,
    );
    assert!(api
        .adaptive_model_effect_at(&session, boundary)
        .unwrap()
        .is_some());
    assert_eq!(
        api.adaptive_model_effect_at(&session, boundary + 1)
            .unwrap(),
        None,
    );
}

#[test]
fn funded_pending_effect_survives_slack_and_expiry_for_result_adoption_and_replay() {
    let (temp, api, session) = funded_fixture();
    let binding = api
        .adaptive_provider_authority(session.grant.authority.agent_id)
        .unwrap()
        .unwrap();
    let context = api.prepare_adaptive_model(&binding).unwrap();
    let effect = AdaptiveEffectV1 {
        id: binding.effect_id,
        request_digest: "a".repeat(64),
    };
    let (_, pending) = api
        .store
        .advance_adaptive_session(
            session.grant.session_id,
            session.version,
            Uuid::new_v4(),
            &AdaptiveTransitionV1::ClaimModel {
                effect: effect.clone(),
                previous_observation_digest: None,
            },
            &session.grant.authority,
            now_unix_ms(),
        )
        .unwrap();
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    let boundary = cutoff(
        &pending,
        pending
            .active_work_funding()
            .unwrap()
            .binding
            .limits
            .dispatch_margin_ms,
    );
    for now in [boundary + 1, pending.active_deadline_ms() + 1] {
        assert_ne!(
            pending.model_admission_at(now),
            AdaptiveModelAdmissionV1::Admissible,
        );
        assert_eq!(
            api.adaptive_model_effect_at(&pending, now).unwrap(),
            Some((binding.session_version, effect.id)),
        );
    }
    assert_eq!(
        api.adaptive_provider_authority_for_exact_binding(&binding)
            .unwrap(),
        Some(binding.clone()),
    );
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
        ),
        before,
    );
    let content = r#"{"schema_version":1,"decision":{"kind":"tool","tool":{"tool":"inspect_file","path":"src/main.rs","max_bytes":1024}}}"#;
    let decision = parse_adaptive_decision(content).unwrap();
    // Resolve a previously claimed result after expiry; do not request another model call.
    let (_, resolved) = api
        .store
        .advance_adaptive_session(
            pending.grant.session_id,
            pending.version,
            Uuid::new_v4(),
            &AdaptiveTransitionV1::ResolveModel {
                effect: effect.clone(),
                result_digest: hex_sha256(content.as_bytes()),
                decision,
            },
            &pending.grant.authority,
            pending.active_deadline_ms() + 1,
        )
        .unwrap();
    assert!(matches!(
        resolved.cursor,
        AdaptiveCursorV1::ReadyForTool { .. }
    ));
    assert_eq!(resolved.model_calls, session.model_calls + 1);
    assert_eq!(resolved.tool_calls, session.tool_calls);
    assert_eq!(resolved.grant, session.grant);
    assert_eq!(resolved.continuation, session.continuation);
    let completion = ModelExecutionCompletion {
        context: ModelExecutionContext::Adaptive(Box::new(context.clone())),
        content: content.into(),
        admissible: true,
    };
    let after = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    for _ in 0..2 {
        api.accept_adaptive_model(
            &completion,
            &context,
            &binding.request_id(),
            &effect.request_digest,
        )
        .unwrap();
    }
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
        ),
        after,
    );
}

#[test]
fn funded_exhausted_ready_window_is_not_selected_even_before_deadline() {
    let (_temp, api, mut session) = funded_fixture();
    let tool = sentinel_common::WorkbenchTool::InspectFile {
        path: "src/main.rs".into(),
        max_bytes: 1024,
    };
    let tool_digest = sentinel_workflow::adaptive_tool_digest(&tool).unwrap();
    let now = now_unix_ms();
    while session.model_calls < session.active_model_ceiling() {
        let effect = AdaptiveEffectV1 {
            id: Uuid::new_v4(),
            request_digest: "b".repeat(64),
        };
        let tool_effect = AdaptiveEffectV1 {
            id: Uuid::new_v4(),
            request_digest: "c".repeat(64),
        };
        for command in [
            AdaptiveTransitionV1::ClaimModel {
                effect: effect.clone(),
                previous_observation_digest: session
                    .last_observation
                    .as_ref()
                    .map(|o| o.observation_digest.clone()),
            },
            AdaptiveTransitionV1::ResolveModel {
                effect,
                result_digest: "d".repeat(64),
                decision: AdaptiveModelDecisionV1::Tool {
                    tool: tool.clone(),
                    tool_digest: tool_digest.clone(),
                },
            },
            AdaptiveTransitionV1::ClaimTool {
                effect: tool_effect.clone(),
                tool_digest: tool_digest.clone(),
            },
            AdaptiveTransitionV1::ObserveTool {
                observation: AdaptiveObservationRefV1 {
                    effect: tool_effect,
                    observation_digest: "e".repeat(64),
                },
            },
        ] {
            session = api
                .store
                .advance_adaptive_session(
                    session.grant.session_id,
                    session.version,
                    Uuid::new_v4(),
                    &command,
                    &session.grant.authority,
                    now,
                )
                .unwrap()
                .1;
        }
    }
    assert!(matches!(session.cursor, AdaptiveCursorV1::ReadyForModel));
    assert_eq!(
        session.model_admission_at(now),
        AdaptiveModelAdmissionV1::CallsExhausted,
    );
    assert_eq!(api.adaptive_model_effect_at(&session, now).unwrap(), None);
    assert!(api
        .adaptive_provider_authority(session.grant.authority.agent_id)
        .unwrap()
        .is_none());
}
