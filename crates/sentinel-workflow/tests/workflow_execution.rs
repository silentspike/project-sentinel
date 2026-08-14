use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use sentinel_workflow::{
    sealed_output_bundle_digest, AgentId, ArtifactExpectationV1, ArtifactInputV1, CommandRuleV1,
    CompletionEvidencePort, DependencyReadiness, ExecutionPlanV1, ExecutionResourceBoundsV1,
    ExecutionStepV1, ExecutionToolV1, GateEvidencePort, GateExpectationV1, IndependentGateEvidence,
    OrganizationRuntimePort, OutputExpectationV1, PendingCompletionEvidenceV1, PendingExecutionV1,
    PendingGateEvidenceV1, PrincipalAuthorityV1, ProjectId, RuntimeAuthoritySnapshotV1,
    SealedArtifactEvidenceV1, SealedOutputEvidenceV1, TenantId, TerminalExecutionEvidence,
    UnavailableGateEvidencePort, WorkExecutionObservation, WorkExecutionPort, WorkItemExecutionV1,
    WorkItemId, WorkItemState, WorkflowCore, WorkflowErrorCode, WorkflowPortError, WorkflowStore,
    EXECUTION_PLAN_SCHEMA_VERSION, WORKFLOW_SCHEMA_VERSION, WORKFLOW_STORE_SCHEMA_VERSION,
};
use uuid::Uuid;

const NOW: u64 = 1_900_000_000_000;

#[derive(Clone)]
struct FakeOrganization {
    snapshot: Arc<Mutex<RuntimeAuthoritySnapshotV1>>,
}

impl OrganizationRuntimePort for FakeOrganization {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Ready
    }

    fn authority_snapshot(
        &self,
        tenant_id: &TenantId,
        project_id: &ProjectId,
        work_item_id: &WorkItemId,
        agent_id: AgentId,
    ) -> Result<RuntimeAuthoritySnapshotV1, WorkflowPortError> {
        let snapshot = self.snapshot.lock().unwrap().clone();
        if snapshot.tenant_id != *tenant_id
            || snapshot.project_id != *project_id
            || snapshot.work_item_id != *work_item_id
            || snapshot.agent_id != agent_id
        {
            return Err(WorkflowPortError::AuthorityConflict);
        }
        Ok(snapshot)
    }
}

#[derive(Clone)]
struct CountingOrganization {
    snapshot: RuntimeAuthoritySnapshotV1,
    calls: Arc<AtomicUsize>,
}

impl OrganizationRuntimePort for CountingOrganization {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Ready
    }

    fn authority_snapshot(
        &self,
        tenant_id: &TenantId,
        project_id: &ProjectId,
        work_item_id: &WorkItemId,
        agent_id: AgentId,
    ) -> Result<RuntimeAuthoritySnapshotV1, WorkflowPortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.snapshot.tenant_id != *tenant_id
            || self.snapshot.project_id != *project_id
            || self.snapshot.work_item_id != *work_item_id
            || self.snapshot.agent_id != agent_id
        {
            return Err(WorkflowPortError::AuthorityConflict);
        }
        Ok(self.snapshot.clone())
    }
}

#[derive(Clone)]
struct UncheckedOrganization {
    snapshot: RuntimeAuthoritySnapshotV1,
    calls: Arc<AtomicUsize>,
}

impl OrganizationRuntimePort for UncheckedOrganization {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Ready
    }

    fn authority_snapshot(
        &self,
        _tenant_id: &TenantId,
        _project_id: &ProjectId,
        _work_item_id: &WorkItemId,
        _agent_id: AgentId,
    ) -> Result<RuntimeAuthoritySnapshotV1, WorkflowPortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.snapshot.clone())
    }
}

#[derive(Clone)]
struct FakeExecution {
    observations: Arc<Mutex<VecDeque<WorkExecutionObservation>>>,
    calls: Arc<AtomicUsize>,
}

impl WorkExecutionPort for FakeExecution {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Ready
    }

    fn reconcile(
        &self,
        _request: &PendingExecutionV1,
    ) -> Result<WorkExecutionObservation, WorkflowPortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.observations
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(WorkflowPortError::Unavailable)
    }
}

#[derive(Clone)]
struct FakeCompletion {
    calls: Arc<AtomicUsize>,
}

struct CompletionReceipt {
    request: PendingCompletionEvidenceV1,
    output_bundle_digest: String,
    outputs: Vec<SealedOutputEvidenceV1>,
    artifacts: Vec<SealedArtifactEvidenceV1>,
    completed_at_unix_ms: u64,
}

impl TerminalExecutionEvidence for CompletionReceipt {
    fn schema_version(&self) -> u16 {
        WORKFLOW_SCHEMA_VERSION
    }

    fn receipt_id(&self) -> &str {
        "workbench-root-receipt-v1"
    }

    fn invocation_id(&self) -> Uuid {
        self.request.invocation_id
    }

    fn plan_digest(&self) -> &str {
        &self.request.plan_digest
    }

    fn step_digest(&self) -> &str {
        &self.request.step_digest
    }

    fn output_bundle_digest(&self) -> &str {
        &self.output_bundle_digest
    }

    fn outputs(&self) -> &[SealedOutputEvidenceV1] {
        &self.outputs
    }

    fn artifacts(&self) -> &[SealedArtifactEvidenceV1] {
        &self.artifacts
    }

    fn completed_at_unix_ms(&self) -> u64 {
        self.completed_at_unix_ms
    }
}

impl CompletionEvidencePort for FakeCompletion {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Ready
    }

    fn terminal_evidence(
        &self,
        request: &PendingCompletionEvidenceV1,
    ) -> Result<Box<dyn TerminalExecutionEvidence>, WorkflowPortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let first_step =
            request.step_id == Uuid::parse_str("018f3f32-4f01-7f2c-a6c1-f6f4a81b2801").unwrap();
        let outputs = vec![SealedOutputEvidenceV1 {
            name: if first_step { "source" } else { "bundle" }.to_owned(),
            kind: "source_tree".to_owned(),
            digest_algorithm: "sha256".to_owned(),
            digest: "b".repeat(64),
        }];
        let artifacts = if first_step {
            Vec::new()
        } else {
            vec![SealedArtifactEvidenceV1 {
                artifact_kind: "source_tree".to_owned(),
                media_type: "application/vnd.sentinel.source-tree".to_owned(),
                paths: vec!["src".to_owned()],
                digest: "d".repeat(64),
            }]
        };
        Ok(Box::new(CompletionReceipt {
            request: request.clone(),
            output_bundle_digest: sealed_output_bundle_digest(&outputs, &artifacts).unwrap(),
            outputs,
            artifacts,
            completed_at_unix_ms: request.created_at_unix_ms + 1,
        }))
    }
}

#[derive(Clone, Default)]
struct FakeGate {
    calls: Arc<AtomicUsize>,
}

struct GateReceipt {
    request: PendingGateEvidenceV1,
    completed_at_unix_ms: u64,
}

impl IndependentGateEvidence for GateReceipt {
    fn schema_version(&self) -> u16 {
        WORKFLOW_SCHEMA_VERSION
    }

    fn receipt_id(&self) -> &str {
        "independent-work-item-gate-v1"
    }

    fn profile_id(&self) -> &str {
        &self.request.expectation.profile_id
    }

    fn profile_generation(&self) -> u64 {
        self.request.expectation.profile_generation
    }

    fn profile_digest(&self) -> &str {
        &self.request.expectation.profile_digest
    }

    fn subject_digest(&self) -> &str {
        &self.request.subject_digest
    }

    fn required_checks_digest(&self) -> &str {
        &self.request.required_checks_digest
    }

    fn passed(&self) -> bool {
        true
    }

    fn completed_at_unix_ms(&self) -> u64 {
        self.completed_at_unix_ms
    }
}

impl GateEvidencePort for FakeGate {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Ready
    }

    fn gate_evidence(
        &self,
        request: &PendingGateEvidenceV1,
    ) -> Result<Box<dyn IndependentGateEvidence>, WorkflowPortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(GateReceipt {
            request: request.clone(),
            completed_at_unix_ms: request.created_at_unix_ms + 1,
        }))
    }
}

fn authority() -> RuntimeAuthoritySnapshotV1 {
    RuntimeAuthoritySnapshotV1 {
        schema_version: WORKFLOW_SCHEMA_VERSION,
        tenant_id: TenantId::parse("tenant-01").unwrap(),
        project_id: ProjectId::parse("project-01").unwrap(),
        work_item_id: WorkItemId::parse("work-01").unwrap(),
        agent_id: AgentId(7),
        assignment_version: 3,
        assignment_digest: "1".repeat(64),
        organization_generation: 9,
        organization_digest: "2".repeat(64),
        principal: PrincipalAuthorityV1::derive("agent-07", 4, &[0x5a; 32]).unwrap(),
        profile_id: "web-authoring-v1".to_owned(),
        profile_generation: 2,
        profile_digest: "3".repeat(64),
        runtime_key: "bwrap-web-v1".to_owned(),
        runtime_generation: 2,
        runtime_digest: "4".repeat(64),
        policy_generation: 6,
        policy_digest: "5".repeat(64),
        active: true,
        capabilities: BTreeSet::from([
            "artifact.commit".to_owned(),
            "file.write".to_owned(),
            "test.run_profile".to_owned(),
        ]),
    }
}

fn gate_expectation() -> GateExpectationV1 {
    GateExpectationV1 {
        profile_id: "web-work-item-qa-v1".to_owned(),
        profile_generation: 1,
        profile_digest: "9".repeat(64),
        required_checks: BTreeSet::from([
            "html_structure".to_owned(),
            "static_security".to_owned(),
        ]),
    }
}

fn write_step() -> ExecutionStepV1 {
    ExecutionStepV1 {
        step_id: Uuid::parse_str("018f3f32-4f01-7f2c-a6c1-f6f4a81b2801").unwrap(),
        invocation_id: Uuid::parse_str("018f3f32-4f01-7f2c-a6c1-f6f4a81b2802").unwrap(),
        ordinal: 0,
        workspace_id: "project-01:work-01".to_owned(),
        capabilities: BTreeSet::from(["file.write".to_owned()]),
        inputs: Vec::new(),
        command_policy: Vec::new(),
        tool: ExecutionToolV1::WriteFile {
            path: "src/index.html".to_owned(),
            content: "<!doctype html>".to_owned(),
            expected_sha256: None,
        },
        outputs: vec![OutputExpectationV1 {
            name: "source".to_owned(),
            kind: "source_tree".to_owned(),
            required: true,
            digest_algorithm: "sha256".to_owned(),
        }],
        artifacts: Vec::new(),
        gate_expectation: gate_expectation(),
        resource_bounds: bounds(),
        deadline_unix_ms: NOW + 50_000,
    }
}

fn package_step() -> ExecutionStepV1 {
    ExecutionStepV1 {
        step_id: Uuid::parse_str("018f3f32-4f01-7f2c-a6c1-f6f4a81b2803").unwrap(),
        invocation_id: Uuid::parse_str("018f3f32-4f01-7f2c-a6c1-f6f4a81b2804").unwrap(),
        ordinal: 1,
        workspace_id: "project-01:work-01".to_owned(),
        capabilities: BTreeSet::from(["artifact.commit".to_owned()]),
        inputs: Vec::new(),
        command_policy: Vec::new(),
        tool: ExecutionToolV1::PackageArtifact {
            artifact_kind: "source_tree".to_owned(),
            media_type: "application/vnd.sentinel.source-tree".to_owned(),
            paths: vec!["src".to_owned()],
        },
        outputs: vec![OutputExpectationV1 {
            name: "bundle".to_owned(),
            kind: "source_tree".to_owned(),
            required: true,
            digest_algorithm: "sha256".to_owned(),
        }],
        artifacts: vec![ArtifactExpectationV1 {
            artifact_kind: "source_tree".to_owned(),
            media_type: "application/vnd.sentinel.source-tree".to_owned(),
            required_paths: vec!["src".to_owned()],
        }],
        gate_expectation: gate_expectation(),
        resource_bounds: bounds(),
        deadline_unix_ms: NOW + 60_000,
    }
}

fn bounds() -> ExecutionResourceBoundsV1 {
    ExecutionResourceBoundsV1 {
        wall_time_ms: 30_000,
        cpu_time_ms: 10_000,
        memory_bytes: 128 * 1024 * 1024,
        process_count: 16,
        file_bytes: 8 * 1024 * 1024,
        stdout_bytes: 64 * 1024,
        stderr_bytes: 64 * 1024,
    }
}

fn plan(step_count: usize) -> ExecutionPlanV1 {
    let authority = authority();
    let mut steps = vec![write_step()];
    if step_count == 2 {
        steps.push(package_step());
    }
    ExecutionPlanV1 {
        schema_version: EXECUTION_PLAN_SCHEMA_VERSION,
        plan_id: Uuid::parse_str("018f3f32-4f01-7f2c-a6c1-f6f4a81b2800").unwrap(),
        tenant_id: authority.tenant_id,
        project_id: authority.project_id,
        work_item_id: authority.work_item_id,
        agent_id: authority.agent_id,
        workspace_id: "project-01:work-01".to_owned(),
        assignment_version: authority.assignment_version,
        assignment_digest: authority.assignment_digest,
        organization_generation: authority.organization_generation,
        organization_digest: authority.organization_digest,
        principal: authority.principal,
        profile_id: authority.profile_id,
        profile_generation: authority.profile_generation,
        profile_digest: authority.profile_digest,
        runtime_key: authority.runtime_key,
        runtime_generation: authority.runtime_generation,
        runtime_digest: authority.runtime_digest,
        policy_generation: authority.policy_generation,
        policy_digest: authority.policy_digest,
        created_at_unix_ms: NOW - 1,
        deadline_unix_ms: NOW + 60_000,
        steps,
        request_digest: String::new(),
    }
    .bind_digest()
    .unwrap()
}

fn package_only_plan() -> ExecutionPlanV1 {
    let mut value = plan(1);
    let mut step = package_step();
    step.ordinal = 0;
    value.steps = vec![step];
    value.bind_digest().unwrap()
}

fn core(
    path: &std::path::Path,
    observations: Vec<WorkExecutionObservation>,
) -> WorkflowCore<FakeOrganization, FakeExecution, FakeCompletion, FakeGate> {
    WorkflowCore::new(
        WorkflowStore::open(path).unwrap(),
        FakeOrganization {
            snapshot: Arc::new(Mutex::new(authority())),
        },
        FakeExecution {
            observations: Arc::new(Mutex::new(observations.into())),
            calls: Arc::new(AtomicUsize::new(0)),
        },
        FakeCompletion {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        FakeGate::default(),
    )
}

#[test]
fn aggregate_readiness_requires_every_productive_port() {
    let directory = tempfile::tempdir().unwrap();
    assert!(core(&directory.path().join("ready.sqlite"), Vec::new()).dependencies_ready());

    let unavailable_gate = WorkflowCore::new(
        WorkflowStore::open(directory.path().join("unavailable.sqlite")).unwrap(),
        FakeOrganization {
            snapshot: Arc::new(Mutex::new(authority())),
        },
        FakeExecution {
            observations: Arc::new(Mutex::new(VecDeque::new())),
            calls: Arc::new(AtomicUsize::new(0)),
        },
        FakeCompletion {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        UnavailableGateEvidencePort,
    );
    assert!(!unavailable_gate.dependencies_ready());
}

fn execution_row_snapshot(database: &std::path::Path) -> Vec<u8> {
    let connection = rusqlite::Connection::open(database).unwrap();
    let row: (String, String, String, String, String, i64, String, Vec<u8>, i64, i64) = connection
        .query_row(
            "SELECT invocation_id,tenant_id,project_id,work_item_id,plan_digest,step_ordinal,state,request,attempts,updated_at_ms FROM workflow_execution_outbox",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ))
            },
        )
        .unwrap();
    serde_json::to_vec(&row).unwrap()
}

#[test]
fn execution_plan_digest_has_a_stable_golden_vector() {
    let plan = plan(1);
    plan.validate_at(NOW).unwrap();
    assert_eq!(
        plan.request_digest,
        "a2e51cc238d2085b71d383c69639b887613b03b243a7533ce499bc90b8319046"
    );

    let json = serde_json::to_string(&plan.principal).unwrap();
    assert!(!json.contains(&"5a".repeat(32)));
    assert!(!format!("{:?}", plan.principal).contains(&"5a".repeat(32)));
}

#[test]
fn plan_rejects_digest_mutation_workspace_split_and_free_form_shell() {
    let mut mutated = plan(2);
    mutated.assignment_version += 1;
    assert_eq!(
        mutated.validate_at(NOW).unwrap_err().code,
        WorkflowErrorCode::InvalidDigest
    );

    let mut split = plan(2);
    split.steps[1].workspace_id = "project-01:other-work".to_owned();
    split = split.bind_digest().unwrap();
    assert_eq!(
        split.validate_at(NOW).unwrap_err().code,
        WorkflowErrorCode::InvalidInput
    );

    let mut shell = plan(1);
    shell.steps[0].capabilities = BTreeSet::from(["command.run_allowlisted".to_owned()]);
    shell.steps[0].command_policy = vec![CommandRuleV1 {
        program: "node".to_owned(),
        required_arg_prefix: vec!["--check".to_owned()],
        max_args: 2,
    }];
    shell.steps[0].tool = ExecutionToolV1::RunCommand {
        program: "sh".to_owned(),
        args: vec!["-c".to_owned(), "id".to_owned()],
    };
    shell = shell.bind_digest().unwrap();
    assert_eq!(
        shell.validate_at(NOW).unwrap_err().code,
        WorkflowErrorCode::InvalidInput
    );

    let mut optional = plan(1);
    optional.steps[0].outputs[0].required = false;
    optional = optional.bind_digest().unwrap();
    assert_eq!(
        optional.validate_at(NOW).unwrap_err().code,
        WorkflowErrorCode::InvalidInput
    );

    let mut duplicate_output = plan(1);
    let duplicated_output = duplicate_output.steps[0].outputs[0].clone();
    duplicate_output.steps[0].outputs.push(duplicated_output);
    duplicate_output = duplicate_output.bind_digest().unwrap();
    assert_eq!(
        duplicate_output.validate_at(NOW).unwrap_err().code,
        WorkflowErrorCode::InvalidInput
    );

    let mut duplicate_artifact = package_only_plan();
    let duplicated_artifact = duplicate_artifact.steps[0].artifacts[0].clone();
    duplicate_artifact.steps[0]
        .artifacts
        .push(duplicated_artifact);
    duplicate_artifact = duplicate_artifact.bind_digest().unwrap();
    assert_eq!(
        duplicate_artifact.validate_at(NOW).unwrap_err().code,
        WorkflowErrorCode::InvalidInput
    );

    let mut alias = plan(1);
    alias.steps[0].tool = ExecutionToolV1::WriteFile {
        path: "src/./index.html".to_owned(),
        content: "x".to_owned(),
        expected_sha256: None,
    };
    alias = alias.bind_digest().unwrap();
    assert_eq!(
        alias.validate_at(NOW).unwrap_err().code,
        WorkflowErrorCode::InvalidInput
    );

    let mut artifact_alias = package_only_plan();
    artifact_alias.steps[0].artifacts[0].required_paths = vec!["src/./index.html".to_owned()];
    artifact_alias = artifact_alias.bind_digest().unwrap();
    assert_eq!(
        artifact_alias.validate_at(NOW).unwrap_err().code,
        WorkflowErrorCode::InvalidInput
    );

    let input = ArtifactInputV1 {
        artifact_id: "input-a".to_owned(),
        digest: "a".repeat(64),
        media_type: "application/octet-stream".to_owned(),
        mount_path: "inputs/a".to_owned(),
    };
    let mut duplicate_input_id = plan(1);
    let mut second_input = input.clone();
    second_input.mount_path = "inputs/b".to_owned();
    duplicate_input_id.steps[0].inputs = vec![input.clone(), second_input];
    duplicate_input_id = duplicate_input_id.bind_digest().unwrap();
    assert_eq!(
        duplicate_input_id.validate_at(NOW).unwrap_err().code,
        WorkflowErrorCode::InvalidInput
    );

    let mut duplicate_mount = plan(1);
    let mut second_input = input.clone();
    second_input.artifact_id = "input-b".to_owned();
    duplicate_mount.steps[0].inputs = vec![input.clone(), second_input];
    duplicate_mount = duplicate_mount.bind_digest().unwrap();
    assert_eq!(
        duplicate_mount.validate_at(NOW).unwrap_err().code,
        WorkflowErrorCode::InvalidInput
    );

    let mut alias_mount = plan(1);
    let mut alias_input = input;
    alias_input.mount_path = "inputs/./a".to_owned();
    alias_mount.steps[0].inputs = vec![alias_input];
    alias_mount = alias_mount.bind_digest().unwrap();
    assert_eq!(
        alias_mount.validate_at(NOW).unwrap_err().code,
        WorkflowErrorCode::InvalidInput
    );

    let mut colliding_artifacts = package_only_plan();
    colliding_artifacts.steps[0]
        .artifacts
        .push(ArtifactExpectationV1 {
            artifact_kind: "metadata".to_owned(),
            media_type: "application/json".to_owned(),
            required_paths: vec!["src".to_owned()],
        });
    colliding_artifacts = colliding_artifacts.bind_digest().unwrap();
    assert_eq!(
        colliding_artifacts.validate_at(NOW).unwrap_err().code,
        WorkflowErrorCode::InvalidInput
    );

    let mut dormant_policy = plan(1);
    dormant_policy.steps[0].command_policy = vec![CommandRuleV1 {
        program: "node".to_owned(),
        required_arg_prefix: vec!["--check".to_owned()],
        max_args: 1,
    }];
    dormant_policy = dormant_policy.bind_digest().unwrap();
    assert_eq!(
        dormant_policy.validate_at(NOW).unwrap_err().code,
        WorkflowErrorCode::InvalidInput
    );

    let mut exact_command = plan(1);
    exact_command.steps[0].capabilities = BTreeSet::from(["command.run_allowlisted".to_owned()]);
    exact_command.steps[0].command_policy = vec![CommandRuleV1 {
        program: "node".to_owned(),
        required_arg_prefix: vec!["--check".to_owned()],
        max_args: 1,
    }];
    exact_command.steps[0].tool = ExecutionToolV1::RunCommand {
        program: "node".to_owned(),
        args: vec!["--check".to_owned()],
    };
    exact_command = exact_command.bind_digest().unwrap();
    exact_command.validate_at(NOW).unwrap();

    let mut duplicate_rule = exact_command.clone();
    let duplicated_rule = duplicate_rule.steps[0].command_policy[0].clone();
    duplicate_rule.steps[0].command_policy.push(duplicated_rule);
    duplicate_rule = duplicate_rule.bind_digest().unwrap();
    assert_eq!(
        duplicate_rule.validate_at(NOW).unwrap_err().code,
        WorkflowErrorCode::InvalidInput
    );

    let mut extra_rule = exact_command.clone();
    extra_rule.steps[0].command_policy.push(CommandRuleV1 {
        program: "node".to_owned(),
        required_arg_prefix: vec!["--version".to_owned()],
        max_args: 1,
    });
    extra_rule = extra_rule.bind_digest().unwrap();
    assert_eq!(
        extra_rule.validate_at(NOW).unwrap_err().code,
        WorkflowErrorCode::InvalidInput
    );

    let mut too_small = exact_command.clone();
    too_small.steps[0].command_policy[0].max_args = 0;
    too_small = too_small.bind_digest().unwrap();
    assert_eq!(
        too_small.validate_at(NOW).unwrap_err().code,
        WorkflowErrorCode::InvalidInput
    );

    let mut prefix_bypass = exact_command.clone();
    prefix_bypass.steps[0].tool = ExecutionToolV1::RunCommand {
        program: "node".to_owned(),
        args: vec!["--check".to_owned(), "other.js".to_owned()],
    };
    prefix_bypass.steps[0].command_policy[0].max_args = 2;
    prefix_bypass = prefix_bypass.bind_digest().unwrap();
    assert_eq!(
        prefix_bypass.validate_at(NOW).unwrap_err().code,
        WorkflowErrorCode::InvalidInput
    );

    let mut control_prefix = exact_command;
    control_prefix.steps[0].command_policy[0].required_arg_prefix = vec!["--check\n".to_owned()];
    control_prefix.steps[0].tool = ExecutionToolV1::RunCommand {
        program: "node".to_owned(),
        args: vec!["--check\n".to_owned()],
    };
    control_prefix = control_prefix.bind_digest().unwrap();
    assert_eq!(
        control_prefix.validate_at(NOW).unwrap_err().code,
        WorkflowErrorCode::InvalidInput
    );
}

#[test]
fn canonical_path_topology_rejects_overlap_but_preserves_siblings_and_reads() {
    let make_input = |id: &str, mount: &str| ArtifactInputV1 {
        artifact_id: id.to_owned(),
        digest: "a".repeat(64),
        media_type: "application/octet-stream".to_owned(),
        mount_path: mount.to_owned(),
    };

    let mut sibling_inputs = plan(1);
    sibling_inputs.steps[0].inputs = vec![
        make_input("input-a", "inputs/deps"),
        make_input("input-b", "inputs/deps2"),
    ];
    sibling_inputs = sibling_inputs.bind_digest().unwrap();
    sibling_inputs.validate_at(NOW).unwrap();

    let mut sibling_write = plan(1);
    sibling_write.steps[0].inputs = vec![make_input("input-a", "src/generated")];
    sibling_write.steps[0].tool = ExecutionToolV1::WriteFile {
        path: "src2/index.html".to_owned(),
        content: "content".to_owned(),
        expected_sha256: None,
    };
    sibling_write = sibling_write.bind_digest().unwrap();
    sibling_write.validate_at(NOW).unwrap();

    let mut sibling_patch = plan(1);
    sibling_patch.steps[0].inputs = vec![make_input("input-a", "src/generated")];
    sibling_patch.steps[0].capabilities = BTreeSet::from(["patch.apply".to_owned()]);
    sibling_patch.steps[0].tool = ExecutionToolV1::ApplyPatch {
        path: "src2/index.html".to_owned(),
        expected_sha256: "b".repeat(64),
        replacements: vec![sentinel_workflow::PatchReplacementV1 {
            old: "before".to_owned(),
            new: "after".to_owned(),
            expected_occurrences: 1,
        }],
    };
    sibling_patch = sibling_patch.bind_digest().unwrap();
    sibling_patch.validate_at(NOW).unwrap();

    let mut inspect_input = plan(1);
    inspect_input.steps[0].inputs = vec![make_input("input-a", "inputs/deps")];
    inspect_input.steps[0].capabilities = BTreeSet::from(["file.inspect".to_owned()]);
    inspect_input.steps[0].tool = ExecutionToolV1::InspectFile {
        path: "inputs/deps/config.json".to_owned(),
        max_bytes: 4096,
    };
    inspect_input = inspect_input.bind_digest().unwrap();
    inspect_input.validate_at(NOW).unwrap();

    for mounts in [
        ["inputs/deps", "inputs/deps"],
        ["inputs/deps", "inputs/deps/lib"],
        ["inputs/deps/lib", "inputs/deps"],
    ] {
        let mut overlapping = plan(1);
        overlapping.steps[0].inputs = vec![
            make_input("input-a", mounts[0]),
            make_input("input-b", mounts[1]),
        ];
        overlapping = overlapping.bind_digest().unwrap();
        assert_eq!(
            overlapping.validate_at(NOW).unwrap_err().code,
            WorkflowErrorCode::InvalidInput
        );
    }

    let mut sibling_artifacts = package_only_plan();
    sibling_artifacts.steps[0].artifacts[0].required_paths = vec!["dist/app".to_owned()];
    sibling_artifacts.steps[0]
        .artifacts
        .push(ArtifactExpectationV1 {
            artifact_kind: "metadata".to_owned(),
            media_type: "application/json".to_owned(),
            required_paths: vec!["dist/assets".to_owned()],
        });
    sibling_artifacts.steps[0].tool = ExecutionToolV1::PackageArtifact {
        artifact_kind: "source_tree".to_owned(),
        media_type: "application/vnd.sentinel.source-tree".to_owned(),
        paths: vec!["dist/app".to_owned()],
    };
    sibling_artifacts = sibling_artifacts.bind_digest().unwrap();
    sibling_artifacts.validate_at(NOW).unwrap();

    for paths in [["dist", "dist"], ["dist", "dist/app"], ["dist/app", "dist"]] {
        let mut overlapping = package_only_plan();
        overlapping.steps[0].artifacts[0].required_paths = vec![paths[0].to_owned()];
        overlapping.steps[0].artifacts.push(ArtifactExpectationV1 {
            artifact_kind: "metadata".to_owned(),
            media_type: "application/json".to_owned(),
            required_paths: vec![paths[1].to_owned()],
        });
        overlapping = overlapping.bind_digest().unwrap();
        assert_eq!(
            overlapping.validate_at(NOW).unwrap_err().code,
            WorkflowErrorCode::InvalidInput
        );
    }

    for output_path in ["inputs/deps", "inputs/deps/generated", "inputs"] {
        let mut overlapping = package_only_plan();
        overlapping.steps[0].inputs = vec![make_input("input-a", "inputs/deps")];
        overlapping.steps[0].artifacts[0].required_paths = vec![output_path.to_owned()];
        overlapping.steps[0].tool = ExecutionToolV1::PackageArtifact {
            artifact_kind: "source_tree".to_owned(),
            media_type: "application/vnd.sentinel.source-tree".to_owned(),
            paths: vec![output_path.to_owned()],
        };
        overlapping = overlapping.bind_digest().unwrap();
        assert_eq!(
            overlapping.validate_at(NOW).unwrap_err().code,
            WorkflowErrorCode::InvalidInput
        );
    }

    for (mount, destination) in [
        ("src", "src"),
        ("src", "src/index.html"),
        ("src/generated", "src"),
    ] {
        let mut write_overlap = plan(1);
        write_overlap.steps[0].inputs = vec![make_input("input-a", mount)];
        write_overlap.steps[0].tool = ExecutionToolV1::WriteFile {
            path: destination.to_owned(),
            content: "content".to_owned(),
            expected_sha256: None,
        };
        write_overlap = write_overlap.bind_digest().unwrap();
        assert_eq!(
            write_overlap.validate_at(NOW).unwrap_err().code,
            WorkflowErrorCode::InvalidInput
        );
    }

    for (mount, destination) in [
        ("src", "src"),
        ("src", "src/index.html"),
        ("src/generated", "src"),
    ] {
        let mut patch_overlap = plan(1);
        patch_overlap.steps[0].inputs = vec![make_input("input-a", mount)];
        patch_overlap.steps[0].capabilities = BTreeSet::from(["patch.apply".to_owned()]);
        patch_overlap.steps[0].tool = ExecutionToolV1::ApplyPatch {
            path: destination.to_owned(),
            expected_sha256: "b".repeat(64),
            replacements: vec![sentinel_workflow::PatchReplacementV1 {
                old: "before".to_owned(),
                new: "after".to_owned(),
                expected_occurrences: 1,
            }],
        };
        patch_overlap = patch_overlap.bind_digest().unwrap();
        assert_eq!(
            patch_overlap.validate_at(NOW).unwrap_err().code,
            WorkflowErrorCode::InvalidInput
        );
    }
}

#[test]
fn canonical_domain_ids_fail_before_any_port_or_durable_write() {
    #[derive(Clone, Copy)]
    enum IdentityField {
        Tenant,
        Project,
        WorkItem,
    }

    let cases = [
        (IdentityField::Tenant, String::new()),
        (IdentityField::Project, "a/b".to_owned()),
        (IdentityField::WorkItem, "work:01".to_owned()),
        (IdentityField::Tenant, "Tenant-01".to_owned()),
        (IdentityField::Project, "project\n01".to_owned()),
        (IdentityField::Tenant, "tenant--01".to_owned()),
        (IdentityField::WorkItem, "x".repeat(129)),
    ];
    for (index, (field, invalid_value)) in cases.into_iter().enumerate() {
        for deserialize in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let database = directory
                .path()
                .join(format!("invalid-{index}-{deserialize}.sqlite"));
            let organization_calls = Arc::new(AtomicUsize::new(0));
            let execution_calls = Arc::new(AtomicUsize::new(0));
            let completion_calls = Arc::new(AtomicUsize::new(0));
            let gate_calls = Arc::new(AtomicUsize::new(0));
            let core = WorkflowCore::new(
                WorkflowStore::open(&database).unwrap(),
                CountingOrganization {
                    snapshot: authority(),
                    calls: Arc::clone(&organization_calls),
                },
                FakeExecution {
                    observations: Arc::new(Mutex::new(VecDeque::new())),
                    calls: Arc::clone(&execution_calls),
                },
                FakeCompletion {
                    calls: Arc::clone(&completion_calls),
                },
                FakeGate {
                    calls: Arc::clone(&gate_calls),
                },
            );
            let mut invalid = plan(1);
            match field {
                IdentityField::Tenant => invalid.tenant_id = TenantId(invalid_value.clone()),
                IdentityField::Project => invalid.project_id = ProjectId(invalid_value.clone()),
                IdentityField::WorkItem => invalid.work_item_id = WorkItemId(invalid_value.clone()),
            }
            invalid = invalid.bind_digest().unwrap();
            if deserialize {
                invalid = serde_json::from_slice(&serde_json::to_vec(&invalid).unwrap()).unwrap();
            }
            let error = core.admit_plan(&invalid, NOW).unwrap_err();
            assert_eq!(error.code, WorkflowErrorCode::InvalidInput);
            assert_eq!(organization_calls.load(Ordering::SeqCst), 0);
            assert_eq!(execution_calls.load(Ordering::SeqCst), 0);
            assert_eq!(completion_calls.load(Ordering::SeqCst), 0);
            assert_eq!(gate_calls.load(Ordering::SeqCst), 0);
            let connection = rusqlite::Connection::open(&database).unwrap();
            for table in [
                "workflow_work_items",
                "workflow_operations",
                "workflow_execution_outbox",
                "workflow_completion_outbox",
                "workflow_gate_outbox",
                "workflow_audit_events",
            ] {
                let count: i64 = connection
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                    .unwrap();
                assert_eq!(count, 0, "unexpected row in {table}");
            }
        }
    }

    let mut invalid_authority = authority();
    invalid_authority.project_id = ProjectId("../project".to_owned());
    assert_eq!(
        invalid_authority.validate().unwrap_err().code,
        WorkflowErrorCode::InvalidInput
    );
    let decoded: RuntimeAuthoritySnapshotV1 =
        serde_json::from_slice(&serde_json::to_vec(&invalid_authority).unwrap()).unwrap();
    assert_eq!(
        decoded.validate().unwrap_err().code,
        WorkflowErrorCode::InvalidInput
    );

    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("invalid-observed-authority.sqlite");
    let seed = core(&database, Vec::new());
    seed.admit_plan(&plan(1), NOW).unwrap();
    let pending = seed.store().pending_executions(1).unwrap().remove(0);
    drop(seed);
    let organization_calls = Arc::new(AtomicUsize::new(0));
    let execution_calls = Arc::new(AtomicUsize::new(0));
    let core = WorkflowCore::new(
        WorkflowStore::open(&database).unwrap(),
        UncheckedOrganization {
            snapshot: invalid_authority,
            calls: Arc::clone(&organization_calls),
        },
        FakeExecution {
            observations: Arc::new(Mutex::new(vec![WorkExecutionObservation::Reserved].into())),
            calls: Arc::clone(&execution_calls),
        },
        FakeCompletion {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        FakeGate::default(),
    );
    let before = execution_row_snapshot(&database);
    let error = core.reconcile_execution(&pending, NOW + 1).unwrap_err();
    assert_eq!(error.code, WorkflowErrorCode::InvalidInput);
    assert_eq!(organization_calls.load(Ordering::SeqCst), 1);
    assert_eq!(execution_calls.load(Ordering::SeqCst), 0);
    assert_eq!(execution_row_snapshot(&database), before);
}

#[test]
fn multi_step_execution_reaches_review_before_independent_gate_allows_done() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    let core = core(
        &database,
        vec![
            WorkExecutionObservation::Reserved,
            WorkExecutionObservation::Succeeded,
            WorkExecutionObservation::Executing,
            WorkExecutionObservation::Succeeded,
        ],
    );
    let plan = plan(2);
    let (replayed, admitted) = core.admit_plan(&plan, NOW).unwrap();
    assert!(!replayed);
    assert_eq!(admitted.state, WorkItemState::Claimed);

    let first = core.store().pending_executions(10).unwrap().remove(0);
    assert_eq!(
        core.reconcile_execution(&first, NOW + 1).unwrap().state,
        WorkItemState::InProgress
    );
    let first = core.store().pending_executions(10).unwrap().remove(0);
    core.reconcile_execution(&first, NOW + 2).unwrap();
    let first_completion = core
        .store()
        .pending_completion_evidence(10)
        .unwrap()
        .remove(0);
    let after_first = core
        .reconcile_completion_evidence(&first_completion, NOW + 11)
        .unwrap();
    assert_eq!(after_first.state, WorkItemState::InProgress);
    assert_eq!(after_first.next_step_ordinal, 1);
    assert!(core.store().pending_gate_evidence(10).unwrap().is_empty());

    let second = core.store().pending_executions(10).unwrap().remove(0);
    core.reconcile_execution(&second, NOW + 12).unwrap();
    let second = core.store().pending_executions(10).unwrap().remove(0);
    core.reconcile_execution(&second, NOW + 13).unwrap();
    let completion = core
        .store()
        .pending_completion_evidence(10)
        .unwrap()
        .remove(0);
    let in_review = core
        .reconcile_completion_evidence(&completion, NOW + 14)
        .unwrap();
    assert_eq!(in_review.state, WorkItemState::InReview);
    assert!(in_review.terminal_execution_evidence.is_some());
    assert!(in_review.gate_evidence.is_none());

    let gate = core.store().pending_gate_evidence(10).unwrap().remove(0);
    let done = core.reconcile_gate_evidence(&gate, NOW + 21).unwrap();
    assert_eq!(done.state, WorkItemState::Done);
    assert!(done.gate_evidence.is_some());
}

#[test]
fn exact_plan_replay_is_idempotent_but_changed_content_conflicts() {
    let directory = tempfile::tempdir().unwrap();
    let core = core(
        &directory.path().join("workflow.sqlite"),
        vec![WorkExecutionObservation::Reserved],
    );
    let plan = plan(1);
    let first = core.admit_plan(&plan, NOW).unwrap();
    let second = core.admit_plan(&plan, NOW).unwrap();
    assert!(!first.0);
    assert!(second.0);
    assert_eq!(first.1, second.1);
    assert_eq!(core.store().pending_executions(10).unwrap().len(), 1);

    let mut rebound = plan.clone();
    rebound.steps[0].outputs[0].name = "changed".to_owned();
    rebound = rebound.bind_digest().unwrap();
    assert_eq!(
        core.admit_plan(&rebound, NOW).unwrap_err().code,
        WorkflowErrorCode::IdempotencyConflict
    );
}

#[test]
fn committed_plan_replays_after_deadline_but_new_or_changed_operations_do_not() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("committed.sqlite");
    let plan = plan(1);
    let first = {
        let core = core(&database, Vec::new());
        let admitted = core.admit_plan(&plan, NOW).unwrap();
        assert!(!admitted.0);
        admitted.1
    };

    let organization_calls = Arc::new(AtomicUsize::new(0));
    let execution_calls = Arc::new(AtomicUsize::new(0));
    let completion_calls = Arc::new(AtomicUsize::new(0));
    let gate_calls = Arc::new(AtomicUsize::new(0));
    let reopened = WorkflowCore::new(
        WorkflowStore::open(&database).unwrap(),
        CountingOrganization {
            snapshot: authority(),
            calls: Arc::clone(&organization_calls),
        },
        FakeExecution {
            observations: Arc::new(Mutex::new(VecDeque::new())),
            calls: Arc::clone(&execution_calls),
        },
        FakeCompletion {
            calls: Arc::clone(&completion_calls),
        },
        FakeGate {
            calls: Arc::clone(&gate_calls),
        },
    );
    let replayed = reopened.admit_plan(&plan, NOW + 70_000).unwrap();
    assert!(replayed.0);
    assert_eq!(replayed.1, first);
    assert_eq!(organization_calls.load(Ordering::SeqCst), 1);
    assert_eq!(execution_calls.load(Ordering::SeqCst), 0);
    assert_eq!(completion_calls.load(Ordering::SeqCst), 0);
    assert_eq!(gate_calls.load(Ordering::SeqCst), 0);
    let connection = rusqlite::Connection::open(&database).unwrap();
    for table in ["workflow_operations", "workflow_execution_outbox"] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    let mut changed = plan.clone();
    changed.steps[0].outputs[0].name = "changed".to_owned();
    changed = changed.bind_digest().unwrap();
    assert_eq!(
        reopened
            .admit_plan(&changed, NOW + 70_000)
            .unwrap_err()
            .code,
        WorkflowErrorCode::IdempotencyConflict
    );
    drop(connection);

    let fresh_database = directory.path().join("fresh-expired.sqlite");
    let fresh = core(&fresh_database, Vec::new());
    assert_eq!(
        fresh.admit_plan(&plan, NOW + 70_000).unwrap_err().code,
        WorkflowErrorCode::InvalidInput
    );
    let connection = rusqlite::Connection::open(fresh_database).unwrap();
    for table in ["workflow_operations", "workflow_execution_outbox"] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }
}

#[test]
fn restart_preserves_pending_invocation_and_does_not_duplicate_it() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    {
        let core = core(&database, vec![]);
        core.admit_plan(&plan(1), NOW).unwrap();
        assert_eq!(core.store().pending_executions(10).unwrap().len(), 1);
    }
    let reopened = core(&database, vec![WorkExecutionObservation::Reserved]);
    let pending = reopened.store().pending_executions(10).unwrap();
    assert_eq!(pending.len(), 1);
    let item = reopened.reconcile_execution(&pending[0], NOW + 1).unwrap();
    assert_eq!(item.state, WorkItemState::InProgress);
    assert_eq!(reopened.store().pending_executions(10).unwrap().len(), 1);
}

#[test]
fn authority_rotation_between_read_and_commit_fails_closed() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    let snapshot = Arc::new(Mutex::new(authority()));
    let organization = FakeOrganization {
        snapshot: Arc::clone(&snapshot),
    };
    let execution = RotatingExecution {
        snapshot: Arc::clone(&snapshot),
    };
    let core = WorkflowCore::new(
        WorkflowStore::open(&database).unwrap(),
        organization,
        execution,
        FakeCompletion {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        FakeGate::default(),
    );
    core.admit_plan(&plan(1), NOW).unwrap();
    let pending = core.store().pending_executions(1).unwrap().remove(0);
    assert_eq!(
        core.reconcile_execution(&pending, NOW + 1)
            .unwrap_err()
            .code,
        WorkflowErrorCode::AuthorityConflict
    );
    let item = core
        .store()
        .work_item(
            &authority().tenant_id,
            &authority().project_id,
            &authority().work_item_id,
        )
        .unwrap()
        .unwrap();
    assert_eq!(item.state, WorkItemState::Claimed);
}

struct RotatingExecution {
    snapshot: Arc<Mutex<RuntimeAuthoritySnapshotV1>>,
}

impl WorkExecutionPort for RotatingExecution {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Ready
    }

    fn reconcile(
        &self,
        _request: &PendingExecutionV1,
    ) -> Result<WorkExecutionObservation, WorkflowPortError> {
        let mut snapshot = self.snapshot.lock().unwrap();
        snapshot.principal.principal_generation += 1;
        snapshot.principal.authority_digest = "e".repeat(64);
        Ok(WorkExecutionObservation::Reserved)
    }
}

#[test]
fn unknown_execution_outcome_is_durable_and_never_advances_to_review() {
    let directory = tempfile::tempdir().unwrap();
    let core = core(
        &directory.path().join("workflow.sqlite"),
        vec![WorkExecutionObservation::UnknownOutcome],
    );
    core.admit_plan(&plan(1), NOW).unwrap();
    let pending = core.store().pending_executions(1).unwrap().remove(0);
    let blocked = core.reconcile_execution(&pending, NOW + 1).unwrap();
    assert_eq!(blocked.state, WorkItemState::Blocked);
    assert_eq!(
        blocked.blocker_code.as_deref(),
        Some("execution_unknown_outcome")
    );
    assert!(core.store().pending_executions(10).unwrap().is_empty());
    assert!(core
        .store()
        .pending_completion_evidence(10)
        .unwrap()
        .is_empty());
    assert!(core.store().pending_gate_evidence(10).unwrap().is_empty());
}

#[test]
fn missing_workbench_invocation_has_a_bounded_fail_closed_reconcile_budget() {
    let directory = tempfile::tempdir().unwrap();
    let core = core(
        &directory.path().join("workflow.sqlite"),
        vec![
            WorkExecutionObservation::NotFound,
            WorkExecutionObservation::NotFound,
            WorkExecutionObservation::NotFound,
        ],
    );
    core.admit_plan(&plan(1), NOW).unwrap();
    for attempt in 0..3 {
        let pending = core.store().pending_executions(1).unwrap().remove(0);
        let item = core
            .reconcile_execution(&pending, NOW + 1 + attempt)
            .unwrap();
        if attempt < 2 {
            assert_eq!(item.state, WorkItemState::Claimed);
        } else {
            assert_eq!(item.state, WorkItemState::Blocked);
            assert_eq!(
                item.blocker_code.as_deref(),
                Some("execution_unknown_outcome")
            );
        }
    }
    assert!(core.store().pending_executions(1).unwrap().is_empty());
}

#[test]
fn cross_tenant_authority_and_tampered_outbox_request_are_rejected_without_write() {
    let directory = tempfile::tempdir().unwrap();
    let core = core(
        &directory.path().join("workflow.sqlite"),
        vec![WorkExecutionObservation::Reserved],
    );
    let plan = plan(1);
    core.admit_plan(&plan, NOW).unwrap();
    let mut tampered = core.store().pending_executions(1).unwrap().remove(0);
    tampered.plan_digest = "f".repeat(64);
    assert_eq!(
        core.reconcile_execution(&tampered, NOW + 1)
            .unwrap_err()
            .code,
        WorkflowErrorCode::IdempotencyConflict
    );

    let mut foreign = plan.clone();
    foreign.tenant_id = TenantId::parse("tenant-02").unwrap();
    foreign = foreign.bind_digest().unwrap();
    assert_eq!(
        core.admit_plan(&foreign, NOW).unwrap_err().code,
        WorkflowErrorCode::AuthorityConflict
    );
    assert_eq!(core.store().pending_executions(10).unwrap().len(), 1);
}

#[test]
fn stale_stable_authority_is_rejected_before_execution_completion_or_gate_io() {
    let directory = tempfile::tempdir().unwrap();

    let execution_database = directory.path().join("execution.sqlite");
    let execution_snapshot = Arc::new(Mutex::new(authority()));
    let execution_calls = Arc::new(AtomicUsize::new(0));
    let execution_core = WorkflowCore::new(
        WorkflowStore::open(&execution_database).unwrap(),
        FakeOrganization {
            snapshot: Arc::clone(&execution_snapshot),
        },
        FakeExecution {
            observations: Arc::new(Mutex::new(vec![WorkExecutionObservation::Reserved].into())),
            calls: Arc::clone(&execution_calls),
        },
        FakeCompletion {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        FakeGate::default(),
    );
    execution_core.admit_plan(&plan(1), NOW).unwrap();
    let execution = execution_core
        .store()
        .pending_executions(1)
        .unwrap()
        .remove(0);
    let execution_before = execution_core
        .store()
        .work_item(
            &authority().tenant_id,
            &authority().project_id,
            &authority().work_item_id,
        )
        .unwrap()
        .unwrap();
    execution_snapshot.lock().unwrap().organization_generation += 1;
    assert_eq!(
        execution_core
            .reconcile_execution(&execution, NOW + 1)
            .unwrap_err()
            .code,
        WorkflowErrorCode::AuthorityConflict
    );
    assert_eq!(execution_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        execution_core.store().pending_executions(1).unwrap()[0],
        execution
    );
    assert_eq!(
        execution_core
            .store()
            .work_item(
                &authority().tenant_id,
                &authority().project_id,
                &authority().work_item_id,
            )
            .unwrap()
            .unwrap(),
        execution_before
    );

    let completion_database = directory.path().join("completion.sqlite");
    let completion_snapshot = Arc::new(Mutex::new(authority()));
    let completion_calls = Arc::new(AtomicUsize::new(0));
    let completion_core = WorkflowCore::new(
        WorkflowStore::open(&completion_database).unwrap(),
        FakeOrganization {
            snapshot: Arc::clone(&completion_snapshot),
        },
        FakeExecution {
            observations: Arc::new(Mutex::new(vec![WorkExecutionObservation::Succeeded].into())),
            calls: Arc::new(AtomicUsize::new(0)),
        },
        FakeCompletion {
            calls: Arc::clone(&completion_calls),
        },
        FakeGate::default(),
    );
    completion_core.admit_plan(&plan(1), NOW).unwrap();
    let execution = completion_core
        .store()
        .pending_executions(1)
        .unwrap()
        .remove(0);
    completion_core
        .reconcile_execution(&execution, NOW + 1)
        .unwrap();
    let completion = completion_core
        .store()
        .pending_completion_evidence(1)
        .unwrap()
        .remove(0);
    let completion_before = completion_core
        .store()
        .work_item(
            &authority().tenant_id,
            &authority().project_id,
            &authority().work_item_id,
        )
        .unwrap()
        .unwrap();
    completion_snapshot.lock().unwrap().runtime_generation += 1;
    assert_eq!(
        completion_core
            .reconcile_completion_evidence(&completion, NOW + 2)
            .unwrap_err()
            .code,
        WorkflowErrorCode::AuthorityConflict
    );
    assert_eq!(completion_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        completion_core
            .store()
            .pending_completion_evidence(1)
            .unwrap()[0],
        completion
    );
    assert_eq!(
        completion_core
            .store()
            .work_item(
                &authority().tenant_id,
                &authority().project_id,
                &authority().work_item_id,
            )
            .unwrap()
            .unwrap(),
        completion_before
    );

    let gate_database = directory.path().join("gate.sqlite");
    let gate_snapshot = Arc::new(Mutex::new(authority()));
    let gate_calls = Arc::new(AtomicUsize::new(0));
    let gate_core = WorkflowCore::new(
        WorkflowStore::open(&gate_database).unwrap(),
        FakeOrganization {
            snapshot: Arc::clone(&gate_snapshot),
        },
        FakeExecution {
            observations: Arc::new(Mutex::new(vec![WorkExecutionObservation::Succeeded].into())),
            calls: Arc::new(AtomicUsize::new(0)),
        },
        FakeCompletion {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        FakeGate {
            calls: Arc::clone(&gate_calls),
        },
    );
    gate_core.admit_plan(&plan(1), NOW).unwrap();
    let execution = gate_core.store().pending_executions(1).unwrap().remove(0);
    gate_core.reconcile_execution(&execution, NOW + 1).unwrap();
    let completion = gate_core
        .store()
        .pending_completion_evidence(1)
        .unwrap()
        .remove(0);
    gate_core
        .reconcile_completion_evidence(&completion, NOW + 2)
        .unwrap();
    let gate = gate_core
        .store()
        .pending_gate_evidence(1)
        .unwrap()
        .remove(0);
    let gate_before = gate_core
        .store()
        .work_item(
            &authority().tenant_id,
            &authority().project_id,
            &authority().work_item_id,
        )
        .unwrap()
        .unwrap();
    gate_snapshot.lock().unwrap().policy_generation += 1;
    assert_eq!(
        gate_core
            .reconcile_gate_evidence(&gate, NOW + 3)
            .unwrap_err()
            .code,
        WorkflowErrorCode::AuthorityConflict
    );
    assert_eq!(gate_calls.load(Ordering::SeqCst), 0);
    assert_eq!(gate_core.store().pending_gate_evidence(1).unwrap()[0], gate);
    assert_eq!(
        gate_core
            .store()
            .work_item(
                &authority().tenant_id,
                &authority().project_id,
                &authority().work_item_id,
            )
            .unwrap()
            .unwrap(),
        gate_before
    );
}

#[test]
fn expired_execution_after_restart_times_out_without_external_io() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    {
        let core = core(&database, Vec::new());
        core.admit_plan(&plan(1), NOW).unwrap();
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let reopened = WorkflowCore::new(
        WorkflowStore::open(&database).unwrap(),
        FakeOrganization {
            snapshot: Arc::new(Mutex::new(authority())),
        },
        FakeExecution {
            observations: Arc::new(Mutex::new(vec![WorkExecutionObservation::Succeeded].into())),
            calls: Arc::clone(&calls),
        },
        FakeCompletion {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        FakeGate::default(),
    );
    let pending = reopened.store().pending_executions(1).unwrap().remove(0);
    let blocked = reopened
        .reconcile_execution(&pending, NOW + 70_000)
        .unwrap();
    assert_eq!(blocked.state, WorkItemState::Blocked);
    assert_eq!(blocked.blocker_code.as_deref(), Some("execution_timed_out"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(reopened.store().pending_executions(1).unwrap().is_empty());
}

#[test]
fn clock_regression_is_rejected_without_external_io() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    let calls = Arc::new(AtomicUsize::new(0));
    let core = WorkflowCore::new(
        WorkflowStore::open(&database).unwrap(),
        FakeOrganization {
            snapshot: Arc::new(Mutex::new(authority())),
        },
        FakeExecution {
            observations: Arc::new(Mutex::new(vec![WorkExecutionObservation::Succeeded].into())),
            calls: Arc::clone(&calls),
        },
        FakeCompletion {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        FakeGate::default(),
    );
    core.admit_plan(&plan(1), NOW).unwrap();
    let pending = core.store().pending_executions(1).unwrap().remove(0);
    assert_eq!(
        core.reconcile_execution(&pending, NOW - 1)
            .unwrap_err()
            .code,
        WorkflowErrorCode::InvalidTransition
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(core.store().pending_executions(1).unwrap()[0], pending);
}

struct ErrorExecution {
    calls: Arc<AtomicUsize>,
    error: WorkflowPortError,
}

impl WorkExecutionPort for ErrorExecution {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Ready
    }

    fn reconcile(
        &self,
        _request: &PendingExecutionV1,
    ) -> Result<WorkExecutionObservation, WorkflowPortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(self.error.clone())
    }
}

#[test]
fn port_unknown_outcome_is_persisted_once_and_never_retried_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    let calls = Arc::new(AtomicUsize::new(0));
    let unknown_outcome_core = WorkflowCore::new(
        WorkflowStore::open(&database).unwrap(),
        FakeOrganization {
            snapshot: Arc::new(Mutex::new(authority())),
        },
        ErrorExecution {
            calls: Arc::clone(&calls),
            error: WorkflowPortError::UnknownOutcome,
        },
        FakeCompletion {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        FakeGate::default(),
    );
    unknown_outcome_core.admit_plan(&plan(1), NOW).unwrap();
    let pending = unknown_outcome_core
        .store()
        .pending_executions(1)
        .unwrap()
        .remove(0);
    let blocked = unknown_outcome_core
        .reconcile_execution(&pending, NOW + 1)
        .unwrap();
    assert_eq!(blocked.state, WorkItemState::Blocked);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(unknown_outcome_core);

    let reopened = core(&database, vec![WorkExecutionObservation::Succeeded]);
    assert!(reopened.store().pending_executions(1).unwrap().is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[derive(Clone, Copy)]
enum EvidenceMutation {
    MissingOutput,
    UnexpectedOutput,
    WrongOutputKind,
    WrongDigestAlgorithm,
    MissingArtifact,
    UnexpectedArtifact,
    WrongArtifactKind,
    WrongArtifactMediaType,
    WrongArtifactPath,
    WrongBundleDigest,
    FutureTimestamp,
    LateTimestamp,
}

struct MutatingCompletion {
    mutation: EvidenceMutation,
}

impl CompletionEvidencePort for MutatingCompletion {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Ready
    }

    fn terminal_evidence(
        &self,
        request: &PendingCompletionEvidenceV1,
    ) -> Result<Box<dyn TerminalExecutionEvidence>, WorkflowPortError> {
        let mut outputs = vec![SealedOutputEvidenceV1 {
            name: "bundle".to_owned(),
            kind: "source_tree".to_owned(),
            digest_algorithm: "sha256".to_owned(),
            digest: "b".repeat(64),
        }];
        let mut artifacts = vec![SealedArtifactEvidenceV1 {
            artifact_kind: "source_tree".to_owned(),
            media_type: "application/vnd.sentinel.source-tree".to_owned(),
            paths: vec!["src".to_owned()],
            digest: "d".repeat(64),
        }];
        match self.mutation {
            EvidenceMutation::MissingOutput => outputs.clear(),
            EvidenceMutation::UnexpectedOutput => outputs.push(outputs[0].clone()),
            EvidenceMutation::WrongOutputKind => outputs[0].kind = "binary".to_owned(),
            EvidenceMutation::WrongDigestAlgorithm => {
                outputs[0].digest_algorithm = "blake3".to_owned();
            }
            EvidenceMutation::MissingArtifact => artifacts.clear(),
            EvidenceMutation::UnexpectedArtifact => artifacts.push(artifacts[0].clone()),
            EvidenceMutation::WrongArtifactKind => {
                artifacts[0].artifact_kind = "binary".to_owned();
            }
            EvidenceMutation::WrongArtifactMediaType => {
                artifacts[0].media_type = "application/octet-stream".to_owned();
            }
            EvidenceMutation::WrongArtifactPath => {
                artifacts[0].paths = vec!["dist".to_owned()];
            }
            EvidenceMutation::WrongBundleDigest
            | EvidenceMutation::FutureTimestamp
            | EvidenceMutation::LateTimestamp => {}
        }
        let mut bundle = sealed_output_bundle_digest(&outputs, &artifacts).unwrap();
        if matches!(self.mutation, EvidenceMutation::WrongBundleDigest) {
            bundle = "f".repeat(64);
        }
        let completed_at_unix_ms = match self.mutation {
            EvidenceMutation::FutureTimestamp => NOW + 100,
            EvidenceMutation::LateTimestamp => NOW + 70_000,
            _ => request.created_at_unix_ms + 1,
        };
        Ok(Box::new(CompletionReceipt {
            request: request.clone(),
            output_bundle_digest: bundle,
            outputs,
            artifacts,
            completed_at_unix_ms,
        }))
    }
}

#[test]
fn sealed_evidence_rejects_descriptor_bundle_and_timestamp_rebinding() {
    for mutation in [
        EvidenceMutation::MissingOutput,
        EvidenceMutation::UnexpectedOutput,
        EvidenceMutation::WrongOutputKind,
        EvidenceMutation::WrongDigestAlgorithm,
        EvidenceMutation::MissingArtifact,
        EvidenceMutation::UnexpectedArtifact,
        EvidenceMutation::WrongArtifactKind,
        EvidenceMutation::WrongArtifactMediaType,
        EvidenceMutation::WrongArtifactPath,
        EvidenceMutation::WrongBundleDigest,
        EvidenceMutation::FutureTimestamp,
        EvidenceMutation::LateTimestamp,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("workflow.sqlite");
        let core = WorkflowCore::new(
            WorkflowStore::open(&database).unwrap(),
            FakeOrganization {
                snapshot: Arc::new(Mutex::new(authority())),
            },
            FakeExecution {
                observations: Arc::new(Mutex::new(
                    vec![WorkExecutionObservation::Succeeded].into(),
                )),
                calls: Arc::new(AtomicUsize::new(0)),
            },
            MutatingCompletion { mutation },
            FakeGate::default(),
        );
        core.admit_plan(&package_only_plan(), NOW).unwrap();
        let execution = core.store().pending_executions(1).unwrap().remove(0);
        core.reconcile_execution(&execution, NOW + 1).unwrap();
        let completion = core
            .store()
            .pending_completion_evidence(1)
            .unwrap()
            .remove(0);
        let reconcile_at = if matches!(mutation, EvidenceMutation::LateTimestamp) {
            NOW + 70_001
        } else {
            NOW + 2
        };
        assert_eq!(
            core.reconcile_completion_evidence(&completion, reconcile_at)
                .unwrap_err()
                .code,
            WorkflowErrorCode::AuthorityConflict
        );
        assert_eq!(
            core.store().pending_completion_evidence(1).unwrap()[0],
            completion
        );
    }
}

struct FutureGate;

impl GateEvidencePort for FutureGate {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Ready
    }

    fn gate_evidence(
        &self,
        request: &PendingGateEvidenceV1,
    ) -> Result<Box<dyn IndependentGateEvidence>, WorkflowPortError> {
        Ok(Box::new(GateReceipt {
            request: request.clone(),
            completed_at_unix_ms: request.created_at_unix_ms + 100,
        }))
    }
}

#[derive(Clone)]
struct TimedGate {
    completed_at_unix_ms: u64,
    calls: Arc<AtomicUsize>,
}

impl GateEvidencePort for TimedGate {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Ready
    }

    fn gate_evidence(
        &self,
        request: &PendingGateEvidenceV1,
    ) -> Result<Box<dyn IndependentGateEvidence>, WorkflowPortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(GateReceipt {
            request: request.clone(),
            completed_at_unix_ms: self.completed_at_unix_ms,
        }))
    }
}

#[test]
fn future_gate_receipt_is_rejected_without_state_advance() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    let core = WorkflowCore::new(
        WorkflowStore::open(&database).unwrap(),
        FakeOrganization {
            snapshot: Arc::new(Mutex::new(authority())),
        },
        FakeExecution {
            observations: Arc::new(Mutex::new(vec![WorkExecutionObservation::Succeeded].into())),
            calls: Arc::new(AtomicUsize::new(0)),
        },
        FakeCompletion {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        FutureGate,
    );
    core.admit_plan(&package_only_plan(), NOW).unwrap();
    let execution = core.store().pending_executions(1).unwrap().remove(0);
    core.reconcile_execution(&execution, NOW + 1).unwrap();
    let completion = core
        .store()
        .pending_completion_evidence(1)
        .unwrap()
        .remove(0);
    core.reconcile_completion_evidence(&completion, NOW + 2)
        .unwrap();
    let gate = core.store().pending_gate_evidence(1).unwrap().remove(0);
    assert_eq!(
        core.reconcile_gate_evidence(&gate, NOW + 3)
            .unwrap_err()
            .code,
        WorkflowErrorCode::AuthorityConflict
    );
    assert_eq!(core.store().pending_gate_evidence(1).unwrap()[0], gate);
}

#[test]
fn gate_deadline_rejects_late_receipt_but_allows_timely_receipt_read_later() {
    let directory = tempfile::tempdir().unwrap();

    let late_database = directory.path().join("late.sqlite");
    let late_calls = Arc::new(AtomicUsize::new(0));
    let late_core = WorkflowCore::new(
        WorkflowStore::open(&late_database).unwrap(),
        FakeOrganization {
            snapshot: Arc::new(Mutex::new(authority())),
        },
        FakeExecution {
            observations: Arc::new(Mutex::new(vec![WorkExecutionObservation::Succeeded].into())),
            calls: Arc::new(AtomicUsize::new(0)),
        },
        FakeCompletion {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        TimedGate {
            completed_at_unix_ms: NOW + 60_001,
            calls: Arc::clone(&late_calls),
        },
    );
    late_core.admit_plan(&package_only_plan(), NOW).unwrap();
    let execution = late_core.store().pending_executions(1).unwrap().remove(0);
    late_core.reconcile_execution(&execution, NOW + 1).unwrap();
    let completion = late_core
        .store()
        .pending_completion_evidence(1)
        .unwrap()
        .remove(0);
    let in_review = late_core
        .reconcile_completion_evidence(&completion, NOW + 2)
        .unwrap();
    let gate = late_core
        .store()
        .pending_gate_evidence(1)
        .unwrap()
        .remove(0);
    assert_eq!(
        late_core
            .reconcile_gate_evidence(&gate, NOW + 60_002)
            .unwrap_err()
            .code,
        WorkflowErrorCode::AuthorityConflict
    );
    assert_eq!(late_calls.load(Ordering::SeqCst), 1);
    assert_eq!(late_core.store().pending_gate_evidence(1).unwrap()[0], gate);
    assert_eq!(
        late_core
            .store()
            .work_item(
                &in_review.tenant_id,
                &in_review.project_id,
                &in_review.work_item_id
            )
            .unwrap()
            .unwrap()
            .state,
        WorkItemState::InReview
    );

    let timely_database = directory.path().join("timely.sqlite");
    let timely_calls = Arc::new(AtomicUsize::new(0));
    let timely_plan = package_only_plan();
    let timely_core = WorkflowCore::new(
        WorkflowStore::open(&timely_database).unwrap(),
        FakeOrganization {
            snapshot: Arc::new(Mutex::new(authority())),
        },
        FakeExecution {
            observations: Arc::new(Mutex::new(vec![WorkExecutionObservation::Succeeded].into())),
            calls: Arc::new(AtomicUsize::new(0)),
        },
        FakeCompletion {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        TimedGate {
            completed_at_unix_ms: NOW + 3,
            calls: Arc::clone(&timely_calls),
        },
    );
    timely_core.admit_plan(&timely_plan, NOW).unwrap();
    let execution = timely_core.store().pending_executions(1).unwrap().remove(0);
    timely_core
        .reconcile_execution(&execution, NOW + 1)
        .unwrap();
    let completion = timely_core
        .store()
        .pending_completion_evidence(1)
        .unwrap()
        .remove(0);
    timely_core
        .reconcile_completion_evidence(&completion, NOW + 2)
        .unwrap();
    let gate = timely_core
        .store()
        .pending_gate_evidence(1)
        .unwrap()
        .remove(0);
    let done = timely_core
        .reconcile_gate_evidence(&gate, NOW + 60_002)
        .unwrap();
    assert_eq!(done.state, WorkItemState::Done);
    assert_eq!(timely_calls.load(Ordering::SeqCst), 1);
    drop(timely_core);

    let replay_gate_calls = Arc::new(AtomicUsize::new(0));
    let reopened = WorkflowCore::new(
        WorkflowStore::open(&timely_database).unwrap(),
        FakeOrganization {
            snapshot: Arc::new(Mutex::new(authority())),
        },
        FakeExecution {
            observations: Arc::new(Mutex::new(VecDeque::new())),
            calls: Arc::new(AtomicUsize::new(0)),
        },
        FakeCompletion {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        TimedGate {
            completed_at_unix_ms: NOW + 70_000,
            calls: Arc::clone(&replay_gate_calls),
        },
    );
    let replayed = reopened
        .reconcile_gate_evidence(&gate, NOW + 70_000)
        .unwrap();
    assert_eq!(replayed, done);
    assert_eq!(replay_gate_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn conflicting_completion_binding_rolls_back_execution_transition() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    let core = core(&database, vec![WorkExecutionObservation::Succeeded]);
    let plan = plan(1);
    core.admit_plan(&plan, NOW).unwrap();
    let pending = core.store().pending_executions(1).unwrap().remove(0);
    let conflicting = PendingCompletionEvidenceV1 {
        schema_version: WORKFLOW_SCHEMA_VERSION,
        request_id: "f".repeat(64),
        plan_id: plan.plan_id,
        plan_digest: plan.request_digest.clone(),
        step_id: pending.step.step_id,
        invocation_id: pending.step.invocation_id,
        step_digest: "e".repeat(64),
        authority_snapshot_digest: authority().canonical_digest().unwrap(),
        request_digest: "d".repeat(64),
        created_at_unix_ms: NOW,
    };
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute(
            "INSERT INTO workflow_completion_outbox (request_id,invocation_id,tenant_id,project_id,work_item_id,state,request,updated_at_ms) VALUES (?1,?2,?3,?4,?5,'pending',?6,?7)",
            rusqlite::params![
                conflicting.request_id,
                conflicting.invocation_id.to_string(),
                plan.tenant_id.0,
                plan.project_id.0,
                plan.work_item_id.0,
                serde_json::to_vec(&conflicting).unwrap(),
                NOW as i64,
            ],
        )
        .unwrap();
    drop(connection);

    let error = core.reconcile_execution(&pending, NOW + 1).unwrap_err();
    assert_eq!(error.code, WorkflowErrorCode::CorruptStore);
    assert!(!error.retryable);
    assert_eq!(core.store().pending_executions(1).unwrap()[0], pending);
    assert_eq!(
        core.store()
            .work_item(&plan.tenant_id, &plan.project_id, &plan.work_item_id)
            .unwrap()
            .unwrap()
            .state,
        WorkItemState::Claimed
    );
}

#[test]
fn zero_row_execution_update_is_corrupt_and_leaves_durable_state_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    let core = core(&database, vec![WorkExecutionObservation::Reserved]);
    let plan = plan(1);
    core.admit_plan(&plan, NOW).unwrap();
    let pending = core.store().pending_executions(1).unwrap().remove(0);
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER ignore_execution_update BEFORE UPDATE ON workflow_execution_outbox BEGIN SELECT RAISE(IGNORE); END;",
        )
        .unwrap();
    drop(connection);

    let error = core.reconcile_execution(&pending, NOW + 1).unwrap_err();
    assert_eq!(error.code, WorkflowErrorCode::CorruptStore);
    assert!(!error.retryable);
    assert_eq!(core.store().pending_executions(1).unwrap()[0], pending);
    assert_eq!(
        core.store()
            .work_item(&plan.tenant_id, &plan.project_id, &plan.work_item_id)
            .unwrap()
            .unwrap()
            .state,
        WorkItemState::Claimed
    );
}

#[test]
fn completed_outbox_without_evidence_is_corrupt_before_port_io() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    let calls = Arc::new(AtomicUsize::new(0));
    let core = WorkflowCore::new(
        WorkflowStore::open(&database).unwrap(),
        FakeOrganization {
            snapshot: Arc::new(Mutex::new(authority())),
        },
        FakeExecution {
            observations: Arc::new(Mutex::new(vec![WorkExecutionObservation::Succeeded].into())),
            calls: Arc::new(AtomicUsize::new(0)),
        },
        FakeCompletion {
            calls: Arc::clone(&calls),
        },
        FakeGate::default(),
    );
    core.admit_plan(&plan(1), NOW).unwrap();
    let execution = core.store().pending_executions(1).unwrap().remove(0);
    core.reconcile_execution(&execution, NOW + 1).unwrap();
    let completion = core
        .store()
        .pending_completion_evidence(1)
        .unwrap()
        .remove(0);
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute(
            "UPDATE workflow_completion_outbox SET state='completed', evidence=NULL WHERE request_id=?1",
            [&completion.request_id],
        )
        .unwrap();
    drop(connection);

    let error = core
        .reconcile_completion_evidence(&completion, NOW + 2)
        .unwrap_err();
    assert_eq!(error.code, WorkflowErrorCode::CorruptStore);
    assert!(!error.retryable);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn corrupt_json_or_schema_is_nonretryable_after_restart() {
    for corrupt_schema in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("workflow.sqlite");
        let core = core(&database, Vec::new());
        core.admit_plan(&plan(1), NOW).unwrap();
        let mut pending = core.store().pending_executions(1).unwrap().remove(0);
        drop(core);
        let bytes = if corrupt_schema {
            pending.schema_version = WORKFLOW_SCHEMA_VERSION + 1;
            serde_json::to_vec(&pending).unwrap()
        } else {
            b"{".to_vec()
        };
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute(
                "UPDATE workflow_execution_outbox SET request=?1 WHERE invocation_id=?2",
                rusqlite::params![bytes, pending.step.invocation_id.to_string()],
            )
            .unwrap();
        drop(connection);

        let reopened = WorkflowStore::open(&database).unwrap();
        for _ in 0..2 {
            let error = reopened.pending_executions(1).unwrap_err();
            assert_eq!(error.code, WorkflowErrorCode::CorruptStore);
            assert!(!error.retryable);
        }
    }
}

#[derive(Clone, Copy)]
enum WorkItemTamper {
    Tenant,
    Agent,
    PlanDigest,
    Version,
    State,
    Ordinal,
}

#[test]
fn decodable_work_item_corruption_never_reaches_authority_or_execution_ports() {
    for tamper in [
        WorkItemTamper::Tenant,
        WorkItemTamper::Agent,
        WorkItemTamper::PlanDigest,
        WorkItemTamper::Version,
        WorkItemTamper::State,
        WorkItemTamper::Ordinal,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("workflow.sqlite");
        let seed = core(&database, Vec::new());
        let plan = plan(1);
        seed.admit_plan(&plan, NOW).unwrap();
        let pending = seed.store().pending_executions(1).unwrap().remove(0);
        drop(seed);

        let connection = rusqlite::Connection::open(&database).unwrap();
        let payload: Vec<u8> = connection
            .query_row(
                "SELECT payload FROM workflow_work_items WHERE tenant_id=?1 AND project_id=?2 AND work_item_id=?3",
                rusqlite::params![plan.tenant_id.0, plan.project_id.0, plan.work_item_id.0],
                |row| row.get(0),
            )
            .unwrap();
        let mut item: WorkItemExecutionV1 = serde_json::from_slice(&payload).unwrap();
        match tamper {
            WorkItemTamper::Tenant => item.tenant_id = TenantId::parse("tenant-02").unwrap(),
            WorkItemTamper::Agent => item.agent_id = AgentId(8),
            WorkItemTamper::PlanDigest => item.plan.assignment_version += 1,
            WorkItemTamper::Version => item.version += 1,
            WorkItemTamper::State => item.state = WorkItemState::Done,
            WorkItemTamper::Ordinal => item.next_step_ordinal = 1,
        }
        let corrupt_payload = serde_json::to_vec(&item).unwrap();
        connection
            .execute(
                "UPDATE workflow_work_items SET payload=?1 WHERE tenant_id=?2 AND project_id=?3 AND work_item_id=?4",
                rusqlite::params![
                    corrupt_payload,
                    plan.tenant_id.0,
                    plan.project_id.0,
                    plan.work_item_id.0,
                ],
            )
            .unwrap();
        drop(connection);

        let authority_calls = Arc::new(AtomicUsize::new(0));
        let execution_calls = Arc::new(AtomicUsize::new(0));
        let reopened = WorkflowCore::new(
            WorkflowStore::open(&database).unwrap(),
            CountingOrganization {
                snapshot: authority(),
                calls: Arc::clone(&authority_calls),
            },
            FakeExecution {
                observations: Arc::new(Mutex::new(vec![WorkExecutionObservation::Reserved].into())),
                calls: Arc::clone(&execution_calls),
            },
            FakeCompletion {
                calls: Arc::new(AtomicUsize::new(0)),
            },
            FakeGate::default(),
        );
        for _ in 0..2 {
            let error = reopened.reconcile_execution(&pending, NOW + 1).unwrap_err();
            assert_eq!(error.code, WorkflowErrorCode::CorruptStore);
            assert!(!error.retryable);
        }
        assert_eq!(authority_calls.load(Ordering::SeqCst), 0);
        assert_eq!(execution_calls.load(Ordering::SeqCst), 0);
        drop(reopened);
        let connection = rusqlite::Connection::open(&database).unwrap();
        let persisted: Vec<u8> = connection
            .query_row(
                "SELECT payload FROM workflow_work_items WHERE tenant_id=?1 AND project_id=?2 AND work_item_id=?3",
                rusqlite::params![plan.tenant_id.0, plan.project_id.0, plan.work_item_id.0],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(persisted, corrupt_payload);
    }
}

#[derive(Clone, Copy)]
enum ExecutionRowTamper {
    Key,
    Tenant,
    Ordinal,
    State,
    Attempts,
    UpdatedAt,
}

#[test]
fn decodable_execution_row_corruption_is_repeatably_nonretryable_and_zero_call() {
    for tamper in [
        ExecutionRowTamper::Key,
        ExecutionRowTamper::Tenant,
        ExecutionRowTamper::Ordinal,
        ExecutionRowTamper::State,
        ExecutionRowTamper::Attempts,
        ExecutionRowTamper::UpdatedAt,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("workflow.sqlite");
        let seed = core(&database, Vec::new());
        seed.admit_plan(&plan(1), NOW).unwrap();
        let pending = seed.store().pending_executions(1).unwrap().remove(0);
        drop(seed);
        let connection = rusqlite::Connection::open(&database).unwrap();
        match tamper {
            ExecutionRowTamper::Key => {
                connection
                    .execute(
                        "UPDATE workflow_execution_outbox SET invocation_id=?1",
                        [Uuid::parse_str("018f3f32-4f01-7f2c-a6c1-f6f4a81b2899")
                            .unwrap()
                            .to_string()],
                    )
                    .unwrap();
            }
            ExecutionRowTamper::Tenant => {
                connection
                    .execute(
                        "UPDATE workflow_execution_outbox SET tenant_id='tenant-02'",
                        [],
                    )
                    .unwrap();
            }
            ExecutionRowTamper::Ordinal => {
                connection
                    .execute("UPDATE workflow_execution_outbox SET step_ordinal=1", [])
                    .unwrap();
            }
            ExecutionRowTamper::State => {
                connection
                    .execute("UPDATE workflow_execution_outbox SET state='reserved'", [])
                    .unwrap();
            }
            ExecutionRowTamper::Attempts => {
                connection
                    .execute("UPDATE workflow_execution_outbox SET attempts=7", [])
                    .unwrap();
            }
            ExecutionRowTamper::UpdatedAt => {
                connection
                    .execute(
                        "UPDATE workflow_execution_outbox SET updated_at_ms=updated_at_ms+1",
                        [],
                    )
                    .unwrap();
            }
        }
        drop(connection);
        let corrupt_row = execution_row_snapshot(&database);

        let authority_calls = Arc::new(AtomicUsize::new(0));
        let execution_calls = Arc::new(AtomicUsize::new(0));
        let reopened = WorkflowCore::new(
            WorkflowStore::open(&database).unwrap(),
            CountingOrganization {
                snapshot: authority(),
                calls: Arc::clone(&authority_calls),
            },
            FakeExecution {
                observations: Arc::new(Mutex::new(vec![WorkExecutionObservation::Reserved].into())),
                calls: Arc::clone(&execution_calls),
            },
            FakeCompletion {
                calls: Arc::new(AtomicUsize::new(0)),
            },
            FakeGate::default(),
        );
        for _ in 0..2 {
            let error = reopened.reconcile_execution(&pending, NOW + 1).unwrap_err();
            assert_eq!(error.code, WorkflowErrorCode::CorruptStore);
            assert!(!error.retryable);
        }
        assert_eq!(authority_calls.load(Ordering::SeqCst), 0);
        assert_eq!(execution_calls.load(Ordering::SeqCst), 0);
        drop(reopened);
        assert_eq!(execution_row_snapshot(&database), corrupt_row);
    }
}

#[test]
fn nonretryable_store_failure_never_reaches_authority_or_effect_ports() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    let seed = core(&database, Vec::new());
    seed.admit_plan(&plan(1), NOW).unwrap();
    let pending = seed.store().pending_executions(1).unwrap().remove(0);
    drop(seed);

    let authority_calls = Arc::new(AtomicUsize::new(0));
    let execution_calls = Arc::new(AtomicUsize::new(0));
    let core = WorkflowCore::new(
        WorkflowStore::open(&database).unwrap(),
        CountingOrganization {
            snapshot: authority(),
            calls: Arc::clone(&authority_calls),
        },
        FakeExecution {
            observations: Arc::new(Mutex::new(vec![WorkExecutionObservation::Reserved].into())),
            calls: Arc::clone(&execution_calls),
        },
        FakeCompletion {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        FakeGate::default(),
    );
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute("DROP TABLE workflow_execution_outbox", [])
        .unwrap();
    drop(connection);

    for _ in 0..2 {
        let error = core.reconcile_execution(&pending, NOW + 1).unwrap_err();
        assert_eq!(error.code, WorkflowErrorCode::PersistenceFailure);
        assert!(!error.retryable);
    }
    assert_eq!(authority_calls.load(Ordering::SeqCst), 0);
    assert_eq!(execution_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn corrupted_operation_response_never_replays_as_an_authoritative_admission() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    let plan = plan(1);
    let store = WorkflowStore::open(&database).unwrap();
    store.admit_plan(&plan, &authority(), NOW).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&database).unwrap();
    let response: Vec<u8> = connection
        .query_row("SELECT response FROM workflow_operations", [], |row| {
            row.get(0)
        })
        .unwrap();
    let mut item: WorkItemExecutionV1 = serde_json::from_slice(&response).unwrap();
    item.state = WorkItemState::InProgress;
    let corrupt_response = serde_json::to_vec(&item).unwrap();
    connection
        .execute(
            "UPDATE workflow_operations SET response=?1",
            [&corrupt_response],
        )
        .unwrap();
    drop(connection);

    let reopened = WorkflowStore::open(&database).unwrap();
    for _ in 0..2 {
        let error = reopened.admit_plan(&plan, &authority(), NOW).unwrap_err();
        assert_eq!(error.code, WorkflowErrorCode::CorruptStore);
        assert!(!error.retryable);
    }
    drop(reopened);
    let connection = rusqlite::Connection::open(&database).unwrap();
    let persisted: Vec<u8> = connection
        .query_row("SELECT response FROM workflow_operations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(persisted, corrupt_response);
}

#[test]
fn completion_and_gate_lane_corruption_is_rejected_before_effect_ports() {
    let directory = tempfile::tempdir().unwrap();

    let completion_database = directory.path().join("completion.sqlite");
    let completion_authority_calls = Arc::new(AtomicUsize::new(0));
    let completion_calls = Arc::new(AtomicUsize::new(0));
    let completion_core = WorkflowCore::new(
        WorkflowStore::open(&completion_database).unwrap(),
        CountingOrganization {
            snapshot: authority(),
            calls: Arc::clone(&completion_authority_calls),
        },
        FakeExecution {
            observations: Arc::new(Mutex::new(vec![WorkExecutionObservation::Succeeded].into())),
            calls: Arc::new(AtomicUsize::new(0)),
        },
        FakeCompletion {
            calls: Arc::clone(&completion_calls),
        },
        FakeGate::default(),
    );
    let completion_plan = plan(1);
    completion_core.admit_plan(&completion_plan, NOW).unwrap();
    let execution = completion_core
        .store()
        .pending_executions(1)
        .unwrap()
        .remove(0);
    completion_core
        .reconcile_execution(&execution, NOW + 1)
        .unwrap();
    let completion = completion_core
        .store()
        .pending_completion_evidence(1)
        .unwrap()
        .remove(0);
    let connection = rusqlite::Connection::open(&completion_database).unwrap();
    let payload: Vec<u8> = connection
        .query_row("SELECT payload FROM workflow_work_items", [], |row| {
            row.get(0)
        })
        .unwrap();
    let mut item: WorkItemExecutionV1 = serde_json::from_slice(&payload).unwrap();
    item.state = WorkItemState::Blocked;
    item.blocker_code = Some("execution_failed".to_owned());
    let blocked_payload = serde_json::to_vec(&item).unwrap();
    connection
        .execute(
            "UPDATE workflow_work_items SET payload=?1",
            [&blocked_payload],
        )
        .unwrap();
    drop(connection);
    drop(completion_core);
    completion_authority_calls.store(0, Ordering::SeqCst);
    let completion_reopened = WorkflowCore::new(
        WorkflowStore::open(&completion_database).unwrap(),
        CountingOrganization {
            snapshot: authority(),
            calls: Arc::clone(&completion_authority_calls),
        },
        FakeExecution {
            observations: Arc::new(Mutex::new(VecDeque::new())),
            calls: Arc::new(AtomicUsize::new(0)),
        },
        FakeCompletion {
            calls: Arc::clone(&completion_calls),
        },
        FakeGate::default(),
    );
    for _ in 0..2 {
        let error = completion_reopened
            .reconcile_completion_evidence(&completion, NOW + 2)
            .unwrap_err();
        assert_eq!(error.code, WorkflowErrorCode::CorruptStore);
        assert!(!error.retryable);
    }
    assert_eq!(completion_authority_calls.load(Ordering::SeqCst), 0);
    assert_eq!(completion_calls.load(Ordering::SeqCst), 0);
    drop(completion_reopened);
    let connection = rusqlite::Connection::open(&completion_database).unwrap();
    let persisted_payload: Vec<u8> = connection
        .query_row("SELECT payload FROM workflow_work_items", [], |row| {
            row.get(0)
        })
        .unwrap();
    let completion_state: String = connection
        .query_row("SELECT state FROM workflow_completion_outbox", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(persisted_payload, blocked_payload);
    assert_eq!(completion_state, "pending");
    drop(connection);

    let gate_database = directory.path().join("gate.sqlite");
    let gate_authority_calls = Arc::new(AtomicUsize::new(0));
    let gate_calls = Arc::new(AtomicUsize::new(0));
    let gate_core = WorkflowCore::new(
        WorkflowStore::open(&gate_database).unwrap(),
        CountingOrganization {
            snapshot: authority(),
            calls: Arc::clone(&gate_authority_calls),
        },
        FakeExecution {
            observations: Arc::new(Mutex::new(vec![WorkExecutionObservation::Succeeded].into())),
            calls: Arc::new(AtomicUsize::new(0)),
        },
        FakeCompletion {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        FakeGate {
            calls: Arc::clone(&gate_calls),
        },
    );
    gate_core.admit_plan(&plan(1), NOW).unwrap();
    let execution = gate_core.store().pending_executions(1).unwrap().remove(0);
    gate_core.reconcile_execution(&execution, NOW + 1).unwrap();
    let completion = gate_core
        .store()
        .pending_completion_evidence(1)
        .unwrap()
        .remove(0);
    gate_core
        .reconcile_completion_evidence(&completion, NOW + 2)
        .unwrap();
    let gate = gate_core
        .store()
        .pending_gate_evidence(1)
        .unwrap()
        .remove(0);
    let connection = rusqlite::Connection::open(&gate_database).unwrap();
    connection
        .execute("UPDATE workflow_gate_outbox SET state='corrupt_state'", [])
        .unwrap();
    drop(connection);
    drop(gate_core);
    gate_authority_calls.store(0, Ordering::SeqCst);
    let gate_reopened = WorkflowCore::new(
        WorkflowStore::open(&gate_database).unwrap(),
        CountingOrganization {
            snapshot: authority(),
            calls: Arc::clone(&gate_authority_calls),
        },
        FakeExecution {
            observations: Arc::new(Mutex::new(VecDeque::new())),
            calls: Arc::new(AtomicUsize::new(0)),
        },
        FakeCompletion {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        FakeGate {
            calls: Arc::clone(&gate_calls),
        },
    );
    let discovery_error = gate_reopened.store().pending_gate_evidence(1).unwrap_err();
    assert_eq!(discovery_error.code, WorkflowErrorCode::CorruptStore);
    assert!(!discovery_error.retryable);
    for _ in 0..2 {
        let error = gate_reopened
            .reconcile_gate_evidence(&gate, NOW + 3)
            .unwrap_err();
        assert_eq!(error.code, WorkflowErrorCode::CorruptStore);
        assert!(!error.retryable);
    }
    assert_eq!(gate_authority_calls.load(Ordering::SeqCst), 0);
    assert_eq!(gate_calls.load(Ordering::SeqCst), 0);
    drop(gate_reopened);
    let connection = rusqlite::Connection::open(&gate_database).unwrap();
    let persisted_state: String = connection
        .query_row("SELECT state FROM workflow_gate_outbox", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(persisted_state, "corrupt_state");
}

fn create_v1_store(database: &std::path::Path) {
    let connection = rusqlite::Connection::open(database).unwrap();
    connection
        .execute_batch(
            r#"
            CREATE TABLE workflow_schema_meta (
                singleton INTEGER PRIMARY KEY CHECK (singleton=1),
                schema_version INTEGER NOT NULL
            );
            INSERT INTO workflow_schema_meta VALUES (1,1);
            CREATE TABLE workflow_work_items (
                tenant_id TEXT NOT NULL, project_id TEXT NOT NULL,
                work_item_id TEXT NOT NULL, version INTEGER NOT NULL,
                payload BLOB NOT NULL, PRIMARY KEY (tenant_id,project_id,work_item_id)
            );
            CREATE TABLE workflow_operations (
                operation_namespace TEXT NOT NULL, operation_id TEXT NOT NULL,
                request_digest TEXT NOT NULL, response BLOB NOT NULL,
                created_at_ms INTEGER NOT NULL,
                PRIMARY KEY (operation_namespace,operation_id)
            );
            CREATE TABLE workflow_execution_outbox (
                invocation_id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL,
                project_id TEXT NOT NULL, work_item_id TEXT NOT NULL,
                plan_digest TEXT NOT NULL, step_ordinal INTEGER NOT NULL,
                state TEXT NOT NULL, request BLOB NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0, updated_at_ms INTEGER NOT NULL
            );
            CREATE INDEX idx_workflow_execution_pending
                ON workflow_execution_outbox(state,updated_at_ms,invocation_id);
            CREATE TABLE workflow_completion_outbox (
                request_id TEXT PRIMARY KEY, invocation_id TEXT NOT NULL UNIQUE,
                tenant_id TEXT NOT NULL, project_id TEXT NOT NULL,
                work_item_id TEXT NOT NULL, state TEXT NOT NULL,
                request BLOB NOT NULL, evidence BLOB,
                attempts INTEGER NOT NULL DEFAULT 0, updated_at_ms INTEGER NOT NULL
            );
            CREATE INDEX idx_workflow_completion_pending
                ON workflow_completion_outbox(state,updated_at_ms,request_id);
            CREATE TABLE workflow_audit_events (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT, event_id TEXT NOT NULL UNIQUE,
                tenant_id TEXT NOT NULL, project_id TEXT NOT NULL, work_item_id TEXT NOT NULL,
                event_type TEXT NOT NULL, before_state TEXT, after_state TEXT NOT NULL,
                authority_digest TEXT NOT NULL, payload_digest TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL
            );
            "#,
        )
        .unwrap();
    drop(connection);
}

type SchemaObjectRow = (String, String, String, Option<String>);
type SchemaSnapshot = (Vec<SchemaObjectRow>, Vec<(i64, i64)>);

fn schema_snapshot(database: &std::path::Path) -> SchemaSnapshot {
    let connection = rusqlite::Connection::open(database).unwrap();
    let mut statement = connection
        .prepare("SELECT type,name,tbl_name,sql FROM sqlite_master ORDER BY type,name,tbl_name,sql")
        .unwrap();
    let objects = statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    drop(statement);
    let has_meta: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='workflow_schema_meta')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let meta = if has_meta {
        let mut statement = connection
            .prepare("SELECT singleton,schema_version FROM workflow_schema_meta ORDER BY singleton")
            .unwrap();
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        rows
    } else {
        Vec::new()
    };
    (objects, meta)
}

fn rewrite_table_sql(database: &std::path::Path, table: &str, from: &str, to: &str) {
    let connection = rusqlite::Connection::open(database).unwrap();
    let original: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
            [table],
            |row| row.get(0),
        )
        .unwrap();
    assert!(original.contains(from));
    let rewritten = original.replacen(from, to, 1);
    connection
        .pragma_update(None, "writable_schema", true)
        .unwrap();
    connection
        .execute(
            "UPDATE sqlite_master SET sql=?1 WHERE type='table' AND name=?2",
            rusqlite::params![rewritten, table],
        )
        .unwrap();
    let schema_version: i64 = connection
        .pragma_query_value(None, "schema_version", |row| row.get(0))
        .unwrap();
    connection
        .pragma_update(None, "schema_version", schema_version + 1)
        .unwrap();
    connection
        .pragma_update(None, "writable_schema", false)
        .unwrap();
}

#[derive(Clone, Copy)]
enum SchemaTamper {
    Nullability,
    Default,
    Index,
    MetaConstraint,
    MetaRow,
    MetaVersion,
}

fn create_tampered_store(database: &std::path::Path, schema_version: u32, tamper: SchemaTamper) {
    if schema_version == 1 {
        create_v1_store(database);
    } else {
        drop(WorkflowStore::open(database).unwrap());
    }
    match tamper {
        SchemaTamper::Nullability => rewrite_table_sql(
            database,
            "workflow_work_items",
            "tenant_id TEXT NOT NULL",
            "tenant_id TEXT",
        ),
        SchemaTamper::Default => rewrite_table_sql(
            database,
            "workflow_execution_outbox",
            "attempts INTEGER NOT NULL DEFAULT 0",
            "attempts INTEGER NOT NULL DEFAULT 1",
        ),
        SchemaTamper::Index => {
            let connection = rusqlite::Connection::open(database).unwrap();
            connection
                .execute("DROP INDEX idx_workflow_execution_pending", [])
                .unwrap();
        }
        SchemaTamper::MetaConstraint => rewrite_table_sql(
            database,
            "workflow_schema_meta",
            if schema_version == 1 {
                "CHECK (singleton=1)"
            } else {
                "CHECK (singleton = 1)"
            },
            "CHECK (singleton>0)",
        ),
        SchemaTamper::MetaRow => {
            rewrite_table_sql(
                database,
                "workflow_schema_meta",
                if schema_version == 1 {
                    "CHECK (singleton=1)"
                } else {
                    "CHECK (singleton = 1)"
                },
                "CHECK (singleton>0)",
            );
            let connection = rusqlite::Connection::open(database).unwrap();
            connection
                .execute("UPDATE workflow_schema_meta SET singleton=2", [])
                .unwrap();
        }
        SchemaTamper::MetaVersion => {
            let connection = rusqlite::Connection::open(database).unwrap();
            connection
                .execute("UPDATE workflow_schema_meta SET schema_version=99", [])
                .unwrap();
        }
    }
}

#[test]
fn store_migrates_v1_gate_outbox_atomically_and_reopens() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    create_v1_store(&database);

    let store = WorkflowStore::open(&database).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&database).unwrap();
    let version: u32 = connection
        .query_row(
            "SELECT schema_version FROM workflow_schema_meta WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let gate_table: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='workflow_gate_outbox')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, WORKFLOW_STORE_SCHEMA_VERSION);
    assert!(gate_table);
    drop(connection);
    WorkflowStore::open(&database).unwrap();
}

#[test]
fn unexpected_v1_trigger_is_rejected_without_schema_promotion() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    create_v1_store(&database);
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch(
            r#"
            CREATE TRIGGER interrupt_workflow_migration
            BEFORE UPDATE OF schema_version ON workflow_schema_meta
            BEGIN
                SELECT RAISE(ABORT, 'injected migration interruption');
            END;
            "#,
        )
        .unwrap();
    drop(connection);

    let before = schema_snapshot(&database);

    let error = WorkflowStore::open(&database).unwrap_err();
    assert_eq!(error.code, WorkflowErrorCode::CorruptStore);
    assert!(!error.retryable);
    let connection = rusqlite::Connection::open(&database).unwrap();
    let version: u32 = connection
        .query_row(
            "SELECT schema_version FROM workflow_schema_meta",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let gate_table: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='workflow_gate_outbox')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, 1);
    assert!(!gate_table);
    drop(connection);
    assert_eq!(schema_snapshot(&database), before);
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute("DROP TRIGGER interrupt_workflow_migration", [])
        .unwrap();
    drop(connection);
    WorkflowStore::open(&database).unwrap();
}

#[test]
fn bootstrap_failure_rolls_back_every_new_workflow_object() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("bootstrap.sqlite");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute("CREATE VIEW workflow_gate_outbox AS SELECT 1 AS value", [])
        .unwrap();
    drop(connection);
    let before = schema_snapshot(&database);

    for _ in 0..2 {
        let error = WorkflowStore::open(&database).unwrap_err();
        assert!(!error.retryable);
        assert_eq!(schema_snapshot(&database), before);
    }
}

#[test]
fn fresh_bootstrap_rejects_exact_and_partial_orphan_workflow_state() {
    let directory = tempfile::tempdir().unwrap();

    let exact = directory.path().join("exact-orphan.sqlite");
    drop(WorkflowStore::open(&exact).unwrap());
    let connection = rusqlite::Connection::open(&exact).unwrap();
    let orphan_payload = b"orphan-operation-response";
    connection
        .execute(
            "INSERT INTO workflow_operations (operation_namespace,operation_id,request_digest,response,created_at_ms) VALUES ('v1:orphan:1','operation','digest',?1,1)",
            [orphan_payload.as_slice()],
        )
        .unwrap();
    connection
        .execute("DROP TABLE workflow_schema_meta", [])
        .unwrap();
    drop(connection);
    let exact_before = schema_snapshot(&exact);
    for _ in 0..2 {
        let error = WorkflowStore::open(&exact).unwrap_err();
        assert_eq!(error.code, WorkflowErrorCode::CorruptStore);
        assert!(!error.retryable);
        assert_eq!(schema_snapshot(&exact), exact_before);
        let connection = rusqlite::Connection::open(&exact).unwrap();
        let retained: Vec<u8> = connection
            .query_row("SELECT response FROM workflow_operations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(retained, orphan_payload);
    }

    let partial = directory.path().join("partial-orphan.sqlite");
    let connection = rusqlite::Connection::open(&partial).unwrap();
    connection
        .execute("CREATE TABLE workflow_orphan (payload BLOB NOT NULL)", [])
        .unwrap();
    connection
        .execute(
            "INSERT INTO workflow_orphan VALUES (?1)",
            [orphan_payload.as_slice()],
        )
        .unwrap();
    drop(connection);
    let partial_before = schema_snapshot(&partial);
    for _ in 0..2 {
        let error = WorkflowStore::open(&partial).unwrap_err();
        assert_eq!(error.code, WorkflowErrorCode::CorruptStore);
        assert!(!error.retryable);
        assert_eq!(schema_snapshot(&partial), partial_before);
        let connection = rusqlite::Connection::open(&partial).unwrap();
        let retained: Vec<u8> = connection
            .query_row("SELECT payload FROM workflow_orphan", [], |row| row.get(0))
            .unwrap();
        assert_eq!(retained, orphan_payload);
    }
}

#[test]
fn v1_and_v2_exact_shape_drift_is_nonretryable_and_never_promoted() {
    let directory = tempfile::tempdir().unwrap();
    for version in [1, 2] {
        for (name, tamper) in [
            ("nullability", SchemaTamper::Nullability),
            ("default", SchemaTamper::Default),
            ("index", SchemaTamper::Index),
            ("meta-constraint", SchemaTamper::MetaConstraint),
            ("meta-row", SchemaTamper::MetaRow),
            ("meta-version", SchemaTamper::MetaVersion),
        ] {
            let database = directory.path().join(format!("v{version}-{name}.sqlite"));
            create_tampered_store(&database, version, tamper);
            let before = schema_snapshot(&database);
            for _ in 0..2 {
                let error = WorkflowStore::open(&database).unwrap_err();
                assert_eq!(error.code, WorkflowErrorCode::CorruptStore);
                assert!(!error.retryable);
                assert_eq!(schema_snapshot(&database), before);
            }
            if version == 1 {
                let connection = rusqlite::Connection::open(&database).unwrap();
                let gate_exists: bool = connection
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='workflow_gate_outbox')",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert!(!gate_exists);
            }
        }
    }
}

#[test]
fn incompatible_table_shape_fails_nonretryably_on_every_open() {
    let directory = tempfile::tempdir().unwrap();
    let extra_column_database = directory.path().join("extra-column.sqlite");
    drop(WorkflowStore::open(&extra_column_database).unwrap());
    let connection = rusqlite::Connection::open(&extra_column_database).unwrap();
    connection
        .execute(
            "ALTER TABLE workflow_gate_outbox ADD COLUMN unexpected TEXT",
            [],
        )
        .unwrap();
    drop(connection);
    for _ in 0..2 {
        let error = WorkflowStore::open(&extra_column_database).unwrap_err();
        assert_eq!(error.code, WorkflowErrorCode::CorruptStore);
        assert!(!error.retryable);
    }

    let missing_table_database = directory.path().join("missing-table.sqlite");
    drop(WorkflowStore::open(&missing_table_database).unwrap());
    let connection = rusqlite::Connection::open(&missing_table_database).unwrap();
    connection
        .execute("DROP TABLE workflow_gate_outbox", [])
        .unwrap();
    drop(connection);
    for _ in 0..2 {
        let error = WorkflowStore::open(&missing_table_database).unwrap_err();
        assert_eq!(error.code, WorkflowErrorCode::CorruptStore);
        assert!(!error.retryable);
    }
}
