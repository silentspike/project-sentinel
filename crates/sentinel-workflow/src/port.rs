use thiserror::Error;
use uuid::Uuid;

use crate::{
    AgentId, PendingCompletionEvidenceV1, PendingExecutionV1, PendingGateEvidenceV1, ProjectId,
    RuntimeAuthoritySnapshotV1, SealedArtifactEvidenceV1, SealedOutputEvidenceV1, TenantId,
    WorkItemId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyReadiness {
    Ready,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkflowPortError {
    #[error("dependency is unavailable")]
    Unavailable,
    #[error("authority is invalid or stale")]
    AuthorityConflict,
    #[error("request was rejected")]
    Rejected,
    #[error("request timed out")]
    TimedOut,
    #[error("outcome is unknown")]
    UnknownOutcome,
}

pub trait OrganizationRuntimePort: Send + Sync {
    fn readiness(&self) -> DependencyReadiness;

    fn authority_snapshot(
        &self,
        tenant_id: &TenantId,
        project_id: &ProjectId,
        work_item_id: &WorkItemId,
        agent_id: AgentId,
    ) -> Result<RuntimeAuthoritySnapshotV1, WorkflowPortError>;
}

/// Typed observation of an already durable Workbench invocation.
///
/// `reconcile` never creates a second invocation. The production adapter maps
/// these states from #694's durable root record by the stable invocation ID.
/// Timeout and Unavailable are permitted only when the adapter proves that no
/// dispatch or external effect occurred. Every post-dispatch ambiguity is
/// returned as `UnknownOutcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkExecutionObservation {
    NotFound,
    Reserved,
    Executing,
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    UnknownOutcome,
}

pub trait WorkExecutionPort: Send + Sync {
    fn readiness(&self) -> DependencyReadiness;

    fn reconcile(
        &self,
        request: &PendingExecutionV1,
    ) -> Result<WorkExecutionObservation, WorkflowPortError>;
}

/// Opaque #694 root completion evidence.
///
/// API callers cannot construct or submit this receipt. Only the configured
/// `CompletionEvidencePort` returns an implementation after authoritative
/// readback of the durable Workbench root record.
pub trait TerminalExecutionEvidence: Send + Sync {
    fn schema_version(&self) -> u16;
    fn receipt_id(&self) -> &str;
    fn invocation_id(&self) -> Uuid;
    fn plan_digest(&self) -> &str;
    fn step_digest(&self) -> &str;
    fn output_bundle_digest(&self) -> &str;
    fn outputs(&self) -> &[SealedOutputEvidenceV1];
    fn artifacts(&self) -> &[SealedArtifactEvidenceV1];
    fn completed_at_unix_ms(&self) -> u64;
}

pub trait CompletionEvidencePort: Send + Sync {
    fn readiness(&self) -> DependencyReadiness;

    fn terminal_evidence(
        &self,
        request: &PendingCompletionEvidenceV1,
    ) -> Result<Box<dyn TerminalExecutionEvidence>, WorkflowPortError>;
}

/// Independent work-item QA receipt. It is deliberately distinct from the
/// Workbench completion evidence and is not owned by #694.
pub trait IndependentGateEvidence: Send + Sync {
    fn schema_version(&self) -> u16;
    fn receipt_id(&self) -> &str;
    fn profile_id(&self) -> &str;
    fn profile_generation(&self) -> u64;
    fn profile_digest(&self) -> &str;
    fn subject_digest(&self) -> &str;
    fn required_checks_digest(&self) -> &str;
    fn passed(&self) -> bool;
    fn completed_at_unix_ms(&self) -> u64;
}

pub trait GateEvidencePort: Send + Sync {
    fn readiness(&self) -> DependencyReadiness;

    fn gate_evidence(
        &self,
        request: &PendingGateEvidenceV1,
    ) -> Result<Box<dyn IndependentGateEvidence>, WorkflowPortError>;
}

#[derive(Debug, Default)]
pub struct UnavailableOrganizationRuntimePort;

impl OrganizationRuntimePort for UnavailableOrganizationRuntimePort {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Unavailable
    }

    fn authority_snapshot(
        &self,
        _tenant_id: &TenantId,
        _project_id: &ProjectId,
        _work_item_id: &WorkItemId,
        _agent_id: AgentId,
    ) -> Result<RuntimeAuthoritySnapshotV1, WorkflowPortError> {
        Err(WorkflowPortError::Unavailable)
    }
}

#[derive(Debug, Default)]
pub struct UnavailableWorkExecutionPort;

impl WorkExecutionPort for UnavailableWorkExecutionPort {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Unavailable
    }

    fn reconcile(
        &self,
        _request: &PendingExecutionV1,
    ) -> Result<WorkExecutionObservation, WorkflowPortError> {
        Err(WorkflowPortError::Unavailable)
    }
}

#[derive(Debug, Default)]
pub struct UnavailableCompletionEvidencePort;

impl CompletionEvidencePort for UnavailableCompletionEvidencePort {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Unavailable
    }

    fn terminal_evidence(
        &self,
        _request: &PendingCompletionEvidenceV1,
    ) -> Result<Box<dyn TerminalExecutionEvidence>, WorkflowPortError> {
        Err(WorkflowPortError::Unavailable)
    }
}

#[derive(Debug, Default)]
pub struct UnavailableGateEvidencePort;

impl GateEvidencePort for UnavailableGateEvidencePort {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Unavailable
    }

    fn gate_evidence(
        &self,
        _request: &PendingGateEvidenceV1,
    ) -> Result<Box<dyn IndependentGateEvidence>, WorkflowPortError> {
        Err(WorkflowPortError::Unavailable)
    }
}
