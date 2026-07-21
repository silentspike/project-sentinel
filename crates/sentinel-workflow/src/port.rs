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

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkExecutionError {
    #[error("work execution dependency is unavailable")]
    Unavailable,
    #[error("work execution request was rejected")]
    Rejected,
    #[error("organization authority snapshot is invalid or stale")]
    InvalidAuthoritySnapshot,
}

/// Narrow boundary implemented by #694 after its dependency chain merges.
/// Implementations must be idempotent by `invocation_id`.
pub trait WorkExecutionPort: Send + Sync {
    fn readiness(&self) -> DependencyReadiness;

    fn reserve(&self, request: &PendingExecution) -> Result<ExecutionReceipt, WorkExecutionError>;
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
