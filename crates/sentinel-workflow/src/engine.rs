use std::sync::Arc;

use crate::digest::constant_time_eq;
use crate::model::{sealed_output_bundle_digest, validate_digest, validate_identifier};
use crate::{
    CompletionEvidencePort, DependencyReadiness, ExecutionEvidenceReadbackV1,
    ExecutionReconcileState, GateEvidencePort, GateEvidenceReadbackV1, OrganizationRuntimePort,
    PendingCompletionEvidenceV1, PendingExecutionV1, PendingGateEvidenceV1,
    RuntimeAuthoritySnapshotV1, WorkExecutionObservation, WorkExecutionPort, WorkflowError,
    WorkflowErrorCode, WorkflowPortError, WorkflowStore, WORKFLOW_SCHEMA_VERSION,
};

pub struct WorkflowCore<O, E, C, G> {
    store: Arc<WorkflowStore>,
    organization: O,
    execution: E,
    completion: C,
    gate: G,
}

impl<O, E, C, G> WorkflowCore<O, E, C, G>
where
    O: OrganizationRuntimePort,
    E: WorkExecutionPort,
    C: CompletionEvidencePort,
    G: GateEvidencePort,
{
    pub fn new(
        store: impl Into<Arc<WorkflowStore>>,
        organization: O,
        execution: E,
        completion: C,
        gate: G,
    ) -> Self {
        Self {
            store: store.into(),
            organization,
            execution,
            completion,
            gate,
        }
    }

    pub fn store(&self) -> &WorkflowStore {
        &self.store
    }

    pub fn dependencies_ready(&self) -> bool {
        [
            self.organization.readiness(),
            self.execution.readiness(),
            self.completion.readiness(),
            self.gate.readiness(),
        ]
        .into_iter()
        .all(|readiness| readiness == DependencyReadiness::Ready)
    }

    pub fn apply_company_command(
        &self,
        principal: &crate::AuthenticatedCompanyPrincipalV1,
        operation_id: uuid::Uuid,
        command: &crate::CompanyWorkflowCommandV1,
        now_ms: u64,
    ) -> Result<crate::CompanyCommandOutcomeV1, WorkflowError> {
        self.store
            .apply_company_command(principal, operation_id, command, now_ms)
    }

    pub fn admit_plan(
        &self,
        plan: &crate::ExecutionPlanV1,
        now_ms: u64,
    ) -> Result<(bool, crate::WorkItemExecutionV1), WorkflowError> {
        plan.validate_canonical()?;
        require_ready(
            self.organization.readiness(),
            WorkflowErrorCode::OrganizationUnavailable,
        )?;
        let authority = self.authority_for_plan(plan)?;
        self.store.admit_plan(plan, &authority, now_ms)
    }

    pub fn reconcile_execution(
        &self,
        request: &PendingExecutionV1,
        now_ms: u64,
    ) -> Result<crate::WorkItemExecutionV1, WorkflowError> {
        require_ready(
            self.organization.readiness(),
            WorkflowErrorCode::OrganizationUnavailable,
        )?;
        require_ready(
            self.execution.readiness(),
            WorkflowErrorCode::ExecutionUnavailable,
        )?;
        let work_item = self.store.execution_work_item(request)?;
        let authority_before = self.authority_for_work_item(&work_item)?;
        validate_pre_io_authority(
            &work_item,
            &authority_before,
            &request.authority_snapshot_digest,
            request.updated_at_unix_ms,
            now_ms,
        )?;
        if now_ms >= request.step.deadline_unix_ms || now_ms >= work_item.plan.deadline_unix_ms {
            return self.store.record_execution_observation(
                request,
                ExecutionReconcileState::TimedOut,
                &authority_before,
                now_ms,
            );
        }
        let observation = match self.execution.reconcile(request) {
            Ok(observation) => observation,
            Err(WorkflowPortError::UnknownOutcome) => {
                let authority = self.authority_for_work_item(&work_item)?;
                ensure_same_authority(&authority_before, &authority)?;
                validate_pre_io_authority(
                    &work_item,
                    &authority,
                    &request.authority_snapshot_digest,
                    request.updated_at_unix_ms,
                    now_ms,
                )?;
                return self.store.record_execution_observation(
                    request,
                    ExecutionReconcileState::UnknownOutcome,
                    &authority,
                    now_ms,
                );
            }
            Err(error) => return Err(map_execution_error(error)),
        };
        let authority = self.authority_for_work_item(&work_item)?;
        ensure_same_authority(&authority_before, &authority)?;
        validate_pre_io_authority(
            &work_item,
            &authority,
            &request.authority_snapshot_digest,
            request.updated_at_unix_ms,
            now_ms,
        )?;
        let state = match observation {
            WorkExecutionObservation::NotFound => ExecutionReconcileState::NotFound,
            WorkExecutionObservation::Reserved => ExecutionReconcileState::Reserved,
            WorkExecutionObservation::Executing => ExecutionReconcileState::Executing,
            WorkExecutionObservation::Succeeded => ExecutionReconcileState::Succeeded,
            WorkExecutionObservation::Failed => ExecutionReconcileState::Failed,
            WorkExecutionObservation::Cancelled => ExecutionReconcileState::Cancelled,
            WorkExecutionObservation::TimedOut => ExecutionReconcileState::TimedOut,
            WorkExecutionObservation::UnknownOutcome => ExecutionReconcileState::UnknownOutcome,
        };
        self.store
            .record_execution_observation(request, state, &authority, now_ms)
    }

    pub fn reconcile_completion_evidence(
        &self,
        request: &PendingCompletionEvidenceV1,
        now_ms: u64,
    ) -> Result<crate::WorkItemExecutionV1, WorkflowError> {
        require_ready(
            self.organization.readiness(),
            WorkflowErrorCode::OrganizationUnavailable,
        )?;
        require_ready(
            self.completion.readiness(),
            WorkflowErrorCode::CompletionUnavailable,
        )?;
        let (work_item, completed) = self.store.completion_work_item(request)?;
        if completed {
            return Ok(work_item);
        }
        let authority_before = self.authority_for_work_item(&work_item)?;
        validate_pre_io_authority(
            &work_item,
            &authority_before,
            &request.authority_snapshot_digest,
            request.created_at_unix_ms,
            now_ms,
        )?;
        let receipt = self
            .completion
            .terminal_evidence(request)
            .map_err(map_completion_error)?;
        let authority = self.authority_for_work_item(&work_item)?;
        ensure_same_authority(&authority_before, &authority)?;
        validate_pre_io_authority(
            &work_item,
            &authority,
            &request.authority_snapshot_digest,
            request.created_at_unix_ms,
            now_ms,
        )?;
        if receipt.schema_version() != WORKFLOW_SCHEMA_VERSION
            || receipt.invocation_id() != request.invocation_id
            || receipt.plan_digest() != request.plan_digest
            || receipt.step_digest() != request.step_digest
            || receipt.completed_at_unix_ms() < request.created_at_unix_ms
            || receipt.completed_at_unix_ms() > now_ms
        {
            return Err(authority_conflict());
        }
        validate_identifier(receipt.receipt_id())?;
        validate_digest(receipt.output_bundle_digest())?;
        let step = work_item
            .plan
            .steps
            .iter()
            .find(|step| step.step_id == request.step_id)
            .ok_or_else(authority_conflict)?;
        if receipt.completed_at_unix_ms() > step.deadline_unix_ms
            || receipt.completed_at_unix_ms() > work_item.plan.deadline_unix_ms
            || receipt.outputs().len() != step.outputs.len()
            || receipt.artifacts().len() != step.artifacts.len()
        {
            return Err(authority_conflict());
        }
        for (expected, observed) in step.outputs.iter().zip(receipt.outputs()) {
            if observed.name != expected.name
                || observed.kind != expected.kind
                || observed.digest_algorithm != expected.digest_algorithm
            {
                return Err(authority_conflict());
            }
            validate_digest(&observed.digest)?;
        }
        for (expected, observed) in step.artifacts.iter().zip(receipt.artifacts()) {
            if observed.artifact_kind != expected.artifact_kind
                || observed.media_type != expected.media_type
                || observed.paths != expected.required_paths
            {
                return Err(authority_conflict());
            }
            validate_digest(&observed.digest)?;
        }
        let bundle_digest = sealed_output_bundle_digest(receipt.outputs(), receipt.artifacts())?;
        if !constant_time_eq(&bundle_digest, receipt.output_bundle_digest()) {
            return Err(authority_conflict());
        }
        let evidence = ExecutionEvidenceReadbackV1::new(
            receipt.receipt_id().to_owned(),
            receipt.invocation_id(),
            receipt.plan_digest().to_owned(),
            receipt.step_digest().to_owned(),
            receipt.output_bundle_digest().to_owned(),
            receipt.outputs().to_vec(),
            receipt.artifacts().to_vec(),
            receipt.completed_at_unix_ms(),
        );
        self.store
            .record_terminal_evidence(request, evidence, &authority, now_ms)
    }

    pub fn reconcile_gate_evidence(
        &self,
        request: &PendingGateEvidenceV1,
        now_ms: u64,
    ) -> Result<crate::WorkItemExecutionV1, WorkflowError> {
        require_ready(
            self.organization.readiness(),
            WorkflowErrorCode::OrganizationUnavailable,
        )?;
        require_ready(self.gate.readiness(), WorkflowErrorCode::GateUnavailable)?;
        let (work_item, completed) = self.store.gate_work_item(request)?;
        if completed {
            return Ok(work_item);
        }
        let authority_before = self.authority_for_work_item(&work_item)?;
        validate_pre_io_authority(
            &work_item,
            &authority_before,
            &request.authority_snapshot_digest,
            request.created_at_unix_ms,
            now_ms,
        )?;
        let receipt = self.gate.gate_evidence(request).map_err(map_gate_error)?;
        let authority = self.authority_for_work_item(&work_item)?;
        ensure_same_authority(&authority_before, &authority)?;
        validate_pre_io_authority(
            &work_item,
            &authority,
            &request.authority_snapshot_digest,
            request.created_at_unix_ms,
            now_ms,
        )?;
        if receipt.schema_version() != WORKFLOW_SCHEMA_VERSION
            || receipt.profile_id() != request.expectation.profile_id
            || receipt.profile_generation() != request.expectation.profile_generation
            || receipt.profile_digest() != request.expectation.profile_digest
            || receipt.subject_digest() != request.subject_digest
            || receipt.required_checks_digest() != request.required_checks_digest
            || !receipt.passed()
            || receipt.completed_at_unix_ms() < request.created_at_unix_ms
            || receipt.completed_at_unix_ms() > now_ms
            || receipt.completed_at_unix_ms() > work_item.plan.deadline_unix_ms
        {
            return Err(authority_conflict());
        }
        validate_identifier(receipt.receipt_id())?;
        let evidence = GateEvidenceReadbackV1::new(
            receipt.receipt_id().to_owned(),
            receipt.profile_generation(),
            receipt.profile_digest().to_owned(),
            receipt.subject_digest().to_owned(),
            receipt.required_checks_digest().to_owned(),
            receipt.completed_at_unix_ms(),
        );
        self.store
            .record_gate_evidence(request, evidence, &authority, now_ms)
    }

    fn authority_for_plan(
        &self,
        plan: &crate::ExecutionPlanV1,
    ) -> Result<RuntimeAuthoritySnapshotV1, WorkflowError> {
        self.organization
            .authority_snapshot(
                &plan.tenant_id,
                &plan.project_id,
                &plan.work_item_id,
                plan.agent_id,
            )
            .map_err(map_organization_error)
    }

    fn authority_for_work_item(
        &self,
        work_item: &crate::WorkItemExecutionV1,
    ) -> Result<RuntimeAuthoritySnapshotV1, WorkflowError> {
        self.organization
            .authority_snapshot(
                &work_item.tenant_id,
                &work_item.project_id,
                &work_item.work_item_id,
                work_item.agent_id,
            )
            .map_err(map_organization_error)
    }
}

fn require_ready(
    readiness: DependencyReadiness,
    code: WorkflowErrorCode,
) -> Result<(), WorkflowError> {
    if readiness == DependencyReadiness::Ready {
        Ok(())
    } else {
        Err(WorkflowError::new(
            code,
            true,
            "required workflow dependency is unavailable",
        ))
    }
}

fn map_organization_error(error: WorkflowPortError) -> WorkflowError {
    map_port_error(error, WorkflowErrorCode::OrganizationUnavailable)
}

fn map_execution_error(error: WorkflowPortError) -> WorkflowError {
    map_port_error(error, WorkflowErrorCode::ExecutionUnavailable)
}

fn map_completion_error(error: WorkflowPortError) -> WorkflowError {
    map_port_error(error, WorkflowErrorCode::CompletionUnavailable)
}

fn map_gate_error(error: WorkflowPortError) -> WorkflowError {
    map_port_error(error, WorkflowErrorCode::GateUnavailable)
}

fn map_port_error(error: WorkflowPortError, unavailable: WorkflowErrorCode) -> WorkflowError {
    match error {
        WorkflowPortError::Unavailable => WorkflowError::new(
            unavailable,
            true,
            "required workflow dependency is unavailable",
        ),
        WorkflowPortError::AuthorityConflict | WorkflowPortError::Rejected => authority_conflict(),
        WorkflowPortError::TimedOut => {
            WorkflowError::new(unavailable, true, "workflow dependency timed out")
        }
        WorkflowPortError::UnknownOutcome => WorkflowError::new(
            WorkflowErrorCode::UnknownOutcome,
            false,
            "workflow dependency outcome is unknown",
        ),
    }
}

fn authority_conflict() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::AuthorityConflict,
        false,
        "workflow authority receipt is invalid or stale",
    )
}

fn ensure_same_authority(
    before: &RuntimeAuthoritySnapshotV1,
    after: &RuntimeAuthoritySnapshotV1,
) -> Result<(), WorkflowError> {
    if before == after && before.canonical_digest()? == after.canonical_digest()? {
        Ok(())
    } else {
        Err(authority_conflict())
    }
}

fn validate_pre_io_authority(
    work_item: &crate::WorkItemExecutionV1,
    authority: &RuntimeAuthoritySnapshotV1,
    request_authority_digest: &str,
    durable_updated_at_ms: u64,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    authority.validate()?;
    if now_ms < durable_updated_at_ms || now_ms < work_item.updated_at_unix_ms {
        return Err(WorkflowError::new(
            WorkflowErrorCode::InvalidTransition,
            false,
            "workflow clock regressed behind durable state",
        ));
    }
    if !work_item.plan.authority_matches(authority)
        || !constant_time_eq(request_authority_digest, &authority.canonical_digest()?)
    {
        return Err(authority_conflict());
    }
    Ok(())
}
