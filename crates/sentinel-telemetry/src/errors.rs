//! Error severity classification for Project Sentinel.
//!
//! Provides a trait for classifying errors by severity, enabling
//! consistent error handling across all crates. The trait is defined
//! here; implementations are added by individual crates in Sprint 2+.

/// Severity classification for errors.
///
/// Determines the system's response to an error condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
pub trait ClassifiedError {
    /// Classify this error's severity.
    fn severity(&self) -> ErrorSeverity;

    /// Whether this error can be retried.
    fn is_retryable(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimaler Test-Error-Typ um den Trait zu verifizieren
    #[derive(Debug)]
    struct TestError(ErrorSeverity);

    impl ClassifiedError for TestError {
        fn severity(&self) -> ErrorSeverity {
            self.0
        }

        fn is_retryable(&self) -> bool {
            self.0 == ErrorSeverity::Transient
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
}
