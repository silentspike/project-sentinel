//! Bounded recovery admission without waiting while holding a partial fence.

use std::sync::{MutexGuard, RwLockWriteGuard};
use std::time::Instant;

use super::*;

const RECOVERY_ADMISSION_TIMEOUT: Duration = Duration::from_secs(5);
const RECOVERY_ADMISSION_RETRY: Duration = Duration::from_millis(2);

struct RecoveryIntent<'a>(&'a AtomicBool);

impl Drop for RecoveryIntent<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

struct RecoveryLocks<'a> {
    _admission: RwLockWriteGuard<'a, bool>,
    _mutation: RwLockWriteGuard<'a, ()>,
    active_requests: MutexGuard<'a, HashMap<String, usize>>,
}

pub(super) struct ProviderRecoveryGuard<'a> {
    locks: RecoveryLocks<'a>,
    // Release intent after the protected locks, including unwinding.
    _intent: RecoveryIntent<'a>,
}

impl ProviderRecoveryGuard<'_> {
    pub(super) fn request_is_active(&self, request_id: &str) -> bool {
        self.locks.active_requests.contains_key(request_id)
    }
}

impl WorkflowApi {
    pub(super) fn provider_recovery_fence(
        &self,
    ) -> Result<ProviderRecoveryGuard<'_>, &'static str> {
        self.provider_recovery_fence_until(Instant::now() + RECOVERY_ADMISSION_TIMEOUT)
    }

    pub(super) fn provider_recovery_fence_until(
        &self,
        deadline: Instant,
    ) -> Result<ProviderRecoveryGuard<'_>, &'static str> {
        if !self.enabled || !self.model_work_enabled {
            return Err("provider recovery unavailable");
        }
        self.provider_recovery_pending
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "workflow recovery busy")?;
        let intent = RecoveryIntent(&self.provider_recovery_pending);
        let mut first_attempt = true;
        let mut last_reason = "workflow recovery busy";
        loop {
            if !first_attempt && Instant::now() >= deadline {
                return Err(last_reason);
            }
            first_attempt = false;
            match self.try_provider_recovery_locks() {
                Ok(locks) => {
                    return Ok(ProviderRecoveryGuard {
                        locks,
                        _intent: intent,
                    });
                }
                Err((reason, false)) => return Err(reason),
                Err((reason, true)) => last_reason = reason,
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(last_reason);
            }
            // Every partial lock was dropped by the failed attempt. Provider
            // reservations and owned completions can therefore finish normally.
            std::thread::sleep(RECOVERY_ADMISSION_RETRY.min(remaining));
        }
    }

    fn try_provider_recovery_locks(&self) -> Result<RecoveryLocks<'_>, (&'static str, bool)> {
        // Same global order as provider reservation. Never block while holding
        // admission, mutation or live ownership; poison is not retryable.
        let admission = self.provider_admission.try_write().map_err(|error| {
            (
                "provider recovery admission unavailable",
                matches!(error, TryLockError::WouldBlock),
            )
        })?;
        if !*admission {
            return Err(("provider admission closed", false));
        }
        let mutation = self.mutation_fence.try_write().map_err(|error| {
            (
                "workflow recovery busy",
                matches!(error, TryLockError::WouldBlock),
            )
        })?;
        let active_requests = self.active_provider_requests.try_lock().map_err(|error| {
            (
                "provider ownership unavailable",
                matches!(error, TryLockError::WouldBlock),
            )
        })?;
        Ok(RecoveryLocks {
            _admission: admission,
            _mutation: mutation,
            active_requests,
        })
    }
}
