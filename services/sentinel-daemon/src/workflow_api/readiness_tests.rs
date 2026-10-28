#![cfg(test)]

use super::*;

#[test]
fn readiness_facts_disabled_wire_and_auth_remain_fail_closed() {
    let mut api = WorkflowApi::disabled().unwrap();
    let health = api.health();
    assert!(!health.ready && !health.dependencies_ready && !health.delivery_ready);
    assert_eq!(health.status, "disabled");
    assert_eq!(
        (
            health.pending_execution,
            health.pending_completion,
            health.pending_gate
        ),
        (0, 0, 0)
    );
    assert_eq!(health.canonical_event_cursor, Some(0));
    api.enabled = true;
    let response = api
        .handle("GET", WORKFLOW_READINESS_PATH, &HashMap::new(), b"")
        .unwrap();
    assert_eq!(response.status, 401);
    assert_eq!(api.store.company_event_cursor().unwrap(), 0);
    assert!(!api.reconciliation_status_snapshot().scan_succeeded);
}

#[test]
fn readiness_facts_current_worker_and_publication_failures_keep_precedence() {
    let api = WorkflowApi::disabled().unwrap();
    api.publish_reconciliation_status(Some("existing-scan-failure".into()));
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _publication = api.collaboration_publication_fence.lock().unwrap();
        panic!("poison publication fixture");
    }))
    .is_err());
    let first = api.health();
    assert!(!first.ready);
    assert_eq!(first.last_error.as_deref(), Some("existing-scan-failure"));
    assert_eq!(first.collaboration_publication_pending, usize::MAX);
    api.publish_reconciliation_status(None);
    assert_eq!(
        api.health().last_error.as_deref(),
        Some("collaboration_publication_fence_poisoned")
    );
    api.mark_company_reconciliation_worker_failed();
    assert_eq!(
        api.health().last_error.as_deref(),
        Some("company_reconciliation_worker_failed")
    );
}

struct ReadStoreDuringDependencyCheck {
    store: Arc<WorkflowStore>,
    completed: AtomicBool,
    workers: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl sentinel_workflow::OrganizationRuntimePort for ReadStoreDuringDependencyCheck {
    fn readiness(&self) -> DependencyReadiness {
        let store = Arc::clone(&self.store);
        let (sender, receiver) = std::sync::mpsc::channel();
        self.workers
            .lock()
            .unwrap()
            .push(std::thread::spawn(move || {
                let _ = sender.send(store.company_event_cursor());
            }));
        // A leaked SQL guard fails this ordering assertion without leaving a hung test.
        self.completed.store(
            matches!(receiver.recv_timeout(Duration::from_secs(1)), Ok(Ok(_))),
            Ordering::Release,
        );
        DependencyReadiness::Unavailable
    }

    fn authority_snapshot(
        &self,
        _tenant: &TenantId,
        _project: &ProjectId,
        _work: &WorkItemId,
        _agent: AgentId,
    ) -> Result<RuntimeAuthoritySnapshotV1, WorkflowPortError> {
        Err(WorkflowPortError::Unavailable)
    }
}

#[test]
fn readiness_facts_release_sql_before_live_dependency_checks() {
    let mut api = WorkflowApi::disabled().unwrap();
    let organization = Arc::new(ReadStoreDuringDependencyCheck {
        store: Arc::clone(&api.store),
        completed: AtomicBool::new(false),
        workers: Mutex::new(Vec::new()),
    });
    let port: Arc<dyn sentinel_workflow::OrganizationRuntimePort> = organization.clone();
    let execution: Arc<dyn WorkExecutionPort> =
        Arc::new(sentinel_workflow::UnavailableWorkExecutionPort);
    let completion: Arc<dyn CompletionEvidencePort> =
        Arc::new(sentinel_workflow::UnavailableCompletionEvidencePort);
    let gate: Arc<dyn GateEvidencePort> = Arc::new(UnavailableGateEvidencePort);
    api.core = Arc::new(WorkflowCore::new(
        Arc::clone(&api.store),
        port,
        execution,
        completion,
        gate,
    ));
    assert!(!api.health().ready);
    for worker in organization.workers.lock().unwrap().drain(..) {
        worker.join().unwrap();
    }
    assert!(organization.completed.load(Ordering::Acquire));
}

#[cfg(feature = "llm")]
mod adaptive {
    use super::*;
    use sentinel_limbo::rusqlite;

    fn facts(
        api: &WorkflowApi,
    ) -> Result<
        Vec<sentinel_workflow::AdaptiveHealthSession>,
        sentinel_workflow::AdaptiveHealthReadError,
    > {
        api.store
            .readiness_facts(MAX_RECONCILE_BATCH, true)
            .adaptive_sessions
    }

    #[test]
    fn readiness_facts_provider_outcomes_remain_live_after_unchanged_sql_proof() {
        let root = tempfile::tempdir().unwrap();
        let workflow = root.path().join("workflow.sqlite");
        let events = root.path().join("events.sqlite");
        let (api, session) = adaptive_recovery::tests::fixture(&workflow, &events, false);
        let effect = AdaptiveEffectV1 {
            id: Uuid::new_v4(),
            request_digest: "a".repeat(64),
        };
        api.store
            .advance_adaptive_session(
                session.grant.session_id,
                session.version,
                Uuid::new_v4(),
                &AdaptiveTransitionV1::ClaimModel {
                    effect: effect.clone(),
                    previous_observation_digest: None,
                },
                &session.grant.authority,
                now_unix_ms().max(session.updated_at_ms),
            )
            .unwrap();
        assert!(!api
            .adaptive_models_have_unknown_outcome_from_facts(facts(&api))
            .unwrap());
        let request = format!(
            "company-adaptive-{}-{}",
            session.grant.session_id, effect.id
        );
        api.event_store
            .as_ref()
            .unwrap()
            .reserve_llm_request(
                &request,
                &effect.request_digest,
                &session.grant.authority.agent_id.to_string(),
            )
            .unwrap();
        assert!(!api
            .adaptive_models_have_unknown_outcome_from_facts(facts(&api))
            .unwrap());
        assert!(api
            .event_store
            .as_ref()
            .unwrap()
            .mark_llm_provider_outcome_unknown(
                &request,
                &effect.request_digest,
                "UnknownOutcome: provider_transport_deadline_elapsed",
            )
            .unwrap());
        assert!(api
            .adaptive_models_have_unknown_outcome_from_facts(facts(&api))
            .unwrap());
        assert_eq!(api.health().last_error.as_deref(), Some("UnknownOutcome"));
        // A different digest or nonempty result is not the exact pending Unknown proof.
        let connection = rusqlite::Connection::open(&events).unwrap();
        connection
            .execute(
                "UPDATE llm_completion_outbox SET request_digest='different' WHERE request_id=?1",
                [&request],
            )
            .unwrap();
        assert!(!api
            .adaptive_models_have_unknown_outcome_from_facts(facts(&api))
            .unwrap());
        connection.execute("UPDATE llm_completion_outbox SET request_digest=?1,payload='nonempty' WHERE request_id=?2",
            rusqlite::params![&effect.request_digest, &request]).unwrap();
        assert!(!api
            .adaptive_models_have_unknown_outcome_from_facts(facts(&api))
            .unwrap());
        connection
            .execute(
                "UPDATE llm_completion_outbox SET payload='',owner_scope=?1 WHERE request_id=?2",
                rusqlite::params![
                    sentinel_common::StateTransferScope::for_agent("999").to_wire(),
                    &request
                ],
            )
            .unwrap();
        assert!(!api
            .adaptive_models_have_unknown_outcome_from_facts(facts(&api))
            .unwrap());
    }

    #[test]
    fn readiness_facts_session_and_project_failures_preserve_error_precedence() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("workflow.sqlite");
        let (api, _) =
            adaptive_recovery::tests::fixture(&path, &root.path().join("events.sqlite"), false);
        api.publish_reconciliation_status(Some("existing-scan-failure".into()));
        facts(&api).unwrap();
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute(
            "INSERT INTO workflow_operations VALUES('adaptive-session-v1:bad','bad','invalid',X'7b7d',1)", [],
        ).unwrap();
        assert_eq!(
            api.health().last_error.as_deref(),
            Some("adaptive sessions unavailable")
        );
        connection.execute(
            "INSERT INTO company_entities(tenant_id,entity_kind,entity_id,version,payload,payload_digest) VALUES('tenant','project','damaged',1,X'7b7d','invalid')", [],
        ).unwrap();
        assert_eq!(
            api.health().last_error.as_deref(),
            Some("adaptive projects unavailable")
        );
    }

    #[test]
    fn readiness_facts_full_get_preserves_business_rows_and_original_session() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("workflow.sqlite");
        let (api, session) =
            adaptive_recovery::tests::fixture(&path, &root.path().join("events.sqlite"), false);
        let before = api.store.company_projects().unwrap();
        let cursor = api.store.company_event_cursor().unwrap();
        let original = api
            .store
            .adaptive_session(session.grant.session_id, &session.grant.authority)
            .unwrap();
        let headers = HashMap::from([(
            "authorization".to_owned(),
            format!("Bearer test-credential-8-{}", "x".repeat(32)),
        )]);
        for _ in 0..2 {
            let health = api.health();
            let response = api
                .handle("GET", WORKFLOW_READINESS_PATH, &headers, b"")
                .unwrap();
            assert_eq!(response.status, 200);
            let body: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
            assert_eq!(body["pending_execution"], health.pending_execution);
            assert_eq!(body["pending_completion"], health.pending_completion);
            assert_eq!(body["pending_gate"], health.pending_gate);
            assert_eq!(
                body["canonical_event_cursor"],
                health.canonical_event_cursor.unwrap()
            );
            assert_eq!(
                health.pending_execution,
                api.store
                    .pending_executions(MAX_RECONCILE_BATCH)
                    .unwrap()
                    .len()
            );
            assert_eq!(
                health.pending_completion,
                api.store
                    .pending_completion_evidence(MAX_RECONCILE_BATCH)
                    .unwrap()
                    .len()
            );
            assert_eq!(
                health.pending_gate,
                api.store
                    .pending_gate_evidence(MAX_RECONCILE_BATCH)
                    .unwrap()
                    .len()
            );
        }
        assert_eq!(api.store.company_projects().unwrap(), before);
        assert_eq!(api.store.company_event_cursor().unwrap(), cursor);
        assert_eq!(
            api.store
                .adaptive_session(session.grant.session_id, &session.grant.authority)
                .unwrap(),
            original
        );
    }

    #[test]
    fn readiness_facts_outbox_failure_keeps_original_fallback_counter() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("workflow.sqlite");
        let (api, _) =
            adaptive_recovery::tests::fixture(&path, &root.path().join("events.sqlite"), false);
        rusqlite::Connection::open(&path).unwrap().execute(
            "INSERT INTO workflow_completion_outbox(request_id,invocation_id,tenant_id,project_id,work_item_id,state,request,updated_at_ms) VALUES('r','i','t','p','w','pending',X'7b7d',1)", [],
        ).unwrap();
        let health = api.health();
        assert!(!health.ready);
        assert_eq!(health.pending_completion, MAX_RECONCILE_BATCH);
        assert_eq!(
            health.pending_execution,
            api.store
                .pending_executions(MAX_RECONCILE_BATCH)
                .unwrap()
                .len()
        );
        assert_eq!(
            health.pending_gate,
            api.store
                .pending_gate_evidence(MAX_RECONCILE_BATCH)
                .unwrap()
                .len()
        );
    }
}
