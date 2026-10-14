use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use sentinel_common::{AgentId, Perception};
use tokio::sync::Notify;
use tokio::task::JoinSet;

use super::provider_blocking_lanes::{
    BlockingExecutor, BlockingJob, JobCancellation, JobError, Lane, SubmitError,
};

type MergePerception = fn(&mut HashMap<AgentId, Perception>, Perception);

#[derive(Default)]
struct Retirement {
    exited: AtomicBool,
    changed: Notify,
}

impl Retirement {
    async fn wait(&self) {
        while !self.exited.load(Ordering::Acquire) {
            self.changed.notified().await;
        }
    }
}

struct ClosureOwnership(Arc<Retirement>);

impl Drop for ClosureOwnership {
    fn drop(&mut self) {
        self.0.exited.store(true, Ordering::Release);
        self.0.changed.notify_one();
    }
}

pub(super) struct ProviderPreparationController<T: Send + 'static> {
    pending: HashMap<AgentId, Perception>,
    order: VecDeque<AgentId>,
    inflight: HashMap<AgentId, JobCancellation>,
    adapters: JoinSet<(AgentId, Result<T, JobError>)>,
    pending_agent_limit: usize,
    prepare_bound: usize,
    merge: MergePerception,
    prefer_urgent: bool,
    intake_stopped: bool,
}

impl<T: Send + 'static> ProviderPreparationController<T> {
    pub(super) fn new(
        pending_agent_limit: usize,
        prepare_bound: usize,
        merge: MergePerception,
    ) -> Self {
        Self {
            pending: HashMap::new(),
            order: VecDeque::new(),
            inflight: HashMap::new(),
            adapters: JoinSet::new(),
            pending_agent_limit,
            prepare_bound,
            merge,
            prefer_urgent: true,
            intake_stopped: false,
        }
    }

    pub(super) fn offer(&mut self, perception: Perception) -> Result<(), Box<Perception>> {
        let agent = perception.agent_id;
        if self.intake_stopped
            || (!self.pending.contains_key(&agent)
                && self.pending.len() >= self.pending_agent_limit)
        {
            return Err(Box::new(perception));
        }
        let present = self.pending.contains_key(&agent);
        (self.merge)(&mut self.pending, perception);
        if !present {
            self.order.push_back(agent);
        }
        Ok(())
    }

    pub(super) fn pump<F>(
        &mut self,
        executor: &BlockingExecutor,
        deadline: Option<Instant>,
        prepare: F,
    ) -> usize
    where
        F: Fn(Perception, JobCancellation) -> T + Clone + Send + Sync + 'static,
    {
        let mut submitted_count = 0;
        while !self.intake_stopped && self.inflight.len() < self.prepare_bound {
            let Some(agent) = self.next_pending_agent() else {
                break;
            };
            let perception = self.pending[&agent].clone();
            let urgent = is_urgent(&perception);
            let prepare = prepare.clone();
            let retirement = Arc::new(Retirement::default());
            let ownership = ClosureOwnership(Arc::clone(&retirement));
            let submitted: Result<BlockingJob<T>, SubmitError> =
                executor.submit(Lane::Preparation, deadline, move |cancellation| {
                    let _ownership = ownership;
                    prepare(perception, cancellation)
                });
            let job = match submitted {
                Ok(job) => job,
                // Submission rejection must leave the original coalesced perception intact.
                Err(_) => break,
            };
            self.pending.remove(&agent);
            self.order.retain(|queued| *queued != agent);
            self.inflight.insert(agent, job.cancellation());
            self.prefer_urgent = !urgent;
            self.adapters.spawn(async move {
                let result = job.wait().await;
                // Observer completion is not proof that a blocking closure has exited.
                retirement.wait().await;
                (agent, result)
            });
            submitted_count += 1;
        }
        submitted_count
    }

    pub(super) async fn next_result(&mut self) -> Option<(AgentId, Result<T, JobError>)> {
        match self.adapters.join_next().await {
            Some(Ok((agent, result))) => {
                self.inflight.remove(&agent);
                Some((agent, result))
            }
            // Blocking-job failures belong in JobError. An adapter failure is a controller fault.
            Some(Err(error)) => panic!("provider preparation result adapter failed: {error}"),
            None => None,
        }
    }

    pub(super) fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub(super) fn inflight_count(&self) -> usize {
        self.inflight.len()
    }

    pub(super) fn has_work(&self) -> bool {
        !self.pending.is_empty() || !self.inflight.is_empty()
    }

    pub(super) fn stop_intake(&mut self) {
        self.intake_stopped = true;
    }

    pub(super) fn cancel_inflight(&self) {
        // Keep adapters and agent reservations until actual closure retirement.
        for cancellation in self.inflight.values() {
            cancellation.cancel();
        }
    }

    pub(super) fn cancel_pending(&mut self) -> Vec<Perception> {
        let mut cancelled = Vec::with_capacity(self.pending.len());
        while let Some(agent) = self.order.pop_front() {
            if let Some(perception) = self.pending.remove(&agent) {
                cancelled.push(perception);
            }
        }
        cancelled
    }

    fn next_pending_agent(&self) -> Option<AgentId> {
        let mut urgent = None;
        let mut normal = None;
        for agent in &self.order {
            if self.inflight.contains_key(agent) {
                continue;
            }
            let Some(perception) = self.pending.get(agent) else {
                continue;
            };
            if is_urgent(perception) {
                urgent.get_or_insert(*agent);
            } else {
                normal.get_or_insert(*agent);
            }
            if urgent.is_some() && normal.is_some() {
                break;
            }
        }
        // FIFO within each class, alternating classes when both have eligible work.
        if self.prefer_urgent {
            urgent.or(normal)
        } else {
            normal.or(urgent)
        }
    }
}

impl<T: Send + 'static> Drop for ProviderPreparationController<T> {
    fn drop(&mut self) {
        self.intake_stopped = true;
        self.cancel_inflight();
        // Dropped adapters cancel their jobs; executor supervisors still own actual closures.
        self.adapters.abort_all();
    }
}

fn is_urgent(perception: &Perception) -> bool {
    !perception.heard_text.is_empty()
        || perception.is_directly_addressed
        || perception.has_operator_impulse
}

#[cfg(test)]
mod tests {
    use std::sync::{mpsc, Mutex};
    use std::time::Duration;

    use sentinel_common::{Tick, Timestamp};

    use super::super::bridge::insert_prefer_heard;
    use super::super::provider_blocking_lanes::OwnedBlockingLanes;
    use super::*;

    fn perception(agent: u16, tick: u64, heard: &str) -> Perception {
        Perception {
            agent_id: AgentId(agent),
            circadian_text: String::new(),
            body_text: format!("body-{tick}"),
            environment_text: String::new(),
            acoustic_text: String::new(),
            heard_text: heard.to_owned(),
            presence_text: String::new(),
            impulse_text: String::new(),
            is_directly_addressed: false,
            timestamp: Timestamp(tick),
            tick: Tick(tick),
            room_id: "test-room".to_owned(),
            max_priority: "P2".to_owned(),
            synth_fingerprint: "|IM:0".to_owned(),
            personality_type: "E".to_owned(),
            has_operator_impulse: false,
        }
    }

    struct ReleaseOnDrop(Option<mpsc::Sender<()>>);

    impl ReleaseOnDrop {
        fn release(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            self.release();
        }
    }

    async fn until(mut ready: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !ready() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("controlled operation did not complete");
    }

    #[test]
    fn supplied_merge_preserves_heard_impulse_latest_and_queue_age() {
        let mut controller = ProviderPreparationController::<()>::new(2, 2, insert_prefer_heard);
        let mut impulse = perception(1, 1, "");
        impulse.has_operator_impulse = true;
        impulse.synth_fingerprint = "|IM:1".to_owned();
        controller.offer(impulse).unwrap();
        controller.offer(perception(2, 1, "")).unwrap();
        controller.offer(perception(1, 2, "directed text")).unwrap();
        controller.offer(perception(1, 3, "")).unwrap();
        controller.offer(perception(2, 9, "")).unwrap();
        assert_eq!(controller.pending_count(), 2);
        assert_eq!(controller.order, VecDeque::from([AgentId(1), AgentId(2)]));
        let heard = &controller.pending[&AgentId(1)];
        assert_eq!(heard.heard_text, "directed text");
        assert_eq!(heard.tick, Tick(2));
        assert!(heard.has_operator_impulse);
        assert_eq!(heard.synth_fingerprint, "|IM:1");
        assert_eq!(controller.pending[&AgentId(2)].tick, Tick(9));
        assert_eq!(controller.next_pending_agent(), Some(AgentId(1)));
        controller.prefer_urgent = false;
        assert_eq!(controller.next_pending_agent(), Some(AgentId(2)));
    }

    #[test]
    fn bounded_pending_queue_returns_complete_overflow_and_stop_drain() {
        let mut controller = ProviderPreparationController::<()>::new(1, 1, insert_prefer_heard);
        controller.offer(perception(1, 1, "")).unwrap();
        let overflow = perception(2, 4, "urgent retained text");
        let encoded = serde_json::to_vec(&overflow).unwrap();
        assert_eq!(
            serde_json::to_vec(&controller.offer(overflow).unwrap_err()).unwrap(),
            encoded
        );
        controller.offer(perception(1, 5, "latest heard")).unwrap();
        controller.stop_intake();
        assert!(controller.offer(perception(1, 6, "new text")).is_err());
        let cancelled = controller.cancel_pending();
        assert_eq!(cancelled.len(), 1);
        assert_eq!(cancelled[0].heard_text, "latest heard");
        assert_eq!(cancelled[0].tick, Tick(5));
        assert!(!controller.has_work());
    }

    #[tokio::test]
    async fn blocked_preparation_preserves_reserved_lane_progress_and_pending_order() {
        let mut lanes = OwnedBlockingLanes::spawn(1, 4).unwrap();
        let executor = lanes.executor();
        let (release_tx, release_rx) = mpsc::channel();
        let mut release = ReleaseOnDrop(Some(release_tx));
        let receiver = Arc::new(Mutex::new(release_rx));
        let entered = Arc::new(Notify::new());
        let notify = Arc::clone(&entered);
        let prepare = move |perception: Perception, _: JobCancellation| {
            if perception.agent_id == AgentId(1) {
                notify.notify_one();
                receiver.lock().unwrap().recv().unwrap();
            }
            perception.tick
        };
        let mut controller = ProviderPreparationController::new(4, 1, insert_prefer_heard);
        controller.offer(perception(1, 1, "")).unwrap();
        controller.offer(perception(2, 2, "")).unwrap();
        controller.offer(perception(3, 3, "")).unwrap();
        assert_eq!(controller.pump(&executor, None, prepare.clone()), 1);
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        assert_eq!(controller.pending_count(), 2);
        assert_eq!(controller.pump(&executor, None, prepare.clone()), 0);

        let provider = executor.submit(Lane::Provider, None, |_| 17).unwrap();
        let recovery = executor.submit(Lane::Recovery, None, |_| 19).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::time::sleep(Duration::from_millis(1)).await;
            assert_eq!(provider.wait().await.unwrap(), 17);
            assert_eq!(recovery.wait().await.unwrap(), 19);
        })
        .await
        .unwrap();
        assert_eq!(controller.inflight_count(), 1);
        assert_eq!(controller.order, VecDeque::from([AgentId(2), AgentId(3)]));

        release.release();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), controller.next_result())
                .await
                .unwrap()
                .unwrap(),
            (AgentId(1), Ok(Tick(1)))
        );
        assert_eq!(controller.pump(&executor, None, prepare.clone()), 1);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), controller.next_result())
                .await
                .unwrap()
                .unwrap(),
            (AgentId(2), Ok(Tick(2)))
        );
        // Holding an earlier prepared result does not reserve an agent through provider HTTP.
        controller.offer(perception(2, 4, "")).unwrap();
        assert_eq!(controller.order, VecDeque::from([AgentId(3), AgentId(2)]));
        assert_eq!(controller.pump(&executor, None, prepare.clone()), 1);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), controller.next_result())
                .await
                .unwrap()
                .unwrap(),
            (AgentId(3), Ok(Tick(3)))
        );
        assert_eq!(controller.pump(&executor, None, prepare), 1);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), controller.next_result())
                .await
                .unwrap()
                .unwrap(),
            (AgentId(2), Ok(Tick(4)))
        );
        assert!(!controller.has_work());
        lanes.stop_and_join().unwrap();
    }

    #[tokio::test]
    async fn executor_queue_saturation_retains_original_pending_perception() {
        let mut lanes = OwnedBlockingLanes::spawn(1, 1).unwrap();
        let executor = lanes.executor();
        let (release_tx, release_rx) = mpsc::channel();
        let mut release = ReleaseOnDrop(Some(release_tx));
        let entered = Arc::new(Notify::new());
        let notify = Arc::clone(&entered);
        let blocker = executor
            .submit(Lane::Preparation, None, move |_| {
                notify.notify_one();
                release_rx.recv().unwrap();
            })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        let queued = executor.submit(Lane::Preparation, None, |_| ()).unwrap();
        let mut controller = ProviderPreparationController::new(2, 1, insert_prefer_heard);
        let pending = perception(3, 4, "heard survives executor saturation");
        let encoded = serde_json::to_vec(&pending).unwrap();
        controller.offer(pending).unwrap();
        assert_eq!(controller.pump(&executor, None, |p, _| p.tick), 0);
        assert_eq!(controller.inflight_count(), 0);
        assert_eq!(controller.pending_count(), 1);
        assert_eq!(
            serde_json::to_vec(&controller.pending[&AgentId(3)]).unwrap(),
            encoded
        );
        release.release();
        blocker.wait().await.unwrap();
        queued.wait().await.unwrap();
        until(|| controller.pump(&executor, None, |p, _| p.tick) == 1).await;
        assert_eq!(
            controller.next_result().await.unwrap(),
            (AgentId(3), Ok(Tick(4)))
        );
        lanes.stop_and_join().unwrap();
    }

    #[tokio::test]
    async fn expired_queued_selection_keeps_agent_reserved_until_actual_retirement() {
        let mut lanes = OwnedBlockingLanes::spawn(1, 2).unwrap();
        let executor = lanes.executor();
        let (release_tx, release_rx) = mpsc::channel();
        let mut release = ReleaseOnDrop(Some(release_tx));
        let entered = Arc::new(Notify::new());
        let notify = Arc::clone(&entered);
        let blocker = executor
            .submit(Lane::Preparation, None, move |_| {
                notify.notify_one();
                release_rx.recv().unwrap();
            })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        let mut controller = ProviderPreparationController::new(2, 2, insert_prefer_heard);
        controller.offer(perception(1, 1, "")).unwrap();
        assert_eq!(
            controller.pump(&executor, Some(Instant::now()), |p, _| p.tick),
            1
        );
        controller
            .offer(perception(1, 2, "new pending text"))
            .unwrap();
        assert_eq!(controller.pump(&executor, None, |p, _| p.tick), 0);
        assert_eq!(controller.inflight_count(), 1);
        release.release();
        blocker.wait().await.unwrap();
        assert_eq!(
            controller.next_result().await.unwrap(),
            (AgentId(1), Err(JobError::DeadlineExceeded))
        );
        assert_eq!(controller.inflight_count(), 0);
        assert_eq!(controller.pending_count(), 1);
        assert_eq!(
            controller.cancel_pending()[0].heard_text,
            "new pending text"
        );
        lanes.stop_and_join().unwrap();
    }

    #[tokio::test]
    async fn explicit_cancellation_retains_running_job_and_skips_queued_closure() {
        let mut lanes = OwnedBlockingLanes::spawn(1, 4).unwrap();
        let executor = lanes.executor();
        let (release_tx, release_rx) = mpsc::channel();
        let mut release = ReleaseOnDrop(Some(release_tx));
        let receiver = Arc::new(Mutex::new(release_rx));
        let entered = Arc::new(Notify::new());
        let notify = Arc::clone(&entered);
        let exited = Arc::new(AtomicBool::new(false));
        let first_exited = Arc::clone(&exited);
        let second_called = Arc::new(AtomicBool::new(false));
        let called = Arc::clone(&second_called);
        let cancellation = Arc::new(Mutex::new(None));
        let captured = Arc::clone(&cancellation);
        let prepare = move |p: Perception, token: JobCancellation| {
            if p.agent_id == AgentId(1) {
                *captured.lock().unwrap() = Some(token);
                notify.notify_one();
                receiver.lock().unwrap().recv().unwrap();
                first_exited.store(true, Ordering::Release);
            } else {
                called.store(true, Ordering::Release);
            }
            p.tick
        };
        let mut controller = ProviderPreparationController::new(4, 2, insert_prefer_heard);
        controller.offer(perception(1, 1, "")).unwrap();
        assert_eq!(controller.pump(&executor, None, prepare.clone()), 1);
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        controller.offer(perception(2, 2, "")).unwrap();
        assert_eq!(controller.pump(&executor, None, prepare), 1);
        controller.offer(perception(3, 3, "")).unwrap();
        controller.stop_intake();
        controller.cancel_inflight();
        controller.cancel_inflight();
        assert_eq!(controller.cancel_pending().len(), 1);
        assert!(cancellation
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .is_cancelled());
        assert_eq!(controller.inflight_count(), 2);
        assert!(controller.has_work());
        assert!(!exited.load(Ordering::Acquire));
        assert!(!second_called.load(Ordering::Acquire));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), controller.next_result())
                .await
                .is_err()
        );
        assert_eq!(controller.inflight_count(), 2);
        let recovery = executor.submit(Lane::Recovery, None, |_| 23).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), recovery.wait())
                .await
                .unwrap()
                .unwrap(),
            23
        );
        assert!(!exited.load(Ordering::Acquire));

        release.release();
        let mut results = HashMap::new();
        for _ in 0..2 {
            let (agent, result) =
                tokio::time::timeout(Duration::from_secs(2), controller.next_result())
                    .await
                    .unwrap()
                    .unwrap();
            results.insert(agent, result);
        }
        assert_eq!(results[&AgentId(1)], Ok(Tick(1)));
        assert_eq!(results[&AgentId(2)], Err(JobError::Cancelled));
        assert!(exited.load(Ordering::Acquire));
        assert!(!second_called.load(Ordering::Acquire));
        assert_eq!(controller.inflight_count(), 0);
        assert!(!controller.has_work());
        lanes.stop_and_join().unwrap();
    }

    struct ExitGuard(Arc<AtomicBool>);

    impl Drop for ExitGuard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[tokio::test]
    async fn controller_receiver_loss_does_not_release_running_closure_or_guard() {
        let mut lanes = OwnedBlockingLanes::spawn(1, 1).unwrap();
        let executor = lanes.executor();
        let (release_tx, release_rx) = mpsc::channel();
        let mut release = ReleaseOnDrop(Some(release_tx));
        let receiver = Arc::new(Mutex::new(release_rx));
        let entered = Arc::new(Notify::new());
        let notify = Arc::clone(&entered);
        let exited = Arc::new(AtomicBool::new(false));
        let guard_flag = Arc::clone(&exited);
        let cancellation = Arc::new(Mutex::new(None));
        let captured = Arc::clone(&cancellation);
        let mut controller = ProviderPreparationController::new(2, 1, insert_prefer_heard);
        controller.offer(perception(1, 1, "")).unwrap();
        assert_eq!(
            controller.pump(&executor, None, move |_, token| {
                let guard = ExitGuard(Arc::clone(&guard_flag));
                *captured.lock().unwrap() = Some(token);
                notify.notify_one();
                receiver.lock().unwrap().recv().unwrap();
                guard
            }),
            1
        );
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        drop(controller);
        until(|| {
            cancellation
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(JobCancellation::is_cancelled)
        })
        .await;
        assert!(!exited.load(Ordering::Acquire));
        let queued = executor.submit(Lane::Preparation, None, |_| ()).unwrap();
        assert!(matches!(
            executor.submit(Lane::Preparation, None, |_| ()),
            Err(SubmitError::Full)
        ));
        assert!(!exited.load(Ordering::Acquire));
        release.release();
        until(|| exited.load(Ordering::Acquire)).await;
        queued.wait().await.unwrap();
        lanes.stop_and_join().unwrap();
    }
}
