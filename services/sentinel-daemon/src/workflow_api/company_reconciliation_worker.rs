use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::WorkflowApi;

const POLL_INTERVAL: Duration = Duration::from_millis(250);
const WORKER_PANICKED: &str = "company_reconciliation_worker_panicked";

pub(crate) struct CompanyReconciliationWorker {
    local_stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), &'static str>>>,
    mark_failed: Arc<dyn Fn() + Send + Sync>,
    joined_result: Option<Result<(), &'static str>>,
}

impl CompanyReconciliationWorker {
    pub(crate) fn spawn(
        api: Arc<WorkflowApi>,
        external_shutdown: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        let failure_api = Arc::clone(&api);
        Self::spawn_poll(
            external_shutdown,
            move |should_stop| api.reconcile_pending_until(should_stop),
            move || failure_api.mark_company_reconciliation_worker_failed(),
        )
    }

    fn spawn_poll<P, F>(
        external_shutdown: Arc<AtomicBool>,
        mut poll: P,
        mark_failed: F,
    ) -> std::io::Result<Self>
    where
        P: FnMut(&dyn Fn() -> bool) + Send + 'static,
        F: Fn() + Send + Sync + 'static,
    {
        let local_stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&local_stop);
        let mark_failed: Arc<dyn Fn() + Send + Sync> = Arc::new(mark_failed);
        let thread_failed = Arc::clone(&mark_failed);
        let thread = thread::Builder::new()
            .name("company-workflow-reconcile".into())
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    let should_stop = || {
                        external_shutdown.load(Ordering::Acquire)
                            || thread_stop.load(Ordering::Acquire)
                    };
                    while !should_stop() {
                        // The API publishes ordinary scan success/error. The
                        // worker must not convert a skipped pass into readiness.
                        poll(&should_stop);
                        if !should_stop() {
                            thread::park_timeout(POLL_INTERVAL);
                        }
                    }
                }));
                match result {
                    Ok(()) => Ok(()),
                    Err(_) => {
                        thread_failed();
                        tracing::error!(
                            code = WORKER_PANICKED,
                            "Company reconciliation worker stopped"
                        );
                        Err(WORKER_PANICKED)
                    }
                }
            })?;
        Ok(Self {
            local_stop,
            thread: Some(thread),
            mark_failed,
            joined_result: None,
        })
    }

    pub(crate) fn stop_and_join(&mut self) -> Result<(), &'static str> {
        self.local_stop.store(true, Ordering::Release);
        if let Some(handle) = self.thread.as_ref() {
            handle.thread().unpark();
        }
        if let Some(handle) = self.thread.take() {
            // A finite exchange may delay this join. A timeout or dropped
            // handle is never evidence that the mutation thread terminated.
            let result = match handle.join() {
                Ok(result) => result,
                Err(_) => {
                    (self.mark_failed)();
                    tracing::error!(
                        code = WORKER_PANICKED,
                        "Company reconciliation worker join failed"
                    );
                    Err(WORKER_PANICKED)
                }
            };
            self.joined_result = Some(result);
        }
        self.joined_result.unwrap_or(Ok(()))
    }
}

impl Drop for CompanyReconciliationWorker {
    fn drop(&mut self) {
        let _ = self.stop_and_join();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;
    use std::time::Instant;

    use super::*;

    fn enabled_api(with_events: bool) -> Arc<WorkflowApi> {
        let mut api = WorkflowApi::disabled().unwrap();
        api.enabled = true;
        if with_events {
            api.event_store = Some(sentinel_limbo::EventStore::open(":memory:").unwrap());
        }
        Arc::new(api)
    }

    fn wait_for_stop(stop: &AtomicBool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !stop.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "owner did not request stop");
            thread::yield_now();
        }
    }

    #[test]
    fn company_worker_panic_after_first_pass_publishes_terminal_failure() {
        let api = enabled_api(true);
        let poll_api = Arc::clone(&api);
        let failure_api = Arc::clone(&api);
        let polls = Arc::new(AtomicUsize::new(0));
        let poll_count = Arc::clone(&polls);
        let failures = Arc::new(AtomicUsize::new(0));
        let failure_count = Arc::clone(&failures);
        let (passed_tx, passed_rx) = mpsc::channel();
        let (failed_tx, failed_rx) = mpsc::channel();
        let mut worker = CompanyReconciliationWorker::spawn_poll(
            Arc::new(AtomicBool::new(false)),
            move |should_stop| {
                if poll_count.fetch_add(1, Ordering::SeqCst) == 0 {
                    poll_api.reconcile_pending_until(should_stop);
                    assert!(poll_api.reconciliation_status_snapshot().scan_succeeded);
                    passed_tx.send(()).unwrap();
                } else {
                    panic!("synthetic company worker panic after first pass");
                }
            },
            move || {
                failure_api.mark_company_reconciliation_worker_failed();
                failure_count.fetch_add(1, Ordering::SeqCst);
                failed_tx.send(()).unwrap();
            },
        )
        .unwrap();
        passed_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        failed_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let status = api.reconciliation_status_snapshot();
        assert!(!status.scan_succeeded);
        assert_eq!(
            status.last_error.as_deref(),
            Some("company_reconciliation_worker_failed")
        );
        assert_eq!(worker.stop_and_join(), Err(WORKER_PANICKED));
        assert_eq!(worker.stop_and_join(), Err(WORKER_PANICKED));
        drop(worker);
        assert_eq!(polls.load(Ordering::SeqCst), 2);
        assert_eq!(failures.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn company_worker_preserves_initial_error_and_success_until_actual_pass() {
        for with_events in [false, true] {
            let api = enabled_api(with_events);
            let poll_api = Arc::clone(&api);
            let external_stop = Arc::new(AtomicBool::new(false));
            let finish = Arc::clone(&external_stop);
            let observed_finish = Arc::clone(&external_stop);
            let (entered_tx, entered_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let mut worker = CompanyReconciliationWorker::spawn_poll(
                external_stop,
                move |should_stop| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    poll_api.reconcile_pending_until(should_stop);
                    finish.store(true, Ordering::Release);
                },
                || panic!("ordinary API result must not terminate the worker"),
            )
            .unwrap();
            entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            let initial = api.reconciliation_status_snapshot();
            assert!(!initial.scan_succeeded);
            assert!(initial.last_error.is_none());
            assert!(!api.health().ready);
            release_tx.send(()).unwrap();
            // Wait for the API publication before asking stop to cancel a pass.
            let deadline = Instant::now() + Duration::from_secs(2);
            while !observed_finish.load(Ordering::Acquire) {
                assert!(Instant::now() < deadline, "API pass did not finish");
                thread::yield_now();
            }
            assert_eq!(worker.stop_and_join(), Ok(()));
            let status = api.reconciliation_status_snapshot();
            assert_eq!(status.scan_succeeded, with_events);
            assert_eq!(
                status.last_error.as_deref(),
                if with_events {
                    None
                } else {
                    Some("PersistenceFailure")
                }
            );
        }
    }

    #[test]
    fn company_worker_later_startup_return_joins_blocked_pass() {
        fn fail_later(_worker: CompanyReconciliationWorker) -> Result<(), &'static str> {
            Err("synthetic_later_startup_failure")
        }
        let external_stop = Arc::new(AtomicBool::new(false));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let polls = Arc::new(AtomicUsize::new(0));
        let poll_count = Arc::clone(&polls);
        let worker = CompanyReconciliationWorker::spawn_poll(
            Arc::clone(&external_stop),
            move |_| {
                poll_count.fetch_add(1, Ordering::SeqCst);
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            },
            || panic!("healthy blocked pass must not publish terminal failure"),
        )
        .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let local_stop = Arc::clone(&worker.local_stop);
        let (returned_tx, returned_rx) = mpsc::channel();
        let owner = thread::spawn(move || {
            returned_tx.send(fail_later(worker)).unwrap();
        });
        wait_for_stop(&local_stop);
        let before_release = returned_rx.try_recv();
        release_tx.send(()).unwrap();
        let result = returned_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        owner.join().unwrap();
        assert_eq!(before_release, Err(mpsc::TryRecvError::Empty));
        assert_eq!(result, Err("synthetic_later_startup_failure"));
        assert!(!external_stop.load(Ordering::Acquire));
        assert_eq!(polls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn company_worker_stop_and_join_waits_for_blocked_pass() {
        let external_stop = Arc::new(AtomicBool::new(false));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut worker = CompanyReconciliationWorker::spawn_poll(
            Arc::clone(&external_stop),
            move |should_stop| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                assert!(should_stop());
            },
            || panic!("normal stop must not publish terminal failure"),
        )
        .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let local_stop = Arc::clone(&worker.local_stop);
        let (joined_tx, joined_rx) = mpsc::channel();
        let owner = thread::spawn(move || {
            let result = worker.stop_and_join();
            assert_eq!(worker.stop_and_join(), result);
            joined_tx.send(result).unwrap();
        });
        wait_for_stop(&local_stop);
        let before_release = joined_rx.try_recv();
        release_tx.send(()).unwrap();
        let result = joined_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        owner.join().unwrap();
        assert_eq!(before_release, Err(mpsc::TryRecvError::Empty));
        assert_eq!(result, Ok(()));
        assert!(!external_stop.load(Ordering::Acquire));
    }

    #[test]
    fn company_worker_external_shutdown_never_polls_or_restarts() {
        let polls = Arc::new(AtomicUsize::new(0));
        let poll_count = Arc::clone(&polls);
        let mut worker = CompanyReconciliationWorker::spawn_poll(
            Arc::new(AtomicBool::new(true)),
            move |_| {
                poll_count.fetch_add(1, Ordering::SeqCst);
            },
            || panic!("external shutdown is not a worker failure"),
        )
        .unwrap();
        assert_eq!(worker.stop_and_join(), Ok(()));
        assert_eq!(worker.stop_and_join(), Ok(()));
        drop(worker);
        assert_eq!(polls.load(Ordering::SeqCst), 0);
    }
}
