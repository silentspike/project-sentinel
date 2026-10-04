use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{OnceLock, Weak};

use super::*;

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
