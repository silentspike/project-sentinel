use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

const RECOVERY_QUEUE_CAPACITY: usize = 8;
const REAP_INTERVAL: Duration = Duration::from_millis(250);
const RESULT_DROP_PANICKED: &str = "blocking_result_drop_panicked";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Lane {
    Preparation,
    Provider,
    Recovery,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SubmitError {
    Closed,
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum JobError {
    Cancelled,
    DeadlineExceeded,
    Panicked,
    Stopped,
}

#[derive(Clone)]
pub(super) struct JobCancellation {
    inner: Arc<CancellationState>,
}

struct CancellationState {
    cancelled: AtomicBool,
    changed: Notify,
}

impl JobCancellation {
    fn new() -> Self {
        Self {
            inner: Arc::new(CancellationState {
                cancelled: AtomicBool::new(false),
                changed: Notify::new(),
            }),
        }
    }

    pub(super) fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::Release);
        self.inner.changed.notify_waiters();
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    pub(super) async fn cancelled(&self) {
        loop {
            let changed = self.inner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            changed.await;
        }
    }
}

struct Completion<T> {
    finished: bool,
    abandoned: bool,
    disposing: bool,
    result: Option<Result<T, JobError>>,
}

struct ResultState<T> {
    cancellation: JobCancellation,
    completion: Mutex<Completion<T>>,
    ready: Notify,
}

impl<T> ResultState<T> {
    fn finish(&self, result: Result<T, JobError>) {
        let mut completion = self.completion.lock().unwrap_or_else(|e| e.into_inner());
        completion.result = Some(result);
        completion.finished = true;
        drop(completion);
        self.ready.notify_waiters();
    }
}

trait OwnedCompletion: Send + Sync {
    fn cancel(&self);
    fn reap(&self, stopping: bool, disposal_failed: &AtomicBool) -> bool;
}

impl<T: Send + 'static> OwnedCompletion for ResultState<T> {
    fn cancel(&self) {
        self.cancellation.cancel();
    }

    fn reap(&self, stopping: bool, disposal_failed: &AtomicBool) -> bool {
        let result = {
            let mut completion = self.completion.lock().unwrap_or_else(|e| e.into_inner());
            if !completion.finished || completion.disposing {
                return false;
            }
            if !stopping && !completion.abandoned && completion.result.is_some() {
                return false;
            }
            completion.disposing = true;
            completion.result.take()
        };
        // User values and their guards are disposed only on an owned worker,
        // outside both the lane mutex and the result mutex.
        if catch_unwind(AssertUnwindSafe(|| drop(result))).is_err() {
            disposal_failed.store(true, Ordering::Release);
            tracing::error!(code = RESULT_DROP_PANICKED);
        }
        let mut completion = self.completion.lock().unwrap_or_else(|e| e.into_inner());
        if stopping && !completion.abandoned {
            completion.result = Some(Err(JobError::Stopped));
        }
        completion.disposing = false;
        drop(completion);
        self.ready.notify_waiters();
        true
    }
}

struct QueuedJob {
    run: Box<dyn FnOnce() + Send>,
}

struct LaneState {
    closed: bool,
    queue: VecDeque<QueuedJob>,
    completions: Vec<Arc<dyn OwnedCompletion>>,
}

struct LaneShared {
    queue_capacity: usize,
    outstanding_capacity: usize,
    disposal_failed: AtomicBool,
    state: Mutex<LaneState>,
    changed: Condvar,
}

impl LaneShared {
    fn new(queue_capacity: usize, workers: usize) -> std::io::Result<Self> {
        let outstanding_capacity = queue_capacity.checked_add(workers).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "blocking lane capacity overflow",
            )
        })?;
        Ok(Self {
            queue_capacity,
            outstanding_capacity,
            disposal_failed: AtomicBool::new(false),
            state: Mutex::new(LaneState {
                closed: false,
                queue: VecDeque::new(),
                completions: Vec::new(),
            }),
            changed: Condvar::new(),
        })
    }

    fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = true;
        for completion in &state.completions {
            completion.cancel();
        }
        drop(state);
        self.changed.notify_all();
    }
}

#[derive(Clone)]
pub(super) struct BlockingExecutor {
    lanes: [Arc<LaneShared>; 3],
    accepting: Arc<AtomicBool>,
}

impl BlockingExecutor {
    fn lane(&self, lane: Lane) -> &Arc<LaneShared> {
        &self.lanes[match lane {
            Lane::Preparation => 0,
            Lane::Provider => 1,
            Lane::Recovery => 2,
        }]
    }

    pub(super) fn submit<T: Send + 'static>(
        &self,
        lane: Lane,
        deadline: Option<Instant>,
        job: impl FnOnce(JobCancellation) -> T + Send + 'static,
    ) -> Result<BlockingJob<T>, SubmitError> {
        self.submit_inner(lane, deadline, false, job)
    }

    // Accepted durable work must execute and classify itself even when its
    // receiver disappears or the owner closes intake. Cancellation is advisory.
    // Full/Closed rejects ownership: the closure drops on the submitting thread.
    // Construct armed durable guards inside accepted jobs, not before submission.
    pub(super) fn submit_required<T: Send + 'static>(
        &self,
        lane: Lane,
        job: impl FnOnce(JobCancellation) -> T + Send + 'static,
    ) -> Result<BlockingJob<T>, SubmitError> {
        self.submit_inner(lane, None, true, job)
    }

    fn submit_inner<T, F>(
        &self,
        lane: Lane,
        deadline: Option<Instant>,
        required: bool,
        job: F,
    ) -> Result<BlockingJob<T>, SubmitError>
    where
        T: Send + 'static,
        F: FnOnce(JobCancellation) -> T + Send + 'static,
    {
        let lane = Arc::clone(self.lane(lane));
        let mut queue = lane.state.lock().unwrap_or_else(|e| e.into_inner());
        if !self.accepting.load(Ordering::Acquire) || queue.closed {
            return Err(SubmitError::Closed);
        }
        // Retained unclaimed results also occupy bounded admission slots.
        if queue.queue.len() >= lane.queue_capacity
            || queue.completions.len() >= lane.outstanding_capacity
        {
            return Err(SubmitError::Full);
        }
        let state = Arc::new(ResultState {
            cancellation: JobCancellation::new(),
            completion: Mutex::new(Completion {
                finished: false,
                abandoned: false,
                disposing: false,
                result: None,
            }),
            ready: Notify::new(),
        });
        let result_state = Arc::clone(&state);
        let cancellation = state.cancellation.clone();
        let run = Box::new(move || {
            let result = catch_unwind(AssertUnwindSafe(move || {
                if !required {
                    if cancellation.is_cancelled() {
                        return Err(JobError::Cancelled);
                    }
                    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                        return Err(JobError::DeadlineExceeded);
                    }
                }
                Ok(job(cancellation))
            }))
            .unwrap_or_else(|_| {
                tracing::error!(code = "blocking_job_panicked");
                Err(JobError::Panicked)
            });
            result_state.finish(result);
        });
        queue.completions.push(state.clone());
        queue.queue.push_back(QueuedJob { run });
        drop(queue);
        lane.changed.notify_one();
        Ok(BlockingJob { state, lane })
    }
}

pub(super) struct BlockingJob<T: Send + 'static> {
    state: Arc<ResultState<T>>,
    lane: Arc<LaneShared>,
}

impl<T: Send + 'static> BlockingJob<T> {
    pub(super) fn cancellation(&self) -> JobCancellation {
        self.state.cancellation.clone()
    }

    pub(super) async fn wait(self) -> Result<T, JobError> {
        loop {
            let ready = self.state.ready.notified();
            tokio::pin!(ready);
            ready.as_mut().enable();
            let result = {
                let mut completion = self
                    .state
                    .completion
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if completion.finished && !completion.disposing {
                    completion.result.take()
                } else {
                    None
                }
            };
            if let Some(result) = result {
                self.lane.changed.notify_all();
                return result;
            }
            ready.await;
        }
    }
}

impl<T: Send + 'static> Drop for BlockingJob<T> {
    fn drop(&mut self) {
        self.state.cancellation.cancel();
        self.state
            .completion
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .abandoned = true;
        self.lane.changed.notify_all();
    }
}

pub(super) struct OwnedBlockingLanes {
    executor: BlockingExecutor,
    threads: Vec<JoinHandle<()>>,
    joined_result: Option<Result<(), &'static str>>,
}

impl OwnedBlockingLanes {
    // Production passes queue_capacity=32. Recovery always has eight queue slots.
    pub(super) fn spawn(provider_workers: usize, queue_capacity: usize) -> std::io::Result<Self> {
        if provider_workers == 0 || queue_capacity == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "blocking lane workers and queue capacity must be positive",
            ));
        }
        let executor = BlockingExecutor {
            accepting: Arc::new(AtomicBool::new(true)),
            lanes: [
                Arc::new(LaneShared::new(queue_capacity, 1)?),
                Arc::new(LaneShared::new(queue_capacity, provider_workers)?),
                Arc::new(LaneShared::new(RECOVERY_QUEUE_CAPACITY, 1)?),
            ],
        };
        let mut owner = Self {
            executor,
            threads: Vec::new(),
            joined_result: None,
        };
        for (lane, workers, name) in [
            (Lane::Preparation, 1, "llm-preparation"),
            (Lane::Provider, provider_workers, "llm-provider"),
            (Lane::Recovery, 1, "llm-recovery"),
        ] {
            for index in 0..workers {
                let shared = Arc::clone(owner.executor.lane(lane));
                match thread::Builder::new()
                    .name(format!("{name}-{index}"))
                    .spawn(move || worker_loop(shared))
                {
                    Ok(handle) => owner.threads.push(handle),
                    Err(error) => {
                        let _ = owner.stop_and_join();
                        return Err(error);
                    }
                }
            }
        }
        Ok(owner)
    }

    pub(super) fn executor(&self) -> BlockingExecutor {
        self.executor.clone()
    }

    // This join, including Drop's fallback, must run outside any runtime thread
    // needed by a job's Handle::block_on I/O/timer driver. A caller may move
    // the owner into one awaited shutdown spawn_blocking, never detach that join.
    pub(super) fn stop_and_join(&mut self) -> Result<(), &'static str> {
        self.executor.accepting.store(false, Ordering::Release);
        for lane in &self.executor.lanes {
            lane.close();
        }
        let mut result = self.joined_result.unwrap_or(Ok(()));
        for handle in self.threads.drain(..) {
            if handle.join().is_err() {
                result = Err("provider_blocking_worker_panicked");
            }
        }
        if result.is_ok()
            && self
                .executor
                .lanes
                .iter()
                .any(|lane| lane.disposal_failed.load(Ordering::Acquire))
        {
            result = Err(RESULT_DROP_PANICKED);
        }
        self.joined_result = Some(result);
        result
    }
}

impl Drop for OwnedBlockingLanes {
    fn drop(&mut self) {
        let _ = self.stop_and_join();
    }
}

fn worker_loop(lane: Arc<LaneShared>) {
    loop {
        let (completions, stopping) = {
            let state = lane.state.lock().unwrap_or_else(|e| e.into_inner());
            (state.completions.clone(), state.closed)
        };
        let reaped: Vec<_> = completions
            .into_iter()
            .filter(|completion| completion.reap(stopping, &lane.disposal_failed))
            .collect();
        let mut state = lane.state.lock().unwrap_or_else(|e| e.into_inner());
        state.completions.retain(|completion| {
            !reaped
                .iter()
                .any(|removed| Arc::ptr_eq(completion, removed))
        });
        if let Some(job) = state.queue.pop_front() {
            drop(state);
            (job.run)();
            lane.changed.notify_all();
        } else if state.closed && state.completions.is_empty() {
            break;
        } else {
            // Result claims/drop do not need the lane lock. A periodic reap
            // also covers a notification arriving just before this wait.
            drop(
                lane.changed
                    .wait_timeout(state, REAP_INTERVAL)
                    .unwrap_or_else(|e| e.into_inner()),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;

    use super::*;

    struct DropProbe(mpsc::Sender<thread::ThreadId>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            let _ = self.0.send(thread::current().id());
        }
    }

    struct PanickingDropProbe(mpsc::Sender<thread::ThreadId>);

    impl Drop for PanickingDropProbe {
        fn drop(&mut self) {
            let _ = self.0.send(thread::current().id());
            panic!("synthetic returned guard disposal panic");
        }
    }

    async fn entered(notify: &Notify) {
        tokio::time::timeout(Duration::from_secs(2), notify.notified())
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocking_closure_does_not_block_tokio_heartbeat() {
        let mut owner = OwnedBlockingLanes::spawn(3, 32).unwrap();
        let executor = owner.executor();
        let started = Arc::new(Notify::new());
        let worker_started = Arc::clone(&started);
        let (release_tx, release_rx) = mpsc::channel();
        let job = executor
            .submit(Lane::Preparation, None, move |_| {
                worker_started.notify_one();
                release_rx.recv().unwrap();
                7
            })
            .unwrap();
        entered(&started).await;
        let ticks = Arc::new(AtomicUsize::new(0));
        let heartbeat_ticks = Arc::clone(&ticks);
        let heartbeat = tokio::spawn(async move {
            for _ in 0..3 {
                tokio::time::sleep(Duration::from_millis(1)).await;
                heartbeat_ticks.fetch_add(1, Ordering::SeqCst);
            }
        });
        tokio::time::timeout(Duration::from_secs(2), heartbeat)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ticks.load(Ordering::SeqCst), 3);
        release_tx.send(()).unwrap();
        assert_eq!(job.wait().await, Ok(7));
        assert_eq!(owner.stop_and_join(), Ok(()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn preparation_saturation_preserves_provider_and_recovery_capacity() {
        let mut owner = OwnedBlockingLanes::spawn(3, 1).unwrap();
        let executor = owner.executor();
        let started = Arc::new(Notify::new());
        let worker_started = Arc::clone(&started);
        let (release_tx, release_rx) = mpsc::channel();
        let running = executor
            .submit(Lane::Preparation, None, move |_| {
                worker_started.notify_one();
                release_rx.recv().unwrap();
            })
            .unwrap();
        entered(&started).await;
        let queued = executor.submit(Lane::Preparation, None, |_| ()).unwrap();
        assert!(matches!(
            executor.submit(Lane::Preparation, None, |_| ()),
            Err(SubmitError::Full)
        ));
        let provider = executor.submit(Lane::Provider, None, |_| 3).unwrap();
        let recovery = executor.submit(Lane::Recovery, None, |_| 8).unwrap();
        assert_eq!(provider.wait().await, Ok(3));
        assert_eq!(recovery.wait().await, Ok(8));
        release_tx.send(()).unwrap();
        assert_eq!(running.wait().await, Ok(()));
        assert_eq!(queued.wait().await, Ok(()));
        assert_eq!(owner.stop_and_join(), Ok(()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prestart_deadline_and_cancellation_never_invoke_closure() {
        let mut owner = OwnedBlockingLanes::spawn(1, 32).unwrap();
        let executor = owner.executor();
        let started = Arc::new(Notify::new());
        let worker_started = Arc::clone(&started);
        let (release_tx, release_rx) = mpsc::channel();
        let blocker = executor
            .submit(Lane::Preparation, None, move |_| {
                worker_started.notify_one();
                release_rx.recv().unwrap();
                thread::current().id()
            })
            .unwrap();
        entered(&started).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let (dropped_tx, dropped_rx) = mpsc::channel();
        let expired_calls = Arc::clone(&calls);
        let expired_guard = DropProbe(dropped_tx.clone());
        let expired = executor
            .submit(Lane::Preparation, Some(Instant::now()), move |_| {
                let _guard = expired_guard;
                expired_calls.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap();
        let cancelled_calls = Arc::clone(&calls);
        let cancelled_guard = DropProbe(dropped_tx);
        let cancelled = executor
            .submit(Lane::Preparation, None, move |_| {
                let _guard = cancelled_guard;
                cancelled_calls.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap();
        cancelled.state.cancellation.cancel();
        release_tx.send(()).unwrap();
        let worker_id = blocker.wait().await.unwrap();
        assert_eq!(expired.wait().await, Err(JobError::DeadlineExceeded));
        assert_eq!(cancelled.wait().await, Err(JobError::Cancelled));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(owner.stop_and_join(), Ok(()));
        assert_eq!(dropped_rx.recv().unwrap(), worker_id);
        assert_eq!(dropped_rx.recv().unwrap(), worker_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn receiver_drop_retains_running_guard_until_worker_exit() {
        let mut owner = OwnedBlockingLanes::spawn(1, 32).unwrap();
        let executor = owner.executor();
        let started = Arc::new(Notify::new());
        let worker_started = Arc::clone(&started);
        let (release_tx, release_rx) = mpsc::channel();
        let (dropped_tx, dropped_rx) = mpsc::channel();
        let (signal_tx, signal_rx) = mpsc::channel();
        let guard = DropProbe(dropped_tx);
        let job = executor
            .submit(Lane::Provider, None, move |signal| {
                let _guard = guard;
                signal_tx.send((signal, thread::current().id())).unwrap();
                worker_started.notify_one();
                release_rx.recv().unwrap();
            })
            .unwrap();
        entered(&started).await;
        let (signal, worker_id) = signal_rx.recv().unwrap();
        drop(job);
        assert!(signal.is_cancelled());
        assert_eq!(dropped_rx.try_recv(), Err(mpsc::TryRecvError::Empty));
        release_tx.send(()).unwrap();
        assert_eq!(owner.stop_and_join(), Ok(()));
        assert_eq!(dropped_rx.recv().unwrap(), worker_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn running_panic_reports_failure_without_restarting_worker() {
        let mut owner = OwnedBlockingLanes::spawn(1, 32).unwrap();
        let executor = owner.executor();
        let (dropped_tx, dropped_rx) = mpsc::channel();
        let guard = DropProbe(dropped_tx);
        let failed = executor
            .submit::<()>(Lane::Preparation, None, move |_| {
                let _guard = guard;
                panic!("synthetic blocking job panic");
            })
            .unwrap();
        assert_eq!(failed.wait().await, Err(JobError::Panicked));
        let next = executor
            .submit(Lane::Preparation, None, |_| thread::current().id())
            .unwrap();
        assert_eq!(next.wait().await.unwrap(), dropped_rx.recv().unwrap());
        assert_eq!(owner.stop_and_join(), Ok(()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unclaimed_completed_result_is_disposed_on_worker() {
        let mut owner = OwnedBlockingLanes::spawn(1, 32).unwrap();
        let executor = owner.executor();
        let (dropped_tx, dropped_rx) = mpsc::channel();
        let (worker_tx, worker_rx) = mpsc::channel();
        let job = executor
            .submit(Lane::Provider, None, move |_| {
                worker_tx.send(thread::current().id()).unwrap();
                DropProbe(dropped_tx)
            })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if job.state.completion.lock().unwrap().finished {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let worker_id = worker_rx.recv().unwrap();
        assert_eq!(dropped_rx.try_recv(), Err(mpsc::TryRecvError::Empty));
        drop(job);
        assert_eq!(owner.stop_and_join(), Ok(()));
        assert_eq!(dropped_rx.recv().unwrap(), worker_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn abandoned_result_disposal_panic_is_retained_until_join() {
        let mut owner = OwnedBlockingLanes::spawn(1, 32).unwrap();
        let executor = owner.executor();
        let (dropped_tx, dropped_rx) = mpsc::channel();
        let (worker_tx, worker_rx) = mpsc::channel();
        let job = executor
            .submit(Lane::Provider, None, move |_| {
                worker_tx.send(thread::current().id()).unwrap();
                PanickingDropProbe(dropped_tx)
            })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !job.state.completion.lock().unwrap().finished {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let worker_id = worker_rx.recv().unwrap();
        drop(job);
        tokio::time::timeout(Duration::from_secs(2), async {
            while !executor
                .lane(Lane::Provider)
                .disposal_failed
                .load(Ordering::Acquire)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(dropped_rx.recv().unwrap(), worker_id);
        let next = executor
            .submit(Lane::Provider, None, |_| thread::current().id())
            .unwrap();
        assert_eq!(next.wait().await.unwrap(), worker_id);
        assert_eq!(owner.stop_and_join(), Err(RESULT_DROP_PANICKED));
        assert_eq!(owner.stop_and_join(), Err(RESULT_DROP_PANICKED));
    }

    #[test]
    fn rejected_required_submission_drops_unaccepted_closure_on_submitter() {
        let mut owner = OwnedBlockingLanes::spawn(1, 32).unwrap();
        let executor = owner.executor();
        assert_eq!(owner.stop_and_join(), Ok(()));
        let (dropped_tx, dropped_rx) = mpsc::channel();
        let guard = DropProbe(dropped_tx);
        let result = executor.submit_required::<()>(Lane::Recovery, move |_| {
            let _guard = guard;
            panic!("rejected required closure must not execute");
        });
        assert!(matches!(result, Err(SubmitError::Closed)));
        assert_eq!(dropped_rx.recv().unwrap(), thread::current().id());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wait_after_owner_shutdown_reports_stopped_without_retained_value() {
        let mut owner = OwnedBlockingLanes::spawn(1, 32).unwrap();
        let job = owner
            .executor()
            .submit_required(Lane::Provider, |_| 7)
            .unwrap();
        assert_eq!(owner.stop_and_join(), Ok(()));
        assert_eq!(job.wait().await, Err(JobError::Stopped));
    }

    #[test]
    fn provider_lane_runs_all_configured_workers_without_reduction() {
        let mut owner = OwnedBlockingLanes::spawn(3, 32).unwrap();
        let executor = owner.executor();
        let (entered_tx, entered_rx) = mpsc::channel();
        let mut releases = Vec::new();
        let mut jobs = Vec::new();
        for _ in 0..3 {
            let started = entered_tx.clone();
            let (release_tx, release_rx) = mpsc::channel();
            releases.push(release_tx);
            jobs.push(
                executor
                    .submit(Lane::Provider, None, move |_| {
                        started.send(thread::current().id()).unwrap();
                        release_rx.recv().unwrap();
                    })
                    .unwrap(),
            );
        }
        let mut workers = Vec::new();
        for _ in 0..3 {
            workers.push(entered_rx.recv_timeout(Duration::from_secs(2)).unwrap());
        }
        assert!(workers[0] != workers[1] && workers[1] != workers[2] && workers[0] != workers[2]);
        for release in releases {
            release.send(()).unwrap();
        }
        assert_eq!(owner.stop_and_join(), Ok(()));
        drop(jobs);
    }

    #[test]
    fn owner_drop_waits_for_running_guard_on_worker() {
        let owner = OwnedBlockingLanes::spawn(1, 32).unwrap();
        let executor = owner.executor();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (dropped_tx, dropped_rx) = mpsc::channel();
        let guard = DropProbe(dropped_tx);
        let job = executor
            .submit(Lane::Provider, None, move |_| {
                let _guard = guard;
                entered_tx.send(thread::current().id()).unwrap();
                release_rx.recv().unwrap();
            })
            .unwrap();
        let worker_id = entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (returned_tx, returned_rx) = mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(owner);
            returned_tx.send(()).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while !executor.lane(Lane::Recovery).state.lock().unwrap().closed {
            assert!(Instant::now() < deadline, "owner Drop did not close intake");
            thread::yield_now();
        }
        assert_eq!(returned_rx.try_recv(), Err(mpsc::TryRecvError::Empty));
        assert_eq!(dropped_rx.try_recv(), Err(mpsc::TryRecvError::Empty));
        release_tx.send(()).unwrap();
        returned_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        dropper.join().unwrap();
        assert_eq!(dropped_rx.recv().unwrap(), worker_id);
        drop(job);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_notification_is_safe_before_and_during_wait() {
        let signal = JobCancellation::new();
        signal.cancel();
        tokio::time::timeout(Duration::from_secs(2), signal.cancelled())
            .await
            .unwrap();
        let signal = JobCancellation::new();
        let waiter_signal = signal.clone();
        let waiter = tokio::spawn(async move { waiter_signal.cancelled().await });
        tokio::task::yield_now().await;
        signal.cancel();
        tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn nested_runtime_wait_cancels_and_drops_guard_on_provider_worker() {
        let mut owner = OwnedBlockingLanes::spawn(1, 32).unwrap();
        let executor = owner.executor();
        let runtime = tokio::runtime::Handle::current();
        let started = Arc::new(Notify::new());
        let worker_started = Arc::clone(&started);
        let exited = Arc::new(Notify::new());
        let worker_exited = Arc::clone(&exited);
        let (dropped_tx, dropped_rx) = mpsc::channel();
        let (worker_tx, worker_rx) = mpsc::channel();
        let guard = DropProbe(dropped_tx);
        let job = executor
            .submit(Lane::Provider, None, move |signal| {
                let _guard = guard;
                worker_tx.send(thread::current().id()).unwrap();
                worker_started.notify_one();
                runtime.block_on(async {
                    tokio::select! {
                        _ = signal.cancelled() => {}
                        _ = std::future::pending::<()>() => unreachable!(),
                    }
                });
                worker_exited.notify_one();
            })
            .unwrap();
        entered(&started).await;
        let worker_id = worker_rx.recv().unwrap();
        drop(job);
        entered(&exited).await;
        assert_eq!(owner.stop_and_join(), Ok(()));
        assert_eq!(dropped_rx.recv().unwrap(), worker_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn explicit_cancellation_keeps_adapter_until_worker_retirement() {
        let mut owner = OwnedBlockingLanes::spawn(1, 32).unwrap();
        let executor = owner.executor();
        let runtime = tokio::runtime::Handle::current();
        let started = Arc::new(Notify::new());
        let worker_started = Arc::clone(&started);
        let (dropped_tx, dropped_rx) = mpsc::channel();
        let guard = DropProbe(dropped_tx);
        let job = executor
            .submit(Lane::Provider, None, move |signal| {
                let _guard = guard;
                worker_started.notify_one();
                runtime.block_on(signal.cancelled());
                thread::current().id()
            })
            .unwrap();
        let signal = job.cancellation();
        entered(&started).await;
        assert_eq!(dropped_rx.try_recv(), Err(mpsc::TryRecvError::Empty));
        signal.cancel();
        assert!(signal.is_cancelled());
        let worker_id = job.wait().await.unwrap();
        assert_eq!(dropped_rx.recv().unwrap(), worker_id);
        assert_eq!(owner.stop_and_join(), Ok(()));
    }

    #[test]
    fn shutdown_joins_running_jobs_and_executes_accepted_required_work() {
        let owner = OwnedBlockingLanes::spawn(1, 32).unwrap();
        let executor = owner.executor();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (provider_release_tx, provider_release_rx) = mpsc::channel();
        let provider_entered = entered_tx.clone();
        let running_provider = executor
            .submit(Lane::Provider, None, move |_| {
                provider_entered.send(()).unwrap();
                provider_release_rx.recv().unwrap();
            })
            .unwrap();
        let (recovery_release_tx, recovery_release_rx) = mpsc::channel();
        let running_recovery = executor
            .submit(Lane::Recovery, None, move |_| {
                entered_tx.send(()).unwrap();
                recovery_release_rx.recv().unwrap();
            })
            .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (classified_tx, classified_rx) = mpsc::channel();
        let provider_classified = classified_tx.clone();
        let required_provider = executor
            .submit_required(Lane::Provider, move |signal| {
                provider_classified
                    .send((Lane::Provider, signal.is_cancelled()))
                    .unwrap();
            })
            .unwrap();
        let required_recovery = executor
            .submit_required(Lane::Recovery, move |signal| {
                classified_tx
                    .send((Lane::Recovery, signal.is_cancelled()))
                    .unwrap();
            })
            .unwrap();
        drop(required_provider);
        let calls = Arc::new(AtomicUsize::new(0));
        let skipped_calls = Arc::clone(&calls);
        let skipped = executor
            .submit(Lane::Provider, None, move |_| {
                skipped_calls.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap();
        let (joined_tx, joined_rx) = mpsc::channel();
        let stopper = thread::spawn(move || {
            let mut owner = owner;
            joined_tx.send(owner.stop_and_join()).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if executor.lane(Lane::Recovery).state.lock().unwrap().closed {
                break;
            }
            assert!(Instant::now() < deadline, "intake did not close");
            thread::yield_now();
        }
        assert_eq!(joined_rx.try_recv(), Err(mpsc::TryRecvError::Empty));
        assert!(matches!(
            executor.submit_required(Lane::Provider, |_| ()),
            Err(SubmitError::Closed)
        ));
        provider_release_tx.send(()).unwrap();
        recovery_release_tx.send(()).unwrap();
        assert_eq!(
            joined_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Ok(())
        );
        stopper.join().unwrap();
        let first = classified_rx.recv().unwrap();
        let second = classified_rx.recv().unwrap();
        assert!(first.1 && second.1);
        assert_ne!(first.0, second.0);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        drop((
            running_provider,
            running_recovery,
            required_recovery,
            skipped,
        ));
    }
}
