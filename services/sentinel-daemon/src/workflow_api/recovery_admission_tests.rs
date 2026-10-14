#![cfg(test)]

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

use super::*;

const WAIT_BOUND: Duration = Duration::from_secs(10);
const CONTENTION_BOUND: Duration = Duration::from_millis(200);

fn enabled_api() -> WorkflowApi {
    let mut api = WorkflowApi::disabled().unwrap();
    api.enabled = true;
    api.model_work_enabled = true;
    *api.provider_admission.write().unwrap() = true;
    api
}

fn wait_until(mut observed: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT_BOUND;
    while !observed() {
        assert!(Instant::now() < deadline, "observation deadline expired");
        thread::yield_now();
    }
}

fn wait_pending(api: &WorkflowApi) {
    wait_until(|| api.provider_recovery_pending.load(Ordering::Acquire));
}

fn assert_released(api: &WorkflowApi) {
    assert!(!api.provider_recovery_pending.load(Ordering::Acquire));
    assert!(*api.provider_admission.try_write().unwrap());
    assert!(api.mutation_fence.try_write().is_ok());
    assert!(api.active_provider_requests.try_lock().is_ok());
    assert!(api.try_background_mutation_read().is_ok());
}

#[test]
fn pending_recovery_denies_background_but_allows_completion_then_acquires() {
    let api = enabled_api();
    let existing_reader = api.mutation_fence.read().unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (acquired_tx, acquired_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    thread::scope(|scope| {
        let api = &api;
        let recovery = scope.spawn(move || {
            started_tx.send(()).unwrap();
            let guard = api
                .provider_recovery_fence_until(Instant::now() + WAIT_BOUND)
                .unwrap();
            acquired_tx.send(()).unwrap();
            release_rx.recv_timeout(WAIT_BOUND).unwrap();
            drop(guard);
        });
        started_rx.recv_timeout(WAIT_BOUND).unwrap();
        wait_pending(api);
        assert!(matches!(
            api.try_background_mutation_read(),
            Err(TryLockError::WouldBlock)
        ));
        // Completion/dispatch reads deliberately do not consult recovery intent.
        let completion = api.mutation_fence.try_read().unwrap();
        wait_until(|| api.provider_admission.try_write().is_ok());
        assert!(api.active_provider_requests.try_lock().is_ok());
        assert!(matches!(
            acquired_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        drop(completion);
        drop(existing_reader);
        acquired_rx.recv_timeout(WAIT_BOUND).unwrap();
        assert!(api.provider_recovery_pending.load(Ordering::Acquire));
        assert!(matches!(
            api.provider_admission.try_read(),
            Err(TryLockError::WouldBlock)
        ));
        assert!(matches!(
            api.mutation_fence.try_read(),
            Err(TryLockError::WouldBlock)
        ));
        assert!(matches!(
            api.active_provider_requests.try_lock(),
            Err(TryLockError::WouldBlock)
        ));
        release_tx.send(()).unwrap();
        recovery.join().unwrap();
    });
    assert_released(&api);
}

#[test]
fn active_registration_contention_releases_both_partial_fences() {
    let api = enabled_api();
    let registration = api.active_provider_requests.lock().unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    thread::scope(|scope| {
        let api = &api;
        let recovery = scope.spawn(move || {
            started_tx.send(()).unwrap();
            let result = api
                .provider_recovery_fence_until(Instant::now() + WAIT_BOUND)
                .map(drop);
            result_tx.send(result).unwrap();
        });
        started_rx.recv_timeout(WAIT_BOUND).unwrap();
        wait_pending(api);
        wait_until(|| {
            let Ok(_admission) = api.provider_admission.try_write() else {
                return false;
            };
            api.mutation_fence.try_write().is_ok()
        });
        assert!(matches!(
            result_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        drop(registration);
        assert_eq!(result_rx.recv_timeout(WAIT_BOUND).unwrap(), Ok(()));
        recovery.join().unwrap();
    });
    assert_released(&api);
}

#[test]
fn concurrent_recovery_is_busy_without_clearing_owner_intent() {
    let api = enabled_api();
    let reader = api.mutation_fence.read().unwrap();
    let (result_tx, result_rx) = mpsc::channel();
    thread::scope(|scope| {
        let api = &api;
        let owner = scope.spawn(move || {
            result_tx
                .send(
                    api.provider_recovery_fence_until(Instant::now() + WAIT_BOUND)
                        .map(drop),
                )
                .unwrap();
        });
        wait_pending(api);
        assert!(matches!(
            api.provider_recovery_fence_until(Instant::now()),
            Err("workflow recovery busy")
        ));
        assert!(api.provider_recovery_pending.load(Ordering::Acquire));
        drop(reader);
        assert_eq!(result_rx.recv_timeout(WAIT_BOUND).unwrap(), Ok(()));
        owner.join().unwrap();
    });
    assert_released(&api);
}

#[test]
fn timeout_releases_intent_and_every_partial_lock_at_each_contention_stage() {
    for stage in 0..3 {
        let api = enabled_api();
        let admission = (stage == 0).then(|| api.provider_admission.read().unwrap());
        let mutation = (stage == 1).then(|| api.mutation_fence.read().unwrap());
        let active = (stage == 2).then(|| api.active_provider_requests.lock().unwrap());
        let deadline = Instant::now() + CONTENTION_BOUND;
        let result = api.provider_recovery_fence_until(deadline).map(drop);
        assert_eq!(
            result,
            Err(match stage {
                0 => "provider recovery admission unavailable",
                1 => "workflow recovery busy",
                _ => "provider ownership unavailable",
            })
        );
        assert!(Instant::now() >= deadline);
        assert!(!api.provider_recovery_pending.load(Ordering::Acquire));
        if stage != 0 {
            assert!(api.provider_admission.try_write().is_ok());
        }
        if stage != 1 {
            assert!(api.mutation_fence.try_write().is_ok());
        }
        if stage != 2 {
            assert!(api.active_provider_requests.try_lock().is_ok());
        }
        drop((admission, mutation, active));
        assert_released(&api);
        drop(api.provider_recovery_fence_until(Instant::now()).unwrap());
        assert_released(&api);
    }
}

#[test]
fn expired_recovery_thread_cannot_clear_a_newer_pending_intent() {
    let api = enabled_api();
    let reader = api.mutation_fence.read().unwrap();
    let (expired_tx, expired_rx) = mpsc::channel();
    let (exit_tx, exit_rx) = mpsc::channel();
    let (acquired_tx, acquired_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    thread::scope(|scope| {
        let api = &api;
        let expired = scope.spawn(move || {
            expired_tx
                .send(api.provider_recovery_fence_until(Instant::now()).map(drop))
                .unwrap();
            exit_rx.recv_timeout(WAIT_BOUND).unwrap();
        });
        assert_eq!(
            expired_rx.recv_timeout(WAIT_BOUND).unwrap(),
            Err("workflow recovery busy")
        );
        assert!(!api.provider_recovery_pending.load(Ordering::Acquire));
        let next = scope.spawn(move || {
            let guard = api
                .provider_recovery_fence_until(Instant::now() + WAIT_BOUND)
                .unwrap();
            acquired_tx.send(()).unwrap();
            release_rx.recv_timeout(WAIT_BOUND).unwrap();
            drop(guard);
        });
        wait_pending(api);
        exit_tx.send(()).unwrap();
        expired.join().unwrap();
        assert!(api.provider_recovery_pending.load(Ordering::Acquire));
        assert!(matches!(
            api.try_background_mutation_read(),
            Err(TryLockError::WouldBlock)
        ));
        drop(reader);
        acquired_rx.recv_timeout(WAIT_BOUND).unwrap();
        assert!(api.provider_recovery_pending.load(Ordering::Acquire));
        release_tx.send(()).unwrap();
        next.join().unwrap();
    });
    assert_released(&api);
}

#[test]
fn recovery_times_out_before_contention_is_released() {
    let api = enabled_api();
    let reader = api.mutation_fence.read().unwrap();
    let (result_tx, result_rx) = mpsc::channel();
    thread::scope(|scope| {
        let api = &api;
        let recovery = scope.spawn(move || {
            let deadline = Instant::now() + CONTENTION_BOUND;
            result_tx
                .send(api.provider_recovery_fence_until(deadline).map(drop))
                .unwrap();
        });
        // Observe expiry before releasing contention, without a scheduling window.
        assert_eq!(
            result_rx.recv_timeout(WAIT_BOUND).unwrap(),
            Err("workflow recovery busy")
        );
        drop(reader);
        recovery.join().unwrap();
    });
    assert_released(&api);
}

#[test]
fn zero_deadline_still_allows_one_uncontended_initial_attempt() {
    let api = enabled_api();
    let guard = api.provider_recovery_fence_until(Instant::now()).unwrap();
    assert!(api.provider_recovery_pending.load(Ordering::Acquire));
    drop(guard);
    assert_released(&api);
    let reader = api.mutation_fence.read().unwrap();
    assert!(matches!(
        api.provider_recovery_fence_until(Instant::now()),
        Err("workflow recovery busy")
    ));
    assert!(!api.provider_recovery_pending.load(Ordering::Acquire));
    assert!(api.provider_admission.try_write().is_ok());
    drop(reader);
    assert_released(&api);
}

#[test]
fn successful_default_fence_preserves_exact_active_map_identity_and_counts() {
    let api = enabled_api();
    let requests = api.active_provider_requests();
    assert!(Arc::ptr_eq(&requests, &api.active_provider_requests));
    let expected = HashMap::from([("review-a".to_owned(), 2), ("review-b".to_owned(), 1)]);
    *requests.lock().unwrap() = expected.clone();
    let guard = api.provider_recovery_fence().unwrap();
    assert!(guard.request_is_active("review-a"));
    assert!(guard.request_is_active("review-b"));
    assert!(!guard.request_is_active("review-c"));
    assert!(api.provider_recovery_pending.load(Ordering::Acquire));
    drop(guard);
    assert_released(&api);
    assert!(Arc::ptr_eq(&requests, &api.active_provider_requests()));
    assert_eq!(*requests.lock().unwrap(), expected);
}

#[test]
fn panic_dropping_recovery_guard_resets_intent_without_clearing_poison() {
    let api = enabled_api();
    assert!(catch_unwind(AssertUnwindSafe(|| {
        let _guard = api.provider_recovery_fence_until(Instant::now()).unwrap();
        panic!("unwind an acquired recovery fence");
    }))
    .is_err());
    assert!(!api.provider_recovery_pending.load(Ordering::Acquire));
    assert!(matches!(
        api.provider_admission.try_write(),
        Err(TryLockError::Poisoned(_))
    ));
    assert!(matches!(
        api.mutation_fence.try_write(),
        Err(TryLockError::Poisoned(_))
    ));
    assert!(matches!(
        api.active_provider_requests.try_lock(),
        Err(TryLockError::Poisoned(_))
    ));
    assert!(matches!(
        api.provider_recovery_fence_until(Instant::now() + WAIT_BOUND),
        Err("provider recovery admission unavailable")
    ));
    assert!(!api.provider_recovery_pending.load(Ordering::Acquire));
}

#[test]
fn poison_at_each_stage_rejects_immediately_and_retains_poison() {
    for stage in 0..3 {
        let api = enabled_api();
        assert!(catch_unwind(AssertUnwindSafe(|| match stage {
            0 => {
                let _guard = api.provider_admission.write().unwrap();
                panic!("poison admission");
            }
            1 => {
                let _guard = api.mutation_fence.write().unwrap();
                panic!("poison mutation");
            }
            _ => {
                let _guard = api.active_provider_requests.lock().unwrap();
                panic!("poison active ownership");
            }
        }))
        .is_err());
        for _ in 0..2 {
            assert_eq!(
                api.provider_recovery_fence_until(Instant::now()).map(drop),
                Err(match stage {
                    0 => "provider recovery admission unavailable",
                    1 => "workflow recovery busy",
                    _ => "provider ownership unavailable",
                })
            );
            assert!(!api.provider_recovery_pending.load(Ordering::Acquire));
            assert_eq!(api.provider_admission.is_poisoned(), stage == 0);
            assert_eq!(api.mutation_fence.is_poisoned(), stage == 1);
            assert_eq!(api.active_provider_requests.is_poisoned(), stage == 2);
            if stage != 0 {
                assert!(api.provider_admission.try_write().is_ok());
            }
            if stage != 1 {
                assert!(api.mutation_fence.try_write().is_ok());
            }
            if stage != 2 {
                assert!(api.active_provider_requests.try_lock().is_ok());
            }
        }
    }
}

#[test]
fn admission_then_mutation_then_active_order_is_preserved_on_failed_attempts() {
    let api = enabled_api();
    assert!(catch_unwind(AssertUnwindSafe(|| {
        let _guard = api.active_provider_requests.lock().unwrap();
        panic!("poison the last fence");
    }))
    .is_err());
    let admission = api.provider_admission.read().unwrap();
    assert!(matches!(
        api.provider_recovery_fence_until(Instant::now()),
        Err("provider recovery admission unavailable")
    ));
    drop(admission);
    let mutation = api.mutation_fence.read().unwrap();
    assert!(matches!(
        api.provider_recovery_fence_until(Instant::now()),
        Err("workflow recovery busy")
    ));
    assert!(api.provider_admission.try_write().is_ok());
    drop(mutation);
    assert!(matches!(
        api.provider_recovery_fence_until(Instant::now()),
        Err("provider ownership unavailable")
    ));
    assert!(!api.provider_recovery_pending.load(Ordering::Acquire));
    assert!(api.provider_admission.try_write().is_ok());
    assert!(api.mutation_fence.try_write().is_ok());
    assert!(api.active_provider_requests.is_poisoned());
}

#[test]
fn closed_or_disabled_recovery_is_immediate_and_never_reopens_admission() {
    for (enabled, model_enabled, admission_open, expected) in [
        (false, true, true, "provider recovery unavailable"),
        (true, false, true, "provider recovery unavailable"),
        (false, false, false, "provider recovery unavailable"),
        (true, true, false, "provider admission closed"),
    ] {
        let mut api = enabled_api();
        api.enabled = enabled;
        api.model_work_enabled = model_enabled;
        *api.provider_admission.write().unwrap() = admission_open;
        assert_eq!(
            api.provider_recovery_fence_until(Instant::now()).map(drop),
            Err(expected)
        );
        assert!(!api.provider_recovery_pending.load(Ordering::Acquire));
        assert_eq!(*api.provider_admission.try_write().unwrap(), admission_open);
        assert!(api.mutation_fence.try_write().is_ok());
        assert!(api.active_provider_requests.try_lock().is_ok());
    }
}

#[test]
fn sticky_yielded_pass_consults_intent_and_never_publishes_success() {
    for tools in [false, true] {
        let mut api = enabled_api();
        api.event_store = Some(sentinel_limbo::EventStore::open(":memory:").unwrap());
        api.publish_reconciliation_status(Some("previous-company-error".to_owned()));
        let calls = std::cell::Cell::new(0);
        let should_stop = || {
            let call = calls.get();
            calls.set(call + 1);
            // Without the sticky latch, the next check clears intent and lets
            // an incomplete pass falsely publish success.
            match call {
                1 => api.provider_recovery_pending.store(true, Ordering::Release),
                2 => api
                    .provider_recovery_pending
                    .store(false, Ordering::Release),
                _ => {}
            }
            false
        };
        if tools {
            assert!(!api.reconcile_adaptive_tools_until(should_stop).unwrap());
            assert!(api.adaptive_tool_progress_error.lock().unwrap().is_none());
        } else {
            api.reconcile_pending_until(should_stop);
        }
        assert_eq!(calls.get(), 2);
        assert!(api.provider_recovery_pending.load(Ordering::Acquire));
        let status = api.reconciliation_status_snapshot();
        assert!(!status.scan_succeeded);
        assert_eq!(status.last_error.as_deref(), Some("previous-company-error"));
        assert!(api.company_sync_cursor.lock().unwrap().is_none());
        assert!(api.adaptive_tool_progress_cursor.lock().unwrap().is_none());
        assert!(!api.health().ready);
        api.provider_recovery_pending
            .store(false, Ordering::Release);
        assert_released(&api);
    }
}

#[test]
fn pending_recovery_defers_real_delivery_outbox_then_normal_scan_drains_once() {
    use std::os::unix::fs::PermissionsExt;

    use crate::delivery::{
        ContentDigest, DeliveryAggregateV1, DeliveryCommitRequestV1, DeliveryStore,
    };

    let temp = tempfile::tempdir().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let config = DeliveryStoreConfigV1::new(temp.path(), "delivery.redb").unwrap();
    let store = DeliveryStore::open(&config).unwrap();
    let mut aggregate = DeliveryAggregateV1::new("tenant-recovery", "project-recovery");
    aggregate.revision = 1;
    store
        .commit(&DeliveryCommitRequestV1 {
            tenant_id: aggregate.tenant_id.clone(),
            project_id: aggregate.project_id.clone(),
            expected_revision: 0,
            principal_id: "recovery-probe".to_owned(),
            command_kind: "fixture".to_owned(),
            idempotency_key: "recovery-publication-1".to_owned(),
            command_digest: ContentDigest::of(&1).unwrap(),
            aggregate,
            event_type: "recovery_fixture".to_owned(),
            event_payload: serde_json::json!({ "revision": 1 }),
            committed_at_ms: 1,
        })
        .unwrap();
    let pending = store.pending_publications().unwrap();
    assert_eq!(pending.len(), 1);
    let request = pending[0].request.clone();
    drop(store);

    let mut api = enabled_api();
    let events = sentinel_limbo::EventStore::open(":memory:").unwrap();
    let config_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../config")
        .canonicalize()
        .unwrap();
    let (qa_profile, qa_digest) =
        WorkbenchProfile::load(config_dir.join("workbench-profiles/web-qa-v1.toml")).unwrap();
    let integration = WorkflowDeliveryIntegration::new(
        Arc::clone(&api.store),
        Arc::clone(&api.principals),
        qa_profile,
        qa_digest,
        Arc::new(HashMap::new()),
        Arc::new(HashMap::new()),
        None,
        ProjectProfileCatalog::embedded(),
    );
    let config = DeliveryStoreConfigV1::new(temp.path(), "delivery.redb").unwrap();
    let delivery = Arc::new(
        ProductDeliveryCore::open(
            &config,
            integration,
            LimboDeliveryEffects::new(
                events.clone(),
                Arc::clone(&api.store),
                Arc::clone(&api.principals),
            ),
            LimboDeliveryPublication::new(events.clone()),
        )
        .unwrap(),
    );
    api.delivery = Some(Arc::clone(&delivery));
    api.event_store = Some(events.clone());
    api.publish_reconciliation_status(Some("previous-company-error".to_owned()));
    let event_count = events.event_count().unwrap();
    let reader = api.mutation_fence.read().unwrap();
    let (result_tx, result_rx) = mpsc::channel();
    thread::scope(|scope| {
        let api = &api;
        let recovery = scope.spawn(move || {
            result_tx
                .send(
                    api.provider_recovery_fence_until(Instant::now() + WAIT_BOUND)
                        .map(drop),
                )
                .unwrap();
        });
        wait_pending(api);
        api.reconcile_pending_until(|| false);
        assert_eq!(delivery.pending_publications_test_only().unwrap(), pending);
        assert_eq!(delivery.pending_publication_count().unwrap(), 1);
        assert_eq!(events.event_count().unwrap(), event_count);
        assert!(events
            .event_by_operation_id(&request.operation_id)
            .unwrap()
            .is_none());
        let status = api.reconciliation_status_snapshot();
        assert!(!status.scan_succeeded);
        assert_eq!(status.last_error.as_deref(), Some("previous-company-error"));
        assert!(!api.health().ready);
        drop(reader);
        assert_eq!(result_rx.recv_timeout(WAIT_BOUND).unwrap(), Ok(()));
        recovery.join().unwrap();
    });
    assert_released(&api);
    api.reconcile_pending_until(|| false);
    assert_eq!(delivery.pending_publication_count().unwrap(), 0);
    assert_eq!(events.event_count().unwrap(), event_count + 1);
    let event = events
        .event_by_operation_id(&request.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(event.event_type, request.event_type);
    assert_eq!(event.aggregate_id, request.aggregate_id);
    assert_eq!(event.payload.as_bytes(), request.payload.as_slice());
    let status = api.reconciliation_status_snapshot();
    assert!(status.scan_succeeded);
    assert!(status.last_error.is_none());
    api.reconcile_pending_until(|| false);
    assert_eq!(delivery.pending_publication_count().unwrap(), 0);
    assert_eq!(events.event_count().unwrap(), event_count + 1);
    assert_released(&api);
}

#[test]
fn pending_company_and_tool_scans_skip_without_fake_readiness_or_error_reset() {
    for previous_error in [None, Some("previous-company-error")] {
        let api = enabled_api();
        if let Some(error) = previous_error {
            api.publish_reconciliation_status(Some(error.to_owned()));
        }
        *api.adaptive_tool_progress_error.lock().unwrap() = Some("previous-tool-error".to_owned());
        let reader = api.mutation_fence.read().unwrap();
        let (result_tx, result_rx) = mpsc::channel();
        thread::scope(|scope| {
            let api = &api;
            let recovery = scope.spawn(move || {
                result_tx
                    .send(
                        api.provider_recovery_fence_until(Instant::now() + WAIT_BOUND)
                            .map(drop),
                    )
                    .unwrap();
            });
            wait_pending(api);
            api.reconcile_pending_until(|| false);
            assert!(!api.reconcile_adaptive_tools_until(|| false).unwrap());
            let status = api.reconciliation_status_snapshot();
            assert!(!status.scan_succeeded);
            assert_eq!(status.last_error.as_deref(), previous_error);
            assert_eq!(
                api.adaptive_tool_progress_error.lock().unwrap().as_deref(),
                Some("previous-tool-error")
            );
            assert!(api.company_sync_cursor.lock().unwrap().is_none());
            assert!(api.adaptive_tool_progress_cursor.lock().unwrap().is_none());
            assert!(!api.health().ready);
            assert!(api.reconciliation_fence.try_lock().is_ok());
            assert!(api.adaptive_tool_progress_fence.try_lock().is_ok());
            drop(reader);
            assert_eq!(result_rx.recv_timeout(WAIT_BOUND).unwrap(), Ok(()));
            recovery.join().unwrap();
        });
        assert_released(&api);
    }
}
