//! Synthetic model responses and scripted clocks, not live leadership or provider evidence.
use super::adaptive_continuation::recover_continuation_audit;
use super::adaptive_leadership_review::tests::{persist, reconcile_review_at};
use super::adaptive_leadership_review::{LeadershipAuthority, LeadershipContext};
use super::adaptive_recovery_release::TestFundedRecoveryGuard;
use super::model_execution::{ModelExecutionCompletion, ModelExecutionContext};
use super::*;
use sentinel_workflow::{
    adaptive_leadership_continuation_audit_id,
    adaptive_leadership_continuation_provider_authority_digest,
    AdaptiveContinuationAuthorizationV1, AdaptiveContinuationSourceV1,
    AdaptiveLeadershipReviewCallV1, AdaptiveLeadershipReviewDecisionV1,
    AdaptiveLeadershipReviewSubjectV2, AdaptiveWorkFundingLimitsV1,
    CompleteAdaptiveLeadershipReviewCallV1,
};
use sha2::{Digest, Sha256};

const WINDOW_MS: u64 = 300_000;

struct Fixture {
    api: WorkflowApi,
    context: LeadershipContext,
    completion: ModelExecutionCompletion,
    request_id: String,
    request_digest: String,
    _proof: Option<TestFundedRecoveryGuard>,
    _temp: tempfile::TempDir,
}

impl Fixture {
    fn new(schema: u16) -> Self {
        let (temp, api, proof) = match schema {
            5 => {
                let (temp, api, session) =
                    adaptive_resume_policy::tests::unreviewed_policy_root_with_limits(
                        120_000, false, 4, 4,
                    );
                let now = now_unix_ms();
                let operator = api.principals.principal("operator").unwrap();
                let request = api
                    .store
                    .adaptive_work_funding_draft(
                        &operator.principal,
                        &session.grant.authority.project_id,
                        session.grant.session_id,
                        Uuid::new_v4(),
                        "synthetic-sealed-audit-clock-replay",
                        AdaptiveWorkFundingLimitsV1 {
                            additional_model_calls: 4,
                            additional_tool_calls: 4,
                            additional_reviews: 4,
                            additional_windows: 4,
                            max_window_ms: WINDOW_MS,
                            max_call_duration_ms: 120_000,
                            dispatch_margin_ms:
                                sentinel_workflow::ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
                            expires_at_unix_ms: now + 600_000,
                        },
                        now,
                    )
                    .unwrap();
                api.store
                    .authorize_adaptive_work_funding(&operator.principal, &request, now)
                    .unwrap();
                let project = api
                    .store
                    .company_project(
                        &session.grant.authority.tenant_id,
                        &session.grant.authority.project_id,
                    )
                    .unwrap()
                    .unwrap();
                reconcile_review_at(&api, &project, now);
                (temp, api, None)
            }
            6 => {
                let (temp, api, source) = funded_review_recovery::tests::live_fixture();
                let proof = TestFundedRecoveryGuard::fixture(&source);
                funded_review_recovery::tests::issue_retry(&api, &source);
                (temp, api, Some(proof))
            }
            _ => panic!("funded review schema required"),
        };
        let agent = api
            .principals
            .principal("pm")
            .unwrap()
            .principal
            .agent_id
            .unwrap();
        let call = api.leadership_review_for_agent(agent).unwrap().unwrap();
        assert_eq!(call.grant.schema_version, schema);
        let context = api
            .prepare_leadership_review(&LeadershipAuthority::from_call(&call))
            .unwrap();
        let request_id = call.request_id();
        let request_digest = "c".repeat(64);
        api.event_store
            .as_ref()
            .unwrap()
            .reserve_llm_request(&request_id, &request_digest, &agent.to_string())
            .unwrap();
        let kind = if schema == 5 {
            "work_funding"
        } else {
            "funded_undispatched_recovery"
        };
        let grant = &context.binding.grant;
        let dispatched = api.subscription_dispatch(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 5, "allowance_id": context.binding.allowance_id,
                "agent_id": agent.0, "request_id": request_id,
                "request_digest": request_digest, "context_digest": context.context_digest,
                "provider": grant.provider, "model": grant.model,
                "catalog_digest": grant.catalog_digest,
                "subject": {"kind": "adaptive_leadership_review",
                    "review_id": grant.review_id, "review_kind": kind},
            }))
            .unwrap(),
        );
        assert_eq!(
            dispatched.status,
            200,
            "{}",
            String::from_utf8_lossy(&dispatched.body)
        );
        let completion = ModelExecutionCompletion {
            context: ModelExecutionContext::AdaptiveLeadershipReview(Box::new(context.clone())),
            content: serde_json::json!({
                "schema_version": schema,
                "decision": {"kind": "continue", "additional_model_calls": 1,
                    "window_ms": WINDOW_MS, "rationale": "Synthetic sealed-window replay.",
                    "evidence_refs": [context.source.evidence_refs[0]]},
            })
            .to_string(),
            admissible: true,
        };
        persist(
            &api,
            &completion,
            &context,
            &request_id,
            &request_digest,
            true,
        );
        Self {
            api,
            context,
            completion,
            request_id,
            request_digest,
            _proof: proof,
            _temp: temp,
        }
    }

    fn call(&self) -> AdaptiveLeadershipReviewCallV1 {
        self.api
            .store
            .adaptive_leadership_review_call(
                &self.context.binding.grant.leadership_principal.tenant_id,
                self.context.binding.grant.review_id,
            )
            .unwrap()
            .unwrap()
    }

    fn source_state(&self) -> serde_json::Value {
        let grant = &self.context.binding.grant;
        serde_json::json!({
            "project": self.api.store.company_project(
                &grant.leadership_principal.tenant_id, &grant.project_id,
            ).unwrap().unwrap(),
            "session": self.api.store.adaptive_session_for_authority(
                &grant.assignee_authority,
            ).unwrap().unwrap(),
        })
    }

    fn audit(
        &self,
        result: &CompleteAdaptiveLeadershipReviewCallV1,
    ) -> sentinel_common::EventEnvelopeV2 {
        self.api
            .event_store
            .as_ref()
            .unwrap()
            .event_v2_by_id(&result.resolution_event_id.unwrap().to_string())
            .unwrap()
            .unwrap()
    }

    fn append_audit(&self) -> CompleteAdaptiveLeadershipReviewCallV1 {
        let call = self.call();
        let funding_end = call
            .grant
            .work_funding
            .as_ref()
            .unwrap()
            .binding
            .limits
            .expires_at_unix_ms;
        // Script a valid issuance near the epoch tail; no wall-clock waiting or provider call.
        let issued_at_ms = (funding_end - WINDOW_MS - 1_000).min(call.grant.expires_at_unix_ms - 1);
        assert!(issued_at_ms >= call.updated_at_unix_ms);
        let deadline_ms = issued_at_ms + WINDOW_MS;
        let decision: AdaptiveLeadershipReviewDecisionV1 =
            serde_json::from_str(&self.completion.content).unwrap();
        let response = format!("{:x}", Sha256::digest(self.completion.content.as_bytes()));
        let event_id = adaptive_leadership_continuation_audit_id(
            call.grant.review_id,
            &self.request_digest,
            &response,
            &decision,
        )
        .unwrap();
        let allowance = call
            .continuation_allowance(issued_at_ms, deadline_ms, 1)
            .unwrap();
        let provider_digest = adaptive_leadership_continuation_provider_authority_digest(
            &allowance,
            &call.grant.assignee_authority,
        )
        .unwrap();
        let Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }) =
            &call.grant.subject
        else {
            panic!("funded budget subject required");
        };
        let result = CompleteAdaptiveLeadershipReviewCallV1 {
            review_id: call.grant.review_id,
            allowance_id: call.allowance_id.clone(),
            request_digest: self.request_digest.clone(),
            model_response_digest: response,
            decision,
            resolution_event_id: Some(event_id),
            continuation: Some(AdaptiveContinuationAuthorizationV1 {
                schema_version: 1,
                operation_id: call.operation_id,
                review_id: call.grant.review_id,
                resolution_event_id: event_id,
                session_id: call.grant.session_id,
                source_session_version: call.grant.expected_session_version,
                source: AdaptiveContinuationSourceV1::BudgetWindowExhausted {
                    active_allowance_digest: budget.active_allowance_digest.clone(),
                    continuation_history_digest: budget.continuation_history_digest.clone(),
                },
                abandoned_model_effect: None,
                provider_allowance_id: allowance.allowance_id.clone(),
                provider_authority_digest: provider_digest,
                issued_at_ms,
                deadline_ms,
                additional_model_calls: 1,
                local_adoption: None,
                resume_policy: None,
                work_funding: call.grant.work_funding.clone(),
            }),
        };
        let before = self.source_state();
        let _fence = self.api.mutation_fence.write().unwrap();
        self.api
            .validate_funded_continuation_before_audit(&call, &result)
            .unwrap();
        assert_eq!(
            self.api.append_continuation_audit(&call, &result).unwrap(),
            result
        );
        assert_eq!(self.call(), call);
        assert_eq!(self.source_state(), before);
        result
    }

    fn accept_at(&self, now: u64) {
        self.api
            .accept_leadership_review_at(
                &self.completion,
                &self.context,
                &self.request_id,
                &self.request_digest,
                now,
            )
            .unwrap();
    }
}

#[test]
fn funded_five_and_six_replay_original_clocks_when_a_new_window_would_exceed_epoch() {
    for schema in [5, 6] {
        let f = Fixture::new(schema);
        let result = f.append_audit();
        let original = result.continuation.as_ref().unwrap();
        assert!(original.local_adoption.is_none());
        let call = f.call();
        let funding_end = call
            .grant
            .work_funding
            .as_ref()
            .unwrap()
            .binding
            .limits
            .expires_at_unix_ms;
        let replay_at = (funding_end - WINDOW_MS + 1).max(call.grant.expires_at_unix_ms);
        assert!(original.issued_at_ms < replay_at && replay_at < original.deadline_ms);
        assert!(replay_at >= call.grant.expires_at_unix_ms);
        assert!(call
            .continuation_allowance(replay_at, replay_at + WINDOW_MS, 1)
            .is_err());
        let audit_bytes = serde_json::to_vec(&f.audit(&result)).unwrap();
        let context_bytes = serde_json::to_vec(&call.context).unwrap();
        let payload = f
            .api
            .event_store
            .as_ref()
            .unwrap()
            .get_llm_completion(&f.request_id)
            .unwrap()
            .unwrap();
        f.accept_at(replay_at);
        let committed = f.call();
        assert_eq!(committed.continuation, result.continuation);
        assert_eq!(committed.decision.as_ref(), Some(&result.decision));
        assert_eq!(
            serde_json::to_vec(&committed.context).unwrap(),
            context_bytes
        );
        let session = f
            .api
            .store
            .adaptive_session_for_authority(&call.grant.assignee_authority)
            .unwrap()
            .unwrap();
        assert_eq!(session.active_deadline_ms(), original.deadline_ms);
        assert_eq!(
            (session.model_calls, session.tool_calls),
            (
                call.context.source_session.model_calls,
                call.context.source_session.tool_calls
            )
        );
        assert!(session.requires_fresh_observation());
        assert_eq!(serde_json::to_vec(&f.audit(&result)).unwrap(), audit_bytes);
        let retained = f
            .api
            .event_store
            .as_ref()
            .unwrap()
            .get_llm_completion(&f.request_id)
            .unwrap()
            .unwrap();
        assert_eq!(retained.payload, payload.payload);
        assert_eq!(retained.request_digest, payload.request_digest);
        let after = f.source_state();
        f.accept_at(funding_end + 1);
        assert_eq!(f.call(), committed);
        assert_eq!(f.source_state(), after);
        assert_eq!(serde_json::to_vec(&f.audit(&result)).unwrap(), audit_bytes);
    }
}

#[test]
fn funded_five_and_six_expired_sealed_windows_retire_without_rebasing_or_source_changes() {
    for schema in [5, 6] {
        for at_funding_end in [false, true] {
            let f = Fixture::new(schema);
            let result = f.append_audit();
            let call = f.call();
            let now = if at_funding_end {
                call.grant
                    .work_funding
                    .as_ref()
                    .unwrap()
                    .binding
                    .limits
                    .expires_at_unix_ms
            } else {
                result.continuation.as_ref().unwrap().deadline_ms
            };
            let before = f.source_state();
            let audit_bytes = serde_json::to_vec(&f.audit(&result)).unwrap();
            f.accept_at(now);
            let retired = f.call();
            assert_eq!(retired.retired_at_unix_ms, Some(now));
            assert!(retired.decision.is_none() && retired.continuation.is_none());
            assert_eq!(f.source_state(), before);
            assert_eq!(serde_json::to_vec(&f.audit(&result)).unwrap(), audit_bytes);
            let payload = f
                .api
                .event_store
                .as_ref()
                .unwrap()
                .get_llm_completion(&f.request_id)
                .unwrap()
                .unwrap();
            assert_eq!(payload.status, "failed");
            assert_eq!(
                payload.last_error.as_deref(),
                Some("leadership_review_stale")
            );
            f.accept_at(now + 1);
            assert_eq!(f.call(), retired);
            assert_eq!(f.source_state(), before);
            assert_eq!(serde_json::to_vec(&f.audit(&result)).unwrap(), audit_bytes);
        }
    }
}

#[test]
fn funded_five_and_six_recovery_rejects_corrupt_envelopes_and_changed_exact_bindings() {
    for schema in [5, 6] {
        let f = Fixture::new(schema);
        let result = f.append_audit();
        let call = f.call();
        let event = f.audit(&result);
        let bytes = serde_json::to_vec(&event).unwrap();
        let before = f.source_state();
        let mut expected = result.clone();
        expected.continuation = None;
        assert_eq!(
            recover_continuation_audit(&call, &expected, &event).unwrap(),
            result
        );
        for mutation in 0..7 {
            let mut changed = event.clone();
            match mutation {
                0 => changed.payload.push(b' '),
                1 => changed.payload_digest = "f".repeat(64),
                2 => changed.causal_context.tenant.id.push('x'),
                3 => changed.causal_context.project.authority_generation += 1,
                4 => changed.causal_context.request_digest = "f".repeat(64),
                5 => changed.producer.push('x'),
                _ => changed.canonical_request_digest = "f".repeat(64),
            }
            assert!(recover_continuation_audit(&call, &expected, &changed).is_err());
        }
        for mutation in 0..3 {
            let mut changed = expected.clone();
            match mutation {
                0 => changed.request_digest = "f".repeat(64),
                1 => changed.model_response_digest = "f".repeat(64),
                _ => changed.allowance_id.push('x'),
            }
            assert!(recover_continuation_audit(&call, &changed, &event).is_err());
        }
        let mut changed_call = call.clone();
        changed_call.context.presentation_version = None;
        assert!(recover_continuation_audit(&changed_call, &expected, &event).is_err());
        assert_eq!(f.call(), call);
        assert_eq!(f.source_state(), before);
        assert_eq!(serde_json::to_vec(&f.audit(&result)).unwrap(), bytes);
    }
}
