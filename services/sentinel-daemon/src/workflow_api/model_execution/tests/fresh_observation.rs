#![cfg(test)]

//! Store-backed fixtures only, not evidence of live provider execution.

use super::*;
use crate::workbench::{
    with_private_observation_store_for_test, WorkbenchCoordinator, WorkbenchInvocationStore,
};
use sentinel_workflow::{
    adaptive_tool_digest, AdaptiveEffectV1, AdaptiveObservationRefV1, AdaptiveSessionGrantV1,
    AdaptiveSessionV1, AdaptiveTransitionV1,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyAdaptiveModelContext {
    binding: AdaptiveProviderAuthority,
    task: sentinel_workflow::CompanyWorkItemSpecV1,
    accepted_customer_contract: super::super::super::model_work::AcceptedCustomerContract,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    artifact_inputs: Vec<super::super::super::model_work::ModelArtifactInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    correction: Option<super::super::super::model_work::ModelWorkCorrection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observation: Option<WorkbenchPrivateObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_catalog: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    schema_retry_feedback: Option<sentinel_workflow::AdaptiveRecoveryFeedbackV1>,
    agent_context: AdaptiveAgentContextV1,
}

fn assert_legacy_context_bytes(context: &AdaptiveModelContext) {
    assert!(!context.fresh_observation_required);
    let mut historical = context.clone();
    historical.working_memory = None;
    let context = &historical;
    let bytes = serde_json::to_vec(context).unwrap();
    let legacy: LegacyAdaptiveModelContext = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(serde_json::to_vec(&legacy).unwrap(), bytes);
    let restored: AdaptiveModelContext =
        serde_json::from_slice(&serde_json::to_vec(&legacy).unwrap()).unwrap();
    assert_eq!(&restored, context);
    assert_eq!(restored.prompt().unwrap(), context.prompt().unwrap());
}

#[test]
fn working_memory_is_private_numeric_history_and_preserves_fresh_inspection() {
    let fixture = Fixture::continued_unknown_with_observation();
    let context = fixture.context();
    let memory = context.working_memory.as_ref().unwrap();
    memory.validate(&context.binding).unwrap();
    assert!(context.fresh_observation_required);
    assert!(!memory.source.rows.is_empty());
    assert!(memory.outcomes.iter().all(|outcome| outcome.available));
    let value = serde_json::to_value(memory).unwrap();
    let encoded = serde_json::to_string(&value).unwrap();
    assert!(!encoded.contains("\"content\""));
    assert!(!encoded.contains("\"stdout\""));
    assert!(!encoded.contains("\"stderr\""));
    let prompt = context.prompt().unwrap();
    assert!(prompt.contains("Root model budget:"));
    assert!(prompt.contains("Actual continuation windows issued:"));
    assert!(prompt.contains("not a review ordinal"));
    let catalog = context.tool_catalog.as_ref().unwrap()["tools"]
        .as_array()
        .unwrap();
    assert!(catalog.iter().all(|tool| matches!(
        tool["tool"].as_str(),
        Some("list_directory" | "inspect_file")
    )));
    assert_eq!(fixture.read(), fixture.session);
}

#[test]
fn working_memory_reconstructs_exact_legacy_before_send_context_without_enrichment() {
    let fixture = Fixture::continued_unknown_with_observation();
    let mut historical = fixture.context();
    historical.working_memory = None;
    let envelope = ModelExecutionContext::Adaptive(Box::new(historical.clone()));
    let request_id = historical.binding.request_id();
    let request_digest = "c".repeat(64);
    let events = fixture.api.event_store.as_ref().unwrap();
    events
        .reserve_llm_request(
            &request_id,
            &request_digest,
            &historical.binding.grant.authority.agent_id.to_string(),
        )
        .unwrap();
    events
        .bind_llm_model_reservation(
            &crate::llm_bridge::bridge::model_reservation(&envelope, &request_id, &request_digest)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
    let before = events.get_llm_completion(&request_id).unwrap();
    let reconstructed = fixture.context();
    assert_eq!(reconstructed, historical);
    assert_eq!(
        reconstructed.prompt().unwrap(),
        historical.prompt().unwrap()
    );
    assert_eq!(events.get_llm_completion(&request_id).unwrap(), before);
    assert_eq!(fixture.read(), fixture.session);
}

#[test]
fn working_memory_rejects_foreign_binding_and_changed_private_observation() {
    let fixture = Fixture::continued_unknown_with_observation();
    let context = fixture.context();
    let mut memory = context.working_memory.clone().unwrap();
    memory.source.authority.agent_id = AgentId(63);
    assert!(memory.validate(&context.binding).is_err());
    let mut source = context.working_memory.unwrap().source;
    source
        .rows
        .last_mut()
        .unwrap()
        .observation
        .observation_digest = "e".repeat(64);
    source.last_observation = source.rows.last().map(|row| row.observation.clone());
    assert!(
        super::super::working_memory::compose(source, context.observation.as_ref(), |_| Err(
            "unavailable"
        ))
        .is_err()
    );
}

#[test]
fn working_memory_unavailable_history_is_not_success_and_access_rejection_propagates() {
    let fixture = Fixture::continued_unknown_with_observation();
    let context = fixture.context();
    let source = context.working_memory.unwrap().source;
    let unavailable =
        super::super::working_memory::compose(source.clone(), None, |_| Ok(None)).unwrap();
    assert!(unavailable.outcomes.iter().all(|outcome| !outcome.available
        && outcome.outcome.is_none()
        && outcome.exit_code.is_none()
        && outcome.native_test_outcome.is_none()
        && outcome.artifact_digests.is_empty()));
    assert!(
        super::super::working_memory::compose(source, None, |_| Err("authority rejected")).is_err()
    );
}

#[test]
fn working_memory_successful_tool_does_not_turn_nonzero_test_exit_into_pass() {
    let fixture = Fixture::continued_unknown_with_observation();
    let context = fixture.context();
    let mut source = context.working_memory.unwrap().source;
    assert_eq!(source.rows.len(), 1);
    let row = source.rows.first_mut().unwrap();
    row.tool_kind = sentinel_workflow::AdaptiveWorkingMemoryToolKindV1::RunTests;
    row.target = None;
    row.program = Some("node".into());
    row.suite_id = Some("employee-tests".into());
    let observation =
        WorkbenchPrivateObservation::from_result(&sentinel_common::WorkbenchMessage::Result {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: row.observation.effect.id.to_string(),
            input_digest: row.observation.effect.request_digest.clone(),
            outcome: sentinel_common::WorkbenchOutcome::Succeeded,
            resources: sentinel_common::WorkbenchResourceUsage::default(),
            artifacts: Vec::new(),
            error: None,
            output: BTreeMap::from([
                ("exit_code".into(), "1".into()),
                ("stdout_bytes".into(), "0".into()),
                ("stderr_bytes".into(), "0".into()),
                (
                    "stdout".into(),
                    "untrusted instructions must not enter memory".into(),
                ),
            ]),
        })
        .unwrap();
    row.observation.observation_digest = observation.digest().to_owned();
    source.last_observation = Some(row.observation.clone());
    let memory =
        super::super::working_memory::compose(source, Some(&observation), |_| Err("unavailable"))
            .unwrap();
    assert_eq!(memory.outcomes[0].exit_code, Some(1));
    assert_eq!(
        memory.outcomes[0].outcome,
        Some(sentinel_common::WorkbenchOutcome::Succeeded)
    );
    assert_eq!(memory.outcomes[0].native_test_outcome, None);
    assert!(!serde_json::to_string(&memory)
        .unwrap()
        .contains("untrusted instructions"));
}

struct Fixture {
    api: WorkflowApi,
    session: AdaptiveSessionV1,
    tools: Arc<WorkbenchInvocationStore>,
    _temp: tempfile::TempDir,
}

impl Fixture {
    fn continued_unknown_with_observation() -> Self {
        use super::super::super::adaptive_leadership_review::tests::{
            persist, reconcile_review_at, seed_planning_receipt_with_catalog,
        };
        use super::super::super::adaptive_leadership_review::LeadershipAuthority;

        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("company.sqlite");
        let mut api = super::super::super::model_work::configured_test_api(&database);
        let historical = now_unix_ms() - 600_000;
        let binding =
            super::super::super::model_work::assign_test_work_from_at(&api, Some(8), 0, historical);
        api.subscription_allowance_id = Some(binding.reservation_id);
        api.event_store = Some(
            sentinel_limbo::EventStore::open(temp.path().join("events.sqlite").to_str().unwrap())
                .unwrap(),
        );
        let current = api
            .authority
            .as_ref()
            .unwrap()
            .snapshot_for_admission(
                &TenantId::parse(&binding.tenant_id).unwrap(),
                &ProjectId::parse(&binding.project_id).unwrap(),
                &WorkItemId::parse(&binding.work_item_id).unwrap(),
                binding.agent_id,
                false,
            )
            .unwrap();
        let project = api
            .store
            .company_project(&current.tenant_id, &current.project_id)
            .unwrap()
            .unwrap();
        let allowance = project.subscription_call.as_ref().unwrap();
        let grant = AdaptiveSessionGrantV1 {
            schema_version: 1,
            session_id: Uuid::new_v4(),
            provider_allowance_id: allowance.allowance_id.clone(),
            provider_authority_digest: sentinel_workflow::adaptive_continuation_provider_digest(
                allowance, &current,
            )
            .unwrap(),
            authority: current.clone(),
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
        let session = api
            .store
            .begin_adaptive_session(&grant, &current, historical)
            .unwrap()
            .1;
        seed_planning_receipt_with_catalog(&api, &database, &project, &grant.catalog_digest);
        api.workbench = Some(Arc::new(WorkbenchExecutionAdapter {
            store: Arc::clone(&api.store),
            authority: Arc::clone(api.authority.as_ref().unwrap()),
        }));
        let tools =
            Arc::new(WorkbenchInvocationStore::open(temp.path().join("workbench.redb")).unwrap());
        let mut fixture = Self {
            api,
            session,
            tools,
            _temp: temp,
        };
        fixture.inspect_and_observe(historical + 1, "historical observation");
        let old_observation = fixture.session.last_observation.clone();
        let prior_version = fixture.session.version;
        let effect = fixture.claim_model(historical + 2);
        let assignment_id = project.work_items[&current.work_item_id]
            .assignments
            .iter()
            .find(|assignment| assignment.active)
            .unwrap()
            .assignment_id
            .clone();
        let authority = ProviderExecutionAuthority::Adaptive(Box::new(AdaptiveProviderAuthority {
            schema_version: 3,
            grant: grant.clone(),
            session_version: prior_version,
            effect_id: effect.id,
            assignment_id: assignment_id.clone(),
            previous_observation: old_observation.clone(),
        }));
        let request_id = authority.request_id();
        let events = fixture.api.event_store.as_ref().unwrap();
        events
            .reserve_llm_request(
                &request_id,
                &effect.request_digest,
                &current.agent_id.to_string(),
            )
            .unwrap();
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
                    session_version: prior_version,
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
                    assignment_id,
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
        fixture.advance(AdaptiveTransitionV1::MarkUnknown { effect }, historical + 2);
        let before = fixture.session.clone();
        assert!(reconcile_review_at(&fixture.api, &project, now_unix_ms()));
        let calls = fixture
            .api
            .store
            .adaptive_leadership_review_calls(&current.tenant_id, grant.session_id)
            .unwrap();
        assert_eq!(calls.len(), 1);
        assert!(matches!(
            calls[0].grant.subject,
            Some(sentinel_workflow::AdaptiveLeadershipReviewSubjectV2::UnknownModel { .. })
        ));
        let review_binding = LeadershipAuthority::from_call(&calls[0]);
        let context = fixture.with_observations(|| {
            fixture
                .api
                .prepare_leadership_review(&review_binding)
                .unwrap()
        });
        let (id, digest) =
            super::super::super::adaptive_continuation_tests::reserve_and_claim_schema2(
                &fixture.api,
                &context,
            );
        let completion = ModelExecutionCompletion {
            context: ModelExecutionContext::AdaptiveLeadershipReview(Box::new(context.clone())),
            content: serde_json::json!({
                "schema_version": 2,
                "decision": {
                    "kind": "continue",
                    "additional_model_calls": 3,
                    "window_ms": 300_000,
                    "rationale": "Inspect fresh state after the sealed unknown effect.",
                    "evidence_refs": context.source.evidence_refs,
                },
            })
            .to_string(),
            admissible: true,
        };
        persist(&fixture.api, &completion, &context, &id, &digest, true);
        fixture
            .api
            .accept_leadership_review(&completion, &context, &id, &digest)
            .unwrap();
        fixture.session = fixture.read();
        assert_eq!(fixture.session.grant, before.grant);
        assert_eq!(fixture.session.model_calls, before.model_calls);
        assert_eq!(fixture.session.tool_calls, before.tool_calls);
        assert_eq!(fixture.session.last_observation, old_observation);
        assert!(fixture.session.requires_fresh_observation());
        fixture
    }

    fn read(&self) -> AdaptiveSessionV1 {
        self.api
            .store
            .adaptive_session(self.session.grant.session_id, &self.session.grant.authority)
            .unwrap()
            .unwrap()
    }

    fn advance(&mut self, command: AdaptiveTransitionV1, now: u64) {
        self.session = self
            .api
            .store
            .advance_adaptive_session(
                self.session.grant.session_id,
                self.session.version,
                Uuid::new_v4(),
                &command,
                &self.session.grant.authority,
                now,
            )
            .unwrap()
            .1;
    }

    fn claim_model(&mut self, now: u64) -> AdaptiveEffectV1 {
        let effect = AdaptiveEffectV1 {
            id: Uuid::new_v4(),
            request_digest: "a".repeat(64),
        };
        self.advance(
            AdaptiveTransitionV1::ClaimModel {
                effect: effect.clone(),
                previous_observation_digest: self
                    .session
                    .last_observation
                    .as_ref()
                    .map(|observation| observation.observation_digest.clone()),
            },
            now,
        );
        effect
    }

    fn inspect_and_observe(&mut self, now: u64, content: &str) {
        let effect = self.claim_model(now);
        let tool = WorkbenchTool::InspectFile {
            path: "src/main.rs".into(),
            max_bytes: 1024,
        };
        let tool_digest = adaptive_tool_digest(&tool).unwrap();
        self.advance(
            AdaptiveTransitionV1::ResolveModel {
                effect,
                result_digest: "b".repeat(64),
                decision: AdaptiveModelDecisionV1::Tool {
                    tool: tool.clone(),
                    tool_digest: tool_digest.clone(),
                },
            },
            now,
        );
        let adapter = self.api.workbench.as_ref().unwrap();
        let effect = adapter.adaptive_tool_effect(&self.session, &tool).unwrap();
        let request = adapter
            .build_adaptive_request(&self.session, &effect, &tool)
            .unwrap();
        self.advance(
            AdaptiveTransitionV1::ClaimTool {
                effect: effect.clone(),
                tool_digest,
            },
            now,
        );
        self.tools.reserve(&request, now).unwrap();
        self.tools
            .mark_executing(&request.invocation_id, &request.input_digest, now)
            .unwrap();
        self.tools
            .accept_result(
                &sentinel_common::WorkbenchMessage::Result {
                    schema_version: WORKBENCH_SCHEMA_VERSION,
                    invocation_id: request.invocation_id.clone(),
                    input_digest: request.input_digest.clone(),
                    outcome: sentinel_common::WorkbenchOutcome::Succeeded,
                    resources: sentinel_common::WorkbenchResourceUsage::default(),
                    artifacts: Vec::new(),
                    error: None,
                    output: BTreeMap::from([("content".into(), content.into())]),
                },
                now,
            )
            .unwrap();
        let authority = self.api.authority.as_ref().unwrap();
        let (profile, digest) = authority
            .profile_for_binding(&self.session.grant.authority.profile_id)
            .unwrap();
        let observation = WorkbenchCoordinator::new(&self.tools, profile, digest)
            .private_observation(&request.invocation_id, authority.as_ref())
            .unwrap()
            .unwrap();
        self.advance(
            AdaptiveTransitionV1::ObserveTool {
                observation: AdaptiveObservationRefV1 {
                    effect,
                    observation_digest: observation.digest().to_owned(),
                },
            },
            now,
        );
    }

    fn with_observations<R>(&self, run: impl FnOnce() -> R) -> R {
        let (profile, digest) = self
            .api
            .authority
            .as_ref()
            .unwrap()
            .profile_for_binding(&self.session.grant.authority.profile_id)
            .unwrap();
        with_private_observation_store_for_test(
            Arc::clone(&self.tools),
            profile.clone(),
            digest.to_owned(),
            run,
        )
    }

    fn context(&self) -> AdaptiveModelContext {
        let binding = self
            .api
            .adaptive_provider_authority_for_claim(self.session.grant.authority.agent_id)
            .unwrap()
            .unwrap();
        self.with_observations(|| self.api.prepare_adaptive_model(&binding).unwrap())
    }

    fn retain_old_preinspection_write(&mut self) -> sentinel_limbo::LlmCompletionEntry {
        self.retain_old_preinspection_write_with_digest(false)
    }

    fn retain_old_preinspection_write_with_digest(
        &mut self,
        include_digest: bool,
    ) -> sentinel_limbo::LlmCompletionEntry {
        // Reproduce the former generation contract, not a live model response.
        let mut context = self.context();
        context.working_memory = None;
        context.fresh_observation_required = false;
        let (profile, _) = self
            .api
            .authority
            .as_ref()
            .unwrap()
            .profile_for_binding(&context.binding.grant.authority.profile_id)
            .unwrap();
        context.tool_catalog = Some(
            super::super::tool_catalog::adaptive_tool_catalog(
                profile,
                &context.binding.grant.authority,
                &context.task,
            )
            .unwrap(),
        );
        let binding = context.binding.clone();
        let request_id = binding.request_id();
        let digest = "c".repeat(64);
        self.advance(
            AdaptiveTransitionV1::ClaimModel {
                effect: AdaptiveEffectV1 {
                    id: binding.effect_id,
                    request_digest: digest.clone(),
                },
                previous_observation_digest: self
                    .session
                    .last_observation
                    .as_ref()
                    .map(|observation| observation.observation_digest.clone()),
            },
            now_unix_ms(),
        );
        let completion = ModelExecutionCompletion {
            context: ModelExecutionContext::Adaptive(Box::new(context)),
            content: serde_json::json!({"schema_version": 1, "decision": {
                "kind": "tool", "tool": {"tool": "write_file", "path": "main.py",
                    "content": "print('fixture only')\n"}
            }})
            .to_string(),
            admissible: true,
        };
        let events = self.api.event_store.as_ref().unwrap();
        events
            .reserve_llm_request(
                &request_id,
                &digest,
                &binding.grant.authority.agent_id.to_string(),
            )
            .unwrap();
        events
            .bind_llm_model_reservation(
                &crate::llm_bridge::bridge::model_reservation(
                    &completion.context,
                    &request_id,
                    &digest,
                )
                .unwrap()
                .unwrap(),
            )
            .unwrap();
        crate::llm_bridge::bridge::retain_adaptive_gateway_completion_for_test(
            events,
            &self.api,
            &completion,
            &request_id,
            &digest,
        )
        .unwrap();
        let produced = events.get_llm_completion(&request_id).unwrap().unwrap();
        assert_eq!(produced.attempt_count, 1);
        assert_eq!(
            produced.last_error.as_deref(),
            Some("adaptive model result admission failed")
        );
        let mut payload: serde_json::Value = serde_json::from_str(&produced.payload).unwrap();
        assert!(payload.get("model_response_digest").is_none());
        assert_eq!(payload["tokens_used"], 18);
        let usage = events
            .event_by_operation_id(&format!("llm_usage_{request_id}"))
            .unwrap()
            .unwrap();
        completion.validate_usage(&usage).unwrap();
        if include_digest {
            payload["model_response_digest"] =
                serde_json::json!(hex_sha256(completion.content.as_bytes()));
            sentinel_limbo::rusqlite::Connection::open(self._temp.path().join("events.sqlite"))
                .unwrap()
                .execute(
                    "UPDATE llm_completion_outbox SET payload=?1 WHERE request_id=?2",
                    sentinel_limbo::rusqlite::params![payload.to_string(), request_id],
                )
                .unwrap();
        }
        let ModelExecutionContext::Adaptive(context) = &completion.context else {
            unreachable!()
        };
        assert!(self
            .api
            .accept_adaptive_model(&completion, context, &request_id, &digest)
            .is_err());
        for _ in 1..5 {
            events
                .record_llm_completion_failure(
                    &request_id,
                    &digest,
                    "adaptive model result admission failed",
                    5,
                )
                .unwrap();
        }
        events.get_llm_completion(&request_id).unwrap().unwrap()
    }
}

#[test]
fn known_rejected_write_disposition_preserves_response_usage_grant_and_requires_fresh_inspection() {
    for receipt_before_recovery in [false, true] {
        let mut fixture = Fixture::continued_unknown_with_observation();
        let entry = fixture.retain_old_preinspection_write();
        assert!(serde_json::from_str::<serde_json::Value>(&entry.payload)
            .unwrap()
            .get("model_response_digest")
            .is_none());
        let before = fixture.read();
        let events = fixture.api.event_store.as_ref().unwrap();
        let usage = events
            .event_by_operation_id(&format!("llm_usage_{}", entry.request_id))
            .unwrap()
            .unwrap();
        if receipt_before_recovery {
            let verified = fixture
                .api
                .verified_known_rejection(&before, &entry)
                .unwrap();
            events
                .record_retained_model_decision_rejection(&verified.evidence)
                .unwrap();
        }
        let project = fixture
            .api
            .store
            .company_project(
                &before.grant.authority.tenant_id,
                &before.grant.authority.project_id,
            )
            .unwrap()
            .unwrap();
        assert!(fixture
            .api
            .recover_known_rejected_adaptive_model(&project)
            .unwrap());
        let after = fixture.read();
        let mut expected = before.clone();
        expected.version += 2;
        expected.updated_at_ms = after.updated_at_ms;
        expected.cursor = AdaptiveCursorV1::ReadyForModel;
        assert_eq!(after, expected);
        assert!(after.requires_fresh_observation());
        let retained = events
            .get_llm_completion(&entry.request_id)
            .unwrap()
            .unwrap();
        assert_eq!(retained.request_id, entry.request_id);
        assert_eq!(retained.request_digest, entry.request_digest);
        assert_eq!(retained.owner_scope, entry.owner_scope);
        assert_eq!(retained.payload, entry.payload);
        assert_eq!(retained.status, entry.status);
        assert_eq!(retained.attempt_count, entry.attempt_count);
        assert_eq!(retained.last_error, entry.last_error);
        assert_eq!(retained.created_at, entry.created_at);
        assert_eq!(retained.updated_at, entry.updated_at);
        assert_eq!(
            serde_json::to_value(
                events
                    .event_by_operation_id(&format!("llm_usage_{}", entry.request_id))
                    .unwrap()
                    .unwrap()
            )
            .unwrap(),
            serde_json::to_value(usage).unwrap()
        );
        let resolution = events
            .event_by_operation_id(&format!("llm_resolution_{}", entry.request_id))
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&resolution.payload).unwrap()["resolution"],
            "model_decision_rejected"
        );
        assert!(!fixture
            .api
            .recover_known_rejected_adaptive_model(&project)
            .unwrap());
        assert_eq!(fixture.read(), after);
        fixture.session = after;
        let context = fixture.context();
        assert!(context.fresh_observation_required);
        assert!(context
            .prompt()
            .unwrap()
            .contains("Only list_directory or inspect_file"));
    }
}

#[test]
fn known_rejected_write_disposition_rejects_changed_raw_response_before_any_receipt_or_journal_write(
) {
    let mut fixture = Fixture::continued_unknown_with_observation();
    let entry = fixture.retain_old_preinspection_write_with_digest(true);
    let before = fixture.read();
    let events = fixture.api.event_store.as_ref().unwrap();
    sentinel_limbo::rusqlite::Connection::open(fixture._temp.path().join("events.sqlite")).unwrap()
        .execute("UPDATE llm_completion_outbox SET payload=replace(payload,'fixture only','changed') WHERE request_id=?1",
        sentinel_limbo::rusqlite::params![entry.request_id]).unwrap();
    let project = fixture
        .api
        .store
        .company_project(
            &before.grant.authority.tenant_id,
            &before.grant.authority.project_id,
        )
        .unwrap()
        .unwrap();
    assert!(fixture
        .api
        .recover_known_rejected_adaptive_model(&project)
        .is_err());
    assert_eq!(fixture.read(), before);
    assert!(events
        .event_by_operation_id(&format!("llm_resolution_{}", entry.request_id))
        .unwrap()
        .is_none());
}

#[test]
fn known_rejected_write_optional_digest_is_exact_or_rejected_without_effects() {
    for digest in [
        serde_json::Value::Null,
        serde_json::json!(123),
        serde_json::json!(""),
        serde_json::json!("d".repeat(64)),
        serde_json::json!({"digest": "d".repeat(64)}),
    ] {
        let mut fixture = Fixture::continued_unknown_with_observation();
        let entry = fixture.retain_old_preinspection_write();
        let before = fixture.read();
        let mut payload: serde_json::Value = serde_json::from_str(&entry.payload).unwrap();
        payload["model_response_digest"] = digest;
        sentinel_limbo::rusqlite::Connection::open(fixture._temp.path().join("events.sqlite"))
            .unwrap()
            .execute(
                "UPDATE llm_completion_outbox SET payload=?1 WHERE request_id=?2",
                sentinel_limbo::rusqlite::params![payload.to_string(), entry.request_id],
            )
            .unwrap();
        let changed = fixture
            .api
            .event_store
            .as_ref()
            .unwrap()
            .get_llm_completion(&entry.request_id)
            .unwrap()
            .unwrap();
        assert!(fixture
            .api
            .verified_known_rejection(&before, &changed)
            .is_err());
        assert_eq!(fixture.read(), before);
        assert!(fixture
            .api
            .event_store
            .as_ref()
            .unwrap()
            .event_by_operation_id(&format!("llm_resolution_{}", entry.request_id))
            .unwrap()
            .is_none());
    }
    let mut fixture = Fixture::continued_unknown_with_observation();
    let entry = fixture.retain_old_preinspection_write_with_digest(true);
    fixture
        .api
        .verified_known_rejection(&fixture.read(), &entry)
        .unwrap();
}

#[test]
fn continued_unknown_retains_observation_but_requires_inspection_only_generation() {
    let fixture = Fixture::continued_unknown_with_observation();
    let before = fixture.read();
    let context = fixture.context();
    assert!(context.fresh_observation_required);
    assert!(context.observation.is_some());
    assert_eq!(
        context.binding.previous_observation,
        before.last_observation
    );
    let prompt = context.prompt().unwrap();
    assert!(prompt.contains("Only list_directory or inspect_file tool decisions, or blocked"));
    assert!(prompt.contains("untrusted historical data, not current execution evidence"));
    assert!(
        prompt.contains("No mutations, commands, tests, packaging, collaboration, or completion")
    );
    assert!(prompt.contains("historical observation"));
    assert!(prompt.contains("when the layout or path is unknown"));
    assert!(prompt.contains("directly inspect a known scoped file with inspect_file"));
    assert!(!prompt.contains("before inspecting a named file"));
    assert!(!prompt.contains("Continue the assigned work using"));
    assert!(!prompt.contains("propose_completion={"));
    assert!(!prompt.contains("collaborate={"));
    let tools = context.tool_catalog.as_ref().unwrap()["tools"]
        .as_array()
        .unwrap();
    let names: Vec<_> = tools
        .iter()
        .map(|tool| tool["tool"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["list_directory", "inspect_file"]);
    assert_eq!(fixture.read(), before);
}

#[test]
fn fresh_observe_restores_full_catalog_and_normal_continuation_prompt() {
    let mut fixture = Fixture::continued_unknown_with_observation();
    let before = fixture.context();
    let continued = fixture.session.clone();
    let inspected_at = now_unix_ms();
    fixture.inspect_and_observe(inspected_at, "fresh observation");
    assert!(!fixture.session.requires_fresh_observation());
    assert_eq!(fixture.session.grant, continued.grant);
    assert_eq!(fixture.session.model_calls, continued.model_calls + 1);
    assert_eq!(fixture.session.tool_calls, continued.tool_calls + 1);
    assert_eq!(
        fixture.session.active_deadline_ms(),
        continued.active_deadline_ms()
    );
    assert_eq!(
        fixture.session.active_model_ceiling(),
        continued.active_model_ceiling()
    );
    let context = fixture.context();
    assert!(!context.fresh_observation_required);
    assert_legacy_context_bytes(&context);
    assert_ne!(context.observation, before.observation);
    assert_eq!(
        context.binding.previous_observation,
        fixture.session.last_observation
    );
    let memory = context.working_memory.as_ref().unwrap();
    assert!(!memory.source.rows.is_empty());
    assert!(memory.source.rows.iter().all(|row| {
        row.tool_kind == sentinel_workflow::AdaptiveWorkingMemoryToolKindV1::InspectFile
    }));
    let (profile, _) = fixture
        .api
        .authority
        .as_ref()
        .unwrap()
        .profile_for_binding(&context.binding.grant.authority.profile_id)
        .unwrap();
    let full_catalog = super::super::tool_catalog::adaptive_tool_catalog(
        profile,
        &context.binding.grant.authority,
        &context.task,
    )
    .unwrap();
    assert_eq!(context.tool_catalog.as_ref(), Some(&full_catalog));
    let prompt = context.prompt().unwrap();
    assert!(prompt.starts_with("Continue the assigned work using"));
    assert!(prompt.contains("fresh observation"));
    assert!(prompt.contains("when the layout or path is unknown"));
    assert!(prompt.contains("directly inspect a known scoped file with inspect_file"));
    assert!(prompt.contains(
        "Do not repeat discovery solely because another model call begins within the same inspected window."
    ));
    assert!(!prompt.contains("before inspecting a named file"));
    assert!(prompt.contains("write_file"));
    assert!(prompt.contains("propose_completion={"));
    assert!(prompt.contains("collaborate={"));

    let fresh_observation = fixture.session.last_observation.clone();
    let write_at = now_unix_ms().max(inspected_at + 1);
    assert_eq!(
        fixture.session.model_admission_at(write_at),
        sentinel_workflow::AdaptiveModelAdmissionV1::Admissible
    );
    let effect = fixture.claim_model(write_at);
    let tool = WorkbenchTool::WriteFile {
        path: "src/main.rs".into(),
        content: "// next same-window edit\n".into(),
        expected_sha256: None,
    };
    let tool_digest = adaptive_tool_digest(&tool).unwrap();
    fixture.advance(
        AdaptiveTransitionV1::ResolveModel {
            effect,
            result_digest: "b".repeat(64),
            decision: AdaptiveModelDecisionV1::Tool {
                tool: tool.clone(),
                tool_digest: tool_digest.clone(),
            },
        },
        write_at,
    );
    let adapter = fixture.api.workbench.as_ref().unwrap();
    let effect = adapter
        .adaptive_tool_effect(&fixture.session, &tool)
        .unwrap();
    adapter
        .build_adaptive_request(&fixture.session, &effect, &tool)
        .unwrap();
    fixture.advance(
        AdaptiveTransitionV1::ClaimTool {
            effect,
            tool_digest,
        },
        write_at,
    );
    assert!(matches!(
        &fixture.session.cursor,
        AdaptiveCursorV1::ToolPending {
            tool: WorkbenchTool::WriteFile { .. },
            ..
        }
    ));
    assert!(!fixture.session.requires_fresh_observation());
    assert_eq!(fixture.session.last_observation, fresh_observation);
    assert_eq!(fixture.session.grant, continued.grant);
    assert_eq!(fixture.session.model_calls, continued.model_calls + 2);
    assert_eq!(fixture.session.tool_calls, continued.tool_calls + 2);
    assert_eq!(
        fixture
            .session
            .continuation
            .as_ref()
            .unwrap()
            .authorizations,
        continued.continuation.as_ref().unwrap().authorizations
    );
    assert_eq!(fixture.read(), fixture.session);
}

#[test]
fn fresh_observation_flag_preserves_legacy_bytes_and_binds_true_in_context_digest() {
    let root = tempfile::tempdir().unwrap();
    let (api, binding, _) = super::super::super::model_work::configured_adaptive_test_api(
        &root.path().join("company.sqlite"),
        &root.path().join("events.sqlite"),
    );
    let context = api.prepare_adaptive_model(&binding).unwrap();
    assert!(!context.fresh_observation_required);
    assert_legacy_context_bytes(&context);
    let legacy_bytes = serde_json::to_vec(&context).unwrap();
    let legacy_value: serde_json::Value = serde_json::from_slice(&legacy_bytes).unwrap();
    assert!(legacy_value.get("fresh_observation_required").is_none());
    let restored: AdaptiveModelContext = serde_json::from_slice(&legacy_bytes).unwrap();
    assert_eq!(serde_json::to_vec(&restored).unwrap(), legacy_bytes);
    assert_eq!(restored.prompt().unwrap(), context.prompt().unwrap());
    let legacy_envelope = ModelExecutionContext::Adaptive(Box::new(restored.clone()));
    let legacy_digest = sentinel_common::sha256_hex(&serde_json::to_vec(&legacy_envelope).unwrap());
    let mut required = restored;
    required.fresh_observation_required = true;
    let required_value = serde_json::to_value(&required).unwrap();
    assert_eq!(required_value["fresh_observation_required"], true);
    assert_eq!(
        serde_json::from_value::<AdaptiveModelContext>(required_value).unwrap(),
        required
    );
    let required_envelope = ModelExecutionContext::Adaptive(Box::new(required));
    assert_ne!(
        sentinel_common::sha256_hex(&serde_json::to_vec(&required_envelope).unwrap()),
        legacy_digest
    );
}

#[test]
fn retained_observation_never_relaxes_store_rejection_of_noninspection_decisions() {
    let mut fixture = Fixture::continued_unknown_with_observation();
    let effect = fixture.claim_model(now_unix_ms());
    let before = fixture.read();
    let decisions = [
        r#"{"schema_version":1,"decision":{"kind":"tool","tool":{"tool":"write_file","path":"main.py","content":"print(1)\n"}}}"#.to_owned(),
        serde_json::json!({"schema_version":1,"decision":{
            "kind":"propose_completion","artifact_digest":"c".repeat(64)
        }})
        .to_string(),
        r#"{"schema_version":1,"decision":{"kind":"collaborate","action":{"kind":"ask_question","question_ref":"dependency"}}}"#.to_owned(),
    ];
    for content in decisions {
        let decision = parse_adaptive_decision(&content).unwrap();
        assert!(fixture
            .api
            .store
            .advance_adaptive_session(
                before.grant.session_id,
                before.version,
                Uuid::new_v4(),
                &AdaptiveTransitionV1::ResolveModel {
                    effect: effect.clone(),
                    result_digest: "e".repeat(64),
                    decision,
                },
                &before.grant.authority,
                now_unix_ms(),
            )
            .is_err());
        assert_eq!(fixture.read(), before);
    }
    fixture.advance(
        AdaptiveTransitionV1::ResolveModel {
            effect,
            result_digest: "e".repeat(64),
            decision: AdaptiveModelDecisionV1::Blocked {
                reason_code: "inspection_unavailable".into(),
            },
        },
        now_unix_ms(),
    );
    assert!(fixture.session.requires_fresh_observation());
    assert_eq!(fixture.session.model_calls, before.model_calls);
    assert_eq!(fixture.session.tool_calls, before.tool_calls);
}
