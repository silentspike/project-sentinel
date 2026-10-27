use super::*;
use crate::domain_store::{
    bytes_digest, encode, get_entity, tests::accepted_project_fixture, validate_project,
    validation_scope,
};
use rusqlite::{params, Connection};
use std::sync::mpsc;
use std::time::Duration;

fn owner(store: &WorkflowStore, tenant: &TenantId, project: &ProjectId) -> Arc<Option<ProjectV1>> {
    read_with(store, tenant, project, Arc::clone).unwrap()
}

fn legacy(
    store: &WorkflowStore,
    tenant: &TenantId,
    project: &ProjectId,
) -> Result<Option<ProjectV1>, WorkflowError> {
    tenant.validate()?;
    project.validate()?;
    let mut connection = store.connection.lock().map_err(|_| persistence())?;
    let key = serde_json::to_vec(&(tenant, project)).map_err(|_| corrupt())?;
    store.validated_read_snapshot(&mut connection, "legacy-project-test", &key, |snapshot| {
        let value = get_entity(snapshot, tenant, "project", &project.0)?;
        if let Some(project) = &value {
            validate_project(project)?;
        }
        Ok(value)
    })
}

fn replace(connection: &Connection, project: &ProjectV1) {
    let payload = encode(project).unwrap();
    let digest = bytes_digest("sentinel.workflow.company-entity-row.v1", &payload).unwrap();
    assert_eq!(
        connection.execute(
            "UPDATE company_entities SET version=?1,payload=?2,payload_digest=?3 WHERE tenant_id=?4 AND entity_kind='project' AND entity_id=?5",
            params![i64::try_from(project.version).unwrap(), payload, digest, project.tenant_id.0, project.project_id.0],
        ).unwrap(),
        1
    );
}

#[test]
fn project_read_cold_warm_owner_and_legacy_bytes_match_without_writes() {
    let (_temp, _path, store, _principal, project) = accepted_project_fixture();
    let changes = || {
        store
            .connection
            .lock()
            .unwrap()
            .query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))
            .unwrap()
    };
    let before = changes();
    let first = owner(&store, &project.tenant_id, &project.project_id);
    let second = owner(&store, &project.tenant_id, &project.project_id);
    assert!(Arc::ptr_eq(&first, &second));
    for _ in 0..2 {
        let current = store
            .company_project(&project.tenant_id, &project.project_id)
            .unwrap();
        assert_eq!(current, Some(project.clone()));
        assert_eq!(
            serde_json::to_vec(&current).unwrap(),
            serde_json::to_vec(&legacy(&store, &project.tenant_id, &project.project_id).unwrap())
                .unwrap()
        );
    }
    assert_eq!(changes(), before);
    assert!(store.connection.lock().unwrap().is_autocommit());
}

#[test]
fn project_read_materializes_after_both_store_guards_are_released() {
    let (_temp, _path, store, _principal, project) = accepted_project_fixture();
    std::thread::scope(|scope| {
        let mut nested = None;
        let (value, unlocked, completed) =
            read_with(&store, &project.tenant_id, &project.project_id, |owner| {
                let unlocked = store.connection.try_lock().is_ok();
                let (send, receive) = mpsc::channel();
                let store = &store;
                let project = &project;
                nested = Some(scope.spawn(move || {
                    let result = store.company_project(&project.tenant_id, &project.project_id);
                    send.send(result).unwrap();
                }));
                // Return before asserting so even a regression releases its guards before join.
                (
                    owner.as_ref().clone(),
                    unlocked,
                    receive.recv_timeout(Duration::from_secs(2)),
                )
            })
            .unwrap();
        nested.unwrap().join().unwrap();
        assert!(
            unlocked,
            "compatibility clone retained the connection guard"
        );
        assert_eq!(completed.unwrap().unwrap(), Some(project.clone()));
        assert_eq!(value, Some(project.clone()));
    });
}

#[test]
fn project_read_materializer_unwind_does_not_poison_store_guards() {
    let (_temp, _path, store, _principal, project) = accepted_project_fixture();
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        read_with(&store, &project.tenant_id, &project.project_id, |_| {
            panic!("materializer failed")
        })
    }));
    assert!(failed.is_err());
    assert!(!store.connection.is_poisoned());
    assert_eq!(
        store
            .company_project(&project.tenant_id, &project.project_id)
            .unwrap(),
        Some(project)
    );
}

#[test]
fn project_read_invalid_ids_and_poisoned_connection_preserve_legacy_errors() {
    let (_temp, _path, store, _principal, project) = accepted_project_fixture();
    for (tenant, id) in [
        (TenantId(String::new()), project.project_id.clone()),
        (project.tenant_id.clone(), ProjectId(String::new())),
    ] {
        assert_eq!(
            store.company_project(&tenant, &id).unwrap_err(),
            legacy(&store, &tenant, &id).unwrap_err()
        );
    }
    let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = store.connection.lock().unwrap();
        panic!("poison fixture connection");
    }));
    assert!(poisoned.is_err());
    assert_eq!(
        store
            .company_project(&project.tenant_id, &project.project_id)
            .unwrap_err(),
        legacy(&store, &project.tenant_id, &project.project_id).unwrap_err()
    );
}

#[test]
fn project_read_absence_and_tenant_keys_never_share_an_authority() {
    let (_temp, path, store, _principal, project) = accepted_project_fixture();
    let foreign = TenantId("foreign-tenant".into());
    let missing = ProjectId("missing-project".into());
    let absent = owner(&store, &project.tenant_id, &missing);
    assert!(absent.is_none());
    assert!(Arc::ptr_eq(
        &absent,
        &owner(&store, &project.tenant_id, &missing)
    ));
    assert!(store
        .company_project(&foreign, &project.project_id)
        .unwrap()
        .is_none());
    assert!(!Arc::ptr_eq(
        &absent,
        &owner(&store, &foreign, &project.project_id)
    ));
    let external = Connection::open(path).unwrap();
    let mut inserted = project;
    inserted.project_id = missing.clone();
    let payload = encode(&inserted).unwrap();
    let digest = bytes_digest("sentinel.workflow.company-entity-row.v1", &payload).unwrap();
    external
        .execute(
            "INSERT INTO company_entities VALUES(?1,'project',?2,?3,?4,?5)",
            params![
                inserted.tenant_id.0,
                missing.0,
                i64::try_from(inserted.version).unwrap(),
                payload,
                digest
            ],
        )
        .unwrap();
    let present = owner(&store, &inserted.tenant_id, &missing);
    assert_eq!(present.as_ref(), &Some(inserted));
    assert!(!Arc::ptr_eq(&absent, &present));
}

#[test]
fn project_read_same_and_external_connection_changes_invalidate_the_owner() {
    let (_temp, path, store, _principal, mut project) = accepted_project_fixture();
    let first = owner(&store, &project.tenant_id, &project.project_id);
    project.version += 1;
    project.updated_at_unix_ms += 1;
    replace(&Connection::open(&path).unwrap(), &project);
    let external = owner(&store, &project.tenant_id, &project.project_id);
    assert!(!Arc::ptr_eq(&first, &external));
    project.version += 1;
    project.updated_at_unix_ms += 1;
    replace(&store.connection.lock().unwrap(), &project);
    let local = owner(&store, &project.tenant_id, &project.project_id);
    assert!(!Arc::ptr_eq(&external, &local));
    assert_eq!(local.as_ref(), &Some(project));
}

#[test]
fn project_read_commit_after_snapshot_does_not_rewrite_the_retained_owner() {
    let (_temp, path, store, _principal, project) = accepted_project_fixture();
    let mut changed = project.clone();
    changed.version += 1;
    changed.updated_at_unix_ms += 1;
    let observed = read_with(&store, &project.tenant_id, &project.project_id, |owner| {
        replace(&Connection::open(path).unwrap(), &changed);
        owner.as_ref().clone()
    })
    .unwrap();
    assert_eq!(observed, Some(project));
    assert_eq!(
        store
            .company_project(&changed.tenant_id, &changed.project_id)
            .unwrap(),
        Some(changed)
    );
}

#[test]
fn project_read_outer_transaction_and_rollback_do_not_retain_transient_owners() {
    let (_temp, _path, store, _principal, project) = accepted_project_fixture();
    let original = owner(&store, &project.tenant_id, &project.project_id);
    let mut changed = project.clone();
    changed.version += 1;
    changed.updated_at_unix_ms += 1;
    for (begin, rollback) in [
        ("BEGIN", "ROLLBACK"),
        ("SAVEPOINT caller", "ROLLBACK TO caller; RELEASE caller"),
    ] {
        {
            let connection = store.connection.lock().unwrap();
            connection.execute_batch(begin).unwrap();
            replace(&connection, &changed);
        }
        let transient = owner(&store, &project.tenant_id, &project.project_id);
        assert_eq!(transient.as_ref(), &Some(changed.clone()));
        assert!(!Arc::ptr_eq(
            &transient,
            &owner(&store, &project.tenant_id, &project.project_id)
        ));
        let connection = store.connection.lock().unwrap();
        assert!(!connection.is_autocommit());
        connection.execute_batch(rollback).unwrap();
        drop(connection);
        let restored = owner(&store, &project.tenant_id, &project.project_id);
        assert_eq!(restored.as_ref(), &Some(project.clone()));
        assert!(!Arc::ptr_eq(&original, &restored));
    }
}

#[test]
fn project_read_reopen_and_restored_database_never_adopt_an_old_owner() {
    let (temp, path, store, _principal, project) = accepted_project_fixture();
    let original = owner(&store, &project.tenant_id, &project.project_id);
    let snapshot = temp.path().join("restored.sqlite");
    store
        .connection
        .lock()
        .unwrap()
        .execute("VACUUM INTO ?1", [snapshot.to_str().unwrap()])
        .unwrap();
    let reopened = WorkflowStore::open(path).unwrap();
    let restored = WorkflowStore::open(snapshot).unwrap();
    for store in [&reopened, &restored] {
        let current = owner(store, &project.tenant_id, &project.project_id);
        assert_eq!(current, original);
        assert!(!Arc::ptr_eq(&current, &original));
    }
}

#[test]
fn project_read_resealed_shape_and_row_binding_corruption_keep_legacy_errors() {
    for mutation in ["shape", "row-version", "digest"] {
        let (_temp, _path, store, _principal, mut project) = accepted_project_fixture();
        owner(&store, &project.tenant_id, &project.project_id);
        let connection = store.connection.lock().unwrap();
        match mutation {
            "shape" => {
                project.schema_version = 0;
                replace(&connection, &project);
            }
            "row-version" => {
                connection
                    .execute(
                        "UPDATE company_entities SET version=version+1 WHERE entity_kind='project'",
                        [],
                    )
                    .unwrap();
            }
            _ => {
                connection
                    .execute(
                        "UPDATE company_entities SET payload_digest=?1 WHERE entity_kind='project'",
                        ["0".repeat(64)],
                    )
                    .unwrap();
            }
        }
        drop(connection);
        let mut materialized = false;
        let current = read_with(&store, &project.tenant_id, &project.project_id, |_| {
            materialized = true;
        });
        assert!(!materialized);
        assert_eq!(
            current.unwrap_err(),
            legacy(&store, &project.tenant_id, &project.project_id).unwrap_err()
        );
        assert!(store.connection.lock().unwrap().is_autocommit());
    }
}

#[test]
fn project_read_deletion_and_unsupported_schema_do_not_reuse_a_stale_owner() {
    let (_temp, path, store, _principal, project) = accepted_project_fixture();
    let first = owner(&store, &project.tenant_id, &project.project_id);
    let external = Connection::open(path).unwrap();
    external
        .execute_batch("CREATE TABLE project_read_future(value BLOB)")
        .unwrap();
    let uncached = owner(&store, &project.tenant_id, &project.project_id);
    assert_eq!(first, uncached);
    assert!(!Arc::ptr_eq(
        &uncached,
        &owner(&store, &project.tenant_id, &project.project_id)
    ));
    external
        .execute(
            "DELETE FROM company_entities WHERE entity_kind='project'",
            [],
        )
        .unwrap();
    assert!(store
        .company_project(&project.tenant_id, &project.project_id)
        .unwrap()
        .is_none());
}

#[test]
fn project_read_raw_index_damage_rejects_equal_sqlite_change_fences() {
    use std::io::{Seek, SeekFrom, Write};
    let (_temp, path, store, _principal, project) = accepted_project_fixture();
    let fences = |connection: &Connection| {
        [
            "PRAGMA data_version",
            "PRAGMA schema_version",
            "SELECT total_changes()",
        ]
        .map(|sql| {
            connection
                .query_row(sql, [], |row| row.get::<_, i64>(0))
                .unwrap()
        })
    };
    let (page_size, page) = {
        let connection = store.connection.lock().unwrap();
        connection.execute_batch("CREATE INDEX project_read_damage ON company_entities(payload_digest); PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
        (
            connection
                .query_row("PRAGMA page_size", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            connection
                .query_row(
                    "SELECT rootpage FROM sqlite_schema WHERE name='project_read_damage'",
                    [],
                    |row| row.get::<_, u32>(0),
                )
                .unwrap(),
        )
    };
    owner(&store, &project.tenant_id, &project.project_id);
    legacy(&store, &project.tenant_id, &project.project_id).unwrap();
    let connection = store.connection.lock().unwrap();
    let before = fences(&connection);
    connection.execute_batch("PRAGMA shrink_memory").unwrap();
    drop(connection);
    let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(u64::from(page - 1) * u64::from(page_size)))
        .unwrap();
    file.write_all(&vec![0; page_size as usize]).unwrap();
    file.sync_all().unwrap();
    assert_eq!(fences(&store.connection.lock().unwrap()), before);
    let current = store
        .company_project(&project.tenant_id, &project.project_id)
        .unwrap_err();
    assert_eq!(
        current,
        legacy(&store, &project.tenant_id, &project.project_id).unwrap_err()
    );
    assert!(store.connection.lock().unwrap().is_autocommit());
}

#[test]
fn project_read_persisted_validation_still_runs_on_a_cold_load_only() {
    let (_temp, _path, store, _principal, project) = accepted_project_fixture();
    let (cold, scopes) = validation_scope::with_completed_validations(|| {
        store.company_project(&project.tenant_id, &project.project_id)
    });
    assert_eq!(cold.unwrap(), Some(project.clone()));
    assert!(!scopes.is_empty());
    let (warm, scopes) = validation_scope::with_completed_validations(|| {
        store.company_project(&project.tenant_id, &project.project_id)
    });
    assert_eq!(warm.unwrap(), Some(project));
    assert!(scopes.is_empty());
}
