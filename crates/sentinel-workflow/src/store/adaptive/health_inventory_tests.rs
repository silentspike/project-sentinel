#![cfg(test)]

use super::*;
use crate::adaptive::continuation_tests::{authorization, effect, grant};
use crate::AdaptiveCursorV1;
use rusqlite::types::Value;

struct Fixture {
    root: tempfile::TempDir,
    store: WorkflowStore,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let store = WorkflowStore::open(root.path().join("health.sqlite")).unwrap();
        Self { root, store }
    }

    fn reopen(&self) -> WorkflowStore {
        WorkflowStore::open(self.root.path().join("health.sqlite")).unwrap()
    }

    fn begin(&self, root: &AdaptiveSessionGrantV1) -> AdaptiveSessionV1 {
        self.store
            .begin_adaptive_session(root, &root.authority, root.created_at_ms)
            .unwrap()
            .1
    }

    fn advance(
        &self,
        session: &AdaptiveSessionV1,
        command: AdaptiveTransitionV1,
    ) -> AdaptiveSessionV1 {
        self.store
            .advance_adaptive_session(
                session.grant.session_id,
                session.version,
                Uuid::from_u128(10_000 + u128::from(session.version)),
                &command,
                &session.grant.authority,
                session.updated_at_ms + 1,
            )
            .unwrap()
            .1
    }

    fn unknown(&self, root: &AdaptiveSessionGrantV1) -> AdaptiveSessionV1 {
        let initial = self.begin(root);
        let pending = self.advance(
            &initial,
            AdaptiveTransitionV1::ClaimModel {
                effect: effect(102),
                previous_observation_digest: None,
            },
        );
        self.advance(
            &pending,
            AdaptiveTransitionV1::MarkUnknown {
                effect: effect(102),
            },
        )
    }

    fn continued(&self) -> AdaptiveSessionV1 {
        let source = self.unknown(&grant());
        let auth = authorization(&source);
        let command = AdaptiveTransitionV1::ContinueGoverned {
            authorization: auth.clone(),
        };
        let next = source.transition(&command, auth.issued_at_ms).unwrap();
        let mut connection = self.store.lock().unwrap();
        let tx = immediate(&mut connection).unwrap();
        let (_, digest) = load(&tx, source.grant.session_id).unwrap().unwrap();
        // Journal-only fixture: no company leadership or effect authority is minted.
        append(
            &tx,
            &namespace(source.grant.session_id),
            &Entry {
                previous_digest: Some(digest),
                command: Some(command.clone()),
                session: next.clone(),
                recovery_feedback: None,
            },
        )
        .unwrap();
        let command_digest = canonical_sha256(
            "sentinel.workflow.adaptive-command.v1",
            &(source.grant.session_id, source.version, &command),
        )
        .unwrap();
        insert_operation(
            &tx,
            &format!("{}:operations", namespace(source.grant.session_id)),
            &auth.operation_id.to_string(),
            &command_digest,
            &next,
            next.updated_at_ms,
        )
        .unwrap();
        update_head(&tx, &source, &next).unwrap();
        tx.commit().unwrap();
        next
    }

    fn rollover(&self) -> (AdaptiveSessionV1, AdaptiveSessionV1) {
        let previous = self.begin(&grant());
        let mut next = previous.grant.clone();
        next.session_id = Uuid::from_u128(202);
        next.provider_allowance_id = "rollover-allowance".into();
        next.created_at_ms = previous.grant.deadline_ms;
        next.deadline_ms = next.created_at_ms + 1_000;
        let current = self.begin(&next);
        let cancelled = load(&self.store.lock().unwrap(), previous.grant.session_id)
            .unwrap()
            .unwrap()
            .0;
        assert_eq!(cancelled.cursor, AdaptiveCursorV1::Cancelled);
        (cancelled, current)
    }

    fn rewrite_entry(&self, id: Uuid, version: u64, mutate: impl FnOnce(&mut Entry)) {
        let connection = self.store.lock().unwrap();
        let (_, mut entry) = evidence_entry(&connection, &namespace(id), version).unwrap();
        mutate(&mut entry);
        let digest = canonical_sha256("sentinel.workflow.adaptive-entry.v1", &entry).unwrap();
        assert_eq!(
            connection
                .execute(
                    "UPDATE workflow_operations SET response=?1,request_digest=?2 WHERE operation_namespace=?3 AND operation_id=?4",
                    params![
                        encode(&entry).unwrap(),
                        digest,
                        namespace(id),
                        format!("{version:020}")
                    ],
                )
                .unwrap(),
            1
        );
    }
}

type DurableRows = Vec<(String, String, Vec<Vec<Value>>)>;

fn durable_rows(store: &WorkflowStore) -> DurableRows {
    let connection = store.lock().unwrap();
    let mut statement = connection
        .prepare("SELECT name,sql FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap();
    let tables = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    tables
        .into_iter()
        .map(|(name, schema)| {
            let quoted = name.replace('"', "\"\"");
            let mut statement = connection
                .prepare(&format!("SELECT * FROM \"{quoted}\" ORDER BY rowid"))
                .unwrap();
            let columns = statement.column_count();
            let rows: Vec<Vec<Value>> = statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|column| row.get::<_, Value>(column))
                        .collect()
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            (name, schema, rows)
        })
        .collect()
}

fn changes(store: &WorkflowStore) -> i64 {
    store
        .lock()
        .unwrap()
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap()
}

fn sorted(mut sessions: Vec<AdaptiveSessionV1>) -> Vec<AdaptiveSessionV1> {
    sessions.sort_by_key(|session| session.grant.session_id);
    sessions
}

fn assert_inventory_read_only(fixture: &Fixture, expected: &[AdaptiveSessionV1]) {
    let before = durable_rows(&fixture.store);
    let expected = sorted(expected.to_vec());
    for store in [&fixture.store, &fixture.reopen()] {
        assert_eq!(durable_rows(store), before);
        let before_changes = changes(store);
        for _ in 0..3 {
            assert_eq!(
                sorted(store.adaptive_sessions_for_health().unwrap()),
                expected
            );
            assert_eq!(durable_rows(store), before);
            assert_eq!(changes(store), before_changes);
        }
    }
}

fn assert_rejected_read_only(fixture: &Fixture) {
    let before = durable_rows(&fixture.store);
    for store in [&fixture.store, &fixture.reopen()] {
        assert_eq!(durable_rows(store), before);
        let before_changes = changes(store);
        for _ in 0..3 {
            assert_eq!(
                store.adaptive_sessions_for_health().unwrap_err().code,
                WorkflowErrorCode::CorruptStore
            );
            assert_eq!(durable_rows(store), before);
            assert_eq!(changes(store), before_changes);
        }
    }
}

#[test]
fn health_inventory_empty_store_is_read_only_across_reopen() {
    assert_inventory_read_only(&Fixture::new(), &[]);
}

#[test]
fn adaptive_cached_reads_reuse_exact_authorized_head_and_digest_proof() {
    let fixture = Fixture::new();
    let session = fixture.unknown(&grant());
    let id = session.grant.session_id;
    let authority = &session.grant.authority;
    let before = durable_rows(&fixture.store);
    let (_, first) = crate::domain_store::validation_scope::with_completed_validations(|| {
        assert_eq!(fixture.store.adaptive_session(id, authority).unwrap(), Some(session.clone()));
    });
    assert!(first.iter().any(|scope| scope.get("adaptive-journal").is_some_and(|count| *count > 0)));
    let (_, repeated) = crate::domain_store::validation_scope::with_completed_validations(|| {
        assert_eq!(fixture.store.adaptive_session(id, authority).unwrap(), Some(session.clone()));
        assert!(fixture.store.adaptive_session_head_digest(id, authority).unwrap().is_some());
    });
    assert!(repeated.is_empty(), "exact unchanged input should reuse the typed proof");
    let (_, selected) = crate::domain_store::validation_scope::with_completed_validations(|| {
        assert_eq!(fixture.store.adaptive_session_for_authority(authority).unwrap(), Some(session.clone()));
    });
    assert!(!selected.is_empty());
    let (_, repeated) = crate::domain_store::validation_scope::with_completed_validations(|| {
        assert_eq!(fixture.store.adaptive_session_for_authority(authority).unwrap(), Some(session.clone()));
    });
    assert!(repeated.is_empty());
    assert_eq!(durable_rows(&fixture.store), before);
    assert!(fixture.store.lock().unwrap().is_autocommit());
}

#[test]
fn adaptive_cached_reads_recheck_authority_and_absent_session_keys() {
    let fixture = Fixture::new();
    let session = fixture.begin(&grant());
    let id = session.grant.session_id;
    let authority = &session.grant.authority;
    assert!(fixture.store.adaptive_session(Uuid::from_u128(888), authority).unwrap().is_none());
    for _ in 0..2 {
        assert_eq!(fixture.store.adaptive_session(id, authority).unwrap(), Some(session.clone()));
        let mut drifted = authority.clone();
        drifted.profile_digest = "a".repeat(64);
        assert_eq!(fixture.store.adaptive_session(id, &drifted).unwrap_err().code, WorkflowErrorCode::AuthorityConflict);
        assert!(fixture.store.adaptive_session(Uuid::from_u128(888), authority).unwrap().is_none());
    }
}

#[test]
fn adaptive_cached_reads_invalidate_after_append_and_pending_checks_remain_fresh() {
    let fixture = Fixture::new();
    let source = fixture.begin(&grant());
    let id = source.grant.session_id;
    let authority = &source.grant.authority;
    assert_eq!(fixture.store.adaptive_session(id, authority).unwrap(), Some(source.clone()));
    let old_digest = fixture.store.adaptive_session_head_digest(id, authority).unwrap().unwrap();
    let pending = fixture.advance(&source, AdaptiveTransitionV1::ClaimModel {
        effect: effect(102), previous_observation_digest: None,
    });
    assert_eq!(fixture.store.adaptive_session(id, authority).unwrap(), Some(pending.clone()));
    let (fresh, digest) = fixture.store.adaptive_pending_model_head_evidence(id, pending.version, &effect(102), authority).unwrap().unwrap();
    assert_eq!(fresh, pending);
    assert_ne!(digest, old_digest);
    assert_eq!(fixture.store.adaptive_pending_model_head_evidence(id, source.version, &effect(102), authority).unwrap_err().code, WorkflowErrorCode::VersionConflict);
    assert_eq!(fixture.store.adaptive_pending_model_head_evidence(id, pending.version, &effect(103), authority).unwrap_err().code, WorkflowErrorCode::AuthorityConflict);
}

#[test]
fn adaptive_cached_reads_reject_resealed_journal_corruption_after_warm_read() {
    let fixture = Fixture::new();
    let session = fixture.unknown(&grant());
    let authority = &session.grant.authority;
    assert_eq!(fixture.store.adaptive_session(session.grant.session_id, authority).unwrap(), Some(session.clone()));
    assert_eq!(fixture.store.adaptive_session_for_authority(authority).unwrap(), Some(session.clone()));
    fixture.rewrite_entry(session.grant.session_id, session.version, |entry| {
        entry.previous_digest = Some("f".repeat(64));
    });
    let before = durable_rows(&fixture.store);
    for store in [&fixture.store, &fixture.reopen()] {
        assert_eq!(store.adaptive_session(session.grant.session_id, authority).unwrap_err().code, WorkflowErrorCode::CorruptStore);
        assert_eq!(store.adaptive_session_head_digest(session.grant.session_id, authority).unwrap_err().code, WorkflowErrorCode::CorruptStore);
        assert_eq!(store.adaptive_session_for_authority(authority).unwrap_err().code, WorkflowErrorCode::CorruptStore);
        assert_eq!(durable_rows(store), before);
    }
}

#[test]
fn adaptive_cached_reads_recheck_external_writes_without_local_change_counter() {
    let fixture = Fixture::new();
    let session = fixture.begin(&grant());
    let id = session.grant.session_id;
    let authority = &session.grant.authority;
    assert_eq!(fixture.store.adaptive_session(id, authority).unwrap(), Some(session.clone()));
    assert_eq!(fixture.store.adaptive_session_for_authority(authority).unwrap(), Some(session.clone()));
    let local_changes: i64 = fixture.store.lock().unwrap().query_row("SELECT total_changes()", [], |row| row.get(0)).unwrap();
    let external = rusqlite::Connection::open(fixture.root.path().join("health.sqlite")).unwrap();
    external.execute("UPDATE workflow_adaptive_heads SET version=version+1", []).unwrap();
    assert_eq!(fixture.store.lock().unwrap().query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0)).unwrap(), local_changes);
    assert_eq!(fixture.store.adaptive_session(id, authority).unwrap_err().code, WorkflowErrorCode::CorruptStore);
    assert_eq!(fixture.store.adaptive_session_for_authority(authority).unwrap_err().code, WorkflowErrorCode::CorruptStore);
}

#[test]
fn adaptive_cached_reads_do_not_hide_new_same_assignment_drifted_heads() {
    let fixture = Fixture::new();
    let session = fixture.begin(&grant());
    let authority = &session.grant.authority;
    assert_eq!(fixture.store.adaptive_session_for_authority(authority).unwrap(), Some(session.clone()));
    let mut drifted = grant();
    drifted.session_id = Uuid::from_u128(303);
    drifted.provider_allowance_id = "new-profile-allowance".into();
    drifted.authority.profile_digest = "a".repeat(64);
    drifted.authority.profile_generation += 1;
    fixture.begin(&drifted);
    assert_eq!(fixture.store.adaptive_session_for_authority(authority).unwrap_err().code, WorkflowErrorCode::AuthorityConflict);
}

#[test]
fn adaptive_cached_reads_preserve_outer_transaction_and_rollback() {
    let fixture = Fixture::new();
    let session = fixture.begin(&grant());
    let id = session.grant.session_id;
    let authority = &session.grant.authority;
    assert_eq!(fixture.store.adaptive_session(id, authority).unwrap(), Some(session.clone()));
    for (begin, rollback) in [("BEGIN", "ROLLBACK"), ("SAVEPOINT caller", "ROLLBACK TO caller; RELEASE caller")] {
        {
            let connection = fixture.store.lock().unwrap();
            connection.execute_batch(begin).unwrap();
            connection.execute("UPDATE workflow_adaptive_heads SET version=version+1", []).unwrap();
        }
        assert_eq!(fixture.store.adaptive_session(id, authority).unwrap_err().code, WorkflowErrorCode::CorruptStore);
        assert_eq!(fixture.store.adaptive_session_for_authority(authority).unwrap_err().code, WorkflowErrorCode::CorruptStore);
        {
            let connection = fixture.store.lock().unwrap();
            assert!(!connection.is_autocommit());
            connection.execute_batch(rollback).unwrap();
        }
        assert_eq!(fixture.store.adaptive_session(id, authority).unwrap(), Some(session.clone()));
        assert_eq!(fixture.store.adaptive_session_for_authority(authority).unwrap(), Some(session.clone()));
    }
}

#[test]
fn health_inventory_keeps_old_and_new_assignment_heads_without_inheriting_authority() {
    let fixture = Fixture::new();
    let historical = fixture.continued();
    let mut current = historical.grant.authority.clone();
    current.assignment_version += 1;
    current.assignment_digest = "a".repeat(64);
    current.profile_generation += 1;
    current.profile_digest = "b".repeat(64);
    assert!(fixture
        .store
        .adaptive_session_for_authority(&current)
        .unwrap()
        .is_none());

    let mut fresh = grant();
    fresh.session_id = Uuid::from_u128(303);
    fresh.provider_allowance_id = "new-assignment-allowance".into();
    fresh.authority = current.clone();
    let new_head = fixture.begin(&fresh);
    assert_inventory_read_only(&fixture, &[historical.clone(), new_head.clone()]);

    let before = durable_rows(&fixture.store);
    for store in [&fixture.store, &fixture.reopen()] {
        assert_eq!(
            store.adaptive_session_for_authority(&current).unwrap(),
            Some(new_head.clone())
        );
        assert_eq!((new_head.model_calls, new_head.tool_calls), (0, 0));
        assert!(new_head.continuation.is_none());
        assert_eq!(
            store
                .adaptive_session(historical.grant.session_id, &current)
                .unwrap_err()
                .code,
            WorkflowErrorCode::AuthorityConflict
        );
        assert_eq!(
            store
                .advance_adaptive_session(
                    historical.grant.session_id,
                    historical.version,
                    Uuid::from_u128(404),
                    &AdaptiveTransitionV1::ClaimModel {
                        effect: effect(405),
                        previous_observation_digest: None,
                    },
                    &current,
                    historical.updated_at_ms + 1,
                )
                .unwrap_err()
                .code,
            WorkflowErrorCode::AuthorityConflict
        );
        let mut later = current.clone();
        later.assignment_version += 1;
        later.assignment_digest = "c".repeat(64);
        assert!(store
            .adaptive_session_for_authority(&later)
            .unwrap()
            .is_none());
        assert_eq!(durable_rows(store), before);
    }
}

#[test]
fn health_inventory_exposes_same_assignment_profile_drift_but_exact_execution_rejects_it() {
    let fixture = Fixture::new();
    let historical = fixture.continued();
    let mut fresh = grant();
    fresh.session_id = Uuid::from_u128(303);
    fresh.provider_allowance_id = "new-profile-allowance".into();
    fresh.authority.profile_generation += 1;
    fresh.authority.profile_digest = "a".repeat(64);
    let new_head = fixture.begin(&fresh);
    assert_inventory_read_only(&fixture, &[historical.clone(), new_head.clone()]);
    let before = durable_rows(&fixture.store);
    for store in [&fixture.store, &fixture.reopen()] {
        for authority in [&historical.grant.authority, &new_head.grant.authority] {
            assert_eq!(
                store
                    .adaptive_session_for_authority(authority)
                    .unwrap_err()
                    .code,
                WorkflowErrorCode::AuthorityConflict
            );
        }
        assert_eq!(durable_rows(store), before);
    }
}

#[test]
fn health_inventory_accepts_cancelled_rollover_without_resurrecting_old_head() {
    let fixture = Fixture::new();
    let (cancelled, current) = fixture.rollover();
    assert_inventory_read_only(&fixture, std::slice::from_ref(&current));
    assert_eq!(
        fixture
            .store
            .adaptive_session_for_authority(&current.grant.authority)
            .unwrap(),
        Some(current)
    );
    assert_eq!(
        fixture
            .store
            .adaptive_session(cancelled.grant.session_id, &cancelled.grant.authority)
            .unwrap_err()
            .code,
        WorkflowErrorCode::CorruptStore
    );
}

#[test]
fn health_inventory_replays_cancelled_non_head_journals_too() {
    let fixture = Fixture::new();
    let (cancelled, _) = fixture.rollover();
    fixture.rewrite_entry(cancelled.grant.session_id, cancelled.version, |entry| {
        entry.previous_digest = Some("f".repeat(64));
    });
    assert_rejected_read_only(&fixture);
}

#[test]
fn health_inventory_rejects_removed_unresolved_heads_instead_of_hiding_them() {
    for state in 0..4 {
        let fixture = Fixture::new();
        let source = match state {
            0 => fixture.begin(&grant()),
            1 => fixture.advance(
                &fixture.begin(&grant()),
                AdaptiveTransitionV1::ClaimModel {
                    effect: effect(102),
                    previous_observation_digest: None,
                },
            ),
            2 => fixture.unknown(&grant()),
            3 => fixture.continued(),
            _ => unreachable!(),
        };
        assert_eq!(
            fixture
                .store
                .lock()
                .unwrap()
                .execute(
                    "DELETE FROM workflow_adaptive_heads WHERE session_id=?1",
                    params![source.grant.session_id.to_string()],
                )
                .unwrap(),
            1
        );
        assert_rejected_read_only(&fixture);
    }
}

#[test]
fn health_inventory_rejects_head_without_journal() {
    let fixture = Fixture::new();
    let source = fixture.begin(&grant());
    assert_eq!(
        fixture
            .store
            .lock()
            .unwrap()
            .execute(
                "DELETE FROM workflow_operations WHERE operation_namespace=?1",
                params![namespace(source.grant.session_id)],
            )
            .unwrap(),
        1
    );
    assert_rejected_read_only(&fixture);
}

#[test]
fn health_inventory_rejects_corrupt_head_identity_digest_version_and_time() {
    for sql in [
        "UPDATE workflow_adaptive_heads SET tenant_id='foreign-tenant'",
        "UPDATE workflow_adaptive_heads SET project_id='foreign-project'",
        "UPDATE workflow_adaptive_heads SET work_item_id='foreign-work'",
        "UPDATE workflow_adaptive_heads SET agent_id=8",
        "UPDATE workflow_adaptive_heads SET agent_id=-1",
        "UPDATE workflow_adaptive_heads SET authority_digest='invalid'",
        "UPDATE workflow_adaptive_heads SET authority_digest=printf('%064d',0)",
        "UPDATE workflow_adaptive_heads SET session_id='invalid'",
        "UPDATE workflow_adaptive_heads SET session_id='00000000-0000-0000-0000-000000000000'",
        "UPDATE workflow_adaptive_heads SET session_id=replace(session_id,'-','')",
        "UPDATE workflow_adaptive_heads SET version=version+1",
        "UPDATE workflow_adaptive_heads SET version=0",
        "UPDATE workflow_adaptive_heads SET version=-1",
        "UPDATE workflow_adaptive_heads SET updated_at_ms=updated_at_ms+1",
        "UPDATE workflow_adaptive_heads SET updated_at_ms=-1",
    ] {
        let fixture = Fixture::new();
        fixture.continued();
        assert_eq!(
            fixture.store.lock().unwrap().execute(sql, []).unwrap(),
            1,
            "{sql}"
        );
        assert_rejected_read_only(&fixture);
    }
}

#[test]
fn health_inventory_rejects_cross_scope_head_alias() {
    let fixture = Fixture::new();
    let source = fixture.begin(&grant());
    let mut foreign = grant();
    foreign.session_id = Uuid::from_u128(303);
    foreign.authority.work_item_id = crate::WorkItemId::parse("foreign-work").unwrap();
    let foreign = fixture.begin(&foreign);
    let connection = fixture.store.lock().unwrap();
    connection
        .execute(
            "DELETE FROM workflow_adaptive_heads WHERE session_id=?1",
            params![foreign.grant.session_id.to_string()],
        )
        .unwrap();
    assert_eq!(
        connection
            .execute(
                "UPDATE workflow_adaptive_heads SET session_id=?1,authority_digest=?2,version=?3,updated_at_ms=?4 WHERE session_id=?5",
                params![
                    foreign.grant.session_id.to_string(),
                    foreign.grant.authority.canonical_digest().unwrap(),
                    sql_u64(foreign.version).unwrap(),
                    sql_u64(foreign.updated_at_ms).unwrap(),
                    source.grant.session_id.to_string()
                ],
            )
            .unwrap(),
        1
    );
    drop(connection);
    assert_rejected_read_only(&fixture);
}

#[test]
fn health_inventory_rejects_journal_gap_digest_key_payload_and_time_corruption() {
    for sql in [
        "DELETE FROM workflow_operations WHERE operation_namespace=?1 AND operation_id='00000000000000000001'",
        "DELETE FROM workflow_operations WHERE operation_namespace=?1 AND operation_id='00000000000000000002'",
        "UPDATE workflow_operations SET request_digest='invalid' WHERE operation_namespace=?1 AND operation_id='00000000000000000002'",
        "UPDATE workflow_operations SET created_at_ms=created_at_ms+1 WHERE operation_namespace=?1 AND operation_id='00000000000000000002'",
        "UPDATE workflow_operations SET created_at_ms=-1 WHERE operation_namespace=?1 AND operation_id='00000000000000000002'",
        "UPDATE workflow_operations SET response=x'7b7d' WHERE operation_namespace=?1 AND operation_id='00000000000000000002'",
        "UPDATE workflow_operations SET operation_id='2' WHERE operation_namespace=?1 AND operation_id='00000000000000000002'",
    ] {
        let fixture = Fixture::new();
        let source = fixture.unknown(&grant());
        assert_eq!(
            fixture
                .store
                .lock()
                .unwrap()
                .execute(sql, params![namespace(source.grant.session_id)])
                .unwrap(),
            1,
            "{sql}"
        );
        assert_rejected_read_only(&fixture);
    }
}

#[test]
fn health_inventory_rejects_forged_digest_valid_journal_replay() {
    for mutation in 0..5 {
        let fixture = Fixture::new();
        let source = fixture.unknown(&grant());
        fixture.rewrite_entry(
            source.grant.session_id,
            source.version,
            |entry| match mutation {
                0 => entry.session.model_calls += 1,
                1 => entry.previous_digest = Some("f".repeat(64)),
                2 => entry.command = None,
                3 => entry.session.grant.authority.profile_generation += 1,
                4 => entry.session.grant.session_id = Uuid::from_u128(303),
                _ => unreachable!(),
            },
        );
        assert_rejected_read_only(&fixture);
    }
}

#[test]
fn health_inventory_rejects_malformed_or_noncanonical_adaptive_namespaces() {
    let id = Uuid::from_u128(0xabc);
    for malformed in [
        "adaptive-session-v1:".to_string(),
        "adaptive-session-v1:not-a-uuid".to_string(),
        format!("adaptive-session-v1:{}", id.simple()),
        format!("adaptive-session-v1:{}", id.to_string().to_uppercase()),
        format!("adaptive-session-v1:{{{id}}}"),
        format!("adaptive-session-v1:urn:uuid:{id}"),
        format!("{}:", namespace(id)),
        format!("{}:foreign", namespace(id)),
        format!("{}:operations:foreign", namespace(id)),
        format!("{}:rejected-model-dispositions:foreign", namespace(id)),
    ] {
        let fixture = Fixture::new();
        let mut root = grant();
        root.session_id = id;
        fixture.begin(&root);
        assert_eq!(
            fixture
                .store
                .lock()
                .unwrap()
                .execute(
                    "INSERT INTO workflow_operations SELECT ?1,operation_id,request_digest,response,created_at_ms FROM workflow_operations WHERE operation_namespace=?2",
                    params![malformed, namespace(id)],
                )
                .unwrap(),
            1
        );
        assert_rejected_read_only(&fixture);
    }
}

#[test]
fn health_inventory_rejects_orphan_companion_namespaces() {
    for suffix in ["operations", "rejected-model-dispositions"] {
        let fixture = Fixture::new();
        let source = fixture.unknown(&grant());
        let ns = namespace(source.grant.session_id);
        let connection = fixture.store.lock().unwrap();
        if suffix != "operations" {
            assert_eq!(
                connection.execute(
                    "INSERT INTO workflow_operations SELECT ?1,operation_id,request_digest,response,created_at_ms FROM workflow_operations WHERE operation_namespace=?2",
                    params![format!("{ns}:{suffix}"), format!("{ns}:operations")],
                ).unwrap(),
                2
            );
            connection
                .execute(
                    "DELETE FROM workflow_operations WHERE operation_namespace=?1",
                    params![format!("{ns}:operations")],
                )
                .unwrap();
        }
        assert_eq!(
            connection
                .execute(
                    "DELETE FROM workflow_operations WHERE operation_namespace=?1",
                    params![ns],
                )
                .unwrap(),
            3
        );
        assert_eq!(
            connection
                .execute("DELETE FROM workflow_adaptive_heads", [])
                .unwrap(),
            1
        );
        drop(connection);
        assert_rejected_read_only(&fixture);
    }
}

#[test]
fn health_inventory_rejects_corrupt_companion_operations_with_valid_head_and_journal() {
    for sql in [
        "UPDATE workflow_operations SET request_digest='invalid' WHERE operation_namespace=?1 AND operation_id=?2",
        "UPDATE workflow_operations SET response=x'7b7d' WHERE operation_namespace=?1 AND operation_id=?2",
        "UPDATE workflow_operations SET created_at_ms=created_at_ms+1 WHERE operation_namespace=?1 AND operation_id=?2",
        "UPDATE workflow_operations SET operation_id='00000000-0000-0000-0000-000000000000' WHERE operation_namespace=?1 AND operation_id=?2",
        "INSERT INTO workflow_operations SELECT operation_namespace,'00000000-0000-0000-0000-00000000ffff',request_digest,response,created_at_ms FROM workflow_operations WHERE operation_namespace=?1 AND operation_id=?2",
    ] {
        let fixture = Fixture::new();
        let source = fixture.unknown(&grant());
        assert_eq!(
            fixture
                .store
                .lock()
                .unwrap()
                .execute(
                    sql,
                    params![
                        format!("{}:operations", namespace(source.grant.session_id)),
                        Uuid::from_u128(10_001).to_string()
                    ],
                )
                .unwrap(),
            1,
            "{sql}"
        );
        assert_rejected_read_only(&fixture);
    }
}

#[test]
fn health_inventory_rejects_missing_command_companion_for_non_rollover_transition() {
    let fixture = Fixture::new();
    let source = fixture.unknown(&grant());
    assert_eq!(
        fixture
            .store
            .lock()
            .unwrap()
            .execute(
                "DELETE FROM workflow_operations WHERE operation_namespace=?1 AND operation_id=?2",
                params![
                    format!("{}:operations", namespace(source.grant.session_id)),
                    Uuid::from_u128(10_001).to_string()
                ],
            )
            .unwrap(),
        1
    );
    assert_rejected_read_only(&fixture);
}

#[test]
fn health_inventory_revalidates_journals_after_external_commit() {
    let fixture = Fixture::new();
    let source = fixture.unknown(&grant());
    assert_inventory_read_only(&fixture, std::slice::from_ref(&source));
    let writer = fixture.reopen();
    assert_eq!(
        writer
            .lock()
            .unwrap()
            .execute(
                "UPDATE workflow_operations SET request_digest='invalid' WHERE operation_namespace=?1 AND operation_id=?2",
                params![namespace(source.grant.session_id), "00000000000000000002"],
            )
            .unwrap(),
        1
    );
    drop(writer);
    assert_rejected_read_only(&fixture);
}

fn lineage_head(fixture: &Fixture, index: usize) -> AdaptiveSessionV1 {
    let mut root = grant();
    root.session_id = Uuid::from_u128(1_000 + index as u128);
    root.authority.assignment_version += index as u64;
    root.provider_allowance_id = format!("lineage-allowance-{index}");
    fixture.begin(&root)
}

#[test]
fn health_inventory_accepts_65_global_independent_work_item_lineages() {
    let fixture = Fixture::new();
    let expected: Vec<_> = (0..65)
        .map(|index| {
            let mut root = grant();
            root.session_id = Uuid::from_u128(3_000 + index as u128);
            root.provider_allowance_id = format!("independent-allowance-{index}");
            root.authority.work_item_id =
                crate::WorkItemId::parse(format!("independent-work-{index}")).unwrap();
            fixture.begin(&root)
        })
        .collect();
    let before = durable_rows(&fixture.store);
    for store in [&fixture.store, &fixture.reopen()] {
        let before_changes = changes(store);
        assert_eq!(store.adaptive_sessions_for_health().unwrap().len(), 65);
        assert_eq!(durable_rows(store), before);
        assert_eq!(changes(store), before_changes);
    }
    assert_inventory_read_only(&fixture, &expected);
}

#[test]
fn health_inventory_accepts_exactly_64_assignment_heads_in_one_lineage() {
    let fixture = Fixture::new();
    let expected: Vec<_> = (0..MAX_SCOPED_ADAPTIVE_HEADS)
        .map(|index| lineage_head(&fixture, index))
        .collect();
    assert_inventory_read_only(&fixture, &expected);
}

#[test]
fn health_inventory_rejects_65_assignment_heads_in_one_lineage_without_truncation() {
    let fixture = Fixture::new();
    for index in 0..=MAX_SCOPED_ADAPTIVE_HEADS {
        lineage_head(&fixture, index);
    }
    assert_rejected_read_only(&fixture);
}

fn rollover_chain(fixture: &Fixture, length: usize) -> AdaptiveSessionV1 {
    assert!(length > 0);
    let mut root = grant();
    root.session_id = Uuid::from_u128(1_000);
    root.provider_allowance_id = "rollover-allowance-0".into();
    let mut head = fixture.begin(&root);
    for index in 1..length {
        root.session_id = Uuid::from_u128(1_000 + index as u128);
        root.provider_allowance_id = format!("rollover-allowance-{index}");
        root.created_at_ms = head.grant.deadline_ms;
        root.deadline_ms = root.created_at_ms + 1_000;
        head = fixture.begin(&root);
    }
    head
}

#[test]
fn health_inventory_accepts_exactly_64_sessions_in_one_rollover_lineage() {
    let fixture = Fixture::new();
    let head = rollover_chain(&fixture, MAX_SCOPED_ADAPTIVE_HEADS);
    assert_inventory_read_only(&fixture, &[head]);
}

#[test]
fn health_inventory_rejects_65_sessions_in_one_rollover_lineage_without_truncation() {
    let fixture = Fixture::new();
    rollover_chain(&fixture, MAX_SCOPED_ADAPTIVE_HEADS + 1);
    assert_rejected_read_only(&fixture);
}

#[test]
fn health_inventory_includes_each_tenant_project_work_item_agent_scope() {
    for dimension in 0..4 {
        let fixture = Fixture::new();
        let mut expected: Vec<_> = (0..MAX_SCOPED_ADAPTIVE_HEADS)
            .map(|index| lineage_head(&fixture, index))
            .collect();
        let mut foreign = grant();
        foreign.session_id = Uuid::from_u128(2_000);
        match dimension {
            0 => foreign.authority.tenant_id = crate::TenantId::parse("foreign-tenant").unwrap(),
            1 => foreign.authority.project_id = crate::ProjectId::parse("foreign-project").unwrap(),
            2 => foreign.authority.work_item_id = crate::WorkItemId::parse("foreign-work").unwrap(),
            3 => foreign.authority.agent_id = crate::AgentId(8),
            _ => unreachable!(),
        }
        expected.push(fixture.unknown(&foreign));
        assert_eq!(expected.len(), 65);
        assert_inventory_read_only(&fixture, &expected);
    }
}
