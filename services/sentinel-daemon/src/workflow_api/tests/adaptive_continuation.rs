//! Fixture-only crash boundaries; these are not evidence of live leadership selection.
#[test]
fn audited_window_expiring_at_domain_commit_retires_without_mutating_source() {
    let f = Fixture::new(Some(1_000));
    let result = f.append_audit(1_000);
    let audit = f.audit(&result);
    let authorization = result.continuation.as_ref().unwrap();
    let deadline = authorization.deadline_ms;
    let samples = [authorization.issued_at_ms, deadline - 1, deadline, deadline];
    let sampled = std::cell::Cell::new(0_usize);
    f.api
        .accept_leadership_review_with_clock(
            &f.completion,
            &f.context,
            &f.request_id,
            &f.request_digest,
            || {
                let index = sampled.get();
                let now = *samples
                    .get(index)
                    .expect("unexpected acceptance clock sample");
                sampled.set(index + 1);
                now
            },
        )
        .unwrap();
    assert_eq!(sampled.get(), samples.len());
    let retired = f.assert_retired();
    assert_eq!(retired.retired_at_unix_ms, Some(deadline));
    assert_eq!(f.session(), f.context.source.source_session);
    assert_eq!(f.project(), f.context.source.source_project);
    assert_eq!(f.audit(&result), audit);
    f.assert_payload("failed");
    for replay_at in [deadline, deadline + 1] {
        f.accept_at(replay_at).unwrap();
        assert_eq!(f.call(), retired);
        assert_eq!(f.session(), f.context.source.source_session);
        assert_eq!(f.project(), f.context.source.source_project);
        assert_eq!(f.audit(&result), audit);
        f.assert_payload("failed");
    }
}

use super::adaptive_leadership_review::tests::persist;
use super::adaptive_leadership_review::{LeadershipAuthority, LeadershipContext};
use super::model_execution::{ModelExecutionCompletion, ModelExecutionContext};
use super::*;
use sentinel_workflow::{
    adaptive_leadership_continuation_audit_id,
    adaptive_leadership_continuation_provider_authority_digest,
    AdaptiveContinuationAuthorizationV1, AdaptiveContinuationSourceV1, AdaptiveEffectV1,
    AdaptiveLeadershipReviewCallV1, AdaptiveLeadershipReviewDecisionV1,
    AdaptiveLeadershipReviewSubjectV2, AdaptiveModelDecisionV1, AdaptiveSessionGrantV1,
    AdaptiveTransitionV1, CompleteAdaptiveLeadershipReviewCallV1, WorkflowStore,
};
use sha2::{Digest, Sha256};

pub(super) fn reserve_and_claim_schema2(
    api: &WorkflowApi,
    context: &LeadershipContext,
) -> (String, String) {
    let grant = &context.binding.grant;
    assert_eq!(grant.schema_version, 2);
    let id = format!("company-leadership-{}", grant.review_id);
    let digest = "c".repeat(64);
    let events = api.event_store.as_ref().unwrap();
    events
        .reserve_llm_request(
            &id,
            &digest,
            &grant.leadership_principal.agent_id.unwrap().to_string(),
        )
        .unwrap();
    let review_kind = match grant.subject.as_ref().unwrap() {
        AdaptiveLeadershipReviewSubjectV2::UnknownModel { .. } => "unknown_model",
        AdaptiveLeadershipReviewSubjectV2::BlockedContinuation { .. } => "blocked_continuation",
        AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { .. } => {
            "budget_window_exhausted"
        }
    };
    let request = serde_json::json!({
        "schema_version": 5, "allowance_id": context.binding.allowance_id,
        "agent_id": grant.leadership_principal.agent_id.unwrap().0,
        "request_id": id, "request_digest": digest, "context_digest": context.context_digest,
        "provider": grant.provider, "model": grant.model, "catalog_digest": grant.catalog_digest,
        "subject": {"kind": "adaptive_leadership_review", "review_id": grant.review_id,
            "review_kind": review_kind},
    });
    let before = api
        .store
        .adaptive_leadership_review_call(&grant.leadership_principal.tenant_id, grant.review_id)
        .unwrap()
        .unwrap();
    assert!(before.dispatch.is_none());
    let mut missing_kind = request.clone();
    missing_kind["subject"]
        .as_object_mut()
        .unwrap()
        .remove("review_kind");
    assert_eq!(
        api.subscription_dispatch(&serde_json::to_vec(&missing_kind).unwrap())
            .status,
        403
    );
    assert_eq!(api.store.adaptive_leadership_review_call(
        &grant.leadership_principal.tenant_id, grant.review_id,
    ).unwrap().unwrap(), before);
    assert_eq!(
        events.get_llm_completion(&id).unwrap().unwrap().status,
        "provider_in_flight"
    );
    let bytes = serde_json::to_vec(&request).unwrap();
    assert_eq!(api.subscription_dispatch(&bytes).status, 200);
    assert_eq!(api.subscription_dispatch(&bytes).status, 403);
    let dispatched = api
        .store
        .adaptive_leadership_review_call(&grant.leadership_principal.tenant_id, grant.review_id)
        .unwrap()
        .unwrap()
        .dispatch
        .unwrap();
    assert_eq!(dispatched.request_id, id);
    assert_eq!(dispatched.request_digest, digest);
    assert_eq!(dispatched.context_digest, context.context_digest);
    (id, digest)
}

pub(super) fn fixture_schema2_blocked(
    path: &Path,
    event_path: &Path,
) -> (WorkflowApi, LeadershipContext) {
    fixture_schema2_source(path, event_path, false, false, false)
}

pub(super) fn fixture_schema2_recovery_source(
    path: &Path,
    event_path: &Path,
    unknown: bool,
) -> (WorkflowApi, LeadershipContext) {
    fixture_schema2_source(path, event_path, true, unknown, false)
}

pub(super) fn fixture_schema2_recovery_source_with_current_allowance(
    path: &Path,
    event_path: &Path,
    unknown: bool,
) -> (WorkflowApi, LeadershipContext) {
    fixture_schema2_source(path, event_path, true, unknown, true)
}

fn fixture_schema2_source(
    path: &Path,
    event_path: &Path,
    historical_review: bool,
    unknown: bool,
    replace_current_allowance: bool,
) -> (WorkflowApi, LeadershipContext) {
    let mut api = super::model_work::configured_test_api(path);
    let historical = now_unix_ms()
        .checked_sub(if historical_review {
            1_200_000
        } else {
            600_000
        })
        .unwrap();
    let binding = super::model_work::assign_test_work_from_at(&api, Some(8), 0, historical);
    api.subscription_allowance_id = Some(binding.reservation_id.clone());
    api.event_store = Some(sentinel_limbo::EventStore::open(event_path.to_str().unwrap()).unwrap());
    let tenant = TenantId::parse(&binding.tenant_id).unwrap();
    let project_id = ProjectId::parse(&binding.project_id).unwrap();
    let work_item_id = WorkItemId::parse(&binding.work_item_id).unwrap();
    let root_project = api
        .store
        .company_project(&tenant, &project_id)
        .unwrap()
        .unwrap();
    let allowance = root_project.subscription_call.as_ref().unwrap();
    let mut project = root_project.clone();
    let current = api
        .authority
        .as_ref()
        .unwrap()
        .snapshot_for_admission(&tenant, &project_id, &work_item_id, binding.agent_id, false)
        .unwrap();
    let grant = AdaptiveSessionGrantV1 {
        schema_version: 1,
        session_id: Uuid::new_v4(),
        authority: current.clone(),
        provider_allowance_id: allowance.allowance_id.clone(),
        provider_authority_digest: adaptive_leadership_continuation_provider_authority_digest(
            allowance, &current,
        )
        .unwrap(),
        provider: allowance.grant.provider.clone(),
        model: allowance.grant.model.clone(),
        catalog_digest: allowance.grant.catalog_digest.clone(),
        max_output_tokens: 4_096,
        max_call_duration_ms: allowance.grant.max_duration_ms,
        max_model_calls: allowance.grant.max_calls,
        max_tool_calls: allowance.grant.max_calls,
        created_at_ms: allowance.created_at_unix_ms,
        deadline_ms: allowance.grant.expires_at_unix_ms,
    };
    let (_, initial) = api
        .store
        .begin_adaptive_session(&grant, &current, historical)
        .unwrap();
    let effect = AdaptiveEffectV1 {
        id: Uuid::new_v4(),
        request_digest: "a".repeat(64),
    };
    let (_, pending) = api
        .store
        .advance_adaptive_session(
            grant.session_id,
            initial.version,
            Uuid::new_v4(),
            &AdaptiveTransitionV1::ClaimModel {
                effect: effect.clone(),
                previous_observation_digest: None,
            },
            &current,
            historical + 1,
        )
        .unwrap();
    if unknown {
        use super::model_execution::{AdaptiveProviderAuthority, ProviderExecutionAuthority};
        let assignment = project.work_items[&work_item_id]
            .assignments
            .iter()
            .find(|entry| entry.active)
            .unwrap();
        let authority = ProviderExecutionAuthority::Adaptive(Box::new(AdaptiveProviderAuthority {
            schema_version: 3,
            grant: grant.clone(),
            session_version: initial.version,
            effect_id: effect.id,
            assignment_id: assignment.assignment_id.clone(),
            previous_observation: None,
        }));
        let request_id = authority.request_id();
        let events = api.event_store.as_ref().unwrap();
        events
            .reserve_llm_request(
                &request_id,
                &effect.request_digest,
                &current.agent_id.to_string(),
            )
            .unwrap();
        // Synthetic before-send registration; the unknown receipt is sealed by the real store.
        events
            .bind_llm_model_reservation(&sentinel_limbo::LlmModelReservationV1 {
                schema_version: 1,
                request_id: request_id.clone(),
                request_digest: effect.request_digest.clone(),
                owner_scope: sentinel_common::StateTransferScope::for_agent(
                    current.agent_id.to_string(),
                ),
                subject: sentinel_limbo::LlmModelSubjectV1::Adaptive {
                    session_id: grant.session_id,
                    effect_id: effect.id,
                    session_version: initial.version,
                },
                allowance_id: grant.provider_allowance_id.clone(),
                context_digest: "d".repeat(64),
                authority_digest: sentinel_common::sha256_hex(
                    &serde_json::to_vec(&authority).unwrap(),
                ),
                usage_binding: sentinel_limbo::LlmModelUsageBindingV1 {
                    agent_id: current.agent_id,
                    tenant_id: current.tenant_id.0.clone(),
                    project_id: current.project_id.0.clone(),
                    work_item_id: current.work_item_id.0.clone(),
                    reservation_id: grant.provider_allowance_id.clone(),
                    assignment_id: assignment.assignment_id.clone(),
                    assignment_version: current.assignment_version,
                    provider: grant.provider.clone(),
                    model: grant.model.clone(),
                },
            })
            .unwrap();
        assert!(events
            .mark_llm_provider_outcome_unknown(
                &request_id,
                &effect.request_digest,
                "UnknownOutcome: provider_transport_deadline_elapsed",
            )
            .unwrap());
        api.store
            .advance_adaptive_session(
                grant.session_id,
                pending.version,
                Uuid::new_v4(),
                &AdaptiveTransitionV1::MarkUnknown {
                    effect: effect.clone(),
                },
                &current,
                historical + 2,
            )
            .unwrap();
    } else {
        api.store
            .advance_adaptive_session(
                grant.session_id,
                pending.version,
                Uuid::new_v4(),
                &AdaptiveTransitionV1::ResolveModel {
                    effect,
                    result_digest: "b".repeat(64),
                    decision: AdaptiveModelDecisionV1::Blocked {
                        reason_code: "dependency_unavailable".into(),
                    },
                },
                &current,
                historical + 2,
            )
            .unwrap();
    }

    let leader = api.principals.principal("pm").unwrap();
    if replace_current_allowance {
        assert!(historical_review);
        let before = api
            .store
            .adaptive_session(grant.session_id, &current)
            .unwrap()
            .unwrap();
        let mut replacement = allowance.grant.clone();
        replacement.expires_at_unix_ms = grant.deadline_ms + 300_000;
        let response = api
            .store
            .apply_company_command(
                &leader.principal,
                Uuid::new_v4(),
                &CompanyWorkflowCommandV1::GrantSubscriptionCall {
                    project_id: project_id.clone(),
                    expected_version: project.version,
                    grant: replacement,
                },
                grant.deadline_ms,
            )
            .unwrap()
            .response;
        let CompanyWorkflowResponseV1::Project(replaced) = response else {
            panic!("replacement allowance project");
        };
        project = *replaced;
        let replacement = project.subscription_call.as_ref().unwrap();
        assert_ne!(replacement.allowance_id, grant.provider_allowance_id);
        assert!(replacement.dispatch.is_none());
        assert_eq!(
            api.store
                .adaptive_session(grant.session_id, &current)
                .unwrap()
                .unwrap(),
            before
        );
        api.subscription_allowance_id = Some(replacement.allowance_id.clone());
    }

    // Synthetic planning receipt only, as in the shared fixture. Keep its policy
    // identical to the root allowance; no provider request or decision is implied.
    let source = api
        .store
        .company_project_events_since(&tenant, 0, 100)
        .unwrap()
        .into_iter()
        .find(|event| event.project_id == project_id && event.event_type == "project_created")
        .unwrap()
        .project;
    let now = if historical_review {
        grant.deadline_ms
    } else {
        now_unix_ms()
    };
    if historical_review {
        assert!(
            now + 300_000 + 10_000 < now_unix_ms(),
            "historical review needs room before epoch issuance"
        );
    }
    let mut planning = sentinel_workflow::ProjectPlanningCallV1 {
        schema_version: 1,
        allowance_id: "continuation-fixture-planning".into(),
        operation_id: Uuid::new_v4(),
        grant: sentinel_workflow::ProjectPlanningGrantV1 {
            schema_version: 1,
            project_id: project_id.clone(),
            expected_version: source.version,
            planner_principal: leader.principal.clone(),
            provider: allowance.grant.provider.clone(),
            model: allowance.grant.model.clone(),
            catalog_digest: allowance.grant.catalog_digest.clone(),
            max_duration_ms: allowance.grant.max_duration_ms,
            token_policy: allowance.grant.token_policy,
            expires_at_unix_ms: now + 300_000,
        },
        source_project: source,
        version: 3,
        created_at_unix_ms: now,
        grant_issued_at_unix_ms: now,
        updated_at_unix_ms: now,
        dispatch: None,
        planned_project: Some(project.clone()),
        model_response_digest: Some("b".repeat(64)),
    };
    planning.dispatch = Some(sentinel_workflow::RequestProviderDispatchV1 {
        request_id: planning.request_id(),
        request_digest: "c".repeat(64),
        context_digest: "d".repeat(64),
        dispatched_at_unix_ms: now,
    });
    let payload = serde_json::to_vec(&planning).unwrap();
    let mut hash = Sha256::new();
    hash.update(b"sentinel.workflow.company-entity-row.v1\0");
    hash.update(serde_json::to_vec(&payload).unwrap());
    sentinel_limbo::rusqlite::Connection::open(path).unwrap().execute(
        "INSERT INTO company_entities(tenant_id,entity_kind,entity_id,version,payload,payload_digest)
         VALUES(?1,'project_planning_call',?2,3,?3,?4)",
        sentinel_limbo::rusqlite::params![tenant.0, project_id.0, payload,
            format!("{:x}", hash.finalize())],
    ).unwrap();
    assert_eq!(
        api.store
            .project_planning_call(&tenant, &project_id)
            .unwrap(),
        Some(planning)
    );
    if historical_review {
        // Seed historical review admission through the store, without changing daemon clock APIs.
        let session = api
            .store
            .adaptive_session(grant.session_id, &current)
            .unwrap()
            .unwrap();
        let work = &project.work_items[&work_item_id];
        let (profile, _) = api
            .authority
            .as_ref()
            .unwrap()
            .profile_for_binding(&current.profile_id)
            .unwrap();
        let catalog = super::model_execution::tool_catalog::adaptive_tool_catalog(
            profile, &current, &work.spec,
        )
        .unwrap();
        let (reason, subject, refs) = match &session.cursor {
            AdaptiveCursorV1::ModelUnknown { effect } => {
                let proof = api
                    .read_only_unknown_model_proof_digest(&project, &session, effect)
                    .unwrap()
                    .unwrap();
                (
                    String::new(),
                    AdaptiveLeadershipReviewSubjectV2::UnknownModel {
                        effect: effect.clone(),
                        sealed_unknown_proof_digest: proof.clone(),
                    },
                    vec![
                        format!(
                            "adaptive-model-unknown:{}:{}",
                            effect.id, effect.request_digest
                        ),
                        format!("sealed-provider-unknown:{proof}"),
                    ],
                )
            }
            AdaptiveCursorV1::Blocked { reason_code } => (
                reason_code.clone(),
                AdaptiveLeadershipReviewSubjectV2::BlockedContinuation {
                    reason_code: reason_code.clone(),
                    resolution_event_id: None,
                },
                vec![format!(
                    "adaptive-model-result:{}",
                    session.last_model_result_digest.as_ref().unwrap()
                )],
            ),
            _ => panic!("unexpected historical fixture cursor"),
        };
        let fingerprint =
            sentinel_workflow::adaptive_leadership_evidence_fingerprint(&catalog, &refs).unwrap();
        let review_grant = sentinel_workflow::AdaptiveLeadershipReviewGrantV1 {
            schema_version: 2,
            recovery_epoch: None,
            subject: Some(subject),
            review_id: sentinel_workflow::adaptive_leadership_review_id(
                grant.session_id,
                session.version,
                &fingerprint,
            )
            .unwrap(),
            project_id: project_id.clone(),
            expected_project_version: project.version,
            work_item_id: work_item_id.clone(),
            session_id: grant.session_id,
            expected_session_version: session.version,
            expected_reason_code: reason,
            evidence_fingerprint: fingerprint,
            leadership_principal: leader.principal.clone(),
            leadership_authority: leader.execution_authority.clone(),
            assignment_id: work
                .assignments
                .iter()
                .find(|entry| entry.active)
                .unwrap()
                .assignment_id
                .clone(),
            assignee_authority: current.clone(),
            provider: allowance.grant.provider.clone(),
            model: allowance.grant.model.clone(),
            catalog_digest: allowance.grant.catalog_digest.clone(),
            max_duration_ms: allowance.grant.max_duration_ms,
            token_policy: allowance.grant.token_policy,
            expires_at_unix_ms: now + 1_000,
        };
        let source = sentinel_workflow::AdaptiveLeadershipReviewContextV1 {
            source_project: project.clone(),
            source_session: session,
            tool_catalog: catalog,
            evidence_refs: refs,
        };
        let call = api
            .store
            .authorize_adaptive_leadership_review_call(
                &leader.principal,
                Uuid::new_v4(),
                "local-fixture-history-initial",
                &review_grant,
                &source,
                now,
            )
            .unwrap();
        let context = LeadershipContext {
            private_observation: None,
            binding: LeadershipAuthority::from_call(&call),
            context_digest: call.context_digest().unwrap(),
            source: call.context,
        };
        return (api, context);
    }
    {
        let _fence = api.mutation_fence.write().unwrap();
        assert!(api.reconcile_adaptive_leadership_reviews(&project).unwrap());
    }
    let call = api
        .leadership_review_for_agent(leader.principal.agent_id.unwrap())
        .unwrap()
        .unwrap();
    let context = api
        .prepare_leadership_review(&LeadershipAuthority::from_call(&call))
        .unwrap();
    assert_eq!(context.context_digest, call.context_digest().unwrap());
    (api, context)
}

struct Fixture {
    temp: tempfile::TempDir,
    api: WorkflowApi,
    context: LeadershipContext,
    completion: ModelExecutionCompletion,
    request_id: String,
    request_digest: String,
    payload: String,
}

impl Fixture {
    fn new(window_ms: Option<u64>) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let (api, context) = fixture_schema2_blocked(
            &temp.path().join("company.db"),
            &temp.path().join("events.db"),
        );
        assert_eq!(context.binding.grant.schema_version, 2);
        assert!(matches!(
            &context.binding.grant.subject,
            Some(AdaptiveLeadershipReviewSubjectV2::BlockedContinuation {
                resolution_event_id: None,
                ..
            })
        ));
        assert!(context.source.source_session.active_deadline_ms() <= context.binding.issued_at_ms);
        let decision = match window_ms {
            Some(window_ms) => serde_json::json!({
                "kind": "continue", "additional_model_calls": 1, "window_ms": window_ms,
                "rationale": "Inspect the current assignment before continuing.",
                "evidence_refs": context.source.evidence_refs,
            }),
            None => serde_json::json!({
                "kind": "keep_blocked", "rationale": "The dependency remains unavailable.",
                "evidence_refs": context.source.evidence_refs,
            }),
        };
        let completion = ModelExecutionCompletion {
            context: ModelExecutionContext::AdaptiveLeadershipReview(Box::new(context.clone())),
            content: serde_json::json!({"schema_version": 2, "decision": decision}).to_string(),
            admissible: true,
        };
        let (request_id, request_digest) = reserve_and_claim_schema2(&api, &context);
        persist(
            &api,
            &completion,
            &context,
            &request_id,
            &request_digest,
            true,
        );
        let payload = api
            .event_store
            .as_ref()
            .unwrap()
            .get_llm_completion(&request_id)
            .unwrap()
            .unwrap()
            .payload;
        Self {
            temp,
            api,
            context,
            completion,
            request_id,
            request_digest,
            payload,
        }
    }

    fn store(&self) -> WorkflowStore {
        WorkflowStore::open(self.temp.path().join("company.db")).unwrap()
    }

    fn events(&self) -> sentinel_limbo::EventStore {
        sentinel_limbo::EventStore::open(self.temp.path().join("events.db").to_str().unwrap())
            .unwrap()
    }

    fn call(&self) -> AdaptiveLeadershipReviewCallV1 {
        self.store()
            .adaptive_leadership_review_call(
                &self.context.binding.grant.leadership_principal.tenant_id,
                self.context.binding.grant.review_id,
            )
            .unwrap()
            .unwrap()
    }

    fn session(&self) -> AdaptiveSessionV1 {
        self.store()
            .adaptive_session(
                self.context.binding.grant.session_id,
                &self.context.binding.grant.assignee_authority,
            )
            .unwrap()
            .unwrap()
    }

    fn project(&self) -> sentinel_workflow::ProjectV1 {
        self.store()
            .company_project(
                &self.context.binding.grant.leadership_principal.tenant_id,
                &self.context.binding.grant.project_id,
            )
            .unwrap()
            .unwrap()
    }

    fn accept_at(&self, now_ms: u64) -> Result<(), &'static str> {
        self.api.accept_leadership_review_at(
            &self.completion,
            &self.context,
            &self.request_id,
            &self.request_digest,
            now_ms,
        )
    }

    fn assert_payload(&self, status: &str) {
        let events = self.events();
        let queued = events
            .get_llm_completion(&self.request_id)
            .unwrap()
            .unwrap();
        assert_eq!(queued.status, status);
        assert_eq!(queued.payload, self.payload);
        assert_eq!(queued.request_digest, self.request_digest);
        if status == "failed" {
            assert_eq!(
                queued.last_error.as_deref(),
                Some("leadership_review_stale")
            );
        }
        let payload: serde_json::Value = serde_json::from_str(&queued.payload).unwrap();
        let persisted = events
            .event_by_operation_id(&format!("llm_usage_{}", self.request_id))
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(persisted).unwrap(),
            payload["usage_event"]
        );
    }

    fn assert_retired(&self) -> AdaptiveLeadershipReviewCallV1 {
        let call = self.call();
        assert_eq!(call.version, 4);
        assert!(call.retired_at_unix_ms.is_some());
        assert!(call.decision.is_none());
        assert!(call.model_response_digest.is_none());
        assert!(call.resolution_event_id.is_none());
        assert!(call.continuation.is_none());
        assert!(call.dispatch.is_some());
        call
    }

    // Stop after the real EventStore append, before the domain transaction.
    fn append_audit(&self, window_ms: u64) -> CompleteAdaptiveLeadershipReviewCallV1 {
        let call = self.call();
        let decision: AdaptiveLeadershipReviewDecisionV1 =
            serde_json::from_str(&self.completion.content).unwrap();
        let response_digest = format!("{:x}", Sha256::digest(self.completion.content.as_bytes()));
        let event_id = adaptive_leadership_continuation_audit_id(
            call.grant.review_id,
            &self.request_digest,
            &response_digest,
            &decision,
        )
        .unwrap();
        let issued_at_ms = now_unix_ms().max(call.dispatch.as_ref().unwrap().dispatched_at_unix_ms);
        let deadline_ms = issued_at_ms + window_ms;
        let allowance = call
            .continuation_allowance(issued_at_ms, deadline_ms, 1)
            .unwrap();
        let Some(AdaptiveLeadershipReviewSubjectV2::BlockedContinuation {
            reason_code,
            resolution_event_id: None,
        }) = &call.grant.subject
        else {
            panic!("expired blocked source");
        };
        let authorization = AdaptiveContinuationAuthorizationV1 {
            schema_version: 1,
            operation_id: call.operation_id,
            review_id: call.grant.review_id,
            resolution_event_id: event_id,
            session_id: call.grant.session_id,
            source_session_version: call.grant.expected_session_version,
            source: AdaptiveContinuationSourceV1::Blocked {
                reason_code: reason_code.clone(),
            },
            abandoned_model_effect: None,
            provider_allowance_id: allowance.allowance_id.clone(),
            provider_authority_digest: adaptive_leadership_continuation_provider_authority_digest(
                &allowance,
                &call.grant.assignee_authority,
            )
            .unwrap(),
            issued_at_ms,
            deadline_ms,
            additional_model_calls: 1,
            local_adoption: None,
        };
        let proposed = CompleteAdaptiveLeadershipReviewCallV1 {
            review_id: call.grant.review_id,
            allowance_id: call.allowance_id.clone(),
            request_digest: self.request_digest.clone(),
            model_response_digest: response_digest,
            decision,
            resolution_event_id: Some(event_id),
            continuation: Some(authorization),
        };
        let _fence = self.api.mutation_fence.write().unwrap();
        let audited = self
            .api
            .append_continuation_audit(&call, &proposed)
            .unwrap();
        assert_eq!(audited, proposed);
        assert_eq!(self.session(), self.context.source.source_session);
        assert_eq!(self.project(), self.context.source.source_project);
        assert_eq!(self.call(), call);
        audited
    }

    fn audit(&self, result: &CompleteAdaptiveLeadershipReviewCallV1) -> serde_json::Value {
        serde_json::to_value(
            self.events()
                .event_v2_by_id(&result.resolution_event_id.unwrap().to_string())
                .unwrap()
                .unwrap(),
        )
        .unwrap()
    }
}

#[test]
fn schema2_retirement_crash_replay_disposes_exact_ready_payload() {
    let f = Fixture::new(None);
    let call = f.call();
    let project = f.project();
    let leader = f.api.principals.principal("pm").unwrap();
    let now = now_unix_ms().max(project.updated_at_unix_ms);
    f.api
        .store
        .apply_company_command(
            &leader.principal,
            Uuid::new_v4(),
            &CompanyWorkflowCommandV1::RecordDecision {
                project_id: project.project_id.clone(),
                expected_version: project.version,
                work_item_id: None,
                choice_ref: "Independent decision".into(),
                rationale_ref: "Assignment and governance unchanged".into(),
            },
            now,
        )
        .unwrap();
    let changed_project = f.project();
    f.api
        .store
        .retire_stale_adaptive_leadership_review_call(
            &leader.principal,
            call.grant.review_id,
            call.version,
            now,
        )
        .unwrap();
    let retired = f.assert_retired();
    f.assert_payload("ready_for_action");
    assert!(f
        .api
        .accept_leadership_review_at(
            &f.completion,
            &f.context,
            &f.request_id,
            &"e".repeat(64),
            now,
        )
        .is_err());
    f.assert_payload("ready_for_action");
    for replay_at in [now, now + 1] {
        f.accept_at(replay_at).unwrap();
        f.assert_payload("failed");
        assert_eq!(f.call(), retired);
        assert_eq!(f.session(), f.context.source.source_session);
        assert_eq!(f.project(), changed_project);
    }
}

#[test]
fn audit_only_crash_replay_keeps_clock_and_commits_once() {
    let f = Fixture::new(Some(120_000));
    let result = f.append_audit(120_000);
    let authorization = result.continuation.as_ref().unwrap();
    let audit = f.audit(&result);
    f.assert_payload("ready_for_action");
    f.accept_at(authorization.issued_at_ms + 7).unwrap();
    let receipt = f.call();
    let session = f.session();
    let project = f.project();
    assert_eq!(receipt.version, 3);
    assert_eq!(receipt.continuation.as_ref(), Some(authorization));
    assert!(receipt.retired_at_unix_ms.is_none());
    assert_eq!(receipt.decision.as_ref(), Some(&result.decision));
    assert_eq!(
        receipt.model_response_digest.as_ref(),
        Some(&result.model_response_digest)
    );
    assert_eq!(receipt.resolution_event_id, result.resolution_event_id);
    assert_eq!(session.version, f.context.source.source_session.version + 1);
    assert_eq!(session.grant, f.context.source.source_session.grant);
    assert_eq!(
        session.model_calls,
        f.context.source.source_session.model_calls
    );
    assert_eq!(
        session.tool_calls,
        f.context.source.source_session.tool_calls
    );
    assert!(session.requires_fresh_observation());
    assert_eq!(
        session.effective_grant().max_model_calls,
        session.model_calls + 1
    );
    assert!(session.effective_grant().max_model_calls <= session.grant.max_model_calls);
    assert_eq!(session.active_deadline_ms(), authorization.deadline_ms);
    assert_eq!(
        session.active_provider_allowance_id(),
        authorization.provider_allowance_id
    );
    assert_eq!(
        project.subscription_call.as_ref(),
        Some(
            &receipt
                .continuation_allowance(authorization.issued_at_ms, authorization.deadline_ms, 1,)
                .unwrap()
        )
    );
    // A committed receipt is replayable even after its window expires.
    for replay_at in [
        authorization.issued_at_ms + 8,
        authorization.deadline_ms + 1,
    ] {
        f.accept_at(replay_at).unwrap();
        assert_eq!(f.call(), receipt);
        assert_eq!(f.session(), session);
        assert_eq!(f.project(), project);
        assert_eq!(f.audit(&result), audit);
        f.assert_payload("ready_for_action");
    }
}

#[test]
fn audit_only_crash_at_deadline_retires_without_renewal() {
    for offset in [0, 1] {
        let f = Fixture::new(Some(1_000));
        let result = f.append_audit(1_000);
        let audit = f.audit(&result);
        let now = result.continuation.as_ref().unwrap().deadline_ms + offset;
        f.accept_at(now).unwrap();
        let retired = f.assert_retired();
        f.assert_payload("failed");
        f.accept_at(now + 1).unwrap();
        assert_eq!(f.call(), retired);
        assert_eq!(f.session(), f.context.source.source_session);
        assert_eq!(f.project(), f.context.source.source_project);
        assert_eq!(f.audit(&result), audit);
        f.assert_payload("failed");
    }
}

#[test]
fn audited_expiry_retirement_crash_replay_finishes_outbox_disposition() {
    let f = Fixture::new(Some(1_000));
    let result = f.append_audit(1_000);
    let audit = f.audit(&result);
    let deadline = result.continuation.as_ref().unwrap().deadline_ms;
    let leader = f.api.principals.principal("pm").unwrap();
    f.api
        .store
        .retire_expired_adaptive_continuation_call(&leader.principal, &result, deadline)
        .unwrap();
    let retired = f.assert_retired();
    f.assert_payload("ready_for_action");
    for replay_at in [deadline, deadline + 1] {
        f.accept_at(replay_at).unwrap();
        assert_eq!(f.call(), retired);
        assert_eq!(f.session(), f.context.source.source_session);
        assert_eq!(f.project(), f.context.source.source_project);
        assert_eq!(f.audit(&result), audit);
        f.assert_payload("failed");
    }
}
