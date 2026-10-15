// Offline retained-response regressions, not provider or installed-release evidence.
use super::*;
use crate::workflow_api::adaptive_leadership_review::tests::{persist, reconcile_review_at};
use crate::workflow_api::adaptive_resume_policy::tests::{claim_review, unreviewed_policy_root_with_limits};
use sentinel_limbo::rusqlite::{params, Connection};

struct FundedFixture {
    temp: tempfile::TempDir,
    api: WorkflowApi,
    call: AdaptiveLeadershipReviewCallV1,
    context: LeadershipContext,
    completion: ModelExecutionCompletion,
    repair: adaptive_recovery_release::TestFundedLocalAdoptionRepairGuard,
}

impl FundedFixture {
    fn new(calls: u16, window: u64, funding_window: u64, funding_lifetime: u64) -> Self {
        let repair = adaptive_recovery_release::TestFundedLocalAdoptionRepairGuard::fixture().unwrap();
        let (temp, api, source) = unreviewed_policy_root_with_limits(120_000, false, 4, 4);
        let operator = api.principals.principal("operator").unwrap();
        let now = now_unix_ms();
        let request = api.store.adaptive_work_funding_draft(
            &operator.principal, &source.grant.authority.project_id, source.grant.session_id,
            Uuid::new_v4(), "funded-retained-response-test",
            sentinel_workflow::AdaptiveWorkFundingLimitsV1 {
                additional_model_calls: 8, additional_tool_calls: 8,
                additional_reviews: 4, additional_windows: 4,
                max_window_ms: funding_window, max_call_duration_ms: 120_000,
                dispatch_margin_ms: sentinel_workflow::ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
                expires_at_unix_ms: now + funding_lifetime,
            }, now,
        ).unwrap();
        api.store.authorize_adaptive_work_funding(&operator.principal, &request, now).unwrap();
        let project = api.store.company_project(
            &source.grant.authority.tenant_id, &source.grant.authority.project_id,
        ).unwrap().unwrap();
        assert!(reconcile_review_at(&api, &project, now_unix_ms()));
        let call = api.store.adaptive_leadership_review_calls(
            &project.tenant_id, source.grant.session_id,
        ).unwrap().pop().unwrap();
        assert_eq!(call.grant.schema_version, 5);
        let context = api.prepare_leadership_review(&LeadershipAuthority::from_call(&call)).unwrap();
        let (id, digest) = claim_review(&api, &context);
        let completion = ModelExecutionCompletion {
            context: ModelExecutionContext::AdaptiveLeadershipReview(Box::new(context.clone())),
            content: serde_json::json!({"schema_version":5,"decision":{
                "kind":"continue","additional_model_calls":calls,"window_ms":window,
                "rationale":"Use the exact retained response in its original funded scope.",
                "evidence_refs":[context.source.evidence_refs[0]],
            }}).to_string(),
            admissible: true,
        };
        persist(&api, &completion, &context, &id, &digest, true);
        for attempt in 1..=5 {
            assert_eq!(api.event_store.as_ref().unwrap().record_llm_completion_failure(
                &id, &digest, FUNDED_LOCAL_ADOPTION_ERROR, 5,
            ).unwrap(), (attempt, attempt == 5));
        }
        let call = api.store.adaptive_leadership_review_call(
            &project.tenant_id, call.grant.review_id,
        ).unwrap().unwrap();
        assert_eq!(call.version, 2);
        Self { temp, api, call, context, completion, repair }
    }

    fn standard() -> Self { Self::new(8, 300_000, 300_000, 900_000) }

    fn candidate(&self) -> Result<AdaptiveLeadershipLocalAdoptionRequestV1, &'static str> {
        self.api.local_adoption_candidate(
            &self.api.principals.principal("operator").unwrap(),
            self.call.grant.review_id, Uuid::new_v4(), None,
        )
    }

    fn submit(&self, request: &AdaptiveLeadershipLocalAdoptionRequestV1) -> WorkflowHttpResponse {
        self.api.local_adoption_http(
            &self.api.principals.principal("operator").unwrap(), "POST",
            ADAPTIVE_LOCAL_ADOPTION_PATH, &serde_json::to_vec(request).unwrap(),
        )
    }

    fn authorize(&self) -> AdaptiveLeadershipLocalAdoptionV1 {
        let request = self.candidate().unwrap();
        let response = self.submit(&request);
        assert_eq!(response.status, 200, "{}", String::from_utf8_lossy(&response.body));
        let response: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(response["schema_version"], 2);
        self.api.store.adaptive_leadership_local_adoption(
            &request.tenant_id, request.review_id,
        ).unwrap().unwrap()
    }

    fn state(&self) -> Vec<Vec<Vec<sentinel_limbo::rusqlite::types::Value>>> {
        discovery_state(&self.temp.path().join("company.sqlite"), &self.temp.path().join("events.sqlite"))
    }

    fn provider_bindings(&self) -> Vec<Vec<sentinel_limbo::rusqlite::types::Value>> {
        let connection = Connection::open(self.temp.path().join("events.sqlite")).unwrap();
        let mut statement = connection.prepare(
            "SELECT request_id,request_digest,owner_scope,model_binding,payload
             FROM llm_completion_outbox ORDER BY request_id",
        ).unwrap();
        let rows = statement.query_map([], |row| (0..5).map(|i| row.get(i)).collect::<Result<Vec<_>, _>>())
            .unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        rows
    }

    fn row(&self) -> sentinel_limbo::LlmCompletionEntry {
        self.api.event_store.as_ref().unwrap().get_llm_completion(&self.call.request_id()).unwrap().unwrap()
    }

    fn journal_count(&self) -> i64 {
        Connection::open(self.temp.path().join("company.sqlite")).unwrap().query_row(
            "SELECT count(*) FROM workflow_operations WHERE operation_namespace=?1",
            [format!("adaptive-session-v1:{}", self.call.grant.session_id)], |row| row.get(0),
        ).unwrap()
    }

    fn current_call(&self) -> AdaptiveLeadershipReviewCallV1 {
        self.api.store.adaptive_leadership_review_call(
            &self.call.grant.leadership_principal.tenant_id, self.call.grant.review_id,
        ).unwrap().unwrap()
    }

    fn reopen(&mut self) {
        self.api.store = Arc::new(WorkflowStore::open(self.temp.path().join("company.sqlite")).unwrap());
        self.api.event_store = Some(sentinel_limbo::EventStore::open(
            self.temp.path().join("events.sqlite").to_str().unwrap(),
        ).unwrap());
    }

    fn reconcile(&self) -> Result<bool, &'static str> {
        let call = self.current_call();
        let _fence = self.api.mutation_fence.write().unwrap();
        self.api.reconcile_local_leadership_adoption(&call, &now_unix_ms)
    }

    // Fixed clocks are only for intentional future expiry or terminal replay.
    fn reconcile_at(&self, now: u64) -> Result<bool, &'static str> {
        let call = self.current_call();
        let _fence = self.api.mutation_fence.write().unwrap();
        self.api.reconcile_local_leadership_adoption(&call, &|| now)
    }

    fn change_execution_admission(&mut self, mutation: &str) {
        if mutation == "proof-missing" {
            self.repair.remove_proof();
            return;
        }
        if mutation == "proof-changed" {
            self.repair.rotate_proof();
            return;
        }
        let issuer = self.api.principals.principal("operator").unwrap().principal.principal_id;
        let mut principals = PrincipalAuthenticator {
            by_credential_digest: self.api.principals.by_credential_digest.clone(),
            by_principal_id: self.api.principals.by_principal_id.clone(),
        };
        if mutation == "issuer-missing" {
            principals.by_principal_id.remove(&issuer);
            principals.by_credential_digest.retain(|_, bound| bound.principal.principal_id != issuer);
        } else {
            for bound in principals.by_principal_id.values_mut()
                .chain(principals.by_credential_digest.values_mut())
                .filter(|bound| bound.principal.principal_id == issuer)
            {
                match mutation {
                    "issuer-principal" => bound.principal.authority_generation += 1,
                    "issuer-authority" => bound.execution_authority.authority_digest = "d".repeat(64),
                    _ => panic!("unsupported local execution admission change"),
                }
            }
        }
        self.api.principals = Arc::new(principals);
        Arc::make_mut(self.api.authority.as_mut().unwrap()).principals = Arc::clone(&self.api.principals);
    }

    fn proposal(&self, adoption: &AdaptiveLeadershipLocalAdoptionV1) -> CompleteAdaptiveLeadershipReviewCallV1 {
        let decision = adoption.request.decision.clone();
        let response = adoption.request.model_response_digest.clone();
        let id = sentinel_workflow::adaptive_leadership_continuation_audit_id(
            self.call.grant.review_id, &adoption.request.request_digest, &response, &decision,
        ).unwrap();
        let AdaptiveLeadershipReviewDecisionKindV1::Continue { additional_model_calls, .. }
            = &decision.decision else { panic!("fixture must retain Continue") };
        let allowance = self.call.continuation_allowance(
            adoption.issued_at_unix_ms, adoption.continuation_deadline_ms, *additional_model_calls,
        ).unwrap();
        let AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }
            = self.call.grant.subject.as_ref().unwrap() else { panic!("funded budget source") };
        let authorization = AdaptiveContinuationAuthorizationV1 {
            schema_version: 1, operation_id: self.call.operation_id,
            review_id: self.call.grant.review_id, resolution_event_id: id,
            session_id: self.call.grant.session_id,
            source_session_version: self.call.grant.expected_session_version,
            source: AdaptiveContinuationSourceV1::BudgetWindowExhausted {
                active_allowance_digest: budget.active_allowance_digest.clone(),
                continuation_history_digest: budget.continuation_history_digest.clone(),
            },
            abandoned_model_effect: None, provider_allowance_id: allowance.allowance_id.clone(),
            provider_authority_digest: sentinel_workflow::adaptive_leadership_continuation_provider_authority_digest(
                &allowance, &self.call.grant.assignee_authority,
            ).unwrap(),
            issued_at_ms: adoption.issued_at_unix_ms, deadline_ms: adoption.continuation_deadline_ms,
            additional_model_calls: *additional_model_calls, local_adoption: Some(Box::new(adoption.clone())),
            resume_policy: None, work_funding: self.call.grant.work_funding.clone(),
        };
        CompleteAdaptiveLeadershipReviewCallV1 {
            review_id: self.call.grant.review_id, allowance_id: self.call.allowance_id.clone(),
            request_digest: adoption.request.request_digest.clone(), model_response_digest: response,
            decision, resolution_event_id: Some(id), continuation: Some(authorization),
        }
    }
}

#[test]
fn funded_failed5_saved_continue_drives_candidate_audit_domain_and_disposition_once_without_provider_io() {
    let mut f = FundedFixture::standard();
    let provider = f.provider_bindings();
    let journal_count = f.journal_count();
    let original_row = f.row();
    let usage = f.api.event_store.as_ref().unwrap().event_by_operation_id(
        &format!("llm_usage_{}", f.call.request_id()),
    ).unwrap().unwrap();
    let before = f.state();
    let request = f.candidate().unwrap();
    assert_eq!(f.state(), before);
    assert!(request.is_funded());
    assert!(request.epoch_digest.is_none());
    assert_eq!(request.funded_binding, Some(AdaptiveLeadershipLocalAdoptionFundingBindingV2::from_epoch(
        f.call.grant.work_funding.as_ref().unwrap(),
    ).unwrap()));
    assert_eq!(serde_json::to_value(&request.decision).unwrap(),
        serde_json::from_str::<serde_json::Value>(&f.completion.content).unwrap());
    let adoption = f.authorize();
    assert!(f.reconcile().unwrap());
    let completed = f.current_call();
    let authorization = completed.continuation.as_ref().unwrap();
    assert_eq!(authorization.local_adoption.as_deref(), Some(&adoption));
    assert_eq!(authorization.issued_at_ms, adoption.issued_at_unix_ms);
    assert_eq!(authorization.deadline_ms, adoption.continuation_deadline_ms);
    assert_eq!(authorization.additional_model_calls, 8);
    assert_eq!(authorization.deadline_ms - authorization.issued_at_ms, 300_000);
    let current = f.api.store.adaptive_session(f.call.grant.session_id, &f.call.grant.assignee_authority)
        .unwrap().unwrap();
    assert_eq!(current.version, f.call.context.source_session.version + 1);
    assert_eq!(f.journal_count(), journal_count + 1);
    assert_eq!(current.grant, f.call.context.source_session.grant);
    assert_eq!((current.model_calls, current.tool_calls),
        (f.call.context.source_session.model_calls, f.call.context.source_session.tool_calls));
    assert_eq!(current.continuation.as_ref().unwrap().authorizations.last(), Some(authorization));
    assert_eq!(f.provider_bindings(), provider);
    let row = f.row();
    assert_eq!(row.payload, original_row.payload);
    assert_eq!(row.status, "action_claimed");
    assert_eq!(row.attempt_count, 5);
    assert_eq!(row.last_error.as_deref(), Some(FUNDED_LOCAL_ADOPTION_ERROR));
    let events = f.api.event_store.as_ref().unwrap();
    assert_eq!(serde_json::to_value(events.event_by_operation_id(&usage.operation_id).unwrap().unwrap()).unwrap(),
        serde_json::to_value(&usage).unwrap());
    let audit = events.event_v2_by_id(&authorization.resolution_event_id.to_string()).unwrap().unwrap();
    audit.validate_seals().unwrap();
    let audit_payload: serde_json::Value = serde_json::from_slice(&audit.payload).unwrap();
    assert_eq!(audit_payload["schema_version"], 3);
    let cleanup = events.event_by_operation_id(&format!("llm_local_adoption_{}", f.call.request_id()))
        .unwrap().unwrap();
    assert_eq!(cleanup.schema_version, 2);
    let all = events.get_all_events().unwrap();
    assert_eq!(all.iter().filter(|event| event.operation_id == usage.operation_id).count(), 1);
    assert_eq!(all.iter().filter(|event| event.operation_id == cleanup.operation_id).count(), 1);
    let connection = Connection::open(f.temp.path().join("company.sqlite")).unwrap();
    let authorizations: i64 = connection.query_row(
        "SELECT count(*) FROM company_events
         WHERE event_type='adaptive_leadership_local_adoption_authorized' AND operation_id=?1",
        [adoption.request.operation_id.to_string()], |row| row.get(0),
    ).unwrap();
    assert_eq!(authorizations, 1);
    let before_replay = f.state();
    f.reopen();
    assert!(!f.reconcile_at(adoption.continuation_deadline_ms + 1).unwrap());
    let response = f.submit(&adoption.request);
    assert_eq!(response.status, 200);
    assert_eq!(f.state(), before_replay);
    assert_eq!(f.provider_bindings(), provider);
}

#[test]
fn funded_local_execution_rechecks_issuer_and_exact_repair_before_first_audit_without_writes() {
    for mutation in [
        "issuer-missing", "issuer-principal", "issuer-authority", "proof-missing", "proof-changed",
    ] {
        let mut f = FundedFixture::standard();
        let adoption = f.authorize();
        let audit_id = f.proposal(&adoption).resolution_event_id.unwrap().to_string();
        let retained = f.row();
        let providers = f.provider_bindings();
        let journal = f.journal_count();
        f.change_execution_admission(mutation);
        let before = f.state();
        let expected = match mutation {
            "issuer-missing" => "funded local adoption issuer missing",
            "issuer-principal" | "issuer-authority" => "funded local adoption issuer authority changed",
            "proof-missing" => "funded local adoption repair unavailable",
            _ => "funded local adoption installed repair changed",
        };
        assert_eq!(f.reconcile(), Err(expected), "{mutation}");
        assert_eq!(f.state(), before, "{mutation}");
        assert_eq!(f.current_call(), f.call);
        assert_eq!(f.row(), retained);
        assert_eq!(f.provider_bindings(), providers);
        assert_eq!(f.journal_count(), journal);
        let events = f.api.event_store.as_ref().unwrap();
        assert!(events.event_v2_by_id(&audit_id).unwrap().is_none());
        assert!(events.event_by_operation_id(&format!("llm_local_adoption_{}", f.call.request_id()))
            .unwrap().is_none());
        assert!(events.event_by_operation_id(&format!("llm_local_adoption_retired_{}", f.call.request_id()))
            .unwrap().is_none());
    }
}

#[test]
fn funded_local_committed_replay_precedes_issuer_and_repair_freshness_after_restart() {
    for mutation in [
        "issuer-missing", "issuer-principal", "issuer-authority", "proof-missing", "proof-changed",
    ] {
        let mut f = FundedFixture::standard();
        let adoption = f.authorize();
        assert!(f.reconcile().unwrap());
        let completed = f.current_call();
        let disposed = f.row();
        let providers = f.provider_bindings();
        f.change_execution_admission(mutation);
        f.reopen();
        // Fresh execution would fail. Exact committed replay must not re-enter that gate.
        assert!(f.api.verify_recovery_review_release(&f.call).is_err(), "{mutation}");
        let before = f.state();
        f.api.accept_leadership_review_with_clock(
            &f.completion, &f.context, &adoption.request.request_id,
            &adoption.request.request_digest, now_unix_ms,
        ).unwrap();
        assert!(!f.reconcile().unwrap());
        assert_eq!(f.current_call(), completed);
        assert_eq!(f.row(), disposed);
        assert_eq!(f.provider_bindings(), providers);
        assert_eq!(f.state(), before, "{mutation}");
    }
}

#[test]
fn funded_local_candidate_rejects_error_attempt_usage_response_and_context_tampering_without_writes() {
    for mutation in ["error", "attempt", "digest", "owner", "actions", "content", "context", "usage"] {
        let f = FundedFixture::standard();
        f.candidate().unwrap();
        let connection = Connection::open(f.temp.path().join("events.sqlite")).unwrap();
        let id = f.call.request_id();
        match mutation {
            "error" => { connection.execute("UPDATE llm_completion_outbox SET last_error='continuation audit invalid' WHERE request_id=?1", [&id]).unwrap(); }
            "attempt" => { connection.execute("UPDATE llm_completion_outbox SET attempt_count=4 WHERE request_id=?1", [&id]).unwrap(); }
            "digest" => { connection.execute("UPDATE llm_completion_outbox SET request_digest=?2 WHERE request_id=?1", params![id, "d".repeat(64)]).unwrap(); }
            "owner" => { connection.execute("UPDATE llm_completion_outbox SET owner_scope='world' WHERE request_id=?1", [&id]).unwrap(); }
            "usage" => { connection.execute("UPDATE events SET timestamp_ms=timestamp_ms+1 WHERE operation_id=?1", [format!("llm_usage_{id}")]).unwrap(); }
            _ => {
                let mut payload: serde_json::Value = serde_json::from_str(&f.row().payload).unwrap();
                match mutation {
                    "actions" => payload["actions"] = serde_json::json!([{"kind":"invented"}]),
                    "content" => payload["model_work"]["content"] = serde_json::json!("changed"),
                    _ => payload["model_work"]["context"]["context_digest"] = serde_json::json!("d".repeat(64)),
                }
                connection.execute("UPDATE llm_completion_outbox SET payload=?2 WHERE request_id=?1",
                    params![id, payload.to_string()]).unwrap();
            }
        }
        let before = f.state();
        assert!(f.candidate().is_err(), "mutation={mutation}");
        assert_eq!(f.state(), before);
    }
}

#[test]
fn funded_local_candidate_requires_durable_original_funding_and_usage() {
    for funding in [false, true] {
        let f = FundedFixture::standard();
        f.candidate().unwrap();
        if funding {
            let epoch = f.call.grant.work_funding.as_ref().unwrap();
            assert_eq!(Connection::open(f.temp.path().join("company.sqlite")).unwrap().execute(
                "DELETE FROM company_events WHERE event_type='adaptive_work_funding_issued' AND operation_id=?1",
                [epoch.receipt.request.operation_id.to_string()],
            ).unwrap(), 1);
        } else {
            assert_eq!(Connection::open(f.temp.path().join("events.sqlite")).unwrap().execute(
                "DELETE FROM events WHERE operation_id=?1",
                [format!("llm_usage_{}", f.call.request_id())],
            ).unwrap(), 1);
        }
        let before = f.state();
        assert!(f.candidate().is_err(), "missing durable funding={funding}");
        assert_eq!(f.state(), before);
    }
}

#[test]
fn funded_local_candidate_preserves_genuine_counts_full_window_and_funding_expiry_caps() {
    for (calls, window, cap, lifetime) in [
        (13, 300_000, 300_000, 900_000),
        (8, 300_001, 300_000, 900_000),
        (8, 300_000, 180_000, 900_000),
        (8, 300_000, 300_000, 200_000),
    ] {
        let f = FundedFixture::new(calls, window, cap, lifetime);
        let before = f.state();
        assert!(f.candidate().is_err(), "calls={calls} window={window} cap={cap} lifetime={lifetime}");
        assert_eq!(f.state(), before);
        assert_eq!(f.row().status, "failed");
        assert_eq!(f.row().attempt_count, 5);
    }
}

#[test]
fn funded_local_post_rejects_operator_decision_binding_and_source_tampering() {
    let f = FundedFixture::standard();
    let request = f.candidate().unwrap();
    for pointer in [
        "/funded_binding/funding_id", "/funded_binding/receipt_digest", "/funded_binding/binding_digest",
        "/original_call_digest", "/project_digest", "/session_digest", "/session_head_digest",
        "/payload_digest", "/model_response_digest", "/usage_event_digest", "/repair_digest",
        "/decision/decision/additional_model_calls", "/decision/decision/window_ms",
    ] {
        let mut value = serde_json::to_value(&request).unwrap();
        *value.pointer_mut(pointer).unwrap() = if pointer.ends_with("additional_model_calls") {
            serde_json::json!(7)
        } else if pointer.ends_with("window_ms") { serde_json::json!(180_000) }
        else { serde_json::json!("d".repeat(64)) };
        let changed: AdaptiveLeadershipLocalAdoptionRequestV1 = serde_json::from_value(value).unwrap();
        let before = f.state();
        assert_ne!(f.submit(&changed).status, 200, "{pointer}");
        assert_eq!(f.state(), before);
    }
}

#[test]
fn funded_local_candidate_rechecks_current_project_leader_assignee_and_operator() {
    for mutation in ["project", "leader", "assignee", "operator"] {
        let mut f = FundedFixture::standard();
        let operator = f.api.principals.principal("operator").unwrap();
        f.candidate().unwrap();
        match mutation {
            "project" => {
                f.api.store.apply_company_command(
                    &f.call.grant.leadership_principal, Uuid::new_v4(),
                    &CompanyWorkflowCommandV1::RecordDecision {
                        project_id: f.call.grant.project_id.clone(),
                        expected_version: f.call.context.source_project.version,
                        work_item_id: None, choice_ref: "Changed source".into(),
                        rationale_ref: "Retained local adoption must not cross source changes".into(),
                    }, now_unix_ms(),
                ).unwrap();
            }
            "assignee" => change_review_assignee(&mut f.api, &f.context, "principal"),
            _ => {
                let mut principals = PrincipalAuthenticator {
                    by_credential_digest: f.api.principals.by_credential_digest.clone(),
                    by_principal_id: f.api.principals.by_principal_id.clone(),
                };
                let name = if mutation == "leader" { "pm" } else { "operator" };
                principals.by_principal_id.get_mut(name).unwrap().principal.authority_generation += 1;
                f.api.principals = Arc::new(principals);
                Arc::make_mut(f.api.authority.as_mut().unwrap()).principals = Arc::clone(&f.api.principals);
            }
        }
        let before = f.state();
        assert!(f.api.local_adoption_candidate(&operator, f.call.grant.review_id, Uuid::new_v4(), None).is_err(),
            "mutation={mutation}");
        assert_eq!(f.state(), before);
    }
}

#[test]
fn funded_local_preaudit_rejects_nondurable_or_forged_authority_saved_decision_and_fixed_times() {
    let f = FundedFixture::standard();
    let request = f.candidate().unwrap();
    let operator = f.api.principals.principal("operator").unwrap();
    let issued = now_unix_ms();
    let forged = AdaptiveLeadershipLocalAdoptionV1 {
        adoption_key: request.key().unwrap(), request,
        issuer_principal: operator.principal.clone(), issuer_authority: operator.execution_authority.clone(),
        issued_at_unix_ms: issued, continuation_deadline_ms: issued + 300_000,
    };
    forged.validate_funded_against(&f.call).unwrap();
    let before = f.state();
    assert_eq!(f.api.validate_funded_continuation_before_audit(&f.call, &f.proposal(&forged)),
        Err("work funding local adoption not durable"));
    assert_eq!(f.state(), before);
    let durable = f.authorize();
    f.api.validate_funded_continuation_before_audit(&f.call, &f.proposal(&durable)).unwrap();
    for mutation in ["issuer", "binding", "decision", "response", "issued", "deadline"] {
        let mut proposal = f.proposal(&durable);
        let authorization = proposal.continuation.as_mut().unwrap();
        match mutation {
            "issuer" => authorization.local_adoption.as_mut().unwrap().issuer_authority.principal_generation += 1,
            "binding" => authorization.local_adoption.as_mut().unwrap().request.funded_binding.as_mut().unwrap().binding_digest = "d".repeat(64),
            "decision" => if let AdaptiveLeadershipReviewDecisionKindV1::Continue { rationale, .. }
                = &mut proposal.decision.decision { rationale.push_str(" changed"); },
            "response" => proposal.model_response_digest = "d".repeat(64),
            "issued" => authorization.issued_at_ms += 1,
            _ => authorization.deadline_ms += 1,
        }
        let before = f.state();
        assert!(f.api.validate_funded_continuation_before_audit(&f.call, &proposal).is_err(), "{mutation}");
        assert_eq!(f.state(), before);
    }
}

#[test]
fn funded_local_domain_commit_crash_and_retirement_crash_reconcile_exact_disposition_after_restart() {
    for retire in [false, true] {
        let mut f = FundedFixture::standard();
        let adoption = f.authorize();
        let original = f.row();
        let provider = f.provider_bindings();
        if retire {
            f.api.store.retire_expired_adaptive_local_adoption(
                &f.call.grant.leadership_principal, f.call.grant.review_id, f.call.version,
                adoption.request.expires_at_unix_ms + 1,
            ).unwrap();
        } else {
            // Commit the real audit/domain path, then stop before Limbo disposition.
            f.api.accept_leadership_review_with_clock(&f.completion, &f.context,
                &adoption.request.request_id, &adoption.request.request_digest,
                now_unix_ms).unwrap();
        }
        assert_eq!(f.row(), original);
        let committed = f.current_call();
        f.reopen();
        assert!(!f.reconcile_at(adoption.request.expires_at_unix_ms + 1).unwrap());
        assert_eq!(f.current_call(), committed);
        let disposed = f.row();
        assert_eq!(disposed.payload, original.payload);
        assert_eq!(disposed.attempt_count, 5);
        assert_eq!(disposed.status, if retire { "failed" } else { "action_claimed" });
        assert_eq!(disposed.last_error.as_deref(), Some(if retire { "leadership_review_stale" } else { FUNDED_LOCAL_ADOPTION_ERROR }));
        let before = f.state();
        assert!(!f.reconcile_at(adoption.continuation_deadline_ms + 1).unwrap());
        assert_eq!(f.state(), before);
        assert_eq!(f.provider_bindings(), provider);
    }
}

#[test]
fn funded_local_reconciliation_rejects_changed_failure_and_cleanup_receipt_after_domain_commit() {
    for mutation in ["error", "attempt", "cleanup"] {
        let f = FundedFixture::standard();
        let adoption = f.authorize();
        f.api.accept_leadership_review_with_clock(
            &f.completion, &f.context, &adoption.request.request_id,
            &adoption.request.request_digest, now_unix_ms,
        ).unwrap();
        if mutation == "cleanup" {
            assert!(!f.reconcile().unwrap());
        }
        let connection = Connection::open(f.temp.path().join("events.sqlite")).unwrap();
        match mutation {
            "error" => {
                connection.execute(
                    "UPDATE llm_completion_outbox SET last_error='continuation audit invalid' WHERE request_id=?1",
                    [f.call.request_id()],
                ).unwrap();
            }
            "attempt" => {
                connection.execute(
                    "UPDATE llm_completion_outbox SET attempt_count=4 WHERE request_id=?1",
                    [f.call.request_id()],
                ).unwrap();
            }
            _ => {
                connection.execute(
                    "UPDATE events SET payload='{}' WHERE operation_id=?1",
                    [format!("llm_local_adoption_{}", f.call.request_id())],
                ).unwrap();
            }
        }
        let before = f.state();
        assert!(f.reconcile().is_err(), "{mutation}");
        assert_eq!(f.state(), before);
    }
}

#[test]
fn legacy_local_request_wire_golden_keeps_epoch_string_and_omits_funded_binding() {
    let f = LocalFixture::new(false);
    let request = &f.adoption.request;
    let wire = serde_json::to_value(request).unwrap();
    let mut expected = serde_json::json!({
        "schema_version":1,"operation_id":request.operation_id,"tenant_id":request.tenant_id,
        "project_id":request.project_id,"work_item_id":request.work_item_id,
        "session_id":request.session_id,"review_id":request.review_id,
        "epoch_digest":request.epoch_digest.as_deref().unwrap(),
        "original_call_digest":request.original_call_digest,"project_digest":request.project_digest,
        "session_digest":request.session_digest,"session_head_digest":request.session_head_digest,
        "request_id":request.request_id,"request_digest":request.request_digest,
        "context_digest":request.context_digest,"payload_digest":request.payload_digest,
        "model_response_digest":request.model_response_digest,"usage_event_digest":request.usage_event_digest,
        "original_completion_error":"continuation audit invalid","completion_attempts":request.completion_attempts,
        "release":request.release,"repair_digest":request.repair_digest,"decision":request.decision,
        "expires_at_unix_ms":request.expires_at_unix_ms,
    });
    assert_eq!(wire, expected);
    assert!(wire["epoch_digest"].is_string());
    assert!(wire.get("funded_binding").is_none());
    let roundtrip: AdaptiveLeadershipLocalAdoptionRequestV1 = serde_json::from_value(expected.clone()).unwrap();
    assert_eq!(&roundtrip, request);
    assert_eq!(roundtrip.canonical_digest().unwrap(), request.canonical_digest().unwrap());
    expected["schema_version"] = serde_json::json!(2);
    let mixed: AdaptiveLeadershipLocalAdoptionRequestV1 = serde_json::from_value(expected).unwrap();
    assert!(mixed.validate(&f.adoption.issuer_principal, f.adoption.issued_at_unix_ms).is_err());
}
