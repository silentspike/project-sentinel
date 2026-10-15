use crate::domain_store::*;
use std::cell::Cell;
use std::sync::Arc;

const OWNED_BODY_MIB: usize = 1024 * 1024;
const OWNED_BODY_LIMIT: usize = 64 * OWNED_BODY_MIB;

#[derive(Serialize)]
struct OwnedBodyHistoryPayload {
    ordinal: usize,
    padding: String,
}

struct OwnedBodyHistoryEvent {
    sequence: u64,
    payload_bytes: usize,
}

fn owned_body_history_fixture(
    count: usize,
    padding_bytes: usize,
) -> (tempfile::TempDir, WorkflowStore, Vec<OwnedBodyHistoryEvent>) {
    let (root, _path, store, principal, project) =
        crate::domain_store::tests::accepted_project_fixture();
    let mut events = Vec::with_capacity(count);
    {
        let mut connection = store.connection.lock().unwrap();
        let transaction = connection.transaction().unwrap();
        // Real appended UTF-8 rows exercise ownership, not permission or leaf trust.
        // Sparse newlines make serialized row bytes measurably exceed raw bytes.
        let chunk = format!("{}\n", "a".repeat(99));
        for ordinal in 0..count {
            let mut padding = chunk.repeat(padding_bytes / chunk.len());
            padding.push_str(&chunk[..padding_bytes % chunk.len()]);
            let payload = OwnedBodyHistoryPayload { ordinal, padding };
            let digest = canonical_sha256(
                "sentinel.workflow.owned-body-accounting-fixture.v1",
                &payload,
            )
            .unwrap();
            let sequence = append_event(
                &transaction,
                &principal,
                Uuid::new_v4(),
                &digest,
                Some(&project.project_id),
                "owned-body-accounting-fixture",
                &payload,
                project.updated_at_unix_ms + u64::try_from(ordinal).unwrap() + 1,
            )
            .unwrap();
            let bytes: i64 = transaction
                .query_row(
                    "SELECT length(payload) FROM company_events WHERE sequence=?1",
                    [sql_u64(sequence).unwrap()],
                    |row| row.get(0),
                )
                .unwrap();
            events.push(OwnedBodyHistoryEvent {
                sequence,
                payload_bytes: usize::try_from(bytes).unwrap(),
            });
        }
        transaction.commit().unwrap();
    }
    (root, store, events)
}

fn owned_body_key_bytes(sequence: u64) -> usize {
    serde_json::to_vec(&sequence).unwrap().len() + 256
}

fn owned_body_result_bytes(owner: &CompanyEventShape) -> usize {
    // Stream the actual full row and memo result, not size_of::<Arc<_>>() or an
    // allocated JSON copy of the tens-of-MiB fixture.
    let row_bytes = crate::digest::serialized_json_size(&**owner).unwrap();
    let result_bytes = crate::digest::serialized_json_size(&Some(owner.clone())).unwrap();
    assert_eq!(result_bytes, row_bytes);
    assert!(result_bytes > owner.payload.len());
    result_bytes
}

#[test]
fn event_owned_history_40mib_fits_once_with_exact_serialized_charges() {
    let (_root, store, events) = owned_body_history_fixture(4, 10 * OWNED_BODY_MIB);
    let connection = store.connection.lock().unwrap();
    let raw_total: usize = events.iter().map(|event| event.payload_bytes).sum();
    assert!((35 * OWNED_BODY_MIB..45 * OWNED_BODY_MIB).contains(&raw_total));
    let changes_before: i64 = connection
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap();

    // First finish normally. Then fill the exact remaining budget and verify
    // that one extra byte fails, exposing any undercharge of the full owners.
    for probe_budget in [false, true] {
        let copies = Cell::new(0);
        let mut keys = 0;
        let mut serialized = 0;
        let mut weak_owner = None;
        let (result, failures) = validation_scope::with_validation_failures(|| {
            validation_scope::with_scope(&connection, || {
                let mut owners: Vec<CompanyEventShape> = Vec::with_capacity(events.len());
                for event in &events {
                    let owner = company_event_shape_with_observer(
                        &connection,
                        event.sequence,
                        |bytes| {
                            assert_eq!(bytes, event.payload_bytes);
                            copies.set(copies.get() + 1);
                        },
                    )?
                    .unwrap();
                    assert_eq!(stored_u64(owner.sequence)?, event.sequence);
                    assert_eq!(owner.payload.len(), event.payload_bytes);
                    for previous in &owners {
                        assert!(!Arc::ptr_eq(&previous.0, &owner.0));
                    }
                    weak_owner = Some(Arc::downgrade(&owner.0));
                    keys += owned_body_key_bytes(event.sequence);
                    serialized += owned_body_result_bytes(&owner);
                    owners.push(owner);
                }
                assert!((35 * OWNED_BODY_MIB..45 * OWNED_BODY_MIB).contains(&serialized));
                assert!(serialized - raw_total > OWNED_BODY_MIB / 4);
                for _ in 0..3 {
                    for owner in &owners {
                        let repeated = company_event_shape_with_observer(
                            &connection,
                            stored_u64(owner.sequence)?,
                            |_| panic!("a warm event owner must not copy its body again"),
                        )?
                        .unwrap();
                        assert!(Arc::ptr_eq(&owner.0, &repeated.0));
                    }
                }
                assert_eq!(copies.get(), events.len());
                assert_eq!(validation_scope::validations("event-shape"), events.len());
                if probe_budget {
                    validation_scope::charge_bytes(
                        &connection,
                        OWNED_BODY_LIMIT - keys - serialized,
                    )?;
                    let repeated = company_event_shape_with_observer(
                        &connection,
                        stored_u64(owners[0].sequence)?,
                        |_| panic!("a hit at the byte limit must not copy or charge again"),
                    )?
                    .unwrap();
                    assert!(Arc::ptr_eq(&owners[0].0, &repeated.0));
                    validation_scope::charge_bytes(&connection, 1)?;
                }
                Ok(())
            })
        });
        if probe_budget {
            assert_eq!(result.unwrap_err().code, WorkflowErrorCode::CorruptStore);
            assert_eq!(failures.len(), 1);
            let failure = &failures[0];
            assert_eq!(failure.reason, validation_scope::ValidationFailureReason::BytesLimit);
            assert_eq!(failure.bytes, OWNED_BODY_LIMIT);
            assert_eq!(failure.attempted_bytes, Some(1));
            let owner_charges = failure
                .top_contributors
                .iter()
                .find(|entry| entry.domain == Some("event-shape"))
                .unwrap();
            assert_eq!(owner_charges.totals.key, keys);
            assert_eq!(owner_charges.totals.body, raw_total);
            assert_eq!(owner_charges.totals.result, serialized - raw_total);
            assert_eq!(
                failure.charge_totals.key
                    + failure.charge_totals.body
                    + failure.charge_totals.result,
                OWNED_BODY_LIMIT,
            );
        } else {
            result.unwrap();
            assert!(failures.is_empty());
        }
        assert!(weak_owner.unwrap().upgrade().is_none());
        assert_eq!(validation_scope::validations("event-shape"), 0);
        assert!(connection.is_autocommit());
    }
    assert_eq!(
        connection
            .query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        changes_before,
    );
}

#[test]
fn event_owned_history_over_64mib_rejects_before_copy_and_cleans_poison() {
    let (_root, store, events) = owned_body_history_fixture(8, 9 * OWNED_BODY_MIB);
    let connection = store.connection.lock().unwrap();
    let fixture_bytes: usize = events.iter().map(|event| event.payload_bytes).sum();
    assert!(fixture_bytes > OWNED_BODY_LIMIT);
    assert!(fixture_bytes < 100 * OWNED_BODY_MIB);
    let changes_before: i64 = connection
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap();
    let copies = Cell::new(0);
    let mut keys = 0;
    let mut raw = 0;
    let mut serialized = 0;
    let mut rejected_bytes = 0;
    let mut weak_owner = None;
    let (result, failures) = validation_scope::with_validation_failures(|| {
        validation_scope::with_scope(&connection, || -> Result<(), WorkflowError> {
            for event in &events {
                keys += owned_body_key_bytes(event.sequence);
                let copies_before = copies.get();
                let result = company_event_shape_with_observer(
                    &connection,
                    event.sequence,
                    |bytes| {
                        assert_eq!(bytes, event.payload_bytes);
                        copies.set(copies.get() + 1);
                    },
                );
                match result {
                    Ok(Some(owner)) => {
                        raw += event.payload_bytes;
                        serialized += owned_body_result_bytes(&owner);
                        weak_owner = Some(Arc::downgrade(&owner.0));
                    }
                    Err(error) => {
                        rejected_bytes = event.payload_bytes;
                        assert_eq!(copies.get(), copies_before);
                        assert!(keys + serialized + rejected_bytes > OWNED_BODY_LIMIT);
                        assert!(company_event_shape_with_observer(
                            &connection,
                            events[0].sequence,
                            |_| panic!("a poisoned scope must not copy even a warm owner"),
                        )
                        .is_err());
                        return Err(error);
                    }
                    Ok(None) => panic!("canonical appended event is missing"),
                }
            }
            panic!("distinct owned event bodies must not exceed the fixed 64 MiB budget");
        })
    });
    assert_eq!(result.unwrap_err().code, WorkflowErrorCode::CorruptStore);
    assert!(copies.get() > 0 && copies.get() < events.len());
    assert_eq!(failures.len(), 1);
    let failure = &failures[0];
    assert_eq!(failure.reason, validation_scope::ValidationFailureReason::BytesLimit);
    assert_eq!(failure.domain, Some("event-shape"));
    assert_eq!(failure.charge_phase, Some(validation_scope::ValidationChargePhase::Body));
    assert_eq!(failure.bytes, keys + serialized);
    assert_eq!(failure.attempted_bytes, Some(rejected_bytes));
    let owner_charges = failure
        .top_contributors
        .iter()
        .find(|entry| entry.domain == Some("event-shape"))
        .unwrap();
    assert_eq!(owner_charges.totals.key, keys);
    assert_eq!(owner_charges.totals.body, raw);
    assert_eq!(owner_charges.totals.result, serialized - raw);
    assert!(weak_owner.unwrap().upgrade().is_none());
    assert_eq!(validation_scope::validations("event-shape"), 0);
    assert!(connection.is_autocommit());

    let copies_before = copies.get();
    validation_scope::with_scope(&connection, || {
        company_event_shape_with_observer(&connection, events[0].sequence, |_| {
            copies.set(copies.get() + 1);
        })?
        .unwrap();
        assert_eq!(validation_scope::validations("event-shape"), 1);
        Ok(())
    })
    .unwrap();
    assert_eq!(copies.get(), copies_before + 1);
    assert_eq!(
        connection
            .query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        changes_before,
    );
    assert!(connection.is_autocommit());
}
