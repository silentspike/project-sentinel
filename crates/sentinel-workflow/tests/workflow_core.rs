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
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Ready
    }

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

#[derive(Debug, Clone, Default)]
enum CompletionMode {
    #[default]
    Valid,
    ArtifactOwner(AgentId),
    Issuer(AgentId),
    FailedGate,
    StaleIssuerAuthority,
    ReplayAssignment,
    ReplayProjectVersion,
    ReplayWorkItemVersion,
    ReplayDomain,
    TimeoutOnce,
    AlwaysTimeout,
    CrashAfterAuthority,
}

#[derive(Debug, Clone)]
struct TestCompletionReceipt {
    schema_version: u32,
    receipt_id: String,
    request_digest: String,
    invocation_id: String,
    project_id: ProjectId,
    project_version: u64,
    work_item_id: WorkItemId,
    work_item_version: u64,
    assignment_version: u64,
    assignment_authority_generation: u64,
    issuer: AgentId,
    issuer_authority_generation: u64,
    issuer_authority_digest: String,
    issued_at_ms: u64,
    expires_at_ms: u64,
    replay_domain: String,
    artifacts: Vec<ArtifactReceipt>,
    gate: GateReceipt,
    crash_on_validation: bool,
}

impl CompletionAuthorityReceipt for TestCompletionReceipt {
    fn schema_version(&self) -> u32 {
        assert!(
            !self.crash_on_validation,
            "injected crash after authority response"
        );
        self.schema_version
    }
    fn receipt_id(&self) -> &str {
        &self.receipt_id
    }
    fn request_digest(&self) -> &str {
        &self.request_digest
    }
    fn invocation_id(&self) -> &str {
        &self.invocation_id
    }
    fn project_id(&self) -> &ProjectId {
        &self.project_id
    }
    fn project_version(&self) -> u64 {
        self.project_version
    }
    fn work_item_id(&self) -> &WorkItemId {
        &self.work_item_id
    }
    fn work_item_version(&self) -> u64 {
        self.work_item_version
    }
    fn assignment_version(&self) -> u64 {
        self.assignment_version
    }
    fn assignment_authority_generation(&self) -> u64 {
        self.assignment_authority_generation
    }
    fn issuer(&self) -> AgentId {
        self.issuer
    }
    fn issuer_authority_generation(&self) -> u64 {
        self.issuer_authority_generation
    }
    fn issuer_authority_digest(&self) -> &str {
        &self.issuer_authority_digest
    }
    fn issued_at_ms(&self) -> u64 {
        self.issued_at_ms
    }
    fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }
    fn replay_domain(&self) -> &str {
        &self.replay_domain
    }
    fn artifacts(&self) -> &[ArtifactReceipt] {
        &self.artifacts
    }
    fn gate(&self) -> &GateReceipt {
        &self.gate
    }
}

#[derive(Debug, Default)]
struct FakeCompletionPort {
    mode: Mutex<CompletionMode>,
    calls: AtomicU64,
}

impl FakeCompletionPort {
    fn set_mode(&self, mode: CompletionMode) {
        *self.mode.lock().expect("completion lock") = mode;
    }

    fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}

impl CompletionEvidencePort for FakeCompletionPort {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Ready
    }

    fn completion_receipt(
        &self,
        query: &CompletionReceiptQuery,
    ) -> Result<Box<dyn CompletionAuthorityReceipt>, WorkExecutionError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut mode = self.mode.lock().expect("completion lock");
        if matches!(*mode, CompletionMode::TimeoutOnce) {
            *mode = CompletionMode::Valid;
            return Err(WorkExecutionError::TimedOut);
        }
        if matches!(*mode, CompletionMode::AlwaysTimeout) {
            return Err(WorkExecutionError::TimedOut);
        }
        let selected = mode.clone();
        if matches!(*mode, CompletionMode::CrashAfterAuthority) {
            *mode = CompletionMode::Valid;
        }
        Ok(Box::new(test_completion_receipt(query, &selected)))
    }
}

fn test_completion_receipt(
    query: &CompletionReceiptQuery,
    mode: &CompletionMode,
) -> TestCompletionReceipt {
    let artifact_owner = match mode {
        CompletionMode::ArtifactOwner(owner) => *owner,
        _ => query.agent_id,
    };
    let issuer = match mode {
        CompletionMode::Issuer(issuer) => *issuer,
        _ => agent(3),
    };
    let issuer_snapshot = fixture_snapshot(
        issuer,
        if issuer == agent(5) {
            ActorRole::ReleaseManager
        } else {
            ActorRole::Qa
        },
        if matches!(mode, CompletionMode::StaleIssuerAuthority) {
            2
        } else {
            1
        },
    );
    let artifacts = vec![ArtifactReceipt {
        kind: "source_tree".into(),
        digest: "b".repeat(64),
        owner: artifact_owner,
        invocation_id: query.invocation_id.clone(),
        project_id: query.project_id.clone(),
        work_item_id: query.work_item_id.clone(),
    }];
    let output_refs = BTreeMap::from([("source_tree".to_owned(), "b".repeat(64))]);
    let subject_digest = canonical_json_digest(&output_refs);
    let gate = GateReceipt {
        receipt_id: format!("gate-{}", query.invocation_id),
        invocation_id: query.invocation_id.clone(),
        project_id: query.project_id.clone(),
        work_item_id: query.work_item_id.clone(),
        gate_id: "browser_smoke".into(),
        runner_id: "web-qa-v1".into(),
        subject_digest,
        passed: !matches!(mode, CompletionMode::FailedGate),
    };
    TestCompletionReceipt {
        schema_version: query.schema_version,
        receipt_id: format!("authority-receipt-{}", query.request_id),
        request_digest: query.request_digest.clone(),
        invocation_id: query.invocation_id.clone(),
        project_id: query.project_id.clone(),
        project_version: if matches!(mode, CompletionMode::ReplayProjectVersion) {
            query.project_version + 1
        } else {
            query.project_version
        },
        work_item_id: query.work_item_id.clone(),
        work_item_version: if matches!(mode, CompletionMode::ReplayWorkItemVersion) {
            query.work_item_version + 1
        } else {
            query.work_item_version
        },
        assignment_version: if matches!(mode, CompletionMode::ReplayAssignment) {
            query.assignment_version + 1
        } else {
            query.assignment_version
        },
        assignment_authority_generation: query.assignment_authority_generation,
        issuer,
        issuer_authority_generation: issuer_snapshot.generation,
        issuer_authority_digest: issuer_snapshot.digest,
        issued_at_ms: 999_000,
        expires_at_ms: 1_100_000,
        replay_domain: if matches!(mode, CompletionMode::ReplayDomain) {
            format!("{}:foreign", query.replay_domain)
        } else {
            query.replay_domain.clone()
        },
        artifacts,
        gate,
        crash_on_validation: matches!(mode, CompletionMode::CrashAfterAuthority),
    }
}

fn canonical_json_digest<T: serde::Serialize>(value: &T) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;

    Sha256::digest(serde_json::to_vec(value).expect("canonical fixture JSON"))
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            let _ = write!(output, "{byte:02x}");
            output
        })
}

#[derive(Debug, Default)]
struct FakeOrganizationPort {
    snapshots: Mutex<HashMap<AgentId, OrganizationAgentSnapshot>>,
}

impl FakeOrganizationPort {
    fn with_defaults() -> Self {
        let port = Self::default();
        port.set_profile(
            1,
            ActorRole::ProjectManager,
            &["project_planning", "dependency_management"],
            None,
            1,
        );
        port.set_profile(
            2,
            ActorRole::Developer,
            &["web_development", "artifact_authoring", "test_execution"],
            Some(1),
            1,
        );
        port.set_profile(
            3,
            ActorRole::Qa,
            &[
                "quality_assurance",
                "browser_validation",
                "security_validation",
            ],
            Some(1),
            1,
        );
        port.set_profile(
            4,
            ActorRole::Designer,
            &["web_design", "artifact_authoring"],
            Some(1),
            1,
        );
        port.set_profile(
            5,
            ActorRole::ReleaseManager,
            &["release_management", "provenance_validation"],
            Some(1),
            1,
        );
        port.set_profile(
            6,
            ActorRole::TechnicalLead,
            &["technical_design", "work_review"],
            Some(1),
            1,
        );
        port
    }

    fn set_profile(
        &self,
        id: u16,
        role: ActorRole,
        capabilities: &[&str],
        reports_to: Option<u16>,
        generation: u64,
    ) {
        let profile = AgentProfile {
            agent_id: agent(id),
            role,
            capabilities: capabilities
                .iter()
                .map(|capability| (*capability).to_owned())
                .collect(),
            reports_to: reports_to.map(agent),
            active: true,
            current_assignments: 0,
            max_assignments: 2,
        };
        let snapshot = OrganizationAgentSnapshot::new(generation, profile)
            .expect("construct authoritative organization snapshot");
        self.snapshots
            .lock()
            .expect("organization snapshots lock")
            .insert(agent(id), snapshot);
    }

    fn set_active(&self, id: u16, active: bool) {
        let mut snapshots = self.snapshots.lock().expect("organization snapshots lock");
        let existing = snapshots.get(&agent(id)).expect("fixture profile").clone();
        let mut profile = existing.profile;
        profile.active = active;
        snapshots.insert(
            agent(id),
            OrganizationAgentSnapshot::new(existing.generation, profile)
                .expect("construct authoritative organization snapshot"),
        );
    }
}

impl OrganizationRuntimePort for FakeOrganizationPort {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Ready
    }

    fn agent_snapshot(
        &self,
        agent_id: AgentId,
    ) -> Result<OrganizationAgentSnapshot, WorkExecutionError> {
        self.snapshots
            .lock()
            .expect("organization snapshots lock")
            .get(&agent_id)
            .cloned()
            .ok_or(WorkExecutionError::Unavailable)
    }
}

fn agent(id: u16) -> AgentId {
    AgentId::new(id).expect("valid fixture AgentId")
}

fn fixture_snapshot(
    agent_id: AgentId,
    role: ActorRole,
    generation: u64,
) -> OrganizationAgentSnapshot {
    let capabilities = match role {
        ActorRole::Qa => [
            "quality_assurance",
            "browser_validation",
            "security_validation",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        ActorRole::ReleaseManager => ["release_management", "provenance_validation"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        _ => BTreeSet::new(),
    };
    OrganizationAgentSnapshot::new(
        generation,
        AgentProfile {
            agent_id,
            role,
            capabilities,
            reports_to: Some(agent(1)),
            active: true,
            current_assignments: 0,
            max_assignments: 2,
        },
    )
    .expect("fixture authority snapshot")
}

fn actor(role: ActorRole, id: u16) -> AuthenticatedPrincipal {
    actor_for_tenant(role, id, "tenant-a")
}

fn actor_for_tenant(role: ActorRole, id: u16, tenant_id: &str) -> AuthenticatedPrincipal {
    AuthenticatedPrincipal {
        principal_id: format!("fixture-{role:?}-{id}"),
        tenant_id: tenant_id.to_owned(),
        kind: PrincipalKind::Operator,
        role,
        customer_id: None,
        agent_id: Some(agent(id)),
    }
}

fn agent_principal(role: ActorRole, id: u16, tenant_id: &str) -> AuthenticatedPrincipal {
    AuthenticatedPrincipal {
        principal_id: format!("agent-principal-{id}"),
        tenant_id: tenant_id.to_owned(),
        kind: PrincipalKind::Agent,
        role,
        customer_id: None,
        agent_id: Some(agent(id)),
    }
}

fn customer(customer_id: &str) -> AuthenticatedPrincipal {
    customer_for_tenant(customer_id, "tenant-a")
}

fn customer_for_tenant(customer_id: &str, tenant_id: &str) -> AuthenticatedPrincipal {
    AuthenticatedPrincipal {
        principal_id: format!("customer-user-{customer_id}"),
        tenant_id: tenant_id.to_owned(),
        kind: PrincipalKind::Customer,
        role: ActorRole::Customer,
        customer_id: Some(customer_id.to_owned()),
        agent_id: None,
    }
}

fn engine_at(
    path: &std::path::Path,
    port: Arc<dyn WorkExecutionPort>,
) -> (WorkflowEngine, Arc<WorkflowStore>) {
    engine_with_organization(path, port, Arc::new(FakeOrganizationPort::with_defaults()))
}

fn engine_with_organization(
    path: &std::path::Path,
    port: Arc<dyn WorkExecutionPort>,
    organization: Arc<dyn OrganizationRuntimePort>,
) -> (WorkflowEngine, Arc<WorkflowStore>) {
    engine_with_ports(
        path,
        port,
        organization,
        Arc::new(FakeCompletionPort::default()),
    )
}

fn engine_with_ports(
    path: &std::path::Path,
    port: Arc<dyn WorkExecutionPort>,
    organization: Arc<dyn OrganizationRuntimePort>,
    completion: Arc<dyn CompletionEvidencePort>,
) -> (WorkflowEngine, Arc<WorkflowStore>) {
    let store = Arc::new(WorkflowStore::open(path).expect("open workflow store"));
    let engine = WorkflowEngine::with_ports_and_clock(
        store.clone(),
        port,
        organization,
        completion,
        Arc::new(CanonicalWorkProfile::embedded().expect("embedded profile")),
        Arc::new(FixedClock::new(1_000_000)),
    );
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
        governance: ProposalGovernance {
            profile_id: "web-project-v1".into(),
            policy_id: "governance-v1".into(),
            owner: agent(1),
            participants: vec![agent(1), agent(2), agent(3), agent(4), agent(5), agent(6)],
        },
        expires_at_ms,
    }
}

fn bootstrap_project(engine: &WorkflowEngine) -> (CustomerRequestId, Project) {
    bootstrap_project_for(engine, "a", "tenant-a", "customer-a")
}

fn bootstrap_project_for(
    engine: &WorkflowEngine,
    suffix: &str,
    tenant_id: &str,
    customer_id: &str,
) -> (CustomerRequestId, Project) {
    let customer_actor = customer_for_tenant(customer_id, tenant_id);
    let request = response_request(
        engine
            .execute(
                customer_actor.clone(),
                &format!("submit-{suffix}"),
                WorkflowCommand::SubmitCustomerRequest {
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
                actor_for_tenant(ActorRole::Sales, 10, tenant_id),
                &format!("qualify-{suffix}"),
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
                actor_for_tenant(ActorRole::Sales, 10, tenant_id),
                &format!("proposal-{suffix}"),
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
            &format!("accept-{suffix}"),
            WorkflowCommand::AcceptProposal {
                request_id: request.id.clone(),
                expected_version: request.version + 1,
                proposal_id: proposal.id,
                proposal_digest: proposal.digest,
            },
        )
        .expect("accept exact proposal");
    let project = match outcome.response {
        WorkflowResponse::AgreementProject { project, .. } => *project,
        other => panic!("expected agreement and project, got {other:?}"),
    };
    (request.id, project)
}

fn work_spec(
    id: &str,
    role: ActorRole,
    dependencies: &[&str],
    capabilities: &[&str],
    outputs: &[&str],
    gate: &str,
) -> WorkItemSpec {
    WorkItemSpec {
        id: WorkItemId(id.into()),
        title: format!("Work {id}"),
        objective: format!("Produce the bounded {id} result"),
        owner: agent(1),
        required_role: role,
        required_capabilities: capabilities.iter().map(|value| (*value).into()).collect(),
        dependency_ids: dependencies
            .iter()
            .map(|dependency| WorkItemId((*dependency).into()))
            .collect(),
        input_refs: vec!["sha256:input".into()],
        required_output_kinds: outputs.iter().map(|value| (*value).into()).collect(),
        quality_gate: gate.into(),
        budget_micros: 1_000,
    }
}

fn canonical_work_graph(prefix: &str) -> Vec<WorkItemSpec> {
    let id = |role: &str| format!("{prefix}-{role}");
    vec![
        work_spec(
            &id("plan"),
            ActorRole::ProjectManager,
            &[],
            &["project_planning", "dependency_management"],
            &["project_plan", "project_closeout_memory"],
            "html_structure",
        ),
        work_spec(
            &id("technical"),
            ActorRole::TechnicalLead,
            &[],
            &["technical_design", "work_review"],
            &["technical_design"],
            "local_link_integrity",
        ),
        work_spec(
            &id("design"),
            ActorRole::Designer,
            &[],
            &["web_design", "artifact_authoring"],
            &["design_specification"],
            "static_security",
        ),
        work_spec(
            &id("developer"),
            ActorRole::Developer,
            &[],
            &["web_development", "artifact_authoring", "test_execution"],
            &["source_tree"],
            "browser_smoke",
        ),
        work_spec(
            &id("qa"),
            ActorRole::Qa,
            &[&id("developer")],
            &[
                "quality_assurance",
                "browser_validation",
                "security_validation",
            ],
            &["qa_report"],
            "agreement_acceptance_criteria",
        ),
        work_spec(
            &id("release"),
            ActorRole::ReleaseManager,
            &[&id("qa")],
            &["release_management", "provenance_validation"],
            &["release_manifest", "delivery_receipt"],
            "digest_provenance",
        ),
    ]
}

fn assign_developer(engine: &WorkflowEngine, work_item_id: &str, version: u64) -> Assignment {
    let outcome = engine
        .execute(
            actor(ActorRole::ProjectManager, 1),
            &format!("assign-{work_item_id}-v{version}"),
            WorkflowCommand::AssignWork {
                work_item_id: WorkItemId(work_item_id.into()),
                expected_version: version,
                assignee: agent(2),
                reason: "capability and workload match".into(),
            },
        )
        .expect("assign work");
    match outcome.response {
        WorkflowResponse::Assignment(value) => value,
        other => panic!("expected assignment, got {other:?}"),
    }
}

fn prepare_completion(engine: &WorkflowEngine, suffix: &str) -> Assignment {
    let (_, project) =
        bootstrap_project_for(engine, suffix, "tenant-a", &format!("customer-{suffix}"));
    engine
        .execute(
            actor(ActorRole::ProjectManager, 1),
            &format!("plan-{suffix}"),
            WorkflowCommand::PlanWorkGraph {
                project_id: project.id,
                expected_version: project.version,
                items: canonical_work_graph(suffix),
            },
        )
        .expect("plan completion graph");
    let assignment = assign_developer(engine, &format!("{suffix}-developer"), 1);
    engine
        .execute(
            actor(ActorRole::Developer, 2),
            &format!("claim-{suffix}"),
            WorkflowCommand::ClaimWork {
                work_item_id: assignment.work_item_id.clone(),
                expected_version: 2,
                agent_id: agent(2),
                input_digest: "a".repeat(64),
                deadline_ms: 2_000_000,
            },
        )
        .expect("claim completion work");
    engine
        .dispatch_pending_executions(1)
        .expect("dispatch completion work");
    assignment
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
            },
        )
        .expect_err("same operation with a different digest must fail");
    assert_eq!(replay.code, WorkflowErrorCode::IdempotencyConflict);

    let stored = engine
        .project(&project.id)
        .expect("read project")
        .expect("project exists");
    assert_eq!(stored.agreement_digest, project.agreement_digest);
    assert_eq!(stored.profile_id, "web-project-v1");
    assert_eq!(stored.profile_version, 1);
    assert_eq!(stored.profile_digest.len(), 64);
    assert_eq!(stored.owner, agent(1));
    assert_eq!(
        stored.participants,
        vec![agent(1), agent(2), agent(3), agent(4), agent(5), agent(6)]
    );
    let cross_tenant = engine
        .execute(
            actor_for_tenant(ActorRole::ProjectManager, 1, "tenant-b"),
            "cross-tenant-plan",
            WorkflowCommand::PlanWorkGraph {
                project_id: project.id.clone(),
                expected_version: project.version,
                items: canonical_work_graph("cross-tenant"),
            },
        )
        .expect_err("matching agent ID in another tenant has no project authority");
    assert_eq!(cross_tenant.code, WorkflowErrorCode::Unauthorized);
    let projection = engine
        .project_projection(&project.id)
        .expect("read projection")
        .expect("projection exists");
    assert!(projection.last_event_sequence > 0);
    assert_eq!(projection.project_id, project.id);
    assert_eq!(projection.agreement.profile.id, "web-project-v1");
    assert_eq!(projection.agreement.profile.digest, stored.profile_digest);
}

#[test]
fn proposal_creation_rejects_an_unknown_profile_before_persisting_it() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (engine, _) = engine_at(
        &directory.path().join("workflow.db"),
        Arc::new(UnavailableExecutionPort),
    );
    let request = response_request(
        engine
            .execute(
                customer("unknown-profile-customer"),
                "submit-unknown-profile",
                WorkflowCommand::SubmitCustomerRequest {
                    summary_ref: "ref:unknown-profile".into(),
                    desired_outcome: "Fail closed".into(),
                    constraints: Vec::new(),
                },
            )
            .expect("submit request"),
    );
    let request = response_request(
        engine
            .execute(
                actor(ActorRole::Sales, 10),
                "qualify-unknown-profile",
                WorkflowCommand::QualifyCustomerRequest {
                    request_id: request.id,
                    expected_version: request.version,
                    reason: "bounded".into(),
                },
            )
            .expect("qualify request"),
    );
    let mut binding = proposal_binding(2_000_000);
    binding.governance.profile_id = "attacker-profile".into();
    let error = engine
        .execute(
            actor(ActorRole::Sales, 10),
            "proposal-unknown-profile",
            WorkflowCommand::CreateProposal {
                request_id: request.id,
                expected_version: request.version,
                binding,
            },
        )
        .expect_err("unknown profile must fail closed");
    assert_eq!(error.code, WorkflowErrorCode::DigestConflict);
}

#[test]
fn customer_state_machine_rejects_stale_expired_and_post_commit_mutations() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (engine, _) = engine_at(
        &directory.path().join("workflow.db"),
        Arc::new(UnavailableExecutionPort),
    );
    let submit = WorkflowCommand::SubmitCustomerRequest {
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
    let organization = Arc::new(FakeOrganizationPort::with_defaults());
    organization.set_profile(2, ActorRole::Developer, &[], Some(1), 2);
    let (engine, _) = engine_with_organization(
        &directory.path().join("workflow.db"),
        Arc::new(UnavailableExecutionPort),
        organization,
    );
    let (_, project) = bootstrap_project(&engine);
    let mut cyclic = canonical_work_graph("cycle");
    let developer_id = WorkItemId("cycle-developer".into());
    let qa_id = WorkItemId("cycle-qa".into());
    cyclic
        .iter_mut()
        .find(|item| item.id == developer_id)
        .expect("developer fixture")
        .dependency_ids
        .insert(qa_id);
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
        .work_item(&WorkItemId("cycle-developer".into()))
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

    let shortcut = canonical_work_graph("shortcut")
        .into_iter()
        .filter(|item| item.required_role == ActorRole::Developer)
        .collect();
    let error = engine
        .execute(
            actor(ActorRole::ProjectManager, 1),
            "plan-one-item-shortcut",
            WorkflowCommand::PlanWorkGraph {
                project_id: project.id.clone(),
                expected_version: project.version,
                items: shortcut,
            },
        )
        .expect_err("one work item cannot shortcut the canonical delivery topology");
    assert_eq!(error.code, WorkflowErrorCode::DigestConflict);

    let mut wrong_artifact_owner = canonical_work_graph("wrong-artifact-owner");
    wrong_artifact_owner
        .iter_mut()
        .find(|item| item.required_role == ActorRole::ProjectManager)
        .expect("project manager item")
        .required_output_kinds
        .remove("project_plan");
    wrong_artifact_owner
        .iter_mut()
        .find(|item| item.required_role == ActorRole::Developer)
        .expect("developer item")
        .required_output_kinds
        .insert("project_plan".into());
    let error = engine
        .execute(
            actor(ActorRole::ProjectManager, 1),
            "plan-wrong-artifact-owner",
            WorkflowCommand::PlanWorkGraph {
                project_id: project.id.clone(),
                expected_version: project.version,
                items: wrong_artifact_owner,
            },
        )
        .expect_err("canonical artifact ownership cannot be reassigned to another role");
    assert_eq!(error.code, WorkflowErrorCode::DigestConflict);

    engine
        .execute(
            actor(ActorRole::ProjectManager, 1),
            "plan-valid",
            WorkflowCommand::PlanWorkGraph {
                project_id: project.id.clone(),
                expected_version: project.version,
                items: canonical_work_graph("valid"),
            },
        )
        .expect("valid graph");
    let error = engine
        .execute(
            actor(ActorRole::ProjectManager, 1),
            "assign-invalid-capability",
            WorkflowCommand::AssignWork {
                work_item_id: WorkItemId("valid-developer".into()),
                expected_version: 1,
                assignee: agent(2),
                reason: "should be denied".into(),
            },
        )
        .expect_err("missing capability must fail");
    assert_eq!(error.code, WorkflowErrorCode::CapabilityDenied);
    assert_eq!(
        engine
            .work_item(&WorkItemId("valid-developer".into()))
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
                    project_id: project.id.clone(),
                    expected_version: project.version,
                    items: canonical_work_graph("exec"),
                },
            )
            .expect("plan graph");
        let assignment = assign_developer(&engine, "exec-developer", 1);
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
                .work_item(&WorkItemId("exec-developer".into()))
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
            .work_item(&WorkItemId("exec-developer".into()))
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
                items: canonical_work_graph("bounded-retry"),
            },
        )
        .expect("plan graph");
    let assignment = assign_developer(&engine, "bounded-retry-developer", 1);
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
    let completion = Arc::new(FakeCompletionPort::default());
    let (engine, _) = engine_with_ports(
        &directory.path().join("workflow.db"),
        Arc::new(FakeExecutionPort::default()),
        Arc::new(FakeOrganizationPort::with_defaults()),
        completion.clone(),
    );
    let (_, project) = bootstrap_project(&engine);
    engine
        .execute(
            actor(ActorRole::ProjectManager, 1),
            "plan-dag",
            WorkflowCommand::PlanWorkGraph {
                project_id: project.id,
                expected_version: project.version,
                items: canonical_work_graph("completion"),
            },
        )
        .expect("plan graph");
    let assignment = assign_developer(&engine, "completion-developer", 1);
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
    engine
        .execute(
            actor(ActorRole::Developer, 2),
            "complete-a",
            WorkflowCommand::RequestWorkCompletion {
                work_item_id: assignment.work_item_id,
                expected_version: 4,
                assignment_version: assignment.assignment_version,
            },
        )
        .expect("complete work");
    assert_eq!(completion.calls(), 1);
    assert_eq!(
        engine
            .work_item(&WorkItemId("completion-qa".into()))
            .expect("read dependent")
            .expect("dependent exists")
            .state,
        WorkItemState::Ready
    );
}

#[test]
fn completion_rejects_forged_authority_artifacts_gates_and_replay() {
    let cases = [
        (
            CompletionMode::ArtifactOwner(agent(3)),
            "artifact owned by another agent",
        ),
        (
            CompletionMode::Issuer(agent(2)),
            "self-issued or forged QA authority",
        ),
        (
            CompletionMode::Issuer(agent(7)),
            "non-participant authority",
        ),
        (
            CompletionMode::StaleIssuerAuthority,
            "stale issuer generation",
        ),
        (CompletionMode::FailedGate, "failed quality gate"),
        (
            CompletionMode::ReplayAssignment,
            "cross-assignment receipt replay",
        ),
        (
            CompletionMode::ReplayProjectVersion,
            "cross-project-version receipt replay",
        ),
        (
            CompletionMode::ReplayWorkItemVersion,
            "cross-work-item-version receipt replay",
        ),
        (CompletionMode::ReplayDomain, "cross-domain receipt replay"),
    ];
    for (index, (mode, label)) in cases.into_iter().enumerate() {
        let directory = tempfile::tempdir().expect("tempdir");
        let completion = Arc::new(FakeCompletionPort::default());
        completion.set_mode(mode);
        let (engine, _) = engine_with_ports(
            &directory.path().join("workflow.db"),
            Arc::new(FakeExecutionPort::default()),
            Arc::new(FakeOrganizationPort::with_defaults()),
            completion,
        );
        let (_, project) = bootstrap_project(&engine);
        engine
            .execute(
                actor(ActorRole::ProjectManager, 1),
                &format!("plan-negative-{index}"),
                WorkflowCommand::PlanWorkGraph {
                    project_id: project.id,
                    expected_version: project.version,
                    items: canonical_work_graph(&format!("negative-{index}")),
                },
            )
            .expect("plan graph");
        let assignment = assign_developer(&engine, &format!("negative-{index}-developer"), 1);
        engine
            .execute(
                actor(ActorRole::Developer, 2),
                &format!("claim-negative-{index}"),
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
            .dispatch_pending_executions(1)
            .expect("dispatch work");
        let error = engine
            .execute(
                actor(ActorRole::Developer, 2),
                &format!("complete-negative-{index}"),
                WorkflowCommand::RequestWorkCompletion {
                    work_item_id: assignment.work_item_id,
                    expected_version: 4,
                    assignment_version: assignment.assignment_version,
                },
            )
            .expect_err(label);
        assert_eq!(error.code, WorkflowErrorCode::DigestConflict, "{label}");
    }
}

#[test]
fn completion_rejects_revoked_issuer_authority() {
    let directory = tempfile::tempdir().expect("tempdir");
    let organization = Arc::new(FakeOrganizationPort::with_defaults());
    organization.set_active(3, false);
    let (engine, _) = engine_with_ports(
        &directory.path().join("workflow.db"),
        Arc::new(FakeExecutionPort::default()),
        organization,
        Arc::new(FakeCompletionPort::default()),
    );
    let assignment = prepare_completion(&engine, "completion-revoked");
    let error = engine
        .execute(
            actor(ActorRole::Developer, 2),
            "completion-revoked-request",
            WorkflowCommand::RequestWorkCompletion {
                work_item_id: assignment.work_item_id,
                expected_version: 4,
                assignment_version: assignment.assignment_version,
            },
        )
        .expect_err("revoked issuer authority must fail closed");
    assert_eq!(error.code, WorkflowErrorCode::DigestConflict);
}

#[test]
fn completion_outbox_recovers_timeout_restart_and_duplicate_without_double_commit() {
    let directory = tempfile::tempdir().expect("tempdir");
    let database = directory.path().join("workflow.db");
    let completion = Arc::new(FakeCompletionPort::default());
    completion.set_mode(CompletionMode::TimeoutOnce);
    let (engine, store) = engine_with_ports(
        &database,
        Arc::new(FakeExecutionPort::default()),
        Arc::new(FakeOrganizationPort::with_defaults()),
        completion.clone(),
    );
    let assignment = prepare_completion(&engine, "completion-restart");
    let command = WorkflowCommand::RequestWorkCompletion {
        work_item_id: assignment.work_item_id.clone(),
        expected_version: 4,
        assignment_version: assignment.assignment_version,
    };
    let timeout = engine
        .execute(
            actor(ActorRole::Developer, 2),
            "completion-timeout",
            command.clone(),
        )
        .expect_err("first authority lookup times out");
    assert_eq!(timeout.code, WorkflowErrorCode::CompletionUnavailable);
    assert_eq!(
        store
            .pending_completion_evidence(10)
            .expect("pending")
            .len(),
        1
    );
    assert_eq!(
        engine
            .work_item(&assignment.work_item_id)
            .expect("work item")
            .expect("present")
            .state,
        WorkItemState::InProgress
    );
    drop(engine);
    drop(store);

    let (restarted, _) = engine_with_ports(
        &database,
        Arc::new(FakeExecutionPort::default()),
        Arc::new(FakeOrganizationPort::with_defaults()),
        completion.clone(),
    );
    assert_eq!(
        restarted
            .dispatch_pending_completion_evidence(10)
            .expect("restart resolves durable request")
            .len(),
        1
    );
    assert_eq!(completion.calls(), 2);
    let replay = restarted
        .execute(
            actor(ActorRole::Developer, 2),
            "completion-timeout",
            command.clone(),
        )
        .expect("duplicate command reads completed result");
    assert!(replay.replayed);
    assert_eq!(completion.calls(), 2);
    assert_eq!(
        restarted
            .work_item(&assignment.work_item_id)
            .expect("work item")
            .expect("present")
            .state,
        WorkItemState::Done
    );

    drop(restarted);
    let (degraded, _) = engine_with_ports(
        &database,
        Arc::new(FakeExecutionPort::default()),
        Arc::new(FakeOrganizationPort::with_defaults()),
        Arc::new(UnavailableCompletionEvidencePort),
    );
    let degraded_replay = degraded
        .execute(
            actor(ActorRole::Developer, 2),
            "completion-timeout",
            command,
        )
        .expect("durable completion replay does not require a live authority");
    assert!(degraded_replay.replayed);
}

#[test]
fn completion_outbox_survives_crash_after_authority_response_before_cas_commit() {
    let directory = tempfile::tempdir().expect("tempdir");
    let database = directory.path().join("workflow.db");
    let completion = Arc::new(FakeCompletionPort::default());
    completion.set_mode(CompletionMode::CrashAfterAuthority);
    let (engine, store) = engine_with_ports(
        &database,
        Arc::new(FakeExecutionPort::default()),
        Arc::new(FakeOrganizationPort::with_defaults()),
        completion.clone(),
    );
    let assignment = prepare_completion(&engine, "completion-crash");
    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = engine.execute(
            actor(ActorRole::Developer, 2),
            "completion-crash-request",
            WorkflowCommand::RequestWorkCompletion {
                work_item_id: assignment.work_item_id.clone(),
                expected_version: 4,
                assignment_version: assignment.assignment_version,
            },
        );
    }));
    assert!(crashed.is_err());
    drop(engine);
    drop(store);

    let (restarted, _) = engine_with_ports(
        &database,
        Arc::new(FakeExecutionPort::default()),
        Arc::new(FakeOrganizationPort::with_defaults()),
        completion.clone(),
    );
    restarted
        .dispatch_pending_completion_evidence(10)
        .expect("same digest-bound authority request is safe to retry after restart");
    assert_eq!(completion.calls(), 2);
    assert_eq!(
        restarted
            .work_item(&assignment.work_item_id)
            .expect("work item")
            .expect("present")
            .state,
        WorkItemState::Done
    );
}

#[test]
fn completion_outbox_is_bounded_when_evidence_authority_keeps_timing_out() {
    let directory = tempfile::tempdir().expect("tempdir");
    let completion = Arc::new(FakeCompletionPort::default());
    completion.set_mode(CompletionMode::AlwaysTimeout);
    let (engine, store) = engine_with_ports(
        &directory.path().join("workflow.db"),
        Arc::new(FakeExecutionPort::default()),
        Arc::new(FakeOrganizationPort::with_defaults()),
        completion.clone(),
    );
    let assignment = prepare_completion(&engine, "completion-bounded-timeout");
    let error = engine
        .execute(
            actor(ActorRole::Developer, 2),
            "completion-bounded-timeout-request",
            WorkflowCommand::RequestWorkCompletion {
                work_item_id: assignment.work_item_id,
                expected_version: 4,
                assignment_version: assignment.assignment_version,
            },
        )
        .expect_err("initial authority timeout");
    assert_eq!(error.code, WorkflowErrorCode::CompletionUnavailable);
    for _ in 0..2 {
        let error = engine
            .dispatch_pending_completion_evidence(1)
            .expect_err("bounded authority retry times out");
        assert_eq!(error.code, WorkflowErrorCode::CompletionUnavailable);
    }
    assert_eq!(completion.calls(), 3);
    assert!(engine
        .dispatch_pending_completion_evidence(1)
        .expect("terminal request is no longer retried")
        .is_empty());
    assert_eq!(completion.calls(), 3);
    let (_, state) = store
        .completion_evidence_request("completion-work-exec-completion-bounded-timeout-developer-v1")
        .expect("read completion outbox")
        .expect("completion outbox entry");
    assert_eq!(state, "failed");
}

#[test]
fn completion_cas_rejects_project_drift_after_evidence_request() {
    let directory = tempfile::tempdir().expect("tempdir");
    let completion = Arc::new(FakeCompletionPort::default());
    completion.set_mode(CompletionMode::TimeoutOnce);
    let (engine, store) = engine_with_ports(
        &directory.path().join("workflow.db"),
        Arc::new(FakeExecutionPort::default()),
        Arc::new(FakeOrganizationPort::with_defaults()),
        completion.clone(),
    );
    let assignment = prepare_completion(&engine, "completion-toctou");
    engine
        .execute(
            actor(ActorRole::Developer, 2),
            "completion-toctou-request",
            WorkflowCommand::RequestWorkCompletion {
                work_item_id: assignment.work_item_id,
                expected_version: 4,
                assignment_version: assignment.assignment_version,
            },
        )
        .expect_err("tx1 survives the authority timeout");
    engine
        .execute(
            actor(ActorRole::Developer, 2),
            "completion-toctou-drift",
            WorkflowCommand::RaiseBlocker {
                project_id: assignment.project_id,
                work_item_id: None,
                cause_ref: "authority-review".into(),
                impact: "project authority changed while evidence was pending".into(),
                owner: agent(2),
                required_resolution_role: ActorRole::ProjectManager,
            },
        )
        .expect("mutate project after tx1");
    let error = engine
        .dispatch_pending_completion_evidence(1)
        .expect_err("tx2 rejects stale project version");
    assert_eq!(error.code, WorkflowErrorCode::VersionConflict);
    assert_eq!(completion.calls(), 2);
    let (_, state) = store
        .completion_evidence_request("completion-work-exec-completion-toctou-developer-v1")
        .expect("read completion outbox")
        .expect("completion outbox entry");
    assert_eq!(state, "failed");
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
                items: canonical_work_graph("cost"),
            },
        )
        .expect("activate project with a bounded graph");
    let blocker = engine
        .execute(
            actor(ActorRole::Developer, 2),
            "raise-delivery-blocker",
            WorkflowCommand::RaiseBlocker {
                project_id: project.id.clone(),
                work_item_id: Some(WorkItemId("cost-developer".into())),
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
                work_item_id: Some(WorkItemId("cost-developer".into())),
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
                work_item_id: Some(WorkItemId("cost-developer".into())),
                owner: agent(3),
                question_ref: "question:approved-copy".into(),
            },
        )
        .expect("record unresolved structured question");
    let assignment = assign_developer(&engine, "cost-developer", 3);
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
            WorkflowCommand::RequestWorkCompletion {
                work_item_id: assignment.work_item_id,
                expected_version: 6,
                assignment_version: assignment.assignment_version,
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
    assert_eq!(projection.state, ProjectState::Active);
    let events = engine
        .events_since(&actor(ActorRole::ProjectManager, 1), 0, 1_000)
        .expect("read event stream");
    assert!(events
        .windows(2)
        .all(|pair| pair[0].sequence < pair[1].sequence));
    assert!(events
        .iter()
        .all(|event| !event.operation_digest.is_empty() && event.schema_version == 2));

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
    assert_eq!(recovered.state, ProjectState::Active);
}

#[test]
fn record_decision_rejects_cross_project_and_cross_tenant_work_items() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (engine, _) = engine_at(
        &directory.path().join("workflow.db"),
        Arc::new(FakeExecutionPort::default()),
    );
    let (_, project_a) = bootstrap_project_for(&engine, "decision-a", "tenant-a", "customer-a");
    let (_, project_b) = bootstrap_project_for(&engine, "decision-b", "tenant-a", "customer-b");
    let (_, project_c) = bootstrap_project_for(&engine, "decision-c", "tenant-b", "customer-c");
    for (suffix, project, tenant) in [
        ("decision-a", &project_a, "tenant-a"),
        ("decision-b", &project_b, "tenant-a"),
        ("decision-c", &project_c, "tenant-b"),
    ] {
        engine
            .execute(
                actor_for_tenant(ActorRole::ProjectManager, 1, tenant),
                &format!("plan-{suffix}"),
                WorkflowCommand::PlanWorkGraph {
                    project_id: project.id.clone(),
                    expected_version: project.version,
                    items: canonical_work_graph(suffix),
                },
            )
            .expect("plan fixture project");
    }
    let decision = |operation: &str, work_item_id: WorkItemId| {
        engine.execute(
            actor(ActorRole::ProjectManager, 1),
            operation,
            WorkflowCommand::RecordDecision {
                project_id: project_a.id.clone(),
                work_item_id: Some(work_item_id),
                choice: "invalid cross-boundary choice".into(),
                alternatives: Vec::new(),
                rationale_ref: "sha256:rationale".into(),
                evidence_refs: Vec::new(),
            },
        )
    };
    assert_eq!(
        decision(
            "decision-cross-project",
            WorkItemId("decision-b-developer".into())
        )
        .expect_err("work item from another project")
        .code,
        WorkflowErrorCode::Unauthorized
    );
    assert_eq!(
        decision(
            "decision-cross-tenant",
            WorkItemId("decision-c-developer".into())
        )
        .expect_err("work item from another tenant")
        .code,
        WorkflowErrorCode::Unauthorized
    );
}

#[test]
fn principal_scoped_idempotency_and_tenant_reads_fail_closed() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (engine, _) = engine_at(
        &directory.path().join("workflow.db"),
        Arc::new(FakeExecutionPort::default()),
    );
    let command = WorkflowCommand::SubmitCustomerRequest {
        summary_ref: "ref:request/shared".into(),
        desired_outcome: "Tenant-isolated delivery".into(),
        constraints: vec!["No cross-tenant access".into()],
    };

    let request_a = response_request(
        engine
            .execute(
                customer_for_tenant("customer-a", "tenant-a"),
                "shared-operation",
                command.clone(),
            )
            .expect("tenant A uses operation id"),
    );
    let request_b = response_request(
        engine
            .execute(
                customer_for_tenant("customer-b", "tenant-b"),
                "shared-operation",
                command,
            )
            .expect("tenant B independently uses the same operation id"),
    );
    assert_ne!(request_a.id, request_b.id);
    assert_eq!(request_a.tenant_id, "tenant-a");
    assert_eq!(request_b.tenant_id, "tenant-b");

    let conflict = engine
        .execute(
            customer_for_tenant("customer-a", "tenant-a"),
            "shared-operation",
            WorkflowCommand::SubmitCustomerRequest {
                summary_ref: "ref:request/different".into(),
                desired_outcome: "Different payload".into(),
                constraints: Vec::new(),
            },
        )
        .expect_err("same principal namespace cannot reuse an operation id");
    assert_eq!(conflict.code, WorkflowErrorCode::IdempotencyConflict);

    let denied = engine
        .customer_request_for(
            &customer_for_tenant("customer-b", "tenant-b"),
            &request_a.id,
        )
        .expect_err("cross-tenant request read must fail closed");
    assert_eq!(denied.code, WorkflowErrorCode::Unauthorized);
    let mutation_denied = engine
        .execute(
            actor_for_tenant(ActorRole::Sales, 10, "tenant-b"),
            "cross-tenant-qualify",
            WorkflowCommand::QualifyCustomerRequest {
                request_id: request_a.id.clone(),
                expected_version: request_a.version,
                reason: "caller must not cross tenant boundaries".into(),
            },
        )
        .expect_err("cross-tenant mutation must fail closed");
    assert_eq!(mutation_denied.code, WorkflowErrorCode::Unauthorized);

    let events_a = engine
        .events_since(
            &actor_for_tenant(ActorRole::ProjectManager, 1, "tenant-a"),
            0,
            100,
        )
        .expect("tenant A event stream");
    let events_b = engine
        .events_since(
            &actor_for_tenant(ActorRole::ProjectManager, 1, "tenant-b"),
            0,
            100,
        )
        .expect("tenant B event stream");
    assert!(!events_a.is_empty());
    assert!(!events_b.is_empty());
    assert!(events_a.iter().all(|event| event.tenant_id == "tenant-a"));
    assert!(events_b.iter().all(|event| event.tenant_id == "tenant-b"));
    assert!(events_a
        .iter()
        .all(|event| event.principal_id == "customer-user-customer-a"));
    assert!(events_b
        .iter()
        .all(|event| event.principal_id == "customer-user-customer-b"));
}

#[test]
fn authority_drift_is_durable_recoverable_and_does_not_poison_other_rows() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("workflow.db");
    let execution = Arc::new(FakeExecutionPort::default());
    let organization = Arc::new(FakeOrganizationPort::with_defaults());
    let (engine, _) = engine_with_organization(&path, execution.clone(), organization.clone());
    let (_, project) = bootstrap_project(&engine);
    engine
        .execute(
            actor(ActorRole::ProjectManager, 1),
            "plan-toctou",
            WorkflowCommand::PlanWorkGraph {
                project_id: project.id.clone(),
                expected_version: project.version,
                items: canonical_work_graph("toctou"),
            },
        )
        .expect("plan work");
    let assignment = assign_developer(&engine, "toctou-developer", 1);
    let developer_capabilities = ["web_development", "artifact_authoring", "test_execution"];

    organization.set_profile(2, ActorRole::Developer, &developer_capabilities, Some(1), 2);
    let stale_claim = engine
        .execute(
            agent_principal(ActorRole::Developer, 2, "tenant-a"),
            "claim-stale-authority",
            WorkflowCommand::ClaimWork {
                work_item_id: assignment.work_item_id.clone(),
                expected_version: 2,
                agent_id: agent(2),
                input_digest: "a".repeat(64),
                deadline_ms: 2_000_000,
            },
        )
        .expect_err("organization change after assignment blocks claim");
    assert_eq!(
        stale_claim.code,
        WorkflowErrorCode::OrganizationAuthorityConflict
    );
    assert!(execution.calls().is_empty());

    organization.set_profile(2, ActorRole::Developer, &developer_capabilities, Some(1), 1);
    engine
        .execute(
            agent_principal(ActorRole::Developer, 2, "tenant-a"),
            "claim-current-authority",
            WorkflowCommand::ClaimWork {
                work_item_id: assignment.work_item_id,
                expected_version: 2,
                agent_id: agent(2),
                input_digest: "b".repeat(64),
                deadline_ms: 2_000_000,
            },
        )
        .expect("unchanged authoritative snapshot allows claim");
    let designer_assignment = match engine
        .execute(
            actor(ActorRole::ProjectManager, 1),
            "assign-independent-designer",
            WorkflowCommand::AssignWork {
                work_item_id: WorkItemId("toctou-design".into()),
                expected_version: 1,
                assignee: agent(4),
                reason: "independent work row".into(),
            },
        )
        .expect("assign independent designer")
        .response
    {
        WorkflowResponse::Assignment(value) => value,
        other => panic!("expected assignment, got {other:?}"),
    };
    engine
        .execute(
            agent_principal(ActorRole::Designer, 4, "tenant-a"),
            "claim-independent-designer",
            WorkflowCommand::ClaimWork {
                work_item_id: designer_assignment.work_item_id,
                expected_version: 2,
                agent_id: agent(4),
                input_digest: "c".repeat(64),
                deadline_ms: 2_000_000,
            },
        )
        .expect("claim independent row");
    organization.set_profile(2, ActorRole::Developer, &developer_capabilities, Some(1), 3);
    let receipts = engine
        .dispatch_pending_executions(10)
        .expect("authority conflict is isolated without poisoning independent rows");
    assert_eq!(receipts.len(), 1);
    assert_eq!(execution.calls().len(), 1);
    assert!(execution.calls()[0].contains("toctou-design"));
    assert_eq!(
        engine
            .work_item(&WorkItemId("toctou-developer".into()))
            .expect("read work item")
            .expect("work item exists")
            .state,
        WorkItemState::Blocked
    );
    let projection = engine
        .project_projection(&project.id)
        .expect("projection")
        .expect("project projection");
    assert_eq!(projection.authority_conflicts.len(), 1);
    assert_eq!(
        projection.authority_conflicts[0].state,
        AuthorityConflictState::Open
    );
    let conflict_id = projection.authority_conflicts[0].id.clone();
    drop(engine);

    let (restarted, _) = engine_with_organization(&path, execution.clone(), organization.clone());
    assert!(restarted
        .dispatch_pending_executions(10)
        .expect("terminal conflict row is not retried after restart")
        .is_empty());
    assert_eq!(execution.calls().len(), 1);
    restarted
        .execute(
            actor(ActorRole::ProjectManager, 1),
            "resolve-authority-conflict",
            WorkflowCommand::ResolveAuthorityConflict {
                conflict_id,
                resolution_ref: "organization:snapshot-reviewed".into(),
            },
        )
        .expect("resolve into reassignment-ready state");
    let reassignment = assign_developer(&restarted, "toctou-developer", 5);
    restarted
        .execute(
            agent_principal(ActorRole::Developer, 2, "tenant-a"),
            "claim-after-authority-resolution",
            WorkflowCommand::ClaimWork {
                work_item_id: reassignment.work_item_id,
                expected_version: 6,
                agent_id: agent(2),
                input_digest: "d".repeat(64),
                deadline_ms: 2_000_000,
            },
        )
        .expect("resolved work is claimable with a fresh authority snapshot");
    assert_eq!(
        restarted
            .dispatch_pending_executions(10)
            .expect("fresh invocation dispatches exactly once")
            .len(),
        1
    );
    assert_eq!(execution.calls().len(), 2);
}

#[test]
fn backup_restore_restart_and_projection_rebuild_preserve_verified_state() {
    let directory = tempfile::tempdir().expect("tempdir");
    let source = directory.path().join("workflow.db");
    let backup = directory.path().join("workflow.backup.db");
    let restored = directory.path().join("workflow.restored.db");
    let rejected_restore = directory.path().join("workflow.rejected.db");
    let project_id;
    let request_id;
    let manifest;
    {
        let (engine, store) = engine_at(&source, Arc::new(FakeExecutionPort::default()));
        let (created_request_id, project) = bootstrap_project(&engine);
        request_id = created_request_id;
        project_id = project.id;
        let before = engine
            .projection_checkpoint()
            .expect("read projection checkpoint before backup");
        assert_eq!(
            before.source_event_high_watermark,
            before.projected_event_high_watermark
        );
        manifest = store.backup_to(&backup).expect("create consistent backup");
        assert_eq!(manifest.projection_checkpoint, before);
        assert!(manifest.entity_history_high_watermark > 0);
        assert!(manifest.entity_count > 0);
        assert!(manifest.operation_count > 0);
        assert_eq!(manifest.project_projection_count, 1);
    }

    WorkflowStore::restore_from_backup(&backup, &restored, &manifest)
        .expect("restore verified image into an offline destination");
    let mut invalid_manifest = manifest.clone();
    invalid_manifest.database_sha256 = "0".repeat(64);
    let rejected =
        WorkflowStore::restore_from_backup(&backup, &rejected_restore, &invalid_manifest)
            .expect_err("restore must reject a mismatched manifest");
    assert_eq!(rejected.code, WorkflowErrorCode::BackupVerificationFailed);
    assert!(!rejected_restore.exists());

    let (restarted, _) = engine_at(&restored, Arc::new(FakeExecutionPort::default()));
    let project = restarted
        .project_for(&actor(ActorRole::ProjectManager, 1), &project_id)
        .expect("authorized restored project read")
        .expect("restored project exists");
    assert_eq!(project.tenant_id, "tenant-a");
    let request = restarted
        .customer_request_for(&customer("customer-a"), &request_id)
        .expect("authorized restored request read")
        .expect("restored request exists");
    assert_eq!(request.state, CustomerRequestState::Accepted);
    let replay = restarted
        .execute(
            customer("customer-a"),
            "submit-a",
            WorkflowCommand::SubmitCustomerRequest {
                summary_ref: "ref:request/sha256:1111".into(),
                desired_outcome: "Deliver the agreed web project".into(),
                constraints: vec!["No production mutation".into()],
            },
        )
        .expect("idempotency record survives restore");
    assert!(replay.replayed);

    let rebuilt = restarted
        .rebuild_project_projections()
        .expect("rebuild projections from authoritative state");
    assert_eq!(
        rebuilt.source_event_high_watermark,
        rebuilt.projected_event_high_watermark
    );
    assert_eq!(rebuilt.project_count, 1);
    assert!(rebuilt.rebuilt_at_ms.is_some());
    let projection = restarted
        .project_projection_for(&actor(ActorRole::ProjectManager, 1), &project_id)
        .expect("authorized rebuilt projection read")
        .expect("rebuilt projection exists");
    assert_eq!(projection.project_id, project_id);
    assert_eq!(projection.tenant_id, "tenant-a");
}
