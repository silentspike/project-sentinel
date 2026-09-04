//! Authenticated M0 company workflow and productive Workbench integration.

mod delivery_intent;
mod delivery_runtime;

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::time::Duration;

use sentinel_common::{
    events::{DomainEvent, DomainEventPayload},
    AgentId, CommandRule, WorkbenchArtifactRef, WorkbenchRequest, WorkbenchResourceLimits,
    WorkbenchTool, WORKBENCH_RUNTIME_BWRAP, WORKBENCH_SCHEMA_VERSION,
};
use sentinel_workflow::{
    collaboration_event_schema_registry, collaboration_policy_ambiguity,
    collaboration_policy_reversibility, collaboration_policy_role_name,
    collaboration_policy_separation_requirements, collaboration_policy_task_risk,
    collaboration_policy_team_shape, collaboration_policy_uncertainty, filtered_collaboration_view,
    is_collaboration_command_v1, sealed_output_bundle_digest, ArtifactExpectationV1,
    ArtifactInputV1, AuthenticatedCompanyPrincipalV1, CollaborationAdmissionBudgetV1,
    CollaborationAdmissionFenceV1, CollaborationAdmissionInputV1, CollaborationCandidateV1,
    CollaborationProgressDispositionV1, CollaborationProgressV1, CollaborationPublicationV1,
    CommandRuleV1, CompanyPrincipalKindV1, CompanyRoleV1, CompanyWorkflowCommandV1,
    CompanyWorkflowResponseV1, CompletionEvidencePort, DependencyReadiness, ExecutionPlanV1,
    ExecutionReconcileState, ExecutionResourceBoundsV1, ExecutionStepV1, ExecutionToolV1,
    GateEvidencePort, GateExpectationV1, OutputExpectationV1, PendingCompletionEvidenceV1,
    PendingExecutionV1, PrincipalAuthorityV1, ProjectId, ReversibilityV1,
    RuntimeAuthoritySnapshotV1, SealedArtifactEvidenceV1, SealedOutputEvidenceV1, TenantId,
    TerminalExecutionEvidence, UnavailableGateEvidencePort, WorkExecutionObservation,
    WorkExecutionPort, WorkItemId, WorkflowCore, WorkflowError, WorkflowErrorCode,
    WorkflowPortError, WorkflowStore, COLLABORATION_ADMISSION_SCHEMA_VERSION,
    COLLABORATION_POLICY_MAX_PARTICIPANTS, COLLABORATION_POLICY_MAX_ROUNDS,
    COLLABORATION_POLICY_MAX_STALLED_UPDATES, COLLABORATION_POLICY_MAX_TOKENS,
    COLLABORATION_POLICY_MINIMUM_NOVELTY_MICROS, COLLABORATION_POLICY_QUALITY_TOLERANCE_MICROS,
    COLLABORATION_POLICY_WINDOW_MS, EXECUTION_PLAN_SCHEMA_VERSION, WORKFLOW_SCHEMA_VERSION,
};
#[cfg(test)]
use sentinel_workflow::{AmbiguityClassV1, TaskRiskV1, UncertaintyClassV1};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::warn;
use uuid::Uuid;

use crate::delivery::{ConfiguredDeliveryCore, DeliveryStoreConfigV1};
use crate::workbench::{
    dispatch_workbench, stage_verified_artifact_inputs, WorkbenchAuthoritySnapshot,
    WorkbenchAuthoritySource, WorkbenchDispatchCommand, WorkbenchDispatchUnavailable,
    WorkbenchInvocationRecord, WorkbenchInvocationState, WorkbenchProfile,
};
use delivery_runtime::{
    LimboDeliveryEffects, LimboDeliveryPublication, WorkflowDeliveryIntegration,
    WorkflowWorkItemGate,
};

pub const CUSTOMER_COMMAND_PATH: &str = "/customer/workflow/commands";
pub const CUSTOMER_REQUEST_PATH: &str = "/customer/workflow/requests";
pub const OPERATOR_COMMAND_PATH: &str = "/operator/workflow/commands";
pub const AGENT_COMMAND_PATH: &str = "/agent/workflow/commands";
pub const OPERATOR_PROJECT_PATH: &str = "/operator/workflow/projects";
pub const OPERATOR_WORK_ITEM_PATH: &str = "/operator/workflow/work-items";
pub const OPERATOR_PROJECTION_PATH: &str = "/operator/workflow/projections";
pub const OPERATOR_EVENTS_PATH: &str = "/operator/workflow/events";
pub const DELIVERY_COMMAND_PATH: &str = "/company/delivery/commands";
pub const DELIVERY_INTENT_PATH: &str = "/company/delivery/intents";
pub const DELIVERY_LINEAGE_PATH: &str = "/company/delivery/lineage";
pub const WORKFLOW_READINESS_PATH: &str = "/company/workflow/readiness";
pub const COLLABORATION_VIEW_PATH: &str = "/company/workflow/collaboration";
pub const COLLABORATION_ADMISSION_PATH: &str = "/company/workflow/collaboration/admissions";
pub const MAX_WORKFLOW_BODY_BYTES: usize = 256 * 1024;

const PRINCIPAL_SCHEMA_VERSION: u16 = 1;
const PROFILE_GENERATION: u64 = 1;
const RUNTIME_GENERATION: u64 = 1;
const DISPATCH_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RECONCILE_BATCH: usize = 32;
const MAX_PRINCIPAL_BINDINGS_BYTES: u64 = 1024 * 1024;
const MAX_EXECUTION_INTENT_STEPS: usize = 16;
const MAX_COLLABORATION_ASSIGNMENT_LOAD: u16 = COLLABORATION_POLICY_MAX_PARTICIPANTS;
const EXECUTION_INTENT_OVERHEAD_MS: u64 = 30_000;
// A durable intent can be accepted immediately before the daemon is restarted.
// Keep restart recovery separate from the per-tool wall-time resource bound.
const EXECUTION_RESTART_RECOVERY_ALLOWANCE_MS: u64 = 190_000;
const LINUX_O_NOFOLLOW: i32 = 0o400000;
const LINUX_O_CLOEXEC: i32 = 0o2000000;

type ProductWorkflowCore = WorkflowCore<
    Arc<dyn sentinel_workflow::OrganizationRuntimePort>,
    Arc<dyn WorkExecutionPort>,
    Arc<dyn CompletionEvidencePort>,
    Arc<dyn GateEvidencePort>,
>;

type ProductDeliveryCore = ConfiguredDeliveryCore<
    WorkflowDeliveryIntegration,
    LimboDeliveryEffects,
    LimboDeliveryPublication,
>;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrincipalBindingsFile {
    schema_version: u16,
    bindings: Vec<PrincipalBinding>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrincipalBinding {
    credential_name: String,
    tenant_id: TenantId,
    principal_id: String,
    kind: CompanyPrincipalKindV1,
    role: CompanyRoleV1,
    customer_id: Option<String>,
    agent_id: Option<AgentId>,
    authority_generation: u64,
}

#[derive(Clone)]
struct BoundPrincipal {
    principal: AuthenticatedCompanyPrincipalV1,
    execution_authority: PrincipalAuthorityV1,
}

#[derive(Default)]
struct PrincipalAuthenticator {
    by_credential_digest: HashMap<String, BoundPrincipal>,
    by_principal_id: HashMap<String, BoundPrincipal>,
}

impl PrincipalAuthenticator {
    fn load(path: &Path, credentials_dir: &Path) -> Result<Self, WorkflowError> {
        let bytes = read_principal_bindings_file(path)?;
        let file: PrincipalBindingsFile =
            serde_json::from_slice(&bytes).map_err(|_| workflow_unavailable())?;
        if file.schema_version != PRINCIPAL_SCHEMA_VERSION || file.bindings.is_empty() {
            return Err(workflow_unavailable());
        }
        let mut values = Vec::with_capacity(file.bindings.len());
        for binding in file.bindings {
            validate_credential_name(&binding.credential_name)?;
            let credential_path = credentials_dir.join(&binding.credential_name);
            let credential = crate::config::read_operator_credential(&credential_path)
                .map_err(|_| workflow_unavailable())?;
            values.push((credential, binding));
        }
        Self::new(values)
    }

    fn new(values: Vec<(String, PrincipalBinding)>) -> Result<Self, WorkflowError> {
        let mut result = Self::default();
        for (credential, binding) in values {
            if credential.len() < 32 || binding.authority_generation == 0 {
                return Err(workflow_unavailable());
            }
            let credential_digest: [u8; 32] = Sha256::digest(credential.as_bytes()).into();
            let execution_authority = PrincipalAuthorityV1::derive(
                binding.principal_id.clone(),
                binding.authority_generation,
                &credential_digest,
            )?;
            let principal = AuthenticatedCompanyPrincipalV1 {
                schema_version: sentinel_workflow::COMPANY_DOMAIN_SCHEMA_VERSION,
                tenant_id: binding.tenant_id,
                principal_id: binding.principal_id,
                kind: binding.kind,
                role: binding.role,
                customer_id: binding.customer_id,
                agent_id: binding.agent_id,
                authority_generation: binding.authority_generation,
                authority_digest: execution_authority.authority_digest.clone(),
            };
            principal.validate()?;
            let bound = BoundPrincipal {
                principal: principal.clone(),
                execution_authority,
            };
            let digest_key = hex_sha256(credential.as_bytes());
            if result
                .by_credential_digest
                .insert(digest_key, bound.clone())
                .is_some()
                || result
                    .by_principal_id
                    .insert(principal.principal_id.clone(), bound)
                    .is_some()
            {
                return Err(workflow_unavailable());
            }
        }
        Ok(result)
    }

    fn authenticate(&self, headers: &HashMap<String, String>) -> Option<BoundPrincipal> {
        let credential = headers.get("authorization")?.strip_prefix("Bearer ")?;
        self.by_credential_digest
            .get(&hex_sha256(credential.as_bytes()))
            .cloned()
    }

    fn principal(&self, principal_id: &str) -> Option<BoundPrincipal> {
        self.by_principal_id.get(principal_id).cloned()
    }
}

fn read_principal_bindings_file(path: &Path) -> Result<Vec<u8>, WorkflowError> {
    let inspected = fs::symlink_metadata(path).map_err(|_| workflow_unavailable())?;
    let current_uid = fs::metadata("/proc/self")
        .map_err(|_| workflow_unavailable())?
        .uid();
    if inspected.file_type().is_symlink()
        || !inspected.is_file()
        || inspected.nlink() != 1
        || inspected.uid() != current_uid
        || inspected.mode() & 0o022 != 0
        || inspected.len() == 0
        || inspected.len() > MAX_PRINCIPAL_BINDINGS_BYTES
    {
        return Err(workflow_unavailable());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(LINUX_O_NOFOLLOW | LINUX_O_CLOEXEC)
        .open(path)
        .map_err(|_| workflow_unavailable())?;
    let opened = file.metadata().map_err(|_| workflow_unavailable())?;
    let identity = (
        opened.dev(),
        opened.ino(),
        opened.len(),
        opened.mtime_nsec(),
    );
    if identity
        != (
            inspected.dev(),
            inspected.ino(),
            inspected.len(),
            inspected.mtime_nsec(),
        )
    {
        return Err(workflow_unavailable());
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_PRINCIPAL_BINDINGS_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| workflow_unavailable())?;
    let after = fs::symlink_metadata(path).map_err(|_| workflow_unavailable())?;
    if bytes.is_empty()
        || bytes.len() as u64 > MAX_PRINCIPAL_BINDINGS_BYTES
        || identity != (after.dev(), after.ino(), after.len(), after.mtime_nsec())
    {
        return Err(workflow_unavailable());
    }
    Ok(bytes)
}

fn validate_credential_name(value: &str) -> Result<(), WorkflowError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(workflow_unavailable());
    }
    Ok(())
}

#[derive(Clone)]
struct CompanyAuthority {
    store: Arc<WorkflowStore>,
    principals: Arc<PrincipalAuthenticator>,
    workbench_profile: WorkbenchProfile,
    workbench_profile_digest: String,
    qa_profile_capabilities: BTreeSet<String>,
    agent_capabilities: Arc<HashMap<AgentId, BTreeSet<String>>>,
    runtime_health: crate::runtime_health::SharedRuntimeHealthState,
    artifact_roots: Arc<HashMap<AgentId, PathBuf>>,
}

impl CompanyAuthority {
    fn validate_plan_contract(&self, plan: &ExecutionPlanV1) -> Result<(), WorkflowPortError> {
        let project = self
            .store
            .company_project(&plan.tenant_id, &plan.project_id)
            .map_err(map_authority_store_error)?
            .ok_or(WorkflowPortError::AuthorityConflict)?;
        let work = project
            .work_items
            .get(&plan.work_item_id)
            .ok_or(WorkflowPortError::AuthorityConflict)?;
        validate_execution_contract(plan, &work.spec)
    }

    fn snapshot(
        &self,
        tenant_id: &TenantId,
        project_id: &ProjectId,
        work_item_id: &WorkItemId,
        agent_id: AgentId,
    ) -> Result<RuntimeAuthoritySnapshotV1, WorkflowPortError> {
        self.snapshot_for_admission(tenant_id, project_id, work_item_id, agent_id, true)
    }

    fn snapshot_for_admission(
        &self,
        tenant_id: &TenantId,
        project_id: &ProjectId,
        work_item_id: &WorkItemId,
        agent_id: AgentId,
        require_serving_state: bool,
    ) -> Result<RuntimeAuthoritySnapshotV1, WorkflowPortError> {
        let project = self
            .store
            .company_project(tenant_id, project_id)
            .map_err(map_authority_store_error)?
            .ok_or(WorkflowPortError::AuthorityConflict)?;
        let work = project
            .work_items
            .get(work_item_id)
            .ok_or(WorkflowPortError::AuthorityConflict)?;
        let assignment = work
            .assignments
            .iter()
            .find(|value| value.active)
            .ok_or(WorkflowPortError::AuthorityConflict)?;
        let participant = project
            .governance
            .participants
            .iter()
            .find(|value| value.agent_id == agent_id)
            .ok_or(WorkflowPortError::AuthorityConflict)?;
        let principal = self
            .principals
            .principal(&participant.principal_id)
            .ok_or(WorkflowPortError::AuthorityConflict)?;
        let capability_coverage = sentinel_workflow::execution_capability_coverage_is_admitted(
            &project, work, assignment,
        )
        .map_err(map_authority_store_error)?;
        if assignment.agent_id != agent_id
            || participant.role != assignment.role
            || principal.principal.agent_id != Some(agent_id)
            || principal.principal.role != assignment.role
            || principal.principal.tenant_id != *tenant_id
            || assignment.profile.profile_id != self.workbench_profile.id
            || assignment.profile.digest != self.workbench_profile_digest
            || assignment.profile.generation != PROFILE_GENERATION
            || project.governance.project_profile.profile_id != "web-project-v1"
            || !capability_coverage
            || (require_serving_state
                && !matches!(
                    work.state,
                    sentinel_workflow::CompanyWorkStateV1::Assigned
                        | sentinel_workflow::CompanyWorkStateV1::InProgress
                        | sentinel_workflow::CompanyWorkStateV1::InReview
                ))
        {
            return Err(WorkflowPortError::AuthorityConflict);
        }
        let capabilities = self
            .agent_capabilities
            .get(&agent_id)
            .cloned()
            .unwrap_or_default()
            .intersection(&self.workbench_profile.capabilities)
            .cloned()
            .collect::<BTreeSet<_>>();
        if capabilities.is_empty() {
            return Err(WorkflowPortError::AuthorityConflict);
        }
        let runtime_digest = domain_digest(
            "sentinel.workflow.runtime.v1",
            &[
                WORKBENCH_RUNTIME_BWRAP.as_bytes(),
                self.workbench_profile_digest.as_bytes(),
            ],
        );
        let snapshot = RuntimeAuthoritySnapshotV1 {
            schema_version: WORKFLOW_SCHEMA_VERSION,
            tenant_id: tenant_id.clone(),
            project_id: project_id.clone(),
            work_item_id: work_item_id.clone(),
            agent_id,
            assignment_version: assignment.assignment_version,
            assignment_digest: assignment
                .canonical_digest()
                .map_err(map_authority_store_error)?,
            organization_generation: assignment.organization_generation,
            organization_digest: assignment.organization_digest.clone(),
            principal: principal.execution_authority,
            profile_id: assignment.profile.profile_id.clone(),
            profile_generation: assignment.profile.generation,
            profile_digest: assignment.profile.digest.clone(),
            runtime_key: WORKBENCH_RUNTIME_BWRAP.to_owned(),
            runtime_generation: RUNTIME_GENERATION,
            runtime_digest,
            policy_generation: project.governance.project_profile.generation,
            policy_digest: project.governance.project_profile.digest.clone(),
            active: true,
            capabilities,
        };
        snapshot
            .validate()
            .map_err(|_| WorkflowPortError::AuthorityConflict)?;
        Ok(snapshot)
    }

    fn plan_from_intent(
        &self,
        principal: &BoundPrincipal,
        operation_id: Uuid,
        intent: &ExecutionIntentV1,
        now_ms: u64,
    ) -> Result<ExecutionIntentAdmission, WorkflowError> {
        let agent_id = principal
            .principal
            .agent_id
            .ok_or_else(execution_authority_conflict)?;
        let project = self
            .store
            .company_project(&principal.principal.tenant_id, &intent.project_id)?
            .ok_or_else(|| {
                WorkflowError::new(
                    WorkflowErrorCode::NotFound,
                    false,
                    "execution project was not found",
                )
            })?;
        let spec = project
            .work_items
            .get(&intent.work_item_id)
            .ok_or_else(|| {
                WorkflowError::new(
                    WorkflowErrorCode::NotFound,
                    false,
                    "execution work item was not found",
                )
            })?;
        let existing = self.store.work_item(
            &principal.principal.tenant_id,
            &intent.project_id,
            &intent.work_item_id,
        )?;
        if let Some(existing) = existing {
            if existing.plan.plan_id == operation_id {
                let authority = self
                    .snapshot_for_admission(
                        &principal.principal.tenant_id,
                        &intent.project_id,
                        &intent.work_item_id,
                        agent_id,
                        false,
                    )
                    .map_err(execution_intent_port_error)?;
                if authority.principal != principal.execution_authority
                    || !existing.plan.authority_matches(&authority)
                {
                    return Err(execution_authority_conflict());
                }
                if !execution_intent_matches_plan(intent, &existing.plan) {
                    return Err(WorkflowError::new(
                        WorkflowErrorCode::IdempotencyConflict,
                        false,
                        "execution intent content changed for an existing operation",
                    ));
                }
                return Ok(ExecutionIntentAdmission {
                    plan: existing.plan,
                    authority,
                    replay: true,
                });
            }

            let authority = self
                .snapshot(
                    &principal.principal.tenant_id,
                    &intent.project_id,
                    &intent.work_item_id,
                    agent_id,
                )
                .map_err(execution_intent_port_error)?;
            if authority.principal != principal.execution_authority {
                return Err(execution_authority_conflict());
            }
            return Err(WorkflowError::new(
                WorkflowErrorCode::VersionConflict,
                false,
                "work item already has a different execution intent",
            ));
        }

        let authority = self
            .snapshot(
                &principal.principal.tenant_id,
                &intent.project_id,
                &intent.work_item_id,
                agent_id,
            )
            .map_err(execution_intent_port_error)?;
        if authority.principal != principal.execution_authority {
            return Err(execution_authority_conflict());
        }
        let inputs = self.materialize_execution_inputs(&project, &spec.spec, agent_id)?;
        let created_at_unix_ms = now_ms;
        let deadline_unix_ms = execution_deadline_unix_ms(
            now_ms,
            self.workbench_profile.resource_ceilings.wall_time_ms,
            intent.tools.len(),
        )?;
        let plan = build_execution_plan(
            operation_id,
            &authority,
            &spec.spec,
            inputs,
            &self.workbench_profile,
            intent,
            created_at_unix_ms,
            deadline_unix_ms,
        )?;
        Ok(ExecutionIntentAdmission {
            plan,
            authority,
            replay: false,
        })
    }

    fn materialize_execution_inputs(
        &self,
        project: &sentinel_workflow::ProjectV1,
        spec: &sentinel_workflow::CompanyWorkItemSpecV1,
        destination_agent: AgentId,
    ) -> Result<Vec<ArtifactInputV1>, WorkflowError> {
        let mut inputs = Vec::new();
        for binding in &spec.inputs {
            let producer = project
                .work_items
                .get(&binding.producer_work_item_id)
                .ok_or_else(execution_authority_conflict)?;
            let assignment = producer
                .assignments
                .iter()
                .find(|value| value.active)
                .ok_or_else(execution_authority_conflict)?;
            let output_contract = producer
                .spec
                .outputs
                .iter()
                .find(|value| value.name == binding.producer_output_name)
                .ok_or_else(execution_authority_conflict)?;
            let output_receipt = producer
                .output_receipts
                .iter()
                .find(|value| value.name == binding.producer_output_name)
                .ok_or_else(execution_authority_conflict)?;
            if producer.state != sentinel_workflow::CompanyWorkStateV1::Done
                || output_contract.contract_generation != binding.expected_contract_generation
                || output_contract.contract_digest != binding.expected_contract_digest
                || output_receipt.contract_generation != binding.expected_contract_generation
                || output_receipt.contract_digest != binding.expected_contract_digest
            {
                return Err(execution_authority_conflict());
            }
            let execution = self
                .store
                .work_item(
                    &project.tenant_id,
                    &project.project_id,
                    &binding.producer_work_item_id,
                )?
                .ok_or_else(execution_authority_conflict)?;
            let terminal = execution
                .terminal_execution_evidence
                .as_ref()
                .filter(|_| execution.state == sentinel_workflow::WorkItemState::Done)
                .ok_or_else(execution_authority_conflict)?;
            let output = terminal
                .outputs
                .iter()
                .find(|value| value.name == binding.producer_output_name)
                .filter(|value| value.digest == output_receipt.content_digest)
                .ok_or_else(execution_authority_conflict)?;
            let matching_artifacts = terminal
                .artifacts
                .iter()
                .filter(|value| {
                    value.artifact_kind == output.kind
                        && value.media_type == output_contract.media_type
                })
                .collect::<Vec<_>>();
            let [artifact] = matching_artifacts.as_slice() else {
                return Err(execution_authority_conflict());
            };
            let staged = stage_verified_artifact_inputs(
                &self.artifact_roots,
                assignment.agent_id,
                destination_agent,
                &project.project_id.0,
                &spec.work_item_id.0,
                &artifact.digest,
                Some(&artifact.artifact_kind),
                &artifact.media_type,
            )
            .map_err(|_| execution_input_unavailable())?;
            if staged.is_empty() {
                return Err(execution_authority_conflict());
            }
            inputs.extend(staged.into_iter().map(|value| ArtifactInputV1 {
                artifact_id: value.artifact_id,
                digest: value.sha256,
                media_type: value.media_type,
                mount_path: value.mount_path,
            }));
        }
        inputs.sort_by(|left, right| {
            left.mount_path
                .cmp(&right.mount_path)
                .then_with(|| left.artifact_id.cmp(&right.artifact_id))
        });
        if inputs.windows(2).any(|pair| {
            pair[0].mount_path == pair[1].mount_path || pair[0].artifact_id == pair[1].artifact_id
        }) {
            return Err(execution_authority_conflict());
        }
        Ok(inputs)
    }

    fn workbench_snapshot(
        &self,
        tenant_id: &TenantId,
        project_id: &ProjectId,
        work_item_id: &WorkItemId,
        agent_id: AgentId,
    ) -> anyhow::Result<WorkbenchAuthoritySnapshot> {
        let runtime = self
            .snapshot(tenant_id, project_id, work_item_id, agent_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let principal = self
            .principals
            .principal(&runtime.principal.principal_id)
            .filter(|principal| {
                principal.execution_authority == runtime.principal
                    && principal.principal.agent_id == Some(agent_id)
                    && principal.principal.tenant_id == *tenant_id
            })
            .ok_or_else(|| anyhow::anyhow!("workbench principal authority changed"))?;
        let role = principal.principal.role;
        let role_capabilities = role_capabilities(
            role,
            &self.workbench_profile.capabilities,
            &self.qa_profile_capabilities,
        );
        Ok(WorkbenchAuthoritySnapshot {
            agent_id,
            caller_id: runtime.principal.principal_id,
            caller_role: collaboration_policy_role_name(role).to_owned(),
            project_id: project_id.0.clone(),
            work_item_id: work_item_id.0.clone(),
            assignment_version: runtime.assignment_version,
            credential_generation: runtime.principal.principal_generation,
            policy_digest: runtime.policy_digest,
            tool_profile: runtime.profile_id,
            tool_profile_digest: runtime.profile_digest,
            runtime_key: runtime.runtime_key,
            assignment_active: true,
            agent_capabilities: runtime.capabilities.clone(),
            role_capabilities,
            assignment_capabilities: runtime.capabilities.clone(),
            project_capabilities: runtime.capabilities.clone(),
            profile_capabilities: self.workbench_profile.capabilities.clone(),
        })
    }
}

fn execution_deadline_unix_ms(
    now_ms: u64,
    tool_wall_time_ms: u64,
    step_count: usize,
) -> Result<u64, WorkflowError> {
    let execution_ms = tool_wall_time_ms
        .checked_mul(u64::try_from(step_count).map_err(|_| execution_intent_invalid())?)
        .and_then(|value| value.checked_add(EXECUTION_INTENT_OVERHEAD_MS))
        .and_then(|value| value.checked_add(EXECUTION_RESTART_RECOVERY_ALLOWANCE_MS))
        .ok_or_else(execution_intent_invalid)?;
    now_ms
        .checked_add(execution_ms)
        .ok_or_else(execution_intent_invalid)
}

fn build_execution_plan(
    operation_id: Uuid,
    authority: &RuntimeAuthoritySnapshotV1,
    spec: &sentinel_workflow::CompanyWorkItemSpecV1,
    inputs: Vec<ArtifactInputV1>,
    profile: &WorkbenchProfile,
    intent: &ExecutionIntentV1,
    created_at_unix_ms: u64,
    deadline_unix_ms: u64,
) -> Result<ExecutionPlanV1, WorkflowError> {
    if operation_id.is_nil()
        || intent.tools.is_empty()
        || intent.tools.len() > MAX_EXECUTION_INTENT_STEPS
        || deadline_unix_ms <= created_at_unix_ms
        || spec.outputs.len() != 1
        || authority.project_id != intent.project_id
        || authority.work_item_id != intent.work_item_id
    {
        return Err(execution_intent_invalid());
    }
    let artifact_kind = match spec.required_role {
        CompanyRoleV1::Designer => "design_specification",
        CompanyRoleV1::Developer => "source_tree",
        _ => return Err(execution_authority_conflict()),
    };
    let output = &spec.outputs[0];
    let final_tool = intent.tools.last().ok_or_else(execution_intent_invalid)?;
    let (final_media_type, final_paths) = match final_tool {
        ExecutionToolV1::PackageArtifact {
            artifact_kind: observed_kind,
            media_type,
            paths,
        } if observed_kind == artifact_kind && media_type == &output.media_type => {
            (media_type.clone(), paths.clone())
        }
        _ => return Err(execution_intent_invalid()),
    };
    if intent.tools[..intent.tools.len() - 1]
        .iter()
        .any(|tool| matches!(tool, ExecutionToolV1::PackageArtifact { .. }))
    {
        return Err(execution_intent_invalid());
    }
    let workspace_id = format!("{}:{}", intent.project_id.0, intent.work_item_id.0);
    let gate_expectation = GateExpectationV1 {
        profile_id: spec.quality_gate.gate_id.clone(),
        profile_generation: spec.quality_gate.generation,
        profile_digest: spec.quality_gate.digest.clone(),
        required_checks: BTreeSet::from(["html_structure".to_owned()]),
    };
    let output_expectation = OutputExpectationV1 {
        name: output.name.clone(),
        kind: artifact_kind.to_owned(),
        required: true,
        digest_algorithm: output.digest_algorithm.clone(),
    };
    let resource_bounds = ExecutionResourceBoundsV1 {
        wall_time_ms: profile.resource_ceilings.wall_time_ms,
        cpu_time_ms: profile.resource_ceilings.cpu_time_ms,
        memory_bytes: profile.resource_ceilings.memory_bytes,
        process_count: profile.resource_ceilings.process_count,
        file_bytes: profile.resource_ceilings.file_bytes,
        stdout_bytes: profile.resource_ceilings.stdout_bytes,
        stderr_bytes: profile.resource_ceilings.stderr_bytes,
    };
    let operation_digest = operation_id.to_string();
    let mut steps = Vec::with_capacity(intent.tools.len());
    for (index, tool) in intent.tools.iter().cloned().enumerate() {
        let required_capability = tool.required_capability().to_owned();
        if !authority.capabilities.contains(&required_capability) {
            return Err(execution_authority_conflict());
        }
        let command_policy = match &tool {
            ExecutionToolV1::RunCommand { program, args } => {
                if !profile
                    .command_rules
                    .iter()
                    .any(|rule| rule.allows(program, args))
                {
                    return Err(execution_intent_invalid());
                }
                vec![CommandRuleV1 {
                    program: program.clone(),
                    required_arg_prefix: args.clone(),
                    max_args: u16::try_from(args.len()).map_err(|_| execution_intent_invalid())?,
                }]
            }
            _ => Vec::new(),
        };
        let artifacts = if index + 1 == intent.tools.len() {
            vec![ArtifactExpectationV1 {
                artifact_kind: artifact_kind.to_owned(),
                media_type: final_media_type.clone(),
                required_paths: final_paths.clone(),
            }]
        } else {
            Vec::new()
        };
        let ordinal = u16::try_from(index).map_err(|_| execution_intent_invalid())?;
        steps.push(ExecutionStepV1 {
            step_id: stable_operation_id(
                "sentinel.workflow.execution-intent-step.v1",
                &operation_digest,
                u64::from(ordinal) + 1,
            ),
            invocation_id: stable_operation_id(
                "sentinel.workflow.execution-intent-invocation.v1",
                &operation_digest,
                u64::from(ordinal) + 1,
            ),
            ordinal,
            workspace_id: workspace_id.clone(),
            capabilities: BTreeSet::from([required_capability]),
            inputs: inputs.clone(),
            command_policy,
            tool,
            outputs: vec![output_expectation.clone()],
            artifacts,
            gate_expectation: gate_expectation.clone(),
            resource_bounds: resource_bounds.clone(),
            deadline_unix_ms,
        });
    }
    ExecutionPlanV1 {
        schema_version: EXECUTION_PLAN_SCHEMA_VERSION,
        plan_id: operation_id,
        tenant_id: authority.tenant_id.clone(),
        project_id: authority.project_id.clone(),
        work_item_id: authority.work_item_id.clone(),
        agent_id: authority.agent_id,
        workspace_id,
        assignment_version: authority.assignment_version,
        assignment_digest: authority.assignment_digest.clone(),
        organization_generation: authority.organization_generation,
        organization_digest: authority.organization_digest.clone(),
        principal: authority.principal.clone(),
        profile_id: authority.profile_id.clone(),
        profile_generation: authority.profile_generation,
        profile_digest: authority.profile_digest.clone(),
        runtime_key: authority.runtime_key.clone(),
        runtime_generation: authority.runtime_generation,
        runtime_digest: authority.runtime_digest.clone(),
        policy_generation: authority.policy_generation,
        policy_digest: authority.policy_digest.clone(),
        created_at_unix_ms,
        deadline_unix_ms,
        steps,
        request_digest: String::new(),
    }
    .bind_digest()
}

fn execution_intent_invalid() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::InvalidInput,
        false,
        "execution intent is invalid or exceeds the bounded M0 profile",
    )
}

fn execution_authority_conflict() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::AuthorityConflict,
        false,
        "execution intent authority is invalid or stale",
    )
}

fn execution_input_unavailable() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::ExecutionUnavailable,
        true,
        "execution dependency artifact is unavailable",
    )
}

fn execution_intent_port_error(error: WorkflowPortError) -> WorkflowError {
    match error {
        WorkflowPortError::Unavailable => WorkflowError::new(
            WorkflowErrorCode::OrganizationUnavailable,
            true,
            "execution authority is unavailable",
        ),
        WorkflowPortError::AuthorityConflict => execution_authority_conflict(),
        WorkflowPortError::Rejected => execution_intent_invalid(),
        WorkflowPortError::TimedOut => WorkflowError::new(
            WorkflowErrorCode::OrganizationUnavailable,
            true,
            "execution authority lookup timed out",
        ),
        WorkflowPortError::UnknownOutcome => WorkflowError::new(
            WorkflowErrorCode::UnknownOutcome,
            true,
            "execution authority outcome is unknown",
        ),
    }
}

impl sentinel_workflow::OrganizationRuntimePort for CompanyAuthority {
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
        self.snapshot(tenant_id, project_id, work_item_id, agent_id)
    }
}

impl WorkbenchAuthoritySource for CompanyAuthority {
    fn current_for_request(
        &self,
        request: &WorkbenchRequest,
    ) -> anyhow::Result<WorkbenchAuthoritySnapshot> {
        let principal = self
            .principals
            .principal(&request.caller_id)
            .ok_or_else(|| anyhow::anyhow!("workbench principal is unavailable"))?;
        self.workbench_snapshot(
            &principal.principal.tenant_id,
            &ProjectId::parse(&request.project_id)?,
            &WorkItemId::parse(&request.work_item_id)?,
            request.agent_id,
        )
    }

    fn current_for_record(
        &self,
        record: &WorkbenchInvocationRecord,
    ) -> anyhow::Result<WorkbenchAuthoritySnapshot> {
        let principal = self
            .principals
            .principal(&record.caller_id)
            .ok_or_else(|| anyhow::anyhow!("workbench principal is unavailable"))?;
        self.workbench_snapshot(
            &principal.principal.tenant_id,
            &ProjectId::parse(&record.project_id)?,
            &WorkItemId::parse(&record.work_item_id)?,
            record.agent_id,
        )
    }
}

fn validate_execution_contract(
    plan: &ExecutionPlanV1,
    spec: &sentinel_workflow::CompanyWorkItemSpecV1,
) -> Result<(), WorkflowPortError> {
    let final_step = plan.steps.last().ok_or(WorkflowPortError::Rejected)?;
    let derived_inputs = plan
        .steps
        .first()
        .map(|step| step.inputs.as_slice())
        .ok_or(WorkflowPortError::Rejected)?;
    if spec.inputs.is_empty() != derived_inputs.is_empty()
        || plan
            .steps
            .iter()
            .any(|step| step.inputs.as_slice() != derived_inputs)
        || final_step.outputs.len() != spec.outputs.len()
        || final_step.outputs.iter().any(|observed| {
            !observed.required
                || !spec.outputs.iter().any(|expected| {
                    observed.name == expected.name
                        && observed.digest_algorithm == expected.digest_algorithm
                })
        })
        || final_step.gate_expectation.profile_id != spec.quality_gate.gate_id
        || final_step.gate_expectation.profile_generation != spec.quality_gate.generation
        || final_step.gate_expectation.profile_digest != spec.quality_gate.digest
    {
        return Err(WorkflowPortError::AuthorityConflict);
    }
    Ok(())
}

fn execution_intent_matches_plan(intent: &ExecutionIntentV1, plan: &ExecutionPlanV1) -> bool {
    intent.project_id == plan.project_id
        && intent.work_item_id == plan.work_item_id
        && intent.tools.len() == plan.steps.len()
        && intent
            .tools
            .iter()
            .zip(&plan.steps)
            .all(|(tool, step)| tool == &step.tool)
}

fn role_capabilities(
    role: CompanyRoleV1,
    authoring_capabilities: &BTreeSet<String>,
    qa_capabilities: &BTreeSet<String>,
) -> BTreeSet<String> {
    match role {
        CompanyRoleV1::Designer | CompanyRoleV1::Developer => authoring_capabilities.clone(),
        CompanyRoleV1::Qa => qa_capabilities.clone(),
        CompanyRoleV1::Customer
        | CompanyRoleV1::Sales
        | CompanyRoleV1::ProjectManager
        | CompanyRoleV1::TechnicalLead
        | CompanyRoleV1::ReleaseManager
        | CompanyRoleV1::Gaia => BTreeSet::new(),
    }
}

fn derive_collaboration_admission_command(
    authority: &CompanyAuthority,
    project: &sentinel_workflow::ProjectV1,
    request: CollaborationAdmissionRequest,
    source_request_digest: String,
) -> Result<CompanyWorkflowCommandV1, WorkflowError> {
    match request {
        CollaborationAdmissionRequest::Admit {
            project_id,
            work_item_id,
            expected_version,
            expected_benefit_ref,
        } => {
            if project.project_id != project_id || project.version != expected_version {
                return Err(collaboration_authority_conflict());
            }
            let work = project
                .work_items
                .get(&work_item_id)
                .ok_or_else(collaboration_authority_conflict)?;
            let assignment = work
                .assignments
                .iter()
                .find(|assignment| assignment.active)
                .ok_or_else(collaboration_authority_conflict)?;
            let capacity = authority
                .store
                .collaboration_capacity_snapshot(&project.tenant_id, &project.project_id)?;
            let now_ms = now_unix_ms();
            let remaining_cost_budget_micros = project
                .cost_ceiling_micros
                .checked_sub(project.reserved_cost_micros)
                .and_then(|value| value.checked_sub(project.committed_cost_micros))
                .and_then(|value| value.checked_sub(capacity.project_reserved_cost_micros))
                .ok_or_else(collaboration_authority_conflict)?;
            let remaining_time_budget_ms = COLLABORATION_POLICY_WINDOW_MS;
            let deadline_unix_ms = now_ms
                .checked_add(remaining_time_budget_ms)
                .ok_or_else(collaboration_authority_conflict)?;
            let task_risk = collaboration_policy_task_risk(work.spec.required_role);
            let reversibility = collaboration_policy_reversibility(work.spec.required_role);
            let ambiguity = collaboration_policy_ambiguity(work.spec.required_role);
            let uncertainty = collaboration_policy_uncertainty(project, &work_item_id);
            let evidence_conflict = project.dissent_records.iter().any(|dissent| {
                project.collaboration_sessions.iter().any(|session| {
                    session.session_id == dissent.session_id
                        && session.work_item_id.as_ref() == Some(&work_item_id)
                })
            });
            let mut required_handoff_agents = Vec::new();
            for dependency_id in &work.spec.dependency_ids {
                let dependency = project
                    .work_items
                    .get(dependency_id)
                    .ok_or_else(collaboration_authority_conflict)?;
                let mut active_assignments = dependency
                    .assignments
                    .iter()
                    .filter(|candidate| candidate.active);
                let dependency_assignment = active_assignments
                    .next()
                    .ok_or_else(collaboration_authority_conflict)?;
                if active_assignments.next().is_some() {
                    return Err(collaboration_authority_conflict());
                }
                if dependency_assignment.agent_id != assignment.agent_id
                    && !required_handoff_agents.contains(&dependency_assignment.agent_id)
                {
                    required_handoff_agents.push(dependency_assignment.agent_id);
                }
            }
            required_handoff_agents.sort_unstable_by_key(|agent_id| agent_id.0);
            let capability_topology = project
                .governance
                .participants
                .iter()
                .map(|participant| (participant.agent_id, participant.specialties.clone()))
                .collect::<Vec<_>>();
            let (directed_handoff_required, specialist_panel_required) =
                collaboration_policy_team_shape(
                    assignment.agent_id,
                    &work.spec.required_specialties,
                    &capability_topology,
                    &required_handoff_agents,
                )?;
            let separation_requirements = collaboration_policy_separation_requirements(
                work.spec.required_role,
                task_risk,
                ambiguity,
                uncertainty,
                evidence_conflict,
            );
            let max_participants = u16::try_from(
                project
                    .governance
                    .participants
                    .len()
                    .min(usize::from(MAX_COLLABORATION_ASSIGNMENT_LOAD)),
            )
            .map_err(|_| collaboration_invalid())?;
            let budget = CollaborationAdmissionBudgetV1 {
                max_participants,
                max_rounds: COLLABORATION_POLICY_MAX_ROUNDS,
                max_tokens: COLLABORATION_POLICY_MAX_TOKENS,
                max_cost_micros: work.spec.budget_micros.min(remaining_cost_budget_micros),
                deadline_unix_ms,
                minimum_novelty_micros: COLLABORATION_POLICY_MINIMUM_NOVELTY_MICROS,
                max_stalled_updates: COLLABORATION_POLICY_MAX_STALLED_UPDATES,
            };
            let assignment_digest = assignment.canonical_digest()?;
            let data_provenance_digest = collaboration_input_provenance_digest(project, work)?;
            let runtime_health = authority
                .runtime_health
                .read()
                .map_err(|_| workflow_unavailable())?
                .clone();
            let candidates = project
                .governance
                .participants
                .iter()
                .map(|participant| {
                    let bound_principal = authority
                        .principals
                        .principal(&participant.principal_id)
                        .filter(|bound| {
                            bound.principal.tenant_id == project.tenant_id
                                && bound.principal.agent_id == Some(participant.agent_id)
                                && bound.principal.role == participant.role
                        });
                    let runtime_capabilities = authority
                        .agent_capabilities
                        .get(&participant.agent_id)
                        .cloned();
                    let runtime_available = runtime_health
                        .agents
                        .iter()
                        .find(|agent| agent.agent_id == participant.agent_id.0)
                        .map(crate::runtime_health::classify_runtime_agent)
                        == Some(crate::runtime_health::RuntimeAgentHealthClass::Healthy);
                    let active = bound_principal.is_some()
                        && runtime_capabilities.is_some()
                        && runtime_available;
                    let required_tools = role_capabilities(
                        participant.role,
                        &authority.workbench_profile.capabilities,
                        &authority.qa_profile_capabilities,
                    );
                    let tools_available = runtime_capabilities
                        .as_ref()
                        .is_some_and(|available| required_tools.is_subset(available));
                    let tool_material =
                        serde_json::to_vec(&runtime_capabilities.clone().unwrap_or_default())
                            .map_err(|_| collaboration_invalid())?;
                    Ok(CollaborationCandidateV1 {
                        agent_id: participant.agent_id,
                        permanent_role: participant.role,
                        mandate: sentinel_workflow::collaboration_policy_mandate(participant.role),
                        active,
                        authority_scope_digest: assignment_digest.clone(),
                        organization_generation: assignment.organization_generation,
                        organization_digest: assignment.organization_digest.clone(),
                        assignment_load: capacity
                            .assignment_load
                            .get(&participant.agent_id.0)
                            .copied()
                            .unwrap_or(0),
                        assignment_limit: MAX_COLLABORATION_ASSIGNMENT_LOAD,
                        capabilities: participant.specialties.clone(),
                        privacy_classes: BTreeSet::from(["project-internal".to_owned()]),
                        runtime_available,
                        tools_available,
                        model_family: "model-unresolved".to_owned(),
                        prompt_digest: participant.profile.digest.clone(),
                        tool_set_digest: domain_digest(
                            "sentinel.workflow.collaboration-tool-set.v1",
                            &[&tool_material],
                        ),
                        data_provenance_digest: data_provenance_digest.clone(),
                        prior_claim_correlation_digest: None,
                        queue_delay_ms: u64::from(
                            capacity
                                .assignment_load
                                .get(&participant.agent_id.0)
                                .copied()
                                .unwrap_or(0),
                        )
                        .saturating_mul(1_000),
                        estimated_cost_micros: 0,
                    })
                })
                .collect::<Result<Vec<_>, WorkflowError>>()?;
            let input = CollaborationAdmissionInputV1 {
                schema_version: COLLABORATION_ADMISSION_SCHEMA_VERSION,
                tenant_id: project.tenant_id.clone(),
                project_id: project.project_id.clone(),
                work_item_id,
                owner: assignment.agent_id,
                task_family: project.governance.project_profile.profile_id.clone(),
                input_class: collaboration_policy_role_name(work.spec.required_role).to_owned(),
                task_risk,
                reversibility,
                ambiguity,
                required_capabilities: work.spec.required_specialties.clone(),
                uncertainty,
                evidence_conflict,
                directed_handoff_required,
                required_handoff_agents,
                specialist_panel_required,
                separation_requirements,
                privacy_class: "project-internal".to_owned(),
                authority_conflict: false,
                privacy_conflict: false,
                human_approval_required: reversibility == ReversibilityV1::Irreversible,
                remaining_cost_budget_micros,
                remaining_time_budget_ms,
                organization_generation: assignment.organization_generation,
                organization_digest: assignment.organization_digest.clone(),
                assignment_id: assignment.assignment_id.clone(),
                assignment_version: assignment.assignment_version,
                assignment_digest,
                behavior_policy_generation: project.governance.project_profile.generation,
                behavior_policy_digest: project.governance.project_profile.digest.clone(),
                learned_reliability_enabled: false,
                collaboration_generation: project.collaboration_generation,
                quality_tolerance_micros: COLLABORATION_POLICY_QUALITY_TOLERANCE_MICROS,
                permitted_packet_classes: BTreeSet::from([
                    "decision".to_owned(),
                    "evidence".to_owned(),
                    "finding".to_owned(),
                    "handoff".to_owned(),
                ]),
                budget,
            };
            Ok(CompanyWorkflowCommandV1::AdmitCollaboration {
                project_id,
                expected_version,
                source_request_digest,
                input,
                candidates,
                reliability: project.collaboration_reliability.clone(),
                expected_benefit_ref,
            })
        }
        CollaborationAdmissionRequest::Progress {
            project_id,
            expected_version,
            admission_id,
            progress,
        } => {
            if project.project_id != project_id || project.version != expected_version {
                return Err(collaboration_authority_conflict());
            }
            let decision = project
                .collaboration_admissions
                .iter()
                .find(|decision| decision.admission_id == admission_id)
                .ok_or_else(collaboration_authority_conflict)?;
            let work = project
                .work_items
                .get(&decision.input.work_item_id)
                .ok_or_else(collaboration_authority_conflict)?;
            let assignment = work
                .assignments
                .iter()
                .find(|assignment| assignment.active)
                .ok_or_else(collaboration_authority_conflict)?;
            Ok(CompanyWorkflowCommandV1::ProgressCollaborationAdmission {
                project_id,
                expected_version,
                source_request_digest,
                admission_id,
                fence: CollaborationAdmissionFenceV1 {
                    organization_generation: assignment.organization_generation,
                    organization_digest: assignment.organization_digest.clone(),
                    assignment_id: assignment.assignment_id.clone(),
                    assignment_version: assignment.assignment_version,
                    assignment_digest: assignment.canonical_digest()?,
                    behavior_policy_generation: project.governance.project_profile.generation,
                    behavior_policy_digest: project.governance.project_profile.digest.clone(),
                    collaboration_generation: project.collaboration_generation,
                },
                progress: CollaborationProgressV1 {
                    expected_transition_sequence: progress.expected_transition_sequence,
                    rounds_delta: 1,
                    tokens_delta: 0,
                    cost_delta_micros: 0,
                    novelty_micros: progress.novelty_micros,
                    novelty_digest: progress.novelty_digest,
                    milestone_digest: progress.milestone_digest,
                    work_digest: progress.work_digest,
                    disposition: progress.disposition,
                    reason_ref: progress.reason_ref,
                },
            })
        }
    }
}

fn collaboration_input_provenance_digest(
    project: &sentinel_workflow::ProjectV1,
    work: &sentinel_workflow::CompanyWorkItemV1,
) -> Result<String, WorkflowError> {
    let mut input_receipts = Vec::with_capacity(work.spec.inputs.len());
    for input in &work.spec.inputs {
        let producer = project
            .work_items
            .get(&input.producer_work_item_id)
            .ok_or_else(collaboration_authority_conflict)?;
        let receipt = producer
            .output_receipts
            .iter()
            .find(|receipt| {
                receipt.name == input.producer_output_name
                    && receipt.contract_generation == input.expected_contract_generation
                    && receipt.contract_digest == input.expected_contract_digest
            })
            .ok_or_else(collaboration_authority_conflict)?;
        input_receipts.push((input, receipt));
    }
    let material = serde_json::to_vec(&(
        project.agreement_digest.as_str(),
        &work.spec.work_item_id,
        work.spec.objective.as_str(),
        &input_receipts,
    ))
    .map_err(|_| collaboration_invalid())?;
    Ok(domain_digest(
        "sentinel.workflow.collaboration-data-provenance.v1",
        &[&material],
    ))
}

fn collaboration_invalid() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::InvalidInput,
        false,
        "collaboration admission is invalid",
    )
}

fn collaboration_authority_conflict() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::AuthorityConflict,
        false,
        "collaboration admission authority is stale",
    )
}

fn collaboration_participant_admission_view(
    decision: &sentinel_workflow::CollaborationAdmissionDecisionV1,
    agent_id: AgentId,
) -> CollaborationAdmissionParticipantView {
    let routes = decision
        .routes
        .iter()
        .filter(|route| route.from == agent_id || route.to == agent_id)
        .cloned()
        .collect::<Vec<_>>();
    let mut visible_agents = routes
        .iter()
        .flat_map(|route| [route.from, route.to])
        .chain(std::iter::once(agent_id))
        .collect::<Vec<_>>();
    visible_agents.sort_by_key(|candidate| candidate.0);
    visible_agents.dedup_by_key(|candidate| candidate.0);
    CollaborationAdmissionParticipantView {
        schema_version: decision.schema_version,
        admission_id: decision.admission_id.clone(),
        project_id: decision.input.project_id.clone(),
        work_item_id: decision.input.work_item_id.clone(),
        mode: decision.mode,
        visible_agents: visible_agents.into_iter().collect(),
        routes,
        reasons: decision.reasons.clone(),
        expected_benefit_ref: decision.expected_benefit_ref.clone(),
        state: decision.state,
        transition_sequence: decision.transition_sequence,
        publication_revision: decision.publication_revision,
        decision_digest: decision.decision_digest.clone(),
        deadline_unix_ms: decision.input.budget.deadline_unix_ms,
        updated_at_unix_ms: decision.updated_at_unix_ms,
    }
}

fn governed_project_participant<'a>(
    project: &'a sentinel_workflow::ProjectV1,
    principal: &AuthenticatedCompanyPrincipalV1,
) -> Option<&'a sentinel_workflow::ParticipantBindingV1> {
    project.governance.participants.iter().find(|participant| {
        Some(participant.agent_id) == principal.agent_id
            && participant.principal_id == principal.principal_id
            && participant.role == principal.role
    })
}

fn may_read_full_project(
    project: &sentinel_workflow::ProjectV1,
    principal: &AuthenticatedCompanyPrincipalV1,
) -> bool {
    principal.kind == CompanyPrincipalKindV1::Operator
        || governed_project_participant(project, principal).is_some_and(|participant| {
            matches!(
                participant.role,
                CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
            )
        })
}

fn company_command_response(
    outcome: &sentinel_workflow::CompanyCommandOutcomeV1,
    principal: &AuthenticatedCompanyPrincipalV1,
) -> WorkflowHttpResponse {
    let project = match &outcome.response {
        CompanyWorkflowResponseV1::Project(project) => Some(project.as_ref()),
        CompanyWorkflowResponseV1::AgreementProject { project, .. } => Some(project.as_ref()),
        CompanyWorkflowResponseV1::CustomerRequest(_) | CompanyWorkflowResponseV1::Proposal(_) => {
            None
        }
    };
    if project.is_none_or(|project| may_read_full_project(project, principal)) {
        return json(200, outcome);
    }
    let mut public = match serde_json::to_value(outcome) {
        Ok(value) => value,
        Err(_) => {
            return json_error(
                503,
                "workflow_corrupt",
                "workflow response could not be encoded",
                false,
            )
        }
    };
    let project_value = match &outcome.response {
        CompanyWorkflowResponseV1::Project(_) => public.pointer_mut("/response/value"),
        CompanyWorkflowResponseV1::AgreementProject { .. } => {
            public.pointer_mut("/response/value/project")
        }
        CompanyWorkflowResponseV1::CustomerRequest(_) | CompanyWorkflowResponseV1::Proposal(_) => {
            None
        }
    };
    let Some(serde_json::Value::Object(project)) = project_value else {
        return json_error(
            503,
            "workflow_corrupt",
            "workflow project response is invalid",
            false,
        );
    };
    for field in [
        "collaboration_sessions",
        "handoff_packets",
        "dissent_records",
        "decision_evidence",
        "collaboration_publications",
        "collaboration_generation",
        "collaboration_admissions",
        "collaboration_reliability",
    ] {
        project.remove(field);
    }
    json(200, &public)
}

fn collaboration_admission_response(
    authority_project: &sentinel_workflow::ProjectV1,
    response: &CompanyWorkflowResponseV1,
    principal: &AuthenticatedCompanyPrincipalV1,
    operation_id: Uuid,
    request_digest: &str,
) -> WorkflowHttpResponse {
    let CompanyWorkflowResponseV1::Project(response_project) = response else {
        return json_error(
            503,
            "workflow_corrupt",
            "collaboration operation response is invalid",
            false,
        );
    };
    if response_project.project_id != authority_project.project_id
        || response_project.tenant_id != authority_project.tenant_id
    {
        return json_error(
            503,
            "workflow_corrupt",
            "collaboration operation response is invalid",
            false,
        );
    }
    let Some(decision) = response_project
        .collaboration_admissions
        .iter()
        .find(|decision| {
            decision.request_bindings.iter().any(|binding| {
                binding.operation_id == operation_id && binding.request_digest == request_digest
            })
        })
    else {
        return json_error(
            503,
            "workflow_corrupt",
            "collaboration operation response is missing its request binding",
            false,
        );
    };
    let governed_participant = governed_project_participant(authority_project, principal);
    if governed_participant.is_some_and(|participant| {
        matches!(
            participant.role,
            CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
        )
    }) {
        return json(200, decision);
    }
    let Some(agent_id) = governed_participant.map(|participant| participant.agent_id) else {
        return json_error(
            403,
            "authority_conflict",
            "collaboration admission is not visible to this principal",
            false,
        );
    };
    if !decision.selected_agents.contains(&agent_id) {
        return json_error(
            403,
            "authority_conflict",
            "collaboration admission is not visible to this principal",
            false,
        );
    }
    json(
        200,
        &collaboration_participant_admission_view(decision, agent_id),
    )
}

fn collaboration_mutation_receipt(
    command: &CompanyWorkflowCommandV1,
    outcome: &sentinel_workflow::CompanyCommandOutcomeV1,
) -> Result<CollaborationMutationReceipt, WorkflowError> {
    let CompanyWorkflowResponseV1::Project(project) = &outcome.response else {
        return Err(workflow_unavailable());
    };
    let session_id = match command {
        CompanyWorkflowCommandV1::CreateCollaborationSession { admission_id, .. } => {
            let mut sessions = project
                .collaboration_sessions
                .iter()
                .filter(|session| session.admission_id.as_deref() == Some(admission_id.as_str()));
            let session_id = sessions
                .next()
                .map(|session| session.session_id.clone())
                .ok_or_else(workflow_unavailable)?;
            if sessions.next().is_some() {
                return Err(workflow_unavailable());
            }
            session_id
        }
        CompanyWorkflowCommandV1::RecordIndependentClaim { session_id, .. }
        | CompanyWorkflowCommandV1::OpenClaimExposureBarrier { session_id, .. }
        | CompanyWorkflowCommandV1::OfferHandoffPacket { session_id, .. }
        | CompanyWorkflowCommandV1::RequestHandoffClarification { session_id, .. }
        | CompanyWorkflowCommandV1::AnswerHandoffClarification { session_id, .. }
        | CompanyWorkflowCommandV1::AcceptHandoffPacket { session_id, .. }
        | CompanyWorkflowCommandV1::RejectHandoffPacket { session_id, .. }
        | CompanyWorkflowCommandV1::ConsumeHandoffPacket { session_id, .. }
        | CompanyWorkflowCommandV1::RecordDissent { session_id, .. }
        | CompanyWorkflowCommandV1::LinkDecisionEvidence { session_id, .. }
        | CompanyWorkflowCommandV1::TransitionCollaborationSession { session_id, .. } => {
            session_id.clone()
        }
        _ => return Err(workflow_unavailable()),
    };
    Ok(CollaborationMutationReceipt {
        replayed: outcome.replayed,
        project_id: project.project_id.clone(),
        project_version: project.version,
        session_id,
    })
}

#[derive(Clone)]
struct WorkbenchExecutionAdapter {
    store: Arc<WorkflowStore>,
    authority: Arc<CompanyAuthority>,
}

impl WorkbenchExecutionAdapter {
    fn build_request(
        &self,
        pending: &PendingExecutionV1,
    ) -> Result<WorkbenchRequest, WorkflowPortError> {
        let work = self
            .store
            .execution_context(pending)
            .map_err(map_execution_store_error)?;
        let authority = self.authority.snapshot(
            &work.tenant_id,
            &work.project_id,
            &work.work_item_id,
            work.agent_id,
        )?;
        if !work.plan.authority_matches(&authority) {
            return Err(WorkflowPortError::AuthorityConflict);
        }
        let caller = self
            .authority
            .principals
            .principal(&authority.principal.principal_id)
            .ok_or(WorkflowPortError::AuthorityConflict)?;
        let command_policy = pending
            .step
            .command_policy
            .iter()
            .map(map_command_rule)
            .collect();
        let tool = map_execution_tool(&pending.step.tool, &self.authority.workbench_profile)?;
        let inputs = pending.step.inputs.iter().map(map_artifact_input).collect();
        let mut request = WorkbenchRequest {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: pending.step.invocation_id.to_string(),
            agent_id: work.agent_id,
            project_id: work.project_id.0,
            work_item_id: work.work_item_id.0,
            workspace_id: pending.step.workspace_id.clone(),
            caller_id: authority.principal.principal_id,
            caller_role: collaboration_policy_role_name(caller.principal.role).to_owned(),
            assignment_version: authority.assignment_version,
            credential_generation: authority.principal.principal_generation,
            policy_digest: authority.policy_digest,
            tool_profile: authority.profile_id,
            tool_profile_digest: authority.profile_digest,
            runtime_key: authority.runtime_key,
            capabilities: pending.step.capabilities.clone(),
            output_artifact_kinds: pending
                .step
                .artifacts
                .iter()
                .map(|value| value.artifact_kind.clone())
                .collect(),
            inputs,
            command_policy,
            resource_limits: map_resource_limits(&pending.step.resource_bounds),
            deadline_unix_ms: pending.step.deadline_unix_ms,
            // One workflow step is one stable Workbench invocation. Reconcile
            // attempts are tracked by the workflow outbox and must never
            // perturb the idempotency-bound Workbench request digest.
            attempt: 1,
            tool,
            input_digest: String::new(),
        };
        request.input_digest = request
            .canonical_digest()
            .map_err(|_| WorkflowPortError::Rejected)?;
        request
            .validate_at(now_unix_ms())
            .map_err(|_| WorkflowPortError::Rejected)?;
        Ok(request)
    }

    fn exchange(
        &self,
        command: impl FnOnce(
            mpsc::SyncSender<anyhow::Result<crate::workbench::WorkbenchCoordinatorUpdate>>,
        ) -> WorkbenchDispatchCommand,
    ) -> Result<crate::workbench::WorkbenchCoordinatorUpdate, WorkflowPortError> {
        let (response, receiver) = mpsc::sync_channel(1);
        dispatch_workbench(command(response)).map_err(|_| WorkflowPortError::Unavailable)?;
        map_workbench_dispatch_response(receiver.recv_timeout(DISPATCH_RESPONSE_TIMEOUT))
    }

    fn observation(
        &self,
        update: crate::workbench::WorkbenchCoordinatorUpdate,
    ) -> Result<WorkExecutionObservation, WorkflowPortError> {
        let record = update
            .records
            .last()
            .ok_or(WorkflowPortError::UnknownOutcome)?;
        Ok(match record.state {
            WorkbenchInvocationState::Reserved => WorkExecutionObservation::Reserved,
            WorkbenchInvocationState::Executing => WorkExecutionObservation::Executing,
            WorkbenchInvocationState::Succeeded => WorkExecutionObservation::Succeeded,
            WorkbenchInvocationState::Failed => WorkExecutionObservation::Failed,
            WorkbenchInvocationState::Cancelled => WorkExecutionObservation::Cancelled,
            WorkbenchInvocationState::TimedOut => WorkExecutionObservation::TimedOut,
            WorkbenchInvocationState::UnknownOutcome => WorkExecutionObservation::UnknownOutcome,
        })
    }

    fn terminal_record(
        &self,
        invocation_id: Uuid,
    ) -> Result<WorkbenchInvocationRecord, WorkflowPortError> {
        let authority: Arc<dyn WorkbenchAuthoritySource> = self.authority.clone();
        let update = self.exchange(|response| WorkbenchDispatchCommand::Poll {
            invocation_id: invocation_id.to_string(),
            authority,
            response,
        })?;
        update
            .records
            .into_iter()
            .last()
            .filter(|record| record.state == WorkbenchInvocationState::Succeeded)
            .ok_or(WorkflowPortError::Rejected)
    }
}

fn map_workbench_dispatch_error(error: anyhow::Error) -> WorkflowPortError {
    if error.is::<WorkbenchDispatchUnavailable>() {
        return WorkflowPortError::Unavailable;
    }
    if let Some(runtime) = error.downcast_ref::<sentinel_common::nano_runtime::NanoExecError>() {
        warn!(
            code = ?runtime.code,
            retryable = runtime.retryable,
            "workbench dispatch returned a classified runtime failure"
        );
        if runtime.retryable {
            return WorkflowPortError::Unavailable;
        }
    }
    WorkflowPortError::UnknownOutcome
}

fn map_workbench_dispatch_response<T>(
    response: Result<anyhow::Result<T>, mpsc::RecvTimeoutError>,
) -> Result<T, WorkflowPortError> {
    match response {
        Ok(Ok(update)) => Ok(update),
        Ok(Err(error)) => Err(map_workbench_dispatch_error(error)),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // The durable, digest-bound invocation remains safe to submit or recover again.
            warn!("workbench dispatch response remains pending for its stable invocation");
            Err(WorkflowPortError::Unavailable)
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(WorkflowPortError::Unavailable),
    }
}

impl WorkExecutionPort for WorkbenchExecutionAdapter {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Ready
    }

    fn reconcile(
        &self,
        pending: &PendingExecutionV1,
    ) -> Result<WorkExecutionObservation, WorkflowPortError> {
        let authority: Arc<dyn WorkbenchAuthoritySource> = self.authority.clone();
        let update = match pending.state {
            ExecutionReconcileState::NotFound | ExecutionReconcileState::Reserved => {
                let request = self.build_request(pending)?;
                self.exchange(|response| WorkbenchDispatchCommand::Submit {
                    request: Box::new(request),
                    authority,
                    response,
                })?
            }
            ExecutionReconcileState::Executing => {
                self.exchange(|response| WorkbenchDispatchCommand::Recover {
                    invocation_id: pending.step.invocation_id.to_string(),
                    authority,
                    response,
                })?
            }
            _ => return Err(WorkflowPortError::Rejected),
        };
        self.observation(update)
    }
}

fn map_execution_tool(
    tool: &ExecutionToolV1,
    profile: &WorkbenchProfile,
) -> Result<WorkbenchTool, WorkflowPortError> {
    Ok(match tool {
        ExecutionToolV1::InspectFile { path, max_bytes } => WorkbenchTool::InspectFile {
            path: path.clone(),
            max_bytes: *max_bytes,
        },
        ExecutionToolV1::WriteFile {
            path,
            content,
            expected_sha256,
        } => WorkbenchTool::WriteFile {
            path: path.clone(),
            content: content.clone(),
            expected_sha256: expected_sha256.clone(),
        },
        ExecutionToolV1::ApplyPatch {
            path,
            expected_sha256,
            replacements,
        } => WorkbenchTool::ApplyPatch {
            path: path.clone(),
            expected_sha256: expected_sha256.clone(),
            replacements: replacements
                .iter()
                .map(|value| sentinel_common::TextReplacement {
                    old: value.old.clone(),
                    new: value.new.clone(),
                    expected_occurrences: value.expected_occurrences,
                })
                .collect(),
        },
        ExecutionToolV1::RunCommand { program, args } => WorkbenchTool::RunCommand {
            program: program.clone(),
            args: args.clone(),
        },
        ExecutionToolV1::RunTests { suite_id, args } => {
            let suite = profile
                .test_suites
                .iter()
                .find(|value| value.id == *suite_id)
                .ok_or(WorkflowPortError::Rejected)?;
            WorkbenchTool::RunTests {
                suite_id: suite_id.clone(),
                program: suite.program.clone(),
                args: args.clone(),
            }
        }
        ExecutionToolV1::PackageArtifact {
            artifact_kind,
            media_type,
            paths,
        } => WorkbenchTool::PackageArtifact {
            artifact_kind: artifact_kind.clone(),
            media_type: media_type.clone(),
            paths: paths.clone(),
        },
    })
}

fn map_artifact_input(value: &ArtifactInputV1) -> sentinel_common::WorkbenchInputRef {
    sentinel_common::WorkbenchInputRef {
        artifact_id: value.artifact_id.clone(),
        sha256: value.digest.clone(),
        mount_path: value.mount_path.clone(),
        media_type: value.media_type.clone(),
    }
}

fn map_command_rule(value: &CommandRuleV1) -> CommandRule {
    CommandRule {
        program: value.program.clone(),
        required_arg_prefix: value.required_arg_prefix.clone(),
        max_args: value.max_args,
    }
}

fn map_resource_limits(
    value: &sentinel_workflow::ExecutionResourceBoundsV1,
) -> WorkbenchResourceLimits {
    WorkbenchResourceLimits {
        wall_time_ms: value.wall_time_ms,
        cpu_time_ms: value.cpu_time_ms,
        memory_bytes: value.memory_bytes,
        process_count: value.process_count,
        file_bytes: value.file_bytes,
        stdout_bytes: value.stdout_bytes,
        stderr_bytes: value.stderr_bytes,
    }
}

struct WorkbenchCompletionReceipt {
    receipt_id: String,
    invocation_id: Uuid,
    plan_digest: String,
    step_digest: String,
    output_bundle_digest: String,
    outputs: Vec<SealedOutputEvidenceV1>,
    artifacts: Vec<SealedArtifactEvidenceV1>,
    completed_at_unix_ms: u64,
}

impl TerminalExecutionEvidence for WorkbenchCompletionReceipt {
    fn schema_version(&self) -> u16 {
        WORKFLOW_SCHEMA_VERSION
    }

    fn receipt_id(&self) -> &str {
        &self.receipt_id
    }

    fn invocation_id(&self) -> Uuid {
        self.invocation_id
    }

    fn plan_digest(&self) -> &str {
        &self.plan_digest
    }

    fn step_digest(&self) -> &str {
        &self.step_digest
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

impl CompletionEvidencePort for WorkbenchExecutionAdapter {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Ready
    }

    fn terminal_evidence(
        &self,
        pending: &PendingCompletionEvidenceV1,
    ) -> Result<Box<dyn TerminalExecutionEvidence>, WorkflowPortError> {
        let (work, completed) = self
            .store
            .completion_context(pending)
            .map_err(map_execution_store_error)?;
        if completed {
            return Err(WorkflowPortError::Rejected);
        }
        let record = self.terminal_record(pending.invocation_id)?;
        let step = work
            .plan
            .steps
            .iter()
            .find(|value| value.step_id == pending.step_id)
            .ok_or(WorkflowPortError::Rejected)?;
        if record.request_digest.is_empty()
            || record.invocation_id != pending.invocation_id.to_string()
            || record.project_id != work.project_id.0
            || record.work_item_id != work.work_item_id.0
            || record.agent_id != work.agent_id
            || record.assignment_version != work.plan.assignment_version
            || record.completed_at_ms.is_none()
        {
            return Err(WorkflowPortError::AuthorityConflict);
        }
        let artifacts = map_sealed_artifacts(step, &record.artifacts)?;
        let outputs = map_sealed_outputs(
            step,
            record
                .result_digest
                .as_deref()
                .ok_or(WorkflowPortError::Rejected)?,
        )?;
        let output_bundle_digest =
            sealed_output_bundle_digest(&outputs, &artifacts).map_err(map_execution_store_error)?;
        Ok(Box::new(WorkbenchCompletionReceipt {
            receipt_id: domain_digest(
                "sentinel.workflow.workbench-receipt.v1",
                &[
                    pending.request_digest.as_bytes(),
                    record.request_digest.as_bytes(),
                    output_bundle_digest.as_bytes(),
                ],
            ),
            invocation_id: pending.invocation_id,
            plan_digest: pending.plan_digest.clone(),
            step_digest: pending.step_digest.clone(),
            output_bundle_digest,
            outputs,
            artifacts,
            completed_at_unix_ms: record.completed_at_ms.unwrap_or_default(),
        }))
    }
}

fn map_sealed_artifacts(
    step: &sentinel_workflow::ExecutionStepV1,
    observed: &[WorkbenchArtifactRef],
) -> Result<Vec<SealedArtifactEvidenceV1>, WorkflowPortError> {
    if observed.len() != step.artifacts.len() {
        return Err(WorkflowPortError::Rejected);
    }
    step.artifacts
        .iter()
        .zip(observed)
        .map(|(expected, value)| {
            if value.artifact_kind != expected.artifact_kind
                || value.media_type != expected.media_type
                || value.sha256.len() != 64
            {
                return Err(WorkflowPortError::Rejected);
            }
            Ok(SealedArtifactEvidenceV1 {
                artifact_kind: value.artifact_kind.clone(),
                media_type: value.media_type.clone(),
                paths: expected.required_paths.clone(),
                digest: value.sha256.clone(),
            })
        })
        .collect()
}

fn map_sealed_outputs(
    step: &sentinel_workflow::ExecutionStepV1,
    result_digest: &str,
) -> Result<Vec<SealedOutputEvidenceV1>, WorkflowPortError> {
    if step.outputs.len() != 1
        || result_digest.len() != 64
        || !result_digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(WorkflowPortError::Rejected);
    }
    Ok(step
        .outputs
        .iter()
        .map(|expected| SealedOutputEvidenceV1 {
            name: expected.name.clone(),
            kind: expected.kind.clone(),
            digest_algorithm: expected.digest_algorithm.clone(),
            digest: result_digest.to_ascii_lowercase(),
        })
        .collect())
}

fn map_authority_store_error(error: WorkflowError) -> WorkflowPortError {
    match error.code {
        WorkflowErrorCode::PersistenceFailure | WorkflowErrorCode::CorruptStore => {
            WorkflowPortError::Unavailable
        }
        _ => WorkflowPortError::AuthorityConflict,
    }
}

fn map_execution_store_error(error: WorkflowError) -> WorkflowPortError {
    match error.code {
        WorkflowErrorCode::PersistenceFailure | WorkflowErrorCode::CorruptStore => {
            WorkflowPortError::Unavailable
        }
        WorkflowErrorCode::IdempotencyConflict => WorkflowPortError::UnknownOutcome,
        _ => WorkflowPortError::Rejected,
    }
}

fn domain_digest(domain: &str, parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    format!("{:x}", hasher.finalize())
}

fn hex_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn usd_to_micros(value: f64) -> Option<u64> {
    if !value.is_finite() || value < 0.0 || value > (u64::MAX as f64 / 1_000_000.0) {
        return None;
    }
    Some((value * 1_000_000.0).round() as u64)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderUsageBinding {
    tenant_id: String,
    project_id: String,
    work_item_id: String,
    reservation_id: String,
    assignment_id: String,
    assignment_version: u64,
    agent_id: AgentId,
    provider: String,
}

fn select_provider_usage_binding(
    projects: &[sentinel_workflow::ProjectV1],
    agent_id: AgentId,
) -> Result<Option<ProviderUsageBinding>, &'static str> {
    let mut selected = None;
    for project in projects {
        if project.lifecycle_state != sentinel_workflow::ProjectLifecycleStateV1::Active {
            continue;
        }
        for (work_item_id, work_item) in &project.work_items {
            if !matches!(
                work_item.state,
                sentinel_workflow::CompanyWorkStateV1::Assigned
                    | sentinel_workflow::CompanyWorkStateV1::InProgress
                    | sentinel_workflow::CompanyWorkStateV1::InReview
            ) {
                continue;
            }
            let mut active_assignments = work_item
                .assignments
                .iter()
                .filter(|assignment| assignment.active);
            let Some(assignment) = active_assignments.next() else {
                return Err("provider work item has no active assignment");
            };
            if active_assignments.next().is_some() {
                return Err("provider work item has ambiguous active assignments");
            }
            if assignment.agent_id != agent_id {
                continue;
            }
            let mut active_reservations = project.reservations.iter().filter(|reservation| {
                reservation.state == sentinel_workflow::CostReservationStateV1::Active
                    && reservation.work_item_id.as_ref() == Some(work_item_id)
            });
            let Some(reservation) = active_reservations.next() else {
                return Err("assigned provider work item has no active reservation");
            };
            if active_reservations.next().is_some() {
                return Err("assigned provider work item has ambiguous active reservations");
            }
            let binding = ProviderUsageBinding {
                tenant_id: project.tenant_id.0.clone(),
                project_id: project.project_id.0.clone(),
                work_item_id: work_item_id.0.clone(),
                reservation_id: reservation.reservation_id.clone(),
                assignment_id: assignment.assignment_id.clone(),
                assignment_version: assignment.assignment_version,
                agent_id,
                provider: reservation.provider.clone(),
            };
            if selected.replace(binding).is_some() {
                return Err("agent has ambiguous provider usage authority");
            }
        }
    }
    Ok(selected)
}

fn validate_provider_usage_event(
    event: &DomainEvent,
    usage_operation_id: &str,
    expected: &ProviderUsageBinding,
    expected_cost_micros: u64,
) -> Result<(), &'static str> {
    if event.event_type != "agent_llm_usage"
        || event.schema_version < 3
        || usage_operation_id != format!("llm_usage_{}", event.correlation_id)
    {
        return Err("provider usage event identity is invalid");
    }
    let payload: DomainEventPayload =
        serde_json::from_str(&event.payload).map_err(|_| "provider usage payload is invalid")?;
    let DomainEventPayload::AgentLlmUsage {
        agent_id,
        tenant_id,
        project_id,
        work_item_id,
        reservation_id,
        assignment_id,
        assignment_version,
        provider,
        requested_model,
        caller_role,
        hierarchy_tier,
        cost_source,
        effective_model,
        cost_usd,
        ..
    } = payload
    else {
        return Err("provider usage payload type is invalid");
    };
    if agent_id != expected.agent_id
        || tenant_id.as_deref() != Some(expected.tenant_id.as_str())
        || project_id.as_deref() != Some(expected.project_id.as_str())
        || work_item_id.as_deref() != Some(expected.work_item_id.as_str())
        || reservation_id.as_deref() != Some(expected.reservation_id.as_str())
        || assignment_id.as_deref() != Some(expected.assignment_id.as_str())
        || assignment_version != Some(expected.assignment_version)
        || provider.as_deref() != Some(expected.provider.as_str())
        || requested_model
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        || caller_role.as_deref() != Some("agent_runtime")
        || hierarchy_tier.is_none()
        || cost_source.is_none()
        || effective_model
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        || event.aggregate_id != format!("AGENT-{:02}", agent_id.0)
        || usd_to_micros(cost_usd) != Some(expected_cost_micros)
    {
        return Err("provider usage does not match the project reservation");
    }
    Ok(())
}

fn workflow_unavailable() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::PersistenceFailure,
        true,
        "company workflow authority is unavailable",
    )
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompanyCommandEnvelope {
    operation_id: Uuid,
    command: CompanyWorkflowCommandV1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CollaborationAdmissionEnvelope {
    operation_id: Uuid,
    command: CollaborationAdmissionRequest,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
struct CollaborationAdmissionParticipantView {
    schema_version: u16,
    admission_id: String,
    project_id: ProjectId,
    work_item_id: WorkItemId,
    mode: sentinel_workflow::CollaborationAdmissionModeV1,
    visible_agents: Vec<AgentId>,
    routes: Vec<sentinel_workflow::CollaborationRouteV1>,
    reasons: Vec<String>,
    expected_benefit_ref: String,
    state: sentinel_workflow::CollaborationAdmissionStateV1,
    transition_sequence: u64,
    publication_revision: u64,
    decision_digest: String,
    deadline_unix_ms: u64,
    updated_at_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
struct CollaborationMutationReceipt {
    replayed: bool,
    project_id: ProjectId,
    project_version: u64,
    session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum CollaborationAdmissionRequest {
    Admit {
        project_id: ProjectId,
        work_item_id: WorkItemId,
        expected_version: u64,
        expected_benefit_ref: String,
    },
    Progress {
        project_id: ProjectId,
        expected_version: u64,
        admission_id: String,
        progress: CollaborationProgressRequestV1,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CollaborationProgressRequestV1 {
    expected_transition_sequence: u64,
    novelty_micros: u32,
    novelty_digest: String,
    milestone_digest: Option<String>,
    work_digest: Option<String>,
    disposition: CollaborationProgressDispositionV1,
    reason_ref: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionPlanEnvelope {
    operation_id: Uuid,
    plan: ExecutionPlanV1,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionIntentEnvelope {
    operation_id: Uuid,
    intent: ExecutionIntentV1,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionIntentV1 {
    project_id: ProjectId,
    work_item_id: WorkItemId,
    tools: Vec<ExecutionToolV1>,
}

struct ExecutionIntentAdmission {
    plan: ExecutionPlanV1,
    authority: RuntimeAuthoritySnapshotV1,
    replay: bool,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct ExecutionAdmissionResponse {
    replayed: bool,
    work_item: sentinel_workflow::WorkItemExecutionV1,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeliveryCommandEnvelope {
    idempotency_key: String,
    command: ProductDeliveryCommand,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum ProductDeliveryCommand {
    RegisterCandidate {
        candidate: crate::delivery::ReleaseCandidateV1,
    },
    AssignQa {
        tenant_id: String,
        project_id: String,
        candidate_id: String,
        plan: Box<crate::delivery::QaEvaluationPlanV1>,
        run: crate::delivery::QaEvaluationRunReceiptV1,
    },
    TransitionQa {
        tenant_id: String,
        project_id: String,
        run_id: String,
        next: crate::delivery::QaRunState,
    },
    ExecuteQa {
        tenant_id: String,
        project_id: String,
        run_id: String,
    },
    ImportEvidenceGraph {
        tenant_id: String,
        project_id: String,
        run_id: String,
        graph: crate::delivery::QaEvidenceGraphV1,
    },
    RecordGate {
        tenant_id: String,
        project_id: String,
        run_id: String,
        gate: crate::delivery::QaReleaseGateReceiptV1,
    },
    RecordReviewBundle {
        tenant_id: String,
        project_id: String,
        run_id: String,
        review: crate::delivery::ReviewV1,
        test_run: crate::delivery::TestRunV1,
        findings: Vec<crate::delivery::FindingV1>,
        approval: Option<crate::delivery::ApprovalV1>,
    },
    Promote {
        tenant_id: String,
        project_id: String,
        candidate_id: String,
        manifest: crate::delivery::ReleaseManifestV1,
        release: crate::delivery::ReleaseV1,
    },
    IssueDelivery {
        project_id: String,
        receipt: crate::delivery::DeliveryReceiptV1,
    },
    CustomerAction {
        tenant_id: String,
        project_id: String,
        feedback: crate::delivery::CustomerFeedbackV1,
        acceptance: Option<crate::delivery::AcceptanceV1>,
    },
    Rollback {
        tenant_id: String,
        project_id: String,
        rollback: crate::delivery::RollbackV1,
    },
    Closeout {
        tenant_id: String,
        project_id: String,
        closeout: crate::delivery::ProjectCloseoutV1,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowHealthSnapshot {
    pub enabled: bool,
    pub status: String,
    pub ready: bool,
    pub dependencies_ready: bool,
    pub canonical_event_cursor: Option<u64>,
    pub pending_execution: usize,
    pub pending_completion: usize,
    pub pending_gate: usize,
    pub delivery_ready: bool,
    pub delivery_publication_pending: usize,
    pub collaboration_publication_pending: usize,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowHttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

pub struct WorkflowApi {
    core: Arc<ProductWorkflowCore>,
    delivery: Option<Arc<ProductDeliveryCore>>,
    store: Arc<WorkflowStore>,
    principals: Arc<PrincipalAuthenticator>,
    authority: Option<Arc<CompanyAuthority>>,
    event_store: Option<sentinel_limbo::EventStore>,
    mutation_fence: RwLock<()>,
    enabled: bool,
    scan_succeeded: AtomicBool,
    collaboration_publication_pending: AtomicUsize,
    last_error: Mutex<Option<String>>,
    company_sync_cursor: Mutex<Option<(TenantId, ProjectId, WorkItemId)>>,
}

impl std::fmt::Debug for WorkflowApi {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkflowApi")
            .finish_non_exhaustive()
    }
}

impl WorkflowApi {
    #[cfg(test)]
    pub(crate) fn disabled() -> Result<Self, WorkflowError> {
        Self::new_disabled(Arc::new(WorkflowStore::open(":memory:")?))
    }

    pub fn open(
        data_dir: &Path,
        config_dir: &Path,
        agent_capabilities: HashMap<AgentId, BTreeSet<String>>,
        runtime_health: crate::runtime_health::SharedRuntimeHealthState,
        event_store: sentinel_limbo::EventStore,
        workbench_artifact_roots: HashMap<AgentId, PathBuf>,
    ) -> Result<Self, WorkflowError> {
        let enabled = workflow_enabled()?;
        let store_path = if enabled {
            data_dir.join("company-workflow.sqlite")
        } else {
            PathBuf::from(":memory:")
        };
        let store = Arc::new(WorkflowStore::open(store_path)?);
        if !enabled {
            return Self::new_disabled(store);
        }
        let credentials_dir = std::env::var_os("CREDENTIALS_DIRECTORY")
            .map(PathBuf::from)
            .ok_or_else(workflow_unavailable)?;
        let principals = Arc::new(PrincipalAuthenticator::load(
            &config_dir.join("company-principals.json"),
            &credentials_dir,
        )?);
        let (profile, profile_digest) =
            WorkbenchProfile::load(config_dir.join("workbench-profiles/web-authoring-v1.toml"))
                .map_err(|_| workflow_unavailable())?;
        let (qa_profile, qa_profile_digest) =
            WorkbenchProfile::load(config_dir.join("workbench-profiles/web-qa-v1.toml"))
                .map_err(|_| workflow_unavailable())?;
        let agent_capabilities = Arc::new(agent_capabilities);
        let authority = Arc::new(CompanyAuthority {
            store: Arc::clone(&store),
            principals: Arc::clone(&principals),
            workbench_profile: profile,
            workbench_profile_digest: profile_digest,
            qa_profile_capabilities: qa_profile.capabilities.clone(),
            agent_capabilities: Arc::clone(&agent_capabilities),
            runtime_health,
            artifact_roots: Arc::new(workbench_artifact_roots.clone()),
        });
        let workbench = Arc::new(WorkbenchExecutionAdapter {
            store: Arc::clone(&store),
            authority: Arc::clone(&authority),
        });
        let organization: Arc<dyn sentinel_workflow::OrganizationRuntimePort> = authority.clone();
        let execution: Arc<dyn WorkExecutionPort> = workbench.clone();
        let completion: Arc<dyn CompletionEvidencePort> = workbench;
        let delivery_integration = WorkflowDeliveryIntegration::new(
            Arc::clone(&store),
            Arc::clone(&principals),
            qa_profile,
            qa_profile_digest,
            agent_capabilities,
            Arc::new(workbench_artifact_roots),
        );
        let gate: Arc<dyn GateEvidencePort> =
            Arc::new(WorkflowWorkItemGate::new(delivery_integration.clone()));
        let delivery_config =
            DeliveryStoreConfigV1::new(data_dir.join("company-delivery"), "company-delivery.redb")
                .map_err(|_| workflow_unavailable())?;
        let delivery = ConfiguredDeliveryCore::open(
            &delivery_config,
            delivery_integration,
            LimboDeliveryEffects::new(
                event_store.clone(),
                Arc::clone(&store),
                Arc::clone(&principals),
            ),
            LimboDeliveryPublication::new(event_store.clone()),
        )
        .map_err(|_| workflow_unavailable())?;
        Ok(Self {
            core: Arc::new(WorkflowCore::new(
                Arc::clone(&store),
                organization,
                execution,
                completion,
                gate,
            )),
            delivery: Some(Arc::new(delivery)),
            store,
            principals,
            authority: Some(authority),
            event_store: Some(event_store),
            mutation_fence: RwLock::new(()),
            enabled: true,
            scan_succeeded: AtomicBool::new(false),
            collaboration_publication_pending: AtomicUsize::new(usize::MAX),
            last_error: Mutex::new(None),
            company_sync_cursor: Mutex::new(None),
        })
    }

    fn new_disabled(store: Arc<WorkflowStore>) -> Result<Self, WorkflowError> {
        let organization: Arc<dyn sentinel_workflow::OrganizationRuntimePort> =
            Arc::new(sentinel_workflow::UnavailableOrganizationRuntimePort);
        let execution: Arc<dyn WorkExecutionPort> =
            Arc::new(sentinel_workflow::UnavailableWorkExecutionPort);
        let completion: Arc<dyn CompletionEvidencePort> =
            Arc::new(sentinel_workflow::UnavailableCompletionEvidencePort);
        let gate: Arc<dyn GateEvidencePort> = Arc::new(UnavailableGateEvidencePort);
        Ok(Self {
            core: Arc::new(WorkflowCore::new(
                Arc::clone(&store),
                organization,
                execution,
                completion,
                gate,
            )),
            delivery: None,
            store,
            principals: Arc::new(PrincipalAuthenticator::default()),
            authority: None,
            event_store: None,
            mutation_fence: RwLock::new(()),
            enabled: false,
            scan_succeeded: AtomicBool::new(false),
            collaboration_publication_pending: AtomicUsize::new(0),
            last_error: Mutex::new(None),
            company_sync_cursor: Mutex::new(None),
        })
    }

    pub fn handle(
        &self,
        method: &str,
        path: &str,
        headers: &HashMap<String, String>,
        body: &[u8],
    ) -> Option<WorkflowHttpResponse> {
        let path_only = path.split('?').next().unwrap_or(path);
        if !is_workflow_path(path_only) {
            return None;
        }
        if !self.enabled {
            return Some(json_error(
                503,
                "workflow_unavailable",
                "company workflow is disabled or incomplete",
                true,
            ));
        }
        let principal = match self.principals.authenticate(headers) {
            Some(value) => value,
            None => {
                return Some(json_error(
                    401,
                    "authentication_failed",
                    "workflow authentication failed",
                    false,
                ))
            }
        };
        Some(match (method, path_only) {
            ("POST", CUSTOMER_COMMAND_PATH) => {
                self.company_command(&principal, CompanyPrincipalKindV1::Customer, body)
            }
            ("POST", OPERATOR_COMMAND_PATH) => {
                self.company_command(&principal, CompanyPrincipalKindV1::Operator, body)
            }
            ("POST", AGENT_COMMAND_PATH) => self.agent_command(&principal, body),
            ("GET", CUSTOMER_REQUEST_PATH) => self.customer_request(&principal, path),
            ("GET", OPERATOR_PROJECT_PATH) => self.project(&principal, path),
            ("GET", OPERATOR_WORK_ITEM_PATH) => self.work_item(&principal, path),
            ("GET", OPERATOR_PROJECTION_PATH) => self.projection(&principal, path),
            ("GET", OPERATOR_EVENTS_PATH) => self.events(&principal, path),
            ("POST", DELIVERY_COMMAND_PATH) => self.delivery_command(&principal, body),
            ("POST", DELIVERY_INTENT_PATH) => delivery_intent::handle(self, &principal, body),
            ("GET", DELIVERY_LINEAGE_PATH) => self.delivery_lineage(&principal, path),
            ("POST", COLLABORATION_ADMISSION_PATH) => {
                self.collaboration_admission(&principal, body)
            }
            ("GET", COLLABORATION_VIEW_PATH) => self.collaboration_view(&principal, path),
            ("GET", COLLABORATION_ADMISSION_PATH) => {
                self.collaboration_admission_view(&principal, path)
            }
            ("GET", WORKFLOW_READINESS_PATH) => json(200, &self.health()),
            _ => json_error(
                405,
                "method_not_allowed",
                "workflow method is not allowed",
                false,
            ),
        })
    }

    fn company_command(
        &self,
        principal: &BoundPrincipal,
        expected_kind: CompanyPrincipalKindV1,
        body: &[u8],
    ) -> WorkflowHttpResponse {
        if principal.principal.kind != expected_kind {
            return json_error(
                403,
                "authority_conflict",
                "workflow role is not allowed",
                false,
            );
        }
        let envelope: CompanyCommandEnvelope = match decode_body(body) {
            Ok(value) => value,
            Err(response) => return response,
        };
        if is_internal_company_command(&envelope.command) {
            return json_error(
                403,
                "evidence_required",
                "command is derived from sealed execution or delivery evidence",
                false,
            );
        }
        let Ok(_guard) = self.mutation_fence.read() else {
            return json_error(503, "workflow_busy", "workflow recovery is active", true);
        };
        let operation_exists = match self
            .store
            .has_company_operation(&principal.principal, envelope.operation_id)
        {
            Ok(value) => value,
            Err(error) => return workflow_error(error),
        };
        if !operation_exists {
            if let Err(response) =
                self.validate_provider_usage_binding(&principal.principal, &envelope.command)
            {
                return response;
            }
        }
        match self.core.apply_company_command(
            &principal.principal,
            envelope.operation_id,
            &envelope.command,
            now_unix_ms(),
        ) {
            Ok(value) => {
                if is_collaboration_command_v1(&envelope.command) {
                    if let Err(error) = self.publish_collaboration_backlog() {
                        if let Ok(mut last_error) = self.last_error.lock() {
                            *last_error = Some(error);
                        }
                        return json_error(
                            503,
                            "collaboration_publication_pending",
                            "collaboration state is durable but canonical publication is pending",
                            true,
                        );
                    }
                }
                if is_collaboration_command_v1(&envelope.command)
                    && principal.principal.kind == CompanyPrincipalKindV1::Agent
                {
                    match collaboration_mutation_receipt(&envelope.command, &value) {
                        Ok(receipt) => json(200, &receipt),
                        Err(error) => workflow_error(error),
                    }
                } else {
                    company_command_response(&value, &principal.principal)
                }
            }
            Err(error) => workflow_error(error),
        }
    }

    fn collaboration_view(&self, principal: &BoundPrincipal, path: &str) -> WorkflowHttpResponse {
        if principal.principal.kind == CompanyPrincipalKindV1::Customer {
            return json_error(
                403,
                "authority_conflict",
                "operator or participating agent authority is required",
                false,
            );
        }
        let (Some(project_value), Some(session_id)) = (
            query_parameter(path, "project_id"),
            query_parameter(path, "session_id"),
        ) else {
            return json_error(
                400,
                "invalid_input",
                "project_id and session_id are required",
                false,
            );
        };
        let project_id = match ProjectId::parse(project_value) {
            Ok(value) => value,
            Err(error) => return workflow_error(error),
        };
        match self
            .store
            .company_project(&principal.principal.tenant_id, &project_id)
        {
            Ok(Some(project)) => {
                match filtered_collaboration_view(&project, &principal.principal, session_id) {
                    Ok(value) => json(200, &value),
                    Err(error) => workflow_error(error),
                }
            }
            Ok(None) => json_error(404, "not_found", "workflow object was not found", false),
            Err(error) => workflow_error(error),
        }
    }

    fn collaboration_admission(
        &self,
        principal: &BoundPrincipal,
        body: &[u8],
    ) -> WorkflowHttpResponse {
        if principal.principal.kind != CompanyPrincipalKindV1::Agent {
            return json_error(
                403,
                "authority_conflict",
                "agent authority is required",
                false,
            );
        }
        let envelope: CollaborationAdmissionEnvelope = match decode_body(body) {
            Ok(value) => value,
            Err(response) => return response,
        };
        let Ok(request_bytes) = serde_json::to_vec(&envelope.command) else {
            return json_error(
                400,
                "invalid_input",
                "collaboration request is invalid",
                false,
            );
        };
        let request_digest = domain_digest(
            "sentinel.workflow.collaboration-admission-request.v1",
            &[&request_bytes],
        );
        let (project_id, expected_version, requested_admission_id) = match &envelope.command {
            CollaborationAdmissionRequest::Admit {
                project_id,
                expected_version,
                ..
            } => (project_id, *expected_version, None),
            CollaborationAdmissionRequest::Progress {
                project_id,
                expected_version,
                admission_id,
                ..
            } => (project_id, *expected_version, Some(admission_id.as_str())),
        };
        let Ok(_guard) = self.mutation_fence.read() else {
            return json_error(503, "workflow_busy", "workflow recovery is active", true);
        };
        match self
            .store
            .has_company_operation(&principal.principal, envelope.operation_id)
        {
            Ok(true) => {
                return self.replay_collaboration_admission(
                    principal,
                    project_id,
                    requested_admission_id,
                    envelope.operation_id,
                    &request_digest,
                )
            }
            Ok(false) => {}
            Err(error) => return workflow_error(error),
        }
        let Some(authority) = self.authority.as_ref() else {
            return json_error(
                503,
                "workflow_unavailable",
                "workflow authority is unavailable",
                true,
            );
        };
        let project = match self
            .store
            .company_project(&principal.principal.tenant_id, project_id)
        {
            Ok(Some(project)) => project,
            Ok(None) => {
                return json_error(404, "not_found", "workflow object was not found", false)
            }
            Err(error) => return workflow_error(error),
        };
        if project.version != expected_version {
            return json_error(
                409,
                "version_conflict",
                "collaboration project version is stale",
                false,
            );
        }
        let command = match derive_collaboration_admission_command(
            authority,
            &project,
            envelope.command,
            request_digest.clone(),
        ) {
            Ok(command) => command,
            Err(error) => return workflow_error(error),
        };
        match self.core.apply_company_command(
            &principal.principal,
            envelope.operation_id,
            &command,
            now_unix_ms(),
        ) {
            Ok(value) => {
                if let Err(error) = self.publish_collaboration_backlog() {
                    if let Ok(mut last_error) = self.last_error.lock() {
                        *last_error = Some(error);
                    }
                    return json_error(
                        503,
                        "collaboration_publication_pending",
                        "collaboration state is durable but canonical publication is pending",
                        true,
                    );
                }
                collaboration_admission_response(
                    &project,
                    &value.response,
                    &principal.principal,
                    envelope.operation_id,
                    &request_digest,
                )
            }
            Err(error) => workflow_error(error),
        }
    }

    fn replay_collaboration_admission(
        &self,
        principal: &BoundPrincipal,
        project_id: &ProjectId,
        requested_admission_id: Option<&str>,
        operation_id: Uuid,
        request_digest: &str,
    ) -> WorkflowHttpResponse {
        let project = match self
            .store
            .company_project(&principal.principal.tenant_id, project_id)
        {
            Ok(Some(project)) => project,
            Ok(None) => {
                return json_error(404, "not_found", "workflow object was not found", false)
            }
            Err(error) => return workflow_error(error),
        };
        let matched = project.collaboration_admissions.iter().any(|decision| {
            requested_admission_id.is_none_or(|value| value == decision.admission_id)
                && decision.request_bindings.iter().any(|binding| {
                    binding.operation_id == operation_id && binding.request_digest == request_digest
                })
        });
        if !matched {
            return json_error(
                409,
                "idempotency_conflict",
                "operation id is bound to another collaboration request",
                false,
            );
        }
        if let Err(error) = self.publish_collaboration_backlog() {
            if let Ok(mut last_error) = self.last_error.lock() {
                *last_error = Some(error);
            }
            return json_error(
                503,
                "collaboration_publication_pending",
                "collaboration state is durable but canonical publication is pending",
                true,
            );
        }
        match self
            .store
            .company_operation_response(&principal.principal, operation_id)
        {
            Ok(Some(response)) => collaboration_admission_response(
                &project,
                &response,
                &principal.principal,
                operation_id,
                request_digest,
            ),
            Ok(None) => json_error(
                503,
                "workflow_corrupt",
                "collaboration operation response is missing",
                false,
            ),
            Err(error) => workflow_error(error),
        }
    }

    fn collaboration_admission_view(
        &self,
        principal: &BoundPrincipal,
        path: &str,
    ) -> WorkflowHttpResponse {
        if principal.principal.kind == CompanyPrincipalKindV1::Customer {
            return json_error(
                403,
                "authority_conflict",
                "operator or admitted participant authority is required",
                false,
            );
        }
        let (Some(project_value), Some(admission_id)) = (
            query_parameter(path, "project_id"),
            query_parameter(path, "admission_id"),
        ) else {
            return json_error(
                400,
                "invalid_input",
                "project_id and admission_id are required",
                false,
            );
        };
        let project_id = match ProjectId::parse(project_value) {
            Ok(value) => value,
            Err(error) => return workflow_error(error),
        };
        let project = match self
            .store
            .company_project(&principal.principal.tenant_id, &project_id)
        {
            Ok(Some(project)) => project,
            Ok(None) => {
                return json_error(404, "not_found", "workflow object was not found", false)
            }
            Err(error) => return workflow_error(error),
        };
        let Some(decision) = project
            .collaboration_admissions
            .iter()
            .find(|decision| decision.admission_id == admission_id)
        else {
            return json_error(404, "not_found", "workflow object was not found", false);
        };
        if principal.principal.kind == CompanyPrincipalKindV1::Operator {
            return json(200, decision);
        }
        let governed_participant = governed_project_participant(&project, &principal.principal);
        if governed_participant.is_some_and(|participant| {
            matches!(
                participant.role,
                CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
            )
        }) {
            return json(200, decision);
        }
        let Some(agent_id) = governed_participant.map(|participant| participant.agent_id) else {
            return json_error(
                403,
                "authority_conflict",
                "collaboration admission is not visible to this principal",
                false,
            );
        };
        if !decision.selected_agents.contains(&agent_id) {
            return json_error(
                403,
                "authority_conflict",
                "collaboration admission is not visible to this principal",
                false,
            );
        }
        json(
            200,
            &collaboration_participant_admission_view(decision, agent_id),
        )
    }

    fn publish_collaboration_backlog(&self) -> Result<(), String> {
        let event_store = self
            .event_store
            .as_ref()
            .ok_or_else(|| "collaboration event store is unavailable".to_owned())?;
        let registry = collaboration_event_schema_registry()
            .map_err(|error| format!("collaboration schema registry is invalid: {error}"))?;
        let publications = self
            .store
            .collaboration_publications()
            .map_err(|error| format!("collaboration publication ledger is unavailable: {error}"))?;
        publish_collaboration_publications(
            event_store,
            &registry,
            &publications,
            &self.collaboration_publication_pending,
        )
        .map(|_| ())
    }
}

fn publish_collaboration_publications(
    event_store: &sentinel_limbo::EventStore,
    registry: &sentinel_common::EventSchemaRegistry,
    publications: &[CollaborationPublicationV1],
    pending: &AtomicUsize,
) -> Result<Vec<sentinel_common::AppendDispositionV2>, String> {
    pending.store(publications.len(), Ordering::Release);
    let mut dispositions = Vec::with_capacity(publications.len());
    for (index, publication) in publications.iter().enumerate() {
        let authority_scope_digest = publication
            .proposal
            .causal_context
            .authority_scope_digest()
            .map_err(|error| format!("collaboration authority is invalid: {error}"))?;
        let caller = sentinel_limbo::AuthenticatedEventCallerV1 {
            service_id: "sentinel-daemon-workflow".to_owned(),
            producer: publication.proposal.producer.clone(),
            authority_scope_digest,
        };
        let outcome = event_store
            .append_gateway(registry)
            .append(&caller, &publication.proposal)
            .map_err(|error| format!("collaboration publication failed: {error}"))?;
        dispositions.push(outcome.disposition);
        pending.store(publications.len() - index - 1, Ordering::Release);
    }
    Ok(dispositions)
}

impl WorkflowApi {
    fn validate_provider_usage_binding(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        command: &CompanyWorkflowCommandV1,
    ) -> Result<(), WorkflowHttpResponse> {
        let CompanyWorkflowCommandV1::CommitCost {
            project_id,
            expected_version,
            reservation_id,
            actual_micros,
            usage_event_operation_id,
            ..
        } = command
        else {
            return Ok(());
        };
        let Some(usage_operation_id) = usage_event_operation_id.as_deref() else {
            return Err(json_error(
                409,
                "evidence_required",
                "provider cost requires a durable usage event",
                false,
            ));
        };
        if !usage_operation_id.starts_with("llm_usage_") {
            return Err(json_error(
                409,
                "evidence_conflict",
                "provider usage operation is invalid",
                false,
            ));
        }
        let event_store = self.event_store.as_ref().ok_or_else(|| {
            json_error(
                503,
                "workflow_unavailable",
                "provider usage authority is unavailable",
                true,
            )
        })?;
        let event = event_store
            .event_by_operation_id(usage_operation_id)
            .map_err(|_| {
                json_error(
                    503,
                    "workflow_unavailable",
                    "provider usage authority could not be read",
                    true,
                )
            })?
            .ok_or_else(|| {
                json_error(
                    409,
                    "evidence_required",
                    "provider usage event does not exist",
                    false,
                )
            })?;
        let project = self
            .store
            .company_project(&principal.tenant_id, project_id)
            .map_err(workflow_error)?
            .ok_or_else(|| json_error(404, "not_found", "workflow object was not found", false))?;
        if project.version != *expected_version {
            return Err(json_error(
                409,
                "evidence_conflict",
                "provider usage was validated against a stale project version",
                false,
            ));
        }
        let reservation = project
            .reservations
            .iter()
            .find(|value| value.reservation_id == *reservation_id)
            .ok_or_else(|| json_error(404, "not_found", "workflow object was not found", false))?;
        let Some(work_item_id) = reservation.work_item_id.as_ref() else {
            return Err(json_error(
                409,
                "evidence_conflict",
                "provider usage must be bound to a work item",
                false,
            ));
        };
        let work_item = project.work_items.get(work_item_id).ok_or_else(|| {
            json_error(
                409,
                "evidence_conflict",
                "work item no longer exists",
                false,
            )
        })?;
        let mut active_assignments = work_item
            .assignments
            .iter()
            .filter(|assignment| assignment.active);
        let assignment = active_assignments.next().ok_or_else(|| {
            json_error(
                409,
                "evidence_conflict",
                "provider work item has no active assignment",
                false,
            )
        })?;
        if active_assignments.next().is_some() {
            return Err(json_error(
                409,
                "evidence_conflict",
                "provider work item has ambiguous active assignments",
                false,
            ));
        }
        let expected = ProviderUsageBinding {
            tenant_id: principal.tenant_id.0.clone(),
            project_id: project_id.0.clone(),
            work_item_id: work_item_id.0.clone(),
            reservation_id: reservation_id.clone(),
            assignment_id: assignment.assignment_id.clone(),
            assignment_version: assignment.assignment_version,
            agent_id: assignment.agent_id,
            provider: reservation.provider.clone(),
        };
        validate_provider_usage_event(&event, usage_operation_id, &expected, *actual_micros)
            .map_err(|message| json_error(409, "evidence_conflict", message, false))?;
        if project.reservations.iter().any(|value| {
            value.reservation_id != *reservation_id
                && value.usage_event_operation_id.as_deref() == Some(usage_operation_id)
        }) {
            return Err(json_error(
                409,
                "evidence_conflict",
                "provider usage does not match the project reservation",
                false,
            ));
        }
        Ok(())
    }

    fn provider_usage_binding_for_agent(
        &self,
        agent_id: AgentId,
    ) -> Result<Option<ProviderUsageBinding>, &'static str> {
        if !self.enabled {
            return Ok(None);
        }
        let projects = self
            .store
            .company_projects()
            .map_err(|_| "company provider authority could not be read")?;
        select_provider_usage_binding(&projects, agent_id)
    }

    fn agent_command(&self, principal: &BoundPrincipal, body: &[u8]) -> WorkflowHttpResponse {
        if principal.principal.kind != CompanyPrincipalKindV1::Agent {
            return json_error(
                403,
                "authority_conflict",
                "agent authority is required",
                false,
            );
        }
        if let Ok(envelope) = serde_json::from_slice::<ExecutionPlanEnvelope>(body) {
            if envelope.plan.principal != principal.execution_authority
                || envelope.plan.agent_id != principal.principal.agent_id.unwrap_or(AgentId(0))
                || envelope.operation_id != envelope.plan.plan_id
            {
                return json_error(
                    403,
                    "authority_conflict",
                    "execution plan authority is stale",
                    false,
                );
            }
            let Ok(_guard) = self.mutation_fence.read() else {
                return json_error(503, "workflow_busy", "workflow recovery is active", true);
            };
            let Some(authority) = self.authority.as_ref() else {
                return json_error(
                    503,
                    "workflow_unavailable",
                    "workflow authority is unavailable",
                    true,
                );
            };
            if authority.validate_plan_contract(&envelope.plan).is_err() {
                return json_error(
                    403,
                    "authority_conflict",
                    "execution plan contract is stale",
                    false,
                );
            }
            let result = self.core.admit_plan(&envelope.plan, now_unix_ms());
            return match result {
                Ok((replayed, work_item)) => json(200, &(replayed, work_item)),
                Err(error) => workflow_error(error),
            };
        }
        if let Ok(envelope) = serde_json::from_slice::<ExecutionIntentEnvelope>(body) {
            let Ok(_guard) = self.mutation_fence.read() else {
                return json_error(503, "workflow_busy", "workflow recovery is active", true);
            };
            let Some(authority) = self.authority.as_ref() else {
                return json_error(
                    503,
                    "workflow_unavailable",
                    "workflow authority is unavailable",
                    true,
                );
            };
            let admission = match authority.plan_from_intent(
                principal,
                envelope.operation_id,
                &envelope.intent,
                now_unix_ms(),
            ) {
                Ok(admission) => admission,
                Err(error) => return workflow_error(error),
            };
            if authority.validate_plan_contract(&admission.plan).is_err() {
                return json_error(
                    403,
                    "authority_conflict",
                    "derived execution plan contract is stale",
                    false,
                );
            }
            let result = if admission.replay {
                self.store
                    .admit_plan(&admission.plan, &admission.authority, now_unix_ms())
            } else {
                self.core.admit_plan(&admission.plan, now_unix_ms())
            };
            return match result {
                Ok((replayed, work_item)) => json(
                    200,
                    &ExecutionAdmissionResponse {
                        replayed,
                        work_item,
                    },
                ),
                Err(error) => workflow_error(error),
            };
        }
        self.company_command(principal, CompanyPrincipalKindV1::Agent, body)
    }

    fn customer_request(&self, principal: &BoundPrincipal, path: &str) -> WorkflowHttpResponse {
        let Some(request_id) = query_parameter(path, "request_id") else {
            return json_error(400, "invalid_input", "request_id is required", false);
        };
        match self
            .store
            .company_customer_request(&principal.principal.tenant_id, request_id)
        {
            Ok(Some(value))
                if value.customer_id
                    == principal.principal.customer_id.clone().unwrap_or_default() =>
            {
                json(200, &value)
            }
            Ok(Some(_)) => json_error(
                403,
                "authority_conflict",
                "customer request is foreign",
                false,
            ),
            Ok(None) => json_error(404, "not_found", "customer request was not found", false),
            Err(error) => workflow_error(error),
        }
    }

    fn project(&self, principal: &BoundPrincipal, path: &str) -> WorkflowHttpResponse {
        if principal.principal.kind == CompanyPrincipalKindV1::Customer {
            return json_error(
                403,
                "authority_conflict",
                "operator or agent authority is required",
                false,
            );
        }
        let Some(value) = query_parameter(path, "project_id") else {
            return json_error(400, "invalid_input", "project_id is required", false);
        };
        let project_id = match ProjectId::parse(value) {
            Ok(value) => value,
            Err(error) => return workflow_error(error),
        };
        match self
            .store
            .company_project(&principal.principal.tenant_id, &project_id)
        {
            Ok(Some(value)) if may_read_full_project(&value, &principal.principal) => {
                json(200, &value)
            }
            Ok(Some(_)) => json_error(
                403,
                "authority_conflict",
                "operator or governed project leadership authority is required",
                false,
            ),
            Ok(None) => json_error(404, "not_found", "project was not found", false),
            Err(error) => workflow_error(error),
        }
    }

    fn work_item(&self, principal: &BoundPrincipal, path: &str) -> WorkflowHttpResponse {
        let Some(project_value) = query_parameter(path, "project_id") else {
            return json_error(400, "invalid_input", "project_id is required", false);
        };
        let Some(work_value) = query_parameter(path, "work_item_id") else {
            return json_error(400, "invalid_input", "work_item_id is required", false);
        };
        let (project_id, work_item_id) = match (
            ProjectId::parse(project_value),
            WorkItemId::parse(work_value),
        ) {
            (Ok(project), Ok(work)) => (project, work),
            (Err(error), _) | (_, Err(error)) => return workflow_error(error),
        };
        match self
            .store
            .work_item(&principal.principal.tenant_id, &project_id, &work_item_id)
        {
            Ok(Some(value)) => json(200, &value),
            Ok(None) => json_error(404, "not_found", "work item was not found", false),
            Err(error) => workflow_error(error),
        }
    }

    fn projection(&self, principal: &BoundPrincipal, path: &str) -> WorkflowHttpResponse {
        if principal.principal.kind == CompanyPrincipalKindV1::Customer {
            return json_error(
                403,
                "authority_conflict",
                "operator or agent authority is required",
                false,
            );
        }
        let Some(value) = query_parameter(path, "project_id") else {
            return json_error(400, "invalid_input", "project_id is required", false);
        };
        let project_id = match ProjectId::parse(value) {
            Ok(value) => value,
            Err(error) => return workflow_error(error),
        };
        match self
            .store
            .company_project_projection(&principal.principal.tenant_id, &project_id)
        {
            Ok(Some(value)) if may_read_full_project(&value.project, &principal.principal) => {
                json(200, &value)
            }
            Ok(Some(_)) => json_error(
                403,
                "authority_conflict",
                "operator or governed project leadership authority is required",
                false,
            ),
            Ok(None) => json_error(404, "not_found", "project projection was not found", false),
            Err(error) => workflow_error(error),
        }
    }

    fn events(&self, principal: &BoundPrincipal, path: &str) -> WorkflowHttpResponse {
        if principal.principal.kind == CompanyPrincipalKindV1::Customer {
            return json_error(
                403,
                "authority_conflict",
                "operator or agent authority is required",
                false,
            );
        }
        let after = match query_parameter(path, "after").map(str::parse::<u64>) {
            Some(Ok(value)) => value,
            None => 0,
            _ => return json_error(400, "invalid_input", "after must be non-negative", false),
        };
        let limit = match query_parameter(path, "limit").map(str::parse::<usize>) {
            Some(Ok(value)) if (1..=1_000).contains(&value) => value,
            None => 100,
            _ => return json_error(400, "invalid_input", "limit is invalid", false),
        };
        let events = match self.store.company_project_events_since(
            &principal.principal.tenant_id,
            after,
            limit,
        ) {
            Ok(value) => value,
            Err(error) => return workflow_error(error),
        };
        if principal.principal.kind == CompanyPrincipalKindV1::Operator {
            return json(200, &events);
        }
        let mut authorized = Vec::new();
        for event in events {
            let current = match self
                .store
                .company_project(&principal.principal.tenant_id, &event.project_id)
            {
                Ok(Some(project)) => project,
                Ok(None) => {
                    return json_error(
                        503,
                        "workflow_corrupt",
                        "event references a missing project",
                        false,
                    )
                }
                Err(error) => return workflow_error(error),
            };
            if may_read_full_project(&current, &principal.principal) {
                authorized.push(event);
            }
        }
        json(200, &authorized)
    }

    fn delivery_command(&self, principal: &BoundPrincipal, body: &[u8]) -> WorkflowHttpResponse {
        let Some(delivery) = self.delivery.as_ref() else {
            return json_error(
                503,
                "delivery_unavailable",
                "delivery authority is unavailable",
                true,
            );
        };
        let envelope: DeliveryCommandEnvelope = match decode_body(body) {
            Ok(value) => value,
            Err(response) => return response,
        };
        if envelope.idempotency_key.is_empty() || envelope.idempotency_key.len() > 128 {
            return json_error(400, "invalid_input", "idempotency_key is invalid", false);
        }
        let Some(delivery_principal) = delivery_principal(&principal.principal) else {
            return json_error(
                403,
                "authority_conflict",
                "principal has no delivery authority",
                false,
            );
        };
        let context = crate::delivery::CommandContextV1 {
            principal: delivery_principal,
            idempotency_key: envelope.idempotency_key,
            now_ms: now_unix_ms(),
        };
        let Ok(_guard) = self.mutation_fence.read() else {
            return json_error(503, "workflow_busy", "workflow recovery is active", true);
        };
        let result = match envelope.command {
            ProductDeliveryCommand::RegisterCandidate { candidate } => delivery
                .register_candidate(&context, candidate)
                .and_then(delivery_json),
            ProductDeliveryCommand::AssignQa {
                tenant_id,
                project_id,
                candidate_id,
                plan,
                run,
            } => delivery
                .assign_qa(&context, &tenant_id, &project_id, &candidate_id, *plan, run)
                .and_then(delivery_json),
            ProductDeliveryCommand::TransitionQa {
                tenant_id,
                project_id,
                run_id,
                next,
            } => delivery
                .transition_qa(&context, &tenant_id, &project_id, &run_id, next)
                .and_then(delivery_json),
            ProductDeliveryCommand::ExecuteQa {
                tenant_id,
                project_id,
                run_id,
            } => delivery
                .execute_qa(&context, &tenant_id, &project_id, &run_id)
                .and_then(delivery_json),
            ProductDeliveryCommand::ImportEvidenceGraph {
                tenant_id,
                project_id,
                run_id,
                graph,
            } => delivery
                .import_evidence_graph(&context, &tenant_id, &project_id, &run_id, graph)
                .and_then(delivery_json),
            ProductDeliveryCommand::RecordGate {
                tenant_id,
                project_id,
                run_id,
                gate,
            } => delivery
                .record_gate(&context, &tenant_id, &project_id, &run_id, gate)
                .and_then(delivery_json),
            ProductDeliveryCommand::RecordReviewBundle {
                tenant_id,
                project_id,
                run_id,
                review,
                test_run,
                findings,
                approval,
            } => delivery
                .record_review_bundle(
                    &context,
                    &tenant_id,
                    &project_id,
                    &run_id,
                    review,
                    test_run,
                    findings,
                    approval,
                )
                .and_then(delivery_json),
            ProductDeliveryCommand::Promote {
                tenant_id,
                project_id,
                candidate_id,
                manifest,
                release,
            } => delivery
                .promote(
                    &context,
                    &tenant_id,
                    &project_id,
                    &candidate_id,
                    manifest,
                    release,
                )
                .and_then(delivery_json),
            ProductDeliveryCommand::IssueDelivery {
                project_id,
                receipt,
            } => delivery
                .issue_delivery(&context, &project_id, receipt)
                .and_then(delivery_json),
            ProductDeliveryCommand::CustomerAction {
                tenant_id,
                project_id,
                feedback,
                acceptance,
            } => delivery
                .customer_action(&context, &tenant_id, &project_id, feedback, acceptance)
                .and_then(delivery_json),
            ProductDeliveryCommand::Rollback {
                tenant_id,
                project_id,
                rollback,
            } => delivery
                .rollback(&context, &tenant_id, &project_id, rollback)
                .and_then(delivery_json),
            ProductDeliveryCommand::Closeout {
                tenant_id,
                project_id,
                closeout,
            } => delivery
                .closeout(&context, &tenant_id, &project_id, closeout)
                .and_then(delivery_json),
        };
        match result {
            Ok(value) => json(200, &value),
            Err(error) => delivery_error(error),
        }
    }

    fn delivery_lineage(&self, principal: &BoundPrincipal, path: &str) -> WorkflowHttpResponse {
        let Some(delivery) = self.delivery.as_ref() else {
            return json_error(
                503,
                "delivery_unavailable",
                "delivery authority is unavailable",
                true,
            );
        };
        let (Some(tenant_id), Some(project_id)) = (
            query_parameter(path, "tenant_id"),
            query_parameter(path, "project_id"),
        ) else {
            return json_error(
                400,
                "invalid_input",
                "tenant_id and project_id are required",
                false,
            );
        };
        let Some(delivery_principal) = delivery_principal(&principal.principal) else {
            return json_error(
                403,
                "authority_conflict",
                "principal has no delivery authority",
                false,
            );
        };
        let context = crate::delivery::CommandContextV1 {
            principal: delivery_principal,
            idempotency_key: format!("read-lineage-{tenant_id}-{project_id}"),
            now_ms: now_unix_ms(),
        };
        match delivery.read_public_lineage(&context, tenant_id, project_id) {
            Ok(value) => {
                if principal.principal.role == CompanyRoleV1::Gaia {
                    if let Err(error) =
                        self.record_gaia_oversight(principal, tenant_id, project_id, &value)
                    {
                        return delivery_error(error);
                    }
                }
                json(200, &value)
            }
            Err(error) => delivery_error(error),
        }
    }

    fn record_gaia_oversight(
        &self,
        principal: &BoundPrincipal,
        tenant_id: &str,
        project_id: &str,
        lineage: &crate::delivery::PublicDeliveryLineageDtoV1,
    ) -> Result<(), crate::delivery::DeliveryError> {
        let event_store = self.event_store.as_ref().ok_or_else(|| {
            crate::delivery::DeliveryError::AdapterUnavailable {
                dependency: "gaia_oversight_event_store",
                reason: "event store is unavailable".to_string(),
            }
        })?;
        let mut canonical = lineage.clone();
        canonical.read_at_ms = 0;
        let lineage_digest = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&canonical)
                    .map_err(|error| crate::delivery::DeliveryError::Storage(error.to_string()))?
            )
        );
        let payload = DomainEventPayload::GaiaProjectOversightObserved {
            tenant_id: tenant_id.to_string(),
            project_id: project_id.to_string(),
            project_revision: lineage.revision,
            lineage_digest: lineage_digest.clone(),
            observer_principal_id: principal.principal.principal_id.clone(),
            observer_authority_generation: principal.principal.authority_generation,
        };
        let operation_id = format!(
            "gaia-oversight:{tenant_id}:{project_id}:{}:{}:{}",
            lineage.revision,
            principal.principal.principal_id,
            principal.principal.authority_generation
        );
        let event_identity = format!(
            "{:x}",
            Sha256::digest(format!("{operation_id}:{lineage_digest}").as_bytes())
        );
        let event = DomainEvent {
            event_id: format!("gaia-oversight-{event_identity}"),
            event_type: payload.event_type_str().to_string(),
            aggregate_id: format!("PROJECT:{tenant_id}:{project_id}"),
            payload: payload.to_json(),
            correlation_id: project_id.to_string(),
            causation_id: None,
            operation_id: operation_id.clone(),
            tick: 0,
            timestamp_ms: now_unix_ms(),
            schema_version: 1,
            compensation_type: "none".to_string(),
        };
        if let Some(existing) = event_store
            .event_by_operation_id(&operation_id)
            .map_err(|error| crate::delivery::DeliveryError::Storage(error.to_string()))?
        {
            if existing.event_id == event.event_id
                && existing.event_type == event.event_type
                && existing.aggregate_id == event.aggregate_id
                && existing.payload == event.payload
                && existing.correlation_id == event.correlation_id
                && existing.operation_id == event.operation_id
                && existing.schema_version == event.schema_version
                && existing.compensation_type == event.compensation_type
            {
                return Ok(());
            }
            return Err(crate::delivery::DeliveryError::Conflict(
                "Gaia oversight operation is already bound to different lineage".to_string(),
            ));
        }
        event_store
            .legacy_append_gateway(sentinel_limbo::LegacyEventProducer::DaemonWorkflow)
            .append_event(&event)
            .map_err(|error| crate::delivery::DeliveryError::Storage(error.to_string()))?;
        Ok(())
    }

    pub fn reconcile_pending(&self) {
        self.reconcile_pending_until(|| false);
    }

    pub fn reconcile_pending_until(&self, should_stop: impl Fn() -> bool) {
        if !self.enabled {
            return;
        }
        let Ok(_guard) = self.mutation_fence.try_write() else {
            return;
        };
        if should_stop() {
            return;
        }
        let result = self.reconcile_batch(&should_stop);
        self.scan_succeeded.store(result.is_ok(), Ordering::Release);
        if let Ok(mut last_error) = self.last_error.lock() {
            *last_error = result.err().map(|error| format!("{:?}", error.code));
        }
    }

    fn reconcile_batch(&self, should_stop: &impl Fn() -> bool) -> Result<(), WorkflowError> {
        self.publish_collaboration_backlog()
            .map_err(|_| workflow_unavailable())?;
        for pending in self.store.pending_executions(MAX_RECONCILE_BATCH)? {
            if should_stop() {
                return Ok(());
            }
            let work = self.core.reconcile_execution(&pending, now_unix_ms())?;
            self.sync_company_state(&work)?;
        }
        for pending in self
            .store
            .pending_completion_evidence(MAX_RECONCILE_BATCH)?
        {
            if should_stop() {
                return Ok(());
            }
            let work = self
                .core
                .reconcile_completion_evidence(&pending, now_unix_ms())?;
            self.sync_company_state(&work)?;
        }
        for pending in self.store.pending_gate_evidence(MAX_RECONCILE_BATCH)? {
            if should_stop() {
                return Ok(());
            }
            let work = self.core.reconcile_gate_evidence(&pending, now_unix_ms())?;
            self.sync_company_state(&work)?;
        }
        self.reconcile_company_state_page()?;
        if let Some(delivery) = self.delivery.as_ref() {
            delivery
                .publish_pending()
                .map_err(delivery_workflow_error)?;
        }
        Ok(())
    }

    fn reconcile_company_state_page(&self) -> Result<(), WorkflowError> {
        let cursor = self
            .company_sync_cursor
            .lock()
            .map_err(|_| workflow_persistence_failure())?
            .clone();
        let page = self.store.workflow_items_after(
            cursor
                .as_ref()
                .map(|(tenant, project, work)| (tenant, project, work)),
            MAX_RECONCILE_BATCH,
        )?;
        for execution in &page {
            self.sync_company_state(execution)?;
        }
        let next = page.last().map(|execution| {
            (
                execution.tenant_id.clone(),
                execution.project_id.clone(),
                execution.work_item_id.clone(),
            )
        });
        *self
            .company_sync_cursor
            .lock()
            .map_err(|_| workflow_persistence_failure())? = next;
        Ok(())
    }

    fn sync_company_state(
        &self,
        execution: &sentinel_workflow::WorkItemExecutionV1,
    ) -> Result<(), WorkflowError> {
        for _ in 0..3 {
            let project = self
                .store
                .company_project(&execution.tenant_id, &execution.project_id)?
                .ok_or_else(|| {
                    WorkflowError::new(WorkflowErrorCode::NotFound, false, "project not found")
                })?;
            let work = project
                .work_items
                .get(&execution.work_item_id)
                .ok_or_else(|| {
                    WorkflowError::new(WorkflowErrorCode::NotFound, false, "work item not found")
                })?;
            let assignment = work
                .assignments
                .iter()
                .find(|value| value.active)
                .ok_or_else(|| {
                    WorkflowError::new(
                        WorkflowErrorCode::AuthorityConflict,
                        false,
                        "assignment unavailable",
                    )
                })?;
            let Some(target) = company_transition_target(execution.state, work.state)? else {
                return Ok(());
            };
            let current_authority = self
                .authority
                .as_ref()
                .ok_or_else(workflow_unavailable)?
                .snapshot(
                    &execution.tenant_id,
                    &execution.project_id,
                    &execution.work_item_id,
                    execution.agent_id,
                )
                .map_err(company_sync_authority_error)?;
            if !execution.plan.authority_matches(&current_authority) {
                return Err(WorkflowError::new(
                    WorkflowErrorCode::AuthorityConflict,
                    false,
                    "company transition authority changed after execution",
                ));
            }
            let gate_owned_transition = target == sentinel_workflow::CompanyWorkStateV1::Done
                || target == sentinel_workflow::CompanyWorkStateV1::Blocked
                    && work.state == sentinel_workflow::CompanyWorkStateV1::InReview;
            let principal = if gate_owned_transition {
                independent_gate_principal(
                    &project.governance.participants,
                    assignment.agent_id,
                    &self.principals,
                )?
            } else {
                project
                    .governance
                    .participants
                    .iter()
                    .find(|value| value.agent_id == assignment.agent_id)
                    .and_then(|value| self.principals.principal(&value.principal_id))
                    .ok_or_else(principal_unavailable)?
            };
            let outputs = if matches!(
                target,
                sentinel_workflow::CompanyWorkStateV1::InReview
                    | sentinel_workflow::CompanyWorkStateV1::Done
            ) {
                execution
                    .terminal_execution_evidence
                    .as_ref()
                    .map(|evidence| {
                        work.spec
                            .outputs
                            .iter()
                            .zip(&evidence.outputs)
                            .map(
                                |(contract, output)| sentinel_workflow::WorkOutputReceiptV1 {
                                    name: contract.name.clone(),
                                    contract_generation: contract.contract_generation,
                                    contract_digest: contract.contract_digest.clone(),
                                    content_digest: output.digest.clone(),
                                },
                            )
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let gate_receipt = company_gate_receipt(target, execution.gate_evidence.as_ref())?;
            let occurred_at = now_unix_ms();
            let receipt = sentinel_workflow::WorkTransitionReceiptV1 {
                schema_version: sentinel_workflow::COMPANY_DOMAIN_SCHEMA_VERSION,
                project_id: project.project_id.clone(),
                work_item_id: execution.work_item_id.clone(),
                expected_project_version: project.version,
                expected_work_version: work.version,
                expected_assignment_version: assignment.assignment_version,
                from_state: work.state,
                to_state: target,
                output_receipts: outputs,
                gate_receipt,
                phase_a_evidence_digest: execution.plan.request_digest.clone(),
                reason_ref: match (target, work.state) {
                    (sentinel_workflow::CompanyWorkStateV1::Done, _) => {
                        "independent-quality-gate-evidence"
                    }
                    (
                        sentinel_workflow::CompanyWorkStateV1::Blocked,
                        sentinel_workflow::CompanyWorkStateV1::InReview,
                    ) => "independent-quality-gate-timeout",
                    (sentinel_workflow::CompanyWorkStateV1::Blocked, _) => {
                        "workbench-execution-blocked"
                    }
                    _ => "sealed-workbench-evidence",
                }
                .to_owned(),
                occurred_at_unix_ms: occurred_at,
            };
            let command = CompanyWorkflowCommandV1::ApplyWorkTransition {
                project_id: project.project_id,
                expected_version: project.version,
                receipt,
            };
            let operation_id = stable_operation_id(
                company_transition_operation(target),
                &execution.plan.request_digest,
                execution.version,
            );
            self.core.apply_company_command(
                &principal.principal,
                operation_id,
                &command,
                occurred_at,
            )?;
        }
        Ok(())
    }

    pub fn health(&self) -> WorkflowHealthSnapshot {
        let pending_execution = self.store.pending_executions(MAX_RECONCILE_BATCH);
        let pending_completion = self.store.pending_completion_evidence(MAX_RECONCILE_BATCH);
        let pending_gate = self.store.pending_gate_evidence(MAX_RECONCILE_BATCH);
        let last_error = self.last_error.lock().ok().and_then(|value| value.clone());
        let canonical_event_cursor = self.store.company_event_cursor();
        let delivery_ready = self
            .delivery
            .as_ref()
            .is_some_and(|delivery| delivery.readiness().is_ok());
        let delivery_publication_pending = self
            .delivery
            .as_ref()
            .and_then(|delivery| delivery.pending_publication_count().ok());
        let collaboration_publication_pending = self
            .collaboration_publication_pending
            .load(Ordering::Acquire);
        let dependencies_ready = self.core.dependencies_ready() && delivery_ready;
        let ready = self.enabled
            && dependencies_ready
            && self.scan_succeeded.load(Ordering::Acquire)
            && last_error.is_none()
            && pending_execution.is_ok()
            && pending_completion.is_ok()
            && pending_gate.is_ok()
            && delivery_publication_pending == Some(0)
            && collaboration_publication_pending == 0
            && canonical_event_cursor.is_ok();
        WorkflowHealthSnapshot {
            enabled: self.enabled,
            status: if ready {
                "ready".to_owned()
            } else if self.enabled {
                "degraded".to_owned()
            } else {
                "disabled".to_owned()
            },
            ready,
            dependencies_ready,
            canonical_event_cursor: canonical_event_cursor.ok(),
            pending_execution: pending_execution
                .map(|value| value.len())
                .unwrap_or(MAX_RECONCILE_BATCH),
            pending_completion: pending_completion
                .map(|value| value.len())
                .unwrap_or(MAX_RECONCILE_BATCH),
            pending_gate: pending_gate
                .map(|value| value.len())
                .unwrap_or(MAX_RECONCILE_BATCH),
            delivery_ready,
            delivery_publication_pending: delivery_publication_pending
                .unwrap_or(MAX_RECONCILE_BATCH),
            collaboration_publication_pending,
            last_error,
        }
    }
}

#[cfg(feature = "llm")]
impl crate::llm_bridge::bridge::ProviderUsageAuthorityResolver for WorkflowApi {
    fn resolve_provider_usage_authority(
        &self,
        agent_id: AgentId,
    ) -> Result<Option<crate::llm_bridge::bridge::ProviderUsageAuthority>, &'static str> {
        self.provider_usage_binding_for_agent(agent_id)
            .map(|binding| {
                binding.map(
                    |binding| crate::llm_bridge::bridge::ProviderUsageAuthority {
                        tenant_id: binding.tenant_id,
                        project_id: binding.project_id,
                        work_item_id: binding.work_item_id,
                        reservation_id: binding.reservation_id,
                        assignment_id: binding.assignment_id,
                        assignment_version: binding.assignment_version,
                        agent_id: binding.agent_id,
                        provider: binding.provider,
                    },
                )
            })
    }
}

fn principal_unavailable() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::AuthorityConflict,
        false,
        "principal unavailable",
    )
}

fn workflow_persistence_failure() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::PersistenceFailure,
        true,
        "workflow synchronization state is unavailable",
    )
}

fn delivery_workflow_error(error: crate::delivery::DeliveryError) -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::PersistenceFailure,
        true,
        match error {
            crate::delivery::DeliveryError::AdapterUnavailable { .. } => {
                "delivery adapter is unavailable"
            }
            _ => "delivery reconciliation failed",
        },
    )
}

fn company_sync_authority_error(error: WorkflowPortError) -> WorkflowError {
    match error {
        WorkflowPortError::Unavailable => WorkflowError::new(
            WorkflowErrorCode::OrganizationUnavailable,
            true,
            "company transition authority is unavailable",
        ),
        _ => WorkflowError::new(
            WorkflowErrorCode::AuthorityConflict,
            false,
            "company transition authority is invalid or stale",
        ),
    }
}

fn company_transition_target(
    execution: sentinel_workflow::WorkItemState,
    company: sentinel_workflow::CompanyWorkStateV1,
) -> Result<Option<sentinel_workflow::CompanyWorkStateV1>, WorkflowError> {
    let target = match (execution, company) {
        (
            sentinel_workflow::WorkItemState::InProgress,
            sentinel_workflow::CompanyWorkStateV1::Assigned,
        ) => Some(sentinel_workflow::CompanyWorkStateV1::InProgress),
        (
            sentinel_workflow::WorkItemState::InReview | sentinel_workflow::WorkItemState::Done,
            sentinel_workflow::CompanyWorkStateV1::Assigned,
        ) => Some(sentinel_workflow::CompanyWorkStateV1::InProgress),
        (
            sentinel_workflow::WorkItemState::InReview | sentinel_workflow::WorkItemState::Done,
            sentinel_workflow::CompanyWorkStateV1::InProgress,
        ) => Some(sentinel_workflow::CompanyWorkStateV1::InReview),
        (
            sentinel_workflow::WorkItemState::Done,
            sentinel_workflow::CompanyWorkStateV1::InReview,
        ) => Some(sentinel_workflow::CompanyWorkStateV1::Done),
        (
            sentinel_workflow::WorkItemState::Blocked | sentinel_workflow::WorkItemState::Cancelled,
            sentinel_workflow::CompanyWorkStateV1::Assigned
            | sentinel_workflow::CompanyWorkStateV1::InProgress
            | sentinel_workflow::CompanyWorkStateV1::InReview,
        ) => Some(sentinel_workflow::CompanyWorkStateV1::Blocked),
        (
            sentinel_workflow::WorkItemState::Assigned | sentinel_workflow::WorkItemState::Claimed,
            sentinel_workflow::CompanyWorkStateV1::Assigned,
        )
        | (
            sentinel_workflow::WorkItemState::InProgress,
            sentinel_workflow::CompanyWorkStateV1::InProgress,
        )
        | (
            sentinel_workflow::WorkItemState::InReview,
            sentinel_workflow::CompanyWorkStateV1::InReview,
        )
        | (sentinel_workflow::WorkItemState::Done, sentinel_workflow::CompanyWorkStateV1::Done)
        | (
            sentinel_workflow::WorkItemState::Blocked | sentinel_workflow::WorkItemState::Cancelled,
            sentinel_workflow::CompanyWorkStateV1::Blocked,
        ) => None,
        _ => {
            return Err(WorkflowError::new(
                WorkflowErrorCode::AuthorityConflict,
                false,
                "company and execution work states cannot be reconciled",
            ))
        }
    };
    Ok(target)
}

fn company_transition_operation(target: sentinel_workflow::CompanyWorkStateV1) -> &'static str {
    match target {
        sentinel_workflow::CompanyWorkStateV1::InProgress => "work-transition-in-progress",
        sentinel_workflow::CompanyWorkStateV1::InReview => "work-transition-in-review",
        sentinel_workflow::CompanyWorkStateV1::Done => "work-transition-done",
        sentinel_workflow::CompanyWorkStateV1::Blocked => "work-transition-blocked",
        _ => "work-transition-invalid",
    }
}

fn company_gate_receipt(
    target: sentinel_workflow::CompanyWorkStateV1,
    evidence: Option<&sentinel_workflow::GateEvidenceReadbackV1>,
) -> Result<Option<sentinel_workflow::QualityGateReceiptBindingV1>, WorkflowError> {
    if target != sentinel_workflow::CompanyWorkStateV1::Done {
        return Ok(None);
    }
    let evidence = evidence.ok_or_else(|| {
        WorkflowError::new(
            WorkflowErrorCode::AuthorityConflict,
            false,
            "independent gate evidence unavailable",
        )
    })?;
    Ok(Some(sentinel_workflow::QualityGateReceiptBindingV1 {
        gate_id: evidence.profile_id.clone(),
        generation: evidence.profile_generation,
        gate_digest: evidence.profile_digest.clone(),
        subject_digest: evidence.subject_digest.clone(),
        passed: evidence.passed,
    }))
}

fn independent_gate_principal(
    participants: &[sentinel_workflow::ParticipantBindingV1],
    assignee: AgentId,
    principals: &PrincipalAuthenticator,
) -> Result<BoundPrincipal, WorkflowError> {
    [CompanyRoleV1::Qa, CompanyRoleV1::ReleaseManager]
        .into_iter()
        .find_map(|role| {
            participants
                .iter()
                .filter(|participant| participant.agent_id != assignee && participant.role == role)
                .find_map(|participant| {
                    principals
                        .principal(&participant.principal_id)
                        .filter(|principal| {
                            principal.principal.role == participant.role
                                && principal.principal.agent_id == Some(participant.agent_id)
                        })
                })
        })
        .ok_or_else(principal_unavailable)
}

fn workflow_enabled() -> Result<bool, WorkflowError> {
    match std::env::var("SENTINEL_COMPANY_WORKFLOW_ENABLED") {
        Err(std::env::VarError::NotPresent) => Ok(false),
        Ok(value) if matches!(value.as_str(), "1" | "true" | "TRUE") => Ok(true),
        Ok(value) if matches!(value.as_str(), "0" | "false" | "FALSE") => Ok(false),
        _ => Err(workflow_unavailable()),
    }
}

fn is_workflow_path(path: &str) -> bool {
    matches!(
        path,
        CUSTOMER_COMMAND_PATH
            | CUSTOMER_REQUEST_PATH
            | OPERATOR_COMMAND_PATH
            | AGENT_COMMAND_PATH
            | OPERATOR_PROJECT_PATH
            | OPERATOR_WORK_ITEM_PATH
            | OPERATOR_PROJECTION_PATH
            | OPERATOR_EVENTS_PATH
            | DELIVERY_COMMAND_PATH
            | DELIVERY_INTENT_PATH
            | DELIVERY_LINEAGE_PATH
            | COLLABORATION_VIEW_PATH
            | COLLABORATION_ADMISSION_PATH
            | WORKFLOW_READINESS_PATH
    )
}

fn decode_body<T: for<'de> Deserialize<'de>>(body: &[u8]) -> Result<T, WorkflowHttpResponse> {
    if body.len() > MAX_WORKFLOW_BODY_BYTES {
        return Err(json_error(
            413,
            "payload_too_large",
            "workflow payload is too large",
            false,
        ));
    }
    serde_json::from_slice(body)
        .map_err(|_| json_error(400, "invalid_input", "workflow payload is invalid", false))
}

fn query_parameter<'a>(path: &'a str, name: &str) -> Option<&'a str> {
    path.split_once('?')?.1.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name && !value.is_empty()).then_some(value)
    })
}

#[derive(Serialize)]
struct PublicWorkflowError<'a> {
    code: &'a str,
    error: &'a str,
    retryable: bool,
}

fn json<T: Serialize>(status: u16, value: &T) -> WorkflowHttpResponse {
    WorkflowHttpResponse {
        status,
        body: serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec()),
    }
}

fn json_error(
    status: u16,
    code: &'static str,
    message: &'static str,
    retryable: bool,
) -> WorkflowHttpResponse {
    json(
        status,
        &PublicWorkflowError {
            code,
            error: message,
            retryable,
        },
    )
}

fn workflow_error(error: WorkflowError) -> WorkflowHttpResponse {
    let status = match error.code {
        WorkflowErrorCode::NotFound => 404,
        WorkflowErrorCode::AuthorityConflict => 403,
        WorkflowErrorCode::VersionConflict | WorkflowErrorCode::IdempotencyConflict => 409,
        WorkflowErrorCode::OrganizationUnavailable
        | WorkflowErrorCode::ExecutionUnavailable
        | WorkflowErrorCode::CompletionUnavailable
        | WorkflowErrorCode::GateUnavailable
        | WorkflowErrorCode::PersistenceFailure => 503,
        _ => 422,
    };
    json_error(
        status,
        workflow_error_code(error.code),
        error.message,
        error.retryable,
    )
}

fn delivery_principal(
    principal: &AuthenticatedCompanyPrincipalV1,
) -> Option<crate::delivery::PrincipalV1> {
    use crate::delivery::AuthorityRole;

    let roles = match principal.role {
        CompanyRoleV1::Customer => BTreeSet::from([AuthorityRole::Customer]),
        CompanyRoleV1::Designer | CompanyRoleV1::Developer => {
            BTreeSet::from([AuthorityRole::Developer])
        }
        CompanyRoleV1::Qa => BTreeSet::from([AuthorityRole::Qa]),
        CompanyRoleV1::ReleaseManager => BTreeSet::from([AuthorityRole::ReleaseManager]),
        CompanyRoleV1::Gaia => BTreeSet::from([AuthorityRole::GaiaObserver]),
        CompanyRoleV1::Sales | CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead => {
            return None
        }
    };
    Some(crate::delivery::PrincipalV1 {
        tenant_id: principal.tenant_id.0.clone(),
        principal_id: principal.principal_id.clone(),
        authority_generation: principal.authority_generation,
        roles,
    })
}

fn is_internal_company_command(command: &CompanyWorkflowCommandV1) -> bool {
    matches!(
        command,
        CompanyWorkflowCommandV1::ApplyWorkTransition { .. }
            | CompanyWorkflowCommandV1::CreateGovernedRework { .. }
            | CompanyWorkflowCommandV1::AdmitCollaboration { .. }
            | CompanyWorkflowCommandV1::ProgressCollaborationAdmission { .. }
            | CompanyWorkflowCommandV1::RecordCollaborationReliability { .. }
    )
}

fn delivery_json<T: Serialize>(
    value: T,
) -> Result<serde_json::Value, crate::delivery::DeliveryError> {
    serde_json::to_value(value)
        .map_err(|error| crate::delivery::DeliveryError::Storage(error.to_string()))
}

fn delivery_error(error: crate::delivery::DeliveryError) -> WorkflowHttpResponse {
    use crate::delivery::DeliveryError;

    let (status, code, retryable) = match error {
        DeliveryError::NotFound(_) => (404, "not_found", false),
        DeliveryError::AuthorityDenied(_) => (403, "authority_conflict", false),
        DeliveryError::Conflict(_)
        | DeliveryError::IdempotencyConflict { .. }
        | DeliveryError::RevisionConflict { .. } => (409, "delivery_conflict", false),
        DeliveryError::AdapterUnavailable { .. } | DeliveryError::Storage(_) => {
            (503, "delivery_unavailable", true)
        }
        DeliveryError::CorruptStore(_) => (503, "delivery_corrupt", false),
        DeliveryError::InvalidDigest(_)
        | DeliveryError::InvalidState { .. }
        | DeliveryError::MissingEvidence(_)
        | DeliveryError::StaleEvidence(_)
        | DeliveryError::Validation(_) => (422, "delivery_rejected", false),
    };
    json_error(status, code, "delivery command was rejected", retryable)
}

fn workflow_error_code(code: WorkflowErrorCode) -> &'static str {
    match code {
        WorkflowErrorCode::InvalidInput => "invalid_input",
        WorkflowErrorCode::InvalidDigest => "invalid_digest",
        WorkflowErrorCode::InvalidTransition => "invalid_transition",
        WorkflowErrorCode::NotFound => "not_found",
        WorkflowErrorCode::VersionConflict => "version_conflict",
        WorkflowErrorCode::IdempotencyConflict => "idempotency_conflict",
        WorkflowErrorCode::AuthorityConflict => "authority_conflict",
        WorkflowErrorCode::OrganizationUnavailable => "organization_unavailable",
        WorkflowErrorCode::ExecutionUnavailable => "execution_unavailable",
        WorkflowErrorCode::CompletionUnavailable => "completion_unavailable",
        WorkflowErrorCode::GateUnavailable => "gate_unavailable",
        WorkflowErrorCode::UnknownOutcome => "unknown_outcome",
        WorkflowErrorCode::CorruptStore => "corrupt_store",
        WorkflowErrorCode::PersistenceFailure => "persistence_failure",
    }
}

fn stable_operation_id(domain: &str, digest: &str, version: u64) -> Uuid {
    let bytes = Sha256::digest(format!("{domain}:{digest}:{version}").as_bytes());
    let mut value = [0_u8; 16];
    value.copy_from_slice(&bytes[..16]);
    value[6] = (value[6] & 0x0f) | 0x50;
    value[8] = (value[8] & 0x3f) | 0x80;
    Uuid::from_bytes(value)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{symlink, PermissionsExt};

    use super::*;

    fn principal_binding(principal_id: &str) -> PrincipalBinding {
        PrincipalBinding {
            credential_name: "workflow-test".to_owned(),
            tenant_id: TenantId::parse("tenant-m0").unwrap(),
            principal_id: principal_id.to_owned(),
            kind: CompanyPrincipalKindV1::Agent,
            role: CompanyRoleV1::Developer,
            customer_id: None,
            agent_id: Some(AgentId(6)),
            authority_generation: 1,
        }
    }

    fn collaboration_publication() -> CollaborationPublicationV1 {
        let authority_digest = "a".repeat(64);
        let tenant_id = TenantId::parse("tenant-collaboration").unwrap();
        let project_id = ProjectId::parse("project-collaboration").unwrap();
        let work_item_id = WorkItemId::parse("work-collaboration").unwrap();
        let mut session = sentinel_workflow::CollaborationSessionV1 {
            schema_version: sentinel_workflow::COLLABORATION_SCHEMA_VERSION,
            session_id: "collaboration-session".to_owned(),
            tenant_id: tenant_id.clone(),
            project_id: project_id.clone(),
            work_item_id: Some(work_item_id.clone()),
            admission_id: None,
            admission_contract_digest: None,
            collaboration_generation: None,
            admission_routes: Vec::new(),
            organization_generation: 1,
            organization_digest: authority_digest.clone(),
            assignment_id: "assignment-collaboration".to_owned(),
            assignment_version: 1,
            assignment_digest: authority_digest.clone(),
            policy_version: 1,
            policy_digest: authority_digest.clone(),
            subject_ref: "publish one collaboration transition".to_owned(),
            input_digest: authority_digest.clone(),
            mode: sentinel_workflow::CollaborationModeV1::IndependentReview,
            budget: sentinel_workflow::CollaborationBudgetV1 {
                max_participants: 2,
                max_claims: 2,
                max_handoffs: 1,
                max_clarification_rounds: 1,
                max_transitions: 8,
                deadline_unix_ms: 100,
            },
            participants: vec![
                sentinel_workflow::CollaborationParticipantV1 {
                    agent_id: AgentId(1),
                    permanent_role: CompanyRoleV1::ProjectManager,
                    mandate: sentinel_workflow::BehaviorMandateV1::Synthesize,
                    capability_snapshot_digest: authority_digest.clone(),
                    capabilities: BTreeSet::from(["coordination".to_owned()]),
                    privacy_classes: BTreeSet::from(["project-internal".to_owned()]),
                },
                sentinel_workflow::CollaborationParticipantV1 {
                    agent_id: AgentId(2),
                    permanent_role: CompanyRoleV1::Developer,
                    mandate: sentinel_workflow::BehaviorMandateV1::Implement,
                    capability_snapshot_digest: authority_digest.clone(),
                    capabilities: BTreeSet::from(["rust".to_owned()]),
                    privacy_classes: BTreeSet::from(["project-internal".to_owned()]),
                },
            ],
            state: sentinel_workflow::CollaborationSessionStateV1::Planned,
            transition_sequence: 1,
            publication_revision: 1,
            binding_digest: String::new(),
            claims: Vec::new(),
            transition_history: Vec::new(),
            created_by: AgentId(1),
            created_at_unix_ms: 1,
            updated_at_unix_ms: 1,
        };
        session.binding_digest = session.expected_binding_digest().unwrap();
        let payload_value = sentinel_workflow::CollaborationEventPayloadV1 {
            schema_version: sentinel_workflow::COLLABORATION_SCHEMA_VERSION,
            tenant_id: tenant_id.clone(),
            project_id: project_id.clone(),
            session_id: session.session_id.clone(),
            transition_sequence: 1,
            command_digest: authority_digest.clone(),
            record: sentinel_workflow::CollaborationEventRecordV1::Session(session.clone()),
        };
        let payload = sentinel_common::canonical_json(&payload_value).unwrap();
        let payload_digest = sentinel_common::sha256_hex(&payload);
        let operation_id = Uuid::from_u128(739);
        let proposal = sentinel_common::AppendProposalV2 {
            proposal_version: sentinel_common::EVENT_PROPOSAL_VERSION_V2,
            requested_event_id: None,
            event_type: sentinel_workflow::COLLABORATION_EVENT_TYPE.to_owned(),
            schema_version: sentinel_workflow::COLLABORATION_EVENT_SCHEMA_VERSION,
            payload_codec: sentinel_common::EventPayloadCodec::Json,
            payload_digest: payload_digest.clone(),
            payload,
            causal_context: sentinel_common::CausalContextV1 {
                schema_version: sentinel_common::CAUSAL_CONTEXT_VERSION_V1,
                tenant: sentinel_common::AuthorityRefV1 {
                    kind: sentinel_common::AuthorityKindV1::Tenant,
                    id: tenant_id.0.clone(),
                    authority_generation: 1,
                    authority_digest: authority_digest.clone(),
                },
                company: sentinel_common::AuthorityRefV1 {
                    kind: sentinel_common::AuthorityKindV1::Company,
                    id: "virtual-company".to_owned(),
                    authority_generation: 1,
                    authority_digest: authority_digest.clone(),
                },
                project: sentinel_common::AuthorityRefV1 {
                    kind: sentinel_common::AuthorityKindV1::Project,
                    id: project_id.0.clone(),
                    authority_generation: 1,
                    authority_digest: authority_digest.clone(),
                },
                workflow: Some(sentinel_common::AuthorityRefV1 {
                    kind: sentinel_common::AuthorityKindV1::Workflow,
                    id: session.session_id.clone(),
                    authority_generation: 1,
                    authority_digest: session.binding_digest,
                }),
                work_item: Some(sentinel_common::AuthorityRefV1 {
                    kind: sentinel_common::AuthorityKindV1::WorkItem,
                    id: work_item_id.0,
                    authority_generation: 1,
                    authority_digest: authority_digest.clone(),
                }),
                request_id: operation_id.to_string(),
                request_digest: authority_digest.clone(),
                correlation_id: session.session_id.clone(),
                causation_event_id: None,
                operation_id: operation_id.to_string(),
                attempt: 1,
                source_generation: 1,
                source_digest: payload_digest.clone(),
                invocation_id: None,
                agent_id: Some("agent-1".to_owned()),
                tick: None,
                artifact_id: None,
                artifact_digest: None,
                qa_run_id: None,
                release_id: None,
                delivery_id: None,
                diagnostic_trace_id: None,
                diagnostic_span_id: None,
            },
            producer: sentinel_workflow::COLLABORATION_EVENT_PRODUCER.to_owned(),
            owner_term: None,
            tick: None,
            requested_durability: sentinel_common::EventDurability::Authoritative,
            expected_stream_revision: sentinel_common::ExpectedStreamRevision::NoStream,
            delivery_intents: vec![sentinel_common::DeliveryIntentV1 {
                intent_id: format!("collaboration-delivery-{operation_id}"),
                topic: sentinel_workflow::COLLABORATION_DELIVERY_TOPIC.to_owned(),
                payload_digest,
            }],
            effect_reservations: Vec::new(),
        };
        CollaborationPublicationV1 {
            operation_id,
            session_id: session.session_id,
            transition_sequence: 1,
            proposal,
        }
    }

    #[test]
    fn collaboration_publication_replays_without_a_second_event() {
        let events = sentinel_limbo::EventStore::open(":memory:").unwrap();
        let registry = collaboration_event_schema_registry().unwrap();
        let publication = collaboration_publication();
        let pending = AtomicUsize::new(usize::MAX);

        let first = publish_collaboration_publications(
            &events,
            &registry,
            std::slice::from_ref(&publication),
            &pending,
        )
        .unwrap();
        assert_eq!(first, [sentinel_common::AppendDispositionV2::Appended]);
        assert_eq!(pending.load(Ordering::Acquire), 0);

        let replay = publish_collaboration_publications(
            &events,
            &registry,
            std::slice::from_ref(&publication),
            &pending,
        )
        .unwrap();
        assert_eq!(
            replay,
            [sentinel_common::AppendDispositionV2::ReplayOfPriorOperation]
        );
        assert_eq!(pending.load(Ordering::Acquire), 0);
    }

    fn provider_usage_event() -> DomainEvent {
        let payload = DomainEventPayload::AgentLlmUsage {
            agent_id: AgentId(6),
            tenant_id: Some("tenant-m0".to_owned()),
            project_id: Some("project-m0".to_owned()),
            work_item_id: Some("build-site".to_owned()),
            reservation_id: Some("reservation-m0".to_owned()),
            assignment_id: Some("assignment-m0".to_owned()),
            assignment_version: Some(1),
            provider: Some("local-loop".to_owned()),
            requested_model: Some("local-loop-v1".to_owned()),
            caller_role: Some("agent_runtime".to_owned()),
            tier: "mid".to_owned(),
            hierarchy_tier: Some(sentinel_common::HierarchyTier::TIER_2),
            cost_source: Some(sentinel_common::CostSource::ProviderReported),
            effective_model: Some("local-loop-v1".to_owned()),
            input_tokens: 10,
            output_tokens: 5,
            cache_read: 0,
            cache_creation: 0,
            cost_usd: 0.0,
        };
        DomainEvent::new(
            payload.event_type_str(),
            "AGENT-06",
            &payload.to_json(),
            "request-m0",
            1,
        )
        .with_operation_id("llm_usage_request-m0")
        .with_schema_version(3)
    }

    fn provider_usage_binding() -> ProviderUsageBinding {
        ProviderUsageBinding {
            tenant_id: "tenant-m0".to_owned(),
            project_id: "project-m0".to_owned(),
            work_item_id: "build-site".to_owned(),
            reservation_id: "reservation-m0".to_owned(),
            assignment_id: "assignment-m0".to_owned(),
            assignment_version: 1,
            agent_id: AgentId(6),
            provider: "local-loop".to_owned(),
        }
    }

    fn project_with_provider_authority() -> sentinel_workflow::ProjectV1 {
        let digest = "a".repeat(64);
        let profile = sentinel_workflow::WorkProfileBindingV1 {
            profile_id: "web-project-v1".to_owned(),
            generation: 1,
            digest: digest.clone(),
        };
        let work_item_id = WorkItemId::parse("build-site").unwrap();
        let work_item = sentinel_workflow::CompanyWorkItemV1 {
            spec: sentinel_workflow::CompanyWorkItemSpecV1 {
                work_item_id: work_item_id.clone(),
                title: "Build site".to_owned(),
                objective: "Create the accepted site".to_owned(),
                required_role: CompanyRoleV1::Developer,
                required_specialties: BTreeSet::from(["frontend".to_owned()]),
                dependency_ids: BTreeSet::new(),
                owner: AgentId(6),
                inputs: Vec::new(),
                outputs: Vec::new(),
                quality_gate: sentinel_workflow::QualityGateBindingV1 {
                    gate_id: "web-qa".to_owned(),
                    generation: 1,
                    digest: digest.clone(),
                },
                budget_micros: 100,
                rework: None,
            },
            state: sentinel_workflow::CompanyWorkStateV1::Assigned,
            version: 1,
            assignments: vec![sentinel_workflow::AssignmentV1 {
                assignment_id: "assignment-m0".to_owned(),
                agent_id: AgentId(6),
                role: CompanyRoleV1::Developer,
                specialties: BTreeSet::from(["frontend".to_owned()]),
                profile: profile.clone(),
                organization_generation: 1,
                organization_digest: digest.clone(),
                assignment_version: 1,
                delegated_by: None,
                reason_ref: "project-plan".to_owned(),
                active: true,
                assigned_by: "pm-1".to_owned(),
                created_at_unix_ms: 2,
                ended_at_unix_ms: None,
            }],
            output_receipts: Vec::new(),
            gate_receipt: None,
            transition_history: Vec::new(),
        };
        sentinel_workflow::ProjectV1 {
            schema_version: sentinel_workflow::COMPANY_DOMAIN_SCHEMA_VERSION,
            tenant_id: TenantId::parse("tenant-m0").unwrap(),
            project_id: ProjectId::parse("project-m0").unwrap(),
            agreement_id: "agreement-m0".to_owned(),
            agreement_digest: digest.clone(),
            governance: sentinel_workflow::ProposalGovernanceV1 {
                owner: AgentId(6),
                participants: vec![sentinel_workflow::ParticipantBindingV1 {
                    agent_id: AgentId(6),
                    principal_id: "developer-6".to_owned(),
                    role: CompanyRoleV1::Developer,
                    specialties: BTreeSet::from(["frontend".to_owned()]),
                    reports_to: None,
                    profile: profile.clone(),
                }],
                project_profile: profile,
            },
            cost_ceiling_micros: 100,
            provider_cost_ceilings_micros: std::collections::BTreeMap::from([(
                "local-loop".to_owned(),
                100,
            )]),
            lifecycle_state: sentinel_workflow::ProjectLifecycleStateV1::Active,
            reserved_cost_micros: 0,
            committed_cost_micros: 0,
            work_items: std::collections::BTreeMap::from([(work_item_id.clone(), work_item)]),
            decisions: Vec::new(),
            handoffs: Vec::new(),
            blockers: Vec::new(),
            approvals: Vec::new(),
            reservations: vec![sentinel_workflow::CostReservationV1 {
                reservation_id: "reservation-m0".to_owned(),
                work_item_id: Some(work_item_id),
                provider: "local-loop".to_owned(),
                reserved_micros: 0,
                committed_micros: None,
                usage_event_operation_id: None,
                state: sentinel_workflow::CostReservationStateV1::Active,
                created_by: "pm-1".to_owned(),
                created_at_unix_ms: 3,
                updated_at_unix_ms: 3,
            }],
            rooms: Vec::new(),
            questions: Vec::new(),
            actions: Vec::new(),
            collaboration_schema_version: Some(sentinel_workflow::COLLABORATION_SCHEMA_VERSION),
            collaboration_sessions: Vec::new(),
            handoff_packets: Vec::new(),
            dissent_records: Vec::new(),
            decision_evidence: Vec::new(),
            collaboration_publications: Vec::new(),
            collaboration_generation: 1,
            collaboration_admissions: Vec::new(),
            collaboration_reliability: Vec::new(),
            version: 3,
            created_at_unix_ms: 1,
            updated_at_unix_ms: 3,
        }
    }

    #[test]
    fn provider_usage_authority_requires_one_exact_active_reservation() {
        let project = project_with_provider_authority();
        assert_eq!(
            select_provider_usage_binding(std::slice::from_ref(&project), AgentId(6)).unwrap(),
            Some(provider_usage_binding())
        );
        assert_eq!(
            select_provider_usage_binding(std::slice::from_ref(&project), AgentId(7)).unwrap(),
            None
        );

        let mut second = project;
        second.project_id = ProjectId::parse("project-other").unwrap();
        second.reservations[0].reservation_id = "reservation-other".to_owned();
        assert!(select_provider_usage_binding(
            &[project_with_provider_authority(), second],
            AgentId(6),
        )
        .is_err());

        let mut missing_reservation = project_with_provider_authority();
        missing_reservation.reservations.clear();
        assert_eq!(
            select_provider_usage_binding(&[missing_reservation], AgentId(6)),
            Err("assigned provider work item has no active reservation")
        );
    }

    #[test]
    fn full_project_read_requires_operator_or_exact_governed_leadership() {
        let mut project = project_with_provider_authority();
        let principals =
            PrincipalAuthenticator::new(vec![("d".repeat(32), principal_binding("developer-6"))])
                .unwrap();
        let developer = principals.principal("developer-6").unwrap().principal;
        assert!(!may_read_full_project(&project, &developer));

        let mut lead = developer.clone();
        lead.role = CompanyRoleV1::TechnicalLead;
        project.governance.participants[0].role = CompanyRoleV1::TechnicalLead;
        assert!(may_read_full_project(&project, &lead));

        lead.principal_id = "foreign-lead".to_owned();
        assert!(!may_read_full_project(&project, &lead));

        let mut operator = developer.clone();
        operator.kind = CompanyPrincipalKindV1::Operator;
        operator.role = CompanyRoleV1::Gaia;
        operator.agent_id = None;
        assert!(may_read_full_project(&project, &operator));

        project.collaboration_generation = 2;
        let outcome = sentinel_workflow::CompanyCommandOutcomeV1 {
            replayed: false,
            response: CompanyWorkflowResponseV1::Project(Box::new(project.clone())),
        };
        let redacted = company_command_response(&outcome, &developer);
        assert_eq!(redacted.status, 200);
        let redacted: serde_json::Value = serde_json::from_slice(&redacted.body).unwrap();
        assert!(redacted
            .pointer("/response/value/collaboration_generation")
            .is_none());

        let full = company_command_response(&outcome, &operator);
        assert_eq!(full.status, 200);
        let full: serde_json::Value = serde_json::from_slice(&full.body).unwrap();
        assert_eq!(
            full.pointer("/response/value/collaboration_generation"),
            Some(&serde_json::json!(2))
        );
    }

    #[test]
    fn provider_usage_event_is_exactly_bound_and_tampering_fails_closed() {
        let event = provider_usage_event();
        let binding = provider_usage_binding();
        assert_eq!(
            validate_provider_usage_event(&event, "llm_usage_request-m0", &binding, 0,),
            Ok(())
        );
        let mut wrong_provider = binding.clone();
        wrong_provider.provider = "anthropic-direct".to_owned();
        assert!(
            validate_provider_usage_event(&event, "llm_usage_request-m0", &wrong_provider, 0,)
                .is_err()
        );
        assert!(
            validate_provider_usage_event(&event, "llm_usage_request-m0", &binding, 1,).is_err()
        );

        let mut legacy = event.clone();
        legacy.schema_version = 2;
        assert!(
            validate_provider_usage_event(&legacy, "llm_usage_request-m0", &binding, 0,).is_err()
        );

        let mut wrong_caller = event;
        let DomainEventPayload::AgentLlmUsage { caller_role, .. } =
            serde_json::from_str::<DomainEventPayload>(&wrong_caller.payload).unwrap()
        else {
            panic!("usage payload expected")
        };
        assert_eq!(caller_role.as_deref(), Some("agent_runtime"));
        wrong_caller.payload = wrong_caller.payload.replace("agent_runtime", "operator");
        assert!(
            validate_provider_usage_event(&wrong_caller, "llm_usage_request-m0", &binding, 0,)
                .is_err()
        );
    }

    #[test]
    fn workbench_dispatch_error_preserves_only_proven_pre_dispatch_unavailability() {
        assert_eq!(
            map_workbench_dispatch_error(WorkbenchDispatchUnavailable.into()),
            WorkflowPortError::Unavailable
        );
        assert_eq!(
            map_workbench_dispatch_error(
                sentinel_common::nano_runtime::NanoExecError::new(
                    sentinel_common::nano_runtime::NanoExecErrorCode::ChannelDisconnected,
                    true,
                    "workbench runtime recycle remains pending",
                )
                .into(),
            ),
            WorkflowPortError::Unavailable
        );
        assert_eq!(
            map_workbench_dispatch_error(
                sentinel_common::nano_runtime::NanoExecError::new(
                    sentinel_common::nano_runtime::NanoExecErrorCode::ProtocolViolation,
                    false,
                    "workbench output failed its protocol binding",
                )
                .into(),
            ),
            WorkflowPortError::UnknownOutcome
        );
        assert_eq!(
            map_workbench_dispatch_error(anyhow::anyhow!("post-dispatch failure")),
            WorkflowPortError::UnknownOutcome
        );
        assert_eq!(
            map_workbench_dispatch_response::<()>(Err(mpsc::RecvTimeoutError::Timeout)),
            Err(WorkflowPortError::Unavailable)
        );
        assert_eq!(
            map_workbench_dispatch_response::<()>(Err(mpsc::RecvTimeoutError::Disconnected)),
            Err(WorkflowPortError::Unavailable)
        );
    }

    fn execution_step() -> sentinel_workflow::ExecutionStepV1 {
        sentinel_workflow::ExecutionStepV1 {
            step_id: Uuid::new_v4(),
            invocation_id: Uuid::new_v4(),
            ordinal: 0,
            workspace_id: "workspace-1".to_owned(),
            capabilities: BTreeSet::from(["file.inspect".to_owned()]),
            inputs: Vec::new(),
            command_policy: Vec::new(),
            tool: ExecutionToolV1::InspectFile {
                path: "README.md".to_owned(),
                max_bytes: 1024,
            },
            outputs: vec![sentinel_workflow::OutputExpectationV1 {
                name: "site".to_owned(),
                kind: "source_tree".to_owned(),
                required: true,
                digest_algorithm: "sha256".to_owned(),
            }],
            artifacts: Vec::new(),
            gate_expectation: sentinel_workflow::GateExpectationV1 {
                profile_id: "web-work-item-qa-v1".to_owned(),
                profile_generation: 1,
                profile_digest: "b".repeat(64),
                required_checks: BTreeSet::from(["html_structure".to_owned()]),
            },
            resource_bounds: sentinel_workflow::ExecutionResourceBoundsV1 {
                wall_time_ms: 1000,
                cpu_time_ms: 1000,
                memory_bytes: 1_048_576,
                process_count: 1,
                file_bytes: 4096,
                stdout_bytes: 4096,
                stderr_bytes: 4096,
            },
            deadline_unix_ms: 10_000,
        }
    }

    #[test]
    fn disabled_workflow_paths_fail_closed() {
        let api = WorkflowApi::disabled().unwrap();
        let response = api
            .handle("POST", AGENT_COMMAND_PATH, &HashMap::new(), br#"{}"#)
            .unwrap();
        assert_eq!(response.status, 503);
        assert!(!api.health().ready);
        assert_eq!(api.health().status, "disabled");
    }

    #[test]
    fn readiness_requires_a_successful_recovery_scan() {
        let mut api = WorkflowApi::disabled().unwrap();
        api.enabled = true;
        assert!(!api.health().ready);
        api.reconcile_pending();
        assert!(!api.health().ready);
        assert!(!api.health().dependencies_ready);
    }

    #[test]
    fn principal_authentication_is_bound_to_server_owned_credential() {
        let credential = "a".repeat(32);
        let principals = PrincipalAuthenticator::new(vec![(
            credential.clone(),
            principal_binding("developer-6"),
        )])
        .unwrap();
        let headers = HashMap::from([("authorization".to_owned(), format!("Bearer {credential}"))]);
        assert_eq!(
            principals
                .authenticate(&headers)
                .unwrap()
                .principal
                .agent_id,
            Some(AgentId(6))
        );
        let foreign = HashMap::from([(
            "authorization".to_owned(),
            format!("Bearer {}", "b".repeat(32)),
        )]);
        assert!(principals.authenticate(&foreign).is_none());
    }

    #[test]
    fn gaia_lineage_observation_is_idempotent_and_grants_no_mutation_role() {
        let directory = tempfile::tempdir().unwrap();
        let events =
            sentinel_limbo::EventStore::open(directory.path().join("events.db").to_str().unwrap())
                .unwrap();
        let mut api = WorkflowApi::disabled().unwrap();
        api.event_store = Some(events.clone());
        let principals = PrincipalAuthenticator::new(vec![(
            "g".repeat(32),
            PrincipalBinding {
                credential_name: "gaia-test".to_owned(),
                tenant_id: TenantId::parse("tenant-m0").unwrap(),
                principal_id: "gaia-9".to_owned(),
                kind: CompanyPrincipalKindV1::Agent,
                role: CompanyRoleV1::Gaia,
                customer_id: None,
                agent_id: Some(AgentId(9)),
                authority_generation: 4,
            },
        )])
        .unwrap();
        let gaia = principals.principal("gaia-9").unwrap();
        let delivery_principal = delivery_principal(&gaia.principal).unwrap();
        assert_eq!(
            delivery_principal.roles,
            BTreeSet::from([crate::delivery::AuthorityRole::GaiaObserver])
        );
        let lineage = crate::delivery::PublicDeliveryLineageDtoV1 {
            schema_version: crate::delivery::DELIVERY_SCHEMA_V1,
            server_redacted: true,
            project_label: "Project delivery".to_owned(),
            revision: 8,
            nodes: Vec::new(),
            edges: Vec::new(),
            blockers: Vec::new(),
            adapter_ready: true,
            authority_generation: 4,
            read_at_ms: 100,
        };

        api.record_gaia_oversight(&gaia, "tenant-m0", "project-m0", &lineage)
            .unwrap();
        let mut replay = lineage.clone();
        replay.read_at_ms = 200;
        api.record_gaia_oversight(&gaia, "tenant-m0", "project-m0", &replay)
            .unwrap();

        let event = events
            .event_by_operation_id("gaia-oversight:tenant-m0:project-m0:8:gaia-9:4")
            .unwrap()
            .unwrap();
        assert_eq!(event.event_type, "gaia_project_oversight_observed");
        assert!(matches!(
            serde_json::from_str::<DomainEventPayload>(&event.payload).unwrap(),
            DomainEventPayload::GaiaProjectOversightObserved {
                project_revision: 8,
                observer_authority_generation: 4,
                ..
            }
        ));
        assert_eq!(events.get_latest_event_id().unwrap(), 1);
    }

    #[test]
    fn evidence_derived_company_commands_are_not_public_http_commands() {
        let transition = CompanyWorkflowCommandV1::ApplyWorkTransition {
            project_id: ProjectId::parse("project-m0").unwrap(),
            expected_version: 1,
            receipt: sentinel_workflow::WorkTransitionReceiptV1 {
                schema_version: sentinel_workflow::COMPANY_DOMAIN_SCHEMA_VERSION,
                project_id: ProjectId::parse("project-m0").unwrap(),
                work_item_id: WorkItemId::parse("work-m0").unwrap(),
                expected_project_version: 1,
                expected_work_version: 1,
                expected_assignment_version: 1,
                from_state: sentinel_workflow::CompanyWorkStateV1::Assigned,
                to_state: sentinel_workflow::CompanyWorkStateV1::InProgress,
                output_receipts: Vec::new(),
                gate_receipt: None,
                phase_a_evidence_digest: "a".repeat(64),
                reason_ref: "sealed-workbench-evidence".to_string(),
                occurred_at_unix_ms: 1,
            },
        };
        let rework = CompanyWorkflowCommandV1::CreateGovernedRework {
            project_id: ProjectId::parse("project-m0").unwrap(),
            expected_version: 1,
            source_candidate_digest: "b".repeat(64),
            feedback_digest: "c".repeat(64),
            source_delivery_id: "delivery-m0".to_string(),
        };

        assert!(is_internal_company_command(&transition));
        assert!(is_internal_company_command(&rework));

        let reliability = CompanyWorkflowCommandV1::RecordCollaborationReliability {
            project_id: ProjectId::parse("project-m0").unwrap(),
            expected_version: 1,
            work_item_id: WorkItemId::parse("work-m0").unwrap(),
            fence: CollaborationAdmissionFenceV1 {
                organization_generation: 1,
                organization_digest: "a".repeat(64),
                assignment_id: "assignment-m0".to_owned(),
                assignment_version: 1,
                assignment_digest: "b".repeat(64),
                behavior_policy_generation: 1,
                behavior_policy_digest: "c".repeat(64),
                collaboration_generation: 1,
            },
            observation: sentinel_workflow::ReliabilityObservationV1 {
                observation_id: "observation-m0".to_owned(),
                agent_id: AgentId(6),
                capability: "frontend".to_owned(),
                task_family: "web-development".to_owned(),
                input_class: "static-site".to_owned(),
                claim_id: "claim-m0".to_owned(),
                accepted_outcome_digest: "d".repeat(64),
                independent_verification_digest: "e".repeat(64),
                verifier_principal_id: "qa-7".to_owned(),
                verifier_authority_digest: "f".repeat(64),
                accepted: true,
                calibration_bucket: 50,
                evidence_quality_micros: 500_000,
                policy_generation: 1,
                observation_digest: "1".repeat(64),
                recorded_at_unix_ms: 1,
            },
        };
        assert!(is_internal_company_command(&reliability));
    }

    fn admission_request(
        project: &sentinel_workflow::ProjectV1,
        _deadline_unix_ms: u64,
    ) -> CollaborationAdmissionRequest {
        CollaborationAdmissionRequest::Admit {
            project_id: project.project_id.clone(),
            work_item_id: WorkItemId::parse("build-site").unwrap(),
            expected_version: project.version,
            expected_benefit_ref: "solo owner covers the accepted task".to_owned(),
        }
    }

    #[test]
    fn public_admission_schema_cannot_inject_roster_or_authority() {
        let project = project_with_provider_authority();
        let envelope = CollaborationAdmissionEnvelope {
            operation_id: Uuid::from_u128(740),
            command: admission_request(&project, now_unix_ms().saturating_add(10_000)),
        };
        let mut value = serde_json::to_value(envelope).unwrap();
        let command = value
            .get_mut("command")
            .and_then(serde_json::Value::as_object_mut)
            .unwrap();
        assert!(!command.contains_key("candidates"));
        assert!(!command.contains_key("organization_generation"));
        assert!(!command.contains_key("assignment_digest"));
        assert!(!command.contains_key("collaboration_generation"));
        assert!(!command.contains_key("task_risk"));
        assert!(!command.contains_key("separation_requirements"));
        assert!(!command.contains_key("budget"));
        command.insert("candidates".to_owned(), serde_json::json!([]));
        assert!(serde_json::from_value::<CollaborationAdmissionEnvelope>(value).is_err());

        let mut value = serde_json::to_value(CollaborationAdmissionEnvelope {
            operation_id: Uuid::from_u128(741),
            command: admission_request(&project, now_unix_ms().saturating_add(10_000)),
        })
        .unwrap();
        value["command"]["task_risk"] = serde_json::json!("low");
        assert!(serde_json::from_value::<CollaborationAdmissionEnvelope>(value).is_err());

        let progress = CollaborationAdmissionEnvelope {
            operation_id: Uuid::from_u128(742),
            command: CollaborationAdmissionRequest::Progress {
                project_id: project.project_id.clone(),
                expected_version: project.version,
                admission_id: "admission-m0".to_owned(),
                progress: CollaborationProgressRequestV1 {
                    expected_transition_sequence: 1,
                    novelty_micros: 500_000,
                    novelty_digest: "d".repeat(64),
                    milestone_digest: Some("e".repeat(64)),
                    work_digest: None,
                    disposition: CollaborationProgressDispositionV1::Continue,
                    reason_ref: "bounded progress".to_owned(),
                },
            },
        };
        let mut value = serde_json::to_value(progress).unwrap();
        let progress = value["command"]["progress"].as_object_mut().unwrap();
        assert!(!progress.contains_key("rounds_delta"));
        assert!(!progress.contains_key("tokens_delta"));
        assert!(!progress.contains_key("cost_delta_micros"));
        progress.insert("tokens_delta".to_owned(), serde_json::json!(0));
        assert!(serde_json::from_value::<CollaborationAdmissionEnvelope>(value).is_err());
    }

    #[test]
    fn admission_tool_requirements_follow_the_candidate_role() {
        let authoring = BTreeSet::from([
            "file.inspect".to_owned(),
            "file.write".to_owned(),
            "patch.apply".to_owned(),
        ]);
        let qa = BTreeSet::from(["file.inspect".to_owned(), "test.run_profile".to_owned()]);

        assert_eq!(
            role_capabilities(CompanyRoleV1::Developer, &authoring, &qa),
            authoring
        );
        assert_eq!(role_capabilities(CompanyRoleV1::Qa, &authoring, &qa), qa);
        assert!(role_capabilities(CompanyRoleV1::ReleaseManager, &authoring, &qa).is_empty());

        let qa_without_test_runner = BTreeSet::from(["file.inspect".to_owned()]);
        assert!(!role_capabilities(CompanyRoleV1::Qa, &authoring, &qa)
            .is_subset(&qa_without_test_runner));
    }

    #[test]
    fn admission_derives_roster_load_and_fences_from_server_authority() {
        let project = project_with_provider_authority();
        let work = project.work_items.values().next().unwrap();
        let expected_data_provenance =
            collaboration_input_provenance_digest(&project, work).unwrap();
        let principals = Arc::new(
            PrincipalAuthenticator::new(vec![("d".repeat(32), principal_binding("developer-6"))])
                .unwrap(),
        );
        let profile = WorkbenchProfile {
            schema_version: 1,
            id: "web-authoring-v1".to_owned(),
            runtime_key: WORKBENCH_RUNTIME_BWRAP.to_owned(),
            network: "deny".to_owned(),
            environment: std::collections::BTreeMap::new(),
            capabilities: BTreeSet::from(["frontend".to_owned()]),
            output_artifact_kinds: BTreeSet::from(["source_tree".to_owned()]),
            resource_ceilings: WorkbenchResourceLimits {
                wall_time_ms: 30_000,
                cpu_time_ms: 10_000,
                memory_bytes: 134_217_728,
                process_count: 16,
                file_bytes: 8_388_608,
                stdout_bytes: 65_536,
                stderr_bytes: 65_536,
            },
            command_rules: Vec::new(),
            test_suites: Vec::new(),
        };
        let authority = CompanyAuthority {
            store: Arc::new(WorkflowStore::open(":memory:").unwrap()),
            principals,
            workbench_profile: profile,
            workbench_profile_digest: "a".repeat(64),
            qa_profile_capabilities: BTreeSet::from([
                "file.inspect".to_owned(),
                "test.run_profile".to_owned(),
            ]),
            agent_capabilities: Arc::new(HashMap::from([(
                AgentId(6),
                BTreeSet::from(["frontend".to_owned()]),
            )])),
            runtime_health: Arc::new(RwLock::new(crate::runtime_health::RuntimeHealthSnapshot {
                agents: vec![crate::runtime_health::RuntimeHealthAgentSnapshot {
                    agent_id: 6,
                    runtime_present: true,
                    projection_present: true,
                    security_runtime_present: true,
                    adapter_handle_present: true,
                    adapter_instance_matches: true,
                    runtime_resources_healthy: true,
                    adapter_health_state: Some(
                        sentinel_common::nano_runtime::NanoHealthState::Healthy,
                    ),
                    logical_status: Some(sentinel_runtime::AgentStatus::Active),
                    ..crate::runtime_health::RuntimeHealthAgentSnapshot::default()
                }],
                ..crate::runtime_health::RuntimeHealthSnapshot::default()
            })),
            artifact_roots: Arc::new(HashMap::new()),
        };
        let command = derive_collaboration_admission_command(
            &authority,
            &project,
            admission_request(&project, now_unix_ms().saturating_add(10_000)),
            "9".repeat(64),
        )
        .unwrap();
        let CompanyWorkflowCommandV1::AdmitCollaboration {
            input,
            candidates,
            reliability,
            expected_benefit_ref,
            ..
        } = command
        else {
            panic!("expected admission command")
        };
        assert_eq!(input.owner, AgentId(6));
        assert_eq!(input.task_family, "web-project-v1");
        assert_eq!(input.input_class, "developer");
        assert_eq!(input.task_risk, TaskRiskV1::Low);
        assert_eq!(input.reversibility, ReversibilityV1::Reversible);
        assert_eq!(input.ambiguity, AmbiguityClassV1::Low);
        assert_eq!(input.uncertainty, UncertaintyClassV1::Low);
        assert!(!input.evidence_conflict);
        assert!(!input.directed_handoff_required);
        assert!(input.required_handoff_agents.is_empty());
        assert!(!input.specialist_panel_required);
        assert!(input.separation_requirements.is_empty());
        assert_eq!(
            input.required_capabilities,
            BTreeSet::from(["frontend".to_owned()])
        );
        assert_eq!(input.assignment_id, "assignment-m0");
        assert_eq!(input.assignment_version, 1);
        assert_eq!(input.organization_generation, 1);
        assert_eq!(input.collaboration_generation, 1);
        assert_eq!(input.quality_tolerance_micros, 100_000);
        assert_eq!(input.budget.max_participants, 1);
        assert_eq!(input.budget.max_rounds, 4);
        assert_eq!(input.budget.max_tokens, 32_000);
        assert_eq!(input.budget.max_cost_micros, 100);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].agent_id, AgentId(6));
        assert_eq!(candidates[0].assignment_load, 0);
        assert!(candidates[0].runtime_available);
        assert!(candidates[0].tools_available);
        assert_eq!(
            candidates[0].data_provenance_digest,
            expected_data_provenance
        );
        assert!(reliability.is_empty());

        authority.runtime_health.write().unwrap().agents.clear();
        let unavailable = derive_collaboration_admission_command(
            &authority,
            &project,
            admission_request(&project, now_unix_ms().saturating_add(10_000)),
            "9".repeat(64),
        )
        .unwrap();
        let CompanyWorkflowCommandV1::AdmitCollaboration {
            candidates: unavailable_candidates,
            ..
        } = unavailable
        else {
            panic!("expected admission command")
        };
        assert!(!unavailable_candidates[0].active);
        assert!(!unavailable_candidates[0].runtime_available);

        let poisoned_health = Arc::clone(&authority.runtime_health);
        assert!(std::thread::spawn(move || {
            let _guard = poisoned_health.write().unwrap();
            panic!("poison collaboration runtime-health fixture");
        })
        .join()
        .is_err());
        let poisoned = derive_collaboration_admission_command(
            &authority,
            &project,
            admission_request(&project, now_unix_ms().saturating_add(10_000)),
            "9".repeat(64),
        )
        .unwrap_err();
        assert_eq!(poisoned.code, WorkflowErrorCode::PersistenceFailure);

        let mut decision = sentinel_workflow::admit_collaboration(
            "admission-m0".to_owned(),
            input,
            &candidates,
            &reliability,
            &std::collections::BTreeMap::new(),
            expected_benefit_ref,
            now_unix_ms(),
        )
        .unwrap();
        let view = serde_json::to_value(collaboration_participant_admission_view(
            &decision,
            AgentId(6),
        ))
        .unwrap();
        assert!(view.get("eligible_agents").is_none());
        assert!(view.get("rejected_candidates").is_none());
        assert!(view.get("reservations").is_none());
        assert!(view.get("input").is_none());
        assert_eq!(view["visible_agents"], serde_json::json!([6]));

        let operation_id = Uuid::from_u128(740);
        let request_digest = "9".repeat(64);
        decision
            .request_bindings
            .push(sentinel_workflow::CollaborationRequestBindingV1 {
                operation_id,
                request_digest: request_digest.clone(),
            });
        decision.refresh_digest().unwrap();
        let mut response_project = project.clone();
        response_project.collaboration_admissions.push(decision);
        let principal = authority
            .principals
            .principal("developer-6")
            .unwrap()
            .principal;
        let response = collaboration_admission_response(
            &project,
            &CompanyWorkflowResponseV1::Project(Box::new(response_project)),
            &principal,
            operation_id,
            &request_digest,
        );
        assert_eq!(response.status, 200);
        let body: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        assert!(body.get("eligible_agents").is_none());
        assert!(body.get("rejected_candidates").is_none());
        assert!(body.get("reservations").is_none());
        assert!(body.get("input").is_none());
        assert_eq!(body["visible_agents"], serde_json::json!([6]));
    }

    #[test]
    fn principal_binding_file_rejects_mutable_and_aliased_authority() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("company-principals.json");
        fs::write(&path, br#"{"schema_version":1,"bindings":[]}"#).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_principal_bindings_file(&path).is_ok());

        fs::set_permissions(&path, fs::Permissions::from_mode(0o664)).unwrap();
        assert!(read_principal_bindings_file(&path).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        let hardlink = directory.path().join("company-principals-hardlink.json");
        fs::hard_link(&path, &hardlink).unwrap();
        assert!(read_principal_bindings_file(&path).is_err());
        fs::remove_file(hardlink).unwrap();

        let symlink_path = directory.path().join("company-principals-symlink.json");
        symlink(&path, &symlink_path).unwrap();
        assert!(read_principal_bindings_file(&symlink_path).is_err());
    }

    #[test]
    fn completion_outputs_reject_ambiguous_shared_digest() {
        let mut step = execution_step();
        assert_eq!(map_sealed_outputs(&step, &"a".repeat(64)).unwrap().len(), 1);
        step.outputs.push(step.outputs[0].clone());
        assert_eq!(
            map_sealed_outputs(&step, &"a".repeat(64)),
            Err(WorkflowPortError::Rejected)
        );
    }

    #[test]
    fn execution_plan_contract_rejects_output_and_gate_rebinding_before_io() {
        let step = execution_step();
        let mut plan = ExecutionPlanV1 {
            schema_version: sentinel_workflow::EXECUTION_PLAN_SCHEMA_VERSION,
            plan_id: Uuid::new_v4(),
            tenant_id: TenantId::parse("tenant-m0").unwrap(),
            project_id: ProjectId::parse("project-m0").unwrap(),
            work_item_id: WorkItemId::parse("work-m0").unwrap(),
            agent_id: AgentId(6),
            workspace_id: "workspace-1".to_owned(),
            assignment_version: 1,
            assignment_digest: "a".repeat(64),
            organization_generation: 1,
            organization_digest: "b".repeat(64),
            principal: PrincipalAuthorityV1 {
                schema_version: WORKFLOW_SCHEMA_VERSION,
                principal_id: "developer-6".to_owned(),
                principal_generation: 1,
                authority_digest: "c".repeat(64),
            },
            profile_id: "web-authoring-v1".to_owned(),
            profile_generation: 1,
            profile_digest: "d".repeat(64),
            runtime_key: WORKBENCH_RUNTIME_BWRAP.to_owned(),
            runtime_generation: 1,
            runtime_digest: "e".repeat(64),
            policy_generation: 1,
            policy_digest: "f".repeat(64),
            created_at_unix_ms: 1,
            deadline_unix_ms: 10_000,
            steps: vec![step],
            request_digest: "9".repeat(64),
        };
        let mut spec = sentinel_workflow::CompanyWorkItemSpecV1 {
            work_item_id: plan.work_item_id.clone(),
            title: "Site".to_owned(),
            objective: "Build site".to_owned(),
            required_role: CompanyRoleV1::Developer,
            required_specialties: BTreeSet::from(["web_development".to_owned()]),
            dependency_ids: BTreeSet::new(),
            owner: AgentId(5),
            inputs: Vec::new(),
            outputs: vec![sentinel_workflow::WorkOutputContractV1 {
                name: "site".to_owned(),
                media_type: "application/vnd.sentinel.source-tree".to_owned(),
                digest_algorithm: "sha256".to_owned(),
                contract_generation: 1,
                contract_digest: "1".repeat(64),
            }],
            quality_gate: sentinel_workflow::QualityGateBindingV1 {
                gate_id: "web-work-item-qa-v1".to_owned(),
                generation: 1,
                digest: "b".repeat(64),
            },
            budget_micros: 1,
            rework: None,
        };
        assert_eq!(validate_execution_contract(&plan, &spec), Ok(()));

        let producer_id = WorkItemId::parse("design-m0").unwrap();
        spec.dependency_ids.insert(producer_id.clone());
        spec.inputs.push(sentinel_workflow::WorkInputContractV1 {
            name: "design_specification".to_owned(),
            producer_work_item_id: producer_id,
            producer_output_name: "design_specification".to_owned(),
            expected_contract_generation: 1,
            expected_contract_digest: "7".repeat(64),
        });
        assert_eq!(
            validate_execution_contract(&plan, &spec),
            Err(WorkflowPortError::AuthorityConflict)
        );
        plan.steps[0].inputs.push(ArtifactInputV1 {
            artifact_id: format!("sha256:{}", "6".repeat(64)),
            digest: "6".repeat(64),
            media_type: "text/markdown".to_owned(),
            mount_path: "design.md".to_owned(),
        });
        assert_eq!(validate_execution_contract(&plan, &spec), Ok(()));

        plan.steps[0].outputs[0].name = "foreign-output".to_owned();
        assert_eq!(
            validate_execution_contract(&plan, &spec),
            Err(WorkflowPortError::AuthorityConflict)
        );
        plan.steps[0].outputs[0].name = "site".to_owned();
        plan.steps[0].gate_expectation.profile_digest = "8".repeat(64);
        assert_eq!(
            validate_execution_contract(&plan, &spec),
            Err(WorkflowPortError::AuthorityConflict)
        );
    }

    #[test]
    fn execution_intent_derives_all_internal_authority_and_stable_effect_ids() {
        let operation_id = Uuid::parse_str("018f3f32-4f01-7f2c-a6c1-f6f4a81b2809").unwrap();
        let authority = RuntimeAuthoritySnapshotV1 {
            schema_version: WORKFLOW_SCHEMA_VERSION,
            tenant_id: TenantId::parse("tenant-m0").unwrap(),
            project_id: ProjectId::parse("project-m0").unwrap(),
            work_item_id: WorkItemId::parse("work-source").unwrap(),
            agent_id: AgentId(6),
            assignment_version: 3,
            assignment_digest: "a".repeat(64),
            organization_generation: 7,
            organization_digest: "b".repeat(64),
            principal: PrincipalAuthorityV1 {
                schema_version: WORKFLOW_SCHEMA_VERSION,
                principal_id: "agent-06-developer".to_owned(),
                principal_generation: 2,
                authority_digest: "c".repeat(64),
            },
            profile_id: "web-authoring-v1".to_owned(),
            profile_generation: 1,
            profile_digest: "d".repeat(64),
            runtime_key: WORKBENCH_RUNTIME_BWRAP.to_owned(),
            runtime_generation: 1,
            runtime_digest: "e".repeat(64),
            policy_generation: 1,
            policy_digest: "f".repeat(64),
            active: true,
            capabilities: BTreeSet::from([
                "artifact.commit".to_owned(),
                "command.run_allowlisted".to_owned(),
                "file.write".to_owned(),
            ]),
        };
        let profile = WorkbenchProfile {
            schema_version: 1,
            id: "web-authoring-v1".to_owned(),
            runtime_key: WORKBENCH_RUNTIME_BWRAP.to_owned(),
            network: "deny".to_owned(),
            environment: std::collections::BTreeMap::new(),
            capabilities: authority.capabilities.clone(),
            output_artifact_kinds: BTreeSet::from(["source_tree".to_owned()]),
            resource_ceilings: WorkbenchResourceLimits {
                wall_time_ms: 30_000,
                cpu_time_ms: 10_000,
                memory_bytes: 134_217_728,
                process_count: 16,
                file_bytes: 8_388_608,
                stdout_bytes: 65_536,
                stderr_bytes: 65_536,
            },
            command_rules: vec![CommandRule {
                program: "node".to_owned(),
                required_arg_prefix: vec!["--check".to_owned()],
                max_args: 2,
            }],
            test_suites: Vec::new(),
        };
        let spec = sentinel_workflow::CompanyWorkItemSpecV1 {
            work_item_id: authority.work_item_id.clone(),
            title: "Website source".to_owned(),
            objective: "Build the accepted static website".to_owned(),
            required_role: CompanyRoleV1::Developer,
            required_specialties: BTreeSet::from(["web_development".to_owned()]),
            dependency_ids: BTreeSet::new(),
            owner: AgentId(5),
            inputs: Vec::new(),
            outputs: vec![sentinel_workflow::WorkOutputContractV1 {
                name: "site".to_owned(),
                media_type: "application/vnd.sentinel.source-tree".to_owned(),
                digest_algorithm: "sha256".to_owned(),
                contract_generation: 1,
                contract_digest: "1".repeat(64),
            }],
            quality_gate: sentinel_workflow::QualityGateBindingV1 {
                gate_id: "web-work-item-qa-v1".to_owned(),
                generation: 1,
                digest: "2".repeat(64),
            },
            budget_micros: 100,
            rework: None,
        };
        let intent = ExecutionIntentV1 {
            project_id: authority.project_id.clone(),
            work_item_id: authority.work_item_id.clone(),
            tools: vec![
                ExecutionToolV1::WriteFile {
                    path: "index.js".to_owned(),
                    content: "console.log('sentinel');".to_owned(),
                    expected_sha256: None,
                },
                ExecutionToolV1::RunCommand {
                    program: "node".to_owned(),
                    args: vec!["--check".to_owned(), "index.js".to_owned()],
                },
                ExecutionToolV1::PackageArtifact {
                    artifact_kind: "source_tree".to_owned(),
                    media_type: "application/vnd.sentinel.source-tree".to_owned(),
                    paths: vec!["index.js".to_owned()],
                },
            ],
        };

        let deadline_unix_ms = execution_deadline_unix_ms(
            1_000,
            profile.resource_ceilings.wall_time_ms,
            intent.tools.len(),
        )
        .unwrap();
        assert_eq!(deadline_unix_ms, 311_000);

        let plan = build_execution_plan(
            operation_id,
            &authority,
            &spec,
            Vec::new(),
            &profile,
            &intent,
            1_000,
            deadline_unix_ms,
        )
        .unwrap();
        let replay = build_execution_plan(
            operation_id,
            &authority,
            &spec,
            Vec::new(),
            &profile,
            &intent,
            1_000,
            deadline_unix_ms,
        )
        .unwrap();
        assert_eq!(plan, replay);
        assert_eq!(plan.plan_id, operation_id);
        assert_eq!(plan.assignment_digest, authority.assignment_digest);
        assert_eq!(plan.policy_digest, authority.policy_digest);
        assert_eq!(plan.steps.len(), 3);
        assert_eq!(plan.steps[1].command_policy[0].max_args, 2);
        assert_eq!(
            plan.steps[1].command_policy[0].required_arg_prefix,
            vec!["--check", "index.js"]
        );
        assert!(plan.steps[..2].iter().all(|step| step.artifacts.is_empty()));
        assert_eq!(plan.steps[2].artifacts[0].artifact_kind, "source_tree");
        assert!(execution_intent_matches_plan(&intent, &plan));
        plan.validate_at(2_000).unwrap();
        plan.validate_at(1_000 + EXECUTION_RESTART_RECOVERY_ALLOWANCE_MS)
            .unwrap();
        assert!(plan.validate_at(deadline_unix_ms).is_err());

        let mut foreign = intent;
        foreign.tools[2] = ExecutionToolV1::PackageArtifact {
            artifact_kind: "foreign_artifact".to_owned(),
            media_type: "application/vnd.sentinel.source-tree".to_owned(),
            paths: vec!["index.js".to_owned()],
        };
        assert!(!execution_intent_matches_plan(&foreign, &plan));
        assert_eq!(
            build_execution_plan(
                operation_id,
                &authority,
                &spec,
                Vec::new(),
                &profile,
                &foreign,
                1_000,
                deadline_unix_ms,
            )
            .unwrap_err()
            .code,
            WorkflowErrorCode::InvalidInput
        );
    }

    #[test]
    fn independent_gate_evidence_is_the_only_path_from_review_to_done() {
        use sentinel_workflow::{CompanyWorkStateV1, GateEvidenceReadbackV1, WorkItemState};

        assert_eq!(
            company_transition_target(WorkItemState::Done, CompanyWorkStateV1::InReview).unwrap(),
            Some(CompanyWorkStateV1::Done)
        );
        assert_eq!(
            company_transition_target(WorkItemState::Done, CompanyWorkStateV1::InProgress).unwrap(),
            Some(CompanyWorkStateV1::InReview)
        );
        assert_eq!(
            company_transition_target(WorkItemState::Done, CompanyWorkStateV1::Assigned).unwrap(),
            Some(CompanyWorkStateV1::InProgress)
        );
        assert_eq!(
            company_transition_target(WorkItemState::Assigned, CompanyWorkStateV1::Assigned)
                .unwrap(),
            None
        );
        assert_eq!(
            company_transition_target(WorkItemState::Blocked, CompanyWorkStateV1::InProgress)
                .unwrap(),
            Some(CompanyWorkStateV1::Blocked)
        );
        assert_eq!(
            company_transition_target(WorkItemState::Blocked, CompanyWorkStateV1::InReview)
                .unwrap(),
            Some(CompanyWorkStateV1::Blocked)
        );
        assert!(
            company_transition_target(WorkItemState::InProgress, CompanyWorkStateV1::Done).is_err()
        );
        assert!(company_gate_receipt(CompanyWorkStateV1::Done, None).is_err());

        let evidence = GateEvidenceReadbackV1 {
            schema_version: WORKFLOW_SCHEMA_VERSION,
            receipt_id: "gate-receipt-1".to_owned(),
            profile_id: "web-work-item-qa-v1".to_owned(),
            profile_generation: 3,
            profile_digest: "a".repeat(64),
            subject_digest: "b".repeat(64),
            required_checks_digest: "c".repeat(64),
            passed: true,
            completed_at_unix_ms: 100,
        };
        let receipt = company_gate_receipt(CompanyWorkStateV1::Done, Some(&evidence))
            .unwrap()
            .unwrap();
        assert_eq!(receipt.gate_id, evidence.profile_id);
        assert_eq!(receipt.generation, evidence.profile_generation);
        assert_eq!(receipt.gate_digest, evidence.profile_digest);
        assert_eq!(receipt.subject_digest, evidence.subject_digest);
        assert!(receipt.passed);
    }

    #[test]
    fn gate_transition_selects_independent_qa_before_release_manager() {
        let binding = |principal_id: &str, role: CompanyRoleV1, agent_id: u16| PrincipalBinding {
            credential_name: format!("credential-{agent_id}"),
            tenant_id: TenantId::parse("tenant-m0").unwrap(),
            principal_id: principal_id.to_owned(),
            kind: CompanyPrincipalKindV1::Agent,
            role,
            customer_id: None,
            agent_id: Some(AgentId(agent_id)),
            authority_generation: 1,
        };
        let principals = PrincipalAuthenticator::new(vec![
            ("q".repeat(32), binding("qa-7", CompanyRoleV1::Qa, 7)),
            (
                "r".repeat(32),
                binding("release-8", CompanyRoleV1::ReleaseManager, 8),
            ),
        ])
        .unwrap();
        let participant = |principal_id: &str, role: CompanyRoleV1, agent_id: u16| {
            sentinel_workflow::ParticipantBindingV1 {
                agent_id: AgentId(agent_id),
                principal_id: principal_id.to_owned(),
                role,
                specialties: BTreeSet::from(["quality".to_owned()]),
                reports_to: None,
                profile: sentinel_workflow::WorkProfileBindingV1 {
                    profile_id: "web-authoring-v1".to_owned(),
                    generation: 1,
                    digest: "d".repeat(64),
                },
            }
        };
        let participants = vec![
            participant("release-8", CompanyRoleV1::ReleaseManager, 8),
            participant("qa-7", CompanyRoleV1::Qa, 7),
        ];

        let selected = independent_gate_principal(&participants, AgentId(6), &principals).unwrap();
        assert_eq!(selected.principal.principal_id, "qa-7");
        assert_eq!(selected.principal.role, CompanyRoleV1::Qa);
        assert!(independent_gate_principal(&participants, AgentId(7), &principals).is_ok());
        assert!(independent_gate_principal(&participants[..1], AgentId(8), &principals).is_err());
    }
}
