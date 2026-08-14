use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowErrorCode {
    InvalidInput,
    InvalidDigest,
    InvalidTransition,
    NotFound,
    VersionConflict,
    IdempotencyConflict,
    AuthorityConflict,
    OrganizationUnavailable,
    ExecutionUnavailable,
    CompletionUnavailable,
    GateUnavailable,
    UnknownOutcome,
    CorruptStore,
    PersistenceFailure,
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("{code:?}: {message}")]
pub struct WorkflowError {
    pub code: WorkflowErrorCode,
    pub retryable: bool,
    pub message: &'static str,
}

impl WorkflowError {
    pub const fn new(code: WorkflowErrorCode, retryable: bool, message: &'static str) -> Self {
        Self {
            code,
            retryable,
            message,
        }
    }

    pub(crate) const fn persistence() -> Self {
        Self::new(
            WorkflowErrorCode::PersistenceFailure,
            false,
            "workflow persistence operation failed",
        )
    }

    pub(crate) const fn transient_persistence() -> Self {
        Self::new(
            WorkflowErrorCode::PersistenceFailure,
            true,
            "workflow persistence operation failed",
        )
    }

    pub(crate) const fn corrupt_store() -> Self {
        Self::new(
            WorkflowErrorCode::CorruptStore,
            false,
            "workflow store integrity validation failed",
        )
    }
}

impl From<rusqlite::Error> for WorkflowError {
    fn from(error: rusqlite::Error) -> Self {
        match error {
            rusqlite::Error::SqliteFailure(failure, _)
                if matches!(
                    failure.code,
                    rusqlite::ErrorCode::DatabaseBusy
                        | rusqlite::ErrorCode::DatabaseLocked
                        | rusqlite::ErrorCode::SystemIoFailure
                ) =>
            {
                Self::transient_persistence()
            }
            rusqlite::Error::SqliteFailure(failure, _)
                if matches!(
                    failure.code,
                    rusqlite::ErrorCode::DatabaseCorrupt
                        | rusqlite::ErrorCode::NotADatabase
                        | rusqlite::ErrorCode::SchemaChanged
                        | rusqlite::ErrorCode::TypeMismatch
                        | rusqlite::ErrorCode::ConstraintViolation
                ) =>
            {
                Self::corrupt_store()
            }
            _ => Self::persistence(),
        }
    }
}

impl From<serde_json::Error> for WorkflowError {
    fn from(_: serde_json::Error) -> Self {
        Self::corrupt_store()
    }
}

#[cfg(test)]
mod tests {
    use super::{WorkflowError, WorkflowErrorCode};

    fn sqlite_error(raw_code: i32) -> WorkflowError {
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(raw_code), None).into()
    }

    #[test]
    fn sqlite_retryability_is_explicit_and_fail_closed() {
        for raw_code in [
            rusqlite::ffi::SQLITE_BUSY,
            rusqlite::ffi::SQLITE_LOCKED,
            rusqlite::ffi::SQLITE_IOERR,
        ] {
            let error = sqlite_error(raw_code);
            assert_eq!(error.code, WorkflowErrorCode::PersistenceFailure);
            assert!(error.retryable);
        }
        for raw_code in [
            rusqlite::ffi::SQLITE_CORRUPT,
            rusqlite::ffi::SQLITE_NOTADB,
            rusqlite::ffi::SQLITE_SCHEMA,
            rusqlite::ffi::SQLITE_MISMATCH,
            rusqlite::ffi::SQLITE_CONSTRAINT,
        ] {
            let error = sqlite_error(raw_code);
            assert_eq!(error.code, WorkflowErrorCode::CorruptStore);
            assert!(!error.retryable);
        }
        for raw_code in [
            rusqlite::ffi::SQLITE_CANTOPEN,
            rusqlite::ffi::SQLITE_PROTOCOL,
            rusqlite::ffi::SQLITE_INTERRUPT,
            rusqlite::ffi::SQLITE_FULL,
            rusqlite::ffi::SQLITE_PERM,
            rusqlite::ffi::SQLITE_READONLY,
            rusqlite::ffi::SQLITE_ABORT,
            rusqlite::ffi::SQLITE_INTERNAL,
            rusqlite::ffi::SQLITE_NOMEM,
            rusqlite::ffi::SQLITE_TOOBIG,
            rusqlite::ffi::SQLITE_MISUSE,
            rusqlite::ffi::SQLITE_AUTH,
            rusqlite::ffi::SQLITE_RANGE,
            rusqlite::ffi::SQLITE_NOTFOUND,
            rusqlite::ffi::SQLITE_NOLFS,
            rusqlite::ffi::SQLITE_ERROR,
        ] {
            let error = sqlite_error(raw_code);
            assert_eq!(error.code, WorkflowErrorCode::PersistenceFailure);
            assert!(!error.retryable);
        }
    }
}
