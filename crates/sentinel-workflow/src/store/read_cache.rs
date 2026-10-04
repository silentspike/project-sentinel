use std::any::{Any, TypeId};
use std::collections::BTreeMap;
use std::mem::size_of;

use rusqlite::types::{Value, ValueRef};
use rusqlite::Connection;
use serde::Serialize;

use crate::digest::serialized_json_size;
use crate::WorkflowError;

const MAX_INPUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_INPUT_ROWS: usize = 16_384;
const MAX_RESULT_BYTES: usize = 16 * 1024 * 1024;
const MAX_RESULTS: usize = 32;
const MAX_KEY_BYTES: usize = 4096;
const TABLES: &[&str] = &[
    "company_schema_meta",
    "company_entities",
    "company_operations",
    "company_events",
    "company_project_projections",
    "workflow_schema_meta",
    "workflow_work_items",
    "workflow_operations",
    "workflow_adaptive_heads",
    "workflow_execution_outbox",
    "workflow_completion_outbox",
    "workflow_gate_outbox",
    "workflow_audit_events",
    "sqlite_sequence",
];
const SCHEMA_QUERY: &str =
    "SELECT type,name,tbl_name,rootpage,sql FROM main.sqlite_schema ORDER BY type,name";

#[derive(Debug)]
struct InputTable {
    query: String,
    columns: usize,
    rows: Vec<Vec<Value>>,
}

#[derive(Debug)]
struct Inputs {
    tables: Vec<InputTable>,
}

impl Inputs {
    fn capture(connection: &Connection) -> rusqlite::Result<Option<Self>> {
        let temporary: i64 =
            connection.query_row("SELECT COUNT(*) FROM temp.sqlite_schema", [], |row| {
                row.get(0)
            })?;
        let attached: i64 = connection.query_row(
            "SELECT COUNT(*) FROM pragma_database_list WHERE name NOT IN ('main','temp')",
            [],
            |row| row.get(0),
        )?;
        if temporary != 0 || attached != 0 {
            return Ok(None);
        }
        let mut names = Vec::new();
        let mut statement = connection.prepare(
            "SELECT name,type,wr FROM pragma_table_list
            WHERE schema='main' AND name!='sqlite_schema' ORDER BY name",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(0)?;
            let kind: String = row.get(1)?;
            let without_rowid: i64 = row.get(2)?;
            // New stores must explicitly join the proof boundary before reuse.
            if !TABLES.contains(&name.as_str()) || kind != "table" || without_rowid != 0 {
                return Ok(None);
            }
            let mut columns =
                connection.prepare(&format!("PRAGMA main.table_xinfo(\"{name}\")"))?;
            let mut details = columns.query([])?;
            while let Some(column) = details.next()? {
                let name: String = column.get(1)?;
                let hidden: i64 = column.get(6)?;
                if hidden != 0
                    || ["rowid", "oid", "_rowid_"]
                        .iter()
                        .any(|alias| name.eq_ignore_ascii_case(alias))
                {
                    return Ok(None);
                }
            }
            names.push(name);
        }
        let queries = std::iter::once(SCHEMA_QUERY.to_owned()).chain(
            names
                .iter()
                .map(|name| format!("SELECT rowid,* FROM main.\"{name}\" ORDER BY rowid")),
        );
        let mut tables = Vec::new();
        let mut bytes = 0_usize;
        let mut count = 0_usize;
        for query in queries {
            let mut statement = connection.prepare(&query)?;
            let columns = statement.column_count();
            bytes = bytes.saturating_add(query.capacity() + size_of::<InputTable>());
            let mut rows = statement.query([])?;
            let mut values = Vec::new();
            while let Some(row) = rows.next()? {
                count += 1;
                bytes = bytes.saturating_add(size_of::<Vec<Value>>());
                if count > MAX_INPUT_ROWS || bytes > MAX_INPUT_BYTES {
                    return Ok(None);
                }
                let mut cells = Vec::with_capacity(columns);
                for column in 0..columns {
                    let value = row.get_ref(column)?;
                    let payload = match value {
                        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => bytes.len(),
                        _ => 0,
                    };
                    bytes = bytes.saturating_add(size_of::<Value>() + payload);
                    if bytes > MAX_INPUT_BYTES {
                        return Ok(None);
                    }
                    // SQLite TEXT need not be valid UTF-8. Optional proof capture
                    // must never panic on corrupt input or poison the store lock.
                    let owned = match value {
                        ValueRef::Text(bytes) => match std::str::from_utf8(bytes) {
                            Ok(text) => Value::Text(text.to_owned()),
                            Err(_) => return Ok(None),
                        },
                        other => other.into(),
                    };
                    cells.push(owned);
                }
                let previous_capacity = values.capacity();
                values.push(cells);
                bytes = bytes.saturating_add(
                    (values.capacity() - previous_capacity) * size_of::<Vec<Value>>(),
                );
                if bytes > MAX_INPUT_BYTES {
                    return Ok(None);
                }
            }
            tables.push(InputTable {
                query,
                columns,
                rows: values,
            });
        }
        Ok(Some(Self { tables }))
    }

    fn matches(&self, connection: &Connection) -> rusqlite::Result<bool> {
        let temporary: i64 =
            connection.query_row("SELECT COUNT(*) FROM temp.sqlite_schema", [], |row| {
                row.get(0)
            })?;
        let attached: i64 = connection.query_row(
            "SELECT COUNT(*) FROM pragma_database_list WHERE name NOT IN ('main','temp')",
            [],
            |row| row.get(0),
        )?;
        if temporary != 0 || attached != 0 {
            return Ok(false);
        }
        // Re-read all values from this pinned snapshot. No generation counter,
        // hash, TTL or previously reported readiness can stand in for equality.
        for table in &self.tables {
            let mut statement = connection.prepare(&table.query)?;
            if statement.column_count() != table.columns {
                return Ok(false);
            }
            let mut rows = statement.query([])?;
            for expected in &table.rows {
                let Some(row) = rows.next()? else {
                    return Ok(false);
                };
                for (column, expected) in expected.iter().enumerate() {
                    let equal = match (expected, row.get_ref(column)?) {
                        (Value::Null, ValueRef::Null) => true,
                        (Value::Integer(left), ValueRef::Integer(right)) => *left == right,
                        (Value::Real(left), ValueRef::Real(right)) => {
                            left.to_bits() == right.to_bits()
                        }
                        (Value::Text(left), ValueRef::Text(right)) => left.as_bytes() == right,
                        (Value::Blob(left), ValueRef::Blob(right)) => left.as_slice() == right,
                        _ => false,
                    };
                    if !equal {
                        return Ok(false);
                    }
                }
            }
            if rows.next()?.is_some() {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

struct ResultProof {
    value: Box<dyn Any + Send>,
    accounted_bytes: usize,
    last_used: u64,
}

#[derive(Default)]
pub(super) struct ReadCache {
    inputs: Option<Inputs>,
    results: BTreeMap<(&'static str, TypeId, Vec<u8>), ResultProof>,
    result_bytes: usize,
    use_counter: u64,
}

impl std::fmt::Debug for ReadCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReadCache")
            .field("views", &self.results.len())
            .field("result_bytes", &self.result_bytes)
            .finish_non_exhaustive()
    }
}

impl ReadCache {
    pub(super) fn clear(&mut self) {
        self.inputs = None;
        self.results.clear();
        self.result_bytes = 0;
        self.use_counter = 0;
    }

    fn next_use(&mut self) -> u64 {
        if self.use_counter == u64::MAX {
            // Only retention order changes on rollover, never proof validity.
            for proof in self.results.values_mut() {
                proof.last_used = 0;
            }
            self.use_counter = 0;
        }
        self.use_counter += 1;
        self.use_counter
    }

    fn evict_for(&mut self, bytes: usize) {
        while self.results.len() >= MAX_RESULTS
            || self.result_bytes.saturating_add(bytes) > MAX_RESULT_BYTES
        {
            let oldest = self
                .results
                .iter()
                .min_by_key(|(_, proof)| proof.last_used)
                .map(|(key, _)| key.clone());
            let Some(key) = oldest else { break };
            if let Some(proof) = self.results.remove(&key) {
                self.result_bytes -= proof.accounted_bytes;
            }
        }
    }

    /// Caller owns one fresh read snapshot and the store connection mutex.
    #[cfg(test)]
    pub(super) fn read<T: Clone + Serialize + Send + 'static>(
        &mut self,
        connection: &Connection,
        eligible: bool,
        domain: &'static str,
        validate: impl FnOnce() -> Result<T, WorkflowError>,
    ) -> Result<T, WorkflowError> {
        self.read_keyed(connection, eligible, domain, &[], validate)
    }

    pub(super) fn read_keyed<T: Clone + Serialize + Send + 'static>(
        &mut self,
        connection: &Connection,
        eligible: bool,
        domain: &'static str,
        query_key: &[u8],
        validate: impl FnOnce() -> Result<T, WorkflowError>,
    ) -> Result<T, WorkflowError> {
        let result = self.read_inner(connection, eligible, domain, query_key, validate);
        if result.is_err() {
            self.clear();
        }
        result
    }

    fn read_inner<T: Clone + Serialize + Send + 'static>(
        &mut self,
        connection: &Connection,
        eligible: bool,
        domain: &'static str,
        query_key: &[u8],
        validate: impl FnOnce() -> Result<T, WorkflowError>,
    ) -> Result<T, WorkflowError> {
        if !eligible || query_key.len() > MAX_KEY_BYTES {
            // Caller-owned transactions may include transient state or rollbacks.
            self.clear();
            return validate();
        }
        connection.query_row("SELECT COUNT(*) FROM main.sqlite_schema", [], |row| {
            row.get::<_, i64>(0)
        })?;
        let changes: i64 = connection.query_row("SELECT total_changes()", [], |row| row.get(0))?;
        let same = self
            .inputs
            .as_ref()
            .is_some_and(|inputs| inputs.matches(connection).unwrap_or(false));
        let key = (domain, TypeId::of::<T>(), query_key.to_vec());
        if same {
            // Table equality alone cannot detect index-only damage. Preserve
            // SQLite-visible corruption coverage before reusing a replay proof.
            if !self.integrity_ok(connection)? {
                return Err(super::corrupt_store());
            }
            let used = self.next_use();
            if let Some(proof) = self.results.get_mut(&key) {
                if let Some(value) = proof.value.downcast_ref::<T>() {
                    proof.last_used = used;
                    return Ok(value.clone());
                }
            }
        } else {
            self.clear();
        }
        let candidate = if same {
            None
        } else {
            Inputs::capture(connection).ok().flatten()
        };
        let value = match validate() {
            Ok(value) => value,
            Err(error) => {
                self.clear();
                return Err(error);
            }
        };
        let after: i64 = connection.query_row("SELECT total_changes()", [], |row| row.get(0))?;
        if after != changes {
            self.clear();
            return Err(super::corrupt_store());
        }
        let inputs = candidate.as_ref().or(self.inputs.as_ref());
        if inputs.is_some_and(|inputs| inputs.matches(connection).unwrap_or(false)) {
            if !self.integrity_ok(connection)? {
                return Err(super::corrupt_store());
            }
            let size = serialized_json_size(&value).ok().and_then(|bytes| {
                bytes.checked_add(size_of::<T>() + size_of::<ResultProof>() + key.2.len())
            });
            if let Some(bytes) = size.filter(|bytes| *bytes <= MAX_RESULT_BYTES) {
                if !self.results.contains_key(&key) {
                    self.evict_for(bytes);
                    let last_used = self.next_use();
                    self.results.insert(
                        key,
                        ResultProof {
                            value: Box::new(value.clone()),
                            accounted_bytes: bytes,
                            last_used,
                        },
                    );
                    self.result_bytes += bytes;
                    if let Some(candidate) = candidate {
                        self.inputs = Some(candidate);
                    }
                }
            }
        } else {
            self.clear();
        }
        Ok(value)
    }

    fn integrity_ok(&mut self, connection: &Connection) -> Result<bool, WorkflowError> {
        match connection.query_row("PRAGMA main.integrity_check", [], |row| {
            row.get::<_, String>(0)
        }) {
            Ok(result) if result == "ok" => Ok(true),
            Ok(_) => {
                self.clear();
                Ok(false)
            }
            Err(error) => {
                self.clear();
                Err(error.into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn database(path: &str) -> Connection {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
             CREATE TABLE workflow_work_items(value BLOB);
             INSERT INTO workflow_work_items VALUES(X'6f6c64')",
            )
            .unwrap();
        connection
    }

    fn value(connection: &Connection) -> Result<Vec<u8>, WorkflowError> {
        Ok(
            connection.query_row("SELECT value FROM workflow_work_items", [], |row| {
                row.get(0)
            })?,
        )
    }

    fn read(
        connection: &mut Connection,
        cache: &mut ReadCache,
        calls: &Cell<usize>,
    ) -> Result<Vec<u8>, WorkflowError> {
        let eligible = connection.is_autocommit();
        let snapshot = connection.savepoint()?;
        let result = cache.read(&snapshot, eligible, "fixture", || {
            calls.set(calls.get() + 1);
            value(&snapshot)
        })?;
        snapshot.commit()?;
        Ok(result)
    }

    #[test]
    fn exact_read_cache_reuses_only_complete_unchanged_inputs() {
        let mut connection = database(":memory:");
        let mut cache = ReadCache::default();
        let calls = Cell::new(0);
        assert_eq!(read(&mut connection, &mut cache, &calls).unwrap(), b"old");
        assert_eq!(read(&mut connection, &mut cache, &calls).unwrap(), b"old");
        assert_eq!(calls.get(), 1);
        connection
            .execute("UPDATE workflow_work_items SET value=X'6e6577'", [])
            .unwrap();
        assert_eq!(read(&mut connection, &mut cache, &calls).unwrap(), b"new");
        assert_eq!(calls.get(), 2);
        assert_eq!(read(&mut connection, &mut cache, &calls).unwrap(), b"new");
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn exact_read_cache_rechecks_external_commits_and_deletions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("store.sqlite");
        let mut connection = database(path.to_str().unwrap());
        let other = Connection::open(&path).unwrap();
        let mut cache = ReadCache::default();
        let calls = Cell::new(0);
        assert_eq!(read(&mut connection, &mut cache, &calls).unwrap(), b"old");
        other
            .execute("UPDATE workflow_work_items SET value=X'6e6577'", [])
            .unwrap();
        assert_eq!(read(&mut connection, &mut cache, &calls).unwrap(), b"new");
        assert_eq!(calls.get(), 2);
        other
            .execute("DELETE FROM workflow_work_items", [])
            .unwrap();
        assert!(read(&mut connection, &mut cache, &calls).is_err());
        assert!(cache.inputs.is_none());
        other
            .execute("INSERT INTO workflow_work_items VALUES(X'6f6c64')", [])
            .unwrap();
        assert_eq!(read(&mut connection, &mut cache, &calls).unwrap(), b"old");
        assert_eq!(calls.get(), 4);
    }

    #[test]
    fn exact_read_cache_preserves_sqlite_storage_types_and_extra_rows() {
        let mut connection = database(":memory:");
        let mut cache = ReadCache::default();
        let calls = Cell::new(0);
        read(&mut connection, &mut cache, &calls).unwrap();
        connection
            .execute("UPDATE workflow_work_items SET value='old'", [])
            .unwrap();
        assert!(read(&mut connection, &mut cache, &calls).is_err());
        assert!(cache.inputs.is_none());
        connection
            .execute("UPDATE workflow_work_items SET value=X'6f6c64'", [])
            .unwrap();
        read(&mut connection, &mut cache, &calls).unwrap();
        connection
            .execute("INSERT INTO workflow_work_items VALUES(X'6f6c64')", [])
            .unwrap();
        read(&mut connection, &mut cache, &calls).unwrap();
        assert_eq!(calls.get(), 4);
        assert_eq!(
            cache
                .inputs
                .as_ref()
                .unwrap()
                .tables
                .last()
                .unwrap()
                .rows
                .len(),
            2
        );
    }

    #[test]
    fn exact_read_cache_invalidates_schema_and_bypasses_unknown_tables() {
        let mut connection = database(":memory:");
        let mut cache = ReadCache::default();
        let calls = Cell::new(0);
        read(&mut connection, &mut cache, &calls).unwrap();
        connection
            .execute_batch("CREATE INDEX index_value ON workflow_work_items(value)")
            .unwrap();
        read(&mut connection, &mut cache, &calls).unwrap();
        assert_eq!(calls.get(), 2);
        connection
            .execute_batch("CREATE TABLE future_store(value BLOB)")
            .unwrap();
        for _ in 0..2 {
            read(&mut connection, &mut cache, &calls).unwrap();
        }
        assert_eq!(calls.get(), 4);
        assert!(cache.inputs.is_none());
    }

    #[test]
    fn exact_read_cache_bypasses_outer_transactions_and_rollback() {
        let mut connection = database(":memory:");
        let mut cache = ReadCache::default();
        let calls = Cell::new(0);
        read(&mut connection, &mut cache, &calls).unwrap();
        connection
            .execute_batch("BEGIN; UPDATE workflow_work_items SET value=X'6e6577'")
            .unwrap();
        assert_eq!(read(&mut connection, &mut cache, &calls).unwrap(), b"new");
        assert!(!connection.is_autocommit());
        assert!(cache.inputs.is_none());
        connection.execute_batch("ROLLBACK").unwrap();
        assert_eq!(read(&mut connection, &mut cache, &calls).unwrap(), b"old");
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn exact_read_cache_shares_inputs_but_separates_views_and_types() {
        let mut connection = database(":memory:");
        let mut cache = ReadCache::default();
        let calls = Cell::new(0);
        read(&mut connection, &mut cache, &calls).unwrap();
        let retained = cache.inputs.as_ref().unwrap() as *const Inputs;
        let snapshot = connection.savepoint().unwrap();
        let first: u64 = cache.read(&snapshot, true, "other-view", || Ok(7)).unwrap();
        assert_eq!(first, 7);
        assert_eq!(cache.inputs.as_ref().unwrap() as *const Inputs, retained);
        let second: u64 = cache
            .read(&snapshot, true, "other-view", || panic!("replay twice"))
            .unwrap();
        assert_eq!(second, first);
        let different_type: String = cache
            .read(&snapshot, true, "other-view", || Ok("other".to_owned()))
            .unwrap();
        assert_eq!(different_type, "other");
        assert_eq!(cache.results.len(), 3);
    }

    #[test]
    fn exact_read_cache_failure_and_validator_write_never_create_a_proof() {
        let mut connection = database(":memory:");
        let mut cache = ReadCache::default();
        let calls = Cell::new(0);
        read(&mut connection, &mut cache, &calls).unwrap();
        {
            let snapshot = connection.savepoint().unwrap();
            assert!(cache
                .read::<u64>(&snapshot, true, "failure", || Err(
                    super::super::corrupt_store()
                ))
                .is_err());
            assert!(cache.inputs.is_none());
        }
        {
            let snapshot = connection.savepoint().unwrap();
            assert!(cache
                .read(&snapshot, true, "write", || {
                    snapshot.execute("UPDATE workflow_work_items SET value=X'6e6577'", [])?;
                    Ok(7_u64)
                })
                .is_err());
            assert!(cache.inputs.is_none());
        }
        assert_eq!(value(&connection).unwrap(), b"old");
        assert!(connection.is_autocommit());
    }

    #[test]
    fn exact_read_cache_input_and_result_overflow_fall_back_without_denial() {
        let mut connection = database(":memory:");
        let mut cache = ReadCache::default();
        let calls = Cell::new(0);
        connection.execute_batch("WITH RECURSIVE rows(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM rows WHERE n<16385) INSERT INTO workflow_work_items SELECT X'6f6c64' FROM rows").unwrap();
        for _ in 0..2 {
            read(&mut connection, &mut cache, &calls).unwrap();
        }
        assert_eq!(calls.get(), 2);
        assert!(cache.inputs.is_none());
        connection
            .execute("DELETE FROM workflow_work_items WHERE rowid>1", [])
            .unwrap();
        let snapshot = connection.savepoint().unwrap();
        let large: String = cache
            .read(&snapshot, true, "oversized-result", || {
                Ok("x".repeat(MAX_RESULT_BYTES))
            })
            .unwrap();
        assert_eq!(large.len(), MAX_RESULT_BYTES);
        assert!(cache.results.is_empty());
        assert!(cache.inputs.is_none());
    }

    #[test]
    fn exact_read_cache_snapshot_ignores_later_commit_but_next_read_rechecks() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("store.sqlite");
        let mut connection = database(path.to_str().unwrap());
        let other = Connection::open(&path).unwrap();
        let mut cache = ReadCache::default();
        let calls = Cell::new(0);
        read(&mut connection, &mut cache, &calls).unwrap();
        {
            let snapshot = connection.savepoint().unwrap();
            snapshot
                .query_row("SELECT value FROM workflow_work_items", [], |row| {
                    row.get::<_, Vec<u8>>(0)
                })
                .unwrap();
            other
                .execute("UPDATE workflow_work_items SET value=X'6e6577'", [])
                .unwrap();
            assert_eq!(
                cache
                    .read::<Vec<u8>>(&snapshot, true, "fixture", || panic!("same pinned data"))
                    .unwrap(),
                b"old".to_vec()
            );
            snapshot.commit().unwrap();
        }
        assert_eq!(read(&mut connection, &mut cache, &calls).unwrap(), b"new");
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn exact_read_cache_rejects_index_only_damage_with_unchanged_table_inputs() {
        use std::io::{Seek, SeekFrom, Write};

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("store.sqlite");
        let mut connection = database(path.to_str().unwrap());
        connection.execute_batch("CREATE INDEX index_value ON workflow_work_items(value); PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
        let mut cache = ReadCache::default();
        let calls = Cell::new(0);
        read(&mut connection, &mut cache, &calls).unwrap();
        let page_size: u32 = connection
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .unwrap();
        let root: u32 = connection
            .query_row(
                "SELECT rootpage FROM sqlite_schema WHERE name='index_value'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        connection.execute_batch("PRAGMA shrink_memory").unwrap();
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(u64::from(root - 1) * u64::from(page_size)))
            .unwrap();
        file.write_all(&vec![0; page_size as usize]).unwrap();
        file.sync_all().unwrap();
        {
            let snapshot = connection.savepoint().unwrap();
            assert!(cache.inputs.as_ref().unwrap().matches(&snapshot).unwrap());
            assert!(cache
                .read::<Vec<u8>>(&snapshot, true, "fixture", || panic!(
                    "never certify damaged index"
                ))
                .is_err());
            assert!(cache.inputs.is_none());
        }
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn exact_read_cache_temp_shadowing_and_unsupported_tables_use_full_validation() {
        let mut connection = database(":memory:");
        let mut cache = ReadCache::default();
        let calls = Cell::new(0);
        read(&mut connection, &mut cache, &calls).unwrap();
        connection.execute_batch("CREATE TEMP TABLE workflow_work_items(value BLOB); INSERT INTO temp.workflow_work_items VALUES(X'6e6577')").unwrap();
        assert_eq!(read(&mut connection, &mut cache, &calls).unwrap(), b"new");
        assert!(cache.inputs.is_none());
        for schema in [
            "CREATE TABLE workflow_work_items(value BLOB PRIMARY KEY) WITHOUT ROWID",
            "CREATE TABLE workflow_work_items(rowid BLOB)",
            "CREATE TABLE workflow_work_items(value BLOB, derived BLOB GENERATED ALWAYS AS (value))",
            "CREATE VIEW workflow_work_items AS SELECT X'6f6c64' AS value",
        ] {
            let other = Connection::open_in_memory().unwrap();
            other.execute_batch(schema).unwrap();
            assert!(Inputs::capture(&other).unwrap().is_none());
        }
    }

    #[test]
    fn exact_read_cache_query_keys_isolate_tenants_projects_and_missing_rows() {
        let mut connection = database(":memory:");
        let snapshot = connection.savepoint().unwrap();
        let mut cache = ReadCache::default();
        for (key, expected) in [
            (b"tenant-a:project-a".as_slice(), Some(1_u64)),
            (b"tenant-b:project-a", Some(2)),
            (b"tenant-a:project-b", None),
        ] {
            assert_eq!(
                cache
                    .read_keyed(&snapshot, true, "project", key, || Ok(expected))
                    .unwrap(),
                expected
            );
            assert_eq!(
                cache
                    .read_keyed::<Option<u64>>(&snapshot, true, "project", key, || panic!(
                        "reuse exact query only"
                    ))
                    .unwrap(),
                expected
            );
        }
        assert_eq!(cache.results.len(), 3);
        for index in 0..MAX_RESULTS * 2 {
            let key = index.to_le_bytes();
            cache
                .read_keyed(&snapshot, true, "project", &key, || Ok(Some(index as u64)))
                .unwrap();
            assert!(cache.results.len() <= MAX_RESULTS);
            assert!(cache.result_bytes <= MAX_RESULT_BYTES);
        }
    }

    #[test]
    fn exact_read_cache_late_views_evict_least_recently_used_proofs() {
        let mut connection = database(":memory:");
        let snapshot = connection.savepoint().unwrap();
        let mut cache = ReadCache::default();
        for index in 0..MAX_RESULTS {
            cache
                .read_keyed(&snapshot, true, "project", &index.to_le_bytes(), || {
                    Ok(index)
                })
                .unwrap();
        }
        cache
            .read_keyed::<usize>(&snapshot, true, "project", &0_usize.to_le_bytes(), || {
                panic!("hot proof retained")
            })
            .unwrap();
        cache
            .read(&snapshot, true, "late-health", || Ok(42_usize))
            .unwrap();
        assert_eq!(cache.results.len(), MAX_RESULTS);
        assert!(cache.results.contains_key(&(
            "project",
            TypeId::of::<usize>(),
            0_usize.to_le_bytes().to_vec()
        )));
        assert!(!cache.results.contains_key(&(
            "project",
            TypeId::of::<usize>(),
            1_usize.to_le_bytes().to_vec()
        )));
        assert_eq!(
            cache
                .read::<usize>(&snapshot, true, "late-health", || panic!(
                    "late health is reusable"
                ))
                .unwrap(),
            42
        );
        assert_eq!(
            cache.result_bytes,
            cache
                .results
                .values()
                .map(|proof| proof.accounted_bytes)
                .sum::<usize>()
        );
    }

    #[test]
    fn exact_read_cache_byte_pressure_evicts_but_oversize_preserves_proofs() {
        let mut connection = database(":memory:");
        let snapshot = connection.savepoint().unwrap();
        let mut cache = ReadCache::default();
        cache
            .read(&snapshot, true, "large-old", || {
                Ok("a".repeat(MAX_RESULT_BYTES / 2))
            })
            .unwrap();
        cache
            .read(&snapshot, true, "hot-small", || Ok(7_u64))
            .unwrap();
        cache
            .read(&snapshot, true, "large-new", || {
                Ok("b".repeat(MAX_RESULT_BYTES / 2))
            })
            .unwrap();
        assert_eq!(cache.results.len(), 2);
        assert!(!cache
            .results
            .contains_key(&("large-old", TypeId::of::<String>(), Vec::new())));
        assert_eq!(
            cache
                .read::<u64>(&snapshot, true, "hot-small", || panic!(
                    "small proof survives byte eviction"
                ))
                .unwrap(),
            7
        );
        let before = cache.result_bytes;
        cache
            .read(&snapshot, true, "oversize", || {
                Ok("c".repeat(MAX_RESULT_BYTES))
            })
            .unwrap();
        assert_eq!(cache.result_bytes, before);
        assert_eq!(cache.results.len(), 2);
        assert_eq!(
            cache
                .read::<String>(&snapshot, true, "large-new", || panic!(
                    "oversize cannot flush useful proofs"
                ))
                .unwrap()
                .len(),
            MAX_RESULT_BYTES / 2
        );
        assert!(cache.result_bytes <= MAX_RESULT_BYTES);
    }

    #[test]
    fn exact_read_cache_rollover_and_mutation_preserve_bounded_freshness() {
        let mut connection = database(":memory:");
        let mut cache = ReadCache::default();
        {
            let snapshot = connection.savepoint().unwrap();
            cache.read(&snapshot, true, "a", || Ok(1_u64)).unwrap();
            cache.read(&snapshot, true, "b", || Ok(2_u64)).unwrap();
            cache.use_counter = u64::MAX;
            assert_eq!(
                cache
                    .read::<u64>(&snapshot, true, "a", || panic!(
                        "rollover retains exact proof"
                    ))
                    .unwrap(),
                1
            );
            assert_eq!(cache.use_counter, 1);
            snapshot.commit().unwrap();
        }
        connection
            .execute("UPDATE workflow_work_items SET value=X'6e6577'", [])
            .unwrap();
        let snapshot = connection.savepoint().unwrap();
        assert_eq!(cache.read(&snapshot, true, "a", || Ok(3_u64)).unwrap(), 3);
        assert_eq!(cache.results.len(), 1);
        assert!(!cache
            .results
            .contains_key(&("b", TypeId::of::<u64>(), Vec::new())));
        assert_eq!(
            cache.result_bytes,
            cache
                .results
                .values()
                .map(|proof| proof.accounted_bytes)
                .sum::<usize>()
        );
    }

    #[test]
    fn exact_read_cache_invalid_utf8_text_never_panics_cold_or_warm() {
        for warm in [false, true] {
            let mut connection = database(":memory:");
            let mut cache = ReadCache::default();
            if warm {
                read(&mut connection, &mut cache, &Cell::new(0)).unwrap();
            }
            connection
                .execute_batch("UPDATE workflow_work_items SET value=CAST(X'ff' AS TEXT)")
                .unwrap();
            let snapshot = connection.savepoint().unwrap();
            let calls = Cell::new(0);
            assert_eq!(
                cache
                    .read(&snapshot, true, "fixture", || {
                        calls.set(calls.get() + 1);
                        Ok(7_u64)
                    })
                    .unwrap(),
                7
            );
            assert_eq!(calls.get(), 1);
            assert!(cache.inputs.is_none());
            snapshot.commit().unwrap();
            assert!(connection.is_autocommit());
        }
    }
}
