//! Synthetic typed decisions and journal transitions; no provider or tool execution.
use super::super::adaptive_leadership_review::tests::{
    discovery_state, persist, reconcile_review_at, stop_review_agent,
};
use super::super::adaptive_leadership_review::{LeadershipAuthority, LeadershipContext};
use super::super::adaptive_resume_policy::tests::unreviewed_policy_root_with_limits;
use super::super::model_execution::{ModelExecutionCompletion, ModelExecutionContext};
use super::*;
use crate::workbench::{
    with_private_observation_store_for_test, WorkbenchCoordinator, WorkbenchInvocationStore,
};
use sentinel_workflow::{
    AdaptiveLeadershipReviewCallV1, AdaptiveLeadershipReviewDecisionKindV1,
    AdaptiveLeadershipReviewDecisionV1, AdaptiveWorkFundingLimitsV1, AdaptiveWorkFundingReceiptV1,
    ClaimAdaptiveLeadershipReviewCallV1,
};
use std::os::unix::fs::PermissionsExt;

fn project(api: &WorkflowApi, session: &AdaptiveSessionV1) -> sentinel_workflow::ProjectV1 {
    api.store
        .company_project(
            &session.grant.authority.tenant_id,
            &session.grant.authority.project_id,
        )
        .unwrap()
        .unwrap()
}

fn session(api: &WorkflowApi, source: &AdaptiveSessionV1) -> AdaptiveSessionV1 {
    api.store
        .adaptive_session_for_authority(&source.grant.authority)
        .unwrap()
        .unwrap()
}

fn calls(api: &WorkflowApi, source: &AdaptiveSessionV1) -> Vec<AdaptiveLeadershipReviewCallV1> {
    api.store
        .adaptive_leadership_review_calls(
            &source.grant.authority.tenant_id,
            source.grant.session_id,
        )
        .unwrap()
}

fn state(temp: &tempfile::TempDir) -> Vec<Vec<Vec<sentinel_limbo::rusqlite::types::Value>>> {
    discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    )
}

fn pending_context(
    api: &WorkflowApi,
    source: &AdaptiveSessionV1,
    stage: &str,
) -> LeadershipContext {
    let pending: Vec<_> = calls(api, source)
        .into_iter()
        .filter(|call| call.decision.is_none() && call.retired_at_unix_ms.is_none())
        .collect();
    assert_eq!(pending.len(), 1, "{stage}");
    let call = &pending[0];
    assert_eq!(call.grant.schema_version, 5);
    assert!(call.grant.work_funding.is_some());
    let binding = LeadershipAuthority::from_call(call);
    // Like the existing historical-clock leadership fixtures, reconstruct the
    // exact persisted context rather than bypassing real-clock dispatch checks.
    let context = LeadershipContext {
        accounting: LeadershipContext::accounting_projection(&binding, &call.context).unwrap(),
        accounting_correction: None,
        binding,
        source: call.context.clone(),
        context_digest: call.context_digest().unwrap(),
        private_observation: call.context.source_session.last_observation.as_ref().map(
            |reference| {
                api.workbench
                    .as_ref()
                    .unwrap()
                    .private_observation(reference.effect.id, &source.grant.authority.profile_id)
                    .unwrap()
            },
        ),
    };
    context.validate_dispatch(call.updated_at_unix_ms).unwrap();
    context
}

fn decide_at(api: &WorkflowApi, context: &LeadershipContext, continued: bool, now: u64) {
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
    api.store
        .claim_adaptive_leadership_review_call(
            &grant.leadership_principal,
            &ClaimAdaptiveLeadershipReviewCallV1 {
                review_id: grant.review_id,
                allowance_id: context.binding.allowance_id.clone(),
                request_id: id.clone(),
                request_digest: digest.clone(),
                context_digest: context.context_digest.clone(),
            },
            now,
        )
        .unwrap();
    let rationale = "Synthetic independently selected leadership disposition.".to_owned();
    let evidence_refs = vec![context.source.evidence_refs[0].clone()];
    let decision = AdaptiveLeadershipReviewDecisionV1 {
        schema_version: 5,
        decision: if continued {
            AdaptiveLeadershipReviewDecisionKindV1::Continue {
                additional_model_calls: 4,
                window_ms: 180_000,
                rationale,
                evidence_refs,
            }
        } else {
            AdaptiveLeadershipReviewDecisionKindV1::DeferBudget {
                rationale,
                evidence_refs,
            }
        },
    };
    decision.validate_subject(grant).unwrap();
    let completion = ModelExecutionCompletion {
        context: ModelExecutionContext::AdaptiveLeadershipReview(Box::new(context.clone())),
        content: serde_json::to_string(&decision).unwrap(),
        admissible: true,
    };
    persist(api, &completion, context, &id, &digest, true);
    api.accept_leadership_review_at(&completion, context, &id, &digest, now)
        .unwrap();
}

fn advance_at(
    api: &WorkflowApi,
    source: &mut AdaptiveSessionV1,
    command: AdaptiveTransitionV1,
    now: u64,
) {
    *source = api
        .store
        .advance_adaptive_session(
            source.grant.session_id,
            source.version,
            Uuid::new_v4(),
            &command,
            &source.grant.authority,
            now,
        )
        .unwrap()
        .1;
}

fn inspect_at(
    api: &WorkflowApi,
    tools: &WorkbenchInvocationStore,
    mut source: AdaptiveSessionV1,
    now: u64,
) -> AdaptiveSessionV1 {
    let model = sentinel_workflow::AdaptiveEffectV1 {
        id: Uuid::new_v4(),
        request_digest: "a".repeat(64),
    };
    let tool = sentinel_common::WorkbenchTool::InspectFile {
        path: "src/main.rs".into(),
        max_bytes: 1024,
    };
    let tool_digest = sentinel_workflow::adaptive_tool_digest(&tool).unwrap();
    advance_at(
        api,
        &mut source,
        AdaptiveTransitionV1::ClaimModel {
            effect: model.clone(),
            previous_observation_digest: None,
        },
        now,
    );
    advance_at(
        api,
        &mut source,
        AdaptiveTransitionV1::ResolveModel {
            effect: model,
            result_digest: "b".repeat(64),
            decision: sentinel_workflow::AdaptiveModelDecisionV1::Tool {
                tool: tool.clone(),
                tool_digest: tool_digest.clone(),
            },
        },
        now,
    );
    let adapter = api.workbench.as_ref().unwrap();
    let effect = adapter.adaptive_tool_effect(&source, &tool).unwrap();
    let request = adapter
        .build_adaptive_request(&source, &effect, &tool)
        .unwrap();
    advance_at(
        api,
        &mut source,
        AdaptiveTransitionV1::ClaimTool {
            effect: effect.clone(),
            tool_digest,
        },
        now,
    );
    tools.reserve(&request, now).unwrap();
    tools
        .mark_executing(&request.invocation_id, &request.input_digest, now)
        .unwrap();
    tools
        .accept_result(
            &sentinel_common::WorkbenchMessage::Result {
                schema_version: sentinel_common::WORKBENCH_SCHEMA_VERSION,
                invocation_id: request.invocation_id.clone(),
                input_digest: request.input_digest.clone(),
                outcome: sentinel_common::WorkbenchOutcome::Succeeded,
                resources: sentinel_common::WorkbenchResourceUsage::default(),
                artifacts: Vec::new(),
                error: None,
                output: std::collections::BTreeMap::from([(
                    "content".into(),
                    "Synthetic bounded inspection, not a live tool result.".into(),
                )]),
            },
            now,
        )
        .unwrap();
    let authority = api.authority.as_ref().unwrap();
    let (profile, digest) = authority
        .profile_for_binding(&request.tool_profile)
        .unwrap();
    let observation = WorkbenchCoordinator::new(tools, profile, digest)
        .private_observation(&request.invocation_id, authority.as_ref())
        .unwrap()
        .unwrap();
    observation
        .validate(&request.invocation_id, &request.input_digest)
        .unwrap();
    advance_at(
        api,
        &mut source,
        AdaptiveTransitionV1::ObserveTool {
            observation: sentinel_workflow::AdaptiveObservationRefV1 {
                effect,
                observation_digest: observation.digest().into(),
            },
        },
        now,
    );
    source
}

fn with_deferred_fixture(run: impl FnOnce(tempfile::TempDir, WorkflowApi, AdaptiveSessionV1)) {
    let (temp, mut api, root) = unreviewed_policy_root_with_limits(120_000, false, 4, 4);
    api.workbench = Some(Arc::new(WorkbenchExecutionAdapter {
        store: Arc::clone(&api.store),
        authority: Arc::clone(api.authority.as_ref().unwrap()),
    }));
    let now = now_unix_ms();
    let issued = now - 240_000;
    let operator = api.principals.principal("operator").unwrap();
    let request = api
        .store
        .adaptive_work_funding_draft(
            &operator.principal,
            &root.grant.authority.project_id,
            root.grant.session_id,
            Uuid::new_v4(),
            "explicit-synthetic-productive-work",
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
            issued,
        )
        .unwrap();
    api.store
        .authorize_adaptive_work_funding(&operator.principal, &request, issued)
        .unwrap();
    assert!(reconcile_review_at(&api, &project(&api, &root), issued));
    assert_eq!(
        calls(&api, &root).len(),
        1,
        "initial funded review discovery"
    );
    let initial = pending_context(&api, &root, "initial funded review before Continue");
    assert_eq!(initial.source.source_session, root);
    assert_eq!(
        initial
            .binding
            .grant
            .work_funding
            .as_ref()
            .unwrap()
            .receipt
            .request,
        request
    );
    decide_at(&api, &initial, true, issued);
    let adopted = session(&api, &root);
    assert!(adopted.active_work_funding().is_some());
    assert!(adopted.model_window_exhausted_at(now));
    // Adoption alone is not work progress and cannot open a same-epoch review.
    assert!(api
        .store
        .adaptive_work_funding_for_review(
            &root.grant.authority.tenant_id,
            root.grant.session_id,
            now,
        )
        .unwrap()
        .is_none());
    let tools =
        Arc::new(WorkbenchInvocationStore::open(temp.path().join("workbench.redb")).unwrap());
    let progressed = inspect_at(&api, &tools, adopted.clone(), issued + 1);
    assert_eq!(progressed.version, adopted.version + 4);
    assert_eq!(progressed.model_calls, root.model_calls + 1);
    assert_eq!(progressed.tool_calls, root.tool_calls + 1);
    assert!(matches!(progressed.cursor, AdaptiveCursorV1::ReadyForModel));
    let (profile, digest) = {
        let authority = api.authority.as_ref().unwrap();
        let (profile, digest) = authority
            .profile_for_binding(&root.grant.authority.profile_id)
            .unwrap();
        (profile.clone(), digest.to_owned())
    };
    with_private_observation_store_for_test(tools, profile, digest, || {
        assert!(reconcile_review_at(&api, &project(&api, &root), now));
        assert_eq!(
            calls(&api, &root).len(),
            2,
            "funded review discovery after journal progress"
        );
        let context = pending_context(
            &api,
            &progressed,
            "post-inspection funded review before Defer",
        );
        assert_eq!(context.source.source_session, progressed);
        assert!(context.private_observation.is_some());
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
        decide_at(&api, &context, false, now);
        let deferred = session(&api, &root);
        assert_eq!(deferred, progressed);
        assert_eq!(deferred.grant, root.grant);
        assert_eq!(calls(&api, &root).len(), 2);
        run(temp, api, deferred);
    });
}

fn policy(api: &WorkflowApi) -> StandingWorkFundingPolicyV1 {
    let issuer = api.principals.principal("operator").unwrap().principal;
    StandingWorkFundingPolicyV1 {
        schema_version: 1,
        policy_id: "standing-integration".into(),
        generation: 1,
        tenant_id: issuer.tenant_id,
        issuer_principal_id: issuer.principal_id,
        epoch_duration_ms: 6 * 60 * 60 * 1000,
        additional_reviews: 4,
        additional_windows: 4,
        max_window_ms: 300_000,
    }
}

fn write_policy(path: &Path, policy: &StandingWorkFundingPolicyV1) {
    fs::write(path, serde_json::to_vec(policy).unwrap()).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn enable(temp: &tempfile::TempDir, api: &mut WorkflowApi) {
    write_policy(&temp.path().join(FILE_NAME), &policy(api));
    api.standing_work_funding_policy =
        StandingFundingPolicyBinding::load(temp.path(), &api.principals, true).unwrap();
    assert!(api.standing_work_funding_policy.is_some());
}

fn proposal(api: &WorkflowApi, source: &AdaptiveSessionV1) -> AdaptiveWorkFundingReceiptV1 {
    let binding = api.standing_work_funding_policy.as_ref().unwrap();
    let issuer = binding.current_operator(&api.principals).unwrap().unwrap();
    let (replayed, receipt) = api
        .store
        .propose_standing_work_funding_checked_with_clock(
            &issuer.principal,
            &binding.policy,
            &source.grant.authority.project_id,
            source.grant.session_id,
            &source.grant.authority,
            || panic!("an exact standing proposal replay must not sample a clock"),
            || panic!("an exact standing proposal replay must not run issuance checks"),
        )
        .unwrap()
        .unwrap();
    assert!(replayed);
    receipt
}

fn assert_no_employee_claim(
    temp: &tempfile::TempDir,
    api: &WorkflowApi,
    source: &AdaptiveSessionV1,
) {
    let before = state(temp);
    assert!(api
        .store
        .advance_adaptive_session(
            source.grant.session_id,
            source.version,
            Uuid::new_v4(),
            &AdaptiveTransitionV1::ClaimModel {
                effect: sentinel_workflow::AdaptiveEffectV1 {
                    id: Uuid::new_v4(),
                    request_digest: "a".repeat(64),
                },
                previous_observation_digest: source
                    .last_observation
                    .as_ref()
                    .map(|reference| reference.observation_digest.clone()),
            },
            &source.grant.authority,
            now_unix_ms(),
        )
        .is_err());
    assert_eq!(state(temp), before);
}

#[test]
fn root_work_batch_reaches_standing_proposal_before_terminal_defer_barrier() {
    with_deferred_fixture(|temp, mut api, source| {
        let project_before = project(&api, &source);
        // Retain the existing manager barrier semantics, but do not let it starve
        // the earlier standing-policy phase in the production work batch.
        assert!(reconcile_review_at(&api, &project_before, now_unix_ms()));
        let calls_before = calls(&api, &source);
        enable(&temp, &mut api);
        let before = state(&temp);
        {
            let _batch = api.reconciliation_fence.lock().unwrap();
            let _fence = api.mutation_fence.read().unwrap();
            api.reconcile_work_batch(&|| false).unwrap();
        }
        let after = state(&temp);
        assert_eq!(after[0].len(), before[0].len() + 1);
        assert!(before[0].iter().all(|row| after[0].contains(row)));
        assert_eq!(after[2].len(), before[2].len() + 1);
        assert_eq!(&after[2][..before[2].len()], before[2]);
        for index in [1, 3, 4, 5, 6] {
            assert_eq!(after[index], before[index], "unchanged table {index}");
        }
        assert_eq!(session(&api, &source), source);
        assert_eq!(project(&api, &source), project_before);
        assert_eq!(calls(&api, &source), calls_before);
        let receipt = proposal(&api, &source);
        assert_eq!(receipt.request.limits.additional_model_calls, 0);
        assert_eq!(receipt.request.limits.additional_tool_calls, 0);
        let replay_before = state(&temp);
        assert_eq!(proposal(&api, &source), receipt);
        assert_eq!(state(&temp), replay_before);
        {
            let _batch = api.reconciliation_fence.lock().unwrap();
            let _fence = api.mutation_fence.read().unwrap();
            api.reconcile_work_batch(&|| false).unwrap();
        }
        let context = pending_context(&api, &source, "next production work batch after proposal");
        assert_eq!(
            context.binding.grant.work_funding.as_ref().unwrap().receipt,
            receipt
        );
        assert_eq!(context.source.source_session, source);
        assert_eq!(session(&api, &source), source);
        let discovered = calls(&api, &source);
        assert_eq!(discovered.len(), calls_before.len() + 1);
        assert!(calls_before.iter().all(|call| discovered.contains(call)));
    });
}

#[test]
fn standing_proposal_requires_independent_typed_decision_before_adoption_and_claim() {
    for continued in [true, false] {
        with_deferred_fixture(|temp, mut api, source| {
            let old_calls = calls(&api, &source);
            enable(&temp, &mut api);
            let current_project = project(&api, &source);
            {
                let _fence = api.mutation_fence.read().unwrap();
                assert!(api
                    .reconcile_standing_work_funding(&current_project)
                    .unwrap());
            }
            let receipt = proposal(&api, &source);
            assert_eq!(session(&api, &source), source);
            assert_no_employee_claim(&temp, &api, &source);
            assert!(reconcile_review_at(&api, &current_project, now_unix_ms()));
            let context = pending_context(&api, &source, "standing proposal independent review");
            assert_eq!(
                context.binding.grant.work_funding.as_ref().unwrap().receipt,
                receipt
            );
            decide_at(&api, &context, continued, now_unix_ms());
            let adopted = session(&api, &source);
            assert_eq!(adopted.grant, source.grant);
            assert_eq!(adopted.model_calls, source.model_calls);
            assert_eq!(adopted.tool_calls, source.tool_calls);
            let retained = calls(&api, &source);
            assert!(old_calls.iter().all(|old| retained.contains(old)));
            if continued {
                assert_eq!(adopted.active_work_funding().unwrap().receipt, receipt);
                assert!(!adopted.model_window_exhausted_at(now_unix_ms()));
                let effect = sentinel_workflow::AdaptiveEffectV1 {
                    id: Uuid::new_v4(),
                    request_digest: "a".repeat(64),
                };
                let claimed = api
                    .store
                    .advance_adaptive_session(
                        source.grant.session_id,
                        adopted.version,
                        Uuid::new_v4(),
                        &AdaptiveTransitionV1::ClaimModel {
                            effect,
                            previous_observation_digest: adopted
                                .last_observation
                                .as_ref()
                                .map(|reference| reference.observation_digest.clone()),
                        },
                        &source.grant.authority,
                        now_unix_ms(),
                    )
                    .unwrap()
                    .1;
                assert_eq!(claimed.model_calls, source.model_calls + 1);
                assert!(matches!(
                    claimed.cursor,
                    AdaptiveCursorV1::ModelPending { .. }
                ));
            } else {
                assert_eq!(adopted, source);
                assert_no_employee_claim(&temp, &api, &source);
                let before = state(&temp);
                for _ in 0..2 {
                    let _fence = api.mutation_fence.read().unwrap();
                    assert!(!api
                        .reconcile_standing_work_funding(&current_project)
                        .unwrap());
                }
                assert_eq!(proposal(&api, &source), receipt);
                assert_eq!(state(&temp), before);
                assert_eq!(calls(&api, &source).len(), old_calls.len() + 1);
                assert!(api
                    .store
                    .adaptive_work_funding_for_review(
                        &source.grant.authority.tenant_id,
                        source.grant.session_id,
                        now_unix_ms(),
                    )
                    .unwrap()
                    .is_none());
            }
        });
    }
}

#[test]
fn standing_default_off_tenant_duty_and_registered_operator_gates_are_read_only() {
    for gate in [
        "default-off",
        "foreign-tenant",
        "off-duty",
        "missing-operator",
    ] {
        with_deferred_fixture(|temp, mut api, source| {
            if gate != "default-off" {
                enable(&temp, &mut api);
            }
            let mut current_project = project(&api, &source);
            match gate {
                "foreign-tenant" => current_project.tenant_id = TenantId::parse("foreign").unwrap(),
                "off-duty" => stop_review_agent(&api, source.grant.authority.agent_id, true),
                "missing-operator" => {
                    let id = policy(&api).issuer_principal_id;
                    let mut principals = PrincipalAuthenticator {
                        by_principal_id: api.principals.by_principal_id.clone(),
                        by_credential_digest: api.principals.by_credential_digest.clone(),
                    };
                    principals.by_principal_id.remove(&id);
                    principals
                        .by_credential_digest
                        .retain(|_, bound| bound.principal.principal_id != id);
                    api.principals = Arc::new(principals);
                }
                _ => {}
            }
            let before = state(&temp);
            let result = {
                let _fence = api.mutation_fence.read().unwrap();
                api.reconcile_standing_work_funding(&current_project)
            };
            if gate == "missing-operator" {
                assert!(result.is_err());
            } else {
                assert!(!result.unwrap(), "{gate}");
            }
            assert_eq!(state(&temp), before, "{gate}");
            assert_eq!(session(&api, &source), source, "{gate}");
        });
    }
}

#[test]
fn checked_precommit_policy_revocation_before_or_after_insert_rolls_back_all_durable_effects() {
    for revocation in ["removed", "changed", "off-duty"] {
        for revoke_at in [1, 2] {
            with_deferred_fixture(|temp, mut api, source| {
                enable(&temp, &mut api);
                let binding = api.standing_work_funding_policy.as_ref().unwrap();
                let operator = binding.current_operator(&api.principals).unwrap().unwrap();
                let current_project = project(&api, &source);
                let before = state(&temp);
                let mut checks = 0;
                let result = api.store.propose_standing_work_funding_checked_with_clock(
                    &operator.principal,
                    &binding.policy,
                    &current_project.project_id,
                    source.grant.session_id,
                    &source.grant.authority,
                    now_unix_ms,
                    || {
                        checks += 1;
                        if checks == revoke_at {
                            match revocation {
                                "removed" => fs::remove_file(&binding.path).unwrap(),
                                "changed" => {
                                    let mut changed = binding.policy.clone();
                                    changed.generation += 1;
                                    write_policy(&binding.path, &changed);
                                }
                                "off-duty" => {
                                    stop_review_agent(&api, source.grant.authority.agent_id, true)
                                }
                                _ => unreachable!(),
                            }
                        }
                        // This is the production checked-store/file-binding
                        // composition; no WorkflowStore call while its mutex is held.
                        let live = binding
                            .current_operator(&api.principals)?
                            .ok_or_else(source_conflict)?;
                        if live.principal != operator.principal
                            || live.execution_authority != operator.execution_authority
                        {
                            return Err(source_conflict());
                        }
                        api.validate_work_funding_operator(&live)?;
                        let current = api
                            .authority
                            .as_ref()
                            .unwrap()
                            .snapshot_from_validated_project(
                                &current_project,
                                &current_project.tenant_id,
                                &current_project.project_id,
                                &source.grant.authority.work_item_id,
                                source.grant.authority.agent_id,
                                true,
                            )
                            .map_err(execution_intent_port_error)?;
                        if current != source.grant.authority {
                            return Err(source_conflict());
                        }
                        Ok(())
                    },
                );
                assert!(result.is_err());
                assert_eq!(checks, revoke_at);
                assert_eq!(
                    state(&temp),
                    before,
                    "revocation={revocation}, check={revoke_at}"
                );
                assert_eq!(session(&api, &source), source);
            });
        }
    }
}
