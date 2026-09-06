use std::time::Duration;

use anyhow::Context;
use rusqlite::{Error, ErrorCode};

const MAX_ATTEMPTS: usize = 3;

/// The caller owns a rollback-safe transaction, a read, or an idempotent write.
pub(crate) fn sqlite_busy<T>(
    operation: &'static str,
    mut attempt: impl FnMut() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    for index in 0..MAX_ATTEMPTS {
        match attempt() {
            Ok(value) => return Ok(value),
            Err(error) => {
                let busy = matches!(
                    error.downcast_ref::<Error>(),
                    Some(Error::SqliteFailure(code, _)) if code.code == ErrorCode::DatabaseBusy
                );
                if !busy || index + 1 == MAX_ATTEMPTS {
                    return Err(error).with_context(|| format!("{operation} failed"));
                }
                let delay = Duration::from_millis(50 << index);
                tracing::warn!(
                    operation,
                    attempt = index + 1,
                    max_attempts = MAX_ATTEMPTS,
                    retry_after_ms = delay.as_millis() as u64,
                    "SQLite writer busy; retrying bounded operation"
                );
                std::thread::sleep(delay);
            }
        }
    }
    unreachable!("bounded loop always returns on its final attempt")
}

/// A failed mirror write must never run an already committed projection again.
pub(crate) fn commit_then_mirror<T>(
    commit: impl FnOnce() -> anyhow::Result<T>,
    mirror: impl FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<T> {
    let value = commit()?;
    sqlite_busy("projection offset mirror", mirror)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn busy() -> anyhow::Error {
        Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY), None).into()
    }

    #[test]
    fn wrapped_busy_recovers_within_three_attempts() {
        let mut attempts = 0;
        let result = sqlite_busy("fixture", || {
            attempts += 1;
            if attempts < 3 {
                Err(busy()).context("nested database operation")
            } else {
                Ok(42)
            }
        });
        assert_eq!(result.unwrap(), 42);
        assert_eq!(attempts, 3);
    }

    #[test]
    fn exhausted_busy_remains_terminal_and_identifiable() {
        let mut attempts = 0;
        let error = sqlite_busy::<()>("fixture", || {
            attempts += 1;
            Err(busy())
        })
        .unwrap_err();
        assert_eq!(attempts, 3);
        assert!(error.downcast_ref::<Error>().is_some());
        assert_eq!(error.to_string(), "fixture failed");
    }

    #[test]
    fn non_busy_sqlite_and_authority_errors_are_not_retried() {
        for code in [
            rusqlite::ffi::SQLITE_LOCKED,
            rusqlite::ffi::SQLITE_CORRUPT,
            rusqlite::ffi::SQLITE_SCHEMA,
            rusqlite::ffi::SQLITE_CONSTRAINT,
            rusqlite::ffi::SQLITE_IOERR,
        ] {
            let mut attempts = 0;
            assert!(sqlite_busy::<()>("fixture", || {
                attempts += 1;
                Err(Error::SqliteFailure(rusqlite::ffi::Error::new(code), None).into())
            })
            .is_err());
            assert_eq!(attempts, 1);
        }
        let mut attempts = 0;
        assert!(sqlite_busy::<()>("fixture", || {
            attempts += 1;
            anyhow::bail!("owner authority changed")
        })
        .is_err());
        assert_eq!(attempts, 1);
    }

    #[test]
    fn mirror_contention_never_reexecutes_committed_batch() {
        let committed = Cell::new(0);
        let mirrors = Cell::new(0);
        let value = commit_then_mirror(
            || {
                committed.set(committed.get() + 1);
                Ok(17)
            },
            || {
                assert_eq!(committed.get(), 1);
                mirrors.set(mirrors.get() + 1);
                if mirrors.get() < 3 {
                    Err(busy())
                } else {
                    Ok(())
                }
            },
        )
        .unwrap();
        assert_eq!(value, 17);
        assert_eq!(committed.get(), 1);
        assert_eq!(mirrors.get(), 3);
    }

    #[test]
    fn exhausted_mirror_preserves_the_single_committed_batch() {
        let mut committed = 0;
        let mut mirrors = 0;
        assert!(commit_then_mirror(
            || {
                committed += 1;
                Ok(())
            },
            || {
                mirrors += 1;
                Err(busy())
            },
        )
        .is_err());
        assert_eq!(committed, 1);
        assert_eq!(mirrors, MAX_ATTEMPTS);
    }

    #[test]
    fn failed_batch_never_advances_mirror() {
        let mut mirrors = 0;
        assert!(commit_then_mirror::<()>(
            || anyhow::bail!("batch rejected"),
            || {
                mirrors += 1;
                Ok(())
            },
        )
        .is_err());
        assert_eq!(mirrors, 0);
    }
}
