// Scripted persisted evidence, never live process, provider or customer acceptance.
// Include as a child of model_execution::family_selection_tests::retained_failure_tests.
use super::*;
use crate::workbench::with_terminal_workbench_service_for_test;
use crate::workflow_api::model_review;
use sentinel_workflow::{PendingGateEvidenceV1, SealedArtifactEvidenceV1, SealedOutputEvidenceV1};
use std::os::unix::fs::PermissionsExt;

struct PackageEvidence {
    outputs: Vec<SealedOutputEvidenceV1>,
    artifacts: Vec<SealedArtifactEvidenceV1>,
    final_invocation: Uuid,
}

impl WorkExecutionPort for PackageEvidence {
    fn readiness(&self) -> DependencyReadiness { DependencyReadiness::Ready }
    fn reconcile(&self, _: &PendingExecutionV1) -> Result<WorkExecutionObservation, WorkflowPortError> {
        Ok(WorkExecutionObservation::Succeeded)
    }
}

impl CompletionEvidencePort for PackageEvidence {
    fn readiness(&self) -> DependencyReadiness { DependencyReadiness::Ready }
    fn terminal_evidence(&self, pending: &PendingCompletionEvidenceV1)
        -> Result<Box<dyn TerminalExecutionEvidence>, WorkflowPortError>
    {
        let artifacts = if pending.invocation_id == self.final_invocation {
            self.artifacts.clone()
        } else {
            Vec::new()
        };
        Ok(Box::new(WorkbenchCompletionReceipt {
            receipt_id: format!("scripted-package-{}", pending.invocation_id),
            invocation_id: pending.invocation_id, plan_digest: pending.plan_digest.clone(),
            step_digest: pending.step_digest.clone(),
            output_bundle_digest: sentinel_workflow::sealed_output_bundle_digest(&self.outputs, &artifacts).unwrap(),
            outputs: self.outputs.clone(), artifacts,
            completed_at_unix_ms: pending.created_at_unix_ms,
        }))
    }
}

struct ScriptedGate(PendingGateEvidenceV1);

impl sentinel_workflow::IndependentGateEvidence for ScriptedGate {
    fn schema_version(&self) -> u16 { WORKFLOW_SCHEMA_VERSION }
    fn receipt_id(&self) -> &str { "scripted-independent-gate" }
    fn profile_id(&self) -> &str { &self.0.expectation.profile_id }
    fn profile_generation(&self) -> u64 { self.0.expectation.profile_generation }
    fn profile_digest(&self) -> &str { &self.0.expectation.profile_digest }
    fn subject_digest(&self) -> &str { &self.0.subject_digest }
    fn required_checks_digest(&self) -> &str { &self.0.required_checks_digest }
    fn passed(&self) -> bool { true }
    fn completed_at_unix_ms(&self) -> u64 { self.0.created_at_unix_ms }
}

impl GateEvidencePort for PackageEvidence {
    fn readiness(&self) -> DependencyReadiness { DependencyReadiness::Ready }
    fn gate_evidence(&self, pending: &PendingGateEvidenceV1)
        -> Result<Box<dyn sentinel_workflow::IndependentGateEvidence>, WorkflowPortError>
    {
        Ok(Box::new(ScriptedGate(pending.clone())))
    }
}

fn planned_fixture(successor: bool) -> (FamilyFixture, ProjectPlanningContext) {
    planned_fixture_with_initial_calls(successor, None)
}

fn planned_fixture_with_initial_calls(successor: bool, calls: Option<u16>) -> (FamilyFixture, ProjectPlanningContext) {
    let temp = tempfile::tempdir().unwrap();
    let (mut api, sales) = crate::workflow_api::model_execution::tests::fixture(
        &temp.path().join("company.sqlite"),
    );
    let planning = accepted_context(&api, &sales, "python-project-v1");
    let tasks = if successor {
        vec![serde_json::json!({
            "key":"design", "title":"Design program", "objective":"Specify accepted program",
            "role":"designer", "depends_on":[],
        }), serde_json::json!({
            "key":"implement", "title":"Implement program", "objective":"Implement accepted design",
            "role":"developer", "depends_on":["design"],
        })]
    } else {
        vec![serde_json::json!({
            "key":"implement", "title":"Implement program", "objective":"Implement accepted program",
            "role":"developer", "depends_on":[],
        })]
    };
    let decision: ProjectPlanningDecision = serde_json::from_value(serde_json::json!({
        "schema_version":1, "rationale":"Scripted handoff composition", "tasks":tasks,
    })).unwrap();
    let planner = &planning.binding.grant.planner_principal;
    let planning_call = api.store.project_planning_call(
        &planner.tenant_id, &planning.source_project.project_id,
    ).unwrap().unwrap();
    api.store.claim_project_planning_call(planner, &sentinel_workflow::ClaimProjectPlanningCallV1 {
        project_id: planning.source_project.project_id.clone(),
        allowance_id: planning_call.allowance_id.clone(), request_id: planning_call.request_id(),
        request_digest: "a".repeat(64), context_digest: "b".repeat(64),
    }, now_unix_ms()).unwrap();
    let apply = |command| {
        let outcome = api.core.apply_company_command(
            planner, Uuid::new_v4(), &command, now_unix_ms(),
        ).unwrap();
        let CompanyWorkflowResponseV1::Project(project) = outcome.response else {
            panic!("fixture project transition")
        };
        *project
    };
    let planned = apply(CompanyWorkflowCommandV1::PlanWorkGraph {
        project_id: planning.source_project.project_id.clone(),
        expected_version: planning.source_project.version,
        items: api.bind_project_plan(&planning, &decision).unwrap(),
    });
    let active = apply(CompanyWorkflowCommandV1::ActivateProject {
        project_id: planned.project_id.clone(), expected_version: planned.version,
        reason_ref: "scripted composition".into(),
    });
    let assigned = api.assign_ready_model_work(planner, active, "handoff-composition").unwrap();
    let project = if let Some(max_calls) = calls {
        let work = assigned.work_items.values().find(|work| work.state == CompanyWorkStateV1::Assigned).unwrap();
        let assignment = work.assignments.iter().find(|assignment| assignment.active).unwrap();
        apply(CompanyWorkflowCommandV1::GrantSubscriptionCall {
            project_id: assigned.project_id.clone(), expected_version: assigned.version,
            grant: sentinel_workflow::SubscriptionCallGrantV1 {
                schema_version: 1, work_item_id: work.spec.work_item_id.clone(),
                assignment_id: assignment.assignment_id.clone(), assignment_version: assignment.assignment_version,
                agent_id: assignment.agent_id, provider: planning.binding.grant.provider.clone(),
                model: planning.binding.grant.model.clone(), catalog_digest: planning.binding.grant.catalog_digest.clone(),
                max_calls, max_concurrent: 1, max_duration_ms: 120_000,
                token_policy: sentinel_workflow::SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,
                expires_at_unix_ms: now_unix_ms() + 300_000,
            },
        })
    } else {
        api.grant_initial_model_work(
            planner, assigned, &planning.binding.grant, "handoff-composition",
        ).unwrap()
    };
    let allowance = project.subscription_call.as_ref().unwrap();
    api.store.complete_project_planning_call(
        planner, &project.project_id, &planning_call.allowance_id,
        &"a".repeat(64), &"c".repeat(64), &project, now_unix_ms(),
    ).unwrap();
    assert_eq!(allowance.grant.max_calls, calls.unwrap_or(16));
    let binding = api.adaptive_provider_authority_for_claim(allowance.grant.agent_id)
        .unwrap().unwrap();
    let session = api.store.adaptive_session(binding.grant.session_id, &binding.grant.authority)
        .unwrap().unwrap();

    let agent_root = temp.path().join("developer");
    let artifacts = agent_root.join("artifacts");
    fs::create_dir_all(&artifacts).unwrap();
    fs::write(agent_root.join(".nano-runtime"), format!("AGENT-{:02}", session.grant.authority.agent_id.0)).unwrap();
    let qa_agent = project.governance.participants.iter()
        .find(|participant| participant.role == CompanyRoleV1::Qa).unwrap().agent_id;
    let qa_root = temp.path().join("qa");
    fs::create_dir_all(qa_root.join("artifacts")).unwrap();
    fs::write(qa_root.join(".nano-runtime"), format!("AGENT-{:02}", qa_agent.0)).unwrap();
    let roots = HashMap::from([
        (session.grant.authority.agent_id, artifacts),
        (qa_agent, qa_root.join("artifacts")),
    ]);
    let mut authority = api.authority.as_ref().unwrap().as_ref().clone();
    authority.artifact_roots = Arc::new(roots.clone());
    let review = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/workbench-profiles/web-review-v1.toml"));
    authority.review_profile = Some((toml::from_str(review).unwrap(), hex_sha256(review.as_bytes())));
    let authority = Arc::new(authority);
    let adapter = Arc::new(WorkbenchExecutionAdapter {
        store: Arc::clone(&api.store), authority: Arc::clone(&authority),
    });
    api.core = Arc::new(WorkflowCore::new(
        Arc::clone(&api.store), Arc::clone(&authority), Arc::clone(&adapter), adapter.clone(),
        Arc::new(UnavailableGateEvidencePort),
    ));
    api.workbench = Some(adapter);
    api.authority = Some(authority.clone());
    let tools = Arc::new(WorkbenchInvocationStore::open_with_artifact_roots(
        temp.path().join("workbench.redb"), roots,
    ).unwrap());
    let delivery = temp.path().join("delivery");
    fs::create_dir(&delivery).unwrap();
    fs::set_permissions(&delivery, fs::Permissions::from_mode(0o700)).unwrap();
    let qa = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/workbench-profiles/web-qa-v1.toml"));
    let events = api.event_store.as_ref().unwrap().clone();
    api.delivery = Some(Arc::new(ConfiguredDeliveryCore::open(
        &DeliveryStoreConfigV1::new(delivery, "delivery.redb").unwrap(),
        WorkflowDeliveryIntegration::new(
            Arc::clone(&api.store), Arc::clone(&api.principals), toml::from_str(qa).unwrap(),
            hex_sha256(qa.as_bytes()), Arc::clone(&authority.agent_capabilities),
            Arc::clone(&authority.artifact_roots), authority.coding_profiles.get("coding-qa-v1").cloned(),
            authority.project_profiles.clone(),
        ),
        LimboDeliveryEffects::new(events.clone(), Arc::clone(&api.store), Arc::clone(&api.principals)),
        LimboDeliveryPublication::new(events),
    ).unwrap()));
    (FamilyFixture { api, session, tools, _temp: temp }, planning)
}

fn project(f: &FamilyFixture) -> sentinel_workflow::ProjectV1 {
    f.api.store.company_project(&f.session.grant.authority.tenant_id, &f.session.grant.authority.project_id)
        .unwrap().unwrap()
}

fn complete_source(f: &mut FamilyFixture, resolve_completion: bool) -> sentinel_workflow::WorkItemExecutionV1 {
    let task = project(f).work_items[&f.session.grant.authority.work_item_id].spec.clone();
    let (artifact_kind, path) = match task.required_role {
        CompanyRoleV1::Designer => ("design_specification", "design.md"),
        CompanyRoleV1::Developer => ("source_tree", "main.py"),
        _ => panic!("fixture source role"),
    };
    let media_type = task.outputs[0].media_type.clone();
    f.inspect(path, "scripted accepted source\n");
    let now = now_unix_ms();
    let model = AdaptiveEffectV1 { id: Uuid::new_v4(), request_digest: "b".repeat(64) };
    f.advance(AdaptiveTransitionV1::ClaimModel {
        effect: model.clone(),
        previous_observation_digest: f.session.last_observation.as_ref().map(|o| o.observation_digest.clone()),
    }, now);
    let tool = WorkbenchTool::PackageArtifact {
        artifact_kind: artifact_kind.into(), media_type: media_type.clone(), paths: vec![path.into()],
    };
    let tool_digest = adaptive_tool_digest(&tool).unwrap();
    f.advance(AdaptiveTransitionV1::ResolveModel {
        effect: model, result_digest: "c".repeat(64),
        decision: AdaptiveModelDecisionV1::Tool { tool: tool.clone(), tool_digest: tool_digest.clone() },
    }, now);
    let adapter = f.api.workbench.as_ref().unwrap();
    let effect = adapter.adaptive_tool_effect(&f.session, &tool).unwrap();
    let request = adapter.build_adaptive_request(&f.session, &effect, &tool).unwrap();
    f.advance(AdaptiveTransitionV1::ClaimTool { effect: effect.clone(), tool_digest }, now);

    let root = &f.api.authority.as_ref().unwrap().artifact_roots[&request.agent_id];
    let scope = root.join(&request.project_id).join(&request.work_item_id);
    fs::create_dir_all(scope.join("blobs")).unwrap();
    let blob = b"print(42)\n";
    let blob_digest = hex_sha256(blob);
    let blob_path = scope.join("blobs").join(&blob_digest);
    if !blob_path.exists() {
        fs::write(&blob_path, blob).unwrap();
        fs::set_permissions(&blob_path, fs::Permissions::from_mode(0o444)).unwrap();
    }
    assert_eq!(fs::read(&blob_path).unwrap().as_slice(), blob.as_slice());
    let manifest = serde_json::to_vec(&serde_json::json!({
        "schema_version":WORKBENCH_SCHEMA_VERSION, "invocation_id":request.invocation_id,
        "input_digest":request.input_digest, "project_id":request.project_id,
        "work_item_id":request.work_item_id, "workspace_id":request.workspace_id,
        "agent_id":request.agent_id.0, "artifact_kind":artifact_kind, "media_type":media_type,
        "runtime_key":request.runtime_key, "tool_profile":request.tool_profile,
        "tool_profile_digest":request.tool_profile_digest, "policy_digest":request.policy_digest,
        "entries":[{"path":path, "blob_id":format!("sha256:{blob_digest}"),
            "sha256":blob_digest, "size_bytes":blob.len()}],
    })).unwrap();
    let digest = hex_sha256(&manifest);
    fs::write(scope.join(format!("{digest}.manifest.json")), &manifest).unwrap();
    f.tools.reserve(&request, now).unwrap();
    f.tools.mark_executing(&request.invocation_id, &request.input_digest, now).unwrap();
    f.tools.accept_result(&WorkbenchMessage::Result {
        schema_version: WORKBENCH_SCHEMA_VERSION, invocation_id: request.invocation_id.clone(),
        input_digest: request.input_digest.clone(), outcome: WorkbenchOutcome::Succeeded,
        resources: sentinel_common::WorkbenchResourceUsage::default(), output: BTreeMap::new(), error: None,
        artifacts: vec![sentinel_common::WorkbenchArtifactRef {
            artifact_id: format!("sha256:{digest}"), sha256: digest.clone(), artifact_kind: artifact_kind.into(),
            media_type: media_type.clone(), size_bytes: blob.len() as u64, manifest_path: format!("{digest}.manifest.json"),
        }],
    }, now).unwrap();
    let authority = f.api.authority.as_ref().unwrap();
    let (profile, profile_digest) = authority.profile_for_binding(&request.tool_profile).unwrap();
    let observation = WorkbenchCoordinator::new(&f.tools, profile, profile_digest)
        .private_observation(&request.invocation_id, authority.as_ref()).unwrap().unwrap();
    f.advance(AdaptiveTransitionV1::ObserveTool {
        observation: AdaptiveObservationRefV1 { effect, observation_digest: observation.digest().into() },
    }, now);
    let context = f.context();
    let completion = ModelExecutionCompletion {
        content: serde_json::json!({"schema_version":1, "decision":{
            "kind":"propose_completion", "artifact_digest":digest,
        }}).to_string(),
        context: ModelExecutionContext::Adaptive(Box::new(context.clone())), admissible: true,
    };
    let request_id = context.binding.request_id();
    let request_digest = "d".repeat(64);
    let model_response_digest = hex_sha256(completion.content.as_bytes());
    let completion_effect = AdaptiveEffectV1 { id: context.binding.effect_id, request_digest: request_digest.clone() };
    f.advance(AdaptiveTransitionV1::ClaimModel {
        effect: completion_effect.clone(),
        previous_observation_digest: f.session.last_observation.as_ref().map(|o| o.observation_digest.clone()),
    }, now_unix_ms());
    let plan_id = stable_operation_id(
        "sentinel.workflow.adaptive-completion.v1", &format!("{request_id}:{request_digest}"), 1,
    );
    let authority = f.api.authority.as_ref().unwrap();
    let (profile, profile_digest) = authority.profile_for_binding(&request.tool_profile).unwrap();
    with_terminal_workbench_service_for_test(
        Arc::clone(&f.tools), profile.clone(), profile_digest.into(), || {
            if context.correction.is_some() {
                let state = crate::workflow_api::adaptive_leadership_review::tests::discovery_state(
                    &f._temp.path().join("company.sqlite"), &f._temp.path().join("company.events.sqlite"),
                );
                let previous = f.api.store.work_item(
                    &context.binding.grant.authority.tenant_id,
                    &context.binding.grant.authority.project_id, &context.task.work_item_id,
                ).unwrap();
                for mutation in ["revision", "feedback", "tools", "missing"] {
                    let mut changed = context.clone();
                    match mutation {
                        "revision" => changed.correction.as_mut().unwrap().revision.previous_plan_id = Uuid::new_v4(),
                        "feedback" => changed.correction.as_mut().unwrap().feedback_ref = "unbound-feedback".into(),
                        "tools" => changed.correction.as_mut().unwrap().previous_tools.clear(),
                        "missing" => changed.correction = None,
                        _ => unreachable!(),
                    }
                    assert!(f.api.admit_adaptive_completion(
                        &f.session, &changed, &digest, &request_id, &request_digest,
                    ).is_err(), "{mutation}");
                    assert_eq!(f.api.store.work_item(
                        &context.binding.grant.authority.tenant_id,
                        &context.binding.grant.authority.project_id, &context.task.work_item_id,
                    ).unwrap(), previous);
                    assert_eq!(crate::workflow_api::adaptive_leadership_review::tests::discovery_state(
                        &f._temp.path().join("company.sqlite"), &f._temp.path().join("company.events.sqlite"),
                    ), state, "{mutation}");
                    assert!(f.api.store.pending_executions(10).unwrap().is_empty());
                }
            }
            f.api.admit_adaptive_completion(&f.session, &context, &digest, &request_id, &request_digest).unwrap();
            let state = crate::workflow_api::adaptive_leadership_review::tests::discovery_state(
                &f._temp.path().join("company.sqlite"), &f._temp.path().join("company.events.sqlite"),
            );
            let pending = f.api.store.pending_executions(10).unwrap();
            assert_eq!(pending.len(), 1);
            f.api.admit_adaptive_completion(&f.session, &context, &digest, &request_id, &request_digest).unwrap();
            assert_eq!(f.api.store.pending_executions(10).unwrap(), pending);
            if let Some(correction) = &context.correction {
                let mut changed = context.clone();
                changed.correction.as_mut().unwrap().revision.previous_state_digest = "f".repeat(64);
                assert_ne!(changed.correction.as_ref().unwrap().revision, correction.revision);
                assert!(f.api.admit_adaptive_completion(
                    &f.session, &changed, &digest, &request_id, &request_digest,
                ).is_err());
                assert_eq!(f.api.store.pending_executions(10).unwrap(), pending);
            }
            assert_eq!(crate::workflow_api::adaptive_leadership_review::tests::discovery_state(
                &f._temp.path().join("company.sqlite"), &f._temp.path().join("company.events.sqlite"),
            ), state);
        },
    );
    let admitted = f.api.store.work_item(
        &context.binding.grant.authority.tenant_id,
        &context.binding.grant.authority.project_id, &context.task.work_item_id,
    ).unwrap().unwrap();
    assert_eq!(admitted.plan.plan_id, plan_id);
    if resolve_completion {
        f.advance(AdaptiveTransitionV1::ResolveModel {
            effect: completion_effect, result_digest: model_response_digest.clone(),
            decision: AdaptiveModelDecisionV1::ProposeCompletion { artifact_digest: digest.clone() },
        }, now_unix_ms());
    }
    let outputs = admitted.plan.steps[0].outputs.iter().map(|o| SealedOutputEvidenceV1 {
        name: o.name.clone(), kind: o.kind.clone(), digest_algorithm: o.digest_algorithm.clone(), digest: digest.clone(),
    }).collect::<Vec<_>>();
    let artifacts = vec![SealedArtifactEvidenceV1 {
        artifact_kind: artifact_kind.into(), media_type, paths: vec![path.into()], digest,
    }];
    let evidence = Arc::new(PackageEvidence {
        outputs, artifacts, final_invocation: admitted.plan.steps[0].invocation_id,
    });
    let core = WorkflowCore::new(
        Arc::clone(&f.api.store), Arc::clone(f.api.authority.as_ref().unwrap()),
        Arc::clone(&evidence), Arc::clone(&evidence), evidence,
    );
    let pending = f.api.store.pending_executions(1).unwrap().remove(0);
    let started = core.reconcile_execution(&pending, now_unix_ms()).unwrap();
    f.api.sync_company_state(&started).unwrap();
    let pending = f.api.store.pending_completion_evidence(1).unwrap().remove(0);
    let reviewed = core.reconcile_completion_evidence(&pending, now_unix_ms()).unwrap();
    f.api.sync_company_state(&reviewed).unwrap();
    let pending = f.api.store.pending_gate_evidence(1).unwrap().remove(0);
    let done = core.reconcile_gate_evidence(&pending, now_unix_ms()).unwrap();
    f.api.sync_company_state(&done).unwrap();

    let binding = &context.binding;
    let grant = &binding.grant;
    let usage = DomainEvent::new("agent_llm_usage", &grant.authority.agent_id.to_string(),
        &serde_json::json!({"type":"AgentLlmUsage", "agent_id":grant.authority.agent_id,
            "tenant_id":grant.authority.tenant_id, "project_id":grant.authority.project_id,
            "work_item_id":grant.authority.work_item_id, "reservation_id":grant.provider_allowance_id,
            "assignment_id":binding.assignment_id, "assignment_version":grant.authority.assignment_version,
            "provider":grant.provider, "requested_model":grant.model, "effective_model":grant.model,
            "caller_role":"agent_runtime", "tier":"mid", "hierarchy_tier":2, "cost_source":"provider_reported",
            "input_tokens":5, "output_tokens":5, "cache_read":0, "cache_creation":0, "cost_usd":0.0,
        }).to_string(), &request_id, 1,
    ).with_operation_id(&format!("llm_usage_{request_id}")).with_schema_version(3);
    let events = f.api.event_store.as_ref().unwrap();
    events.reserve_llm_request(&request_id, &request_digest, &grant.authority.agent_id.to_string()).unwrap();
    events.bind_llm_model_reservation(&sentinel_limbo::LlmModelReservationV1 {
        schema_version:1, request_id:request_id.clone(), request_digest:request_digest.clone(),
        owner_scope:sentinel_common::StateTransferScope::for_agent(grant.authority.agent_id.to_string()),
        subject:sentinel_limbo::LlmModelSubjectV1::Adaptive {
            session_id:grant.session_id, effect_id:binding.effect_id, session_version:binding.session_version,
        },
        allowance_id:grant.provider_allowance_id.clone(),
        context_digest:hex_sha256(&serde_json::to_vec(&completion.context).unwrap()),
        authority_digest:hex_sha256(&serde_json::to_vec(&ProviderExecutionAuthority::Adaptive(Box::new(binding.clone()))).unwrap()),
        usage_binding:sentinel_limbo::LlmModelUsageBindingV1 {
            agent_id:grant.authority.agent_id, tenant_id:grant.authority.tenant_id.0.clone(),
            project_id:grant.authority.project_id.0.clone(), work_item_id:grant.authority.work_item_id.0.clone(),
            reservation_id:grant.provider_allowance_id.clone(), assignment_id:binding.assignment_id.clone(),
            assignment_version:grant.authority.assignment_version, provider:grant.provider.clone(), model:grant.model.clone(),
        },
    }).unwrap();
    events.enqueue_llm_completion(&request_id, &request_digest, &serde_json::json!({
        "version":2, "request_id":request_id, "request_digest":request_digest,
        "usage_event":usage, "model_work":completion, "actions":[], "tokens_used":10,
        "model_response_digest":model_response_digest,
    }).to_string()).unwrap();
    events.persist_llm_completion_usage(&request_id, &request_digest, &usage).unwrap();
    assert!(events.claim_llm_completion_actions(&request_id, &request_digest).unwrap());
    done
}

#[test]
fn proved_adaptive_completion_composes_independent_one_shot_qa_and_rejects_arbitrary_done() {
    let (mut missing, _) = planned_fixture(false);
    complete_source(&mut missing, false);
    let unproved = project(&missing);
    assert!(model_review::setup_due(&unproved));
    assert!(missing.api.ensure_source_review(unproved.clone()).is_err());
    assert_eq!(project(&missing), unproved);

    let (mut f, _) = planned_fixture(false);
    let before = project(&f);
    let source_id = f.session.grant.authority.work_item_id.clone();
    let mut descriptor = before.clone();
    descriptor.lifecycle_state = sentinel_workflow::ProjectLifecycleStateV1::DeliveryCandidate;
    descriptor.work_items.get_mut(&source_id).unwrap().state = CompanyWorkStateV1::Done;
    assert!(model_review::setup_due(&descriptor));
    assert!(f.api.ensure_source_review(descriptor).is_err());
    assert_eq!(project(&f), before);
    let done = complete_source(&mut f, true);
    let completed = project(&f);
    let previous = completed.subscription_call.clone().unwrap();
    let proof = f.api.store.adaptive_source_completion(
        &completed.tenant_id, &completed.project_id, &previous, done.plan.plan_id,
    ).unwrap().unwrap();
    let source_completed = completed.work_items[&source_id].clone();
    assert_eq!(proof.provider_source.version + 2, f.session.version);
    assert!(model_review::setup_due(&completed));
    let authority = f.api.authority.as_ref().unwrap();
    let (profile, digest) = authority.profile_for_binding(&f.session.grant.authority.profile_id).unwrap();
    let handoff_started = now_unix_ms();
    with_terminal_workbench_service_for_test(Arc::clone(&f.tools), profile.clone(), digest.into(), || {
        f.api.ensure_source_review(completed).unwrap();
    });
    let handoff_finished = now_unix_ms();
    let reviewed = project(&f);
    assert_eq!(reviewed.source_review_previous_call.as_ref(), Some(&previous));
    let qa = reviewed.subscription_call.as_ref().unwrap();
    assert_eq!(qa.grant.max_calls, 1);
    assert_eq!(qa.grant.max_duration_ms, 120_000);
    assert_eq!(qa.grant.max_concurrent, 1);
    assert!(qa.grant.expires_at_unix_ms >= handoff_started + 300_000);
    assert!(qa.grant.expires_at_unix_ms <= handoff_finished + 300_000);
    assert_ne!(qa.grant.agent_id, previous.grant.agent_id);
    assert_ne!(qa.allowance_id, previous.allowance_id);
    assert!(qa.dispatch.is_none());
    let review = &reviewed.work_items[&qa.grant.work_item_id];
    assert_eq!(review.spec.required_role, CompanyRoleV1::Qa);
    assert_eq!(review.state, CompanyWorkStateV1::Assigned);
    assert!(review.spec.dependency_ids.contains(&source_id));
    assert_eq!(review.assignments[0].profile.profile_id, model_review::PROFILE_ID);
    assert_eq!(reviewed.work_items[&source_id], source_completed);
    assert!(!model_review::setup_due(&reviewed));
    let binding = select_provider_usage_binding(std::slice::from_ref(&reviewed), qa.grant.agent_id, Some(&qa.allowance_id))
        .unwrap().unwrap();
    assert!(f.api.prepare_model_work(&binding).unwrap().is_some());
    assert!(f.api.adaptive_provider_authority_for_claim(qa.grant.agent_id).unwrap().is_none());
}

#[test]
fn actual_successor_grant_authenticates_adaptive_completion_before_at_and_after_expiry() {
    for boundary in ["before", "at", "after", "missing-proof", "unfinished-before", "unfinished-at", "unfinished-after"] {
        let (mut f, planning) = planned_fixture(true);
        if !boundary.starts_with("unfinished") {
            complete_source(&mut f, boundary != "missing-proof");
            f.api.assign_ready_model_work(
                &planning.binding.grant.planner_principal, project(&f), "handoff-composition-next",
            ).unwrap();
        }
        let before = project(&f);
        let session_before = f.session.clone();
        let old = before.subscription_call.clone().unwrap();
        let now = match boundary {
            "before" | "unfinished-before" => now_unix_ms(),
            "at" | "unfinished-at" => old.grant.expires_at_unix_ms,
            _ => old.grant.expires_at_unix_ms + 1,
        };
        let outcome = f.api.grant_model_work_at(
            &planning.binding.grant.planner_principal, before.clone(), &planning.binding.grant,
            "handoff-composition-successor", now,
        );
        match boundary {
            "missing-proof" => {
                assert!(outcome.is_err());
                assert_eq!(project(&f), before);
            }
            "unfinished-before" | "unfinished-at" | "unfinished-after" => {
                assert_eq!(outcome.unwrap(), before);
                assert_eq!(project(&f), before);
            }
            _ => {
                let next = outcome.unwrap();
                assert_ne!(next.subscription_call.as_ref().unwrap().allowance_id, old.allowance_id);
                assert_ne!(next.subscription_call.as_ref().unwrap().grant.work_item_id, old.grant.work_item_id);
                assert_eq!(next.subscription_call.as_ref().unwrap().grant.max_calls, 16);
                assert_eq!(before.work_items[&old.grant.work_item_id].spec.required_role, CompanyRoleV1::Designer);
                assert_eq!(next.work_items[&next.subscription_call.as_ref().unwrap().grant.work_item_id].spec.required_role, CompanyRoleV1::Developer);
                assert_eq!(next.work_items, before.work_items);
            }
        }
        assert_eq!(f.api.store.adaptive_session(session_before.grant.session_id, &session_before.grant.authority)
            .unwrap().unwrap(), session_before);
    }
}

fn complete_negative_qa(f: &FamilyFixture) -> model_review::ValidatedProjectReview {
    let assigned = project(f);
    let allowance = assigned.subscription_call.as_ref().unwrap();
    let binding = select_provider_usage_binding(
        std::slice::from_ref(&assigned), allowance.grant.agent_id, Some(&allowance.allowance_id),
    ).unwrap().unwrap();
    let context = f.api.prepare_model_work(&binding).unwrap().unwrap();
    let source_files = model_review::source_inventory(&context).unwrap();
    let report = model_review::SourceReview {
        schema_version: 1,
        findings: vec![model_review::Finding {
            path: source_files[0].path.clone(), line: 1,
            reason: "The accepted program has no regression test for its printed result.".into(),
        }],
        source_files, verdict: model_review::Verdict::ChangesRequested,
    };
    let content = serde_json::to_string(&report).unwrap();
    let request_id = format!("company-provider-{}", allowance.allowance_id);
    let request_digest = "e".repeat(64);
    let qa = f.api.principals.principal(&context.authority.principal.principal_id).unwrap();
    f.api.core.apply_company_command(&qa.principal, Uuid::new_v4(),
        &CompanyWorkflowCommandV1::ClaimSubscriptionCall {
            project_id: assigned.project_id.clone(), expected_version: assigned.version,
            allowance_id: allowance.allowance_id.clone(), request_id: request_id.clone(),
            request_digest: request_digest.clone(),
        }, now_unix_ms(),
    ).unwrap();
    f.api.accept_model_work(&crate::workflow_api::model_work::ModelWorkCompletion {
        context: context.clone(), content: content.clone(), admissible: true,
    }, &request_id, &request_digest).unwrap();

    let execution = f.api.store.work_item(&assigned.tenant_id, &assigned.project_id, &context.task.work_item_id)
        .unwrap().unwrap();
    let adapter = Arc::clone(f.api.workbench.as_ref().unwrap());
    let scripted = Arc::new(PackageEvidence {
        outputs: Vec::new(), artifacts: Vec::new(),
        final_invocation: execution.plan.steps.last().unwrap().invocation_id,
    });
    let core = WorkflowCore::new(
        Arc::clone(&f.api.store), Arc::clone(f.api.authority.as_ref().unwrap()),
        scripted.clone(), adapter.clone(), scripted,
    );
    let authority = f.api.authority.as_ref().unwrap();
    let (profile, profile_digest) = authority.profile_for_binding(model_review::PROFILE_ID).unwrap();
    for _ in 0..2 {
        let pending = f.api.store.pending_executions(10).unwrap().into_iter()
            .find(|pending| pending.plan_id == execution.plan.plan_id).unwrap();
        let request = adapter.build_request(&pending).unwrap();
        let artifacts = if let WorkbenchTool::PackageArtifact { artifact_kind, media_type, paths } = &request.tool {
            assert_eq!(paths, &["review.json"]);
            let root = &authority.artifact_roots[&request.agent_id];
            let scope = root.join(&request.project_id).join(&request.work_item_id);
            fs::create_dir_all(scope.join("blobs")).unwrap();
            let blob_digest = hex_sha256(content.as_bytes());
            let blob_path = scope.join("blobs").join(&blob_digest);
            fs::write(&blob_path, content.as_bytes()).unwrap();
            fs::set_permissions(&blob_path, fs::Permissions::from_mode(0o444)).unwrap();
            let manifest = serde_json::to_vec(&serde_json::json!({
                "schema_version":WORKBENCH_SCHEMA_VERSION, "invocation_id":request.invocation_id,
                "input_digest":request.input_digest, "project_id":request.project_id,
                "work_item_id":request.work_item_id, "workspace_id":request.workspace_id,
                "agent_id":request.agent_id.0, "artifact_kind":artifact_kind, "media_type":media_type,
                "runtime_key":request.runtime_key, "tool_profile":request.tool_profile,
                "tool_profile_digest":request.tool_profile_digest, "policy_digest":request.policy_digest,
                "entries":[{"path":"review.json", "blob_id":format!("sha256:{blob_digest}"),
                    "sha256":blob_digest, "size_bytes":content.len()}],
            })).unwrap();
            let digest = hex_sha256(&manifest);
            fs::write(scope.join(format!("{digest}.manifest.json")), &manifest).unwrap();
            vec![sentinel_common::WorkbenchArtifactRef {
                artifact_id:format!("sha256:{digest}"), sha256:digest.clone(),
                artifact_kind:artifact_kind.clone(), media_type:media_type.clone(),
                size_bytes:content.len() as u64, manifest_path:format!("{digest}.manifest.json"),
            }]
        } else {
            let WorkbenchTool::WriteFile { path, content: written, .. } = &request.tool else {
                panic!("only the real confined QA report transport is admitted")
            };
            assert_eq!(path, "review.json");
            assert_eq!(written, &content);
            Vec::new()
        };
        let now = now_unix_ms();
        f.tools.reserve(&request, now).unwrap();
        f.tools.mark_executing(&request.invocation_id, &request.input_digest, now).unwrap();
        f.tools.accept_result(&WorkbenchMessage::Result {
            schema_version:WORKBENCH_SCHEMA_VERSION, invocation_id:request.invocation_id.clone(),
            input_digest:request.input_digest.clone(), outcome:WorkbenchOutcome::Succeeded,
            resources:sentinel_common::WorkbenchResourceUsage::default(), artifacts,
            output:BTreeMap::new(), error:None,
        }, now).unwrap();
        let started = core.reconcile_execution(&pending, now).unwrap();
        f.api.sync_company_state(&started).unwrap();
        let completion = f.api.store.pending_completion_evidence(10).unwrap().into_iter()
            .find(|pending| pending.plan_id == execution.plan.plan_id).unwrap();
        let reviewed = with_terminal_workbench_service_for_test(
            Arc::clone(&f.tools), profile.clone(), profile_digest.into(),
            || core.reconcile_completion_evidence(&completion, now_unix_ms()).unwrap(),
        );
        f.api.sync_company_state(&reviewed).unwrap();
    }
    let pending = f.api.store.pending_gate_evidence(10).unwrap().into_iter()
        .find(|pending| pending.plan_id == execution.plan.plan_id).unwrap();
    let done = core.reconcile_gate_evidence(&pending, now_unix_ms()).unwrap();
    f.api.sync_company_state(&done).unwrap();

    let grant = &allowance.grant;
    let usage = DomainEvent::new("agent_llm_usage", &grant.agent_id.to_string(),
        &serde_json::json!({"type":"AgentLlmUsage", "agent_id":grant.agent_id,
            "tenant_id":assigned.tenant_id, "project_id":assigned.project_id,
            "work_item_id":grant.work_item_id, "reservation_id":allowance.allowance_id,
            "assignment_id":grant.assignment_id, "assignment_version":grant.assignment_version,
            "provider":grant.provider, "requested_model":grant.model, "effective_model":grant.model,
            "caller_role":"agent_runtime", "tier":"mid", "hierarchy_tier":2, "cost_source":"provider_reported",
            "input_tokens":5, "output_tokens":5, "cache_read":0, "cache_creation":0, "cost_usd":0.0,
        }).to_string(), &request_id, 1,
    ).with_operation_id(&format!("llm_usage_{request_id}")).with_schema_version(3);
    let events = f.api.event_store.as_ref().unwrap();
    events.reserve_llm_request(&request_id, &request_digest, &grant.agent_id.to_string()).unwrap();
    events.enqueue_llm_completion(&request_id, &request_digest, &serde_json::json!({
        "version":2, "request_id":request_id, "request_digest":request_digest, "usage_event":usage,
        "model_work":ModelExecutionCompletion {
            context:ModelExecutionContext::Project(Box::new(context)), content, admissible:true,
        },
        "actions":[], "tokens_used":10,
    }).to_string()).unwrap();
    events.persist_llm_completion_usage(&request_id, &request_digest, &usage).unwrap();
    assert!(events.claim_llm_completion_actions(&request_id, &request_digest).unwrap());
    let completed = project(f);
    let candidates = f.api.model_artifact_inputs(&completed, &completed.work_items[&grant.work_item_id].spec)
        .unwrap().iter().map(|input|input.manifest_digest.clone()).collect();
    model_review::validated_project_review(&f.api, &completed, &qa.principal.principal_id, &candidates).unwrap()
}

#[test]
fn negative_independent_qa_starts_new_default_correction_and_admits_exact_revision() {
    negative_qa_correction(false);
}

#[test]
fn completed_funded_continuation_starts_a_new_correction_without_inheriting_its_epoch() {
    negative_qa_correction(true);
}

fn fund_and_continue(f: &mut FamilyFixture) {
    use crate::workflow_api::adaptive_leadership_review::{tests::{persist, reconcile_review_at}, LeadershipAuthority};
    use crate::workflow_api::adaptive_resume_policy::tests::claim_review;
    for _ in 0..4 {
        f.inspect("main.py", "scripted prior observation\n");
    }
    let now = now_unix_ms();
    let operator = f.api.principals.principal("operator").unwrap();
    let request = f.api.store.adaptive_work_funding_draft(
        &operator.principal, &f.session.grant.authority.project_id,
        f.session.grant.session_id, Uuid::new_v4(), "scripted-completed-campaign-regression",
        sentinel_workflow::AdaptiveWorkFundingLimitsV1 {
            additional_model_calls: 4, additional_tool_calls: 4,
            additional_reviews: 4, additional_windows: 4,
            max_window_ms: 300_000, max_call_duration_ms: 120_000,
            dispatch_margin_ms: sentinel_workflow::ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
            expires_at_unix_ms: now + 600_000,
        }, now,
    ).unwrap();
    let (_, receipt) = f.api.store.authorize_adaptive_work_funding(
        &operator.principal, &request, now,
    ).unwrap();
    assert!(reconcile_review_at(&f.api, &project(f), now_unix_ms()));
    let leader = f.api.principals.principal("pm").unwrap().principal.agent_id.unwrap();
    let call = f.api.leadership_review_for_agent(leader).unwrap().unwrap();
    let context = f.api.prepare_leadership_review(&LeadershipAuthority::from_call(&call)).unwrap();
    let (id, digest) = claim_review(&f.api, &context);
    let completion = ModelExecutionCompletion {
        context: ModelExecutionContext::AdaptiveLeadershipReview(Box::new(context.clone())),
        content: serde_json::json!({"schema_version":5,"decision":{
            "kind":"continue", "additional_model_calls":4, "window_ms":300_000,
            "rationale":"Scripted new funded source window", "evidence_refs":[context.source.evidence_refs[0]],
        }}).to_string(),
        admissible: true,
    };
    persist(&f.api, &completion, &context, &id, &digest, true);
    f.api.accept_leadership_review_at(&completion, &context, &id, &digest, now_unix_ms()).unwrap();
    f.session = f.api.store.adaptive_session_for_authority(&f.session.grant.authority)
        .unwrap().unwrap();
    assert_eq!(f.session.active_work_funding().unwrap().receipt, receipt);
}

fn negative_qa_correction(funded: bool) {
    let (mut f, _) = planned_fixture_with_initial_calls(false, Some(4));
    if funded {
        fund_and_continue(&mut f);
    }
    let original_done = complete_source(&mut f, true);
    let original = f.session.clone();
    assert_eq!(original.grant.max_model_calls, 4);
    assert_eq!(original.model_calls, if funded { 7 } else { 3 });
    let completed = project(&f);
    let original_allowance = completed.subscription_call.clone().unwrap();
    let authority = f.api.authority.as_ref().unwrap();
    let (profile, digest) = authority.profile_for_binding(&original.grant.authority.profile_id).unwrap();
    let profile = profile.clone();
    let digest = digest.to_owned();
    with_terminal_workbench_service_for_test(Arc::clone(&f.tools), profile.clone(), digest.clone(), || {
        f.api.ensure_source_review(completed).unwrap();
    });
    let review = complete_negative_qa(&f);
    assert_eq!(review.report.verdict, model_review::Verdict::ChangesRequested);
    model_review::raise_source_review_blocker(&f.api, &project(&f), &review).unwrap();
    let blocked = project(&f);
    let events = f.api.event_store.as_ref().unwrap().clone();
    let proof = f.api.store.adaptive_source_completion(
        &blocked.tenant_id, &blocked.project_id, &original_allowance, original_done.plan.plan_id,
    ).unwrap().unwrap();
    let original_usage = events.event_by_operation_id(&format!("llm_usage_company-adaptive-{}-{}",
        original.grant.session_id, proof.effect.id,
    )).unwrap().unwrap();
    let calls = f.api.store.adaptive_leadership_review_calls(&blocked.tenant_id, original.grant.session_id).unwrap();
    with_terminal_workbench_service_for_test(Arc::clone(&f.tools), profile, digest, || {
        model_review::restart_source_after_negative_review(&f.api, &blocked).unwrap();
    });
    let restarted = project(&f);
    let next = restarted.subscription_call.as_ref().unwrap();
    assert_eq!(next.grant.max_calls, 16);
    assert_eq!(next.grant.max_duration_ms, 120_000);
    assert_eq!(next.grant.assignment_id, original_allowance.grant.assignment_id);
    assert_eq!(next.grant.assignment_version, original_allowance.grant.assignment_version);
    assert_ne!(next.allowance_id, original_allowance.allowance_id);
    assert_eq!(next.grant.work_item_id, original_allowance.grant.work_item_id);
    assert!(next.dispatch.is_none());
    assert_eq!(restarted.archived_source_reviews.len(), 1);
    assert_eq!(restarted.archived_source_reviews[0].source_allowance, original_allowance);
    assert_eq!(restarted.archived_source_reviews[0].report_digest, review.report_digest);
    assert_eq!(restarted.cost_ceiling_micros, blocked.cost_ceiling_micros);
    assert_eq!(restarted.reserved_cost_micros, blocked.reserved_cost_micros);
    assert_eq!(restarted.committed_cost_micros, blocked.committed_cost_micros);
    assert_eq!(restarted.work_items[&next.grant.work_item_id].state, CompanyWorkStateV1::Assigned);
    let binding = f.api.adaptive_provider_authority_for_claim(next.grant.agent_id).unwrap().unwrap();
    assert_ne!(binding.grant.session_id, original.grant.session_id);
    f.session = f.api.store.adaptive_session(binding.grant.session_id, &binding.grant.authority).unwrap().unwrap();
    assert_eq!((f.session.model_calls, f.session.tool_calls), (0, 0));
    assert_eq!(f.session.grant.max_model_calls, 16);
    assert_eq!(f.session.grant.authority, original.grant.authority);
    assert!(f.session.active_work_funding().is_none());
    let corrected = complete_source(&mut f, true);
    assert_ne!(corrected.plan.plan_id, original_done.plan.plan_id);
    assert_eq!(f.api.store.work_item_for_plan(
        &restarted.tenant_id, &restarted.project_id, &next.grant.work_item_id, original_done.plan.plan_id,
    ).unwrap().unwrap(), original_done);
    assert_eq!(f.api.store.adaptive_session(original.grant.session_id, &original.grant.authority).unwrap().unwrap(), original);
    assert_eq!(f.api.store.adaptive_leadership_review_calls(&restarted.tenant_id, original.grant.session_id).unwrap(), calls);
    assert_eq!(serde_json::to_value(events.event_by_operation_id(&original_usage.operation_id).unwrap().unwrap()).unwrap(),
        serde_json::to_value(original_usage).unwrap());
}
