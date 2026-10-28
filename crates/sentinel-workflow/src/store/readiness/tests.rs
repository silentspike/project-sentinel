#![cfg(test)]

use super::*;
use crate::adaptive::continuation_tests::{effect, grant};
use crate::{AdaptiveSessionV1, AdaptiveTransitionV1};
use rusqlite::types::Value;

fn store() -> (tempfile::TempDir, std::path::PathBuf, WorkflowStore) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("readiness.sqlite");
    let store = WorkflowStore::open(&path).unwrap();
    (root, path, store)
}

fn pending(store: &WorkflowStore) -> AdaptiveSessionV1 {
    let grant = grant();
    let initial = store.begin_adaptive_session(&grant, &grant.authority, grant.created_at_ms).unwrap().1;
    store.advance_adaptive_session(
        grant.session_id, initial.version, Uuid::from_u128(500),
        &AdaptiveTransitionV1::ClaimModel { effect: effect(102), previous_observation_digest: None },
        &grant.authority, initial.updated_at_ms + 1,
    ).unwrap().1
}

fn unknown(store: &WorkflowStore) -> AdaptiveSessionV1 {
    let pending = pending(store);
    store.advance_adaptive_session(
        pending.grant.session_id, pending.version, Uuid::from_u128(501),
        &AdaptiveTransitionV1::MarkUnknown { effect: effect(102) },
        &pending.grant.authority, pending.updated_at_ms + 1,
    ).unwrap().1
}

fn rows(store: &WorkflowStore) -> Vec<Vec<Vec<Value>>> {
    let connection = store.lock().unwrap();
    ["company_entities", "company_events", "company_operations", "company_project_projections",
     "workflow_work_items", "workflow_operations", "workflow_adaptive_heads",
     "workflow_execution_outbox", "workflow_completion_outbox", "workflow_gate_outbox",
     "workflow_audit_events", "workflow_schema_meta"]
        .iter().map(|table| {
            let mut query = connection.prepare(&format!("SELECT * FROM {table} ORDER BY rowid")).unwrap();
            let columns = query.column_count();
            let values = query.query_map([], |row| (0..columns).map(|index| row.get(index)).collect())
                .unwrap().collect::<rusqlite::Result<Vec<Vec<Value>>>>().unwrap();
            values
        }).collect()
}

fn assert_unlocked(store: &WorkflowStore) {
    assert!(store.connection.try_lock().unwrap().is_autocommit());
    assert!(store.validated_reads.try_lock().is_ok());
}

#[test]
fn readiness_facts_empty_matches_old_readers_and_reopen_without_writes() {
    let (_root, path, store) = store();
    let before = rows(&store);
    for current in [&store, &WorkflowStore::open(path).unwrap()] {
        let changes = current.lock().unwrap().total_changes();
        for include in [false, true] {
            let facts = current.readiness_facts(100, include);
            assert_eq!(facts.pending_execution.unwrap(), current.pending_executions(100).unwrap().len());
            assert_eq!(facts.pending_completion.unwrap(), current.pending_completion_evidence(100).unwrap().len());
            assert_eq!(facts.pending_gate.unwrap(), current.pending_gate_evidence(100).unwrap().len());
            assert_eq!(facts.canonical_event_cursor.unwrap(), current.company_event_cursor().unwrap());
            assert!(facts.adaptive_sessions.unwrap().is_empty());
            assert_eq!(rows(current), before);
            assert_eq!(current.lock().unwrap().total_changes(), changes);
            assert_unlocked(current);
        }
    }
}

#[test]
fn readiness_facts_projection_matches_full_inventory_and_reuses_proof() {
    let (_root, _path, store) = store();
    let session = pending(&store);
    let before = rows(&store);
    let (projects, sessions) = store.adaptive_health_inventory().unwrap();
    assert!(projects.is_empty());
    assert_eq!(sessions, vec![session.clone()]);
    let facts = store.readiness_facts(100, true).adaptive_sessions.unwrap();
    assert_eq!(facts.len(), 1);
    assert!(!facts[0].project_present());
    assert!(!facts[0].unknown_outcome());
    let model = facts[0].pending_model().unwrap();
    assert_eq!(model.request_id(), format!("company-adaptive-{}-{}", session.grant.session_id, effect(102).id));
    assert_eq!(model.request_digest(), effect(102).request_digest);
    assert_eq!(model.agent_id(), session.grant.authority.agent_id);
    let (again, validations) = crate::domain_store::validation_scope::with_completed_validations(|| {
        store.readiness_facts(100, true).adaptive_sessions.unwrap()
    });
    assert!(validations.is_empty(), "unchanged input must not reconstruct journals");
    assert_eq!(serde_json::to_value(facts).unwrap(), serde_json::to_value(again).unwrap());
    assert_eq!(rows(&store), before);
    assert_unlocked(&store);
}

#[test]
fn readiness_facts_valid_company_membership_and_cursor_match_legacy() {
    let (_root, _path, store, _, _, _, _, _) =
        crate::domain_store::adaptive_leadership_review::material_preview_fixture_for_tests();
    let (projects, sessions) = store.adaptive_health_inventory().unwrap();
    let facts = store.readiness_facts(100, true);
    assert_eq!(facts.canonical_event_cursor.unwrap(), store.company_event_cursor().unwrap());
    let compact = facts.adaptive_sessions.unwrap();
    assert_eq!(compact.len(), sessions.len());
    for (fact, session) in compact.iter().zip(sessions) {
        assert_eq!(fact.project_present(), projects.iter().any(|project| {
            project.tenant_id == session.grant.authority.tenant_id
                && project.project_id == session.grant.authority.project_id
        }));
        assert_eq!(fact.unknown_outcome(), matches!(session.cursor,
            AdaptiveCursorV1::ModelUnknown { .. } | AdaptiveCursorV1::ToolUnknown { .. }));
    }
}

#[test]
fn readiness_facts_unknown_does_not_hide_corrupt_neighbor() {
    let (_root, _path, store) = store();
    unknown(&store);
    assert!(store.readiness_facts(100, true).adaptive_sessions.unwrap()[0].unknown_outcome());
    store.lock().unwrap().execute(
        "INSERT INTO workflow_operations(operation_namespace,operation_id,request_digest,response,created_at_ms) VALUES(?1,'bad','invalid',?2,1)",
        params![format!("adaptive-session-v1:{}", Uuid::from_u128(999999)), b"{}".as_slice()],
    ).unwrap();
    let facts = store.readiness_facts(100, true);
    assert!(matches!(facts.adaptive_sessions, Err(AdaptiveHealthReadError::Sessions(_))));
    assert_eq!(facts.pending_execution.unwrap(), 0);
    assert!(facts.canonical_event_cursor.is_ok());
    assert_unlocked(&store);
}

#[test]
fn readiness_facts_external_project_corruption_invalidates_warm_proof() {
    let (_root, path, store) = store();
    unknown(&store);
    store.readiness_facts(100, true).adaptive_sessions.unwrap();
    Connection::open(path).unwrap().execute(
        "INSERT INTO company_entities(tenant_id,entity_kind,entity_id,version,payload,payload_digest) VALUES('tenant','project','damaged',1,?1,'invalid')",
        [b"{}".as_slice()],
    ).unwrap();
    let facts = store.readiness_facts(100, true);
    assert!(matches!(facts.adaptive_sessions, Err(AdaptiveHealthReadError::Projects(_))));
    assert!(facts.pending_completion.is_ok() && facts.pending_gate.is_ok());
    assert_unlocked(&store);
}

#[test]
fn readiness_facts_removed_journal_or_companion_rejects_after_warm_read() {
    for companion in [false, true] {
        let (_root, path, store) = store();
        let session = unknown(&store);
        store.readiness_facts(100, true).adaptive_sessions.unwrap();
        let namespace = format!("adaptive-session-v1:{}{}", session.grant.session_id,
            if companion { ":operations" } else { "" });
        Connection::open(path).unwrap().execute(
            "DELETE FROM workflow_operations WHERE operation_namespace=?1", [namespace],
        ).unwrap();
        assert!(matches!(store.readiness_facts(100, true).adaptive_sessions,
            Err(AdaptiveHealthReadError::Sessions(_))));
        assert_unlocked(&store);
    }
}

#[test]
fn readiness_facts_nonadaptive_mode_preserves_outbox_reads_without_adaptive_validation() {
    let (_root, _path, store) = store();
    store.lock().unwrap().execute(
        "INSERT INTO workflow_operations VALUES('adaptive-session-v1:bad','bad','invalid',X'7b7d',1)", [],
    ).unwrap();
    let facts = store.readiness_facts(100, false);
    assert!(facts.adaptive_sessions.unwrap().is_empty());
    assert_eq!(facts.pending_execution.unwrap(), 0);
    assert!(store.readiness_facts(100, true).adaptive_sessions.is_err());
}

fn insert_completion(store: &WorkflowStore, state: &str, request: &[u8]) {
    store.lock().unwrap().execute(
        "INSERT INTO workflow_completion_outbox(request_id,invocation_id,tenant_id,project_id,work_item_id,state,request,updated_at_ms) VALUES('r','i','t','p','w',?1,?2,1)",
        params![state, request],
    ).unwrap();
}

#[test]
fn readiness_facts_outbox_errors_are_independent_and_match_old_precedence() {
    for (state, payload) in [("invalid-state", b"{}".as_slice()), ("pending", b"invalid".as_slice()),
                              ("pending", b"{\"schema_version\":999}".as_slice())] {
        let (_root, _path, store) = store();
        insert_completion(&store, state, payload);
        let expected = store.pending_completion_evidence(100).unwrap_err();
        let facts = store.readiness_facts(100, true);
        assert_eq!(facts.pending_completion.unwrap_err(), expected);
        assert_eq!(facts.pending_execution.unwrap(), 0);
        assert_eq!(facts.pending_gate.unwrap(), 0);
        assert!(facts.adaptive_sessions.is_ok() && facts.canonical_event_cursor.is_ok());
        assert_unlocked(&store);
    }
}

#[test]
fn readiness_facts_execution_and_gate_state_and_payload_errors_match_old_readers() {
    for execution in [false, true] {
        for (state, payload) in [("invalid-state", b"{}".as_slice()), ("pending", b"invalid".as_slice())] {
            let (_root, _path, store) = store();
            let connection = store.lock().unwrap();
            if execution {
                connection.execute(
                    "INSERT INTO workflow_execution_outbox(invocation_id,tenant_id,project_id,work_item_id,plan_digest,step_ordinal,state,request,updated_at_ms) VALUES('i','t','p','w','digest',0,?1,?2,1)",
                    params![if state == "pending" { "executing" } else { state }, payload],
                ).unwrap();
            } else {
                connection.execute(
                    "INSERT INTO workflow_gate_outbox(request_id,plan_id,tenant_id,project_id,work_item_id,state,request,updated_at_ms) VALUES('r','i','t','p','w',?1,?2,1)",
                    params![state, payload],
                ).unwrap();
            }
            drop(connection);
            let facts = store.readiness_facts(100, true);
            if execution {
                assert_eq!(facts.pending_execution.unwrap_err(), store.pending_executions(100).unwrap_err());
                assert_eq!(facts.pending_gate.unwrap(), 0);
            } else {
                assert_eq!(facts.pending_gate.unwrap_err(), store.pending_gate_evidence(100).unwrap_err());
                assert_eq!(facts.pending_execution.unwrap(), 0);
            }
            assert_eq!(facts.pending_completion.unwrap(), 0);
            assert_unlocked(&store);
        }
    }
}

#[test]
fn readiness_facts_valid_pending_payloads_are_decoded_and_limit_preserved() {
    let (_root, _path, store) = store();
    let request = PendingCompletionEvidenceV1 {
        schema_version: WORKFLOW_SCHEMA_VERSION,
        request_id: "completion-1".into(),
        plan_id: Uuid::from_u128(8),
        plan_digest: "1".repeat(64),
        invocation_id: Uuid::from_u128(9),
        step_id: Uuid::from_u128(10),
        step_digest: "2".repeat(64),
        authority_snapshot_digest: "3".repeat(64),
        request_digest: "4".repeat(64),
        created_at_unix_ms: 1,
    };
    insert_completion(&store, "pending", &serde_json::to_vec(&request).unwrap());
    for limit in [0, 1, 100, 101] {
        assert_eq!(store.readiness_facts(limit, false).pending_completion.unwrap(),
            store.pending_completion_evidence(limit).unwrap().len());
    }
}

#[test]
fn readiness_facts_one_snapshot_preserves_mvcc_and_next_read_sees_external_edit() {
    let (_root, path, store) = store();
    pending(&store);
    let mut connection = store.lock().unwrap();
    let snapshot = connection.savepoint().unwrap();
    let before = adaptive_facts(&snapshot, &mut false).unwrap();
    Connection::open(path).unwrap().execute(
        "INSERT INTO workflow_operations VALUES('adaptive-session-v1:bad','bad','invalid',X'7b7d',1)", [],
    ).unwrap();
    let pinned = adaptive_facts(&snapshot, &mut false).unwrap();
    assert_eq!(serde_json::to_value(before).unwrap(), serde_json::to_value(pinned).unwrap());
    snapshot.commit().unwrap();
    drop(connection);
    assert!(store.readiness_facts(100, true).adaptive_sessions.is_err());
}

#[test]
fn readiness_facts_index_only_corruption_remains_fail_closed() {
    use std::io::{Seek, SeekFrom, Write};
    let (_root, path, store) = store();
    pending(&store);
    let connection = store.lock().unwrap();
    connection.execute_batch("CREATE INDEX readiness_test_index ON workflow_operations(request_digest); PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    let page: u32 = connection.query_row("PRAGMA page_size", [], |row| row.get(0)).unwrap();
    let root: u32 = connection.query_row("SELECT rootpage FROM sqlite_schema WHERE name='readiness_test_index'", [], |row| row.get(0)).unwrap();
    drop(connection);
    store.readiness_facts(100, true).adaptive_sessions.unwrap();
    store.lock().unwrap().execute_batch("PRAGMA shrink_memory").unwrap();
    let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(u64::from(root - 1) * u64::from(page))).unwrap();
    file.write_all(&vec![0; page as usize]).unwrap();
    file.sync_all().unwrap();
    assert!(store.readiness_facts(100, true).adaptive_sessions.is_err());
    assert_unlocked(&store);
}

#[test]
fn readiness_facts_poisoned_store_returns_all_field_failures() {
    let (_root, _path, store) = store();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _connection = store.lock().unwrap();
        panic!("poison fixture store");
    })).is_err());
    let facts = store.readiness_facts(100, true);
    assert!(facts.pending_execution.is_err() && facts.pending_completion.is_err()
        && facts.pending_gate.is_err() && facts.adaptive_sessions.is_err()
        && facts.canonical_event_cursor.is_err());
}
