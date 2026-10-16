// Synthetic retained decisions and clock boundaries, not provider execution evidence.
use super::*;

struct CompletionExpiryFixture {
    temp: tempfile::TempDir,
    api: WorkflowApi,
    context: LeadershipContext,
    completion: ModelExecutionCompletion,
    id: String,
    digest: String,
}

impl CompletionExpiryFixture {
    fn new(funded: bool) -> Self {
        let (temp, api, context) = if funded {
            let (temp, api, session) = super::super::super::adaptive_resume_policy::tests::
                unreviewed_policy_root_with_limits(120_000, false, 4, 4);
            let now = now_unix_ms();
            let operator = api.principals.principal("operator").unwrap();
            let request = api.store.adaptive_work_funding_draft(
                &operator.principal, &session.grant.authority.project_id,
                session.grant.session_id, Uuid::new_v4(), "synthetic-completion-expiry",
                sentinel_workflow::AdaptiveWorkFundingLimitsV1 {
                    additional_model_calls: 4,
                    additional_tool_calls: 4,
                    additional_reviews: 4,
                    additional_windows: 4,
                    max_window_ms: 300_000,
                    max_call_duration_ms: 120_000,
                    dispatch_margin_ms: sentinel_workflow::ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
                    expires_at_unix_ms: now + 600_000,
                }, now,
            ).unwrap();
            api.store.authorize_adaptive_work_funding(&operator.principal, &request, now)
                .unwrap();
            let project = api.store.company_project(
                &session.grant.authority.tenant_id, &session.grant.authority.project_id,
            ).unwrap().unwrap();
            reconcile_review_at(&api, &project, now);
            let agent = api.principals.principal("pm").unwrap().principal.agent_id.unwrap();
            let call = api.leadership_review_for_agent(agent).unwrap().unwrap();
            let context = api.prepare_leadership_review(&LeadershipAuthority::from_call(&call))
                .unwrap();
            assert_eq!(context.binding.grant.schema_version, 5);
            (temp, api, context)
        } else {
            let temp = tempfile::tempdir().unwrap();
            let (api, context) = schema2_expiry_fixture(
                &temp.path().join("company.sqlite"), &temp.path().join("events.sqlite"),
            );
            (temp, api, context)
        };
        assert!(context.binding.grant.subject.is_some());
        let (id, digest) = register_expiry_dispatch(&api, &context, true);
        let completion = ModelExecutionCompletion {
            context: ModelExecutionContext::AdaptiveLeadershipReview(Box::new(context.clone())),
            content: serde_json::json!({
                "schema_version": context.binding.grant.schema_version,
                "decision": {
                    "kind": if funded { "defer_budget" } else { "keep_blocked" },
                    "rationale": "Synthetic independent refusal; preserve the exact source.",
                    "evidence_refs": [context.source.evidence_refs[0]],
                },
            }).to_string(),
            admissible: true,
        };
        persist(&api, &completion, &context, &id, &digest, true);
        Self { temp, api, context, completion, id, digest }
    }

    fn call(&self) -> AdaptiveLeadershipReviewCallV1 {
        expiry_review(&self.api, &self.context)
    }

    fn state(&self) -> Vec<Vec<Vec<sentinel_limbo::rusqlite::types::Value>>> {
        discovery_state(
            &self.temp.path().join("company.sqlite"),
            &self.temp.path().join("events.sqlite"),
        )
    }

    fn assert_source_unchanged(&self) {
        let grant = &self.context.binding.grant;
        assert_eq!(self.api.store.adaptive_session(
            grant.session_id, &grant.assignee_authority,
        ).unwrap(), Some(self.context.source.source_session.clone()));
        assert_eq!(self.api.store.company_project(
            &grant.leadership_principal.tenant_id, &grant.project_id,
        ).unwrap(), Some(self.context.source.source_project.clone()));
        assert!(self.api.store.adaptive_leadership_local_adoption(
            &grant.leadership_principal.tenant_id, grant.review_id,
        ).unwrap().is_none());
    }

    fn accept_samples(&self, times: &[u64]) -> Result<(), &'static str> {
        let samples = std::cell::Cell::new(0_usize);
        let result = self.api.accept_leadership_review_with_clock(
            &self.completion, &self.context, &self.id, &self.digest, || {
                let index = samples.get();
                samples.set(index + 1);
                *times.get(index).expect("unexpected completion clock sample")
            },
        );
        assert_eq!(samples.get(), times.len(), "must reach the final store sample");
        result
    }
}

#[test]
fn nonresolving_final_store_expiry_retires_on_last_attempt_without_changing_payload_or_source() {
    for funded in [true, false] {
        let f = CompletionExpiryFixture::new(funded);
        let events = f.api.event_store.as_ref().unwrap();
        // Model the last admission attempt using the real durable failure API;
        // this does not invoke the bridge scheduler or any provider.
        for attempt in 1..=4 {
            assert_eq!(events.record_llm_completion_failure(
                &f.id, &f.digest, "synthetic prior admission failure", 5,
            ).unwrap(), (attempt, false));
        }
        let retained = events.get_llm_completion(&f.id).unwrap().unwrap();
        assert_eq!(retained.status, "ready_for_action");
        assert_eq!(retained.attempt_count, 4);
        let dispatched = f.call();
        let expiry = dispatched.grant.expires_at_unix_ms;
        assert!(dispatched.updated_at_unix_ms < expiry - 2);
        let before = f.state();

        // Parent precheck, store admission, store final boundary, parent recovery.
        f.accept_samples(&[expiry - 2, expiry - 1, expiry, expiry]).unwrap();

        let retired = f.call();
        assert_eq!(retired.version, 4);
        assert_eq!(retired.retired_at_unix_ms, Some(expiry));
        assert!(retired.decision.is_none());
        assert!(retired.continuation.is_none());
        assert!(retired.model_response_digest.is_none());
        f.assert_source_unchanged();
        let after = f.state();
        assert_eq!(after[3], before[3], "adaptive journal must roll back");
        assert_eq!(after[4], before[4], "adaptive head must roll back");
        let disposed = events.get_llm_completion(&f.id).unwrap().unwrap();
        assert_eq!(disposed.status, "failed");
        assert_eq!(disposed.attempt_count, 5);
        assert_eq!(disposed.last_error.as_deref(), Some("leadership_review_stale"));
        assert_eq!(disposed.payload, retained.payload);
        assert_eq!(disposed.request_digest, retained.request_digest);
        assert_eq!(disposed.owner_scope, retained.owner_scope);

        f.api.accept_leadership_review_with_clock(
            &f.completion, &f.context, &f.id, &f.digest,
            || panic!("terminal replay must not issue or refresh a clock"),
        ).unwrap();
        assert_eq!(f.call(), retired);
        assert_eq!(f.state(), after);
        f.assert_source_unchanged();
    }
}

#[test]
fn nonexpired_nonresolving_store_failure_remains_error_without_retirement_or_disposition() {
    for funded in [true, false] {
        let f = CompletionExpiryFixture::new(funded);
        let call = f.call();
        let now = call.updated_at_unix_ms + 2;
        assert!(now < call.grant.expires_at_unix_ms);
        let before = f.state();
        let retained = f.api.event_store.as_ref().unwrap()
            .get_llm_completion(&f.id).unwrap().unwrap();

        // Reject a backwards final store clock while the parent's fresh clock
        // remains live. Expiry handling must not conceal this unrelated failure.
        assert_eq!(f.accept_samples(&[now, now, now - 1, now]),
            Err("leadership receipt rejected"));

        assert_eq!(f.call(), call);
        assert!(f.call().retired_at_unix_ms.is_none());
        f.assert_source_unchanged();
        assert_eq!(f.state(), before);
        let pending = f.api.event_store.as_ref().unwrap()
            .get_llm_completion(&f.id).unwrap().unwrap();
        assert_eq!(pending.status, "ready_for_action");
        assert_eq!(pending.attempt_count, 0);
        assert_eq!(pending.payload, retained.payload);
        assert_eq!(pending.last_error, retained.last_error);
    }
}
