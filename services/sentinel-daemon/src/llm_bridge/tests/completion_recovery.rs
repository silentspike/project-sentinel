// Included only under bridge::tests::completion_recovery_tests. No provider I/O.
use super::*;
use std::sync::atomic::AtomicUsize;

const WAIT: Duration = Duration::from_secs(10);
const PROMPT_RETURN: Duration = Duration::from_secs(1);
const REQUEST_ID: &str = "company-provider-reservation-m0";

fn context_and_response() -> (ModelWorkContext, GatewayResponse) {
    let context = crate::workflow_api::model_work::test_context().into();
    let response = serde_json::from_value(serde_json::json!({
        "content":"{\"schema_version\":1,\"tools\":[]}", "decision":"forward",
        "request_id":REQUEST_ID, "provider":"codex-cli", "tokens_used":30,
        "input_tokens":10, "output_tokens":20, "hierarchy_tier":2, "tier":"mid",
        "cost_source":"provider_reported", "effective_model":"test-model"
    }))
    .unwrap();
    (context, response)
}

fn resolver(context: &ModelWorkContext, fail_next: bool) -> ModelWorkResolver {
    ModelWorkResolver {
        context: context.clone(),
        admissions: Mutex::new(Vec::new()),
        fail_next: AtomicBool::new(fail_next),
    }
}

struct RecoveryFixture {
    directory: tempfile::TempDir,
    store: Arc<EventStore>,
    context: ModelWorkContext,
    pending: LlmCompletionEntry,
    usage: DomainEvent,
}

impl RecoveryFixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("events.sqlite");
        let store = Arc::new(EventStore::open(path.to_str().unwrap()).unwrap());
        let (context, response) = context_and_response();
        let binding = context.binding();
        let digest = "a".repeat(64);
        let usage = build_usage_event(
            binding.agent_id(), 1, "", Some(&binding), &response, true,
        )
        .unwrap();
        let completed = CompletedLlmResponse {
            version: 2,
            request_id: REQUEST_ID.to_owned(),
            request_digest: digest.clone(),
            usage_event: usage.clone(),
            actions: Vec::new(),
            tokens_used: 30,
            model_work: Some(ModelWorkCompletion {
                context: context.clone(),
                content: response.content,
                admissible: true,
            }),
            model_response_digest: None,
        };
        completed.model_work.as_ref().unwrap().validate_usage(&usage).unwrap();
        assert!(store.reserve_request(REQUEST_ID, &digest, &binding.agent_id().to_string()).unwrap());
        store.enqueue_completion(
            REQUEST_ID, &digest, &serde_json::to_string(&completed).unwrap(),
        ).unwrap();
        let pending = store.get_completion(REQUEST_ID).unwrap().unwrap();
        assert_eq!(pending.status, "pending_usage");
        assert_eq!(pending.attempt_count, 0);
        Self { directory, store, context, pending, usage }
    }
}

fn assert_one_usage(store: &EventStore, usage: &DomainEvent) {
    let events = store.get_all_events().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        serde_json::to_value(&events[0]).unwrap(),
        serde_json::to_value(usage).unwrap(),
    );
}

struct PausedAdmission {
    inner: ModelWorkResolver,
    calls: AtomicUsize,
    entered: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl ProviderUsageAuthorityResolver for PausedAdmission {
    fn resolve_provider_usage_authority(
        &self,
        agent: AgentId,
    ) -> Result<Option<ProviderExecutionAuthority>, &'static str> {
        self.inner.resolve_provider_usage_authority(agent)
    }

    fn admit_model_work(
        &self,
        completion: &ModelWorkCompletion,
        request_id: &str,
        request_digest: &str,
    ) -> Result<(), &'static str> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.send(()).map_err(|_| "admission observer closed")?;
        self.release.lock().unwrap().recv_timeout(WAIT)
            .map_err(|_| "paused admission timed out")?;
        self.inner.admit_model_work(completion, request_id, request_digest)
    }
}

#[test]
fn completion_recovery_coalesces_paused_admission_and_skips_late_stale_clone() {
    let fixture = RecoveryFixture::new();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let paused = PausedAdmission {
        inner: resolver(&fixture.context, false),
        calls: AtomicUsize::new(0),
        entered: entered_tx,
        release: Mutex::new(release_rx),
    };
    let (action_tx, action_rx) = mpsc::channel();
    std::thread::scope(|scope| {
        // Both invocations deliberately borrow the same EventStore address.
        let store = fixture.store.as_ref();
        let first_entry = fixture.pending.clone();
        let second_entry = fixture.pending.clone();
        let action_tx = &action_tx;
        let paused = &paused;
        let first = scope.spawn(move || {
            recover_completion(store, first_entry, action_tx, 3, Some(paused));
        });
        entered_rx.recv_timeout(WAIT).expect("first recovery never reached admission");
        let (finished_tx, finished_rx) = mpsc::channel();
        let second = scope.spawn(move || {
            recover_completion(store, second_entry, action_tx, 3, Some(paused));
            let _ = finished_tx.send(());
        });
        let prompt_return = finished_rx.recv_timeout(PROMPT_RETURN);
        let calls_while_paused = paused.calls.load(Ordering::SeqCst);
        let retained = store.get_completion(REQUEST_ID).unwrap().unwrap();
        // Release before assertions and joins, including on regression paths.
        let _ = release_tx.send(());
        first.join().unwrap();
        second.join().unwrap();
        assert!(prompt_return.is_ok(), "duplicate recovery waited for admission");
        assert_eq!(calls_while_paused, 1);
        assert_eq!(retained.status, "ready_for_action");
        assert_eq!(retained.attempt_count, 0);
        assert_eq!(retained.last_error, None);
    });
    assert_eq!(paused.calls.load(Ordering::SeqCst), 1);
    assert_eq!(*paused.inner.admissions.lock().unwrap(), vec![REQUEST_ID.to_owned()]);
    assert!(fixture.store.get_completion(REQUEST_ID).unwrap().is_none());
    let late = resolver(&fixture.context, false);
    recover_completion(fixture.store.as_ref(), fixture.pending.clone(), &action_tx, 3, Some(&late));
    assert!(late.admissions.lock().unwrap().is_empty());
    assert_one_usage(fixture.store.as_ref(), &fixture.usage);
    assert!(matches!(action_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
}

#[test]
fn completion_recovery_rejects_cached_binding_changes_without_canonical_substitution() {
    let fixture = RecoveryFixture::new();
    let resolver = resolver(&fixture.context, false);
    let (action_tx, action_rx) = mpsc::channel();
    for field in ["digest", "payload", "owner", "created_at"] {
        let mut cached = fixture.pending.clone();
        match field {
            "digest" => cached.request_digest = "b".repeat(64),
            // Different bytes but the same decodable completion are still stale.
            "payload" => cached.payload.push(' '),
            "owner" => cached.owner_scope = sentinel_common::StateTransferScope::for_agent("AGENT-07"),
            "created_at" => cached.created_at = cached.created_at.checked_add(1).unwrap(),
            _ => unreachable!(),
        }
        recover_completion(fixture.store.as_ref(), cached, &action_tx, 3, Some(&resolver));
        assert_eq!(fixture.store.get_completion(REQUEST_ID).unwrap(), Some(fixture.pending.clone()), "{field}");
        assert!(resolver.admissions.lock().unwrap().is_empty(), "{field}");
        assert!(fixture.store.get_all_events().unwrap().is_empty(), "{field}");
        assert!(matches!(action_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }
    // Every mismatch must release the guard, leaving the real row recoverable.
    recover_completion(fixture.store.as_ref(), fixture.pending.clone(), &action_tx, 3, Some(&resolver));
    assert_eq!(*resolver.admissions.lock().unwrap(), vec![REQUEST_ID.to_owned()]);
    assert!(fixture.store.get_completion(REQUEST_ID).unwrap().is_none());
    assert_one_usage(fixture.store.as_ref(), &fixture.usage);
}

#[test]
fn completion_recovery_refreshes_terminal_status_before_usage_or_admission() {
    for status in ["failed", "action_claimed"] {
        let fixture = RecoveryFixture::new();
        let store = fixture.store.as_ref();
        if status == "failed" {
            assert_eq!(store.record_failure(REQUEST_ID, &fixture.pending.request_digest, "terminal", 1).unwrap(), (1, true));
        } else {
            store.persist_usage(REQUEST_ID, &fixture.pending.request_digest, &fixture.usage).unwrap();
            assert!(store.claim_actions(REQUEST_ID, &fixture.pending.request_digest).unwrap());
        }
        let terminal = store.get_completion(REQUEST_ID).unwrap().unwrap();
        assert_eq!(terminal.status, status);
        let resolver = resolver(&fixture.context, false);
        let (action_tx, action_rx) = mpsc::channel();
        recover_completion(store, fixture.pending.clone(), &action_tx, 3, Some(&resolver));
        assert_eq!(store.get_completion(REQUEST_ID).unwrap(), Some(terminal));
        assert!(resolver.admissions.lock().unwrap().is_empty());
        if status == "failed" {
            assert!(store.get_all_events().unwrap().is_empty());
        } else {
            assert_one_usage(store, &fixture.usage);
        }
        assert!(matches!(action_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }
}

#[test]
fn completion_recovery_admission_error_releases_guard_and_preserves_one_durable_usage() {
    let fixture = RecoveryFixture::new();
    let resolver = resolver(&fixture.context, true);
    let (action_tx, action_rx) = mpsc::channel();
    let store = fixture.store.as_ref();
    recover_completion(store, fixture.pending.clone(), &action_tx, 3, Some(&resolver));
    let retry = store.get_completion(REQUEST_ID).unwrap().unwrap();
    assert_eq!(retry.status, "ready_for_action");
    assert_eq!(retry.attempt_count, 1);
    assert_eq!(retry.last_error.as_deref(), Some("injected admission failure"));
    assert!(resolver.admissions.lock().unwrap().is_empty());
    assert_one_usage(store, &fixture.usage);
    // The original cached status/attempt metadata is stale, but its sealed
    // identity is unchanged. Recovery must use the fresh ready-for-action row.
    recover_completion(store, fixture.pending.clone(), &action_tx, 3, Some(&resolver));
    assert_eq!(*resolver.admissions.lock().unwrap(), vec![REQUEST_ID.to_owned()]);
    assert!(store.get_completion(REQUEST_ID).unwrap().is_none());
    assert_one_usage(store, &fixture.usage);
    let reopened = EventStore::open(fixture.directory.path().join("events.sqlite").to_str().unwrap()).unwrap();
    assert!(reopened.get_completion(REQUEST_ID).unwrap().is_none());
    assert_one_usage(&reopened, &fixture.usage);
    assert!(matches!(action_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
}

#[test]
fn completion_recovery_held_guard_enqueue_retains_response_then_recovers_once() {
    let directory = tempfile::tempdir().unwrap();
    let store = EventStore::open(directory.path().join("events.sqlite").to_str().unwrap()).unwrap();
    let (context, response) = context_and_response();
    let binding = context.binding();
    let resolver = resolver(&context, false);
    let digest = "a".repeat(64);
    assert!(store.reserve_request(REQUEST_ID, &digest, &binding.agent_id().to_string()).unwrap());
    let (action_tx, action_rx) = mpsc::channel();
    let guard = CompletionRecovery::try_acquire(&store, REQUEST_ID).unwrap();
    store_gateway_completion(
        &store,
        &action_tx,
        GatewayCompletionContext {
            request_id: REQUEST_ID,
            request_digest: &digest,
            agent_id: binding.agent_id(),
            tick: 1,
            requested_model: "",
            authority: Some(&binding),
            authority_resolver: Some(&resolver),
            gateway_response: &response,
            usage_v2_enabled: true,
            model_work: Some(&context),
        },
        3,
    ).unwrap();
    let pending = store.get_completion(REQUEST_ID).unwrap().unwrap();
    assert_eq!(pending.status, "pending_usage");
    assert_eq!(pending.attempt_count, 0);
    assert!(resolver.admissions.lock().unwrap().is_empty());
    assert!(store.get_all_events().unwrap().is_empty());
    let completed: CompletedLlmResponse = serde_json::from_str(&pending.payload).unwrap();
    drop(guard);
    recover_completion(&store, pending.clone(), &action_tx, 3, Some(&resolver));
    recover_completion(&store, pending, &action_tx, 3, Some(&resolver));
    assert_eq!(*resolver.admissions.lock().unwrap(), vec![REQUEST_ID.to_owned()]);
    assert!(store.get_completion(REQUEST_ID).unwrap().is_none());
    assert_one_usage(&store, &completed.usage_event);
    assert!(matches!(action_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
}
