//! Authenticated M0 company workflow and productive Workbench integration.

mod delivery_runtime;

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::time::Duration;

use sentinel_common::{
    events::{DomainEvent, DomainEventPayload},
    AgentId, CommandRule, WorkbenchArtifactRef, WorkbenchRequest, WorkbenchResourceLimits,
    WorkbenchTool, WORKBENCH_RUNTIME_BWRAP, WORKBENCH_SCHEMA_VERSION,
};
use sentinel_workflow::{
    sealed_output_bundle_digest, ArtifactInputV1, AuthenticatedCompanyPrincipalV1, CommandRuleV1,
    CompanyPrincipalKindV1, CompanyRoleV1, CompanyWorkflowCommandV1, CompletionEvidencePort,
    DependencyReadiness, ExecutionPlanV1, ExecutionReconcileState, ExecutionToolV1,
    GateEvidencePort, PendingCompletionEvidenceV1, PendingExecutionV1, PrincipalAuthorityV1,
    ProjectId, RuntimeAuthoritySnapshotV1, SealedArtifactEvidenceV1, SealedOutputEvidenceV1,
    TenantId, TerminalExecutionEvidence, UnavailableGateEvidencePort, WorkExecutionObservation,
    WorkExecutionPort, WorkItemId, WorkflowCore, WorkflowError, WorkflowErrorCode,
    WorkflowPortError, WorkflowStore, WORKFLOW_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::delivery::{ConfiguredDeliveryCore, DeliveryStoreConfigV1};
use crate::workbench::{
    dispatch_workbench, WorkbenchAuthoritySnapshot, WorkbenchAuthoritySource,
    WorkbenchDispatchCommand, WorkbenchInvocationRecord, WorkbenchInvocationState,
    WorkbenchProfile,
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
pub const DELIVERY_LINEAGE_PATH: &str = "/company/delivery/lineage";
pub const MAX_WORKFLOW_BODY_BYTES: usize = 256 * 1024;

const PRINCIPAL_SCHEMA_VERSION: u16 = 1;
const PROFILE_GENERATION: u64 = 1;
const RUNTIME_GENERATION: u64 = 1;
const DISPATCH_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RECONCILE_BATCH: usize = 32;
const MAX_PRINCIPAL_BINDINGS_BYTES: u64 = 1024 * 1024;
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
    agent_capabilities: Arc<HashMap<AgentId, BTreeSet<String>>>,
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
        if assignment.agent_id != agent_id
            || participant.role != assignment.role
            || principal.principal.agent_id != Some(agent_id)
            || principal.principal.role != assignment.role
            || principal.principal.tenant_id != *tenant_id
            || assignment.profile.profile_id != self.workbench_profile.id
            || assignment.profile.digest != self.workbench_profile_digest
            || assignment.profile.generation != PROFILE_GENERATION
            || project.governance.project_profile.profile_id != "web-project-v1"
            || !matches!(
                work.state,
                sentinel_workflow::CompanyWorkStateV1::Assigned
                    | sentinel_workflow::CompanyWorkStateV1::InProgress
                    | sentinel_workflow::CompanyWorkStateV1::InReview
            )
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
        let role_capabilities = role_capabilities(role, &self.workbench_profile);
        Ok(WorkbenchAuthoritySnapshot {
            agent_id,
            caller_id: runtime.principal.principal_id,
            caller_role: role_name(role).to_owned(),
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
    if final_step.outputs.len() != spec.outputs.len()
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

fn role_capabilities(role: CompanyRoleV1, profile: &WorkbenchProfile) -> BTreeSet<String> {
    if matches!(role, CompanyRoleV1::Designer | CompanyRoleV1::Developer) {
        profile.capabilities.clone()
    } else {
        BTreeSet::new()
    }
}

fn role_name(role: CompanyRoleV1) -> &'static str {
    match role {
        CompanyRoleV1::Customer => "customer",
        CompanyRoleV1::Sales => "sales",
        CompanyRoleV1::ProjectManager => "project_manager",
        CompanyRoleV1::TechnicalLead => "technical_lead",
        CompanyRoleV1::Designer => "designer",
        CompanyRoleV1::Developer => "developer",
        CompanyRoleV1::Qa => "qa",
        CompanyRoleV1::ReleaseManager => "release_manager",
        CompanyRoleV1::Gaia => "gaia",
    }
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
            caller_role: role_name(caller.principal.role).to_owned(),
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
        match receiver.recv_timeout(DISPATCH_RESPONSE_TIMEOUT) {
            Ok(Ok(update)) => Ok(update),
            Ok(Err(_)) => Err(WorkflowPortError::UnknownOutcome),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(WorkflowPortError::UnknownOutcome),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(WorkflowPortError::Unavailable),
        }
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionPlanEnvelope {
    operation_id: Uuid,
    plan: ExecutionPlanV1,
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
        let agent_capabilities = Arc::new(agent_capabilities);
        let authority = Arc::new(CompanyAuthority {
            store: Arc::clone(&store),
            principals: Arc::clone(&principals),
            workbench_profile: profile,
            workbench_profile_digest: profile_digest,
            agent_capabilities: Arc::clone(&agent_capabilities),
        });
        let workbench = Arc::new(WorkbenchExecutionAdapter {
            store: Arc::clone(&store),
            authority: Arc::clone(&authority),
        });
        let organization: Arc<dyn sentinel_workflow::OrganizationRuntimePort> = authority.clone();
        let execution: Arc<dyn WorkExecutionPort> = workbench.clone();
        let completion: Arc<dyn CompletionEvidencePort> = workbench;
        let (qa_profile, qa_profile_digest) =
            WorkbenchProfile::load(config_dir.join("workbench-profiles/web-qa-v1.toml"))
                .map_err(|_| workflow_unavailable())?;
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
            ("GET", DELIVERY_LINEAGE_PATH) => self.delivery_lineage(&principal, path),
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
        match self.core.apply_company_command(
            &principal.principal,
            envelope.operation_id,
            &envelope.command,
            now_unix_ms(),
        ) {
            Ok(value) => json(200, &value),
            Err(error) => workflow_error(error),
        }
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
            Ok(Some(value)) => json(200, &value),
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
            Ok(Some(value)) => json(200, &value),
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
        match self
            .store
            .company_project_events_since(&principal.principal.tenant_id, after, limit)
        {
            Ok(value) => json(200, &value),
            Err(error) => workflow_error(error),
        }
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
            let principal = if target == sentinel_workflow::CompanyWorkStateV1::Done {
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
                reason_ref: match target {
                    sentinel_workflow::CompanyWorkStateV1::Done => {
                        "independent-quality-gate-evidence"
                    }
                    sentinel_workflow::CompanyWorkStateV1::Blocked => "workbench-execution-blocked",
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
        let dependencies_ready = self.core.dependencies_ready() && delivery_ready;
        let ready = self.enabled
            && dependencies_ready
            && self.scan_succeeded.load(Ordering::Acquire)
            && last_error.is_none()
            && pending_execution.is_ok()
            && pending_completion.is_ok()
            && pending_gate.is_ok()
            && delivery_publication_pending == Some(0)
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
            last_error,
        }
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
            | sentinel_workflow::CompanyWorkStateV1::InProgress,
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
            | DELIVERY_LINEAGE_PATH
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
        let spec = sentinel_workflow::CompanyWorkItemSpecV1 {
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
