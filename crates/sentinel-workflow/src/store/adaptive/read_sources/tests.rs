use super::*;
use crate::adaptive::continuation_tests::{effect, grant, NOW};
use crate::AdaptiveProgressReadKind;

fn fixture() -> (tempfile::TempDir, WorkflowStore, AdaptiveSessionV1) {
    let root = tempfile::tempdir().unwrap();
    let store = WorkflowStore::open(root.path().join("batch.sqlite")).unwrap();
    let grant = grant();
    let session = store
        .begin_adaptive_session(&grant, &grant.authority, NOW)
        .unwrap()
        .1;
    (root, store, session)
}

fn prepared(
    store: &WorkflowStore,
    session: &AdaptiveSessionV1,
) -> AdaptiveModelPreparationReadSnapshot {
    store
        .adaptive_model_preparation_read_snapshot(
            session.grant.session_id,
            session.version,
            working_memory_model_effect_id(session.grant.session_id, session.version),
            &session.grant.authority,
        )
        .unwrap()
        .unwrap()
}

#[test]
fn provider_batch_replays_once_and_preserves_memory_and_feedback_bytes() {
    let (_root, store, session) = fixture();
    WORKING_MEMORY_READS.with(|reads| reads.set((0, 0, 0, 0)));
    let batch = prepared(&store, &session);
    assert_eq!(WORKING_MEMORY_READS.with(|reads| reads.get()), (1, 1, 0, 0));
    assert_eq!(batch.session(), &session);
    assert_eq!(
        batch.project().unwrap_err().code,
        WorkflowErrorCode::NotFound
    );
    assert_eq!(batch.recovery_feedback().unwrap(), None);
    let old = store
        .adaptive_working_memory_source(
            session.grant.session_id,
            session.version,
            working_memory_model_effect_id(session.grant.session_id, session.version),
            &session.grant.authority,
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        encode(batch.working_memory_source().unwrap()).unwrap(),
        encode(&old).unwrap()
    );
    assert_eq!(
        batch.recovery_feedback().unwrap().cloned(),
        store
            .adaptive_recovery_feedback(&session.grant.authority)
            .unwrap()
    );
    assert!(store.lock().unwrap().is_autocommit());
}

#[test]
fn provider_batch_defers_new_memory_errors_without_retaining_failed_view() {
    let (_root, store, session) = fixture();
    let read = || {
        store
            .adaptive_model_preparation_read_snapshot(
                session.grant.session_id,
                session.version,
                Uuid::from_u128(999),
                &session.grant.authority,
            )
            .unwrap()
            .unwrap()
    };
    let batch = read();
    assert_eq!(batch.session(), &session);
    assert_eq!(batch.recovery_feedback().unwrap(), None);
    assert_eq!(
        batch.working_memory_source().unwrap_err().code,
        WorkflowErrorCode::AuthorityConflict
    );
    // Legacy callers can consume session/feedback without consuming the memory error.
    WORKING_MEMORY_READS.with(|reads| reads.set((0, 0, 0, 0)));
    assert_eq!(read().session(), &session);
    assert_eq!(WORKING_MEMORY_READS.with(|reads| reads.get()).0, 1);
}

#[test]
fn provider_batch_preserves_missing_session_before_nil_memory_arguments() {
    let (_root, store, session) = fixture();
    assert!(store
        .adaptive_model_preparation_read_snapshot(
            Uuid::nil(),
            0,
            Uuid::nil(),
            &session.grant.authority,
        )
        .unwrap()
        .is_none());
    let view = store
        .adaptive_model_preparation_read_snapshot(
            session.grant.session_id,
            0,
            Uuid::nil(),
            &session.grant.authority,
        )
        .unwrap()
        .unwrap();
    assert_eq!(view.session(), &session);
    assert_eq!(view.recovery_feedback().unwrap(), None);
    assert_eq!(
        view.working_memory_source().unwrap_err().code,
        WorkflowErrorCode::AuthorityConflict
    );
}

#[test]
fn empty_progress_batch_avoids_unrelated_database_validation() {
    let (_root, store, _session) = fixture();
    store
        .lock()
        .unwrap()
        .execute_batch("DROP TABLE workflow_operations")
        .unwrap();
    assert!(store.adaptive_progress_read_batch(&[]).unwrap().is_empty());
}

#[test]
fn provider_batch_matches_pending_prefix_and_rejects_corrupt_history() {
    let (root, store, initial) = fixture();
    let version = initial.version;
    let mut model = effect(501);
    model.id = working_memory_model_effect_id(initial.grant.session_id, version);
    let pending = store
        .advance_adaptive_session(
            initial.grant.session_id,
            version,
            Uuid::from_u128(502),
            &crate::AdaptiveTransitionV1::ClaimModel {
                effect: model.clone(),
                previous_observation_digest: None,
            },
            &initial.grant.authority,
            NOW + 1,
        )
        .unwrap()
        .1;
    let batch = store
        .adaptive_model_preparation_read_snapshot(
            initial.grant.session_id,
            version,
            model.id,
            &initial.grant.authority,
        )
        .unwrap()
        .unwrap();
    assert_eq!(batch.session(), &pending);
    assert_eq!(batch.working_memory_source().unwrap().head_version, version);
    let external = Connection::open(root.path().join("batch.sqlite")).unwrap();
    external.execute("UPDATE workflow_operations SET request_digest='broken' WHERE operation_namespace=?1 AND operation_id='00000000000000000001'",
        [namespace(initial.grant.session_id)]).unwrap();
    assert_eq!(
        store
            .adaptive_model_preparation_read_snapshot(
                initial.grant.session_id,
                version,
                model.id,
                &initial.grant.authority
            )
            .unwrap_err()
            .code,
        WorkflowErrorCode::CorruptStore
    );
    assert!(store.lock().unwrap().is_autocommit());
}

#[test]
fn progress_batch_matches_exact_reads_and_preserves_per_item_errors() {
    let (_root, store, session) = fixture();
    let mut drifted = session.grant.authority.clone();
    drifted.policy_generation += 1;
    let mut unrelated = session.grant.authority.clone();
    unrelated.work_item_id = crate::WorkItemId::parse("other-work").unwrap();
    let authorities = vec![session.grant.authority.clone(), drifted, unrelated];
    let batch = store.adaptive_progress_read_batch(&authorities).unwrap();
    for (current, result) in authorities.iter().zip(&batch) {
        match (store.adaptive_session_for_authority(current), result) {
            (Ok(session), Ok(actual)) => {
                assert_eq!(actual, &session.map(|value| value.progress_read_facts()))
            }
            (Err(expected), Err(actual)) => assert_eq!(actual, &expected),
            _ => panic!("batch and exact read differ"),
        }
    }
    assert_eq!(
        batch[0].as_ref().unwrap().as_ref().unwrap().kind(),
        AdaptiveProgressReadKind::ReadyForModel
    );
    assert_eq!(
        batch[1].as_ref().unwrap_err().code,
        WorkflowErrorCode::AuthorityConflict
    );
    assert_eq!(batch[2], Ok(None));
}

#[test]
fn progress_batch_rejects_hidden_competing_campaign_and_corrupt_journal() {
    let (_root, store, session) = fixture();
    let mut other = session.grant.clone();
    other.session_id = Uuid::from_u128(777);
    other.authority.policy_generation += 1;
    other.provider_allowance_id = "other-root".into();
    store
        .begin_adaptive_session(&other, &other.authority, NOW)
        .unwrap();
    let result = store
        .adaptive_progress_read_batch(std::slice::from_ref(&session.grant.authority))
        .unwrap();
    assert_eq!(
        result[0].as_ref().unwrap_err().code,
        WorkflowErrorCode::AuthorityConflict
    );
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE workflow_operations SET request_digest='broken' WHERE operation_namespace=?1",
            [namespace(other.session_id)],
        )
        .unwrap();
    let result = store
        .adaptive_progress_read_batch(std::slice::from_ref(&session.grant.authority))
        .unwrap();
    assert_eq!(
        result[0].as_ref().unwrap_err().code,
        WorkflowErrorCode::CorruptStore
    );
}

#[test]
fn progress_batch_is_compact_and_has_independent_source_arenas() {
    let (_root, store, session) = fixture();
    let mut other = session.grant.clone();
    other.session_id = Uuid::from_u128(778);
    other.authority.work_item_id = crate::WorkItemId::parse("other-work").unwrap();
    other.provider_allowance_id = "other-root".into();
    store
        .begin_adaptive_session(&other, &other.authority, NOW)
        .unwrap();
    let (result, scopes) =
        crate::domain_store::validation_scope::with_completed_validations(|| {
            store.adaptive_progress_read_batch(&[session.grant.authority.clone(), other.authority])
        });
    assert!(result.unwrap().iter().all(Result::is_ok));
    assert_eq!(scopes.len(), 2);
    assert!(scopes
        .iter()
        .all(|scope| scope.get("adaptive-journal") == Some(&1)));
    assert!(encode(&session.progress_read_facts()).unwrap().len() < 512);
    assert_eq!(
        store
            .adaptive_progress_read_batch(&vec![session.grant.authority; 128 * 128 + 1])
            .unwrap_err()
            .code,
        WorkflowErrorCode::InvalidInput
    );
}

#[test]
fn progress_batch_warm_read_rechecks_external_head_changes_and_outer_transaction() {
    let (root, store, session) = fixture();
    let read =
        || store.adaptive_progress_read_batch(std::slice::from_ref(&session.grant.authority));
    assert_eq!(read().unwrap()[0], Ok(Some(session.progress_read_facts())));
    let external = Connection::open(root.path().join("batch.sqlite")).unwrap();
    external
        .execute("UPDATE workflow_adaptive_heads SET version=version+1", [])
        .unwrap();
    let result = read().unwrap();
    assert_eq!(
        result[0].as_ref().unwrap_err().code,
        WorkflowErrorCode::CorruptStore
    );
    external
        .execute("UPDATE workflow_adaptive_heads SET version=version-1", [])
        .unwrap();
    store.lock().unwrap().execute_batch("BEGIN").unwrap();
    assert!(read().unwrap()[0].is_ok());
    assert!(!store.lock().unwrap().is_autocommit());
    store.lock().unwrap().execute_batch("ROLLBACK").unwrap();
}

#[test]
fn batched_reads_reject_index_only_damage_without_changed_rows() {
    use std::io::{Seek, SeekFrom, Write};
    for provider in [false, true] {
        let (root, store, session) = fixture();
        store.lock().unwrap().execute_batch(
            "CREATE INDEX batch_payload_index ON workflow_operations(created_at_ms); PRAGMA wal_checkpoint(TRUNCATE)",
        ).unwrap();
        let read = || -> Result<(), WorkflowError> {
            if provider {
                store
                    .adaptive_model_preparation_read_snapshot(
                        session.grant.session_id,
                        session.version,
                        working_memory_model_effect_id(session.grant.session_id, session.version),
                        &session.grant.authority,
                    )
                    .map(|_| ())
            } else {
                store
                    .adaptive_progress_read_batch(std::slice::from_ref(&session.grant.authority))
                    .map(|_| ())
            }
        };
        read().unwrap();
        let connection = store.lock().unwrap();
        let size: u32 = connection
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .unwrap();
        let page: u32 = connection
            .query_row(
                "SELECT rootpage FROM sqlite_schema WHERE name='batch_payload_index'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        connection.execute_batch("PRAGMA shrink_memory").unwrap();
        drop(connection);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(root.path().join("batch.sqlite"))
            .unwrap();
        file.seek(SeekFrom::Start(u64::from(page - 1) * u64::from(size)))
            .unwrap();
        file.write_all(&vec![0; size as usize]).unwrap();
        file.sync_all().unwrap();
        assert_eq!(read().unwrap_err().code, WorkflowErrorCode::CorruptStore);
        assert!(store.lock().unwrap().is_autocommit());
    }
}

#[test]
fn progress_batch_preserves_scoped_head_cap() {
    let (_root, store, session) = fixture();
    for index in 0..64 {
        let mut other = session.grant.clone();
        other.session_id = Uuid::from_u128(1_000 + index);
        other.provider_allowance_id = format!("other-root-{index}");
        other.authority.policy_generation += index as u64 + 1;
        store
            .begin_adaptive_session(&other, &other.authority, NOW)
            .unwrap();
    }
    let batch = store
        .adaptive_progress_read_batch(std::slice::from_ref(&session.grant.authority))
        .unwrap();
    assert_eq!(
        batch[0].as_ref().unwrap_err().code,
        WorkflowErrorCode::AuthorityConflict
    );
}
