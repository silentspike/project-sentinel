#![cfg(test)]
//! Offline scheduling and real HTTP-entry guards; no provider/runtime acceptance.
use super::*;
use crate::workflow_api::adaptive_leadership_review::tests::discovery_state;
use crate::workflow_api::adaptive_resume_policy::tests::unreviewed_policy_root_with_limits;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(10);

fn fixture() -> (tempfile::TempDir, WorkflowApi, String) {
    let (temp, api, session) = unreviewed_policy_root_with_limits(120_000, false, 4, 4);
    // A well-formed preview with no such refused review. It reaches the real
    // exclusive fence and source verifier, then rejects without issuing anything.
    let path = format!("{ADAPTIVE_WORK_FUNDING_PATH}?mode=material-window-expansion&project_id={}&session_id={}&operation_id={}&reason_ref=scheduling-test&declined_funding_receipt_digest={}&refused_review_id={}&additional_model_calls=0&additional_tool_calls=0&additional_reviews=1&additional_windows=1&max_window_ms=300000&max_call_duration_ms=120000&dispatch_margin_ms={}&expires_at_unix_ms={}",
        session.grant.authority.project_id, session.grant.session_id, Uuid::new_v4(),
        "a".repeat(64), Uuid::new_v4(), sentinel_workflow::ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
        now_unix_ms() + 600_000);
    (temp, api, path)
}

#[test]
fn material_preparation_nested_intents_preserve_each_other_and_existing_priorities() {
    let api = WorkflowApi::disabled().unwrap();
    let first = api.material_preparation_intent();
    let second = api.material_preparation_intent();
    let completion = api.completion_adoption_intent();
    assert_eq!(api.material_preparation_pending.load(Ordering::Acquire), 2);
    assert!(api.background_work_should_yield());
    assert!(
        api.mutation_fence.try_write().is_ok(),
        "intent is not ownership"
    );
    drop(first);
    assert_eq!(api.material_preparation_pending.load(Ordering::Acquire), 1);
    drop(second);
    assert_eq!(api.material_preparation_pending.load(Ordering::Acquire), 0);
    assert!(
        api.background_work_should_yield(),
        "completion still owns its intent"
    );
    api.provider_recovery_pending.store(true, Ordering::Release);
    drop(completion);
    assert!(
        api.background_work_should_yield(),
        "recovery still has priority"
    );
    api.provider_recovery_pending
        .store(false, Ordering::Release);
    assert!(api.try_background_mutation_read().is_ok());
}

#[test]
fn material_preparation_unwind_clears_only_its_own_intent() {
    let api = WorkflowApi::disabled().unwrap();
    let outer = api.material_preparation_intent();
    let failed = catch_unwind(AssertUnwindSafe(|| {
        let _inner = api.material_preparation_intent();
        assert_eq!(api.material_preparation_pending.load(Ordering::Acquire), 2);
        panic!("synthetic preparation unwind");
    }));
    assert!(failed.is_err());
    assert_eq!(api.material_preparation_pending.load(Ordering::Acquire), 1);
    assert!(!api.mutation_fence.is_poisoned());
    drop(outer);
    assert!(!api.background_work_should_yield());
}

#[test]
fn material_preparation_background_owner_finishes_boundary_then_yields_without_readiness() {
    let mut api = WorkflowApi::disabled().unwrap();
    api.enabled = true;
    api.event_store = Some(sentinel_limbo::EventStore::open(":memory:").unwrap());
    api.publish_reconciliation_status(Some("retained scan failure".to_owned()));
    let before = api.reconciliation_status_snapshot();
    let (boundary_tx, boundary_rx) = mpsc::channel();
    let (intent_tx, intent_rx) = mpsc::channel();
    let (acquired_tx, acquired_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    std::thread::scope(|scope| {
        let api = &api;
        let writer = scope.spawn(move || {
            boundary_rx.recv_timeout(WAIT).unwrap();
            let _intent = api.material_preparation_intent();
            intent_tx
                .send(api.mutation_fence.try_write().is_err())
                .unwrap();
            let deadline = Instant::now() + WAIT;
            let guard = loop {
                match api.mutation_fence.try_write() {
                    Ok(guard) => break Some(guard),
                    Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                        std::thread::yield_now()
                    }
                    Err(_) => break None,
                }
            };
            acquired_tx.send(guard.is_some()).unwrap();
            if guard.is_some() {
                release_rx.recv_timeout(WAIT).unwrap();
            }
        });
        let checks = std::cell::Cell::new(0);
        let owner_was_preserved = std::cell::Cell::new(false);
        api.reconcile_pending_until(|| {
            let check = checks.replace(checks.get() + 1);
            if check == 1 {
                boundary_tx.send(()).unwrap();
                owner_was_preserved.set(intent_rx.recv_timeout(WAIT).unwrap());
            }
            false
        });
        let acquired = acquired_rx.recv_timeout(WAIT);
        let after = api.reconciliation_status_snapshot();
        api.reconcile_pending_until(|| false);
        let skipped = api.reconciliation_status_snapshot();
        let _ = release_tx.send(());
        writer.join().unwrap();
        assert!(owner_was_preserved.get());
        assert!(matches!(acquired, Ok(true)));
        assert_eq!(after.scan_succeeded, before.scan_succeeded);
        assert_eq!(after.last_error, before.last_error);
        assert_eq!(skipped.scan_succeeded, before.scan_succeeded);
        assert_eq!(skipped.last_error, before.last_error);
    });
    assert_eq!(api.material_preparation_pending.load(Ordering::Acquire), 0);
    assert!(api.try_background_mutation_read().is_ok());
}

#[test]
fn material_preparation_real_get_signals_before_wait_and_clears_on_source_error() {
    let (temp, api, path) = fixture();
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    let operator = api.principals.principal("operator").unwrap();
    let owner = api.mutation_fence.read().unwrap();
    std::thread::scope(|scope| {
        let child = scope.spawn(|| {
            let calls = std::cell::Cell::new(0);
            let result =
                api.material_window_expansion_http_with_clock(&operator, "GET", &path, &[], || {
                    calls.set(calls.get() + 1);
                    assert!(
                        api.mutation_fence.try_read().is_err(),
                        "preview must remain exclusive"
                    );
                    now_unix_ms()
                });
            (result, calls.get())
        });
        let deadline = Instant::now() + WAIT;
        while api.material_preparation_pending.load(Ordering::Acquire) == 0
            && Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        let signaled = api.material_preparation_pending.load(Ordering::Acquire);
        let owner_preserved = api.mutation_fence.try_write().is_err();
        drop(owner);
        let (result, calls) = child.join().unwrap();
        assert_eq!(signaled, 1);
        assert!(owner_preserved);
        assert_ne!(result.status, 200);
        assert!(calls > 0);
    });
    assert_eq!(api.material_preparation_pending.load(Ordering::Acquire), 0);
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite")
        ),
        before
    );
}

#[test]
fn material_preparation_real_get_poisoned_fence_clears_intent() {
    let (_temp, api, path) = fixture();
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _guard = api.mutation_fence.write().unwrap();
        panic!("synthetic owner failure");
    }));
    let operator = api.principals.principal("operator").unwrap();
    let result = api.material_window_expansion_http(&operator, "GET", &path, &[]);
    assert_eq!(result.status, 503);
    assert_eq!(api.material_preparation_pending.load(Ordering::Acquire), 0);
}

#[test]
fn material_preparation_real_get_panic_clears_intent_and_invalid_input_never_registers() {
    let (_temp, api, path) = fixture();
    let operator = api.principals.principal("operator").unwrap();
    assert_eq!(
        api.material_window_expansion_http(&operator, "GET", "invalid", &[])
            .status,
        400
    );
    assert_eq!(api.material_preparation_pending.load(Ordering::Acquire), 0);
    let result = catch_unwind(AssertUnwindSafe(|| {
        api.material_window_expansion_http_with_clock(&operator, "GET", &path, &[], || {
            assert_eq!(api.material_preparation_pending.load(Ordering::Acquire), 1);
            panic!("synthetic preparation clock failure");
        })
    }));
    assert!(result.is_err());
    assert_eq!(api.material_preparation_pending.load(Ordering::Acquire), 0);
}
