use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::digest::{
    canonical_sha256, constant_time_eq, derive_principal_authority_digest, validate_sha256,
};
use crate::{AgentId, WorkflowError, WorkflowErrorCode, WORKFLOW_SCHEMA_VERSION};

pub const EXECUTION_PLAN_SCHEMA_VERSION: u16 = 1;
pub const WORK_ITEM_GATE_PROFILE: &str = "web-work-item-qa-v1";
pub const MAX_PLAN_STEPS: usize = 32;
pub const MAX_PLAN_BYTES: usize = 256 * 1024;
pub const MAX_STEP_INPUTS: usize = 64;
pub const MAX_STEP_OUTPUTS: usize = 32;
pub const MAX_STEP_ARTIFACTS: usize = 16;

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn parse(value: impl Into<String>) -> Result<Self, WorkflowError> {
                let value = value.into();
                validate_domain_id(&value)?;
                Ok(Self(value))
            }

            pub fn validate(&self) -> Result<(), WorkflowError> {
                validate_domain_id(&self.0)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

string_id!(TenantId);
string_id!(ProjectId);
string_id!(WorkItemId);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalAuthorityV1 {
    pub schema_version: u16,
    pub principal_id: String,
    pub principal_generation: u64,
    /// Domain-separated authority digest. This is not the credential digest.
    pub authority_digest: String,
}

impl PrincipalAuthorityV1 {
    /// Derives the internal authority binding from server-owned credential data.
    /// Neither the credential nor its digest is retained by this value.
    pub fn derive(
        principal_id: impl Into<String>,
        principal_generation: u64,
        credential_digest: &[u8; 32],
    ) -> Result<Self, WorkflowError> {
        let principal_id = principal_id.into();
        validate_identifier(&principal_id)?;
        if principal_generation == 0 {
            return Err(invalid("principal generation must be positive"));
        }
        Ok(Self {
            schema_version: WORKFLOW_SCHEMA_VERSION,
            principal_id,
            principal_generation,
            authority_digest: derive_principal_authority_digest(
                principal_generation,
                credential_digest,
            ),
        })
    }

    pub fn validate(&self) -> Result<(), WorkflowError> {
        if self.schema_version != WORKFLOW_SCHEMA_VERSION || self.principal_generation == 0 {
            return Err(invalid(
                "principal authority version or generation is invalid",
            ));
        }
        validate_identifier(&self.principal_id)?;
        validate_digest(&self.authority_digest)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeAuthoritySnapshotV1 {
    pub schema_version: u16,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub work_item_id: WorkItemId,
    pub agent_id: AgentId,
    pub assignment_version: u64,
    pub assignment_digest: String,
    pub organization_generation: u64,
    pub organization_digest: String,
    pub principal: PrincipalAuthorityV1,
    pub profile_id: String,
    pub profile_generation: u64,
    pub profile_digest: String,
    pub runtime_key: String,
    pub runtime_generation: u64,
    pub runtime_digest: String,
    pub policy_generation: u64,
    pub policy_digest: String,
    pub active: bool,
    pub capabilities: BTreeSet<String>,
}

impl RuntimeAuthoritySnapshotV1 {
    pub fn canonical_digest(&self) -> Result<String, WorkflowError> {
        canonical_sha256("sentinel.workflow.runtime-authority.v1", self)
    }

    pub fn validate(&self) -> Result<(), WorkflowError> {
        if self.schema_version != WORKFLOW_SCHEMA_VERSION
            || self.agent_id.0 == 0
            || self.assignment_version == 0
            || self.organization_generation == 0
            || self.profile_generation == 0
            || self.runtime_generation == 0
            || self.policy_generation == 0
            || !self.active
            || self.capabilities.is_empty()
        {
            return Err(authority("runtime authority is incomplete or inactive"));
        }
        self.tenant_id.validate()?;
        self.project_id.validate()?;
        self.work_item_id.validate()?;
        self.principal.validate()?;
        validate_identifier(&self.profile_id)?;
        validate_identifier(&self.runtime_key)?;
        for digest in [
            &self.assignment_digest,
            &self.organization_digest,
            &self.profile_digest,
            &self.runtime_digest,
            &self.policy_digest,
        ] {
            validate_digest(digest)?;
        }
        for capability in &self.capabilities {
            validate_identifier(capability)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionResourceBoundsV1 {
    pub wall_time_ms: u64,
    pub cpu_time_ms: u64,
    pub memory_bytes: u64,
    pub process_count: u32,
    pub file_bytes: u64,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
}

impl ExecutionResourceBoundsV1 {
    fn validate(&self) -> Result<(), WorkflowError> {
        if self.wall_time_ms == 0
            || self.cpu_time_ms == 0
            || self.memory_bytes == 0
            || self.process_count == 0
            || self.file_bytes == 0
            || self.stdout_bytes == 0
            || self.stderr_bytes == 0
            || self.cpu_time_ms > self.wall_time_ms
        {
            return Err(invalid("execution resource bounds are invalid"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactInputV1 {
    pub artifact_id: String,
    pub digest: String,
    pub media_type: String,
    pub mount_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchReplacementV1 {
    pub old: String,
    pub new: String,
    pub expected_occurrences: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutionToolV1 {
    InspectFile {
        path: String,
        max_bytes: u64,
    },
    WriteFile {
        path: String,
        content: String,
        expected_sha256: Option<String>,
    },
    ApplyPatch {
        path: String,
        expected_sha256: String,
        replacements: Vec<PatchReplacementV1>,
    },
    RunCommand {
        program: String,
        args: Vec<String>,
    },
    RunTests {
        suite_id: String,
        args: Vec<String>,
    },
    PackageArtifact {
        artifact_kind: String,
        media_type: String,
        paths: Vec<String>,
    },
}

impl ExecutionToolV1 {
    pub fn required_capability(&self) -> &'static str {
        match self {
            Self::InspectFile { .. } => "file.inspect",
            Self::WriteFile { .. } => "file.write",
            Self::ApplyPatch { .. } => "file.patch",
            Self::RunCommand { .. } => "command.run_allowlisted",
            Self::RunTests { .. } => "test.run",
            Self::PackageArtifact { .. } => "artifact.commit",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRuleV1 {
    pub program: String,
    pub required_arg_prefix: Vec<String>,
    pub max_args: u16,
}

impl CommandRuleV1 {
    fn validates(&self, program: &str, args: &[String]) -> bool {
        self.program == program
            && args.len() <= usize::from(self.max_args)
            && args.starts_with(&self.required_arg_prefix)
            && args.iter().all(|argument| valid_command_argument(argument))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputExpectationV1 {
    pub name: String,
    pub kind: String,
    pub required: bool,
    pub digest_algorithm: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealedOutputEvidenceV1 {
    pub name: String,
    pub kind: String,
    pub digest_algorithm: String,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactExpectationV1 {
    pub artifact_kind: String,
    pub media_type: String,
    pub required_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealedArtifactEvidenceV1 {
    pub artifact_kind: String,
    pub media_type: String,
    pub paths: Vec<String>,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateExpectationV1 {
    pub profile_id: String,
    pub profile_generation: u64,
    pub profile_digest: String,
    pub required_checks: BTreeSet<String>,
}

impl GateExpectationV1 {
    fn validate(&self) -> Result<(), WorkflowError> {
        if self.profile_id != WORK_ITEM_GATE_PROFILE
            || self.profile_generation == 0
            || self.required_checks.is_empty()
        {
            return Err(invalid("work-item gate expectation is invalid"));
        }
        validate_digest(&self.profile_digest)?;
        for check in &self.required_checks {
            validate_identifier(check)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionStepV1 {
    pub step_id: Uuid,
    pub invocation_id: Uuid,
    pub ordinal: u16,
    pub workspace_id: String,
    pub capabilities: BTreeSet<String>,
    pub inputs: Vec<ArtifactInputV1>,
    pub command_policy: Vec<CommandRuleV1>,
    pub tool: ExecutionToolV1,
    pub outputs: Vec<OutputExpectationV1>,
    pub artifacts: Vec<ArtifactExpectationV1>,
    pub gate_expectation: GateExpectationV1,
    pub resource_bounds: ExecutionResourceBoundsV1,
    pub deadline_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPlanV1 {
    pub schema_version: u16,
    pub plan_id: Uuid,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub work_item_id: WorkItemId,
    pub agent_id: AgentId,
    pub workspace_id: String,
    pub assignment_version: u64,
    pub assignment_digest: String,
    pub organization_generation: u64,
    pub organization_digest: String,
    pub principal: PrincipalAuthorityV1,
    pub profile_id: String,
    pub profile_generation: u64,
    pub profile_digest: String,
    pub runtime_key: String,
    pub runtime_generation: u64,
    pub runtime_digest: String,
    pub policy_generation: u64,
    pub policy_digest: String,
    pub created_at_unix_ms: u64,
    pub deadline_unix_ms: u64,
    pub steps: Vec<ExecutionStepV1>,
    pub request_digest: String,
}

impl ExecutionPlanV1 {
    pub fn bind_digest(mut self) -> Result<Self, WorkflowError> {
        self.request_digest.clear();
        self.request_digest = self.canonical_digest()?;
        Ok(self)
    }

    pub fn canonical_digest(&self) -> Result<String, WorkflowError> {
        let mut canonical = self.clone();
        canonical.request_digest.clear();
        canonical_sha256("sentinel.workflow.execution-plan.v1", &canonical)
    }

    pub fn validate_at(&self, now_ms: u64) -> Result<(), WorkflowError> {
        self.validate_canonical()?;
        if self.created_at_unix_ms > now_ms
            || self.deadline_unix_ms <= now_ms
            || self
                .steps
                .iter()
                .any(|step| step.deadline_unix_ms <= now_ms)
        {
            return Err(invalid("execution plan is not currently admissible"));
        }
        Ok(())
    }

    pub(crate) fn validate_canonical(&self) -> Result<(), WorkflowError> {
        if self.schema_version != EXECUTION_PLAN_SCHEMA_VERSION
            || self.plan_id.is_nil()
            || self.agent_id.0 == 0
            || self.assignment_version == 0
            || self.organization_generation == 0
            || self.profile_generation == 0
            || self.runtime_generation == 0
            || self.policy_generation == 0
            || self.deadline_unix_ms <= self.created_at_unix_ms
            || self.steps.is_empty()
            || self.steps.len() > MAX_PLAN_STEPS
        {
            return Err(invalid("execution plan header or bounds are invalid"));
        }
        self.tenant_id.validate()?;
        self.project_id.validate()?;
        self.work_item_id.validate()?;
        self.principal.validate()?;
        validate_identifier(&self.profile_id)?;
        validate_identifier(&self.runtime_key)?;
        validate_workspace_binding(&self.workspace_id, &self.project_id, &self.work_item_id)?;
        for digest in [
            &self.assignment_digest,
            &self.organization_digest,
            &self.profile_digest,
            &self.runtime_digest,
            &self.policy_digest,
            &self.request_digest,
        ] {
            validate_digest(digest)?;
        }
        let bytes = serde_json::to_vec(self).map_err(|_| invalid("execution plan is invalid"))?;
        if bytes.len() > MAX_PLAN_BYTES {
            return Err(invalid("execution plan exceeds the bounded size"));
        }
        let mut step_ids = BTreeSet::new();
        let mut invocation_ids = BTreeSet::new();
        let expected_gate = &self.steps[0].gate_expectation;
        for (index, step) in self.steps.iter().enumerate() {
            if usize::from(step.ordinal) != index
                || step.step_id.is_nil()
                || step.invocation_id.is_nil()
                || !step_ids.insert(step.step_id)
                || !invocation_ids.insert(step.invocation_id)
                || step.workspace_id != self.workspace_id
                || step.deadline_unix_ms > self.deadline_unix_ms
                || step.deadline_unix_ms <= self.created_at_unix_ms
                || step.gate_expectation != *expected_gate
            {
                return Err(invalid("execution step identity or ordering is invalid"));
            }
            validate_step(step)?;
        }
        if !constant_time_eq(&self.canonical_digest()?, &self.request_digest) {
            return Err(WorkflowError::new(
                WorkflowErrorCode::InvalidDigest,
                false,
                "execution plan digest does not match canonical content",
            ));
        }
        Ok(())
    }

    pub fn authority_matches(&self, observed: &RuntimeAuthoritySnapshotV1) -> bool {
        self.tenant_id == observed.tenant_id
            && self.project_id == observed.project_id
            && self.work_item_id == observed.work_item_id
            && self.agent_id == observed.agent_id
            && self.assignment_version == observed.assignment_version
            && self.assignment_digest == observed.assignment_digest
            && self.organization_generation == observed.organization_generation
            && self.organization_digest == observed.organization_digest
            && self.principal == observed.principal
            && self.profile_id == observed.profile_id
            && self.profile_generation == observed.profile_generation
            && self.profile_digest == observed.profile_digest
            && self.runtime_key == observed.runtime_key
            && self.runtime_generation == observed.runtime_generation
            && self.runtime_digest == observed.runtime_digest
            && self.policy_generation == observed.policy_generation
            && self.policy_digest == observed.policy_digest
            && observed.active
            && self.steps.iter().all(|step| {
                step.capabilities
                    .iter()
                    .all(|capability| observed.capabilities.contains(capability))
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemState {
    Assigned,
    Claimed,
    InProgress,
    InReview,
    Done,
    Blocked,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkItemExecutionV1 {
    pub schema_version: u16,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub work_item_id: WorkItemId,
    pub agent_id: AgentId,
    pub state: WorkItemState,
    pub version: u64,
    pub plan: ExecutionPlanV1,
    pub next_step_ordinal: u16,
    pub terminal_execution_evidence: Option<ExecutionEvidenceReadbackV1>,
    pub gate_evidence: Option<GateEvidenceReadbackV1>,
    pub blocker_code: Option<String>,
    pub updated_at_unix_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionReconcileState {
    NotFound,
    Reserved,
    Executing,
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    UnknownOutcome,
}

impl ExecutionReconcileState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::Failed
                | Self::Cancelled
                | Self::TimedOut
                | Self::UnknownOutcome
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingExecutionV1 {
    pub schema_version: u16,
    pub plan_id: Uuid,
    pub plan_digest: String,
    pub step: ExecutionStepV1,
    pub authority_snapshot_digest: String,
    pub state: ExecutionReconcileState,
    pub attempts: u16,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionEvidenceReadbackV1 {
    pub schema_version: u16,
    pub receipt_id: String,
    pub invocation_id: Uuid,
    pub plan_digest: String,
    pub step_digest: String,
    pub output_bundle_digest: String,
    pub outputs: Vec<SealedOutputEvidenceV1>,
    pub artifacts: Vec<SealedArtifactEvidenceV1>,
    pub completed_at_unix_ms: u64,
}

impl ExecutionEvidenceReadbackV1 {
    pub(crate) fn new(
        receipt_id: String,
        invocation_id: Uuid,
        plan_digest: String,
        step_digest: String,
        output_bundle_digest: String,
        outputs: Vec<SealedOutputEvidenceV1>,
        artifacts: Vec<SealedArtifactEvidenceV1>,
        completed_at_unix_ms: u64,
    ) -> Self {
        Self {
            schema_version: WORKFLOW_SCHEMA_VERSION,
            receipt_id,
            invocation_id,
            plan_digest,
            step_digest,
            output_bundle_digest,
            outputs,
            artifacts,
            completed_at_unix_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateEvidenceReadbackV1 {
    pub schema_version: u16,
    pub receipt_id: String,
    pub profile_id: String,
    pub profile_generation: u64,
    pub profile_digest: String,
    pub subject_digest: String,
    pub required_checks_digest: String,
    pub passed: bool,
    pub completed_at_unix_ms: u64,
}

impl GateEvidenceReadbackV1 {
    pub(crate) fn new(
        receipt_id: String,
        profile_generation: u64,
        profile_digest: String,
        subject_digest: String,
        required_checks_digest: String,
        completed_at_unix_ms: u64,
    ) -> Self {
        Self {
            schema_version: WORKFLOW_SCHEMA_VERSION,
            receipt_id,
            profile_id: WORK_ITEM_GATE_PROFILE.to_owned(),
            profile_generation,
            profile_digest,
            subject_digest,
            required_checks_digest,
            passed: true,
            completed_at_unix_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingCompletionEvidenceV1 {
    pub schema_version: u16,
    pub request_id: String,
    pub plan_id: Uuid,
    pub plan_digest: String,
    pub step_id: Uuid,
    pub invocation_id: Uuid,
    pub step_digest: String,
    pub authority_snapshot_digest: String,
    pub request_digest: String,
    pub created_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingGateEvidenceV1 {
    pub schema_version: u16,
    pub request_id: String,
    pub plan_id: Uuid,
    pub plan_digest: String,
    pub execution_receipt_id: String,
    pub subject_digest: String,
    pub required_checks_digest: String,
    pub expectation: GateExpectationV1,
    pub authority_snapshot_digest: String,
    pub request_digest: String,
    pub created_at_unix_ms: u64,
}

pub(crate) fn step_digest(step: &ExecutionStepV1) -> Result<String, WorkflowError> {
    canonical_sha256("sentinel.workflow.execution-step.v1", step)
}

pub(crate) fn execution_subject_digest(
    plan: &ExecutionPlanV1,
    evidence: &ExecutionEvidenceReadbackV1,
) -> Result<String, WorkflowError> {
    canonical_sha256(
        "sentinel.workflow.work-item-gate-subject.v1",
        &(
            &plan.tenant_id,
            &plan.project_id,
            &plan.work_item_id,
            plan.agent_id,
            plan.assignment_version,
            &plan.request_digest,
            evidence,
        ),
    )
}

pub fn sealed_output_bundle_digest(
    outputs: &[SealedOutputEvidenceV1],
    artifacts: &[SealedArtifactEvidenceV1],
) -> Result<String, WorkflowError> {
    canonical_sha256(
        "sentinel.workflow.sealed-output-bundle.v1",
        &(outputs, artifacts),
    )
}

fn validate_step(step: &ExecutionStepV1) -> Result<(), WorkflowError> {
    if step.capabilities.is_empty()
        || step.inputs.len() > MAX_STEP_INPUTS
        || step.outputs.is_empty()
        || step.outputs.len() > MAX_STEP_OUTPUTS
        || step.artifacts.len() > MAX_STEP_ARTIFACTS
        || !step.capabilities.contains(step.tool.required_capability())
    {
        return Err(invalid("execution step bounds or capability are invalid"));
    }
    step.resource_bounds.validate()?;
    step.gate_expectation.validate()?;
    let mut input_ids = BTreeSet::new();
    let mut input_mounts: Vec<&str> = Vec::new();
    for input in &step.inputs {
        validate_identifier(&input.artifact_id)?;
        validate_digest(&input.digest)?;
        validate_media_type(&input.media_type)?;
        validate_relative_path(&input.mount_path)?;
        if !input_ids.insert(&input.artifact_id)
            || input_mounts
                .iter()
                .any(|mount| canonical_paths_overlap(mount, &input.mount_path))
        {
            return Err(invalid(
                "artifact input identity or mount path is duplicated",
            ));
        }
        input_mounts.push(input.mount_path.as_str());
    }
    let mut output_names = BTreeSet::new();
    for output in &step.outputs {
        validate_identifier(&output.name)?;
        validate_identifier(&output.kind)?;
        if !output.required
            || output.digest_algorithm != "sha256"
            || !output_names.insert(&output.name)
        {
            return Err(invalid("output digest algorithm is unsupported"));
        }
    }
    let mut artifact_kinds = BTreeSet::new();
    let mut artifact_paths: Vec<&str> = Vec::new();
    for artifact in &step.artifacts {
        validate_identifier(&artifact.artifact_kind)?;
        validate_media_type(&artifact.media_type)?;
        if artifact.required_paths.is_empty()
            || !artifact_kinds.insert(&artifact.artifact_kind)
            || artifact
                .required_paths
                .iter()
                .collect::<BTreeSet<_>>()
                .len()
                != artifact.required_paths.len()
        {
            return Err(invalid("artifact expectation has no paths"));
        }
        for path in &artifact.required_paths {
            validate_relative_path(path)?;
            if artifact_paths
                .iter()
                .any(|existing| canonical_paths_overlap(existing, path))
                || input_mounts
                    .iter()
                    .any(|mount| canonical_paths_overlap(mount, path))
            {
                return Err(invalid("artifact output path is assigned more than once"));
            }
            artifact_paths.push(path.as_str());
        }
    }
    if let Some(destination) = mutating_file_destination(&step.tool) {
        if input_mounts
            .iter()
            .any(|mount| canonical_paths_overlap(destination, mount))
        {
            return Err(invalid(
                "mutating file destination overlaps a read-only input",
            ));
        }
    }
    validate_tool(&step.tool, &step.command_policy, &step.artifacts)
}

fn mutating_file_destination(tool: &ExecutionToolV1) -> Option<&str> {
    match tool {
        ExecutionToolV1::WriteFile { path, .. } | ExecutionToolV1::ApplyPatch { path, .. } => {
            Some(path)
        }
        _ => None,
    }
}

fn canonical_path_is_within(path: &str, root: &str) -> bool {
    std::path::Path::new(path).starts_with(std::path::Path::new(root))
}

fn canonical_paths_overlap(left: &str, right: &str) -> bool {
    canonical_path_is_within(left, right) || canonical_path_is_within(right, left)
}

fn validate_tool(
    tool: &ExecutionToolV1,
    command_policy: &[CommandRuleV1],
    artifacts: &[ArtifactExpectationV1],
) -> Result<(), WorkflowError> {
    if !matches!(tool, ExecutionToolV1::RunCommand { .. }) && !command_policy.is_empty() {
        return Err(invalid(
            "non-command tool carries dormant command authority",
        ));
    }
    match tool {
        ExecutionToolV1::InspectFile { path, max_bytes } => {
            validate_relative_path(path)?;
            if *max_bytes == 0 {
                return Err(invalid("inspect byte limit must be positive"));
            }
        }
        ExecutionToolV1::WriteFile {
            path,
            content,
            expected_sha256,
        } => {
            validate_relative_path(path)?;
            if content.len() > MAX_PLAN_BYTES {
                return Err(invalid("write content exceeds plan bound"));
            }
            if let Some(digest) = expected_sha256 {
                validate_digest(digest)?;
            }
        }
        ExecutionToolV1::ApplyPatch {
            path,
            expected_sha256,
            replacements,
        } => {
            validate_relative_path(path)?;
            validate_digest(expected_sha256)?;
            if replacements.is_empty()
                || replacements.len() > 128
                || replacements.iter().any(|replacement| {
                    replacement.old.is_empty() || replacement.expected_occurrences == 0
                })
            {
                return Err(invalid("patch replacement set is invalid"));
            }
        }
        ExecutionToolV1::RunCommand { program, args } => {
            validate_program(program)?;
            if args
                .iter()
                .any(|argument| !valid_command_argument(argument))
                || command_policy.len() != 1
            {
                return Err(invalid("command is not admitted by the exact plan policy"));
            }
            let rule = &command_policy[0];
            validate_program(&rule.program)?;
            if rule.max_args == 0
                || usize::from(rule.max_args) < rule.required_arg_prefix.len()
                || rule
                    .required_arg_prefix
                    .iter()
                    .any(|argument| !valid_command_argument(argument))
                || rule.program != program.as_str()
                || rule.required_arg_prefix.as_slice() != args.as_slice()
                || usize::from(rule.max_args) != args.len()
                || !rule.validates(program, args)
            {
                return Err(invalid("command policy is not the exact bound command"));
            }
        }
        ExecutionToolV1::RunTests { suite_id, args } => {
            validate_identifier(suite_id)?;
            if args
                .iter()
                .any(|argument| !valid_command_argument(argument))
            {
                return Err(invalid("test argument escapes the workspace"));
            }
        }
        ExecutionToolV1::PackageArtifact {
            artifact_kind,
            media_type,
            paths,
        } => {
            validate_identifier(artifact_kind)?;
            validate_media_type(media_type)?;
            if paths.is_empty()
                || paths
                    .iter()
                    .any(|path| validate_relative_path(path).is_err())
            {
                return Err(invalid("artifact package path is invalid"));
            }
            if !artifacts.iter().any(|expectation| {
                expectation.artifact_kind == *artifact_kind
                    && expectation.media_type == *media_type
                    && expectation.required_paths == *paths
            }) {
                return Err(invalid(
                    "artifact package does not match its exact expectation",
                ));
            }
        }
    }
    Ok(())
}

fn validate_workspace_binding(
    workspace_id: &str,
    project_id: &ProjectId,
    work_item_id: &WorkItemId,
) -> Result<(), WorkflowError> {
    let expected = format!("{}:{}", project_id.0, work_item_id.0);
    if workspace_id != expected {
        return Err(invalid(
            "workspace is not bound to the project and work item",
        ));
    }
    Ok(())
}

pub(crate) fn validate_identifier(value: &str) -> Result<(), WorkflowError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(invalid("workflow identifier is invalid"));
    }
    Ok(())
}

fn validate_domain_id(value: &str) -> Result<(), WorkflowError> {
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
        || !value
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || !value
            .as_bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
    {
        return Err(invalid("domain identity is not canonical"));
    }
    let mut previous_was_separator = false;
    for byte in value.bytes() {
        let is_separator = matches!(byte, b'-' | b'_');
        if is_separator && previous_was_separator {
            return Err(invalid("domain identity is not canonical"));
        }
        previous_was_separator = is_separator;
    }
    Ok(())
}

pub(crate) fn validate_digest(value: &str) -> Result<(), WorkflowError> {
    if !validate_sha256(value) {
        return Err(WorkflowError::new(
            WorkflowErrorCode::InvalidDigest,
            false,
            "workflow digest is invalid",
        ));
    }
    Ok(())
}

fn validate_relative_path(value: &str) -> Result<(), WorkflowError> {
    let path = std::path::Path::new(value);
    if value.is_empty()
        || value.contains('\\')
        || value.contains('\0')
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        || path
            .components()
            .map(|component| component.as_os_str().to_str().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("/")
            != value
    {
        return Err(invalid("workspace-relative path is invalid"));
    }
    Ok(())
}

fn validate_program(value: &str) -> Result<(), WorkflowError> {
    if value.is_empty()
        || value.contains('/')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(invalid("command program is invalid"));
    }
    Ok(())
}

fn valid_command_argument(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && !value.contains('\0')
        && !value.contains('\\')
        && !value.chars().any(char::is_control)
        && !value.starts_with('~')
        && !std::path::Path::new(value).is_absolute()
        && !std::path::Path::new(value).components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::CurDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
}

fn validate_media_type(value: &str) -> Result<(), WorkflowError> {
    if value.is_empty()
        || value.len() > 128
        || !value.contains('/')
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'+' | b'-')
        })
    {
        return Err(invalid("media type is invalid"));
    }
    Ok(())
}

fn invalid(message: &'static str) -> WorkflowError {
    WorkflowError::new(WorkflowErrorCode::InvalidInput, false, message)
}

fn authority(message: &'static str) -> WorkflowError {
    WorkflowError::new(WorkflowErrorCode::AuthorityConflict, false, message)
}
