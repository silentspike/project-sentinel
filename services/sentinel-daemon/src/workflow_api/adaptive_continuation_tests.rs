//! Fixture-only crash boundaries; these are not evidence of live leadership selection.
use super::adaptive_leadership_review::tests::{persist, reserve_and_claim};
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

fn fixture_schema2_blocked(path: &Path, event_path: &Path) -> (WorkflowApi, LeadershipContext) {
    let mut api = super::model_work::configured_test_api(path);
    let historical = now_unix_ms().checked_sub(600_000).unwrap();
    let binding = super::model_work::assign_test_work_from_at(&api, Some(8), 0, historical);
    api.subscription_allowance_id = Some(binding.reservation_id.clone());
    api.event_store = Some(sentinel_limbo::EventStore::open(event_path.to_str().unwrap()).unwrap());
    let tenant = TenantId::parse(&binding.tenant_id).unwrap();
    let project_id = ProjectId::parse(&binding.project_id).unwrap();
    let work_item_id = WorkItemId::parse(&binding.work_item_id).unwrap();
    let project = api.store.company_project(&tenant, &project_id).unwrap().unwrap();
    let allowance = project.subscription_call.as_ref().unwrap();
    let current = api.authority.as_ref().unwrap().snapshot_for_admission(
        &tenant, &project_id, &work_item_id, binding.agent_id, false,
    ).unwrap();
    let grant = AdaptiveSessionGrantV1 {
        schema_version: 1,
        session_id: Uuid::new_v4(),
        authority: current.clone(),
        provider_allowance_id: allowance.allowance_id.clone(),
        provider_authority_digest: adaptive_leadership_continuation_provider_authority_digest(
            allowance, &current,
        ).unwrap(),
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
    let (_, initial) = api.store.begin_adaptive_session(&grant, &current, historical).unwrap();
    let effect = AdaptiveEffectV1 { id: Uuid::new_v4(), request_digest: "a".repeat(64) };
    let (_, pending) = api.store.advance_adaptive_session(
        grant.session_id, initial.version, Uuid::new_v4(),
        &AdaptiveTransitionV1::ClaimModel {
            effect: effect.clone(), previous_observation_digest: None,
        }, &current, historical + 1,
    ).unwrap();
    api.store.advance_adaptive_session(
        grant.session_id, pending.version, Uuid::new_v4(),
        &AdaptiveTransitionV1::ResolveModel {
            effect, result_digest: "b".repeat(64),
            decision: AdaptiveModelDecisionV1::Blocked {
                reason_code: "dependency_unavailable".into(),
            },
        }, &current, historical + 2,
    ).unwrap();

    // Synthetic planning receipt only, as in the shared fixture. Keep its policy
    // identical to the root allowance; no provider request or decision is implied.
    let source = api.store.company_project_events_since(&tenant, 0, 100).unwrap()
        .into_iter().find(|event| {
            event.project_id == project_id && event.event_type == "project_created"
        }).unwrap().project;
    let leader = api.principals.principal("pm").unwrap();
    let now = now_unix_ms();
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
        request_id: planning.request_id(), request_digest: "c".repeat(64),
        context_digest: "d".repeat(64), dispatched_at_unix_ms: now,
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
    assert_eq!(api.store.project_planning_call(&tenant, &project_id).unwrap(), Some(planning));
    {
        let _fence = api.mutation_fence.write().unwrap();
        assert!(api.reconcile_adaptive_leadership_reviews(&project).unwrap());
    }
    let call = api.leadership_review_for_agent(leader.principal.agent_id.unwrap())
        .unwrap().unwrap();
    let context = api.prepare_leadership_review(&LeadershipAuthority::from_call(&call)).unwrap();
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
        let (request_id, request_digest) = reserve_and_claim(&api, &context);
        persist(&api, &completion, &context, &request_id, &request_digest, true);
        let payload = api.event_store.as_ref().unwrap()
            .get_llm_completion(&request_id).unwrap().unwrap().payload;
        Self { temp, api, context, completion, request_id, request_digest, payload }
    }

    fn store(&self) -> WorkflowStore {
        WorkflowStore::open(self.temp.path().join("company.db")).unwrap()
    }

    fn events(&self) -> sentinel_limbo::EventStore {
        sentinel_limbo::EventStore::open(
            self.temp.path().join("events.db").to_str().unwrap(),
        ).unwrap()
    }

    fn call(&self) -> AdaptiveLeadershipReviewCallV1 {
        self.store().adaptive_leadership_review_call(
            &self.context.binding.grant.leadership_principal.tenant_id,
            self.context.binding.grant.review_id,
        ).unwrap().unwrap()
    }

    fn session(&self) -> AdaptiveSessionV1 {
        self.store().adaptive_session(
            self.context.binding.grant.session_id,
            &self.context.binding.grant.assignee_authority,
        ).unwrap().unwrap()
    }

    fn project(&self) -> sentinel_workflow::ProjectV1 {
        self.store().company_project(
            &self.context.binding.grant.leadership_principal.tenant_id,
            &self.context.binding.grant.project_id,
        ).unwrap().unwrap()
    }

    fn accept_at(&self, now_ms: u64) -> Result<(), &'static str> {
        self.api.accept_leadership_review_at(
            &self.completion, &self.context, &self.request_id, &self.request_digest, now_ms,
        )
    }

    fn assert_payload(&self, status: &str) {
        let events = self.events();
        let queued = events.get_llm_completion(&self.request_id).unwrap().unwrap();
        assert_eq!(queued.status, status);
        assert_eq!(queued.payload, self.payload);
        assert_eq!(queued.request_digest, self.request_digest);
        if status == "failed" {
            assert_eq!(queued.last_error.as_deref(), Some("leadership_review_stale"));
        }
        let payload: serde_json::Value = serde_json::from_str(&queued.payload).unwrap();
        let persisted = events.event_by_operation_id(
            &format!("llm_usage_{}", self.request_id),
        ).unwrap().unwrap();
        assert_eq!(serde_json::to_value(persisted).unwrap(), payload["usage_event"]);
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
            call.grant.review_id, &self.request_digest, &response_digest, &decision,
        ).unwrap();
        let issued_at_ms = now_unix_ms().max(call.dispatch.as_ref().unwrap().dispatched_at_unix_ms);
        let deadline_ms = issued_at_ms + window_ms;
        let allowance = call.continuation_allowance(issued_at_ms, deadline_ms, 1).unwrap();
        let Some(AdaptiveLeadershipReviewSubjectV2::BlockedContinuation {
            reason_code, resolution_event_id: None,
        }) = &call.grant.subject else { panic!("expired blocked source"); };
        let authorization = AdaptiveContinuationAuthorizationV1 {
            schema_version: 1,
            operation_id: call.operation_id,
            review_id: call.grant.review_id,
            resolution_event_id: event_id,
            session_id: call.grant.session_id,
            source_session_version: call.grant.expected_session_version,
            source: AdaptiveContinuationSourceV1::Blocked { reason_code: reason_code.clone() },
            abandoned_model_effect: None,
            provider_allowance_id: allowance.allowance_id.clone(),
            provider_authority_digest: adaptive_leadership_continuation_provider_authority_digest(
                &allowance, &call.grant.assignee_authority,
            ).unwrap(),
            issued_at_ms,
            deadline_ms,
            additional_model_calls: 1,
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
        let audited = self.api.append_continuation_audit(&call, &proposed).unwrap();
        assert_eq!(audited, proposed);
        assert_eq!(self.session(), self.context.source.source_session);
        assert_eq!(self.project(), self.context.source.source_project);
        assert_eq!(self.call(), call);
        audited
    }

    fn audit(&self, result: &CompleteAdaptiveLeadershipReviewCallV1) -> serde_json::Value {
        serde_json::to_value(self.events().event_v2_by_id(
            &result.resolution_event_id.unwrap().to_string(),
        ).unwrap().unwrap()).unwrap()
    }
}

#[test]
fn schema2_retirement_crash_replay_disposes_exact_ready_payload() {
    let f = Fixture::new(None);
    let call = f.call();
    let project = f.project();
    let leader = f.api.principals.principal("pm").unwrap();
    let now = now_unix_ms().max(project.updated_at_unix_ms);
    f.api.store.apply_company_command(
        &leader.principal, Uuid::new_v4(),
        &CompanyWorkflowCommandV1::RecordDecision {
            project_id: project.project_id.clone(), expected_version: project.version,
            work_item_id: None, choice_ref: "Independent decision".into(),
            rationale_ref: "Assignment and governance unchanged".into(),
        }, now,
    ).unwrap();
    let changed_project = f.project();
    f.api.store.retire_stale_adaptive_leadership_review_call(
        &leader.principal, call.grant.review_id, call.version, now,
    ).unwrap();
    let retired = f.assert_retired();
    f.assert_payload("ready_for_action");
    assert!(f.api.accept_leadership_review_at(
        &f.completion, &f.context, &f.request_id, &"e".repeat(64), now,
    ).is_err());
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
    assert_eq!(receipt.model_response_digest.as_ref(), Some(&result.model_response_digest));
    assert_eq!(receipt.resolution_event_id, result.resolution_event_id);
    assert_eq!(session.version, f.context.source.source_session.version + 1);
    assert_eq!(session.grant, f.context.source.source_session.grant);
    assert_eq!(session.model_calls, f.context.source.source_session.model_calls);
    assert_eq!(session.tool_calls, f.context.source.source_session.tool_calls);
    assert!(session.requires_fresh_observation());
    assert_eq!(session.effective_grant().max_model_calls, session.model_calls + 1);
    assert!(session.effective_grant().max_model_calls <= session.grant.max_model_calls);
    assert_eq!(session.active_deadline_ms(), authorization.deadline_ms);
    assert_eq!(session.active_provider_allowance_id(), authorization.provider_allowance_id);
    assert_eq!(project.subscription_call.as_ref(), Some(&receipt.continuation_allowance(
        authorization.issued_at_ms, authorization.deadline_ms, 1,
    ).unwrap()));
    // A committed receipt is replayable even after its window expires.
    for replay_at in [authorization.issued_at_ms + 8, authorization.deadline_ms + 1] {
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
    f.api.store.retire_expired_adaptive_continuation_call(
        &leader.principal, &result, deadline,
    ).unwrap();
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
