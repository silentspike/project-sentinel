use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::WorkflowApi;

const POLL_INTERVAL: Duration = Duration::from_millis(250);
const RECONCILE_FAILED: &str = "adaptive_tool_progress_reconcile_failed";
const WORKER_PANICKED: &str = "adaptive_tool_progress_worker_panicked";

pub(crate) struct AdaptiveToolProgressWorker {
    local_stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), &'static str>>>,
    mark_failed: Arc<dyn Fn() + Send + Sync>,
    joined_result: Option<Result<(), &'static str>>,
}

impl AdaptiveToolProgressWorker {
    pub(crate) fn spawn(
        api: Arc<WorkflowApi>,
        external_shutdown: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        let failure_api = Arc::clone(&api);
        Self::spawn_poll(
            external_shutdown,
            move |should_stop| {
                api.reconcile_adaptive_tools_until(should_stop)
                    .map_err(|_| RECONCILE_FAILED)
            },
            move || failure_api.mark_adaptive_tool_progress_worker_failed(),
        )
    }

    fn spawn_poll<P, F>(
        external_shutdown: Arc<AtomicBool>,
        mut poll: P,
        mark_failed: F,
    ) -> std::io::Result<Self>
    where
        P: FnMut(&dyn Fn() -> bool) -> Result<bool, &'static str> + Send + 'static,
        F: Fn() + Send + Sync + 'static,
    {
        let local_stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&local_stop);
        let mark_failed: Arc<dyn Fn() + Send + Sync> = Arc::new(mark_failed);
        let thread_failed = Arc::clone(&mark_failed);
        let thread = thread::Builder::new()
            .name("adaptive-tool-progress".into())
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    let should_stop = || {
                        external_shutdown.load(Ordering::Acquire)
                            || thread_stop.load(Ordering::Acquire)
                    };
                    while !should_stop() {
                        if poll(&should_stop).is_err() {
                            tracing::warn!(
                                code = RECONCILE_FAILED,
                                "Adaptive tool progress pass failed"
                            );
                        }
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
                            "Adaptive tool progress worker stopped"
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
            // Reconciliation may still be inside a finite exchange. Never
            // substitute a deadline or dropped handle for actual termination.
            let result = match handle.join() {
                Ok(result) => result,
                Err(_) => {
                    (self.mark_failed)();
                    tracing::error!(
                        code = WORKER_PANICKED,
                        "Adaptive tool progress worker join failed"
                    );
                    Err(WORKER_PANICKED)
                }
            };
            self.joined_result = Some(result);
        }
        self.joined_result.unwrap_or(Ok(()))
    }
}

impl Drop for AdaptiveToolProgressWorker {
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

    fn wait_for_stop(stop: &AtomicBool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !stop.load(Ordering::Acquire) {
            assert!(
                Instant::now() < deadline,
                "owner did not request worker stop"
            );
            thread::yield_now();
        }
    }

    #[test]
    fn worker_panic_after_healthy_empty_progress_publishes_terminal_failure() {
        let mut api = WorkflowApi::disabled().unwrap();
        api.enabled = true;
        api.model_work_enabled = true;
        let api = Arc::new(api);
        assert_eq!(api.adaptive_tool_progress_health_error(), None);
        let poll_api = Arc::clone(&api);
        let failure_api = Arc::clone(&api);
        let polls = Arc::new(AtomicUsize::new(0));
        let poll_count = Arc::clone(&polls);
        let failures = Arc::new(AtomicUsize::new(0));
        let failure_count = Arc::clone(&failures);
        let (healthy_tx, healthy_rx) = mpsc::channel();
        let (failed_tx, failed_rx) = mpsc::channel();
        let mut worker = AdaptiveToolProgressWorker::spawn_poll(
            Arc::new(AtomicBool::new(false)),
            move |should_stop| {
                if poll_count.fetch_add(1, Ordering::SeqCst) == 0 {
                    let result = poll_api
                        .reconcile_adaptive_tools_until(should_stop)
                        .map_err(|_| RECONCILE_FAILED);
                    assert_eq!(result, Ok(true));
                    healthy_tx.send(()).unwrap();
                    result
                } else {
                    panic!("synthetic worker panic after healthy empty progress");
                }
            },
            move || {
                failure_api.mark_adaptive_tool_progress_worker_failed();
                failure_count.fetch_add(1, Ordering::SeqCst);
                failed_tx.send(()).unwrap();
            },
        )
        .unwrap();
        healthy_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        failed_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        // No further reconciliation invocation is needed to publish failure.
        assert_eq!(
            api.adaptive_tool_progress_health_error().as_deref(),
            Some("adaptive_tool_progress_worker_failed"),
        );
        assert!(!api.health().ready);
        assert!(api.health().last_error.is_some());
        assert_eq!(worker.stop_and_join(), Err(WORKER_PANICKED));
        assert_eq!(worker.stop_and_join(), Err(WORKER_PANICKED));
        drop(worker);
        assert_eq!(polls.load(Ordering::SeqCst), 2);
        assert_eq!(failures.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn stop_and_join_retains_blocked_pass_until_actual_release() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let polls = Arc::new(AtomicUsize::new(0));
        let poll_count = Arc::clone(&polls);
        let mut worker = AdaptiveToolProgressWorker::spawn_poll(
            Arc::new(AtomicBool::new(false)),
            move |_| {
                poll_count.fetch_add(1, Ordering::SeqCst);
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(true)
            },
            || panic!("healthy blocked worker must not publish terminal failure"),
        )
        .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let stop = Arc::clone(&worker.local_stop);
        let (joined_tx, joined_rx) = mpsc::channel();
        let owner = thread::spawn(move || {
            let result = worker.stop_and_join();
            joined_tx.send(result).unwrap();
        });
        wait_for_stop(&stop);
        // A controlled pass models any exchange exceeding the old six-second
        // deadline without a sleep: ownership cannot end before this release.
        let before_release = joined_rx.try_recv();
        release_tx.send(()).unwrap();
        let result = joined_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        owner.join().unwrap();
        assert_eq!(before_release, Err(mpsc::TryRecvError::Empty));
        assert_eq!(result, Ok(()));
        assert_eq!(polls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn owner_drop_on_later_startup_failure_joins_blocked_worker() {
        fn fail_later(_worker: AdaptiveToolProgressWorker) -> Result<(), &'static str> {
            Err("synthetic_later_startup_failure")
        }

        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let polls = Arc::new(AtomicUsize::new(0));
        let poll_count = Arc::clone(&polls);
        let worker = AdaptiveToolProgressWorker::spawn_poll(
            Arc::new(AtomicBool::new(false)),
            move |_| {
                poll_count.fetch_add(1, Ordering::SeqCst);
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(true)
            },
            || panic!("owner drop must not publish a healthy pass as failed"),
        )
        .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let stop = Arc::clone(&worker.local_stop);
        let (returned_tx, returned_rx) = mpsc::channel();
        let owner = thread::spawn(move || {
            returned_tx.send(fail_later(worker)).unwrap();
        });
        wait_for_stop(&stop);
        let before_release = returned_rx.try_recv();
        release_tx.send(()).unwrap();
        let result = returned_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        owner.join().unwrap();
        assert_eq!(before_release, Err(mpsc::TryRecvError::Empty));
        assert_eq!(result, Err("synthetic_later_startup_failure"));
        assert_eq!(polls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn external_shutdown_before_spawn_never_polls_or_restarts() {
        let polls = Arc::new(AtomicUsize::new(0));
        let poll_count = Arc::clone(&polls);
        let mut worker = AdaptiveToolProgressWorker::spawn_poll(
            Arc::new(AtomicBool::new(true)),
            move |_| {
                poll_count.fetch_add(1, Ordering::SeqCst);
                Ok(true)
            },
            || panic!("external shutdown must not publish terminal failure"),
        )
        .unwrap();
        assert_eq!(worker.stop_and_join(), Ok(()));
        assert_eq!(worker.stop_and_join(), Ok(()));
        drop(worker);
        assert_eq!(polls.load(Ordering::SeqCst), 0);
    }
}
