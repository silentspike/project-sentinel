#![cfg(test)]

use super::journal_entry_reuse_tests::{journal, persist};
use super::*;
use crate::domain_store::validation_scope::{self, ValidationFailureReason};
use std::cell::Cell;
use std::sync::Arc;

const MIB: usize = 1024 * 1024;
const LIMIT: usize = 64 * MIB;

fn total_changes(connection: &Connection) -> i64 {
    connection
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn journal_single_decoded_40mib_owners_preserve_bytes_warm_identity_and_full_replay() {
    let root = tempfile::tempdir().unwrap();
    let store = WorkflowStore::open(root.path().join("journal-owners.sqlite")).unwrap();
    let id = Uuid::from_u128(101);
    let entries = journal(id, 32, 2700);
    persist(&store, &entries);
    let raw_total: usize = entries
        .iter()
        .map(|entry| encode(entry).unwrap().len())
        .sum();
    assert!((35 * MIB..45 * MIB).contains(&raw_total));
    let connection = store.lock().unwrap();
    let changes = total_changes(&connection);
    for exact_boundary in [false, true] {
        let mut weak = None;
        let total = Cell::new(0);
        WORKING_MEMORY_READS.with(|reads| reads.set((0, 0, 0, 0)));
        let (result, failures) = validation_scope::with_validation_failures(|| {
            validation_scope::with_scope(&connection, || {
                let mut owners: Vec<JournalEntry> = Vec::with_capacity(entries.len());
                let mut old_additive_total = 0;
                for entry in &entries {
                    let version = entry.session.version;
                    let owner = journal_entry(&connection, id, version)?.unwrap();
                    assert_eq!(encode(&owner.0.entry)?, encode(entry)?);
                    assert_eq!(
                        owner.0.digest,
                        canonical_sha256("sentinel.workflow.adaptive-entry.v1", entry)?
                    );
                    assert!(owners.iter().all(|prior| !Arc::ptr_eq(&prior.0, &owner.0)));
                    let raw_bytes: i64 = connection.query_row(
                        "SELECT length(response) FROM workflow_operations WHERE operation_namespace=?1 AND operation_id=?2",
                        params![namespace(id), format!("{version:020}")], |row| row.get(0),
                    )?;
                    let raw_bytes = usize::try_from(raw_bytes).unwrap();
                    let result_bytes =
                        crate::digest::serialized_json_size(&Some(owner.clone())).unwrap();
                    let key_bytes = encode(&(id, version))?.len() + 256;
                    total.set(total.get() + key_bytes + raw_bytes.max(result_bytes));
                    old_additive_total += key_bytes + raw_bytes + result_bytes;
                    weak = Some(Arc::downgrade(&owner.0));
                    owners.push(owner);
                }
                assert!((35 * MIB..45 * MIB).contains(&total.get()));
                assert!(
                    old_additive_total > LIMIT,
                    "the fixture must distinguish the old double charge"
                );
                if exact_boundary {
                    validation_scope::charge_bytes(&connection, LIMIT - total.get())?;
                }
                for _ in 0..2 {
                    for (entry, owner) in entries.iter().zip(&owners) {
                        let warm = journal_entry(&connection, id, entry.session.version)?.unwrap();
                        assert!(Arc::ptr_eq(&owner.0, &warm.0));
                    }
                }
                assert_eq!(
                    validation_scope::validations("adaptive-journal-entry"),
                    entries.len()
                );
                assert_eq!(
                    WORKING_MEMORY_READS.with(|reads| reads.get().1),
                    entries.len()
                );
                if exact_boundary {
                    validation_scope::charge_bytes(&connection, 1)?;
                }
                Ok(())
            })
        });
        if exact_boundary {
            assert!(result.is_err());
            assert_eq!(failures.len(), 1);
            assert_eq!(failures[0].reason, ValidationFailureReason::BytesLimit);
            assert_eq!(failures[0].bytes, LIMIT);
            assert_eq!(failures[0].attempted_bytes, Some(1));
        } else {
            result.unwrap();
            assert!(failures.is_empty());
        }
        assert!(
            weak.unwrap().upgrade().is_none(),
            "owners cannot escape their validation scope"
        );
        assert!(connection.is_autocommit());
    }
    let (result, failures) = validation_scope::with_validation_failures(|| {
        validation_scope::with_scope(&connection, || {
            let loaded = load_with_feedback(&connection, id)?.unwrap();
            assert_eq!(loaded.0, entries.last().unwrap().session);
            assert_eq!(loaded.2, entries[0].recovery_feedback);
            assert_eq!(
                validation_scope::validations("adaptive-journal-entry"),
                entries.len()
            );
            assert_eq!(
                validation_scope::validations("adaptive-journal-step"),
                entries.len()
            );
            Ok(())
        })
    });
    result.unwrap();
    assert!(
        failures.is_empty(),
        "ownership accounting must preserve complete journal validation"
    );
    assert_eq!(total_changes(&connection), changes);
    assert!(connection.is_autocommit());
}

#[test]
fn journal_single_decoded_raw_whitespace_and_escapes_keep_full_input_charge() {
    let root = tempfile::tempdir().unwrap();
    let store = WorkflowStore::open(root.path().join("journal-whitespace.sqlite")).unwrap();
    let id = Uuid::from_u128(101);
    let entries = journal(id, 0, 0);
    persist(&store, &entries);
    let connection = store.lock().unwrap();
    let canonical = encode(&entries[0]).unwrap();
    let escaped = String::from_utf8(canonical.clone()).unwrap().replacen(
        "\"session\"",
        "\"\\u0073ession\"",
        1,
    );
    assert_ne!(escaped.as_bytes(), canonical.as_slice());
    let mut raw = b" \n\t".repeat(4096);
    raw.extend_from_slice(escaped.as_bytes());
    assert_eq!(decode::<Entry>(&raw).unwrap(), entries[0]);
    connection.execute(
        "UPDATE workflow_operations SET response=?1 WHERE operation_namespace=?2 AND operation_id=?3",
        params![raw, namespace(id), format!("{:020}", 1)],
    ).unwrap();
    let key_bytes = encode(&(id, 1_u64)).unwrap().len() + 256;
    let changes = total_changes(&connection);
    let (result, failures) = validation_scope::with_validation_failures(|| {
        validation_scope::with_scope(&connection, || {
            validation_scope::charge_bytes(&connection, LIMIT - key_bytes - raw.len())?;
            let owner = journal_entry(&connection, id, 1)?.unwrap();
            assert_eq!(encode(&owner.0.entry)?, canonical);
            assert!(crate::digest::serialized_json_size(&Some(owner.clone())).unwrap() < raw.len());
            assert!(Arc::ptr_eq(
                &owner.0,
                &journal_entry(&connection, id, 1)?.unwrap().0
            ));
            validation_scope::charge_bytes(&connection, 1)
        })
    });
    assert!(result.is_err());
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].reason, ValidationFailureReason::BytesLimit);
    assert_eq!(failures[0].bytes, LIMIT);
    assert_eq!(failures[0].attempted_bytes, Some(1));
    assert_eq!(total_changes(&connection), changes);
    assert!(connection.is_autocommit());
}

#[test]
fn journal_single_decoded_real_oversize_blob_rejects_before_decode() {
    let root = tempfile::tempdir().unwrap();
    let store = WorkflowStore::open(root.path().join("journal-oversize.sqlite")).unwrap();
    let id = Uuid::from_u128(101);
    persist(&store, &journal(id, 0, 0));
    let connection = store.lock().unwrap();
    connection
        .execute(
            "UPDATE workflow_operations SET response=zeroblob(?1) WHERE operation_namespace=?2",
            params![(LIMIT + 1) as i64, namespace(id)],
        )
        .unwrap();
    WORKING_MEMORY_READS.with(|reads| reads.set((0, 0, 0, 0)));
    let (result, failures) =
        validation_scope::with_validation_failures(|| journal_entry(&connection, id, 1));
    assert!(result.is_err());
    assert_eq!(WORKING_MEMORY_READS.with(|reads| reads.get().1), 0);
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].reason, ValidationFailureReason::BytesLimit);
    assert_eq!(
        failures[0].charge_phase,
        Some(validation_scope::ValidationChargePhase::Body)
    );
    assert_eq!(failures[0].attempted_bytes, Some(LIMIT + 1));
    assert!(connection.is_autocommit());
}

#[test]
fn journal_single_decoded_missing_and_corrupt_rows_still_fail_full_cold_and_warm_history() {
    for damage in [
        "non-blob",
        "malformed",
        "digest",
        "session",
        "version",
        "clock",
        "chain",
        "gap",
    ] {
        let root = tempfile::tempdir().unwrap();
        let store = WorkflowStore::open(root.path().join("journal-corruption.sqlite")).unwrap();
        let id = Uuid::from_u128(101);
        let entries = journal(id, 1, 0);
        persist(&store, &entries);
        let connection = store.lock().unwrap();
        assert!(journal_entry(&connection, id, entries.len() as u64 + 1)
            .unwrap()
            .is_none());
        let key = format!("{:020}", entries.len());
        match damage {
            "non-blob" => {
                connection.execute("UPDATE workflow_operations SET response='{}' WHERE operation_namespace=?1 AND operation_id=?2", params![namespace(id), key]).unwrap();
            }
            "malformed" => {
                connection.execute("UPDATE workflow_operations SET response=x'ff' WHERE operation_namespace=?1 AND operation_id=?2", params![namespace(id), key]).unwrap();
            }
            "digest" => {
                connection.execute("UPDATE workflow_operations SET request_digest=?1 WHERE operation_namespace=?2 AND operation_id=?3", params!["0".repeat(64), namespace(id), key]).unwrap();
            }
            "clock" => {
                connection.execute("UPDATE workflow_operations SET created_at_ms=created_at_ms+1 WHERE operation_namespace=?1 AND operation_id=?2", params![namespace(id), key]).unwrap();
            }
            "gap" => {
                connection.execute("DELETE FROM workflow_operations WHERE operation_namespace=?1 AND operation_id=?2", params![namespace(id), format!("{:020}", 2)]).unwrap();
            }
            _ => {
                let mut changed = entries.last().unwrap().clone();
                match damage {
                    "session" => changed.session.grant.session_id = Uuid::from_u128(102),
                    "version" => changed.session.version += 1,
                    "chain" => changed.previous_digest = Some("b".repeat(64)),
                    _ => unreachable!(),
                }
                connection.execute("UPDATE workflow_operations SET response=?1,request_digest=?2 WHERE operation_namespace=?3 AND operation_id=?4",
                    params![encode(&changed).unwrap(), canonical_sha256("sentinel.workflow.adaptive-entry.v1", &changed).unwrap(), namespace(id), key]).unwrap();
            }
        }
        let changes = total_changes(&connection);
        for warm in [false, true] {
            let (result, failures) = validation_scope::with_validation_failures(|| {
                validation_scope::with_scope(&connection, || {
                    if warm {
                        replay_journal(&connection, id, None, Some(1))?;
                    }
                    load_with_feedback(&connection, id)
                })
            });
            assert_eq!(
                result.unwrap_err().code,
                WorkflowErrorCode::CorruptStore,
                "{damage}, warm={warm}"
            );
            assert!(!failures.is_empty());
            assert!(
                failures.iter().all(|failure| !matches!(
                    failure.reason,
                    ValidationFailureReason::BytesLimit | ValidationFailureReason::BytesOverflow
                )),
                "small corrupt fixtures must fail integrity, not resource admission"
            );
            assert!(connection.is_autocommit());
            assert_eq!(total_changes(&connection), changes);
        }
    }
}
