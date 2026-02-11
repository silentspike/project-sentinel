//! Error severity classification for Project Sentinel.
//!
//! Provides a trait for classifying errors by severity, enabling
//! consistent error handling across all crates. The trait is defined
//! here; implementations are added by individual crates in Sprint 2+.
//!
//! [`ErrorEvent`] is the wire format published to `sentinel/telemetry/errors`
//! for Dashboard consumption.

use sentinel_common::{AgentId, Tick, Timestamp};
use serde::{Deserialize, Serialize};

/// Severity classification for errors.
///
/// Determines the system's response to an error condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorSeverity {
    /// Unrecoverable error. Triggers graceful shutdown.
    /// Example: redb corruption, persistent Zenoh disconnect.
    Fatal,
    /// Temporary error. Retry with exponential backoff.
    /// Example: API timeout, transient network failure.
    Transient,
    /// Partial failure. Continue with reduced functionality.
    /// Example: single agent LLM timeout, non-critical cache miss.
    Degraded,
}

/// Trait for classifying errors by severity.
///
/// Implement this on error types to enable consistent error handling.
/// The runtime uses severity to decide: shutdown, retry, or degrade.
///
/// # Example (Sprint 2+)
///
/// ```ignore
/// impl ClassifiedError for RedbError {
///     fn severity(&self) -> ErrorSeverity {
///         match self {
///             RedbError::Corruption(_) => ErrorSeverity::Fatal,
///             RedbError::LockTimeout => ErrorSeverity::Transient,
///             _ => ErrorSeverity::Degraded,
///         }
///     }
///
///     fn is_retryable(&self) -> bool {
///         self.severity() == ErrorSeverity::Transient
///     }
/// }
/// ```
pub trait ClassifiedError: std::error::Error {
    /// Classify this error's severity.
    fn severity(&self) -> ErrorSeverity;

    /// The subsystem that produced this error (e.g. "redb", "zenoh", "limbo").
    fn subsystem(&self) -> &str;

    /// Whether this error can be retried.
    /// Default: true only for Transient errors.
    fn is_retryable(&self) -> bool {
        matches!(self.severity(), ErrorSeverity::Transient)
    }
}

// ──────────────────────────────────────────────
// ErrorEvent (wire format for Dashboard)
// ──────────────────────────────────────────────

/// Classified error event published to `sentinel/telemetry/errors`.
///
/// The Dashboard subscribes to this topic for real-time error display.
/// Serialized as MessagePack over Zenoh transport.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEvent {
    /// Severity classification.
    pub severity: ErrorSeverity,
    /// Subsystem that produced the error (e.g. "redb", "zenoh", "limbo").
    pub subsystem: String,
    /// Human-readable error message.
    pub message: String,
    /// Whether this error can be retried.
    pub retryable: bool,
    /// Agent context (if error relates to a specific agent).
    pub agent_id: Option<AgentId>,
    /// Simulation tick when error occurred.
    pub tick: Option<Tick>,
    /// Wall-clock timestamp.
    pub timestamp: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimaler Test-Error-Typ um den Trait zu verifizieren
    #[derive(Debug)]
    struct TestError(ErrorSeverity);

    impl std::fmt::Display for TestError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "test error: {:?}", self.0)
        }
    }

    impl std::error::Error for TestError {}

    impl ClassifiedError for TestError {
        fn severity(&self) -> ErrorSeverity {
            self.0
        }

        fn subsystem(&self) -> &str {
            "test"
        }
    }

    #[test]
    fn test_fatal_not_retryable() {
        let err = TestError(ErrorSeverity::Fatal);
        assert_eq!(err.severity(), ErrorSeverity::Fatal);
        assert!(!err.is_retryable());
    }

    #[test]
    fn test_transient_is_retryable() {
        let err = TestError(ErrorSeverity::Transient);
        assert_eq!(err.severity(), ErrorSeverity::Transient);
        assert!(err.is_retryable());
    }

    #[test]
    fn test_degraded_not_retryable() {
        let err = TestError(ErrorSeverity::Degraded);
        assert_eq!(err.severity(), ErrorSeverity::Degraded);
        assert!(!err.is_retryable());
    }

    #[test]
    fn test_error_event_serialization_roundtrip() {
        let event = ErrorEvent {
            severity: ErrorSeverity::Transient,
            subsystem: "zenoh".to_string(),
            message: "Connection timeout".to_string(),
            retryable: true,
            agent_id: Some(AgentId(7)),
            tick: Some(Tick(42)),
            timestamp: Timestamp(1000),
        };

        let json = serde_json::to_string(&event).unwrap();
        let deserialized: ErrorEvent = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.severity, ErrorSeverity::Transient);
        assert_eq!(deserialized.subsystem, "zenoh");
        assert!(deserialized.retryable);
        assert_eq!(deserialized.agent_id, Some(AgentId(7)));
        assert_eq!(deserialized.tick, Some(Tick(42)));
    }

    #[test]
    fn test_error_severity_serialization() {
        let json = serde_json::to_string(&ErrorSeverity::Fatal).unwrap();
        let deserialized: ErrorSeverity = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, ErrorSeverity::Fatal);
    }
}
