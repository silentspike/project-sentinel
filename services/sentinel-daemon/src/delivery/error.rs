use std::fmt;

#[derive(Debug)]
pub enum DeliveryError {
    AdapterUnavailable {
        dependency: &'static str,
        reason: String,
    },
    AuthorityDenied(String),
    Conflict(String),
    CorruptStore(String),
    IdempotencyConflict {
        key: String,
    },
    InvalidDigest(String),
    InvalidState {
        entity: &'static str,
        from: String,
        to: String,
    },
    MissingEvidence(String),
    NotFound(String),
    RevisionConflict {
        expected: u64,
        actual: u64,
    },
    StaleEvidence(String),
    Storage(String),
    Validation(String),
}

impl fmt::Display for DeliveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AdapterUnavailable { dependency, reason } => {
                write!(formatter, "{dependency} unavailable: {reason}")
            }
            Self::AuthorityDenied(reason) => write!(formatter, "authority denied: {reason}"),
            Self::Conflict(reason) => write!(formatter, "conflict: {reason}"),
            Self::CorruptStore(reason) => write!(formatter, "corrupt delivery store: {reason}"),
            Self::IdempotencyConflict { key } => {
                write!(
                    formatter,
                    "idempotency key reused with different content: {key}"
                )
            }
            Self::InvalidDigest(value) => write!(formatter, "invalid SHA-256 digest: {value}"),
            Self::InvalidState { entity, from, to } => {
                write!(formatter, "invalid {entity} transition {from} -> {to}")
            }
            Self::MissingEvidence(reason) => write!(formatter, "missing evidence: {reason}"),
            Self::NotFound(entity) => write!(formatter, "not found: {entity}"),
            Self::RevisionConflict { expected, actual } => {
                write!(
                    formatter,
                    "revision conflict: expected {expected}, actual {actual}"
                )
            }
            Self::StaleEvidence(reason) => write!(formatter, "stale evidence: {reason}"),
            Self::Storage(reason) => write!(formatter, "delivery storage failure: {reason}"),
            Self::Validation(reason) => write!(formatter, "validation failed: {reason}"),
        }
    }
}

impl std::error::Error for DeliveryError {}

impl From<serde_json::Error> for DeliveryError {
    fn from(value: serde_json::Error) -> Self {
        Self::Storage(value.to_string())
    }
}

impl From<redb::Error> for DeliveryError {
    fn from(value: redb::Error) -> Self {
        Self::Storage(value.to_string())
    }
}

impl From<redb::DatabaseError> for DeliveryError {
    fn from(value: redb::DatabaseError) -> Self {
        Self::Storage(value.to_string())
    }
}

impl From<redb::TransactionError> for DeliveryError {
    fn from(value: redb::TransactionError) -> Self {
        Self::Storage(value.to_string())
    }
}

impl From<redb::TableError> for DeliveryError {
    fn from(value: redb::TableError) -> Self {
        Self::Storage(value.to_string())
    }
}

impl From<redb::StorageError> for DeliveryError {
    fn from(value: redb::StorageError) -> Self {
        Self::Storage(value.to_string())
    }
}

impl From<redb::CommitError> for DeliveryError {
    fn from(value: redb::CommitError) -> Self {
        Self::Storage(value.to_string())
    }
}
