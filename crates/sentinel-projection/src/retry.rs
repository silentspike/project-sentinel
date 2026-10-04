use std::time::Duration;

use anyhow::Context;
use rusqlite::{Error, ErrorCode};
use sentinel_limbo::event_store::ProjectionOffsetAcquisitionBusy;

const MAX_ATTEMPTS: usize = 3;
pub(crate) const MIRROR_DEFER_BACKOFF: Duration = Duration::from_millis(50);

#[derive(Debug)]
struct CommittedMirrorBusy {
    projection: &'static str,
}

impl std::fmt::Display for CommittedMirrorBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "committed projection mirror busy: {}", self.projection)
    }
}

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
    projection: &'static str,
    mirror_exists: bool,
    commit: impl FnOnce() -> anyhow::Result<T>,
    mut mirror: impl FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<T> {
    let value = commit()?;
    let acquisition_busy = |error: &anyhow::Error| {
        matches!(
            error.downcast_ref::<Error>(),
            Some(Error::SqliteFailure(code, _)) if code.code == ErrorCode::DatabaseBusy
        ) && error
            .downcast_ref::<ProjectionOffsetAcquisitionBusy>()
            .is_some_and(|busy| busy.projection() == projection)
    };
    // A later clean acquisition failure cannot prove an earlier attempt was safe.
    let mut every_failed_attempt_is_acquisition_busy = true;
    if let Err(error) = sqlite_busy("projection offset mirror", || {
        let result = mirror();
        if let Err(error) = &result {
            every_failed_attempt_is_acquisition_busy &= acquisition_busy(error);
        }
        result
    }) {
        if mirror_exists && every_failed_attempt_is_acquisition_busy && acquisition_busy(&error) {
            return Err(error.context(CommittedMirrorBusy { projection }));
        }
        return Err(error);
    }
    Ok(value)
}

/// Only the live loop may postpone an established mirror after a committed batch.
pub(crate) fn live_batch(result: anyhow::Result<usize>) -> anyhow::Result<Option<usize>> {
    match result {
        Ok(count) => Ok(Some(count)),
        Err(error) if error.downcast_ref::<CommittedMirrorBusy>().is_some() => {
            let busy = error.downcast_ref::<CommittedMirrorBusy>().unwrap();
            tracing::warn!(
                projection = busy.projection,
                error = %format!("{error:#}"),
                "Projection committed; postponing contended offset mirror until next live poll"
            );
            Ok(None)
        }
        Err(error) => Err(error),
    }
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
            "fixture",
            true,
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
            "fixture",
            true,
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
            "fixture",
            true,
            || anyhow::bail!("batch rejected"),
            || {
                mirrors += 1;
                Ok(())
            },
        )
        .is_err());
        assert_eq!(mirrors, 0);
    }

    #[test]
    fn real_sqlite_writer_contention_recovers_with_bounded_retry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mirror-retry.db");
        let mut connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE mirror(value INTEGER)")
            .unwrap();
        connection.busy_timeout(Duration::ZERO).unwrap();
        let competitor = rusqlite::Connection::open(path).unwrap();
        competitor.execute_batch("BEGIN IMMEDIATE").unwrap();
        let mut attempts = 0;
        sqlite_busy("real mirror writer", || {
            attempts += 1;
            if attempts == MAX_ATTEMPTS {
                competitor.execute_batch("ROLLBACK")?;
            }
            let transaction =
                connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            transaction.execute("INSERT INTO mirror VALUES(1)", [])?;
            transaction.commit()?;
            Ok(())
        })
        .unwrap();
        assert_eq!(attempts, MAX_ATTEMPTS);
        assert!(connection.is_autocommit());
        let total: i64 = connection
            .query_row("SELECT SUM(value) FROM mirror", [], |row| row.get(0))
            .unwrap();
        assert_eq!(total, 1);
    }

    #[test]
    fn live_deferral_requires_existing_matching_mirror_and_typed_acquisition_busy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("typed-mirror-busy.db");
        let store = sentinel_limbo::EventStore::open(path.to_str().unwrap()).unwrap();
        store.update_offset("fixture", 0).unwrap();
        let competitor = rusqlite::Connection::open(path).unwrap();
        competitor.execute_batch("BEGIN IMMEDIATE").unwrap();
        let error = store.update_offset("fixture", 1).unwrap_err();
        let acquisition = error
            .downcast_ref::<ProjectionOffsetAcquisitionBusy>()
            .unwrap()
            .clone();
        competitor.execute_batch("ROLLBACK").unwrap();

        for (projection, exists, deferrable) in [
            ("fixture", true, true),
            ("fixture", false, false),
            ("other-projection", true, false),
        ] {
            let mut commits = 0;
            let mut attempts = 0;
            let error = commit_then_mirror(
                projection,
                exists,
                || {
                    commits += 1;
                    Ok(17)
                },
                || {
                    attempts += 1;
                    Err(busy().context(acquisition.clone()))
                },
            )
            .unwrap_err();
            assert_eq!(commits, 1);
            assert_eq!(attempts, MAX_ATTEMPTS);
            let result = live_batch(Err(error.context("live poll fixture")));
            if deferrable {
                assert_eq!(result.unwrap(), None);
            } else {
                assert!(result.is_err());
            }
        }
        let error = commit_then_mirror(
            "fixture",
            true,
            || Ok(17),
            || Err(anyhow::anyhow!("offset commit ownership changed").context(acquisition.clone())),
        )
        .unwrap_err();
        assert!(live_batch(Err(error)).is_err());
        assert_eq!(live_batch(Ok(17)).unwrap(), Some(17));
        assert_eq!(live_batch(Ok(0)).unwrap(), Some(0));
    }

    #[test]
    fn live_deferral_rejects_generic_busy_and_non_busy_mirror_failure_matrix() {
        for code in [
            rusqlite::ffi::SQLITE_BUSY,
            rusqlite::ffi::SQLITE_LOCKED,
            rusqlite::ffi::SQLITE_CORRUPT,
            rusqlite::ffi::SQLITE_SCHEMA,
            rusqlite::ffi::SQLITE_CONSTRAINT,
            rusqlite::ffi::SQLITE_IOERR,
        ] {
            let mut commits = 0;
            let mut attempts = 0;
            let error = commit_then_mirror(
                "fixture",
                true,
                || {
                    commits += 1;
                    Ok(17)
                },
                || {
                    attempts += 1;
                    Err(Error::SqliteFailure(rusqlite::ffi::Error::new(code), None).into())
                },
            )
            .unwrap_err();
            assert_eq!(commits, 1);
            assert_eq!(
                attempts,
                if code == rusqlite::ffi::SQLITE_BUSY {
                    MAX_ATTEMPTS
                } else {
                    1
                }
            );
            assert!(live_batch(Err(error)).is_err(), "SQLite code {code}");
        }
        for message in [
            "owner authority changed",
            "offset monotonicity violation",
            "offset write failed",
            "offset commit failed",
            "offset rollback failed",
            "unresolved projection offset transaction",
        ] {
            let error = commit_then_mirror("fixture", true, || Ok(17), || anyhow::bail!(message))
                .unwrap_err();
            assert!(live_batch(Err(error)).is_err(), "{message}");
        }
    }

    #[test]
    fn live_deferral_rejects_unproved_busy_before_clean_acquisition_failures() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mixed-mirror-busy.db");
        let store = sentinel_limbo::EventStore::open(path.to_str().unwrap()).unwrap();
        store.update_offset("fixture", 0).unwrap();
        let competitor = rusqlite::Connection::open(path).unwrap();
        competitor.execute_batch("BEGIN IMMEDIATE").unwrap();
        let error = store.update_offset("fixture", 1).unwrap_err();
        let acquisition = error
            .downcast_ref::<ProjectionOffsetAcquisitionBusy>()
            .unwrap()
            .clone();
        competitor.execute_batch("ROLLBACK").unwrap();
        for unproved_attempt in 1..=MAX_ATTEMPTS {
            let mut attempts = 0;
            let error = commit_then_mirror(
                "fixture",
                true,
                || Ok(17),
                || {
                    attempts += 1;
                    if attempts == unproved_attempt {
                        Err(busy().context("unproved mirror write or commit failure"))
                    } else {
                        Err(busy().context(acquisition.clone()))
                    }
                },
            )
            .unwrap_err();
            assert_eq!(attempts, MAX_ATTEMPTS);
            assert!(live_batch(Err(error)).is_err());
        }
    }

    #[test]
    fn live_deferral_never_suppresses_batch_or_cleanup_failure() {
        for message in [
            "batch commit failed",
            "smell cleanup failed",
            "batch rollback failed",
        ] {
            let mut mirrors = 0;
            let error = commit_then_mirror::<usize>(
                "fixture",
                true,
                || Err(busy().context(message)),
                || {
                    mirrors += 1;
                    Ok(())
                },
            )
            .unwrap_err();
            assert_eq!(mirrors, 0);
            assert!(live_batch(Err(error)).is_err(), "{message}");
        }
    }
}
