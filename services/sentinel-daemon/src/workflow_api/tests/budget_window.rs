//! Schema-3 regression fixtures, not evidence of live model-selected renewal.
//! All review grants are issued by WorkflowApi, never fabricated by this leaf.

use super::adaptive_continuation_tests::{fixture_schema2_blocked, reserve_and_claim_schema2};
use super::adaptive_leadership_review::tests::{
    persist, persist_discovery_project, reconcile_review_at, seed_planning_receipt_with_catalog,
};
use super::adaptive_leadership_review::{LeadershipAuthority, LeadershipContext};
use super::model_execution::{ModelExecutionCompletion, ModelExecutionContext};
use super::*;
use crate::workbench::{
    with_private_observation_store_for_test, WorkbenchCoordinator, WorkbenchInvocationStore,
};
use sentinel_common::{WorkbenchMessage, WorkbenchOutcome, WorkbenchResourceUsage};
use sentinel_workflow::{
    adaptive_budget_allowance_digest, adaptive_budget_history_digest, adaptive_tool_digest,
    AdaptiveBudgetWindowAuthorityV1, AdaptiveEffectV1, AdaptiveLeadershipReviewCallV1,
    AdaptiveLeadershipReviewDecisionKindV1, AdaptiveLeadershipReviewSubjectV2,
    AdaptiveModelDecisionV1, AdaptiveObservationRefV1, AdaptiveSessionGrantV1,
};
use std::collections::BTreeMap;

const PRIVATE_CONTENT: &str = "PRIVATE-BUDGET-WINDOW-OBSERVATION";

struct RetainedReceiptTool<'a> {
    fixture: &'a Fixture,
    reassign_after_read: bool,
    reads: Arc<std::sync::atomic::AtomicUsize>,
}

impl AdaptiveToolPort for RetainedReceiptTool<'_> {
    fn reconcile_tool(
        &self,
        session: &AdaptiveSessionV1,
        effect: &AdaptiveEffectV1,
        tool: &WorkbenchTool,
        tool_digest: &str,
    ) -> Result<AdaptiveToolObservationV1, WorkflowPortError> {
        assert_eq!(session, &self.fixture.session);
        assert_eq!(adaptive_tool_digest(tool).as_deref(), Ok(tool_digest));
        let observation = self.fixture.private_observation(&effect.id.to_string());
        observation
            .validate(&effect.id.to_string(), &effect.request_digest)
            .unwrap();
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.reassign_after_read {
            reassign_receipt_work(self.fixture, false);
        }
        Ok(AdaptiveToolObservationV1::Completed {
            observation_digest: observation.digest().to_owned(),
        })
    }
}

fn reassign_receipt_work(fixture: &Fixture, different_agent: bool) {
    let mut project = fixture.project();
    let work = project
        .work_items
        .get_mut(&fixture.session.grant.authority.work_item_id)
        .unwrap();
    let assignment = work.assignments.iter_mut().find(|a| a.active).unwrap();
    if different_agent {
        assignment.agent_id = AgentId(7);
    } else {
        assignment.organization_generation += 1;
    }
    persist_discovery_project(&fixture.temp.path().join("company.sqlite"), &project);
    if !different_agent {
        assert_eq!(fixture.project(), project);
    }
}

fn stop_receipt_agent(fixture: &Fixture) {
    let mut health = fixture
        .api
        .authority
        .as_ref()
        .unwrap()
        .runtime_health
        .write()
        .unwrap();
    let agent = health
        .agents
        .iter_mut()
        .find(|agent| agent.agent_id == fixture.session.grant.authority.agent_id.0)
        .unwrap();
    agent.adapter_health_state = Some(sentinel_common::NanoHealthState::Stopped);
}

#[test]
fn stopped_record_recovery_adopts_original_pending_and_unknown_receipts_without_reset() {
    for unknown in [false, true] {
        let mut fixture = Fixture::continued();
        let now = now_unix_ms();
        let request = fixture.claim_tool(now);
        let observation = fixture.retain_tool_result(&request, now, now, PRIVATE_CONTENT);
        let effect = AdaptiveEffectV1 {
            id: Uuid::parse_str(&request.invocation_id).unwrap(),
            request_digest: request.input_digest.clone(),
        };
        if unknown {
            fixture.advance(
                AdaptiveTransitionV1::MarkUnknown {
                    effect: effect.clone(),
                },
                now,
            );
        }
        let before = fixture.read();
        assert_eq!((before.model_calls, before.tool_calls), (2, 1));
        stop_receipt_agent(&fixture);
        let authority = fixture.api.authority.as_ref().unwrap();
        assert!(authority.current_for_request(&request).is_err());
        assert!(
            sentinel_workflow::OrganizationRuntimePort::authority_snapshot(
                authority.as_ref(),
                &before.grant.authority.tenant_id,
                &before.grant.authority.project_id,
                &before.grant.authority.work_item_id,
                before.grant.authority.agent_id,
            )
            .is_err()
        );
        let mut fresh = before.clone();
        fresh.cursor = AdaptiveCursorV1::ReadyForTool {
            tool: request.tool.clone(),
            tool_digest: adaptive_tool_digest(&request.tool).unwrap(),
        };
        assert!(fixture
            .api
            .workbench
            .as_ref()
            .unwrap()
            .build_adaptive_request(&fresh, &effect, &request.tool)
            .is_err());
        let recovered_at = now_unix_ms();
        let core = AdaptiveWorkflowCore::new(
            Arc::clone(&fixture.api.store),
            WorkbenchRecordRecoveryAuthority(authority.as_ref()),
            UnavailableAdaptiveModel,
            RetainedReceiptTool {
                fixture: &fixture,
                reassign_after_read: false,
                reads: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            },
        );
        let adopted = core
            .reconcile_tool(
                before.grant.session_id,
                &before.grant.authority,
                Uuid::new_v4(),
                recovered_at,
            )
            .unwrap();
        let mut expected = before;
        expected.cursor = AdaptiveCursorV1::ReadyForModel;
        expected.version += 1;
        expected.updated_at_ms = recovered_at;
        expected.last_observation = Some(AdaptiveObservationRefV1 {
            effect,
            observation_digest: observation.digest().to_owned(),
        });
        expected.continuation.as_mut().unwrap().observation_required = false;
        assert_eq!(adopted, expected);
        assert_eq!(fixture.read(), expected);
        assert!(authority.current_for_request(&request).is_err());
    }
}

#[test]
fn stopped_record_recovery_rejects_assignment_changes_before_and_after_receipt_read() {
    for (after_read, different_agent) in [(false, false), (false, true), (true, false)] {
        let mut fixture = Fixture::continued();
        let now = now_unix_ms();
        let request = fixture.claim_tool(now);
        fixture.retain_tool_result(&request, now, now, PRIVATE_CONTENT);
        let before = fixture.read();
        stop_receipt_agent(&fixture);
        let authority = fixture.api.authority.as_ref().unwrap();
        if !after_read {
            reassign_receipt_work(&fixture, different_agent);
        }
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tool = RetainedReceiptTool {
            fixture: &fixture,
            reassign_after_read: after_read,
            reads: Arc::clone(&reads),
        };
        let core = AdaptiveWorkflowCore::new(
            Arc::clone(&fixture.api.store),
            WorkbenchRecordRecoveryAuthority(authority.as_ref()),
            UnavailableAdaptiveModel,
            tool,
        );
        let error = core
            .reconcile_tool(
                before.grant.session_id,
                &before.grant.authority,
                Uuid::new_v4(),
                now_unix_ms(),
            )
            .unwrap_err();
        assert_eq!(
            error.code,
            if different_agent {
                WorkflowErrorCode::OrganizationUnavailable
            } else {
                WorkflowErrorCode::AuthorityConflict
            }
        );
        assert_eq!(reads.load(Ordering::SeqCst), usize::from(after_read));
        assert_eq!(fixture.read(), before);
    }
}

struct ReceiptReaderRuntime {
    invocation_id: String,
    input_digest: String,
    result: WorkbenchMessage,
    calls: Vec<String>,
}

impl crate::workbench::WorkbenchRuntimeClient for ReceiptReaderRuntime {
    fn exchange(
        &mut self,
        agent_id: AgentId,
        request: sentinel_common::NanoExecRequest,
    ) -> anyhow::Result<crate::workbench::WorkbenchRuntimeExchange<'_>> {
        let frame: serde_json::Value = serde_json::from_str(&request.input).unwrap();
        assert_eq!(frame["invocation_id"], self.invocation_id);
        let (state, messages) = match request.operation.as_str() {
            "workbench_recover" => {
                assert!(self.calls.is_empty());
                assert_eq!(frame["input_digest"], self.input_digest);
                ("accepted", Vec::new())
            }
            "workbench_poll" => {
                assert_eq!(self.calls, ["workbench_recover"]);
                (
                    "completed",
                    vec![
                        self.result.clone(),
                        WorkbenchMessage::Progress {
                            schema_version: WORKBENCH_SCHEMA_VERSION,
                            invocation_id: self.invocation_id.clone(),
                            stage: sentinel_common::WorkbenchProgressStage::Completed,
                            elapsed_ms: 0,
                        },
                    ],
                )
            }
            operation => panic!("receipt recovery dispatched unexpected effect: {operation}"),
        };
        self.calls.push(request.operation);
        Ok(crate::workbench::WorkbenchRuntimeExchange::new(
            sentinel_common::NanoExecResult {
                runtime_key: WORKBENCH_RUNTIME_BWRAP.to_owned(),
                workload_id: format!("AGENT-{:02}", agent_id.0),
                success: true,
                output: serde_json::to_string(&serde_json::json!({
                    "schema_version": WORKBENCH_SCHEMA_VERSION,
                    "invocation_id": self.invocation_id,
                    "state": state,
                    "messages": messages,
                }))?,
            },
            || Ok(()),
        ))
    }
}

#[test]
fn started_pending_tool_recovers_exact_digest_then_polls_without_second_start() {
    let mut fixture = Fixture::continued();
    let now = now_unix_ms();
    let request = fixture.claim_tool(now);
    fixture.tools.reserve(&request, now).unwrap();
    fixture
        .tools
        .mark_executing(&request.invocation_id, &request.input_digest, now)
        .unwrap();
    stop_receipt_agent(&fixture);
    let authority = fixture.api.authority.as_ref().unwrap();
    let source: Arc<dyn WorkbenchAuthoritySource> = authority.clone();
    let (response, _receiver) = mpsc::sync_channel(1);
    let command = pending_adaptive_tool_dispatch(request.clone(), source, response, true);
    let WorkbenchDispatchCommand::Recover {
        invocation_id,
        authority: source,
        ..
    } = command
    else {
        panic!("started pending tool must recover, never submit or poll first");
    };
    assert_eq!(invocation_id, request.invocation_id);
    let (profile, digest) = authority
        .profile_for_binding(&request.tool_profile)
        .unwrap();
    let coordinator = WorkbenchCoordinator::new(&fixture.tools, profile, digest);
    let result = WorkbenchMessage::Result {
        schema_version: WORKBENCH_SCHEMA_VERSION,
        invocation_id: request.invocation_id.clone(),
        input_digest: request.input_digest.clone(),
        outcome: WorkbenchOutcome::Succeeded,
        resources: WorkbenchResourceUsage::default(),
        artifacts: Vec::new(),
        output: BTreeMap::from([("content".into(), PRIVATE_CONTENT.into())]),
        error: None,
    };
    let mut runtime = ReceiptReaderRuntime {
        invocation_id: request.invocation_id.clone(),
        input_digest: request.input_digest.clone(),
        result,
        calls: Vec::new(),
    };
    let accepted = coordinator
        .recover_executing(&mut runtime, &invocation_id, source.as_ref(), now)
        .unwrap();
    let effect = AdaptiveEffectV1 {
        id: Uuid::parse_str(&invocation_id).unwrap(),
        request_digest: request.input_digest.clone(),
    };
    let terminal = poll_executing_adaptive_tool(accepted, &effect, || {
        coordinator
            .poll(&mut runtime, &invocation_id, source.as_ref(), now)
            .map_err(map_workbench_dispatch_error)
    })
    .unwrap();
    assert!(terminal.records.last().unwrap().state.is_terminal());
    assert_eq!(runtime.calls, ["workbench_recover", "workbench_poll"]);
    fixture
        .private_observation(&invocation_id)
        .validate(&invocation_id, &request.input_digest)
        .unwrap();
    assert_eq!(fixture.read(), fixture.session);
    assert_eq!(
        (fixture.session.model_calls, fixture.session.tool_calls),
        (2, 1)
    );
    let (response, _receiver) = mpsc::sync_channel(1);
    assert!(matches!(
        pending_adaptive_tool_dispatch(request.clone(), authority.clone(), response, false),
        WorkbenchDispatchCommand::Submit { request: submitted, .. }
            if *submitted == request
    ));
    assert!(authority.current_for_request(&request).is_err());
    assert!(coordinator
        .submit(&mut runtime, &request, source.as_ref(), now)
        .is_err());
    assert_eq!(runtime.calls, ["workbench_recover", "workbench_poll"]);
}

#[test]
fn exact_pending_tool_authority_survives_process_exit_but_new_admission_does_not() {
    let mut fixture = Fixture::root(false);
    let now = now_unix_ms();
    let request = fixture.claim_tool(now);
    fixture.tools.reserve(&request, now).unwrap();
    fixture
        .tools
        .mark_executing(&request.invocation_id, &request.input_digest, now)
        .unwrap();
    let record = fixture.tools.load(&request.invocation_id).unwrap().unwrap();
    let authority = fixture.api.authority.as_ref().unwrap();
    authority.runtime_health.write().unwrap().agents.clear();
    assert!(authority.current_for_request(&request).is_err());
    assert!(authority.current_for_record(&record).is_ok());
    let adapter = fixture.api.workbench.as_ref().unwrap();
    let effect = AdaptiveEffectV1 {
        id: Uuid::parse_str(&request.invocation_id).unwrap(),
        request_digest: request.input_digest.clone(),
    };
    let rebuilt = adapter
        .build_adaptive_request(&fixture.session, &effect, &request.tool)
        .unwrap();
    assert_eq!(rebuilt.input_digest, request.input_digest);
    let mut fresh = fixture.session.clone();
    fresh.cursor = AdaptiveCursorV1::ReadyForTool {
        tool: request.tool.clone(),
        tool_digest: adaptive_tool_digest(&request.tool).unwrap(),
    };
    assert!(adapter
        .build_adaptive_request(&fresh, &effect, &request.tool)
        .is_err());
    let mut foreign = record.clone();
    foreign.project_id = "project-foreign".to_owned();
    assert!(authority.current_for_record(&foreign).is_err());
    let mut revoked = record;
    revoked.caller_id = "revoked-principal".to_owned();
    assert!(authority.current_for_record(&revoked).is_err());
}

#[test]
fn committed_private_tool_observation_is_readable_without_a_live_process() {
    let mut fixture = Fixture::root(false);
    let now = now_unix_ms();
    let request = fixture.claim_tool(now);
    fixture.observe_tool(&request, now, now);
    fixture
        .api
        .authority
        .as_ref()
        .unwrap()
        .runtime_health
        .write()
        .unwrap()
        .agents
        .clear();
    let observation = fixture.private_observation(&request.invocation_id);
    observation
        .validate(&request.invocation_id, &request.input_digest)
        .unwrap();
    assert_eq!(
        fixture.read().last_observation.unwrap().observation_digest,
        observation.digest()
    );
    assert!(fixture
        .api
        .authority
        .as_ref()
        .unwrap()
        .current_for_request(&request)
        .is_err());
}

struct Fixture {
    api: WorkflowApi,
    session: AdaptiveSessionV1,
    tools: Arc<WorkbenchInvocationStore>,
    temp: tempfile::TempDir,
}

impl Fixture {
    fn continued() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let (mut api, context) = fixture_schema2_blocked(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
        );
        let before = context.source.source_session.clone();
        let (id, digest) = reserve_and_claim_schema2(&api, &context);
        let completion = completion(&context, 2, "continue", Some((3, 30_000)));
        persist(&api, &completion, &context, &id, &digest, true);
        api.accept_leadership_review(&completion, &context, &id, &digest)
            .unwrap();
        let session = api
            .store
            .adaptive_session(before.grant.session_id, &before.grant.authority)
            .unwrap()
            .unwrap();
        assert_eq!(session.grant, before.grant);
        assert_eq!(session.model_calls, before.model_calls);
        assert_eq!(session.active_model_ceiling(), before.model_calls + 3);
        assert!(session.active_model_ceiling() < session.grant.max_model_calls);
        Self::attach_workbench(&mut api);
        let tools =
            Arc::new(WorkbenchInvocationStore::open(temp.path().join("workbench.redb")).unwrap());
        Self {
            api,
            session,
            tools,
            temp,
        }
    }

    fn root(expired: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("company.sqlite");
        let mut api = super::model_work::configured_test_api(&path);
        let created = now_unix_ms() - if expired { 600_000 } else { 0 };
        let binding = super::model_work::assign_test_work_from_at(&api, Some(8), 0, created);
        api.subscription_allowance_id = Some(binding.reservation_id);
        api.event_store = Some(
            sentinel_limbo::EventStore::open(temp.path().join("events.sqlite").to_str().unwrap())
                .unwrap(),
        );
        let tenant = TenantId::parse(&binding.tenant_id).unwrap();
        let project_id = ProjectId::parse(&binding.project_id).unwrap();
        let work_item = WorkItemId::parse(&binding.work_item_id).unwrap();
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
            .snapshot_for_admission(&tenant, &project_id, &work_item, binding.agent_id, false)
            .unwrap();
        let grant = AdaptiveSessionGrantV1 {
            schema_version: 1,
            session_id: Uuid::new_v4(),
            provider_allowance_id: allowance.allowance_id.clone(),
            provider_authority_digest:
                sentinel_workflow::adaptive_leadership_continuation_provider_authority_digest(
                    allowance, &authority,
                )
                .unwrap(),
            authority: authority.clone(),
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
            .begin_adaptive_session(&grant, &authority, created)
            .unwrap()
            .1;
        seed_planning_receipt_with_catalog(&api, &path, &project, &allowance.grant.catalog_digest);
        let planning = api
            .store
            .project_planning_call(&tenant, &project_id)
            .unwrap()
            .unwrap();
        assert_eq!(planning.grant.provider, session.grant.provider);
        assert_eq!(planning.grant.model, session.grant.model);
        assert_eq!(planning.grant.catalog_digest, session.grant.catalog_digest);
        assert_eq!(planning.grant.token_policy, allowance.grant.token_policy);
        assert_eq!(
            planning.grant.max_duration_ms,
            session.grant.max_call_duration_ms
        );
        Self::attach_workbench(&mut api);
        let tools =
            Arc::new(WorkbenchInvocationStore::open(temp.path().join("workbench.redb")).unwrap());
        Self {
            api,
            session,
            tools,
            temp,
        }
    }

    fn attach_workbench(api: &mut WorkflowApi) {
        api.workbench = Some(Arc::new(WorkbenchExecutionAdapter {
            store: Arc::clone(&api.store),
            authority: Arc::clone(api.authority.as_ref().unwrap()),
        }));
    }

    fn with_workbench_store<R>(&self, run: impl FnOnce() -> R) -> R {
        let authority = self.api.authority.as_ref().unwrap();
        let (profile, digest) = authority
            .profile_for_binding(&self.session.grant.authority.profile_id)
            .unwrap();
        with_private_observation_store_for_test(
            Arc::clone(&self.tools),
            profile.clone(),
            digest.to_owned(),
            run,
        )
    }

    fn project(&self) -> sentinel_workflow::ProjectV1 {
        let authority = &self.session.grant.authority;
        self.api
            .store
            .company_project(&authority.tenant_id, &authority.project_id)
            .unwrap()
            .unwrap()
    }

    fn read(&self) -> AdaptiveSessionV1 {
        self.api
            .store
            .adaptive_session(self.session.grant.session_id, &self.session.grant.authority)
            .unwrap()
            .unwrap()
    }

    fn calls(&self) -> Vec<AdaptiveLeadershipReviewCallV1> {
        self.api
            .store
            .adaptive_leadership_review_calls(
                &self.session.grant.authority.tenant_id,
                self.session.grant.session_id,
            )
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

    fn claim_tool(&mut self, now: u64) -> WorkbenchRequest {
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
        let adapter = WorkbenchExecutionAdapter {
            store: Arc::clone(&self.api.store),
            authority: Arc::clone(self.api.authority.as_ref().unwrap()),
        };
        let tool_effect = adapter.adaptive_tool_effect(&self.session, &tool).unwrap();
        let request = adapter
            .build_adaptive_request(&self.session, &tool_effect, &tool)
            .unwrap();
        self.advance(
            AdaptiveTransitionV1::ClaimTool {
                effect: tool_effect,
                tool_digest,
            },
            now,
        );
        request
    }

    fn observe_tool(&mut self, request: &WorkbenchRequest, claimed_at: u64, observed_at: u64) {
        self.observe_tool_content(request, claimed_at, observed_at, PRIVATE_CONTENT);
    }

    fn observe_tool_content(
        &mut self,
        request: &WorkbenchRequest,
        claimed_at: u64,
        observed_at: u64,
        content: &str,
    ) {
        let observation = self.retain_tool_result(request, claimed_at, observed_at, content);
        self.advance(
            AdaptiveTransitionV1::ObserveTool {
                observation: AdaptiveObservationRefV1 {
                    effect: AdaptiveEffectV1 {
                        id: Uuid::parse_str(&request.invocation_id).unwrap(),
                        request_digest: request.input_digest.clone(),
                    },
                    observation_digest: observation.digest().to_owned(),
                },
            },
            observed_at,
        );
        assert_eq!(self.session.cursor, AdaptiveCursorV1::ReadyForModel);
    }

    fn retain_tool_result(
        &self,
        request: &WorkbenchRequest,
        claimed_at: u64,
        observed_at: u64,
        content: &str,
    ) -> sentinel_common::WorkbenchPrivateObservation {
        self.tools.reserve(request, claimed_at).unwrap();
        self.tools
            .mark_executing(&request.invocation_id, &request.input_digest, claimed_at)
            .unwrap();
        let result = WorkbenchMessage::Result {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: request.invocation_id.clone(),
            input_digest: request.input_digest.clone(),
            outcome: WorkbenchOutcome::Succeeded,
            resources: WorkbenchResourceUsage::default(),
            artifacts: Vec::new(),
            error: None,
            output: BTreeMap::from([("content".into(), content.into())]),
        };
        let record = self.tools.accept_result(&result, observed_at).unwrap();
        assert!(!serde_json::to_string(&record)
            .unwrap()
            .contains(PRIVATE_CONTENT));
        let observation = self.private_observation(&record.invocation_id);
        observation
            .validate(&request.invocation_id, &request.input_digest)
            .unwrap();
        observation
    }

    fn private_observation(
        &self,
        invocation_id: &str,
    ) -> sentinel_common::WorkbenchPrivateObservation {
        let authority = self.api.authority.as_ref().unwrap();
        let (profile, digest) = authority
            .profile_for_binding(&self.session.grant.authority.profile_id)
            .unwrap();
        WorkbenchCoordinator::new(&self.tools, profile, digest)
            .private_observation(invocation_id, authority.as_ref())
            .unwrap()
            .unwrap()
    }

    fn exhaust_calls(&mut self) {
        while self.session.model_calls < self.session.active_model_ceiling() {
            let now = now_unix_ms().max(self.session.updated_at_ms);
            let request = self.claim_tool(now);
            self.observe_tool(&request, now, now);
        }
    }

    fn issue(&self, now: u64) -> LeadershipContext {
        let before = self.read();
        let project = self.project();
        assert!(reconcile_review_at(&self.api, &project, now));
        assert_eq!(self.read(), before);
        assert_eq!(self.project(), project);
        let calls: Vec<_> = self.calls().into_iter().filter(is_budget).collect();
        assert_eq!(calls.len(), 1);
        let call = calls.into_iter().next().unwrap();
        assert_eq!(call.grant.schema_version, 3);
        assert_eq!(call.grant.expected_session_version, before.version);
        assert_eq!(call.context.source_session, before);
        assert_eq!(call.context.source_project, project);
        assert!(call.dispatch.is_none());
        assert!(call.decision.is_none());
        let binding = LeadershipAuthority::from_call(&call);
        let context = self
            .with_workbench_store(|| self.api.prepare_leadership_review(&binding))
            .expect("production preparation must retrieve the authorized retained observation");
        assert_eq!(context.binding, binding);
        assert_eq!(context.source, call.context);
        assert_eq!(context.context_digest, call.context_digest().unwrap());
        let reference = before.last_observation.as_ref().unwrap();
        let observation = context.private_observation.as_ref().unwrap();
        observation
            .validate(
                &reference.effect.id.to_string(),
                &reference.effect.request_digest,
            )
            .unwrap();
        assert_eq!(observation.digest(), reference.observation_digest);
        assert_eq!(
            observation,
            &self.private_observation(&reference.effect.id.to_string())
        );
        context.validate_dispatch(now).unwrap();
        let budget = budget(&context);
        assert_eq!(budget.schema_version, 1);
        assert_eq!(
            budget.root_allowance.allowance_id,
            before.grant.provider_allowance_id
        );
        assert_eq!(
            budget.root_allowance.grant.max_calls,
            before.grant.max_model_calls
        );
        assert_eq!(budget.observed_at_ms, now);
        assert_eq!(
            budget.active_allowance_digest,
            adaptive_budget_allowance_digest(project.subscription_call.as_ref().unwrap()).unwrap()
        );
        assert_eq!(
            budget.continuation_history_digest,
            adaptive_budget_history_digest(&before.continuation).unwrap()
        );
        assert_eq!(
            budget.model_calls_exhausted,
            before.model_calls >= before.active_model_ceiling()
        );
        assert_eq!(budget.deadline_expired, now >= before.active_deadline_ms());
        for reference in [
            format!(
                "adaptive-budget-root:{}:{}",
                budget.root_allowance.allowance_id, before.grant.provider_authority_digest
            ),
            format!("adaptive-budget-current:{}", budget.active_allowance_digest),
            format!(
                "adaptive-budget-history:{}",
                budget.continuation_history_digest
            ),
            format!(
                "workbench-observation:{}:{}",
                before.last_observation.as_ref().unwrap().effect.id,
                before.last_observation.as_ref().unwrap().observation_digest
            ),
        ] {
            assert!(context.source.evidence_refs.contains(&reference));
        }
        assert!(context.prompt().unwrap().contains(PRIVATE_CONTENT));
        assert!(!serde_json::to_string(&context.source)
            .unwrap()
            .contains(PRIVATE_CONTENT));
        context
    }

    fn technical_lead_fallback(&self) {
        // Fixture-only governed participant resealing; no production grant is seeded.
        let mut project = self.project();
        let mut leader = project
            .governance
            .participants
            .iter()
            .find(|participant| participant.role == CompanyRoleV1::ProjectManager)
            .unwrap()
            .clone();
        leader.agent_id = AgentId(7);
        leader.principal_id = "technical-lead".into();
        leader.role = CompanyRoleV1::TechnicalLead;
        project.governance.participants.push(leader);
        persist_discovery_project(&self.temp.path().join("company.sqlite"), &project);
        self.api
            .authority
            .as_ref()
            .unwrap()
            .runtime_health
            .write()
            .unwrap()
            .agents
            .iter_mut()
            .find(|agent| agent.agent_id == 5)
            .unwrap()
            .expected_active = false;
    }

    fn issue_without_observation_at(&self, now: u64, schema: u16) -> LeadershipContext {
        assert!(self.session.last_observation.is_none());
        let before = self.read();
        assert!(reconcile_review_at(&self.api, &self.project(), now));
        let call = self
            .calls()
            .into_iter()
            .find(|call| {
                call.grant.expected_session_version == before.version
                    && call.grant.schema_version == schema
                    && call.decision.is_none()
                    && call.retired_at_unix_ms.is_none()
            })
            .expect("reconciliation must issue the exact review head");
        let context = LeadershipContext {
            binding: LeadershipAuthority::from_call(&call),
            context_digest: call.context_digest().unwrap(),
            source: call.context,
            private_observation: None,
        };
        context.validate_dispatch(now).unwrap();
        assert_eq!(context.source.source_session, before);
        assert_eq!(self.read(), before);
        context
    }

    fn continue_without_observation_at(
        &mut self,
        context: &LeadershipContext,
        calls: u16,
        window: u64,
        now: u64,
    ) {
        // Deterministic historical before-send fixture, never a live provider call.
        let grant = &context.binding.grant;
        let id = format!("company-leadership-{}", grant.review_id);
        let digest = "c".repeat(64);
        self.api
            .event_store
            .as_ref()
            .unwrap()
            .reserve_llm_request(
                &id,
                &digest,
                &grant.leadership_principal.agent_id.unwrap().to_string(),
            )
            .unwrap();
        self.api
            .store
            .claim_adaptive_leadership_review_call(
                &grant.leadership_principal,
                &sentinel_workflow::ClaimAdaptiveLeadershipReviewCallV1 {
                    review_id: grant.review_id,
                    allowance_id: context.binding.allowance_id.clone(),
                    request_id: id.clone(),
                    request_digest: digest.clone(),
                    context_digest: context.context_digest.clone(),
                },
                now,
            )
            .unwrap();
        let result = completion(
            context,
            grant.schema_version,
            "continue",
            Some((calls, window)),
        );
        persist(&self.api, &result, context, &id, &digest, true);
        let before = self.read();
        self.api
            .accept_leadership_review_at(&result, context, &id, &digest, now)
            .unwrap();
        self.session = self.read();
        assert_eq!(self.session.grant, before.grant);
        assert_eq!(
            (self.session.model_calls, self.session.tool_calls),
            (before.model_calls, before.tool_calls)
        );
    }

    fn assert_provider_duration_across_reopen(&self, expected: u64) {
        let before = self.read();
        let project = self.project();
        let allowance = project.subscription_call.as_ref().unwrap();
        assert_eq!(allowance.grant.max_duration_ms, expected);
        assert_eq!(before.effective_grant().max_call_duration_ms, expected);
        let mut reopened =
            super::model_work::configured_test_api(&self.temp.path().join("company.sqlite"));
        reopened.subscription_allowance_id = Some(allowance.allowance_id.clone());
        reopened.event_store = Some(
            sentinel_limbo::EventStore::open(
                self.temp.path().join("events.sqlite").to_str().unwrap(),
            )
            .unwrap(),
        );
        Self::attach_workbench(&mut reopened);
        let original = self
            .api
            .adaptive_provider_authority(before.grant.authority.agent_id)
            .expect("effective duration must agree with the persisted allowance")
            .expect("current continuation must prepare provider authority");
        for api in [&self.api, &reopened] {
            let binding = api
                .adaptive_provider_authority(before.grant.authority.agent_id)
                .expect("mixed duration must remain admissible after reopen")
                .expect("reopened continuation must retain provider authority");
            assert_eq!(binding, original);
            assert_eq!(binding.grant, before.effective_grant());
            assert_eq!(binding.grant.max_call_duration_ms, expected);
            assert_eq!(binding.grant.provider_allowance_id, allowance.allowance_id);
            assert_eq!(
                binding.grant.deadline_ms,
                allowance.grant.expires_at_unix_ms
            );
            assert_eq!(binding.session_version, before.version);
            assert_eq!(binding.previous_observation, before.last_observation);
            let model = self.with_workbench_store(|| api.prepare_adaptive_model(&binding))
                .expect("production model preparation must use persisted duration and private observation");
            assert_eq!(model.binding, binding);
            let expected_observation = before
                .last_observation
                .as_ref()
                .map(|reference| self.private_observation(&reference.effect.id.to_string()));
            assert_eq!(model.observation, expected_observation);
            model.validate_dispatch(now_unix_ms()).unwrap();
            assert_eq!(
                api.store
                    .adaptive_session(before.grant.session_id, &before.grant.authority)
                    .unwrap()
                    .unwrap(),
                before
            );
            assert_eq!(
                api.store
                    .company_project(&project.tenant_id, &project.project_id)
                    .unwrap()
                    .unwrap(),
                project
            );
        }
        assert_eq!(self.read(), before);
        assert_eq!(
            self.calls(),
            reopened
                .store
                .adaptive_leadership_review_calls(
                    &before.grant.authority.tenant_id,
                    before.grant.session_id,
                )
                .unwrap()
        );
    }
}

fn is_budget(call: &AdaptiveLeadershipReviewCallV1) -> bool {
    matches!(
        &call.grant.subject,
        Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { .. })
    )
}

fn budget(context: &LeadershipContext) -> &AdaptiveBudgetWindowAuthorityV1 {
    match context.binding.grant.subject.as_ref().unwrap() {
        AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget } => budget,
        _ => panic!("expected actual schema-3 budget subject"),
    }
}

fn completion(
    context: &LeadershipContext,
    schema: u16,
    kind: &str,
    window: Option<(u16, u64)>,
) -> ModelExecutionCompletion {
    let mut decision = serde_json::json!({"kind": kind,
        "rationale": "Review the exact retained observation within current policy.",
        "evidence_refs": context.source.evidence_refs});
    if let Some((calls, millis)) = window {
        decision["additional_model_calls"] = serde_json::json!(calls);
        decision["window_ms"] = serde_json::json!(millis);
    }
    ModelExecutionCompletion {
        context: ModelExecutionContext::AdaptiveLeadershipReview(Box::new(context.clone())),
        content: serde_json::json!({"schema_version": schema, "decision": decision}).to_string(),
        admissible: true,
    }
}

fn dispatch(fixture: &Fixture, context: &LeadershipContext) -> (String, String) {
    let api = &fixture.api;
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
    let mut request = serde_json::json!({"schema_version": 5,
        "allowance_id": context.binding.allowance_id,
        "agent_id": grant.leadership_principal.agent_id.unwrap().0,
        "request_id": id, "request_digest": digest, "context_digest": context.context_digest,
        "provider": grant.provider, "model": grant.model, "catalog_digest": grant.catalog_digest,
        "subject": {"kind": "adaptive_leadership_review", "review_id": grant.review_id,
            "review_kind": "budget_window_exhausted"}});
    let before = api
        .store
        .adaptive_leadership_review_call(&grant.leadership_principal.tenant_id, grant.review_id)
        .unwrap()
        .unwrap();
    request["subject"]["review_kind"] = serde_json::json!("blocked_continuation");
    assert_eq!(
        fixture
            .with_workbench_store(
                || api.subscription_dispatch(&serde_json::to_vec(&request).unwrap())
            )
            .status,
        403
    );
    assert_eq!(api.store.adaptive_leadership_review_call(
        &grant.leadership_principal.tenant_id, grant.review_id,
    ).unwrap().unwrap(), before);
    request["subject"]["review_kind"] = serde_json::json!("budget_window_exhausted");
    // Real preparation and dispatch admission, not a provider invocation.
    assert_eq!(
        fixture
            .with_workbench_store(
                || api.subscription_dispatch(&serde_json::to_vec(&request).unwrap())
            )
            .status,
        200
    );
    let claimed = api
        .store
        .adaptive_leadership_review_call(&grant.leadership_principal.tenant_id, grant.review_id)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.context, before.context);
    assert_eq!(claimed.grant, before.grant);
    assert!(claimed.dispatch.is_some());
    assert_eq!(
        fixture
            .with_workbench_store(
                || api.subscription_dispatch(&serde_json::to_vec(&request).unwrap())
            )
            .status,
        403
    );
    (id, digest)
}

#[test]
fn exact_observed_call_ceiling_issues_pm_or_governed_tl_without_developer_grant() {
    for technical_lead in [false, true] {
        let mut fixture = Fixture::continued();
        fixture.exhaust_calls();
        if technical_lead {
            fixture.technical_lead_fallback();
        }
        let now = now_unix_ms().max(fixture.session.updated_at_ms);
        assert!(fixture.session.model_window_exhausted_at(now));
        let before = fixture.read();
        let context = fixture.issue(now);
        assert_eq!(
            context.binding.grant.leadership_principal.role,
            if technical_lead {
                CompanyRoleV1::TechnicalLead
            } else {
                CompanyRoleV1::ProjectManager
            }
        );
        assert!(budget(&context).model_calls_exhausted);
        assert!(!budget(&context).deadline_expired);
        assert_eq!(fixture.read(), before);
        let calls = fixture.calls();
        reconcile_review_at(&fixture.api, &fixture.project(), now);
        assert_eq!(fixture.calls(), calls);
        assert_eq!(fixture.read(), before);
    }
}

#[test]
fn observed_tool_at_exact_deadline_issues_budget_review_not_completion_or_blocked() {
    let mut fixture = Fixture::root(true);
    let claimed_at = fixture.session.grant.created_at_ms + 1;
    let deadline = fixture.session.active_deadline_ms();
    let request = fixture.claim_tool(claimed_at);
    let pending = fixture.read();
    reconcile_review_at(&fixture.api, &fixture.project(), deadline);
    assert_eq!(fixture.read(), pending);
    assert!(fixture.calls().is_empty());
    fixture.observe_tool(&request, claimed_at, deadline);
    assert!(fixture.session.model_calls < fixture.session.active_model_ceiling());
    let now = now_unix_ms();
    let context = fixture.issue(now);
    assert!(budget(&context).deadline_expired);
    assert!(!budget(&context).model_calls_exhausted);
    assert_eq!(context.source.source_session.updated_at_ms, deadline);
    assert_eq!(
        context.source.source_session.cursor,
        AdaptiveCursorV1::ReadyForModel
    );
}

#[test]
fn pending_and_unknown_model_and_tool_heads_are_not_normal_budget_subjects() {
    for tool_head in [false, true] {
        let mut fixture = Fixture::root(false);
        let now = now_unix_ms();
        let effect = if tool_head {
            let request = fixture.claim_tool(now);
            AdaptiveEffectV1 {
                id: Uuid::parse_str(&request.invocation_id).unwrap(),
                request_digest: request.input_digest,
            }
        } else {
            fixture.claim_model(now)
        };
        for unknown in [false, true] {
            if unknown {
                fixture.advance(
                    AdaptiveTransitionV1::MarkUnknown {
                        effect: effect.clone(),
                    },
                    now,
                );
            }
            let before = fixture.read();
            reconcile_review_at(
                &fixture.api,
                &fixture.project(),
                before.active_deadline_ms(),
            );
            assert_eq!(fixture.read(), before);
            assert!(fixture.calls().iter().all(|call| !is_budget(call)));
        }
    }
}

#[test]
fn normal_continue_is_model_selected_preserves_counters_root_and_replays_once() {
    let mut fixture = Fixture::continued();
    fixture.exhaust_calls();
    let context = fixture.issue(now_unix_ms());
    let before = fixture.read();
    let (id, digest) = dispatch(&fixture, &context);
    let result = completion(&context, 3, "continue", Some((2, 30_000)));
    persist(&fixture.api, &result, &context, &id, &digest, true);
    fixture
        .api
        .accept_leadership_review(&result, &context, &id, &digest)
        .unwrap();
    let after = fixture.read();
    assert_eq!(after.grant, before.grant);
    assert_eq!(
        (after.model_calls, after.tool_calls),
        (before.model_calls, before.tool_calls)
    );
    assert_eq!(after.last_observation, before.last_observation);
    assert_eq!(
        after.last_model_result_digest,
        before.last_model_result_digest
    );
    assert_eq!(after.cursor, AdaptiveCursorV1::ReadyForModel);
    assert_eq!(after.active_model_ceiling(), before.model_calls + 2);
    assert!(after.requires_fresh_observation());
    let history = &after.continuation.as_ref().unwrap().authorizations;
    let old_history = &before.continuation.as_ref().unwrap().authorizations;
    assert_eq!(&history[..old_history.len()], old_history.as_slice());
    assert_eq!(history.len(), old_history.len() + 1);
    assert_eq!(history.last().unwrap().additional_model_calls, 2);
    let calls = fixture.calls();
    let project = fixture.project();
    fixture
        .api
        .accept_leadership_review(&result, &context, &id, &digest)
        .unwrap();
    assert_eq!(fixture.read(), after);
    assert_eq!(fixture.calls(), calls);
    assert_eq!(fixture.project(), project);
    let changed = completion(&context, 3, "continue", Some((1, 30_000)));
    assert!(fixture
        .api
        .accept_leadership_review(&changed, &context, &id, &digest)
        .is_err());
    assert_eq!(fixture.read(), after);
}

#[test]
fn defer_budget_is_durable_without_any_developer_grant() {
    let mut fixture = Fixture::continued();
    fixture.exhaust_calls();
    assert!(fixture.session.model_calls < fixture.session.grant.max_model_calls);
    let context = fixture.issue(now_unix_ms());
    let before = fixture.read();
    let project = fixture.project();
    let (id, digest) = dispatch(&fixture, &context);
    let result = completion(&context, 3, "defer_budget", None);
    persist(&fixture.api, &result, &context, &id, &digest, true);
    fixture
        .api
        .accept_leadership_review(&result, &context, &id, &digest)
        .unwrap();
    let call = fixture.calls().into_iter().find(is_budget).unwrap();
    assert!(matches!(
        call.decision.as_ref().unwrap().decision,
        AdaptiveLeadershipReviewDecisionKindV1::DeferBudget { .. }
    ));
    assert!(call.model_response_digest.is_some());
    assert!(call.continuation.is_none());
    assert!(call.resolution_event_id.is_none());
    assert_eq!(fixture.read(), before);
    assert_eq!(fixture.project(), project);
    fixture
        .api
        .accept_leadership_review(&result, &context, &id, &digest)
        .unwrap();
    reconcile_review_at(&fixture.api, &project, now_unix_ms());
    assert_eq!(
        fixture
            .calls()
            .into_iter()
            .filter(is_budget)
            .collect::<Vec<_>>(),
        vec![call]
    );
    assert_eq!(fixture.read(), before);
    assert_eq!(fixture.project(), project);
}

#[test]
fn root_ceiling_exhaustion_is_reported_but_cannot_issue_normal_renewal_authority() {
    let mut fixture = Fixture::continued();
    fixture.exhaust_calls();
    let context = fixture.issue(now_unix_ms());
    let remaining = fixture.session.grant.max_model_calls - fixture.session.model_calls;
    let (id, digest) = dispatch(&fixture, &context);
    // Synthetic completion through real stores establishes prior reviews and
    // spends the final bounded window; this is not a provider/live decision.
    let result = completion(&context, 3, "continue", Some((remaining, 30_000)));
    persist(&fixture.api, &result, &context, &id, &digest, true);
    fixture
        .api
        .accept_leadership_review(&result, &context, &id, &digest)
        .unwrap();
    fixture.session = fixture.read();
    fixture.exhaust_calls();
    let now = now_unix_ms();
    let before = fixture.read();
    let project = fixture.project();
    let old_reviews = fixture.calls();
    assert!(!old_reviews.is_empty());
    assert!(old_reviews.iter().all(|call| call.decision.is_some()));
    let tenant = &before.grant.authority.tenant_id;
    let session_id = before.grant.session_id;
    assert_eq!(before.model_calls, before.grant.max_model_calls);
    assert!(before.model_window_exhausted_at(now));
    assert!(!fixture
        .api
        .store
        .adaptive_budget_window_limit_recorded(tenant, session_id, before.version,)
        .unwrap());

    let company_path = fixture.temp.path().join("company.sqlite");
    let receipt_rows = || {
        let connection = sentinel_limbo::rusqlite::Connection::open(&company_path).unwrap();
        let mut statement = connection
            .prepare(
                "SELECT entity_id,version,payload,payload_digest FROM company_entities
             WHERE tenant_id=?1 AND entity_kind='adaptive_budget_window_limit' ORDER BY entity_id",
            )
            .unwrap();
        let rows = statement
            .query_map(sentinel_limbo::rusqlite::params![tenant.0], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    };
    let company_events = || {
        let connection = sentinel_limbo::rusqlite::Connection::open(&company_path).unwrap();
        let mut statement = connection.prepare(
            "SELECT sequence,event_type,payload,payload_digest FROM company_events ORDER BY sequence",
        ).unwrap();
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    };
    let provider_rows = || {
        let connection =
            sentinel_limbo::rusqlite::Connection::open(fixture.temp.path().join("events.sqlite"))
                .unwrap();
        let mut statement = connection.prepare(
            "SELECT request_id,request_digest,status,payload FROM llm_completion_outbox ORDER BY request_id",
        ).unwrap();
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    };
    assert!(receipt_rows().is_empty());
    let prior_events = company_events();
    let prior_provider_rows = provider_rows();
    assert!(reconcile_review_at(&fixture.api, &project, now));
    assert!(fixture
        .api
        .store
        .adaptive_budget_window_limit_recorded(tenant, session_id, before.version,)
        .unwrap());
    let receipts = receipt_rows();
    assert_eq!(receipts.len(), 1);
    let receipt: serde_json::Value = serde_json::from_slice(&receipts[0].2).unwrap();
    assert_eq!(receipts[0].1, 1);
    assert_eq!(receipt["receipt_id"], serde_json::json!(receipts[0].0));
    assert_eq!(receipt["schema_version"], 1);
    assert_eq!(receipt["recorded_at_ms"], serde_json::json!(now));
    assert_eq!(
        receipt["causes"],
        serde_json::json!(["root_calls_exhausted"])
    );
    assert_eq!(
        receipt["grant"]["session_id"],
        serde_json::json!(session_id)
    );
    assert_eq!(
        receipt["grant"]["expected_session_version"],
        serde_json::json!(before.version)
    );
    assert_eq!(
        receipt["context"]["source_session"],
        serde_json::to_value(&before).unwrap()
    );
    assert_eq!(
        receipt["context"]["source_project"],
        serde_json::to_value(&project).unwrap()
    );
    assert!(receipt.get("decision").is_none());
    assert!(receipt.get("continuation").is_none());
    let recorded_events = company_events();
    assert_eq!(recorded_events.len(), prior_events.len() + 1);
    assert_eq!(
        &recorded_events[..prior_events.len()],
        prior_events.as_slice()
    );
    let event = recorded_events.last().unwrap();
    assert_eq!(event.1, "adaptive_budget_window_limit_recorded");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&event.2).unwrap(),
        receipt
    );
    assert_eq!(fixture.read(), before);
    assert_eq!(fixture.project(), project);
    assert_eq!(fixture.calls(), old_reviews);
    assert_eq!(provider_rows(), prior_provider_rows);

    let reopened = sentinel_workflow::WorkflowStore::open(&company_path).unwrap();
    assert!(reopened
        .adaptive_budget_window_limit_recorded(tenant, session_id, before.version)
        .unwrap());
    assert!(!reopened
        .adaptive_budget_window_limit_recorded(tenant, session_id, before.version - 1)
        .unwrap());
    assert!(!reopened
        .adaptive_budget_window_limit_recorded(tenant, session_id, before.version + 1)
        .unwrap());
    assert!(!reopened
        .adaptive_budget_window_limit_recorded(tenant, Uuid::new_v4(), before.version)
        .unwrap());
    assert!(!reopened
        .adaptive_budget_window_limit_recorded(
            &TenantId::parse("tenant-other").unwrap(),
            session_id,
            before.version,
        )
        .unwrap());
    for replay_at in [now, now + 1, before.active_deadline_ms()] {
        assert!(reconcile_review_at(&fixture.api, &project, replay_at));
        assert_eq!(receipt_rows(), receipts);
        assert_eq!(company_events(), recorded_events);
        assert_eq!(provider_rows(), prior_provider_rows);
        assert_eq!(fixture.read(), before);
        assert_eq!(fixture.project(), project);
        assert_eq!(fixture.calls(), old_reviews);
    }
}

#[test]
fn private_observation_is_required_and_bound_to_exact_invocation_request_and_digest() {
    let mut fixture = Fixture::continued();
    fixture.exhaust_calls();
    let now = now_unix_ms();
    let context = fixture.issue(now);
    let (id, digest) = dispatch(&fixture, &context);
    let result = completion(&context, 3, "continue", Some((1, 30_000)));
    persist(&fixture.api, &result, &context, &id, &digest, true);
    let before = fixture.read();
    let calls = fixture.calls();
    let mut invalid = context.clone();
    invalid.private_observation = None;
    assert!(invalid.validate_dispatch(now).is_err());
    assert!(invalid.prompt().is_err());
    assert!(fixture
        .api
        .accept_leadership_review(&result, &invalid, &id, &digest)
        .is_err());
    for field in 0..3 {
        let mut invalid = context.clone();
        let reference = invalid
            .source
            .source_session
            .last_observation
            .as_mut()
            .unwrap();
        match field {
            0 => reference.effect.id = Uuid::new_v4(),
            1 => reference.effect.request_digest = "f".repeat(64),
            _ => reference.observation_digest = "f".repeat(64),
        }
        assert!(invalid.validate_dispatch(now).is_err());
        assert!(invalid.prompt().is_err());
        assert!(fixture
            .api
            .accept_leadership_review(&result, &invalid, &id, &digest)
            .is_err());
    }
    assert_eq!(fixture.read(), before);
    assert_eq!(fixture.calls(), calls);
}

#[test]
fn production_leadership_preparation_retrieves_retained_observation_after_store_reopen() {
    let mut fixture = Fixture::continued();
    fixture.exhaust_calls();
    let context = fixture.issue(now_unix_ms());
    let before = fixture.read();
    let project = fixture.project();
    let calls = fixture.calls();
    let reference = before.last_observation.as_ref().unwrap();
    let invocation = reference.effect.id.to_string();
    let record = fixture.tools.load(&invocation).unwrap().unwrap();
    assert_eq!(
        context
            .private_observation
            .as_ref()
            .unwrap()
            .output()
            .get("content")
            .unwrap(),
        PRIVATE_CONTENT
    );
    // Reopen the real retained store after all scoped hook clones have dropped.
    drop(fixture.tools);
    fixture.tools = Arc::new(
        WorkbenchInvocationStore::open(fixture.temp.path().join("workbench.redb")).unwrap(),
    );
    let prepared = fixture
        .with_workbench_store(|| fixture.api.prepare_leadership_review(&context.binding))
        .unwrap();
    assert_eq!(prepared, context);
    let observation = prepared.private_observation.as_ref().unwrap();
    observation
        .validate(&invocation, &reference.effect.request_digest)
        .unwrap();
    assert_eq!(observation.digest(), reference.observation_digest);
    assert_eq!(fixture.tools.load(&invocation).unwrap().unwrap(), record);
    assert_eq!(fixture.read(), before);
    assert_eq!(fixture.project(), project);
    assert_eq!(fixture.calls(), calls);
}

#[test]
fn production_preparation_denies_wrong_profile_and_revoked_observation_authority() {
    let mut fixture = Fixture::continued();
    fixture.exhaust_calls();
    let context = fixture.issue(now_unix_ms());
    let before = fixture.read();
    let project = fixture.project();
    let calls = fixture.calls();
    let authority = fixture.api.authority.as_ref().unwrap();
    let (profile, _) = authority
        .profile_for_binding(&before.grant.authority.profile_id)
        .unwrap();
    let profile = profile.clone();
    fixture.with_workbench_store(|| {
        let denied = with_private_observation_store_for_test(
            Arc::clone(&fixture.tools),
            profile,
            "f".repeat(64),
            || fixture.api.prepare_leadership_review(&context.binding),
        );
        assert_eq!(
            denied.unwrap_err(),
            "leadership private observation unavailable"
        );
        // Nested denial restores the actual authorized store/profile scope.
        assert_eq!(
            fixture
                .api
                .prepare_leadership_review(&context.binding)
                .unwrap(),
            context
        );
    });

    let healthy_adapter = fixture.api.workbench.clone().unwrap();
    let mut revoked = fixture.api.authority.as_ref().unwrap().as_ref().clone();
    assert!(Arc::make_mut(&mut revoked.agent_capabilities)
        .get_mut(&before.grant.authority.agent_id)
        .unwrap()
        .remove(sentinel_common::WORKBENCH_RETAIN_OBSERVATION));
    fixture.api.workbench = Some(Arc::new(WorkbenchExecutionAdapter {
        store: Arc::clone(&fixture.api.store),
        authority: Arc::new(revoked),
    }));
    // The review head still validates, but the adapter's current capability
    // intersection no longer authorizes reading the retained output.
    assert_eq!(
        fixture
            .with_workbench_store(|| fixture.api.prepare_leadership_review(&context.binding))
            .unwrap_err(),
        "leadership private observation unavailable"
    );
    fixture.api.workbench = Some(healthy_adapter);
    assert_eq!(
        fixture
            .with_workbench_store(|| fixture.api.prepare_leadership_review(&context.binding))
            .unwrap(),
        context
    );
    assert_eq!(fixture.read(), before);
    assert_eq!(fixture.project(), project);
    assert_eq!(fixture.calls(), calls);
}

#[test]
fn exact_budget_envelope_rejects_root_current_history_clock_and_exhaustion_changes() {
    let mut fixture = Fixture::continued();
    fixture.exhaust_calls();
    let now = now_unix_ms();
    let context = fixture.issue(now);
    for field in 0..7 {
        let mut invalid = context.clone();
        let Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }) =
            invalid.binding.grant.subject.as_mut()
        else {
            panic!("budget subject")
        };
        match field {
            0 => budget.root_allowance.allowance_id.push_str("-foreign"),
            1 => budget.root_allowance.grant.max_calls -= 1,
            2 => budget.active_allowance_digest = "f".repeat(64),
            3 => budget.continuation_history_digest = "f".repeat(64),
            4 => budget.observed_at_ms = context.source.source_session.updated_at_ms - 1,
            5 => budget.model_calls_exhausted = !budget.model_calls_exhausted,
            _ => budget.deadline_expired = !budget.deadline_expired,
        }
        assert!(
            invalid.validate_dispatch(now).is_err(),
            "unbound envelope field {field}"
        );
    }
    let before = fixture.read();
    let calls = fixture.calls();
    let mut invalid = context.clone();
    invalid.source.source_session.model_calls -= 1;
    assert!(invalid.validate_dispatch(now).is_err());
    assert_eq!(fixture.read(), before);
    assert_eq!(fixture.calls(), calls);
}

#[test]
fn budget_evidence_cannot_omit_required_refs_or_invent_model_and_tool_evidence() {
    let mut fixture = Fixture::continued();
    fixture.exhaust_calls();
    let now = now_unix_ms();
    let context = fixture.issue(now);
    for prefix in [
        "adaptive-budget-root:",
        "adaptive-budget-current:",
        "adaptive-budget-history:",
    ] {
        let mut invalid = context.clone();
        invalid
            .source
            .evidence_refs
            .retain(|reference| !reference.starts_with(prefix));
        assert!(invalid.validate_dispatch(now).is_err());
        assert!(invalid.prompt().is_err());
    }
    for reference in [
        "adaptive-budget-current:invented",
        "adaptive-model-result:invented",
        "workbench-observation:invented",
    ] {
        let mut invalid = context.clone();
        invalid.source.evidence_refs.push(reference.into());
        assert!(invalid.validate_dispatch(now).is_err());
        assert!(invalid.prompt().is_err());
    }
}

#[test]
fn authentic_private_observation_cannot_overflow_total_leadership_context() {
    let mut fixture = Fixture::continued();
    while fixture.session.model_calls + 1 < fixture.session.active_model_ceiling() {
        let now = now_unix_ms();
        let request = fixture.claim_tool(now);
        fixture.observe_tool(&request, now, now);
    }
    let now = now_unix_ms();
    let request = fixture.claim_tool(now);
    fixture.observe_tool_content(&request, now, now, &"x".repeat(128 * 1024));
    assert!(reconcile_review_at(&fixture.api, &fixture.project(), now));
    let call = fixture.calls().into_iter().find(is_budget).unwrap();
    // The observation has a real retained digest and the public source validates;
    // production preparation must reject the oversized combined private context.
    call.context.validate(&call.grant).unwrap();
    let observation = fixture.private_observation(&request.invocation_id);
    observation
        .validate(&request.invocation_id, &request.input_digest)
        .unwrap();
    assert!(
        serde_json::to_vec(&observation).unwrap().len()
            > sentinel_workflow::ADAPTIVE_LEADERSHIP_MAX_CONTEXT_BYTES
    );
    let before = fixture.read();
    let calls = fixture.calls();
    let binding = LeadershipAuthority::from_call(&call);
    assert_eq!(
        fixture
            .with_workbench_store(|| fixture.api.prepare_leadership_review(&binding))
            .unwrap_err(),
        "private leadership context exceeds bound"
    );
    assert_eq!(fixture.read(), before);
    assert_eq!(fixture.calls(), calls);
}

#[test]
fn ready_head_before_exhaustion_and_backwards_clock_cannot_issue_budget_review() {
    let mut fixture = Fixture::continued();
    let now = now_unix_ms();
    let request = fixture.claim_tool(now);
    fixture.observe_tool(&request, now, now);
    let before = fixture.read();
    reconcile_review_at(&fixture.api, &fixture.project(), now);
    assert!(fixture.calls().iter().all(|call| !is_budget(call)));
    assert_eq!(fixture.read(), before);
    fixture.exhaust_calls();
    let before = fixture.read();
    reconcile_review_at(&fixture.api, &fixture.project(), before.updated_at_ms - 1);
    assert!(fixture.calls().iter().all(|call| !is_budget(call)));
    assert_eq!(fixture.read(), before);
}

#[test]
fn model_continue_cannot_exceed_immutable_root_remaining_budget() {
    let mut fixture = Fixture::continued();
    fixture.exhaust_calls();
    let context = fixture.issue(now_unix_ms());
    let before = fixture.read();
    let project = fixture.project();
    let (id, digest) = dispatch(&fixture, &context);
    let remaining = before.grant.max_model_calls - before.model_calls;
    let result = completion(&context, 3, "continue", Some((remaining + 1, 30_000)));
    persist(&fixture.api, &result, &context, &id, &digest, true);
    assert!(fixture
        .api
        .accept_leadership_review(&result, &context, &id, &digest)
        .is_err());
    assert_eq!(fixture.read(), before);
    assert_eq!(fixture.project(), project);
    let call = fixture.calls().into_iter().find(is_budget).unwrap();
    assert!(call.decision.is_none());
    assert!(call.continuation.is_none());
}

#[test]
fn forbidden_subscription_replacement_is_read_only_and_project_change_retires_pending_review() {
    let mut fixture = Fixture::continued();
    fixture.exhaust_calls();
    let context = fixture.issue(now_unix_ms());
    let (id, digest) = dispatch(&fixture, &context);
    let result = completion(&context, 3, "continue", Some((2, 30_000)));
    persist(&fixture.api, &result, &context, &id, &digest, true);
    let before = fixture.read();
    let project = fixture.project();
    let calls = fixture.calls();
    let events = fixture.api.event_store.as_ref().unwrap();
    let queued = events.get_llm_completion(&id).unwrap().unwrap();
    let company_counts = || {
        let connection =
            sentinel_limbo::rusqlite::Connection::open(fixture.temp.path().join("company.sqlite"))
                .unwrap();
        connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM company_entities),
                    (SELECT COUNT(*) FROM company_events),
                    (SELECT COUNT(*) FROM company_operations)",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .unwrap()
    };
    let counts = company_counts();
    let mut replacement: sentinel_workflow::SubscriptionCallGrantV1 =
        project.subscription_call.as_ref().unwrap().grant.clone();
    replacement.max_calls = 1;
    replacement.max_duration_ms = 120_000;
    replacement.expires_at_unix_ms = now_unix_ms() + 120_000;
    let denied = fixture
        .api
        .store
        .apply_company_command(
            &fixture.api.principals.principal("pm").unwrap().principal,
            Uuid::new_v4(),
            &CompanyWorkflowCommandV1::GrantSubscriptionCall {
                project_id: project.project_id.clone(),
                expected_version: project.version,
                grant: replacement,
            },
            now_unix_ms(),
        )
        .unwrap_err();
    assert_eq!(denied.message, "subscription call authority unavailable");
    assert_eq!(company_counts(), counts);
    assert_eq!(fixture.project(), project);
    assert_eq!(fixture.read(), before);
    assert_eq!(fixture.calls(), calls);
    assert_eq!(events.get_llm_completion(&id).unwrap().unwrap(), queued);

    // Change the exact project source through a valid typed command, rather
    // than inventing permission to replace an active governed allowance.
    let response = fixture
        .api
        .store
        .apply_company_command(
            &fixture.api.principals.principal("pm").unwrap().principal,
            Uuid::new_v4(),
            &CompanyWorkflowCommandV1::RecordDecision {
                project_id: project.project_id.clone(),
                expected_version: project.version,
                work_item_id: None,
                choice_ref: "Independent project decision".into(),
                rationale_ref: "No subscription, assignment or governance change".into(),
            },
            now_unix_ms(),
        )
        .unwrap();
    let CompanyWorkflowResponseV1::Project(changed) = response.response else {
        panic!("project decision response");
    };
    let changed_project = fixture.project();
    assert_eq!(changed_project, *changed);
    assert_eq!(changed_project.version, project.version + 1);
    assert_eq!(changed_project.decisions.len(), project.decisions.len() + 1);
    assert_eq!(changed_project.subscription_call, project.subscription_call);
    assert_eq!(changed_project.governance, project.governance);
    assert_eq!(changed_project.work_items, project.work_items);
    assert_eq!(fixture.read(), before);
    assert_eq!(
        fixture
            .api
            .prepare_leadership_review(&context.binding)
            .unwrap_err(),
        "leadership project changed"
    );
    fixture
        .api
        .accept_leadership_review(&result, &context, &id, &digest)
        .unwrap();
    assert_eq!(fixture.read(), before);
    assert_eq!(fixture.project(), changed_project);
    let call = fixture.calls().into_iter().find(is_budget).unwrap();
    assert!(call.decision.is_none());
    assert!(call.continuation.is_none());
    assert!(call.retired_at_unix_ms.is_some());
    assert!(call.model_response_digest.is_none());
    assert!(call.resolution_event_id.is_none());
    let failed = events.get_llm_completion(&id).unwrap().unwrap();
    assert_eq!(failed.status, "failed");
    assert_eq!(
        failed.last_error.as_deref(),
        Some("leadership_review_stale")
    );
    assert_eq!(failed.payload, queued.payload);
    let counts = company_counts();
    fixture
        .api
        .accept_leadership_review(&result, &context, &id, &digest)
        .unwrap();
    assert_eq!(company_counts(), counts);
    assert_eq!(fixture.calls().into_iter().find(is_budget).unwrap(), call);
    assert_eq!(fixture.read(), before);
    assert_eq!(fixture.project(), changed_project);
}

#[test]
fn normal_to_recovery_narrows_provider_duration_and_prepares_after_reopen() {
    let mut fixture = Fixture::root(true);
    let root = fixture.session.grant.clone();
    assert_eq!(root.max_call_duration_ms, 120_000);
    // The normal deadline-only subject is eligible before the first model call.
    // Historical clocks keep both windows genuinely expired without wall sleeps.
    let normal_at = fixture.session.active_deadline_ms();
    let normal = fixture.issue_without_observation_at(normal_at, 3);
    fixture.continue_without_observation_at(&normal, 3, 60_000, normal_at);
    assert_eq!(
        fixture
            .project()
            .subscription_call
            .as_ref()
            .unwrap()
            .grant
            .max_duration_ms,
        60_000
    );
    let effect = fixture.claim_model(normal_at + 1);
    fixture.advance(
        AdaptiveTransitionV1::ResolveModel {
            effect,
            result_digest: "b".repeat(64),
            decision: AdaptiveModelDecisionV1::Blocked {
                reason_code: "dependency_unavailable".into(),
            },
        },
        normal_at + 2,
    );
    let recovery_at = now_unix_ms();
    assert!(fixture.session.active_deadline_ms() < recovery_at);
    let recovery = fixture.issue_without_observation_at(recovery_at, 2);
    assert!(matches!(
        &recovery.binding.grant.subject,
        Some(AdaptiveLeadershipReviewSubjectV2::BlockedContinuation { .. })
    ));
    fixture.continue_without_observation_at(&recovery, 1, 120_000, recovery_at);
    assert_eq!(fixture.session.grant, root);
    assert_eq!(
        (fixture.session.model_calls, fixture.session.tool_calls),
        (1, 0)
    );
    let latest = fixture
        .session
        .continuation
        .as_ref()
        .unwrap()
        .authorizations
        .last()
        .unwrap();
    assert_eq!(latest.deadline_ms - latest.issued_at_ms, 120_000);
    // Recovery retains the preceding normal allowance's narrower duration,
    // even when the new recovery window itself is longer.
    fixture.assert_provider_duration_across_reopen(60_000);
}

#[test]
fn recovery_to_normal_restores_root_provider_duration_after_reopen() {
    let mut fixture = Fixture::continued();
    let root = fixture.session.grant.clone();
    assert_eq!(root.max_call_duration_ms, 120_000);
    assert_eq!(
        fixture
            .project()
            .subscription_call
            .as_ref()
            .unwrap()
            .grant
            .max_duration_ms,
        30_000
    );
    fixture.exhaust_calls();
    let before = fixture.read();
    let context = fixture.issue(now_unix_ms());
    let (id, digest) = dispatch(&fixture, &context);
    let result = completion(&context, 3, "continue", Some((2, 120_000)));
    persist(&fixture.api, &result, &context, &id, &digest, true);
    fixture
        .api
        .accept_leadership_review(&result, &context, &id, &digest)
        .unwrap();
    fixture.session = fixture.read();
    assert_eq!(fixture.session.grant, root);
    assert_eq!(
        (fixture.session.model_calls, fixture.session.tool_calls),
        (before.model_calls, before.tool_calls)
    );
    assert_eq!(fixture.session.last_observation, before.last_observation);
    fixture.assert_provider_duration_across_reopen(120_000);
}
