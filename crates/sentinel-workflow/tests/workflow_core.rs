use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use sentinel_workflow::*;

#[derive(Debug)]
struct FixedClock(AtomicU64);

impl FixedClock {
    fn new(now_ms: u64) -> Self {
        Self(AtomicU64::new(now_ms))
    }
}

impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        self.0.fetch_add(1, Ordering::SeqCst)
    }
}

#[derive(Debug, Default)]
struct FakeExecutionPort {
    fail: AtomicBool,
    calls: Mutex<Vec<String>>,
    receipts: Mutex<HashMap<String, ExecutionReceipt>>,
}

impl FakeExecutionPort {
    fn failing() -> Self {
        Self {
            fail: AtomicBool::new(true),
            ..Self::default()
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("calls lock").clone()
    }
}

impl WorkExecutionPort for FakeExecutionPort {
    fn reserve(&self, request: &PendingExecution) -> Result<ExecutionReceipt, WorkExecutionError> {
        self.calls
            .lock()
            .expect("calls lock")
            .push(request.invocation_id.clone());
        if self.fail.load(Ordering::SeqCst) {
            return Err(WorkExecutionError::Unavailable);
        }
        let mut receipts = self.receipts.lock().expect("receipt lock");
        Ok(receipts
            .entry(request.invocation_id.clone())
            .or_insert_with(|| ExecutionReceipt {
                invocation_id: request.invocation_id.clone(),
                accepted: true,
            })
            .clone())
    }
}

fn agent(id: u16) -> AgentId {
    AgentId::new(id).expect("valid fixture AgentId")
}

fn actor(role: ActorRole, id: u16) -> AuthenticatedActor {
    AuthenticatedActor {
        actor_id: format!("fixture-{role:?}-{id}"),
        role,
        customer_id: None,
        agent_id: Some(agent(id)),
    }
}

fn customer(customer_id: &str) -> AuthenticatedActor {
    AuthenticatedActor {
        actor_id: format!("customer-user-{customer_id}"),
        role: ActorRole::Customer,
        customer_id: Some(customer_id.to_owned()),
        agent_id: None,
    }
}

fn engine_at(
    path: &std::path::Path,
    port: Arc<dyn WorkExecutionPort>,
) -> (WorkflowEngine, Arc<WorkflowStore>) {
    let store = Arc::new(WorkflowStore::open(path).expect("open workflow store"));
    let engine =
        WorkflowEngine::with_clock(store.clone(), port, Arc::new(FixedClock::new(1_000_000)));
    (engine, store)
}

fn response_request(outcome: CommandOutcome) -> CustomerRequest {
    match outcome.response {
        WorkflowResponse::CustomerRequest(value) => value,
        other => panic!("expected customer request, got {other:?}"),
    }
}

fn response_proposal(outcome: CommandOutcome) -> Proposal {
    match outcome.response {
        WorkflowResponse::Proposal(value) => value,
        other => panic!("expected proposal, got {other:?}"),
    }
}

fn proposal_binding(expires_at_ms: u64) -> ProposalBinding {
    ProposalBinding {
        scope: "Versioned website delivery".into(),
        deliverables: vec!["source".into(), "evidence".into()],
        exclusions: vec!["live deployment".into()],
        acceptance_criteria: vec!["quality gate passes".into()],
        assumptions: vec!["approved inputs are available".into()],
        cost_ceiling_micros: 10_000,
        provider_cost_ceilings_micros: BTreeMap::from([("mock".into(), 5_000)]),
        expires_at_ms,
    }
}

fn bootstrap_project(engine: &WorkflowEngine) -> (CustomerRequestId, Project) {
    let customer_actor = customer("customer-a");
    let request = response_request(
        engine
            .execute(
                customer_actor.clone(),
                "submit-a",
                WorkflowCommand::SubmitCustomerRequest {
                    customer_id: "customer-a".into(),
                    summary_ref: "ref:request/sha256:1111".into(),
                    desired_outcome: "Deliver the agreed web project".into(),
                    constraints: vec!["No production mutation".into()],
                },
            )
            .expect("submit request"),
    );
    let request = response_request(
        engine
            .execute(
                actor(ActorRole::Sales, 10),
                "qualify-a",
                WorkflowCommand::QualifyCustomerRequest {
                    request_id: request.id.clone(),
                    expected_version: request.version,
                    reason: "scope is bounded".into(),
                },
            )
            .expect("qualify request"),
    );
    let proposal = response_proposal(
        engine
            .execute(
                actor(ActorRole::Sales, 10),
                "proposal-a",
                WorkflowCommand::CreateProposal {
                    request_id: request.id.clone(),
                    expected_version: request.version,
                    binding: proposal_binding(2_000_000),
                },
            )
            .expect("create proposal"),
    );
    let outcome = engine
        .execute(
            customer_actor,
            "accept-a",
            WorkflowCommand::AcceptProposal {
                request_id: request.id.clone(),
                expected_version: request.version + 1,
                proposal_id: proposal.id,
                proposal_digest: proposal.digest,
                profile_id: "web-project-v1".into(),
                project_owner: agent(1),
                participants: vec![agent(1), agent(2), agent(3), agent(4), agent(5)],
            },
        )
        .expect("accept exact proposal");
    let project = match outcome.response {
        WorkflowResponse::AgreementProject { project, .. } => project,
        other => panic!("expected agreement and project, got {other:?}"),
    };
    (request.id, project)
}

fn work_spec(id: &str, role: ActorRole, dependencies: &[&str], capability: &str) -> WorkItemSpec {
    WorkItemSpec {
        id: WorkItemId(id.into()),
        title: format!("Work {id}"),
        objective: format!("Produce the bounded {id} result"),
        owner: agent(1),
        required_role: role,
        required_capabilities: BTreeSet::from([capability.into()]),
        dependency_ids: dependencies
            .iter()
            .map(|dependency| WorkItemId((*dependency).into()))
            .collect(),
        input_refs: vec!["sha256:input".into()],
        required_output_kinds: BTreeSet::from(["artifact".into()]),
        quality_gate: "gate:unit".into(),
        budget_micros: 4_000,
    }
}

fn assign_developer(engine: &WorkflowEngine, work_item_id: &str, version: u64) -> Assignment {
    let outcome = engine
        .execute(
            actor(ActorRole::ProjectManager, 1),
            &format!("assign-{work_item_id}"),
            WorkflowCommand::AssignWork {
                work_item_id: WorkItemId(work_item_id.into()),
                expected_version: version,
                assignee: AgentProfile {
                    agent_id: agent(2),
                    role: ActorRole::Developer,
                    capabilities: BTreeSet::from(["implementation".into()]),
                    reports_to: Some(agent(1)),
                    active: true,
                    current_assignments: 0,
                    max_assignments: 2,
                },
                reason: "capability and workload match".into(),
            },
        )
        .expect("assign work");
    match outcome.response {
        WorkflowResponse::Assignment(value) => value,
        other => panic!("expected assignment, got {other:?}"),
    }
}

#[test]
fn agreement_project_creation_is_atomic_idempotent_and_digest_bound() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (engine, _) = engine_at(
        &directory.path().join("workflow.db"),
        Arc::new(UnavailableExecutionPort),
    );
    let (_, project) = bootstrap_project(&engine);

    let replay = engine
        .execute(
            customer("customer-a"),
            "accept-a",
            WorkflowCommand::AcceptProposal {
                request_id: CustomerRequestId("irrelevant-on-replay".into()),
                expected_version: 999,
                proposal_id: ProposalId("irrelevant".into()),
                proposal_digest: "different".into(),
                profile_id: "different".into(),
                project_owner: agent(1),
                participants: vec![agent(1)],
            },
        )
        .expect_err("same operation with a different digest must fail");
    assert_eq!(replay.code, WorkflowErrorCode::IdempotencyConflict);

    let stored = engine
        .project(&project.id)
        .expect("read project")
        .expect("project exists");
    assert_eq!(stored.agreement_digest, project.agreement_digest);
    let projection = engine
        .project_projection(&project.id)
        .expect("read projection")
        .expect("projection exists");
    assert!(projection.last_event_sequence > 0);
    assert_eq!(projection.project_id, project.id);
}

#[test]
fn customer_state_machine_rejects_stale_expired_and_post_commit_mutations() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (engine, _) = engine_at(
        &directory.path().join("workflow.db"),
        Arc::new(UnavailableExecutionPort),
    );
    let submit = WorkflowCommand::SubmitCustomerRequest {
        customer_id: "customer-b".into(),
        summary_ref: "ref:request/customer-b".into(),
        desired_outcome: "Bounded website".into(),
        constraints: vec!["No upload".into()],
    };
    let first = engine
        .execute(customer("customer-b"), "submit-b", submit.clone())
        .expect("submit request");
    let request = response_request(first);
    let replay = engine
        .execute(customer("customer-b"), "submit-b", submit)
        .expect("identical submit replays");
    assert!(replay.replayed);
    assert_eq!(response_request(replay).id, request.id);

    let stale = engine
        .execute(
            customer("customer-b"),
            "clarify-stale",
            WorkflowCommand::ClarifyCustomerRequest {
                request_id: request.id.clone(),
                expected_version: 99,
                question_ref: "question:scope".into(),
                answer_ref: "answer:bounded".into(),
            },
        )
        .expect_err("stale request version is rejected");
    assert_eq!(stale.code, WorkflowErrorCode::VersionConflict);

    let qualified = response_request(
        engine
            .execute(
                actor(ActorRole::Sales, 10),
                "qualify-b",
                WorkflowCommand::QualifyCustomerRequest {
                    request_id: request.id.clone(),
                    expected_version: request.version,
                    reason: "bounded".into(),
                },
            )
            .expect("qualify request"),
    );
    let proposal = response_proposal(
        engine
            .execute(
                actor(ActorRole::Sales, 10),
                "proposal-b",
                WorkflowCommand::CreateProposal {
                    request_id: request.id.clone(),
                    expected_version: qualified.version,
                    binding: proposal_binding(1_000_006),
                },
            )
            .expect("create short-lived proposal"),
    );
    let digest_error = engine
        .execute(
            customer("customer-b"),
            "accept-wrong-digest",
            WorkflowCommand::AcceptProposal {
                request_id: request.id.clone(),
                expected_version: qualified.version + 1,
                proposal_id: proposal.id.clone(),
                proposal_digest: "f".repeat(64),
                profile_id: "web-project-v1".into(),
                project_owner: agent(1),
                participants: vec![agent(1)],
            },
        )
        .expect_err("stale proposal digest is rejected");
    assert_eq!(digest_error.code, WorkflowErrorCode::DigestConflict);
    let expired = engine
        .execute(
            customer("customer-b"),
            "accept-expired",
            WorkflowCommand::AcceptProposal {
                request_id: request.id,
                expected_version: qualified.version + 1,
                proposal_id: proposal.id,
                proposal_digest: proposal.digest,
                profile_id: "web-project-v1".into(),
                project_owner: agent(1),
                participants: vec![agent(1)],
            },
        )
        .expect_err("expired proposal is rejected");
    assert_eq!(expired.code, WorkflowErrorCode::InvalidTransition);

    let request_c = response_request(
        engine
            .execute(
                customer("customer-c"),
                "submit-c",
                WorkflowCommand::SubmitCustomerRequest {
                    customer_id: "customer-c".into(),
                    summary_ref: "ref:request/customer-c".into(),
                    desired_outcome: "Rejectable website proposal".into(),
                    constraints: Vec::new(),
                },
            )
            .expect("submit rejectable request"),
    );
    let request_c = response_request(
        engine
            .execute(
                actor(ActorRole::Sales, 10),
                "qualify-c",
                WorkflowCommand::QualifyCustomerRequest {
                    request_id: request_c.id,
                    expected_version: request_c.version,
                    reason: "bounded".into(),
                },
            )
            .expect("qualify rejectable request"),
    );
    let proposal_c = response_proposal(
        engine
            .execute(
                actor(ActorRole::Sales, 10),
                "proposal-c",
                WorkflowCommand::CreateProposal {
                    request_id: request_c.id.clone(),
                    expected_version: request_c.version,
                    binding: proposal_binding(2_000_000),
                },
            )
            .expect("create rejectable proposal"),
    );
    let rejected = response_request(
        engine
            .execute(
                customer("customer-c"),
                "reject-c",
                WorkflowCommand::RejectProposal {
                    request_id: request_c.id,
                    expected_version: request_c.version + 1,
                    proposal_id: proposal_c.id,
                    proposal_digest: proposal_c.digest,
                    reason_ref: "customer:scope-mismatch".into(),
                },
            )
            .expect("customer rejects exact proposal"),
    );
    assert_eq!(rejected.state, CustomerRequestState::Rejected);
    let feedback = response_request(
        engine
            .execute(
                customer("customer-c"),
                "feedback-c",
                WorkflowCommand::RecordCustomerFeedback {
                    request_id: rejected.id,
                    feedback_ref: "customer:revise-copy".into(),
                },
            )
            .expect("terminal customer feedback is bounded and durable"),
    );
    assert_eq!(feedback.feedback.len(), 1);

    let cancellable = response_request(
        engine
            .execute(
                customer("customer-d"),
                "submit-d",
                WorkflowCommand::SubmitCustomerRequest {
                    customer_id: "customer-d".into(),
                    summary_ref: "ref:request/customer-d".into(),
                    desired_outcome: "Cancelled request".into(),
                    constraints: Vec::new(),
                },
            )
            .expect("submit cancellable request"),
    );
    let cancelled = response_request(
        engine
            .execute(
                customer("customer-d"),
                "cancel-d",
                WorkflowCommand::CancelCustomerRequest {
                    request_id: cancellable.id,
                    expected_version: cancellable.version,
                    reason_ref: "customer:withdrawn".into(),
                },
            )
            .expect("customer cancels before agreement"),
    );
    assert_eq!(cancelled.state, CustomerRequestState::Cancelled);

    let (accepted_request_id, _) = bootstrap_project(&engine);
    let accepted_request = engine
        .customer_request(&accepted_request_id)
        .expect("read accepted request")
        .expect("accepted request exists");
    let cancel = engine
        .execute(
            customer("customer-a"),
            "cancel-after-commit",
            WorkflowCommand::CancelCustomerRequest {
                request_id: accepted_request.id,
                expected_version: accepted_request.version,
                reason_ref: "customer:late-cancel".into(),
            },
        )
        .expect_err("accepted agreement cannot be cancelled");
    assert_eq!(cancel.code, WorkflowErrorCode::InvalidTransition);
}

#[test]
fn invalid_dag_and_assignment_policy_fail_without_partial_state() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (engine, _) = engine_at(
        &directory.path().join("workflow.db"),
        Arc::new(UnavailableExecutionPort),
    );
    let (_, project) = bootstrap_project(&engine);
    let cyclic = vec![
        work_spec(
            "work-a",
            ActorRole::Developer,
            &["work-b"],
            "implementation",
        ),
        work_spec(
            "work-b",
            ActorRole::Developer,
            &["work-a"],
            "implementation",
        ),
    ];
    let error = engine
        .execute(
            actor(ActorRole::ProjectManager, 1),
            "plan-invalid",
            WorkflowCommand::PlanWorkGraph {
                project_id: project.id.clone(),
                expected_version: project.version,
                items: cyclic,
            },
        )
        .expect_err("cyclic graph must fail");
    assert_eq!(error.code, WorkflowErrorCode::DagInvalid);
    assert!(engine
        .work_item(&WorkItemId("work-a".into()))
        .expect("read work item")
        .is_none());
    assert_eq!(
        engine
            .project(&project.id)
            .expect("read project")
            .expect("project exists")
            .state,
        ProjectState::Planned
    );

    engine
        .execute(
            actor(ActorRole::ProjectManager, 1),
            "plan-valid",
            WorkflowCommand::PlanWorkGraph {
                project_id: project.id,
                expected_version: project.version,
                items: vec![work_spec(
                    "work-a",
                    ActorRole::Developer,
                    &[],
                    "implementation",
                )],
            },
        )
        .expect("valid graph");
    let error = engine
        .execute(
            actor(ActorRole::ProjectManager, 1),
            "assign-invalid-capability",
            WorkflowCommand::AssignWork {
                work_item_id: WorkItemId("work-a".into()),
                expected_version: 1,
                assignee: AgentProfile {
                    agent_id: agent(2),
                    role: ActorRole::Developer,
                    capabilities: BTreeSet::new(),
                    reports_to: Some(agent(1)),
                    active: true,
                    current_assignments: 0,
                    max_assignments: 1,
                },
                reason: "should be denied".into(),
            },
        )
        .expect_err("missing capability must fail");
    assert_eq!(error.code, WorkflowErrorCode::CapabilityDenied);
    assert_eq!(
        engine
            .work_item(&WorkItemId("work-a".into()))
            .expect("read work item")
            .expect("work item exists")
            .state,
        WorkItemState::Ready
    );
}

#[test]
fn durable_execution_outbox_recovers_after_restart_without_duplicate_dispatch() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("workflow.db");
    let project_id;
    {
        let failing_port = Arc::new(FakeExecutionPort::failing());
        let (engine, _) = engine_at(&path, failing_port.clone());
        let (_, project) = bootstrap_project(&engine);
        project_id = project.id.clone();
        engine
            .execute(
                actor(ActorRole::ProjectManager, 1),
                "plan-execution",
                WorkflowCommand::PlanWorkGraph {
                    project_id: project.id,
                    expected_version: project.version,
                    items: vec![work_spec(
                        "work-exec",
                        ActorRole::Developer,
                        &[],
                        "implementation",
                    )],
                },
            )
            .expect("plan graph");
        let assignment = assign_developer(&engine, "work-exec", 1);
        engine
            .execute(
                actor(ActorRole::Developer, 2),
                "claim-execution",
                WorkflowCommand::ClaimWork {
                    work_item_id: assignment.work_item_id,
                    expected_version: 2,
                    agent_id: agent(2),
                    input_digest: "a".repeat(64),
                    deadline_ms: 2_000_000,
                },
            )
            .expect("claim work");
        let error = engine
            .dispatch_pending_executions(10)
            .expect_err("failed dependency remains recoverable");
        assert_eq!(error.code, WorkflowErrorCode::ExecutionUnavailable);
        assert_eq!(failing_port.calls().len(), 1);
        assert_eq!(
            engine
                .work_item(&WorkItemId("work-exec".into()))
                .expect("read work")
                .expect("work exists")
                .state,
            WorkItemState::Claimed
        );
    }

    let healthy_port = Arc::new(FakeExecutionPort::default());
    let (restarted, _) = engine_at(&path, healthy_port.clone());
    let receipts = restarted
        .dispatch_pending_executions(10)
        .expect("restart dispatches persisted request");
    assert_eq!(receipts.len(), 1);
    assert_eq!(healthy_port.calls().len(), 1);
    assert!(restarted
        .dispatch_pending_executions(10)
        .expect("dispatched outbox is not replayed")
        .is_empty());
    assert_eq!(healthy_port.calls().len(), 1);
    assert_eq!(
        restarted
            .work_item(&WorkItemId("work-exec".into()))
            .expect("read work")
            .expect("work exists")
            .state,
        WorkItemState::InProgress
    );
    let projection = restarted
        .project_projection(&project_id)
        .expect("read projection")
        .expect("projection exists");
    assert_eq!(projection.work_items_by_state.get("in_progress"), Some(&1));
}

#[test]
fn execution_retries_are_bounded_and_materialize_an_operator_blocker() {
    let directory = tempfile::tempdir().expect("tempdir");
    let failing_port = Arc::new(FakeExecutionPort::failing());
    let (engine, _) = engine_at(&directory.path().join("workflow.db"), failing_port.clone());
    let (_, project) = bootstrap_project(&engine);
    engine
        .execute(
            actor(ActorRole::ProjectManager, 1),
            "plan-bounded-retry",
            WorkflowCommand::PlanWorkGraph {
                project_id: project.id.clone(),
                expected_version: project.version,
                items: vec![work_spec(
                    "work-bounded-retry",
                    ActorRole::Developer,
                    &[],
                    "implementation",
                )],
            },
        )
        .expect("plan graph");
    let assignment = assign_developer(&engine, "work-bounded-retry", 1);
    engine
        .execute(
            actor(ActorRole::Developer, 2),
            "claim-bounded-retry",
            WorkflowCommand::ClaimWork {
                work_item_id: assignment.work_item_id.clone(),
                expected_version: 2,
                agent_id: agent(2),
                input_digest: "a".repeat(64),
                deadline_ms: 2_000_000,
            },
        )
        .expect("claim work");

    for _ in 0..3 {
        let error = engine
            .dispatch_pending_executions(1)
            .expect_err("unavailable fake fails closed");
        assert_eq!(error.code, WorkflowErrorCode::ExecutionUnavailable);
    }
    assert_eq!(failing_port.calls().len(), 3);
    assert!(engine
        .dispatch_pending_executions(1)
        .expect("failed outbox is no longer retried")
        .is_empty());
    assert_eq!(failing_port.calls().len(), 3);
    let projection = engine
        .project_projection(&project.id)
        .expect("read projection")
        .expect("projection exists");
    assert_eq!(projection.state, ProjectState::Blocked);
    assert_eq!(projection.open_blockers, 1);
    assert_eq!(
        projection.blockers[0].cause_ref,
        "execution_retry_exhausted"
    );
    engine
        .execute(
            actor(ActorRole::ProjectManager, 1),
            "resolve-execution-retry",
            WorkflowCommand::ResolveBlocker {
                blocker_id: projection.blockers[0].id.clone(),
                resolution_ref: "operator:dependency-restored".into(),
            },
        )
        .expect("operator explicitly re-arms the failed outbox");
    drop(engine);
    let healthy_port = Arc::new(FakeExecutionPort::default());
    let (restarted, _) = engine_at(&directory.path().join("workflow.db"), healthy_port.clone());
    assert_eq!(
        restarted
            .dispatch_pending_executions(1)
            .expect("operator-resolved request can be dispatched")
            .len(),
        1
    );
    assert_eq!(healthy_port.calls().len(), 1);
}

#[test]
fn completion_unlocks_dag_and_enforces_gate_and_assignment_version() {
    let directory = tempfile::tempdir().expect("tempdir");
    let port = Arc::new(FakeExecutionPort::default());
    let (engine, _) = engine_at(&directory.path().join("workflow.db"), port);
    let (_, project) = bootstrap_project(&engine);
    engine
        .execute(
            actor(ActorRole::ProjectManager, 1),
            "plan-dag",
            WorkflowCommand::PlanWorkGraph {
                project_id: project.id,
                expected_version: project.version,
                items: vec![
                    work_spec("work-a", ActorRole::Developer, &[], "implementation"),
                    work_spec(
                        "work-b",
                        ActorRole::Developer,
                        &["work-a"],
                        "implementation",
                    ),
                ],
            },
        )
        .expect("plan graph");
    let assignment = assign_developer(&engine, "work-a", 1);
    engine
        .execute(
            actor(ActorRole::Developer, 2),
            "claim-a",
            WorkflowCommand::ClaimWork {
                work_item_id: assignment.work_item_id.clone(),
                expected_version: 2,
                agent_id: agent(2),
                input_digest: "a".repeat(64),
                deadline_ms: 2_000_000,
            },
        )
        .expect("claim work");
    engine
        .dispatch_pending_executions(10)
        .expect("fake accepts work");
    let error = engine
        .execute(
            actor(ActorRole::Developer, 2),
            "complete-bad-gate",
            WorkflowCommand::CompleteWork {
                work_item_id: assignment.work_item_id.clone(),
                expected_version: 4,
                assignment_version: assignment.assignment_version,
                output_refs: BTreeMap::from([("artifact".into(), "b".repeat(64))]),
                gate_id: "gate:unit".into(),
                gate_passed: false,
            },
        )
        .expect_err("failed gate cannot complete work");
    assert_eq!(error.code, WorkflowErrorCode::InvalidTransition);
    engine
        .execute(
            actor(ActorRole::Developer, 2),
            "complete-a",
            WorkflowCommand::CompleteWork {
                work_item_id: assignment.work_item_id,
                expected_version: 4,
                assignment_version: assignment.assignment_version,
                output_refs: BTreeMap::from([("artifact".into(), "b".repeat(64))]),
                gate_id: "gate:unit".into(),
                gate_passed: true,
            },
        )
        .expect("complete work");
    assert_eq!(
        engine
            .work_item(&WorkItemId("work-b".into()))
            .expect("read dependent")
            .expect("dependent exists")
            .state,
        WorkItemState::Ready
    );
}

#[test]
fn structured_collaboration_and_cost_controls_are_authoritative() {
    let directory = tempfile::tempdir().expect("tempdir");
    let port = Arc::new(FakeExecutionPort::default());
    let (engine, _) = engine_at(&directory.path().join("workflow.db"), port.clone());
    let (_, project) = bootstrap_project(&engine);
    engine
        .execute(
            actor(ActorRole::ProjectManager, 1),
            "plan-cost-project",
            WorkflowCommand::PlanWorkGraph {
                project_id: project.id.clone(),
                expected_version: project.version,
                items: vec![work_spec(
                    "work-cost",
                    ActorRole::Developer,
                    &[],
                    "implementation",
                )],
            },
        )
        .expect("activate project with a bounded graph");
    let blocker = engine
        .execute(
            actor(ActorRole::Developer, 2),
            "raise-delivery-blocker",
            WorkflowCommand::RaiseBlocker {
                project_id: project.id.clone(),
                work_item_id: Some(WorkItemId("work-cost".into())),
                cause_ref: "input:customer-copy-missing".into(),
                impact: "implementation cannot start".into(),
                owner: agent(2),
                required_resolution_role: ActorRole::ProjectManager,
            },
        )
        .expect("participant raises a durable blocker");
    let blocker = match blocker.response {
        WorkflowResponse::Blocker(value) => value,
        other => panic!("expected blocker, got {other:?}"),
    };
    engine
        .execute(
            actor(ActorRole::ProjectManager, 1),
            "escalate-delivery-blocker",
            WorkflowCommand::EscalateBlocker {
                blocker_id: blocker.id.clone(),
                escalation_target: agent(4),
                reason: "customer decision is required".into(),
            },
        )
        .expect("project manager escalates to a project participant");
    let unauthorized_resolution = engine
        .execute(
            actor(ActorRole::Developer, 2),
            "resolve-delivery-blocker-wrong-role",
            WorkflowCommand::ResolveBlocker {
                blocker_id: blocker.id.clone(),
                resolution_ref: "customer:copy-approved".into(),
            },
        )
        .expect_err("only the declared resolution role may unblock work");
    assert_eq!(
        unauthorized_resolution.code,
        WorkflowErrorCode::Unauthorized
    );
    engine
        .execute(
            actor(ActorRole::ProjectManager, 1),
            "resolve-delivery-blocker",
            WorkflowCommand::ResolveBlocker {
                blocker_id: blocker.id,
                resolution_ref: "customer:copy-approved".into(),
            },
        )
        .expect("declared resolution role unblocks work");
    let decision = engine
        .execute(
            actor(ActorRole::ProjectManager, 1),
            "decision-a",
            WorkflowCommand::RecordDecision {
                project_id: project.id.clone(),
                work_item_id: None,
                choice: "Use the approved profile".into(),
                alternatives: vec!["defer".into()],
                rationale_ref: "sha256:rationale".into(),
                evidence_refs: vec!["sha256:evidence".into()],
            },
        )
        .expect("record decision");
    assert!(matches!(decision.response, WorkflowResponse::Decision(_)));
    engine
        .execute(
            actor(ActorRole::ProjectManager, 1),
            "create-project-room",
            WorkflowCommand::CreateProjectRoom {
                project_id: project.id.clone(),
                kind: ProjectRoomKind::Project,
                team_ref: None,
                members: vec![agent(1), agent(2), agent(3)],
            },
        )
        .expect("create structured project room");
    let action = engine
        .execute(
            actor(ActorRole::ProjectManager, 1),
            "record-action",
            WorkflowCommand::RecordActionItem {
                project_id: project.id.clone(),
                work_item_id: Some(WorkItemId("work-cost".into())),
                owner: agent(2),
                action_ref: "action:prepare-fixture".into(),
                due_at_ms: Some(1_500_000),
            },
        )
        .expect("record structured action item");
    let action = match action.response {
        WorkflowResponse::ActionItem(value) => value,
        other => panic!("expected action item, got {other:?}"),
    };
    engine
        .execute(
            actor(ActorRole::Developer, 2),
            "resolve-action",
            WorkflowCommand::ResolveActionItem {
                action_item_id: action.id,
                completed: true,
                resolution_ref: "evidence:fixture-ready".into(),
            },
        )
        .expect("action owner resolves action item");
    engine
        .execute(
            actor(ActorRole::Developer, 2),
            "record-question",
            WorkflowCommand::RecordQuestion {
                project_id: project.id.clone(),
                work_item_id: Some(WorkItemId("work-cost".into())),
                owner: agent(3),
                question_ref: "question:approved-copy".into(),
            },
        )
        .expect("record unresolved structured question");
    let assignment = assign_developer(&engine, "work-cost", 3);
    engine
        .execute(
            actor(ActorRole::Qa, 3),
            "approve-work-subject",
            WorkflowCommand::RecordApproval {
                project_id: project.id.clone(),
                work_item_id: Some(assignment.work_item_id.clone()),
                gate_id: "gate:input-review".into(),
                subject_digest: "e".repeat(64),
                approved: true,
                reason: "independent reviewer verified the input".into(),
            },
        )
        .expect("independent QA records a digest-bound approval");
    let handoff = engine
        .execute(
            actor(ActorRole::Developer, 2),
            "create-handoff",
            WorkflowCommand::CreateHandoff {
                project_id: project.id.clone(),
                work_item_id: assignment.work_item_id.clone(),
                producer: agent(2),
                consumer: agent(3),
                artifact_digests: BTreeSet::from(["d".repeat(64)]),
                reason: "review the bounded fixture".into(),
            },
        )
        .expect("create digest-bound handoff");
    let handoff = match handoff.response {
        WorkflowResponse::Handoff(value) => value,
        other => panic!("expected handoff, got {other:?}"),
    };
    engine
        .execute(
            actor(ActorRole::Qa, 3),
            "acknowledge-handoff",
            WorkflowCommand::AcknowledgeHandoff {
                handoff_id: handoff.id,
                accepted: true,
                reason: "artifact digest verified".into(),
            },
        )
        .expect("designated consumer acknowledges handoff");
    let gaia_error = engine
        .execute(
            actor(ActorRole::Gaia, 5),
            "gaia-cost",
            WorkflowCommand::ReserveCost {
                project_id: project.id.clone(),
                work_item_id: None,
                provider: "mock".into(),
                amount_micros: 100,
            },
        )
        .expect_err("Gaia cannot authorize spend");
    assert_eq!(gaia_error.code, WorkflowErrorCode::Unauthorized);
    let reservation = engine
        .execute(
            actor(ActorRole::ProjectManager, 1),
            "reserve-cost",
            WorkflowCommand::ReserveCost {
                project_id: project.id.clone(),
                work_item_id: None,
                provider: "mock".into(),
                amount_micros: 4_000,
            },
        )
        .expect("reserve admitted cost");
    let reservation = match reservation.response {
        WorkflowResponse::CostReservation(value) => value,
        other => panic!("expected reservation, got {other:?}"),
    };
    let blocked = engine
        .execute(
            actor(ActorRole::ProjectManager, 1),
            "reserve-over-ceiling",
            WorkflowCommand::ReserveCost {
                project_id: project.id.clone(),
                work_item_id: None,
                provider: "mock".into(),
                amount_micros: 7_000,
            },
        )
        .expect("budget exhaustion is committed as a durable blocker");
    let blocker = match blocked.response {
        WorkflowResponse::Blocker(value) => value,
        other => panic!("expected budget blocker, got {other:?}"),
    };
    assert_eq!(blocker.cause_ref, "provider_ceiling_exhausted");
    assert_eq!(blocker.required_resolution_role, ActorRole::ProjectManager);
    engine
        .execute(
            actor(ActorRole::ProjectManager, 1),
            "resolve-budget-blocker",
            WorkflowCommand::ResolveBlocker {
                blocker_id: blocker.id,
                resolution_ref: "decision:use-existing-reservation".into(),
            },
        )
        .expect("authorized operator resolves the typed blocker");
    engine
        .execute(
            actor(ActorRole::ProjectManager, 1),
            "commit-cost",
            WorkflowCommand::CommitCost {
                reservation_id: reservation.id,
                actual_micros: 3_500,
            },
        )
        .expect("commit actual cost");
    engine
        .execute(
            actor(ActorRole::Developer, 2),
            "claim-cost-work",
            WorkflowCommand::ClaimWork {
                work_item_id: assignment.work_item_id.clone(),
                expected_version: 4,
                agent_id: agent(2),
                input_digest: "f".repeat(64),
                deadline_ms: 2_000_000,
            },
        )
        .expect("assigned developer claims work");
    engine
        .dispatch_pending_executions(1)
        .expect("deterministic fake accepts one execution");
    assert_eq!(port.calls().len(), 1);
    engine
        .execute(
            actor(ActorRole::Developer, 2),
            "complete-cost-work",
            WorkflowCommand::CompleteWork {
                work_item_id: assignment.work_item_id,
                expected_version: 6,
                assignment_version: assignment.assignment_version,
                output_refs: BTreeMap::from([("artifact".into(), "a".repeat(64))]),
                gate_id: "gate:unit".into(),
                gate_passed: true,
            },
        )
        .expect("accepted execution completes with digest-bound evidence");
    let projection = engine
        .project_projection(&project.id)
        .expect("read projection")
        .expect("projection exists");
    assert_eq!(projection.reserved_cost_micros, 0);
    assert_eq!(projection.committed_cost_micros, 3_500);
    assert_eq!(projection.decisions.len(), 1);
    assert_eq!(projection.rooms.len(), 1);
    assert_eq!(projection.action_items.len(), 1);
    assert_eq!(projection.open_action_items, 0);
    assert_eq!(projection.open_questions, 1);
    assert_eq!(projection.handoffs.len(), 1);
    assert_eq!(projection.approvals.len(), 1);
    assert_eq!(projection.completion_evidence.len(), 1);
    assert_eq!(projection.state, ProjectState::DeliveryCandidate);
    let events = engine.events_since(0, 1_000).expect("read event stream");
    assert!(events
        .windows(2)
        .all(|pair| pair[0].sequence < pair[1].sequence));
    assert!(events
        .iter()
        .all(|event| !event.operation_digest.is_empty() && event.schema_version == 1));

    drop(engine);
    let (restarted, _) = engine_at(
        &directory.path().join("workflow.db"),
        Arc::new(UnavailableExecutionPort),
    );
    let recovered = restarted
        .project_projection(&project.id)
        .expect("read projection after restart")
        .expect("projection survives restart");
    assert_eq!(recovered.decisions.len(), 1);
    assert_eq!(recovered.handoffs[0].state, HandoffState::Accepted);
    assert_eq!(recovered.open_questions, 1);
    assert_eq!(recovered.committed_cost_micros, 3_500);
    assert_eq!(recovered.approvals.len(), 1);
    assert_eq!(recovered.completion_evidence.len(), 1);
    assert_eq!(recovered.state, ProjectState::DeliveryCandidate);
}
