use super::*;
use serde::{Deserialize, Deserializer};
use std::cell::Cell;

const KIND: &str = "entity-shape-test";
const BYTE_LIMIT: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Counts {
    decoded: usize,
    base_pure: usize,
    base_persisted: usize,
    view_pure: usize,
    view_persisted: usize,
}

thread_local! {
    static COUNTS: Cell<Counts> = Cell::new(Counts::default());
}

struct CountGuard(Counts);

impl CountGuard {
    fn reset() -> Self {
        Self(COUNTS.with(|counts| counts.replace(Counts::default())))
    }
}

impl Drop for CountGuard {
    fn drop(&mut self) {
        COUNTS.with(|counts| counts.set(self.0));
    }
}

fn count(update: impl FnOnce(&mut Counts)) {
    COUNTS.with(|counts| {
        let mut value = counts.get();
        update(&mut value);
        counts.set(value);
    });
}

fn counts() -> Counts {
    COUNTS.with(Cell::get)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ShapeWire {
    tenant: TenantId,
    kind: String,
    id: String,
    version: u64,
    full_pure_valid: bool,
    full_persisted_valid: bool,
    mutate_during_validation: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
struct ShapeBase(ShapeWire);

impl<'de> Deserialize<'de> for ShapeBase {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = ShapeWire::deserialize(deserializer)?;
        count(|counts| counts.decoded += 1);
        Ok(Self(value))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
struct SealedView(ShapeBase);

fn dependency(connection: &Connection, value: &ShapeBase) -> Result<(), WorkflowError> {
    let version: i64 = connection.query_row(
        "SELECT version FROM entity_shape_test_dependencies WHERE id=?1",
        [&value.0.id],
        |row| row.get(0),
    )?;
    if stored_u64(version)? != value.0.version {
        return Err(corrupt());
    }
    Ok(())
}

impl CompanyEntity for ShapeBase {
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        let kind = if self.0.kind == KIND {
            KIND
        } else {
            "foreign-shape-kind"
        };
        (&self.0.tenant, kind, &self.0.id, self.0.version)
    }

    fn validate_entity(&self) -> Result<(), WorkflowError> {
        count(|counts| counts.base_pure += 1);
        if !self.0.full_pure_valid {
            return Err(corrupt());
        }
        Ok(())
    }

    fn validate_persisted(&self, connection: &Connection) -> Result<(), WorkflowError> {
        count(|counts| counts.base_persisted += 1);
        dependency(connection, self)?;
        if self.0.mutate_during_validation {
            connection.execute(
                "UPDATE entity_shape_test_dependencies SET version=version+1 WHERE id=?1",
                [&self.0.id],
            )?;
        }
        if !self.0.full_persisted_valid {
            return Err(corrupt());
        }
        Ok(())
    }
}

impl CompanyEntity for SealedView {
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        self.0.row_binding()
    }

    fn load_shape(
        connection: &Connection,
        tenant: &TenantId,
        kind: &str,
        id: &str,
    ) -> Result<Option<Self>, WorkflowError> {
        entity_shape::<ShapeBase>(connection, tenant, kind, id).map(|value| value.map(Self))
    }

    fn validate_entity(&self) -> Result<(), WorkflowError> {
        count(|counts| counts.view_pure += 1);
        if self.0 .0.id.is_empty() || self.0 .0.version == 0 {
            return Err(corrupt());
        }
        Ok(())
    }

    fn validate_persisted(&self, connection: &Connection) -> Result<(), WorkflowError> {
        count(|counts| counts.view_persisted += 1);
        dependency(connection, &self.0)
    }
}

fn fixture() -> (tempfile::TempDir, WorkflowStore, ShapeBase) {
    let (root, _path, store, principal, _project) = tests::accepted_project_fixture();
    let value = ShapeBase(ShapeWire {
        tenant: principal.tenant_id,
        kind: KIND.into(),
        id: "shape-a".into(),
        version: 1,
        full_pure_valid: true,
        full_persisted_valid: true,
        mutate_during_validation: false,
    });
    {
        let mut connection = store.connection.lock().unwrap();
        connection.execute_batch("CREATE TABLE entity_shape_test_dependencies(id TEXT PRIMARY KEY, version INTEGER NOT NULL)").unwrap();
        let transaction = connection.transaction().unwrap();
        put_entity(
            &transaction,
            &value.0.tenant,
            KIND,
            &value.0.id,
            value.0.version,
            &value,
        )
        .unwrap();
        transaction
            .execute(
                "INSERT INTO entity_shape_test_dependencies(id,version) VALUES(?1,?2)",
                params![value.0.id, sql_u64(value.0.version).unwrap()],
            )
            .unwrap();
        transaction.commit().unwrap();
    }
    (root, store, value)
}

fn read<T: DeserializeOwned + Serialize + Clone + CompanyEntity + 'static>(
    connection: &Connection,
    value: &ShapeBase,
) -> Result<Option<T>, WorkflowError> {
    get_entity(connection, &value.0.tenant, KIND, &value.0.id)
}

fn rewrite_payload(connection: &Connection, locator: &ShapeBase, payload: &ShapeBase) {
    let bytes = encode(payload).unwrap();
    let digest = bytes_digest("sentinel.workflow.company-entity-row.v1", &bytes).unwrap();
    assert_eq!(connection.execute(
        "UPDATE company_entities SET payload=?1,payload_digest=?2 WHERE tenant_id=?3 AND entity_kind=?4 AND entity_id=?5",
        params![bytes, digest, locator.0.tenant.0, KIND, locator.0.id],
    ).unwrap(), 1);
}

#[test]
fn entity_shape_reuse_reads_body_once_and_validates_each_view_in_either_order() {
    for view_first in [false, true] {
        let _counts = CountGuard::reset();
        let (_root, store, value) = fixture();
        let connection = store.connection.lock().unwrap();
        let before: i64 = connection
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        validation_scope::with_scope(&connection, || {
            for _ in 0..3 {
                if view_first {
                    assert_eq!(
                        read::<SealedView>(&connection, &value)?,
                        Some(SealedView(value.clone()))
                    );
                    assert_eq!(read::<ShapeBase>(&connection, &value)?, Some(value.clone()));
                } else {
                    assert_eq!(read::<ShapeBase>(&connection, &value)?, Some(value.clone()));
                    assert_eq!(
                        read::<SealedView>(&connection, &value)?,
                        Some(SealedView(value.clone()))
                    );
                }
            }
            assert_eq!(validation_scope::validations("entity-shape"), 1);
            assert_eq!(validation_scope::validations("entity"), 2);
            assert_eq!(
                counts(),
                Counts {
                    decoded: 1,
                    base_pure: 1,
                    base_persisted: 1,
                    view_pure: 1,
                    view_persisted: 1
                }
            );
            Ok(())
        })
        .unwrap();
        assert_eq!(
            connection
                .query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            before
        );
        assert!(connection.is_autocommit());
    }
}

#[test]
fn entity_shape_reuse_sealed_success_never_authorizes_full_failure() {
    for pure_failure in [false, true] {
        for view_first in [false, true] {
            let _counts = CountGuard::reset();
            let (_root, store, mut value) = fixture();
            let connection = store.connection.lock().unwrap();
            value.0.full_pure_valid = !pure_failure;
            value.0.full_persisted_valid = pure_failure;
            rewrite_payload(&connection, &value, &value);
            validation_scope::with_scope(&connection, || {
                if view_first {
                    assert!(read::<SealedView>(&connection, &value)?.is_some());
                }
                assert_eq!(
                    read::<ShapeBase>(&connection, &value).unwrap_err().code,
                    WorkflowErrorCode::CorruptStore
                );
                if !view_first {
                    assert!(read::<SealedView>(&connection, &value)?.is_some());
                }
                assert_eq!(validation_scope::validations("entity-shape"), 1);
                assert_eq!(
                    counts(),
                    Counts {
                        decoded: 1,
                        base_pure: 1,
                        base_persisted: usize::from(!pure_failure),
                        view_pure: 1,
                        view_persisted: 1
                    }
                );
                Ok(())
            })
            .unwrap();
        }
    }
}

#[test]
fn entity_shape_reuse_rejects_raw_digest_version_and_key_mismatches() {
    for damage in [
        "digest",
        "zero-version",
        "negative-version",
        "version",
        "tenant",
        "kind",
        "id",
        "non-blob",
    ] {
        for view in [false, true] {
            let _counts = CountGuard::reset();
            let (_root, store, value) = fixture();
            let connection = store.connection.lock().unwrap();
            match damage {
                "digest" => {
                    connection.execute("UPDATE company_entities SET payload_digest='broken' WHERE entity_kind=?1", [KIND]).unwrap();
                }
                "zero-version" | "negative-version" | "version" => {
                    let version = match damage {
                        "zero-version" => 0,
                        "negative-version" => -1,
                        _ => 2,
                    };
                    connection
                        .execute(
                            "UPDATE company_entities SET version=?1 WHERE entity_kind=?2",
                            params![version, KIND],
                        )
                        .unwrap();
                }
                "non-blob" => {
                    connection.execute("UPDATE company_entities SET payload=CAST(payload AS TEXT) WHERE entity_kind=?1", [KIND]).unwrap();
                }
                _ => {
                    let mut changed = value.clone();
                    match damage {
                        "tenant" => changed.0.tenant = TenantId::parse("foreign-tenant").unwrap(),
                        "kind" => changed.0.kind = "foreign-shape-kind".into(),
                        "id" => changed.0.id = "foreign-id".into(),
                        _ => unreachable!(),
                    }
                    rewrite_payload(&connection, &value, &changed);
                }
            }
            let result = if view {
                read::<SealedView>(&connection, &value).map(|_| ())
            } else {
                read::<ShapeBase>(&connection, &value).map(|_| ())
            };
            let expected = if damage == "non-blob" {
                WorkflowErrorCode::PersistenceFailure
            } else {
                WorkflowErrorCode::CorruptStore
            };
            assert_eq!(result.unwrap_err().code, expected, "{damage}; view={view}");
            let observed = counts();
            assert_eq!(
                (
                    observed.base_pure,
                    observed.base_persisted,
                    observed.view_pure,
                    observed.view_persisted
                ),
                (0, 0, 0, 0)
            );
            assert!(connection.is_autocommit());
        }
    }
}

#[test]
fn entity_shape_reuse_preserves_missing_and_distinct_row_membership() {
    let _counts = CountGuard::reset();
    let (_root, store, value) = fixture();
    let mut connection = store.connection.lock().unwrap();
    let mut second = value.clone();
    second.0.id = "shape-b".into();
    let transaction = connection.transaction().unwrap();
    put_entity(
        &transaction,
        &second.0.tenant,
        KIND,
        &second.0.id,
        second.0.version,
        &second,
    )
    .unwrap();
    transaction
        .execute(
            "INSERT INTO entity_shape_test_dependencies(id,version) VALUES(?1,?2)",
            params![second.0.id, sql_u64(second.0.version).unwrap()],
        )
        .unwrap();
    transaction.commit().unwrap();
    validation_scope::with_scope(&connection, || {
        for item in [&value, &second] {
            assert_eq!(read::<ShapeBase>(&connection, item)?, Some(item.clone()));
            assert_eq!(
                read::<SealedView>(&connection, item)?,
                Some(SealedView(item.clone()))
            );
        }
        let mut missing = value.clone();
        missing.0.id = "missing-shape".into();
        assert!(read::<SealedView>(&connection, &missing)?.is_none());
        assert!(read::<ShapeBase>(&connection, &missing)?.is_none());
        assert_eq!(validation_scope::validations("entity-shape"), 3);
        assert_eq!(validation_scope::validations("entity"), 6);
        assert_eq!(
            counts(),
            Counts {
                decoded: 2,
                base_pure: 2,
                base_persisted: 2,
                view_pure: 2,
                view_persisted: 2
            }
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn entity_shape_reuse_invalidates_after_write_and_rebuilds_next_scope() {
    let _counts = CountGuard::reset();
    let (_root, store, value) = fixture();
    let connection = store.connection.lock().unwrap();
    let mut changed = value.clone();
    changed.0.version = 2;
    validation_scope::with_scope(&connection, || {
        assert_eq!(read::<ShapeBase>(&connection, &value)?, Some(value.clone()));
        validation_scope::before_write(&connection)?;
        rewrite_payload(&connection, &value, &changed);
        connection.execute(
            "UPDATE company_entities SET version=2 WHERE entity_kind=?1",
            [KIND],
        )?;
        connection.execute(
            "UPDATE entity_shape_test_dependencies SET version=2 WHERE id=?1",
            [&value.0.id],
        )?;
        assert_eq!(
            read::<ShapeBase>(&connection, &value)?,
            Some(changed.clone())
        );
        assert_eq!(counts().decoded, 2);
        Ok(())
    })
    .unwrap();
    validation_scope::with_scope(&connection, || {
        assert_eq!(
            read::<SealedView>(&connection, &value)?,
            Some(SealedView(changed.clone()))
        );
        assert_eq!(
            read::<ShapeBase>(&connection, &value)?,
            Some(changed.clone())
        );
        assert_eq!(validation_scope::validations("entity-shape"), 1);
        assert_eq!(
            counts(),
            Counts {
                decoded: 3,
                base_pure: 3,
                base_persisted: 3,
                view_pure: 1,
                view_persisted: 1
            }
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn entity_shape_reuse_rejects_mutation_between_pure_and_persisted_proofs() {
    let _counts = CountGuard::reset();
    let (_root, store, mut value) = fixture();
    let connection = store.connection.lock().unwrap();
    value.0.mutate_during_validation = true;
    rewrite_payload(&connection, &value, &value);
    let (result, failures) = validation_scope::with_validation_failures(|| {
        validation_scope::with_scope(&connection, || {
            assert!(read::<SealedView>(&connection, &value)?.is_some());
            read::<ShapeBase>(&connection, &value)
        })
    });
    assert_eq!(result.unwrap_err().code, WorkflowErrorCode::CorruptStore);
    assert_eq!(failures.len(), 1);
    assert_eq!(
        failures[0].reason,
        validation_scope::ValidationFailureReason::WriteDuringValidation
    );
    assert_eq!(
        counts(),
        Counts {
            decoded: 1,
            base_pure: 1,
            base_persisted: 1,
            view_pure: 1,
            view_persisted: 1
        }
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT version FROM entity_shape_test_dependencies WHERE id=?1",
                [&value.0.id],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert!(connection.is_autocommit());
}

#[test]
fn entity_shape_reuse_charges_full_owner_once_without_raising_byte_limit() {
    let _counts = CountGuard::reset();
    let (_root, store, value) = fixture();
    let connection = store.connection.lock().unwrap();
    let payload_bytes = encode(&value).unwrap().len();
    let (result, failures) = validation_scope::with_validation_failures(|| {
        validation_scope::with_scope(&connection, || {
            assert!(read::<SealedView>(&connection, &value)?.is_some());
            assert!(read::<ShapeBase>(&connection, &value)?.is_some());
            assert_eq!(validation_scope::validations("entity-shape"), 1);
            validation_scope::charge_bytes(&connection, BYTE_LIMIT)
        })
    });
    assert_eq!(result.unwrap_err().code, WorkflowErrorCode::CorruptStore);
    assert_eq!(failures.len(), 1);
    let failure = &failures[0];
    assert_eq!(
        failure.reason,
        validation_scope::ValidationFailureReason::BytesLimit
    );
    assert_eq!(
        failure.charge_phase,
        Some(validation_scope::ValidationChargePhase::Body)
    );
    assert_eq!(failure.attempted_bytes, Some(BYTE_LIMIT));
    assert!(failure.bytes > payload_bytes && failure.bytes < BYTE_LIMIT);
    let shape = failure
        .top_contributors
        .iter()
        .find(|entry| entry.domain == Some("entity-shape"))
        .unwrap();
    assert_eq!(shape.totals.body, payload_bytes);
    assert_eq!(shape.totals.result, payload_bytes);
    assert_eq!(
        failure.charge_totals.key + failure.charge_totals.body + failure.charge_totals.result,
        failure.bytes
    );
    assert_eq!(counts().decoded, 1);
    assert!(connection.is_autocommit());
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct LeafValue {
    revision: u16,
    label: String,
}

#[derive(Clone)]
struct LeafExpected {
    tenant: TenantId,
    project: ProjectId,
    event_type: String,
    operation_id: Uuid,
    operation_digest: String,
    principal: AuthenticatedCompanyPrincipalV1,
    issued_at: u64,
    value: LeafValue,
}

fn append_leaf(
    transaction: &Transaction<'_>,
    expected: &LeafExpected,
) -> Result<u64, WorkflowError> {
    append_event(
        transaction,
        &expected.principal,
        expected.operation_id,
        &expected.operation_digest,
        Some(&expected.project),
        &expected.event_type,
        &expected.value,
        expected.issued_at,
    )
}

fn require_leaf(connection: &Connection, expected: &LeafExpected) -> Result<(), WorkflowError> {
    adaptive_resume_policy::require_leaf_event(
        connection,
        &expected.tenant,
        &expected.project,
        &expected.event_type,
        expected.operation_id,
        &expected.operation_digest,
        &expected.principal,
        expected.issued_at,
        &expected.value,
    )
}

fn leaf_fixture() -> (tempfile::TempDir, WorkflowStore, LeafExpected) {
    let (root, _path, store, principal, project) = tests::accepted_project_fixture();
    let value = LeafValue {
        revision: 1,
        label: "synthetic-leaf-proof".into(),
    };
    let expected = LeafExpected {
        tenant: principal.tenant_id.clone(),
        project: project.project_id,
        event_type: "entity_shape_test_leaf".into(),
        operation_id: Uuid::new_v4(),
        operation_digest: canonical_sha256("entity-shape-test-leaf.v1", &value).unwrap(),
        principal,
        issued_at: 20,
        value,
    };
    {
        let mut connection = store.connection.lock().unwrap();
        let transaction = connection.transaction().unwrap();
        append_leaf(&transaction, &expected).unwrap();
        transaction.commit().unwrap();
    }
    (root, store, expected)
}

#[test]
fn leaf_event_memo_reuses_exact_tuple_and_rejects_every_changed_component() {
    for field in [
        "tenant",
        "project",
        "type",
        "operation",
        "digest",
        "principal",
        "time",
        "value",
    ] {
        let (_root, store, expected) = leaf_fixture();
        let connection = store.connection.lock().unwrap();
        let before: i64 = connection
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        validation_scope::with_scope(&connection, || {
            for _ in 0..3 {
                require_leaf(&connection, &expected)?;
            }
            assert_eq!(validation_scope::validations("leaf-event"), 1);
            let mut changed = expected.clone();
            match field {
                "tenant" => changed.tenant = TenantId::parse("foreign-tenant")?,
                "project" => changed.project = ProjectId::parse("foreign-project")?,
                "type" => changed.event_type.push_str("_foreign"),
                "operation" => changed.operation_id = Uuid::new_v4(),
                "digest" => changed.operation_digest = "b".repeat(64),
                "principal" => changed.principal.authority_generation += 1,
                "time" => changed.issued_at += 1,
                "value" => changed.value.revision += 1,
                _ => unreachable!(),
            }
            let validations = validation_scope::validations("leaf-event");
            assert_eq!(
                require_leaf(&connection, &changed).unwrap_err().code,
                WorkflowErrorCode::CorruptStore,
                "{field}"
            );
            assert_eq!(
                validation_scope::validations("leaf-event"),
                validations + 1,
                "{field}"
            );
            require_leaf(&connection, &expected)?;
            assert_eq!(validation_scope::validations("leaf-event"), validations + 1);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            connection
                .query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            before
        );
        assert!(connection.is_autocommit());
    }
}

#[test]
fn leaf_event_memo_revalidates_after_native_write_and_in_next_scope() {
    let (_root, store, expected) = leaf_fixture();
    let mut connection = store.connection.lock().unwrap();
    let transaction = connection.transaction().unwrap();
    validation_scope::with_scope(&transaction, || {
        require_leaf(&transaction, &expected)?;
        require_leaf(&transaction, &expected)?;
        let validations = validation_scope::validations("leaf-event");
        assert_eq!(validations, 1);
        let mut unrelated = expected.clone();
        unrelated.operation_id = Uuid::new_v4();
        unrelated.issued_at += 1;
        append_leaf(&transaction, &unrelated)?;
        require_leaf(&transaction, &expected)?;
        assert_eq!(validation_scope::validations("leaf-event"), validations + 1);
        Ok(())
    })
    .unwrap();
    assert!(!transaction.is_autocommit());
    transaction.commit().unwrap();
    validation_scope::with_scope(&connection, || {
        assert_eq!(validation_scope::validations("leaf-event"), 0);
        require_leaf(&connection, &expected)?;
        require_leaf(&connection, &expected)?;
        assert_eq!(validation_scope::validations("leaf-event"), 1);
        Ok(())
    })
    .unwrap();
    assert!(connection.is_autocommit());
}

#[test]
fn leaf_event_memo_rejects_corruption_and_duplicates_on_cold_and_warm_reads() {
    for damage in ["payload-digest", "event-id", "duplicate", "non-blob"] {
        for warm in [false, true] {
            let (_root, store, expected) = leaf_fixture();
            let mut connection = store.connection.lock().unwrap();
            let transaction = connection.transaction().unwrap();
            let damage_event = || -> Result<(), WorkflowError> {
                if damage == "duplicate" {
                    // The native writer creates two individually sealed events for one leaf selector.
                    let mut duplicate = expected.clone();
                    duplicate.issued_at += 1;
                    append_leaf(&transaction, &duplicate)?;
                } else {
                    let sql = match damage {
                        "payload-digest" => "UPDATE company_events SET payload_digest='broken' WHERE event_type=?1 AND operation_id=?2",
                        "non-blob" => "UPDATE company_events SET payload=CAST(payload AS TEXT) WHERE event_type=?1 AND operation_id=?2",
                        _ => "UPDATE company_events SET event_id='broken' WHERE event_type=?1 AND operation_id=?2",
                    };
                    assert_eq!(
                        transaction.execute(
                            sql,
                            params![expected.event_type, expected.operation_id.to_string()]
                        )?,
                        1
                    );
                }
                Ok(())
            };
            if !warm {
                damage_event().unwrap();
            }
            let result = validation_scope::with_scope(&transaction, || {
                if warm {
                    require_leaf(&transaction, &expected)?;
                    damage_event()?;
                }
                let validations = validation_scope::validations("leaf-event");
                let result = require_leaf(&transaction, &expected);
                assert_eq!(
                    validation_scope::validations("leaf-event"),
                    validations + 1,
                    "{damage}; warm={warm}"
                );
                result
            });
            let expected_code = if damage == "non-blob" {
                WorkflowErrorCode::PersistenceFailure
            } else {
                WorkflowErrorCode::CorruptStore
            };
            assert_eq!(
                result.unwrap_err().code,
                expected_code,
                "{damage}; warm={warm}"
            );
            transaction.rollback().unwrap();
            validation_scope::with_scope(&connection, || {
                require_leaf(&connection, &expected)?;
                assert_eq!(validation_scope::validations("leaf-event"), 1);
                Ok(())
            })
            .unwrap();
            assert!(connection.is_autocommit());
        }
    }
}

#[test]
fn company_event_owner_reuses_one_copy_and_preserves_distinct_history_metadata() {
    let (_root, store, expected) = leaf_fixture();
    let mut connection = store.connection.lock().unwrap();
    let first_sequence: i64 = connection
        .query_row(
            "SELECT sequence FROM company_events WHERE event_type=?1 AND operation_id=?2",
            params![expected.event_type, expected.operation_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let first_sequence = stored_u64(first_sequence).unwrap();
    let mut second = expected.clone();
    second.operation_id = Uuid::new_v4();
    second.issued_at += 1;
    second.value.revision += 1;
    second.operation_digest = canonical_sha256("entity-shape-test-leaf.v1", &second.value).unwrap();
    let transaction = connection.transaction().unwrap();
    let second_sequence = append_leaf(&transaction, &second).unwrap();
    transaction.commit().unwrap();
    assert_ne!(first_sequence, second_sequence);
    let first_row = read_company_event_row(&connection, first_sequence)
        .unwrap()
        .unwrap();
    let second_row = read_company_event_row(&connection, second_sequence)
        .unwrap()
        .unwrap();
    let changes_before: i64 = connection
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap();
    let copies = Cell::new(0);
    validation_scope::with_scope(&connection, || {
        let first = company_event_shape_with_observer(&connection, first_sequence, |bytes| {
            assert_eq!(bytes, first_row.payload.len());
            copies.set(copies.get() + 1);
        })?
        .unwrap();
        for _ in 0..3 {
            let repeated = company_event_shape_with_observer(&connection, first_sequence, |_| {
                copies.set(copies.get() + 1);
            })?
            .unwrap();
            assert!(std::sync::Arc::ptr_eq(&first.0, &repeated.0));
            assert_eq!(encode(&*repeated)?, encode(&first_row)?);
        }
        assert_eq!(copies.get(), 1);
        let historical =
            company_event_shape_with_observer(&connection, second_sequence, |bytes| {
                assert_eq!(bytes, second_row.payload.len());
                copies.set(copies.get() + 1);
            })?
            .unwrap();
        assert!(!std::sync::Arc::ptr_eq(&first.0, &historical.0));
        assert_eq!(encode(&*historical)?, encode(&second_row)?);
        assert_ne!(first.sequence, historical.sequence);
        assert_eq!(decode::<LeafValue>(&first.payload)?, expected.value);
        assert_eq!(decode::<LeafValue>(&historical.payload)?, second.value);
        let serialized = serde_json::to_value(&*first).unwrap();
        assert_eq!(
            serialized["payload"].as_str(),
            Some(std::str::from_utf8(&first.payload).unwrap())
        );
        require_leaf(&connection, &expected)?;
        require_leaf(&connection, &second)?;
        assert_eq!(validation_scope::validations("event-shape"), 2);
        assert_eq!(copies.get(), 2);
        Ok(())
    })
    .unwrap();
    assert_eq!(
        connection
            .query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        changes_before
    );
    assert!(connection.is_autocommit());
}

#[test]
fn company_event_owner_invalidates_on_write_and_next_scope_without_authorizing_leaf() {
    let (_root, store, expected) = leaf_fixture();
    let connection = store.connection.lock().unwrap();
    let sequence: i64 = connection
        .query_row(
            "SELECT sequence FROM company_events WHERE event_type=?1 AND operation_id=?2",
            params![expected.event_type, expected.operation_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let sequence = stored_u64(sequence).unwrap();
    let copies = Cell::new(0);
    let mut prior = None;
    let result = validation_scope::with_scope(&connection, || {
        let original = company_event_shape_with_observer(&connection, sequence, |_| {
            copies.set(copies.get() + 1);
        })?
        .unwrap();
        let original_digest = original.payload_digest.clone();
        prior = Some(original.clone());
        assert_eq!(
            connection.execute(
                "UPDATE company_events SET payload_digest='broken' WHERE sequence=?1",
                [sql_u64(sequence)?],
            )?,
            1
        );
        // An owner may retain corrupt metadata; only the caller's leaf proof grants trust.
        let changed = company_event_shape_with_observer(&connection, sequence, |_| {
            copies.set(copies.get() + 1);
        })?
        .unwrap();
        assert!(!std::sync::Arc::ptr_eq(&original.0, &changed.0));
        assert_eq!(original.payload_digest, original_digest);
        assert_eq!(changed.payload_digest, "broken");
        assert_eq!(copies.get(), 2);
        require_leaf(&connection, &expected)
    });
    assert_eq!(result.unwrap_err().code, WorkflowErrorCode::CorruptStore);
    assert!(connection.is_autocommit());
    validation_scope::with_scope(&connection, || {
        let fresh = company_event_shape_with_observer(&connection, sequence, |_| {
            copies.set(copies.get() + 1);
        })?
        .unwrap();
        let prior = prior.as_ref().unwrap();
        assert!(!std::sync::Arc::ptr_eq(&prior.0, &fresh.0));
        assert_eq!(encode(&*fresh)?, encode(&**prior)?);
        require_leaf(&connection, &expected)?;
        assert_eq!(validation_scope::validations("event-shape"), 1);
        assert_eq!(copies.get(), 3);
        Ok(())
    })
    .unwrap();
}

#[test]
fn company_event_owner_charges_before_copy_and_retains_full_utf8_row_not_pointer() {
    for boundary in ["body", "result", "invalid-utf8"] {
        let (_root, store, expected) = leaf_fixture();
        let connection = store.connection.lock().unwrap();
        let sequence: i64 = connection
            .query_row(
                "SELECT sequence FROM company_events WHERE event_type=?1 AND operation_id=?2",
                params![expected.event_type, expected.operation_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        let sequence = stored_u64(sequence).unwrap();
        let row = read_company_event_row(&connection, sequence)
            .unwrap()
            .unwrap();
        let payload_bytes = row.payload.len();
        let row_bytes = encode(&row).unwrap().len();
        let key_bytes = encode(&sequence).unwrap().len() + 256;
        assert!(row_bytes > payload_bytes);
        assert!(row_bytes > std::mem::size_of::<std::sync::Arc<CompanyEventRow>>());
        if boundary == "invalid-utf8" {
            assert_eq!(
                connection
                    .execute(
                        "UPDATE company_events SET payload=?1 WHERE sequence=?2",
                        params![vec![0xff_u8], sql_u64(sequence).unwrap()]
                    )
                    .unwrap(),
                1
            );
        }
        let copies = Cell::new(0);
        let (result, failures) = validation_scope::with_validation_failures(|| {
            validation_scope::with_scope(&connection, || {
                match boundary {
                    "body" => validation_scope::charge_bytes(
                        &connection,
                        BYTE_LIMIT - key_bytes - payload_bytes + 1,
                    )?,
                    "result" => validation_scope::charge_bytes(
                        &connection,
                        BYTE_LIMIT - key_bytes - payload_bytes - row_bytes + 1,
                    )?,
                    _ => {}
                }
                company_event_shape_with_observer(&connection, sequence, |_| {
                    copies.set(copies.get() + 1);
                })
                .map(|_| ())
            })
        });
        assert_eq!(
            result.unwrap_err().code,
            WorkflowErrorCode::CorruptStore,
            "{boundary}"
        );
        assert_eq!(failures.len(), 1);
        let failure = &failures[0];
        assert_eq!(failure.domain, Some("event-shape"));
        let shape = failure
            .top_contributors
            .iter()
            .find(|entry| entry.domain == Some("event-shape"))
            .unwrap();
        assert_eq!(shape.totals.key, key_bytes);
        assert_eq!(shape.totals.result, 0);
        match boundary {
            "body" => {
                assert_eq!(copies.get(), 0);
                assert_eq!(
                    failure.reason,
                    validation_scope::ValidationFailureReason::BytesLimit
                );
                assert_eq!(
                    failure.charge_phase,
                    Some(validation_scope::ValidationChargePhase::Body)
                );
                assert_eq!(failure.attempted_bytes, Some(payload_bytes));
                assert_eq!(failure.bytes, BYTE_LIMIT - payload_bytes + 1);
                assert_eq!(shape.totals.body, 0);
            }
            "result" => {
                assert_eq!(copies.get(), 1);
                assert_eq!(
                    failure.reason,
                    validation_scope::ValidationFailureReason::BytesLimit
                );
                assert_eq!(
                    failure.charge_phase,
                    Some(validation_scope::ValidationChargePhase::Result)
                );
                assert_eq!(failure.attempted_bytes, Some(row_bytes));
                assert_eq!(failure.bytes, BYTE_LIMIT - row_bytes + 1);
                assert_eq!(shape.totals.body, payload_bytes);
            }
            _ => {
                assert_eq!(copies.get(), 1);
                assert_eq!(
                    failure.reason,
                    validation_scope::ValidationFailureReason::ValidationError
                );
                assert_eq!(failure.charge_phase, None);
                assert_eq!(failure.attempted_bytes, None);
                assert_eq!(shape.totals.body, 1);
                assert_eq!(failure.bytes, key_bytes + 1);
            }
        }
        assert_eq!(
            failure.charge_totals.key + failure.charge_totals.body + failure.charge_totals.result,
            failure.bytes
        );
        assert!(connection.is_autocommit());
    }
}
