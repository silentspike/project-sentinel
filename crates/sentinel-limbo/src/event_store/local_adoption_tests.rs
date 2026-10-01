#[cfg(test)]
mod tests {
    use super::super::*;

    const REQUEST_ID: &str = "company-leadership-local-adoption-test";
    const AGENT: &str = "AGENT-07";
    const FAILURE: &str = "continuation audit invalid";
    const ADOPTION_KEY: &str = "local-adoption-test";
    const KNOWN_PAYLOAD: &str = r#"{"version":2,"actions":[],"model_work":{"admissible":true}}"#;

    fn request_digest() -> String {
        "d".repeat(64)
    }

    fn receipt_digest() -> String {
        "e".repeat(64)
    }

    fn failed_completion(store: &EventStore, payload: &str, failure: &str) {
        let digest = request_digest();
        assert!(store
            .reserve_llm_request(REQUEST_ID, &digest, AGENT)
            .unwrap());
        store
            .enqueue_llm_completion(REQUEST_ID, &digest, payload)
            .unwrap();
        let usage = DomainEvent::new("agent_llm_usage", AGENT, "{}", REQUEST_ID, 0)
            .with_operation_id(&format!("llm_usage_{REQUEST_ID}"));
        store
            .persist_llm_completion_usage(REQUEST_ID, &digest, &usage)
            .unwrap();
        for attempt in 1..=3 {
            assert_eq!(
                store
                    .record_llm_completion_failure(REQUEST_ID, &digest, failure, 3)
                    .unwrap(),
                (attempt, attempt == 3)
            );
        }
    }

    fn finish(store: &EventStore, payload: &str) -> anyhow::Result<bool> {
        store.finish_known_leadership_local_adoption(
            REQUEST_ID,
            &request_digest(),
            &sentinel_common::sha256_hex(payload.as_bytes()),
            ADOPTION_KEY,
            &receipt_digest(),
        )
    }

    fn assert_rejected_without_mutation(store: &EventStore, payload: &str) {
        let before = store.get_llm_completion(REQUEST_ID).unwrap();
        let event_count = store.get_all_events().unwrap().len();
        assert!(finish(store, payload).is_err());
        assert_eq!(store.get_llm_completion(REQUEST_ID).unwrap(), before);
        assert_eq!(store.get_all_events().unwrap().len(), event_count);
        assert!(store
            .event_by_operation_id(&format!("llm_local_adoption_{REQUEST_ID}"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn failed_exact_known_local_adoption_retains_completion_evidence() {
        let store = EventStore::open(":memory:").unwrap();
        failed_completion(&store, KNOWN_PAYLOAD, FAILURE);
        let before = store.get_llm_completion(REQUEST_ID).unwrap().unwrap();
        assert_eq!(before.status, "failed");
        assert_eq!(before.payload, KNOWN_PAYLOAD);
        assert_eq!(before.last_error.as_deref(), Some(FAILURE));
        assert_eq!(before.attempt_count, 3);

        assert!(finish(&store, KNOWN_PAYLOAD).unwrap());
        let mut expected = before;
        expected.status = "action_claimed".to_string();
        assert_eq!(
            store.get_llm_completion(REQUEST_ID).unwrap(),
            Some(expected)
        );
        let event = store
            .event_by_operation_id(&format!("llm_local_adoption_{REQUEST_ID}"))
            .unwrap()
            .unwrap();
        assert_eq!(event.event_type, "llm_completion_locally_adopted");
        assert_eq!(event.aggregate_id, AGENT);
        assert_eq!(event.correlation_id, REQUEST_ID);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&event.payload).unwrap(),
            serde_json::json!({
                "request_id": REQUEST_ID,
                "request_digest": request_digest(),
                "payload_digest": sentinel_common::sha256_hex(KNOWN_PAYLOAD.as_bytes()),
                "adoption_key": ADOPTION_KEY,
                "domain_receipt_digest": receipt_digest(),
            })
        );
        assert!(store.poll_llm_completions(10).unwrap().is_empty());
        assert!(store.poll_llm_provider_in_flight(10).unwrap().is_empty());
        assert!(!store
            .reserve_llm_request(REQUEST_ID, &request_digest(), AGENT)
            .unwrap());
        assert!(!store
            .claim_llm_completion_actions(REQUEST_ID, &request_digest())
            .unwrap());
    }

    #[test]
    fn same_local_adoption_receipt_is_idempotent_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local-adoption.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();
        failed_completion(&store, KNOWN_PAYLOAD, FAILURE);
        assert!(finish(&store, KNOWN_PAYLOAD).unwrap());
        let before = store.get_llm_completion(REQUEST_ID).unwrap();
        let event = store
            .event_by_operation_id(&format!("llm_local_adoption_{REQUEST_ID}"))
            .unwrap()
            .unwrap();
        let event_count = store.get_all_events().unwrap().len();
        assert!(!finish(&store, KNOWN_PAYLOAD).unwrap());
        drop(store);

        let store = EventStore::open(path.to_str().unwrap()).unwrap();
        assert!(!finish(&store, KNOWN_PAYLOAD).unwrap());
        assert_eq!(store.get_llm_completion(REQUEST_ID).unwrap(), before);
        assert_eq!(store.get_all_events().unwrap().len(), event_count);
        let replay = store
            .event_by_operation_id(&format!("llm_local_adoption_{REQUEST_ID}"))
            .unwrap()
            .unwrap();
        assert_eq!(replay.event_id, event.event_id);
        assert_eq!(replay.payload, event.payload);
    }

    #[test]
    fn conflicting_local_adoption_receipt_is_rejected() {
        let store = EventStore::open(":memory:").unwrap();
        failed_completion(&store, KNOWN_PAYLOAD, FAILURE);
        assert!(finish(&store, KNOWN_PAYLOAD).unwrap());
        let before = store.get_llm_completion(REQUEST_ID).unwrap();
        let operation = format!("llm_local_adoption_{REQUEST_ID}");
        let receipt = store.event_by_operation_id(&operation).unwrap().unwrap();
        let event_count = store.get_all_events().unwrap().len();
        let original_digest = receipt_digest();
        let changed_digest = "f".repeat(64);
        for (key, digest) in [
            ("local-adoption-conflicting", original_digest.as_str()),
            (ADOPTION_KEY, changed_digest.as_str()),
        ] {
            assert!(store
                .finish_known_leadership_local_adoption(
                    REQUEST_ID,
                    &request_digest(),
                    &sentinel_common::sha256_hex(KNOWN_PAYLOAD.as_bytes()),
                    key,
                    digest,
                )
                .is_err());
            assert_eq!(store.get_llm_completion(REQUEST_ID).unwrap(), before);
            assert_eq!(store.get_all_events().unwrap().len(), event_count);
            let retained = store.event_by_operation_id(&operation).unwrap().unwrap();
            assert_eq!(retained.event_id, receipt.event_id);
            assert_eq!(retained.payload, receipt.payload);
        }
    }

    #[test]
    fn local_adoption_rejects_unknown_provider_outcome() {
        let store = EventStore::open(":memory:").unwrap();
        let digest = request_digest();
        assert!(store
            .reserve_llm_request(REQUEST_ID, &digest, AGENT)
            .unwrap());
        assert!(store
            .mark_llm_provider_outcome_unknown(
                REQUEST_ID,
                &digest,
                "UnknownOutcome: provider_transport_deadline_elapsed",
            )
            .unwrap());
        assert_rejected_without_mutation(&store, "");
    }

    #[test]
    fn local_adoption_rejects_empty_unknown_and_action_bearing_payloads() {
        for payload in [
            "",
            r#"{"version":2,"actions":[],"model_work":{"admissible":false}}"#,
            r#"{"version":2,"actions":[],"model_work":{}}"#,
            r#"{"version":2,"actions":[{"type":"send_message"}],"model_work":{"admissible":true}}"#,
        ] {
            let store = EventStore::open(":memory:").unwrap();
            failed_completion(&store, payload, FAILURE);
            assert_rejected_without_mutation(&store, payload);
        }
    }

    #[test]
    fn local_adoption_requires_exact_digests_and_failure_state() {
        let store = EventStore::open(":memory:").unwrap();
        failed_completion(&store, KNOWN_PAYLOAD, FAILURE);
        let before = store.get_llm_completion(REQUEST_ID).unwrap();
        let digest = request_digest();
        let payload_digest = sentinel_common::sha256_hex(KNOWN_PAYLOAD.as_bytes());
        let changed = "a".repeat(64);
        let event_count = store.get_all_events().unwrap().len();
        for (request, payload) in [
            (changed.as_str(), payload_digest.as_str()),
            (digest.as_str(), changed.as_str()),
        ] {
            assert!(store
                .finish_known_leadership_local_adoption(
                    REQUEST_ID,
                    request,
                    payload,
                    ADOPTION_KEY,
                    &receipt_digest(),
                )
                .is_err());
            assert_eq!(store.get_llm_completion(REQUEST_ID).unwrap(), before);
            assert_eq!(store.get_all_events().unwrap().len(), event_count);
        }

        let store = EventStore::open(":memory:").unwrap();
        failed_completion(&store, KNOWN_PAYLOAD, "different failure");
        assert_rejected_without_mutation(&store, KNOWN_PAYLOAD);
    }

    fn retire(store: &EventStore, payload: &str) -> anyhow::Result<bool> {
        store.retire_known_leadership_local_adoption(
            REQUEST_ID,
            &request_digest(),
            &sentinel_common::sha256_hex(payload.as_bytes()),
            ADOPTION_KEY,
            &receipt_digest(),
        )
    }

    #[test]
    fn local_adoption_retirement_preserves_history_and_replays_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("retirement.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();
        failed_completion(&store, KNOWN_PAYLOAD, FAILURE);
        let mut expected = store.get_llm_completion(REQUEST_ID).unwrap().unwrap();
        expected.last_error = Some("leadership_review_stale".into());
        assert!(retire(&store, KNOWN_PAYLOAD).unwrap());
        assert_eq!(
            store.get_llm_completion(REQUEST_ID).unwrap(),
            Some(expected.clone())
        );
        let operation = format!("llm_local_adoption_retired_{REQUEST_ID}");
        let receipt = store.event_by_operation_id(&operation).unwrap().unwrap();
        assert_eq!(receipt.event_type, "llm_completion_local_adoption_retired");
        assert_eq!(receipt.aggregate_id, AGENT);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&receipt.payload).unwrap(),
            serde_json::json!({"request_id":REQUEST_ID,"request_digest":request_digest(),
            "payload_digest":sentinel_common::sha256_hex(KNOWN_PAYLOAD.as_bytes()),
            "adoption_key":ADOPTION_KEY,"domain_receipt_digest":receipt_digest(),
            "original_completion_error":FAILURE})
        );
        let count = store.get_all_events().unwrap().len();
        drop(store);
        let store = EventStore::open(path.to_str().unwrap()).unwrap();
        assert!(!retire(&store, KNOWN_PAYLOAD).unwrap());
        assert_eq!(
            store.get_llm_completion(REQUEST_ID).unwrap(),
            Some(expected)
        );
        assert_eq!(store.get_all_events().unwrap().len(), count);
        assert_eq!(
            store
                .event_by_operation_id(&operation)
                .unwrap()
                .unwrap()
                .event_id,
            receipt.event_id
        );
        assert!(store.poll_llm_completions(10).unwrap().is_empty());
        assert!(store.poll_llm_provider_in_flight(10).unwrap().is_empty());
        assert!(!store
            .reserve_llm_request(REQUEST_ID, &request_digest(), AGENT)
            .unwrap());
        assert!(!store
            .claim_llm_completion_actions(REQUEST_ID, &request_digest())
            .unwrap());
        assert!(finish(&store, KNOWN_PAYLOAD).is_err());
    }

    #[test]
    fn local_adoption_retirement_rejects_changed_receipts_and_non_known_responses() {
        let store = EventStore::open(":memory:").unwrap();
        failed_completion(&store, KNOWN_PAYLOAD, FAILURE);
        assert!(retire(&store, KNOWN_PAYLOAD).unwrap());
        let before = store.get_llm_completion(REQUEST_ID).unwrap();
        let count = store.get_all_events().unwrap().len();
        assert!(store
            .retire_known_leadership_local_adoption(
                REQUEST_ID,
                &request_digest(),
                &sentinel_common::sha256_hex(KNOWN_PAYLOAD.as_bytes()),
                ADOPTION_KEY,
                &"f".repeat(64)
            )
            .is_err());
        assert_eq!(store.get_llm_completion(REQUEST_ID).unwrap(), before);
        assert_eq!(store.get_all_events().unwrap().len(), count);
        for payload in [
            "",
            r#"{"version":2,"actions":[{}],"model_work":{"admissible":true}}"#,
            r#"{"version":2,"actions":[],"model_work":{"admissible":false}}"#,
        ] {
            let store = EventStore::open(":memory:").unwrap();
            failed_completion(&store, payload, FAILURE);
            let before = store.get_llm_completion(REQUEST_ID).unwrap();
            let count = store.get_all_events().unwrap().len();
            assert!(retire(&store, payload).is_err());
            assert_eq!(store.get_llm_completion(REQUEST_ID).unwrap(), before);
            assert_eq!(store.get_all_events().unwrap().len(), count);
        }
    }
}
