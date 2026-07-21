use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowErrorCode {
    InvalidInput,
    NotFound,
    Unauthorized,
    InvalidTransition,
    VersionConflict,
    IdempotencyConflict,
    DigestConflict,
    DagInvalid,
    CapabilityDenied,
    BudgetExceeded,
    ExecutionUnavailable,
    PersistenceFailure,
}

#[derive(Debug, Error)]
#[error("{code:?}: {message}")]
pub struct WorkflowError {
    pub code: WorkflowErrorCode,
    pub retryable: bool,
    pub message: String,
}

impl WorkflowError {
    pub fn new(code: WorkflowErrorCode, retryable: bool, message: impl Into<String>) -> Self {
        Self {
            code,
            retryable,
            message: message.into(),
        }
    }

    pub(crate) fn persistence() -> Self {
        Self::new(
            WorkflowErrorCode::PersistenceFailure,
            true,
            "workflow persistence operation failed",
        )
    }
}

impl From<rusqlite::Error> for WorkflowError {
    fn from(_: rusqlite::Error) -> Self {
        Self::persistence()
    }
}

impl From<serde_json::Error> for WorkflowError {
    fn from(_: serde_json::Error) -> Self {
        Self::persistence()
    }
}
