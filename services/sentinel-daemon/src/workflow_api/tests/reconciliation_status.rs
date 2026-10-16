use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{OnceLock, Weak};

use super::*;

#[cfg(feature = "llm")]
mod independent_tool_progress {
    use super::*;

    fn api() -> WorkflowApi {
        let mut api = enabled_api();
        api.model_work_enabled = true;
        api
    }

    #[test]
    fn tool_progress_does_not_wait_for_company_scan_or_publish_scan_success() {
        let api = api();
        let _scan = api.reconciliation_fence.lock().unwrap();
        assert!(api.reconcile_adaptive_tools_until(|| false).unwrap());
        assert_status(&api, false, None);
        assert!(api.adaptive_tool_progress_cursor.lock().unwrap().is_none());
    }

    #[test]
    fn tool_progress_contention_recovery_and_stop_preserve_status_and_error() {
        let api = api();
        api.publish_reconciliation_status(Some("existing-scan-error".to_owned()));
        *api.adaptive_tool_progress_error.lock().unwrap() = Some("existing-tool-error".to_owned());
        {
            let _pass = api.adaptive_tool_progress_fence.lock().unwrap();
            assert!(!api.reconcile_adaptive_tools_until(|| false).unwrap());
        }
        {
            let _recovery = api.mutation_fence.write().unwrap();
            assert!(!api.reconcile_adaptive_tools_until(|| false).unwrap());
        }
        assert!(!api.reconcile_adaptive_tools_until(|| true).unwrap());
        assert_status(&api, false, Some("existing-scan-error"));
        assert_eq!(
            api.adaptive_tool_progress_error.lock().unwrap().as_deref(),
            Some("existing-tool-error")
        );
        assert!(api.adaptive_tool_progress_cursor.lock().unwrap().is_none());
        assert!(api.reconcile_adaptive_tools_until(|| false).unwrap());
        assert!(api.adaptive_tool_progress_error.lock().unwrap().is_none());
        assert_status(&api, false, Some("existing-scan-error"));
    }

    #[test]
    fn poisoned_tool_progress_fences_and_hints_fail_closed_in_health() {
        for poison_hints in [false, true] {
            let api = api();
            assert!(catch_unwind(AssertUnwindSafe(|| {
                if poison_hints {
                    let _guard = api.adaptive_tool_progress_hints.lock().unwrap();
                    panic!("poison hints");
                } else {
                    let _guard = api.adaptive_tool_progress_fence.lock().unwrap();
                    panic!("poison progress fence");
                }
            }))
            .is_err());
            assert!(api.reconcile_adaptive_tools_until(|| false).is_err());
            assert!(api.adaptive_tool_progress_error.lock().unwrap().is_some());
            assert!(!api.health().ready);
            assert!(api.health().last_error.is_some());
            assert_status(&api, false, None);
        }
    }

    #[test]
    fn tool_progress_cancellation_after_failure_retains_health_error() {
        let api = api();
        *api.adaptive_tool_progress_hints.lock().unwrap() = Arc::new(vec![(
            TenantId::parse("tenant-progress-missing").unwrap(),
            ProjectId::parse("project-progress-missing").unwrap(),
            WorkItemId::parse("work-progress-missing").unwrap(),
            AgentId(7),
        )]);
        let checks = AtomicUsize::new(0);
        let cancelled = AtomicBool::new(false);
        let stop = || {
            if checks.fetch_add(1, Ordering::SeqCst) == 2 {
                // Allow the selected attempt, then observe cancellation after its failure.
                cancelled.store(true, Ordering::SeqCst);
                return false;
            }
            cancelled.load(Ordering::SeqCst)
        };
        assert!(api.reconcile_adaptive_tools_until(stop).is_err());
        assert!(cancelled.load(Ordering::SeqCst));
        assert!(api.adaptive_tool_progress_error.lock().unwrap().is_some());
        assert_eq!(api.adaptive_tool_progress_failures.lock().unwrap().len(), 1);
        assert!(!api.health().ready);
        assert!(api.health().last_error.is_some());
        assert_status(&api, false, None);
    }
}

fn enabled_api() -> WorkflowApi {
    let mut api = WorkflowApi::disabled().unwrap();
    api.enabled = true;
    api
}

fn assert_status(api: &WorkflowApi, succeeded: bool, error: Option<&str>) {
    let status = api.reconciliation_status_snapshot();
    assert_eq!(status.scan_succeeded, succeeded);
    assert_eq!(status.last_error.as_deref(), error);
}

fn poison_status(api: &WorkflowApi) {
    assert!(catch_unwind(AssertUnwindSafe(|| {
        let _status = api.reconciliation_status.lock().unwrap();
        panic!("poison reconciliation status");
    }))
    .is_err());
    assert!(api.reconciliation_status.is_poisoned());
}

#[test]
fn reconciliation_status_initial_and_stopped_scan_remain_not_ready() {
    let api = enabled_api();
    assert_status(&api, false, None);
    let health = api.health();
    assert!(!health.ready);
    assert_eq!(health.status, "degraded");
    assert!(health.last_error.is_none());

    api.reconcile_pending_until(|| true);
    assert_status(&api, false, None);
    assert!(!api.health().ready);
}

#[test]
fn reconciliation_status_publishes_coherent_success_and_failure() {
    let api = enabled_api();
    api.publish_reconciliation_status(None);
    let success = api.reconciliation_status_snapshot();
    assert_status(&api, true, None);

    api.publish_reconciliation_status(Some("PersistenceFailure".to_owned()));
    let failure = api.reconciliation_status_snapshot();
    assert_status(&api, false, Some("PersistenceFailure"));
    assert!(success.scan_succeeded);
    assert!(success.last_error.is_none());
    let health = api.health();
    assert!(!health.ready);
    assert_eq!(health.last_error.as_deref(), Some("PersistenceFailure"));

    api.publish_reconciliation_status(None);
    assert_status(&api, true, None);
    assert!(!failure.scan_succeeded);
    assert_eq!(failure.last_error.as_deref(), Some("PersistenceFailure"));
    assert!(api.health().last_error.is_none());
}

#[test]
fn failed_reconciliation_batch_publishes_its_actual_failure() {
    let api = enabled_api();
    api.publish_reconciliation_status(None);

    // Missing EventStore makes the real collaboration-publication step fail.
    api.reconcile_pending_until(|| false);
    assert_status(&api, false, Some("PersistenceFailure"));
    let health = api.health();
    assert!(!health.ready);
    assert_eq!(health.last_error.as_deref(), Some("PersistenceFailure"));
}

#[test]
fn terminal_company_worker_failure_cannot_be_cleared_by_a_later_healthy_pass() {
    let api = enabled_api();
    api.publish_reconciliation_status(None);
    assert_status(&api, true, None);
    api.mark_company_reconciliation_worker_failed();
    for _ in 0..2 {
        api.publish_reconciliation_status(None);
        assert_status(&api, false, Some("company_reconciliation_worker_failed"));
        let health = api.health();
        assert!(!health.ready);
        assert_eq!(
            health.last_error.as_deref(),
            Some("company_reconciliation_worker_failed")
        );
    }
}

#[test]
fn terminal_company_worker_failure_is_visible_before_first_scan_and_with_poisoned_status() {
    let api = enabled_api();
    poison_status(&api);
    api.mark_company_reconciliation_worker_failed();
    assert_status(&api, false, Some("company_reconciliation_worker_failed"));
    assert!(!api.health().ready);
    assert_eq!(
        api.health().last_error.as_deref(),
        Some("company_reconciliation_worker_failed")
    );
}

#[test]
fn reconciliation_phase_observation_preserves_values_errors_and_single_execution() {
    let api = enabled_api();
    let calls = AtomicUsize::new(0);
    let success: Result<u16, &'static str> = api.trace_reconciliation_phase("fixture", || {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(7)
    });
    assert_eq!(success, Ok(7));
    let failure: Result<u16, &'static str> = api.trace_reconciliation_phase("fixture", || {
        calls.fetch_add(1, Ordering::SeqCst);
        Err("fixture failure")
    });
    assert_eq!(failure, Err("fixture failure"));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_status(&api, false, None);
}

#[test]
fn cancellation_after_publication_or_inventory_never_publishes_full_scan_success() {
    for stop_at in [1, 2] {
        for previous in [None, Some(None), Some(Some("existing error"))] {
            let mut api = enabled_api();
            api.event_store = Some(sentinel_limbo::EventStore::open(":memory:").unwrap());
            if let Some(previous) = previous {
                api.publish_reconciliation_status(previous.map(str::to_owned));
            }
            let before = api.reconciliation_status_snapshot();
            let calls = AtomicUsize::new(0);
            api.reconcile_pending_until(|| calls.fetch_add(1, Ordering::SeqCst) >= stop_at);
            let after = api.reconciliation_status_snapshot();
            assert_eq!(after.scan_succeeded, before.scan_succeeded);
            assert_eq!(after.last_error, before.last_error);
        }
    }
}

#[test]
fn failed_scan_error_is_not_suppressed_by_a_later_cancellation() {
    let api = enabled_api();
    api.publish_reconciliation_status(None);
    let calls = AtomicUsize::new(0);
    api.reconcile_pending_until(|| calls.fetch_add(1, Ordering::SeqCst) > 0);
    assert_status(&api, false, Some("PersistenceFailure"));
}

fn publication_api() -> WorkflowApi {
    let mut api = enabled_api();
    api.event_store = Some(sentinel_limbo::EventStore::open(":memory:").unwrap());
    api
}

fn completed_empty_batch_stop_checks() -> usize {
    let api = publication_api();
    let checks = AtomicUsize::new(0);
    api.reconcile_batch(&|| {
        checks.fetch_add(1, Ordering::SeqCst);
        false
    })
    .unwrap();
    checks.load(Ordering::SeqCst)
}

#[test]
fn completed_scan_publishes_success_without_sampling_a_new_final_stop() {
    let boundary_checks = completed_empty_batch_stop_checks();
    assert!(boundary_checks > 0);
    for previous in [None, Some(None), Some(Some("existing error"))] {
        let api = publication_api();
        if let Some(previous) = previous {
            api.publish_reconciliation_status(previous.map(str::to_owned));
        }
        let checks = AtomicUsize::new(0);
        // The entry check plus all real batch boundaries allow completion.
        // Only an extra, post-completion cancellation sample would return true.
        api.reconcile_pending_until(|| checks.fetch_add(1, Ordering::SeqCst) > boundary_checks);
        assert_eq!(checks.load(Ordering::SeqCst), boundary_checks + 1);
        assert_status(&api, true, None);
    }
}

#[test]
fn cancellation_at_every_actual_empty_batch_boundary_preserves_previous_status() {
    let boundary_checks = completed_empty_batch_stop_checks();
    for stop_at in 0..=boundary_checks {
        for previous in [None, Some(None), Some(Some("existing error"))] {
            let api = publication_api();
            if let Some(previous) = previous {
                api.publish_reconciliation_status(previous.map(str::to_owned));
            }
            let before = api.reconciliation_status_snapshot();
            let checks = AtomicUsize::new(0);
            api.reconcile_pending_until(|| checks.fetch_add(1, Ordering::SeqCst) >= stop_at);
            let after = api.reconciliation_status_snapshot();
            assert_eq!(
                after.scan_succeeded, before.scan_succeeded,
                "stop {stop_at}"
            );
            assert_eq!(after.last_error, before.last_error, "stop {stop_at}");
        }
    }
}

#[test]
fn poisoned_publication_fence_is_unknown_and_cannot_grant_readiness() {
    let api = publication_api();
    api.publish_reconciliation_status(None);
    assert!(catch_unwind(AssertUnwindSafe(|| {
        let _publication = api.collaboration_publication_fence.lock().unwrap();
        panic!("poison collaboration publication fence");
    }))
    .is_err());
    assert!(api.collaboration_publication_fence.is_poisoned());
    assert_eq!(
        api.collaboration_publication_pending
            .load(Ordering::Acquire),
        0
    );
    let health = api.health();
    assert!(!health.ready);
    assert_eq!(health.collaboration_publication_pending, usize::MAX);
    assert_eq!(
        health.last_error.as_deref(),
        Some("collaboration_publication_fence_poisoned")
    );
    for _ in 0..2 {
        api.collaboration_publication_pending
            .store(0, Ordering::Release);
        assert!(api.publish_collaboration_backlog().is_err());
        assert_eq!(
            api.collaboration_publication_pending
                .load(Ordering::Acquire),
            usize::MAX
        );
        assert!(!api.health().ready);
    }
    api.reconcile_pending_until(|| false);
    assert_status(&api, false, Some("PersistenceFailure"));
}

#[test]
fn contended_publication_fence_preserves_counter_until_snapshot_can_be_read() {
    let api = publication_api();
    api.collaboration_publication_pending
        .store(7, Ordering::Release);
    let guard = api.collaboration_publication_fence.lock().unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let api = &api;
        scope.spawn(move || {
            started_tx.send(()).unwrap();
            result_tx.send(api.publish_collaboration_backlog()).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(
            result_rx.recv_timeout(Duration::from_millis(50)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        assert_eq!(
            api.collaboration_publication_pending
                .load(Ordering::Acquire),
            7
        );
        // A writer's unknown marker must survive until the fenced snapshot is verified.
        api.collaboration_publication_pending
            .store(usize::MAX, Ordering::Release);
        drop(guard);
        result_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
    });
    assert_eq!(
        api.collaboration_publication_pending
            .load(Ordering::Acquire),
        0
    );
}

struct PublishDuringDependencyCheck {
    api: OnceLock<Weak<WorkflowApi>>,
    error: Option<String>,
}

impl sentinel_workflow::OrganizationRuntimePort for PublishDuringDependencyCheck {
    fn readiness(&self) -> DependencyReadiness {
        let api = self.api.get().unwrap().upgrade().unwrap();
        assert!(api.reconciliation_status.try_lock().is_ok());
        api.publish_reconciliation_status(self.error.clone());
        DependencyReadiness::Unavailable
    }

    fn authority_snapshot(
        &self,
        _tenant_id: &TenantId,
        _project_id: &ProjectId,
        _work_item_id: &WorkItemId,
        _agent_id: AgentId,
    ) -> Result<RuntimeAuthoritySnapshotV1, WorkflowPortError> {
        Err(WorkflowPortError::Unavailable)
    }
}

#[test]
fn health_samples_coherent_status_after_dependency_checks() {
    for (initial_error, next_error) in [
        (None, Some("publication failed")),
        (Some("publication failed"), None),
    ] {
        let mut api = enabled_api();
        api.publish_reconciliation_status(initial_error.map(str::to_owned));
        let publisher = Arc::new(PublishDuringDependencyCheck {
            api: OnceLock::new(),
            error: next_error.map(str::to_owned),
        });
        let organization: Arc<dyn sentinel_workflow::OrganizationRuntimePort> = publisher.clone();
        let execution: Arc<dyn WorkExecutionPort> =
            Arc::new(sentinel_workflow::UnavailableWorkExecutionPort);
        let completion: Arc<dyn CompletionEvidencePort> =
            Arc::new(sentinel_workflow::UnavailableCompletionEvidencePort);
        let gate: Arc<dyn GateEvidencePort> = Arc::new(UnavailableGateEvidencePort);
        api.core = Arc::new(WorkflowCore::new(
            Arc::clone(&api.store),
            organization,
            execution,
            completion,
            gate,
        ));
        let api = Arc::new(api);
        publisher.api.set(Arc::downgrade(&api)).unwrap();

        let health = api.health();
        assert_eq!(health.last_error.as_deref(), next_error);
        assert_status(&api, next_error.is_none(), next_error);
        assert!(!health.ready);
    }
}

#[test]
fn poisoned_successful_reconciliation_status_fails_closed() {
    let api = enabled_api();
    api.publish_reconciliation_status(None);
    poison_status(&api);

    for _ in 0..2 {
        assert_status(&api, false, Some("reconciliation_status_poisoned"));
        let health = api.health();
        assert!(!health.ready);
        assert_eq!(health.status, "degraded");
        assert_eq!(
            health.last_error.as_deref(),
            Some("reconciliation_status_poisoned")
        );
        api.publish_reconciliation_status(None);
        assert!(api.reconciliation_status.is_poisoned());
    }
}

#[test]
fn poisoned_failed_reconciliation_status_preserves_actual_error() {
    let api = enabled_api();
    api.publish_reconciliation_status(Some("collaboration publication failed".to_owned()));
    poison_status(&api);
    api.publish_reconciliation_status(None);

    assert_status(&api, false, Some("collaboration publication failed"));
    let health = api.health();
    assert!(!health.ready);
    assert_eq!(
        health.last_error.as_deref(),
        Some("collaboration publication failed")
    );
}

#[test]
fn poisoned_reconciliation_fence_invalidates_successful_status() {
    let api = enabled_api();
    api.publish_reconciliation_status(None);
    assert!(catch_unwind(AssertUnwindSafe(|| {
        let _batch = api.reconciliation_fence.lock().unwrap();
        panic!("poison reconciliation fence");
    }))
    .is_err());

    for _ in 0..2 {
        api.reconcile_pending_until(|| panic!("poisoned fence must not enter the batch"));
        assert_status(&api, false, Some("reconciliation_fence_poisoned"));
        let health = api.health();
        assert!(!health.ready);
        assert_eq!(health.status, "degraded");
        assert_eq!(
            health.last_error.as_deref(),
            Some("reconciliation_fence_poisoned")
        );
        assert!(api.reconciliation_fence.is_poisoned());
        assert!(!api.reconciliation_status.is_poisoned());
    }
}

#[test]
fn poisoned_mutation_fence_invalidates_successful_status() {
    let api = enabled_api();
    api.publish_reconciliation_status(None);
    assert!(catch_unwind(AssertUnwindSafe(|| {
        let _recovery = api.mutation_fence.write().unwrap();
        panic!("poison mutation fence");
    }))
    .is_err());

    for _ in 0..2 {
        api.reconcile_pending_until(|| panic!("poisoned fence must not enter the batch"));
        assert_status(&api, false, Some("mutation_fence_poisoned"));
        let health = api.health();
        assert!(!health.ready);
        assert_eq!(health.status, "degraded");
        assert_eq!(
            health.last_error.as_deref(),
            Some("mutation_fence_poisoned")
        );
        assert!(api.mutation_fence.is_poisoned());
        assert!(api.reconciliation_fence.try_lock().is_ok());
        assert!(!api.reconciliation_status.is_poisoned());
    }
}

#[test]
fn active_reconciliation_batch_preserves_last_status() {
    for error in [None, Some("PersistenceFailure")] {
        let api = enabled_api();
        api.publish_reconciliation_status(error.map(str::to_owned));
        let batch = api.reconciliation_fence.lock().unwrap();

        api.reconcile_pending_until(|| panic!("active batch must not be reentered"));
        assert_status(&api, error.is_none(), error);
        assert_eq!(api.health().last_error.as_deref(), error);

        drop(batch);
        api.publish_reconciliation_status(Some("active batch failed".to_owned()));
        assert_status(&api, false, Some("active batch failed"));
    }
}

#[test]
fn active_mutation_writer_preserves_last_status() {
    for error in [None, Some("PersistenceFailure")] {
        let api = enabled_api();
        api.publish_reconciliation_status(error.map(str::to_owned));
        let recovery = api.mutation_fence.write().unwrap();

        api.reconcile_pending_until(|| panic!("active recovery must not be entered"));
        assert_status(&api, error.is_none(), error);
        assert_eq!(api.health().last_error.as_deref(), error);
        assert!(api.reconciliation_fence.try_lock().is_ok());

        drop(recovery);
        api.publish_reconciliation_status(None);
        assert_status(&api, true, None);
    }
}

#[test]
fn shared_mutation_reader_does_not_block_reconciliation() {
    let api = enabled_api();
    api.publish_reconciliation_status(None);
    let _dispatch = api.mutation_fence.read().unwrap();

    api.reconcile_pending_until(|| false);
    assert_status(&api, false, Some("PersistenceFailure"));
}
