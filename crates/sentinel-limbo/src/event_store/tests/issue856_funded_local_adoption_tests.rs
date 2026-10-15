use super::*;

const REQUEST: &str = "company-leadership-funded-local-adoption-test";
const AGENT: &str = "AGENT-07";
const KEY: &str = "local-adoption-funded-test";
const FUNDING_ERROR: &str = "work funding current reviews unavailable";
const LEGACY_ERROR: &str = "continuation audit invalid";

fn request_digest() -> String {
    "d".repeat(64)
}

fn receipt_digest() -> String {
    "e".repeat(64)
}

fn retained_response() -> (String, DomainEvent) {
    let usage_payload = serde_json::json!({
        "type": "AgentLlmUsage", "agent_id": 7,
        "tenant_id": "tenant-test", "project_id": "project-test",
        "work_item_id": "work-test", "reservation_id": "review-test",
        "assignment_id": "assignment-test", "assignment_version": 1,
        "provider": "codex-cli", "requested_model": "model-test",
        "effective_model": "model-test", "caller_role": "agent_runtime",
        "tier": "mid", "hierarchy_tier": 2, "cost_source": "provider_reported",
        "input_tokens": 5, "output_tokens": 10, "cache_read": 1, "cache_creation": 2,
        "cost_usd": 0.125,
    });
    let typed_usage: DomainEventPayload = serde_json::from_value(usage_payload).unwrap();
    let usage = DomainEvent::new("agent_llm_usage", AGENT, &typed_usage.to_json(), REQUEST, 1)
        .with_operation_id(&format!("llm_usage_{REQUEST}"))
        .with_schema_version(6);
    // Limbo retains raw model bytes; the daemon/domain own typed decision admission.
    let content = serde_json::json!({
        "schema_version": 5, "decision": {"kind": "continue",
            "additional_model_calls": 1, "window_ms": 180_000,
            "rationale": "Test-only funded decision", "evidence_refs": ["funding-test"]},
    }).to_string();
    let payload = serde_json::json!({
        "version": 2, "request_id": REQUEST, "request_digest": request_digest(),
        "actions": [], "usage_event": usage,
        "model_response_digest": sentinel_common::sha256_hex(content.as_bytes()),
        "model_work": {"context": {"binding": {"schema_version": 5}},
            "content": content, "admissible": true},
    }).to_string();
    (payload, usage)
}

fn failed(store: &EventStore, payload: &str, usage: &DomainEvent, error: &str, attempts: u32) {
    assert!(store.reserve_llm_request(REQUEST, &request_digest(), AGENT).unwrap());
    store.enqueue_llm_completion(REQUEST, &request_digest(), payload).unwrap();
    store.persist_llm_completion_usage(REQUEST, &request_digest(), usage).unwrap();
    for attempt in 1..=attempts {
        assert_eq!(
            store.record_llm_completion_failure(REQUEST, &request_digest(), error, attempts).unwrap(),
            (attempt, attempt == attempts)
        );
    }
}

fn dispose(store: &EventStore, payload: &str, retire: bool) -> anyhow::Result<bool> {
    let digest = sentinel_common::sha256_hex(payload.as_bytes());
    if retire {
        store.retire_known_funded_leadership_local_adoption(
            REQUEST, &request_digest(), &digest, KEY, &receipt_digest(),
        )
    } else {
        store.finish_known_funded_leadership_local_adoption(
            REQUEST, &request_digest(), &digest, KEY, &receipt_digest(),
        )
    }
}

fn legacy_dispose(store: &EventStore, payload: &str, retire: bool) -> anyhow::Result<bool> {
    let digest = sentinel_common::sha256_hex(payload.as_bytes());
    if retire {
        store.retire_known_leadership_local_adoption(
            REQUEST, &request_digest(), &digest, KEY, &receipt_digest(),
        )
    } else {
        store.finish_known_leadership_local_adoption(
            REQUEST, &request_digest(), &digest, KEY, &receipt_digest(),
        )
    }
}

fn snapshot(store: &EventStore) -> (Option<LlmCompletionEntry>, Vec<u8>) {
    (
        store.get_llm_completion(REQUEST).unwrap(),
        serde_json::to_vec(&store.get_all_events().unwrap()).unwrap(),
    )
}

fn assert_no_mutation(store: &EventStore, payload: &str, retire: bool) {
    let before = snapshot(store);
    assert!(dispose(store, payload, retire).is_err());
    assert_eq!(snapshot(store), before);
    assert!(store.conn().is_autocommit());
}

fn operation(retire: bool) -> String {
    if retire {
        format!("llm_local_adoption_retired_{REQUEST}")
    } else {
        format!("llm_local_adoption_{REQUEST}")
    }
}

#[test]
fn funded_dispositions_preserve_response_usage_owner_and_exhausted_budget() {
    for retire in [false, true] {
        let store = EventStore::open(":memory:").unwrap();
        let (payload, usage) = retained_response();
        failed(&store, &payload, &usage, FUNDING_ERROR, 5);
        let mut expected = store.get_llm_completion(REQUEST).unwrap().unwrap();
        let event_count = store.get_all_events().unwrap().len();
        assert!(dispose(&store, &payload, retire).unwrap());
        if retire {
            expected.last_error = Some("leadership_review_stale".into());
        } else {
            expected.status = "action_claimed".into();
        }
        assert_eq!(store.get_llm_completion(REQUEST).unwrap(), Some(expected));
        assert_eq!(store.get_all_events().unwrap().len(), event_count + 1);
        let persisted = store.event_by_operation_id(&usage.operation_id).unwrap().unwrap();
        assert_eq!(
            serde_json::to_value(persisted).unwrap(),
            serde_json::to_value(&usage).unwrap()
        );
        let audit = store.event_by_operation_id(&operation(retire)).unwrap().unwrap();
        assert_eq!(audit.schema_version, 2);
        assert_eq!(audit.aggregate_id, AGENT);
        assert_eq!(audit.correlation_id, REQUEST);
        assert_eq!(audit.event_type, if retire {
            "llm_completion_local_adoption_retired"
        } else { "llm_completion_locally_adopted" });
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&audit.payload).unwrap(),
            serde_json::json!({
                "schema_version": 2, "request_id": REQUEST, "request_digest": request_digest(),
                "payload_digest": sentinel_common::sha256_hex(payload.as_bytes()),
                "adoption_key": KEY, "domain_receipt_digest": receipt_digest(),
                "original_completion_error": FUNDING_ERROR,
            })
        );
        let before = snapshot(&store);
        assert!(store.poll_llm_completions(10).unwrap().is_empty());
        assert!(store.poll_llm_provider_in_flight(10).unwrap().is_empty());
        assert!(!store.reserve_llm_request(REQUEST, &request_digest(), AGENT).unwrap());
        assert!(!store.claim_llm_completion_actions(REQUEST, &request_digest()).unwrap());
        let usage_replay = store.persist_llm_completion_usage(REQUEST, &request_digest(), &usage);
        if retire {
            assert!(usage_replay.is_err());
        } else {
            usage_replay.unwrap();
        }
        assert_eq!(snapshot(&store), before);
        assert_eq!(store.get_all_events().unwrap().iter()
            .filter(|event| event.event_type == "agent_llm_usage").count(), 1);
    }
}

#[test]
fn funded_dispositions_replay_exactly_after_reopen_and_reject_changed_receipts() {
    for retire in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("funded-adoption.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();
        let (payload, usage) = retained_response();
        failed(&store, &payload, &usage, FUNDING_ERROR, 5);
        assert!(dispose(&store, &payload, retire).unwrap());
        let before = snapshot(&store);
        assert!(!dispose(&store, &payload, retire).unwrap());
        drop(store);
        let store = EventStore::open(path.to_str().unwrap()).unwrap();
        assert!(!dispose(&store, &payload, retire).unwrap());
        assert_eq!(snapshot(&store), before);
        let payload_digest = sentinel_common::sha256_hex(payload.as_bytes());
        let changed = "f".repeat(64);
        for (key, receipt) in [("local-adoption-other", receipt_digest()), (KEY, changed)] {
            let rejected = if retire {
                store.retire_known_funded_leadership_local_adoption(
                    REQUEST, &request_digest(), &payload_digest, key, &receipt,
                )
            } else {
                store.finish_known_funded_leadership_local_adoption(
                    REQUEST, &request_digest(), &payload_digest, key, &receipt,
                )
            };
            assert!(rejected.is_err());
            assert_eq!(snapshot(&store), before);
        }
        assert_no_mutation(&store, &payload, !retire);
    }
}

#[test]
fn funded_dispositions_reject_other_errors_and_non_five_attempt_terminal_rows() {
    for retire in [false, true] {
        for error in [LEGACY_ERROR, "unrelated failure", "work funding current reviews unavailable "] {
            let store = EventStore::open(":memory:").unwrap();
            let (payload, usage) = retained_response();
            failed(&store, &payload, &usage, error, 5);
            assert_no_mutation(&store, &payload, retire);
        }
        for attempts in [1, 4, 6] {
            let store = EventStore::open(":memory:").unwrap();
            let (payload, usage) = retained_response();
            failed(&store, &payload, &usage, FUNDING_ERROR, attempts);
            assert_no_mutation(&store, &payload, retire);
        }
        for status in ["pending_usage", "ready_for_action", "action_claimed"] {
            let store = EventStore::open(":memory:").unwrap();
            let (payload, usage) = retained_response();
            failed(&store, &payload, &usage, FUNDING_ERROR, 5);
            store
                .conn()
                .execute("UPDATE llm_completion_outbox SET status=?1", [status])
                .unwrap();
            assert_no_mutation(&store, &payload, retire);
        }
        let store = EventStore::open(":memory:").unwrap();
        let (payload, _) = retained_response();
        store.reserve_llm_request(REQUEST, &request_digest(), AGENT).unwrap();
        store.mark_llm_provider_outcome_unknown(
            REQUEST,
            &request_digest(),
            "UnknownOutcome: provider_transport_deadline_elapsed",
        ).unwrap();
        assert_no_mutation(&store, &payload, retire);
    }
}

#[test]
fn funded_dispositions_reject_changed_source_digests_and_owner() {
    for retire in [false, true] {
        for target in ["request", "payload", "domain", "key", "owner", "world"] {
            let store = EventStore::open(":memory:").unwrap();
            let (payload, usage) = retained_response();
            failed(&store, &payload, &usage, FUNDING_ERROR, 5);
            if target == "owner" || target == "world" {
                let owner = if target == "owner" {
                    StateTransferScope::for_agent("AGENT-08").to_wire()
                } else { StateTransferScope::World.to_wire() };
                store.conn().execute("UPDATE llm_completion_outbox SET owner_scope=?1", [owner]).unwrap();
                assert_no_mutation(&store, &payload, retire);
                continue;
            }
            let before = snapshot(&store);
            let request = if target == "request" { "a".repeat(64) } else { request_digest() };
            let digest = if target == "payload" { "b".repeat(64) }
                else { sentinel_common::sha256_hex(payload.as_bytes()) };
            let domain = if target == "domain" { String::new() } else { receipt_digest() };
            let key = if target == "key" { "not-an-adoption" } else { KEY };
            let rejected = if retire {
                store.retire_known_funded_leadership_local_adoption(REQUEST, &request, &digest, key, &domain)
            } else {
                store.finish_known_funded_leadership_local_adoption(REQUEST, &request, &digest, key, &domain)
            };
            assert!(rejected.is_err(), "{target}");
            assert_eq!(snapshot(&store), before);
        }
    }
}

#[test]
fn funded_dispositions_require_known_raw_response_and_exact_durable_usage() {
    for retire in [false, true] {
        for target in [
            "actions", "unknown", "response", "request", "usage", "missing-usage",
            "usage-owner", "empty-content", "unknown-shape",
        ] {
            let store = EventStore::open(":memory:").unwrap();
            let (payload, usage) = retained_response();
            let mut value: serde_json::Value = serde_json::from_str(&payload).unwrap();
            match target {
                "actions" => value["actions"] = serde_json::json!([{}]),
                "unknown" => value["model_work"]["admissible"] = serde_json::json!(false),
                "response" => value["model_response_digest"] = serde_json::json!("a".repeat(64)),
                "request" => value["request_id"] = serde_json::json!("company-leadership-other"),
                "usage" => value["usage_event"]["timestamp_ms"] = serde_json::json!(usage.timestamp_ms + 1),
                "usage-owner" => value["usage_event"]["aggregate_id"] = serde_json::json!("AGENT-08"),
                "empty-content" => {
                    value["model_work"]["content"] = serde_json::json!("");
                    value["model_response_digest"] = serde_json::json!(sentinel_common::sha256_hex(b""));
                }
                "unknown-shape" => value["model_work"]["unknown"] = serde_json::json!(true),
                "missing-usage" => {},
                _ => unreachable!(),
            }
            let payload = value.to_string();
            failed(&store, &payload, &usage, FUNDING_ERROR, 5);
            if target == "missing-usage" {
                store.conn().execute("DELETE FROM events WHERE operation_id=?1", [&usage.operation_id]).unwrap();
            }
            assert_no_mutation(&store, &payload, retire);
        }
    }
}

#[test]
fn funded_and_legacy_dispositions_never_cross_failure_kinds_or_replay_receipts() {
    for retire in [false, true] {
        for funded in [false, true] {
            let store = EventStore::open(":memory:").unwrap();
            let (payload, usage) = retained_response();
            failed(&store, &payload, &usage, if funded { FUNDING_ERROR } else { LEGACY_ERROR }, 5);
            let before = snapshot(&store);
            let wrong = if funded { legacy_dispose(&store, &payload, retire) }
                else { dispose(&store, &payload, retire) };
            assert!(wrong.is_err());
            assert_eq!(snapshot(&store), before);
            if funded {
                assert!(dispose(&store, &payload, retire).unwrap());
            } else {
                assert!(legacy_dispose(&store, &payload, retire).unwrap());
                let audit = store.event_by_operation_id(&operation(retire)).unwrap().unwrap();
                let mut expected = serde_json::json!({
                    "request_id": REQUEST, "request_digest": request_digest(),
                    "payload_digest": sentinel_common::sha256_hex(payload.as_bytes()),
                    "adoption_key": KEY, "domain_receipt_digest": receipt_digest(),
                });
                if retire { expected["original_completion_error"] = serde_json::json!(LEGACY_ERROR); }
                assert_eq!(audit.payload, expected.to_string(), "legacy audit bytes changed");
                assert_eq!(audit.schema_version, 1);
            }
            let before = snapshot(&store);
            let wrong = if funded { legacy_dispose(&store, &payload, retire) }
                else { dispose(&store, &payload, retire) };
            assert!(wrong.is_err());
            assert_eq!(snapshot(&store), before);
        }
    }
}

#[test]
fn funded_disposition_audit_and_state_failures_roll_back_before_exact_retry() {
    for retire in [false, true] {
        for phase in ["audit", "state"] {
            let store = EventStore::open(":memory:").unwrap();
            let (payload, usage) = retained_response();
            failed(&store, &payload, &usage, FUNDING_ERROR, 5);
            let trigger = if phase == "audit" {
                "CREATE TRIGGER reject_funded_disposition BEFORE INSERT ON events
                 WHEN NEW.event_type IN ('llm_completion_locally_adopted','llm_completion_local_adoption_retired')
                 BEGIN SELECT RAISE(ABORT, 'test audit failure'); END;"
            } else {
                "CREATE TRIGGER reject_funded_disposition BEFORE UPDATE ON llm_completion_outbox
                 BEGIN SELECT RAISE(ABORT, 'test state failure'); END;"
            };
            store.conn().execute_batch(trigger).unwrap();
            assert_no_mutation(&store, &payload, retire);
            assert!(store.event_by_operation_id(&operation(retire)).unwrap().is_none());
            store.conn().execute_batch("DROP TRIGGER reject_funded_disposition").unwrap();
            assert!(dispose(&store, &payload, retire).unwrap());
            assert!(!dispose(&store, &payload, retire).unwrap());
        }
    }
}

#[test]
fn funded_replay_rejects_changed_audit_source_or_error_kind() {
    for retire in [false, true] {
        for target in ["kind", "owner", "version", "source", "error", "attempts"] {
            let store = EventStore::open(":memory:").unwrap();
            let (payload, usage) = retained_response();
            failed(&store, &payload, &usage, FUNDING_ERROR, 5);
            dispose(&store, &payload, retire).unwrap();
            let op = operation(retire);
            match target {
                "kind" => {
                    let audit = store.event_by_operation_id(&op).unwrap().unwrap();
                    let mut value: serde_json::Value = serde_json::from_str(&audit.payload).unwrap();
                    value["original_completion_error"] = serde_json::json!(LEGACY_ERROR);
                    store.conn().execute("UPDATE events SET payload=?1 WHERE operation_id=?2", params![value.to_string(), op]).unwrap();
                }
                "owner" => { store.conn().execute("UPDATE events SET aggregate_id='AGENT-08' WHERE operation_id=?1", [&op]).unwrap(); }
                "version" => { store.conn().execute("UPDATE events SET schema_version=1 WHERE operation_id=?1", [&op]).unwrap(); }
                "source" => { store.conn().execute("UPDATE events SET correlation_id='other' WHERE operation_id=?1", [&op]).unwrap(); }
                "error" => { store.conn().execute("UPDATE llm_completion_outbox SET last_error=?1", [LEGACY_ERROR]).unwrap(); }
                "attempts" => { store.conn().execute("UPDATE llm_completion_outbox SET attempt_count=6", []).unwrap(); }
                _ => unreachable!(),
            }
            assert_no_mutation(&store, &payload, retire);
        }
    }
}
