use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{ActorRole, AgentId, ProjectId, WorkItemId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingExecution {
    pub schema_version: u32,
    pub invocation_id: String,
    pub project_id: ProjectId,
    pub work_item_id: WorkItemId,
    pub agent_id: AgentId,
    pub requested_by: String,
    pub requested_role: ActorRole,
    pub assignment_version: u64,
    pub capabilities: BTreeSet<String>,
    pub input_digest: String,
    pub deadline_ms: u64,
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
}

/// Narrow boundary implemented by #694 after its dependency chain merges.
/// Implementations must be idempotent by `invocation_id`.
pub trait WorkExecutionPort: Send + Sync {
    fn reserve(&self, request: &PendingExecution) -> Result<ExecutionReceipt, WorkExecutionError>;
}

#[derive(Debug, Default)]
pub struct UnavailableExecutionPort;

impl WorkExecutionPort for UnavailableExecutionPort {
    fn reserve(&self, _request: &PendingExecution) -> Result<ExecutionReceipt, WorkExecutionError> {
        Err(WorkExecutionError::Unavailable)
    }
}
