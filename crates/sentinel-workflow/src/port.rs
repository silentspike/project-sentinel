use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use sha2::{Digest, Sha256};

use crate::{ActorRole, AgentId, AgentProfile, ProjectId, WorkItemId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyReadiness {
    Ready,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrganizationAgentSnapshot {
    pub generation: u64,
    pub digest: String,
    pub profile: AgentProfile,
}

impl OrganizationAgentSnapshot {
    pub fn new(generation: u64, profile: AgentProfile) -> Result<Self, WorkExecutionError> {
        let digest = organization_snapshot_digest(generation, &profile)?;
        Ok(Self {
            generation,
            digest,
            profile,
        })
    }

    pub fn verify(&self) -> Result<(), WorkExecutionError> {
        if self.digest == organization_snapshot_digest(self.generation, &self.profile)? {
            Ok(())
        } else {
            Err(WorkExecutionError::InvalidAuthoritySnapshot)
        }
    }
}

pub trait OrganizationRuntimePort: Send + Sync {
    fn readiness(&self) -> DependencyReadiness;

    fn agent_snapshot(
        &self,
        agent_id: AgentId,
    ) -> Result<OrganizationAgentSnapshot, WorkExecutionError>;
}

#[derive(Debug, Default)]
pub struct UnavailableOrganizationRuntimePort;

impl OrganizationRuntimePort for UnavailableOrganizationRuntimePort {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Unavailable
    }

    fn agent_snapshot(
        &self,
        _agent_id: AgentId,
    ) -> Result<OrganizationAgentSnapshot, WorkExecutionError> {
        Err(WorkExecutionError::Unavailable)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingExecution {
    pub schema_version: u32,
    pub invocation_id: String,
    pub project_id: ProjectId,
    pub work_item_id: WorkItemId,
    pub agent_id: AgentId,
    pub requested_by: String,
    pub requested_role: ActorRole,
    #[serde(default = "legacy_tenant_id")]
    pub tenant_id: String,
    pub assignment_version: u64,
    #[serde(default)]
    pub organization_generation: u64,
    #[serde(default)]
    pub organization_digest: String,
    pub capabilities: BTreeSet<String>,
    pub input_digest: String,
    pub deadline_ms: u64,
}

fn legacy_tenant_id() -> String {
    "legacy".to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionReceipt {
    pub invocation_id: String,
    pub accepted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionReceiptQuery {
    pub schema_version: u32,
    pub request_id: String,
    pub request_digest: String,
    pub invocation_id: String,
    pub project_id: ProjectId,
    pub project_version: u64,
    pub work_item_id: WorkItemId,
    pub work_item_version: u64,
    pub assignment_version: u64,
    pub agent_id: AgentId,
    pub assignment_authority_generation: u64,
    pub assignment_authority_digest: String,
    pub input_digest: String,
    pub replay_domain: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCompletionEvidence {
    pub query: CompletionReceiptQuery,
    pub requested_by: String,
    pub requested_role: ActorRole,
    pub tenant_id: String,
    pub operation_id: String,
    pub operation_digest: String,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactReceipt {
    pub kind: String,
    pub digest: String,
    pub owner: AgentId,
    pub invocation_id: String,
    pub project_id: ProjectId,
    pub work_item_id: WorkItemId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateReceipt {
    pub receipt_id: String,
    pub invocation_id: String,
    pub project_id: ProjectId,
    pub work_item_id: WorkItemId,
    pub gate_id: String,
    pub runner_id: String,
    pub subject_digest: String,
    pub passed: bool,
}

/// Opaque authority result returned only by the configured evidence port.
///
/// The workflow never accepts a receipt from an API caller and deliberately
/// exposes no concrete receipt constructor. A production #694 adapter owns its
/// private receipt representation and implements these read-only claims after
/// it has verified the authority's signature or capability.
pub trait CompletionAuthorityReceipt: Send + Sync {
    fn schema_version(&self) -> u32;
    fn receipt_id(&self) -> &str;
    fn request_digest(&self) -> &str;
    fn invocation_id(&self) -> &str;
    fn project_id(&self) -> &ProjectId;
    fn project_version(&self) -> u64;
    fn work_item_id(&self) -> &WorkItemId;
    fn work_item_version(&self) -> u64;
    fn assignment_version(&self) -> u64;
    fn assignment_authority_generation(&self) -> u64;
    fn issuer(&self) -> AgentId;
    fn issuer_authority_generation(&self) -> u64;
    fn issuer_authority_digest(&self) -> &str;
    fn issued_at_ms(&self) -> u64;
    fn expires_at_ms(&self) -> u64;
    fn replay_domain(&self) -> &str;
    fn artifacts(&self) -> &[ArtifactReceipt];
    fn gate(&self) -> &GateReceipt;
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkExecutionError {
    #[error("work execution dependency is unavailable")]
    Unavailable,
    #[error("work execution request was rejected")]
    Rejected,
    #[error("organization authority snapshot is invalid or stale")]
    InvalidAuthoritySnapshot,
    #[error("completion evidence receipt is invalid or stale")]
    InvalidCompletionReceipt,
    #[error("completion evidence authority timed out")]
    TimedOut,
}

/// Narrow boundary implemented by #694 after its dependency chain merges.
/// Implementations must be idempotent by `invocation_id`.
pub trait WorkExecutionPort: Send + Sync {
    fn readiness(&self) -> DependencyReadiness;

    fn reserve(&self, request: &PendingExecution) -> Result<ExecutionReceipt, WorkExecutionError>;
}

/// Evidence boundary implemented by #694. The caller asks for completion; only
/// this configured authority may return an opaque receipt implementation.
pub trait CompletionEvidencePort: Send + Sync {
    fn readiness(&self) -> DependencyReadiness;

    fn completion_receipt(
        &self,
        query: &CompletionReceiptQuery,
    ) -> Result<Box<dyn CompletionAuthorityReceipt>, WorkExecutionError>;
}

#[derive(Debug, Default)]
pub struct UnavailableCompletionEvidencePort;

impl CompletionEvidencePort for UnavailableCompletionEvidencePort {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Unavailable
    }

    fn completion_receipt(
        &self,
        _query: &CompletionReceiptQuery,
    ) -> Result<Box<dyn CompletionAuthorityReceipt>, WorkExecutionError> {
        Err(WorkExecutionError::Unavailable)
    }
}

#[derive(Debug, Default)]
pub struct UnavailableExecutionPort;

impl WorkExecutionPort for UnavailableExecutionPort {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Unavailable
    }

    fn reserve(&self, _request: &PendingExecution) -> Result<ExecutionReceipt, WorkExecutionError> {
        Err(WorkExecutionError::Unavailable)
    }
}

fn organization_snapshot_digest(
    generation: u64,
    profile: &AgentProfile,
) -> Result<String, WorkExecutionError> {
    use std::fmt::Write as _;

    let bytes = serde_json::to_vec(&(generation, profile))
        .map_err(|_| WorkExecutionError::InvalidAuthoritySnapshot)?;
    let hash = Sha256::digest(bytes);
    let mut digest = String::with_capacity(64);
    for byte in hash {
        write!(&mut digest, "{byte:02x}")
            .map_err(|_| WorkExecutionError::InvalidAuthoritySnapshot)?;
    }
    Ok(digest)
}
