// Offline scheduling regressions; these do not assert provider or customer acceptance.
use super::*;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

const WAIT: Duration = Duration::from_secs(10);

fn enabled_fixture() -> WorkflowApi {
    let mut api = WorkflowApi::disabled().unwrap();
    api.enabled = true;
    api.event_store = Some(sentinel_limbo::EventStore::open(":memory:").unwrap());
    api
}

fn assert_same_scan_status(before: &ReconciliationStatus, api: &WorkflowApi) {
    let after = api.reconciliation_status_snapshot();
    assert_eq!(after.scan_succeeded, before.scan_succeeded);
    assert_eq!(after.last_error, before.last_error);
}

#[test]
fn completion_priority_nested_intents_do_not_clear_each_other_or_recovery_intent() {
    let api = enabled_fixture();
    assert_eq!(api.completion_adoption_pending.load(Ordering::Acquire), 0);
    assert!(!api.background_work_should_yield());
    let first = api.completion_adoption_intent();
    let second = api.completion_adoption_intent();
    assert_eq!(api.completion_adoption_pending.load(Ordering::Acquire), 2);
    assert!(api.background_work_should_yield());
    drop(first);
    assert_eq!(api.completion_adoption_pending.load(Ordering::Acquire), 1);
    assert!(matches!(
        api.try_background_mutation_read(),
        Err(TryLockError::WouldBlock)
    ));
    api.provider_recovery_pending.store(true, Ordering::Release);
    drop(second);
    assert_eq!(api.completion_adoption_pending.load(Ordering::Acquire), 0);
    assert!(api.background_work_should_yield());
    api.provider_recovery_pending
        .store(false, Ordering::Release);
    assert!(!api.background_work_should_yield());
    assert!(api.try_background_mutation_read().is_ok());
}

#[test]
fn completion_priority_unwind_releases_only_the_panicking_intent() {
    let api = enabled_fixture();
    let outer = api.completion_adoption_intent();
    let result = catch_unwind(AssertUnwindSafe(|| {
        let _inner = api.completion_adoption_intent();
        assert_eq!(api.completion_adoption_pending.load(Ordering::Acquire), 2);
        panic!("synthetic completion adoption unwind");
    }));
    assert!(result.is_err());
    assert_eq!(api.completion_adoption_pending.load(Ordering::Acquire), 1);
    assert!(api.background_work_should_yield());
    assert!(!api.mutation_fence.is_poisoned());
    drop(outer);
    assert_eq!(api.completion_adoption_pending.load(Ordering::Acquire), 0);
    assert!(!api.background_work_should_yield());
    assert!(api.try_background_mutation_read().is_ok());
}

#[test]
fn completion_priority_preserves_existing_reader_and_skipped_worker_status() {
    let mut api = enabled_fixture();
    api.model_work_enabled = true;
    *api.provider_admission.write().unwrap() = true;
    api.publish_reconciliation_status(Some("retained scan error".to_owned()));
    *api.adaptive_tool_progress_error.lock().unwrap() = Some("retained tool error".to_owned());
    let before = api.reconciliation_status_snapshot();
    let existing = api.try_background_mutation_read().unwrap();
    let intent = api.completion_adoption_intent();
    // Intent neither revokes the existing reader nor grants exclusive ownership.
    assert!(matches!(
        api.mutation_fence.try_write(),
        Err(TryLockError::WouldBlock)
    ));
    assert!(api.mutation_fence.try_read().is_ok());
    assert!(matches!(
        api.try_background_mutation_read(),
        Err(TryLockError::WouldBlock)
    ));
    api.reconcile_pending_until(|| false);
    assert!(!api.reconcile_adaptive_tools_until(|| false).unwrap());
    assert_same_scan_status(&before, &api);
    assert_eq!(
        api.adaptive_tool_progress_error.lock().unwrap().as_deref(),
        Some("retained tool error")
    );
    assert!(api
        .adaptive_tool_progress_failures
        .lock()
        .unwrap()
        .is_empty());
    assert!(!api.provider_recovery_is_pending());
    assert!(*api.provider_admission.read().unwrap());
    drop(existing);
    assert!(
        api.mutation_fence.try_write().is_ok(),
        "intent must not itself own the fence"
    );
    assert!(matches!(
        api.try_background_mutation_read(),
        Err(TryLockError::WouldBlock)
    ));
    drop(intent);
    assert!(api.try_background_mutation_read().is_ok());
    assert!(api.reconcile_adaptive_tools_until(|| false).unwrap());
    assert!(api.adaptive_tool_progress_error.lock().unwrap().is_none());
    assert_same_scan_status(&before, &api);
}

#[test]
fn completion_priority_company_batch_yields_at_boundary_then_resumes_cleanly() {
    for previous in [None, Some(None), Some(Some("retained scan failure"))] {
        let api = enabled_fixture();
        if let Some(previous) = previous {
            api.publish_reconciliation_status(previous.map(str::to_owned));
        }
        let before = api.reconciliation_status_snapshot();
        let checks = AtomicUsize::new(0);
        let (boundary_tx, boundary_rx) = mpsc::channel();
        let (intent_tx, intent_rx) = mpsc::channel();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            let api = &api;
            let writer = scope.spawn(move || {
                boundary_rx
                    .recv_timeout(WAIT)
                    .expect("batch never reached its durable boundary");
                let _intent = api.completion_adoption_intent();
                let initially_blocked = matches!(
                    api.mutation_fence.try_write(),
                    Err(TryLockError::WouldBlock)
                );
                intent_tx.send(initially_blocked).unwrap();
                // Bounded waiting models the adopter without leaving a test thread
                // permanently blocked on a regressed reader lifetime.
                let deadline = Instant::now() + WAIT;
                let guard = loop {
                    match api.mutation_fence.try_write() {
                        Ok(guard) => break Some(guard),
                        Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                            std::thread::yield_now();
                        }
                        Err(_) => break None,
                    }
                };
                acquired_tx.send(guard.is_some()).unwrap();
                if guard.is_some() {
                    release_rx
                        .recv_timeout(WAIT)
                        .expect("writer was not released");
                }
            });
            let blocked_at_boundary = std::cell::Cell::new(false);
            api.reconcile_pending_until(|| {
                if checks.fetch_add(1, Ordering::SeqCst) == 1 {
                    // The first check precedes the batch. This check follows
                    // the real collaboration-publication step under its reader.
                    boundary_tx.send(()).unwrap();
                    blocked_at_boundary.set(intent_rx.recv_timeout(WAIT).unwrap());
                }
                false
            });
            let acquired = acquired_rx.recv_timeout(WAIT);
            let after = api.reconciliation_status_snapshot();
            let pending = api.completion_adoption_pending.load(Ordering::Acquire);
            // A skipped pass must not manufacture readiness or clear an old error.
            api.reconcile_pending_until(|| false);
            let skipped = api.reconciliation_status_snapshot();
            let _ = release_tx.send(());
            writer.join().unwrap();
            assert!(blocked_at_boundary.get());
            assert!(
                matches!(acquired, Ok(true)),
                "writer did not acquire after the batch yielded"
            );
            assert_eq!(pending, 1);
            assert_eq!(after.scan_succeeded, before.scan_succeeded);
            assert_eq!(after.last_error, before.last_error);
            assert_eq!(skipped.scan_succeeded, before.scan_succeeded);
            assert_eq!(skipped.last_error, before.last_error);
        });
        assert_eq!(api.completion_adoption_pending.load(Ordering::Acquire), 0);
        assert!(!api.background_work_should_yield());
        api.reconcile_pending_until(|| false);
        let resumed = api.reconciliation_status_snapshot();
        assert!(resumed.scan_succeeded);
        assert!(resumed.last_error.is_none());
        assert!(api.reconciliation_fence.try_lock().is_ok());
        assert!(api.try_background_mutation_read().is_ok());
    }
}

#[test]
fn completion_priority_trait_entry_signals_before_fence_wait_and_cleans_up_on_error() {
    use crate::llm_bridge::bridge::ProviderUsageAuthorityResolver;

    let mut api = enabled_fixture();
    api.model_work_enabled = true;
    *api.provider_admission.write().unwrap() = true;
    let context: model_execution::ModelExecutionContext = model_work::test_context().into();
    let request_id = context.binding().request_id();
    let completion = model_execution::ModelExecutionCompletion {
        context,
        content: "{}".to_owned(),
        admissible: false,
    };
    let before = api.reconciliation_status_snapshot();
    let fence = api.mutation_fence.write().unwrap();
    std::thread::scope(|scope| {
        let (returned_tx, returned_rx) = mpsc::channel();
        let api = &api;
        let completion = &completion;
        let request_id = &request_id;
        let caller = scope.spawn(move || {
            let result = ProviderUsageAuthorityResolver::admit_model_work(
                api,
                completion,
                request_id,
                &"a".repeat(64),
            );
            returned_tx.send(result).unwrap();
        });
        let deadline = Instant::now() + WAIT;
        while api.completion_adoption_pending.load(Ordering::Acquire) == 0
            && Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        let pending = api.completion_adoption_pending.load(Ordering::Acquire);
        let yielded = api.background_work_should_yield();
        let returned_while_fenced = returned_rx.try_recv();
        drop(fence);
        let result = returned_rx.recv_timeout(WAIT);
        caller.join().unwrap();
        assert_eq!(pending, 1, "trait entry failed to register adoption intent");
        assert!(yielded);
        assert!(matches!(
            returned_while_fenced,
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(matches!(result, Ok(Err(_))));
    });
    assert_eq!(api.completion_adoption_pending.load(Ordering::Acquire), 0);
    assert!(!api.background_work_should_yield());
    assert!(*api.provider_admission.read().unwrap());
    assert!(!api.provider_recovery_is_pending());
    assert_same_scan_status(&before, &api);
    assert!(api
        .event_store
        .as_ref()
        .unwrap()
        .get_all_events()
        .unwrap()
        .is_empty());
    assert!(api.try_background_mutation_read().is_ok());
}
