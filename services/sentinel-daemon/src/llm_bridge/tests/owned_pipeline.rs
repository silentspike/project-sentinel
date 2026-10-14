// Included only inside bridge::tests; reuse its entry point and fixtures.
struct OwnedPipelineBarrier {
    entered: tokio::sync::Notify,
    release: Mutex<mpsc::Receiver<()>>,
    exited: AtomicBool,
}

impl OwnedPipelineBarrier {
    fn wait(&self) {
        self.entered.notify_one();
        self.release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(8))
            .expect("owned pipeline barrier was not released within eight seconds");
        self.exited.store(true, Ordering::SeqCst);
    }
}

struct OwnedPipelineResolver {
    legacy: ExactValidationResolver,
    barrier: Arc<OwnedPipelineBarrier>,
    block_selection: bool,
}

impl ProviderUsageAuthorityResolver for OwnedPipelineResolver {
    fn resolve_provider_usage_authority(
        &self,
        agent: AgentId,
    ) -> Result<Option<ProviderExecutionAuthority>, &'static str> {
        if self.block_selection {
            self.barrier.wait();
            return Ok(None);
        }
        self.legacy.resolve_provider_usage_authority(agent)
    }

    fn allows_unbound_provider_usage(&self) -> bool {
        true
    }

    fn validate_provider_usage_authority(
        &self,
        expected: &ProviderExecutionAuthority,
    ) -> Result<bool, &'static str> {
        self.barrier.wait();
        self.legacy.validate_provider_usage_authority(expected)
    }

    fn model_work_context(
        &self,
        expected: &ProviderExecutionAuthority,
    ) -> Result<Option<ModelWorkContext>, &'static str> {
        self.legacy.model_work_context(expected)
    }
}

struct OwnedPipelineHarness {
    perception_tx: mpsc::Sender<Perception>,
    shutdown_tx: watch::Sender<bool>,
    bridge: tokio::task::JoinHandle<Result<(), &'static str>>,
    provider: tokio::task::JoinHandle<()>,
    provider_calls: Arc<std::sync::atomic::AtomicUsize>,
    action_rx: mpsc::Receiver<AgentAction>,
    state: Arc<StateStore>,
    release: Option<mpsc::Sender<()>>,
}

impl OwnedPipelineHarness {
    async fn start<S: CompletionStore + 'static>(
        directory: &std::path::Path,
        store: Arc<S>,
        resolver: Arc<dyn ProviderUsageAuthorityResolver>,
        release: Option<mpsc::Sender<()>>,
    ) -> Self {
        let provider_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (gateway_url, provider) =
            start_mock_agent_provider(Arc::clone(&provider_calls), None, None).await;
        let config = LlmBridgeConfig {
            gateway_url,
            credential: "test-credential".into(),
            max_concurrent: 1,
            min_ticks_between_calls: 0,
            usage_v2_enabled: true,
            request_timeout: Duration::from_secs(5),
            shutdown_drain_timeout: Duration::from_secs(2),
            completion_retry_interval: Duration::from_secs(3600),
            provider_usage_authority: Some(resolver),
            ..Default::default()
        };
        let state =
            Arc::new(StateStore::open(directory.join("state.redb").to_str().unwrap()).unwrap());
        let (perception_tx, perception_rx) = mpsc::channel();
        let (action_tx, action_rx) = mpsc::channel();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let bridge = tokio::spawn(run_llm_bridge_with_store(
            config,
            perception_rx,
            action_tx,
            Arc::new(BridgeTelemetry::default()),
            Arc::clone(&state),
            store,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(HashMap::new())),
            shutdown_rx,
            Arc::new(RwLock::new(true)),
        ));
        Self {
            perception_tx,
            shutdown_tx,
            bridge,
            provider,
            provider_calls,
            action_rx,
            state,
            release,
        }
    }

    fn release(&mut self) {
        if let Some(sender) = self.release.take() {
            let _ = sender.send(());
        }
    }

    async fn stop(&mut self) {
        let _ = self.shutdown_tx.send(true);
        self.release();
        tokio::time::timeout(Duration::from_secs(3), &mut self.bridge)
            .await
            .expect("owned pipeline did not retire")
            .unwrap()
            .unwrap();
    }
}

impl Drop for OwnedPipelineHarness {
    fn drop(&mut self) {
        // Release synchronous callbacks before any supervisor can join its worker.
        self.release();
        let _ = self.shutdown_tx.send(true);
        self.provider.abort();
    }
}

async fn owned_pipeline_wait(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("owned pipeline milestone was not reached");
}

fn owned_pipeline_review(directory: &std::path::Path) -> ModelWorkContext {
    let (_, mut review) = crate::workflow_api::adaptive_leadership_review::tests::fixture(
        &directory.join("company.sqlite"),
        &directory.join("events.sqlite"),
    );
    review.binding.grant.max_duration_ms = 2_000;
    review.binding.grant.expires_at_unix_ms = review.binding.issued_at_ms + 2_000;
    ModelWorkContext::AdaptiveLeadershipReview(Box::new(review))
}

#[tokio::test]
async fn owned_pipeline_slow_selection_keeps_heartbeat_and_shutdown_ownership() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(EventStore::open(":memory:").unwrap());
    let (release_tx, release_rx) = mpsc::channel();
    let barrier = Arc::new(OwnedPipelineBarrier {
        entered: tokio::sync::Notify::new(),
        release: Mutex::new(release_rx),
        exited: AtomicBool::new(false),
    });
    let resolver = Arc::new(OwnedPipelineResolver {
        legacy: ExactValidationResolver::new(
            crate::workflow_api::model_work::test_context().into(),
        ),
        barrier: Arc::clone(&barrier),
        block_selection: true,
    });
    let mut harness = OwnedPipelineHarness::start(
        directory.path(),
        Arc::clone(&store),
        resolver,
        Some(release_tx),
    )
    .await;
    let perception = recovery_test_perception();
    let request_id = agent_runtime_request_id(&perception, None);
    harness.perception_tx.send(perception.clone()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), barrier.entered.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_millis(250), async {
        for _ in 0..3 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("synchronous selection blocked Tokio heartbeat");
    harness.shutdown_tx.send(true).unwrap();
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(!barrier.exited.load(Ordering::SeqCst));
    assert!(
        !harness.bridge.is_finished(),
        "shutdown released actual closure ownership"
    );
    assert_eq!(harness.provider_calls.load(Ordering::SeqCst), 0);
    assert!(store.get_completion(&request_id).unwrap().is_none());
    harness.stop().await;
    assert!(barrier.exited.load(Ordering::SeqCst));
    assert_eq!(harness.provider_calls.load(Ordering::SeqCst), 0);
    assert!(store.get_completion(&request_id).unwrap().is_none());
}

#[tokio::test]
async fn owned_pipeline_fresh_authority_callback_crossing_expiry_never_reserves() {
    let directory = tempfile::tempdir().unwrap();
    let context = owned_pipeline_review(directory.path());
    context.validate_dispatch(unix_now_ms()).unwrap();
    let authority = context.binding();
    let store = Arc::new(FailFirstCompletionStore {
        inner: EventStore::open(":memory:").unwrap(),
        reservation_calls: std::sync::atomic::AtomicUsize::new(0),
        fail_next_usage: AtomicBool::new(false),
        append_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        failure_observed: Arc::new(tokio::sync::Notify::new()),
    });
    let (release_tx, release_rx) = mpsc::channel();
    let barrier = Arc::new(OwnedPipelineBarrier {
        entered: tokio::sync::Notify::new(),
        release: Mutex::new(release_rx),
        exited: AtomicBool::new(false),
    });
    let resolver = Arc::new(OwnedPipelineResolver {
        legacy: ExactValidationResolver::new(context.clone()),
        barrier: Arc::clone(&barrier),
        block_selection: false,
    });
    let mut harness = OwnedPipelineHarness::start(
        directory.path(),
        Arc::clone(&store),
        resolver.clone(),
        Some(release_tx),
    )
    .await;
    let mut perception = recovery_test_perception();
    perception.agent_id = authority.agent_id();
    harness.perception_tx.send(perception).unwrap();
    tokio::time::timeout(Duration::from_secs(2), barrier.entered.notified())
        .await
        .unwrap();
    assert!(
        context.validate_dispatch(unix_now_ms()).is_ok(),
        "fixture expired before callback"
    );
    owned_pipeline_wait(|| context.validate_dispatch(unix_now_ms()).is_err()).await;
    harness.release();
    owned_pipeline_wait(|| resolver.legacy.validation_calls.load(Ordering::SeqCst) == 1).await;
    // A new context read proves the original exact preparation guard retired;
    // do not let shutdown itself explain the first attempt's rejection.
    owned_pipeline_wait(|| {
        if resolver.legacy.context_calls.load(Ordering::SeqCst) >= 2 {
            return true;
        }
        let mut retry = recovery_test_perception();
        retry.agent_id = authority.agent_id();
        retry.tick = Tick(60);
        harness.perception_tx.send(retry).unwrap();
        false
    })
    .await;
    assert_eq!(harness.provider_calls.load(Ordering::SeqCst), 0);
    assert!(store
        .get_completion(&authority.request_id())
        .unwrap()
        .is_none());
    harness.stop().await;
    assert_eq!(resolver.legacy.validation_calls.load(Ordering::SeqCst), 1);
    assert_eq!(store.reservation_calls.load(Ordering::SeqCst), 0);
    assert_eq!(harness.provider_calls.load(Ordering::SeqCst), 0);
    assert!(store
        .get_completion(&authority.request_id())
        .unwrap()
        .is_none());
    assert!(store.inner.get_all_events().unwrap().is_empty());
}

#[tokio::test]
async fn owned_pipeline_historical_completion_recovers_after_dispatch_expiry() {
    let directory = tempfile::tempdir().unwrap();
    let context = owned_pipeline_review(directory.path());
    context.validate_dispatch(unix_now_ms()).unwrap();
    let authority = context.binding();
    let ModelWorkContext::AdaptiveLeadershipReview(review) = &context else {
        unreachable!()
    };
    let model = review.binding.grant.model.clone();
    let id = authority.request_id();
    let digest = "a".repeat(64);
    let resolver = Arc::new(ModelWorkResolver {
        context: context.clone(),
        admissions: Mutex::new(Vec::new()),
        fail_next: AtomicBool::new(true),
    });
    let store = Arc::new(EventStore::open(":memory:").unwrap());
    assert!(store
        .reserve_request(&id, &digest, &authority.agent_id().to_string())
        .unwrap());
    let response: GatewayResponse = serde_json::from_value(serde_json::json!({
        "content": "historical fixture response", "decision": "forward", "actions": [],
        "request_id": id, "provider": authority.provider(), "tokens_used": 7,
        "input_tokens": 5, "output_tokens": 2, "cache_read": 0, "cache_creation": 0,
        "tier": "mid", "hierarchy_tier": 2, "cost_usd": 0.0,
        "cost_source": "provider_reported", "effective_model": model
    }))
    .unwrap();
    let (action_tx, action_rx) = mpsc::channel();
    store_gateway_completion(
        store.as_ref(),
        &action_tx,
        GatewayCompletionContext {
            request_id: &id,
            request_digest: &digest,
            agent_id: authority.agent_id(),
            tick: 1,
            requested_model: &model,
            authority: Some(&authority),
            authority_resolver: Some(resolver.as_ref()),
            gateway_response: &response,
            usage_v2_enabled: true,
            model_work: Some(&context),
        },
        3,
    )
    .unwrap();
    assert_eq!(
        store.get_completion(&id).unwrap().unwrap().status,
        "ready_for_action"
    );
    assert!(resolver.admissions.lock().unwrap().is_empty());
    assert!(action_rx.try_recv().is_err());
    owned_pipeline_wait(|| context.validate_dispatch(unix_now_ms()).is_err()).await;
    let mut harness =
        OwnedPipelineHarness::start(directory.path(), Arc::clone(&store), resolver.clone(), None)
            .await;
    owned_pipeline_wait(|| store.get_completion(&id).unwrap().is_none()).await;
    harness.stop().await;
    assert_eq!(*resolver.admissions.lock().unwrap(), vec![id.clone()]);
    assert_eq!(harness.provider_calls.load(Ordering::SeqCst), 0);
    assert!(harness.action_rx.try_recv().is_err());
    assert_eq!(store.get_all_events().unwrap().len(), 1);
    assert!(!store
        .reserve_request(&id, &digest, &authority.agent_id().to_string())
        .unwrap());
}

struct OwnedPipelineLateContextResolver {
    legacy: ModelWorkResolver,
    barrier: Arc<OwnedPipelineBarrier>,
}

impl ProviderUsageAuthorityResolver for OwnedPipelineLateContextResolver {
    fn resolve_provider_usage_authority(
        &self,
        agent: AgentId,
    ) -> Result<Option<ProviderExecutionAuthority>, &'static str> {
        self.legacy.resolve_provider_usage_authority(agent)
    }

    fn model_work_context(
        &self,
        expected: &ProviderExecutionAuthority,
    ) -> Result<Option<ModelWorkContext>, &'static str> {
        self.barrier.wait();
        self.legacy.model_work_context(expected)
    }

    fn admit_model_work(
        &self,
        completion: &ModelWorkCompletion,
        id: &str,
        digest: &str,
    ) -> Result<(), &'static str> {
        self.legacy.admit_model_work(completion, id, digest)
    }
}

#[tokio::test]
async fn owned_pipeline_completion_arriving_during_context_recovers_after_expiry() {
    let directory = tempfile::tempdir().unwrap();
    let context = owned_pipeline_review(directory.path());
    context.validate_dispatch(unix_now_ms()).unwrap();
    let authority = context.binding();
    let ModelWorkContext::AdaptiveLeadershipReview(review) = &context else {
        unreachable!()
    };
    let model = review.binding.grant.model.clone();
    let id = authority.request_id();
    let store = Arc::new(EventStore::open(":memory:").unwrap());
    let (release_tx, release_rx) = mpsc::channel();
    let barrier = Arc::new(OwnedPipelineBarrier {
        entered: tokio::sync::Notify::new(),
        release: Mutex::new(release_rx),
        exited: AtomicBool::new(false),
    });
    let resolver = Arc::new(OwnedPipelineLateContextResolver {
        legacy: ModelWorkResolver {
            context: context.clone(),
            admissions: Mutex::new(Vec::new()),
            fail_next: AtomicBool::new(true),
        },
        barrier: Arc::clone(&barrier),
    });
    let mut harness = OwnedPipelineHarness::start(
        directory.path(),
        Arc::clone(&store),
        resolver.clone(),
        Some(release_tx),
    )
    .await;
    let mut perception = recovery_test_perception();
    perception.agent_id = authority.agent_id();
    let mut request = build_gateway_request(&perception, &harness.state, &id, Some(&authority));
    bind_model_work_request(&mut request, &context).unwrap();
    let digest = gateway_request_digest(&request).unwrap();
    harness.perception_tx.send(perception).unwrap();
    tokio::time::timeout(Duration::from_secs(2), barrier.entered.notified())
        .await
        .unwrap();
    // Reaching context proves the earlier exact recovery lookup found no row.
    assert!(store.get_completion(&id).unwrap().is_none());
    assert!(context.validate_dispatch(unix_now_ms()).is_ok());
    assert!(store
        .reserve_request(&id, &digest, &authority.agent_id().to_string())
        .unwrap());
    let response: GatewayResponse = serde_json::from_value(serde_json::json!({
        "content": "late historical fixture response", "decision": "forward", "actions": [],
        "request_id": id, "provider": authority.provider(), "tokens_used": 7,
        "input_tokens": 5, "output_tokens": 2, "cache_read": 0, "cache_creation": 0,
        "tier": "mid", "hierarchy_tier": 2, "cost_usd": 0.0,
        "cost_source": "provider_reported", "effective_model": model
    }))
    .unwrap();
    let (action_tx, action_rx) = mpsc::channel();
    store_gateway_completion(
        store.as_ref(),
        &action_tx,
        GatewayCompletionContext {
            request_id: &id,
            request_digest: &digest,
            agent_id: authority.agent_id(),
            tick: 1,
            requested_model: &model,
            authority: Some(&authority),
            authority_resolver: Some(resolver.as_ref()),
            gateway_response: &response,
            usage_v2_enabled: true,
            model_work: Some(&context),
        },
        3,
    )
    .unwrap();
    assert_eq!(
        store.get_completion(&id).unwrap().unwrap().status,
        "ready_for_action"
    );
    assert!(resolver.legacy.admissions.lock().unwrap().is_empty());
    owned_pipeline_wait(|| context.validate_dispatch(unix_now_ms()).is_err()).await;
    assert_eq!(
        store.get_completion(&id).unwrap().unwrap().status,
        "ready_for_action"
    );
    assert!(resolver.legacy.admissions.lock().unwrap().is_empty());
    harness.release();
    // No new perception or periodic retry: the preparation's late lookup must recover it.
    owned_pipeline_wait(|| store.get_completion(&id).unwrap().is_none()).await;
    harness.stop().await;
    assert_eq!(
        *resolver.legacy.admissions.lock().unwrap(),
        vec![id.clone()]
    );
    assert!(barrier.exited.load(Ordering::SeqCst));
    assert_eq!(harness.provider_calls.load(Ordering::SeqCst), 0);
    assert!(action_rx.try_recv().is_err());
    assert!(harness.action_rx.try_recv().is_err());
    assert_eq!(store.get_all_events().unwrap().len(), 1);
    assert!(!store
        .reserve_request(&id, &digest, &authority.agent_id().to_string())
        .unwrap());
}
