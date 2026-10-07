use std::any::{Any, TypeId};
use std::collections::{BTreeMap, HashSet};
use std::hash::BuildHasher;
use std::mem::{align_of, size_of};
use std::sync::Arc;

use rusqlite::types::ValueRef;
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
enum InputValue {
    Null,
    Integer(i64),
    RealBits(u64),
    Text(Arc<[u8]>),
    Blob(Arc<[u8]>),
}

impl InputValue {
    fn matches(&self, value: ValueRef<'_>) -> bool {
        match (self, value) {
            (Self::Null, ValueRef::Null) => true,
            (Self::Integer(left), ValueRef::Integer(right)) => *left == right,
            (Self::RealBits(left), ValueRef::Real(right)) => *left == right.to_bits(),
            (Self::Text(left), ValueRef::Text(right))
            | (Self::Blob(left), ValueRef::Blob(right)) => left.as_ref() == right,
            _ => false,
        }
    }
}

fn charge_input(bytes: &mut usize, additional: usize) -> Option<()> {
    let next = bytes.checked_add(additional)?;
    if next > MAX_INPUT_BYTES {
        return None;
    }
    *bytes = next;
    Some(())
}

fn payload_bytes(length: usize) -> Option<usize> {
    // Arc's two reference counters and allocation-alignment slack are charged once.
    length.checked_add(2 * size_of::<usize>() + align_of::<usize>() - 1)
}

fn interner_bytes(capacity: usize) -> Option<usize> {
    if capacity == 0 {
        return Some(0);
    }
    // Budget spare hash buckets, control bytes and alignment conservatively,
    // not just occupied entries. The pool is temporary but overlaps the proof.
    capacity
        .checked_mul(2)?
        .checked_mul(size_of::<Arc<[u8]>>() + size_of::<usize>())?
        .checked_add(32)
}

fn intern_bytes<S: BuildHasher>(
    pool: &mut HashSet<Arc<[u8]>, S>,
    value: &[u8],
    bytes: &mut usize,
) -> Option<Arc<[u8]>> {
    // Hashes only locate candidates; HashSet compares the complete byte slice.
    if let Some(existing) = pool.get(value) {
        return Some(Arc::clone(existing));
    }
    let payload = payload_bytes(value.len())?;
    if bytes.checked_add(payload)? > MAX_INPUT_BYTES {
        return None;
    }
    let previous = interner_bytes(pool.capacity())?;
    if pool.len() == pool.capacity() {
        let reserve_capacity = pool
            .capacity()
            .checked_add(1)?
            .checked_next_power_of_two()?
            .checked_mul(2)?
            .max(4);
        // Include both old and new pool storage during a growth operation.
        let growth = interner_bytes(reserve_capacity)?;
        if bytes.checked_add(payload)?.checked_add(growth)? > MAX_INPUT_BYTES {
            return None;
        }
        pool.try_reserve(1).ok()?;
    }
    let growth = interner_bytes(pool.capacity())?.checked_sub(previous)?;
    charge_input(bytes, payload.checked_add(growth)?)?;
    let owned: Arc<[u8]> = Arc::from(value);
    pool.insert(Arc::clone(&owned));
    Some(owned)
}

#[derive(Debug)]
struct InputTable {
    query: String,
    columns: usize,
    rows: Vec<Vec<InputValue>>,
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
        let mut names = Vec::with_capacity(TABLES.len());
        let mut bytes = size_of::<Self>()
            + size_of::<HashSet<Arc<[u8]>>>()
            + size_of::<Vec<String>>()
            + names.capacity() * size_of::<String>();
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
            if charge_input(&mut bytes, name.capacity()).is_none() {
                return Ok(None);
            }
            names.push(name);
        }
        let mut tables = Vec::with_capacity(names.len() + 1);
        if charge_input(&mut bytes, tables.capacity() * size_of::<InputTable>()).is_none() {
            return Ok(None);
        }
        let mut pool = HashSet::new();
        let queries = std::iter::once(SCHEMA_QUERY.to_owned()).chain(
            names
                .iter()
                .map(|name| format!("SELECT rowid,* FROM main.\"{name}\" ORDER BY rowid")),
        );
        let mut count = 0_usize;
        for query in queries {
            let mut statement = connection.prepare(&query)?;
            let columns = statement.column_count();
            if charge_input(&mut bytes, query.capacity()).is_none() {
                return Ok(None);
            }
            let mut rows = statement.query([])?;
            let mut values = Vec::new();
            while let Some(row) = rows.next()? {
                count += 1;
                bytes = bytes.saturating_add(size_of::<Vec<InputValue>>());
                if count > MAX_INPUT_ROWS || bytes > MAX_INPUT_BYTES {
                    return Ok(None);
                }
                let mut cells = Vec::with_capacity(columns);
                if charge_input(&mut bytes, cells.capacity() * size_of::<InputValue>()).is_none() {
                    return Ok(None);
                }
                for column in 0..columns {
                    let value = row.get_ref(column)?;
                    // SQLite TEXT need not be valid UTF-8. Optional proof capture
                    // must never panic on corrupt input or poison the store lock.
                    let owned = match value {
                        ValueRef::Null => InputValue::Null,
                        ValueRef::Integer(value) => InputValue::Integer(value),
                        ValueRef::Real(value) => InputValue::RealBits(value.to_bits()),
                        ValueRef::Text(value) => {
                            if std::str::from_utf8(value).is_err() {
                                return Ok(None);
                            }
                            let Some(value) = intern_bytes(&mut pool, value, &mut bytes) else {
                                return Ok(None);
                            };
                            InputValue::Text(value)
                        }
                        ValueRef::Blob(value) => {
                            let Some(value) = intern_bytes(&mut pool, value, &mut bytes) else {
                                return Ok(None);
                            };
                            InputValue::Blob(value)
                        }
                    };
                    cells.push(owned);
                }
                let previous_capacity = values.capacity();
                values.push(cells);
                bytes = bytes.saturating_add(
                    (values.capacity() - previous_capacity) * size_of::<Vec<InputValue>>(),
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
                    if !expected.matches(row.get_ref(column)?) {
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

    #[cfg(test)]
    pub(super) fn read_keyed<T: Clone + Serialize + Send + 'static>(
        &mut self,
        connection: &Connection,
        eligible: bool,
        domain: &'static str,
        query_key: &[u8],
        validate: impl FnOnce() -> Result<T, WorkflowError>,
    ) -> Result<T, WorkflowError> {
        self.read_keyed_if(connection, eligible, domain, query_key, validate, |_| true)
    }

    pub(super) fn read_keyed_if<T: Clone + Serialize + Send + 'static>(
        &mut self,
        connection: &Connection,
        eligible: bool,
        domain: &'static str,
        query_key: &[u8],
        validate: impl FnOnce() -> Result<T, WorkflowError>,
        retain: impl Fn(&T) -> bool,
    ) -> Result<T, WorkflowError> {
        let result = self.read_inner(connection, eligible, domain, query_key, validate, retain);
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
        retain: impl Fn(&T) -> bool,
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
            if !retain(&value) {
                self.clear();
                return Ok(value);
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
    use std::hash::{BuildHasherDefault, Hasher};
    use std::mem::size_of_val;

    #[derive(Default)]
    struct CollisionHasher;

    impl Hasher for CollisionHasher {
        fn finish(&self) -> u64 {
            0
        }

        fn write(&mut self, _: &[u8]) {}
    }

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
        connection.execute_batch("INSERT INTO workflow_work_items SELECT value FROM workflow_work_items;
            CREATE INDEX index_value ON workflow_work_items(value); PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
        let mut cache = ReadCache::default();
        let calls = Cell::new(0);
        read(&mut connection, &mut cache, &calls).unwrap();
        let table = cache.inputs.as_ref().unwrap().tables.last().unwrap();
        let (InputValue::Blob(first), InputValue::Blob(second)) =
            (&table.rows[0][1], &table.rows[1][1])
        else {
            panic!("interned index fixture");
        };
        assert!(Arc::ptr_eq(first, second));
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
    fn exact_read_cache_deferred_failures_are_not_retained_as_completed_proofs() {
        let mut connection = database(":memory:");
        let snapshot = connection.savepoint().unwrap();
        let mut cache = ReadCache::default();
        let validations = Cell::new(0);
        cache.read(&snapshot, true, "valid", || Ok(1_u64)).unwrap();
        assert_eq!(cache.results.len(), 1);
        for expected in 1..=2 {
            let value = cache
                .read_keyed_if(
                    &snapshot,
                    true,
                    "deferred",
                    b"scope",
                    || {
                        validations.set(validations.get() + 1);
                        Ok((42_u64, false))
                    },
                    |value| value.1,
                )
                .unwrap();
            assert_eq!(value, (42, false));
            assert_eq!(validations.get(), expected);
            assert!(cache.results.is_empty());
            assert!(cache.inputs.is_none());
        }
        assert!(!snapshot.is_autocommit());
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
    fn exact_input_values_keep_storage_types_and_real_bits() {
        let bytes: Arc<[u8]> = Arc::from(b"same".as_slice());
        let text = InputValue::Text(Arc::clone(&bytes));
        let blob = InputValue::Blob(bytes);
        assert!(text.matches(ValueRef::Text(b"same")));
        assert!(blob.matches(ValueRef::Blob(b"same")));
        assert!(!text.matches(ValueRef::Blob(b"same")));
        assert!(!blob.matches(ValueRef::Text(b"same")));
        assert!(!text.matches(ValueRef::Text(b"diff")));
        assert!(InputValue::Null.matches(ValueRef::Null));
        assert!(!InputValue::Null.matches(ValueRef::Integer(0)));
        assert!(InputValue::Integer(1).matches(ValueRef::Integer(1)));
        assert!(!InputValue::Integer(1).matches(ValueRef::Real(1.0)));
        assert!(InputValue::RealBits(1.0_f64.to_bits()).matches(ValueRef::Real(1.0)));
        assert!(!InputValue::RealBits(1.0_f64.to_bits()).matches(ValueRef::Integer(1)));
        assert!(!InputValue::RealBits(0.0_f64.to_bits()).matches(ValueRef::Real(-0.0)));
        let nan = f64::from_bits(0x7ff8_0000_0000_0001);
        assert!(InputValue::RealBits(nan.to_bits()).matches(ValueRef::Real(nan)));
        assert!(!InputValue::RealBits(nan.to_bits())
            .matches(ValueRef::Real(f64::from_bits(0x7ff8_0000_0000_0002))));
    }

    #[test]
    fn exact_input_interner_collisions_compare_all_bytes_and_charge_once() {
        let mut pool = HashSet::<Arc<[u8]>, BuildHasherDefault<CollisionHasher>>::default();
        let mut bytes = size_of_val(&pool);
        let first = intern_bytes(&mut pool, b"same", &mut bytes).unwrap();
        let second = intern_bytes(&mut pool, b"diff", &mut bytes).unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(first.as_ref(), b"same");
        assert_eq!(second.as_ref(), b"diff");
        assert_eq!(pool.len(), 2);
        assert_eq!(
            bytes,
            size_of_val(&pool)
                + 2 * payload_bytes(4).unwrap()
                + interner_bytes(pool.capacity()).unwrap()
        );
        let before = bytes;
        let duplicate = intern_bytes(&mut pool, b"same", &mut bytes).unwrap();
        assert!(Arc::ptr_eq(&first, &duplicate));
        assert_eq!(bytes, before);
    }

    #[test]
    fn exact_input_interner_accounts_headers_capacity_and_boundary() {
        let mut pool = HashSet::new();
        let mut bytes = 0;
        let first = intern_bytes(&mut pool, b"seed", &mut bytes).unwrap();
        assert_eq!(
            bytes,
            payload_bytes(4).unwrap() + interner_bytes(pool.capacity()).unwrap()
        );
        assert!(
            interner_bytes(pool.capacity()).unwrap() > pool.capacity() * size_of::<Arc<[u8]>>()
        );
        bytes = MAX_INPUT_BYTES;
        assert!(Arc::ptr_eq(
            &first,
            &intern_bytes(&mut pool, b"seed", &mut bytes).unwrap()
        ));
        assert!(intern_bytes(&mut pool, b"next", &mut bytes).is_none());
        assert_eq!(pool.len(), 1);
        bytes = MAX_INPUT_BYTES - payload_bytes(4).unwrap();
        assert!(intern_bytes(&mut pool, b"next", &mut bytes).is_some());
        assert_eq!(bytes, MAX_INPUT_BYTES);
        assert_eq!(pool.len(), 2);

        let mut empty = HashSet::new();
        let mut only_payload = MAX_INPUT_BYTES - payload_bytes(4).unwrap();
        assert!(intern_bytes(&mut empty, b"seed", &mut only_payload).is_none());
        assert!(empty.is_empty());
        assert_eq!(empty.capacity(), 0);
        assert!(payload_bytes(usize::MAX).is_none());
        assert!(interner_bytes(usize::MAX).is_none());
        assert!(charge_input(&mut bytes, usize::MAX).is_none());
        assert_eq!(bytes, MAX_INPUT_BYTES);

        let mut growing = HashSet::new();
        let mut used = 0;
        intern_bytes(&mut growing, b"first", &mut used).unwrap();
        let capacity = growing.capacity();
        for index in 1..capacity {
            intern_bytes(&mut growing, &index.to_le_bytes(), &mut used).unwrap();
        }
        assert_eq!(growing.len(), capacity);
        used = MAX_INPUT_BYTES - payload_bytes(4).unwrap();
        assert!(intern_bytes(&mut growing, b"next", &mut used).is_none());
        assert_eq!(growing.capacity(), capacity);
        assert_eq!(growing.len(), capacity);
    }

    #[test]
    fn exact_input_capture_shares_payloads_not_types_rows_or_schema_keys() {
        let connection = database(":memory:");
        connection
            .execute_batch("INSERT INTO workflow_work_items VALUES('old'),(X'6f6c64')")
            .unwrap();
        let inputs = Inputs::capture(&connection).unwrap().unwrap();
        let table = inputs.tables.last().unwrap();
        assert_eq!(table.rows.len(), 3);
        let InputValue::Blob(first) = &table.rows[0][1] else {
            panic!("blob input");
        };
        let InputValue::Text(second) = &table.rows[1][1] else {
            panic!("text input");
        };
        let InputValue::Blob(third) = &table.rows[2][1] else {
            panic!("duplicate blob input");
        };
        assert!(Arc::ptr_eq(first, second));
        assert!(Arc::ptr_eq(first, third));
        assert!(inputs.matches(&connection).unwrap());
        connection
            .execute("UPDATE workflow_work_items SET rowid=4 WHERE rowid=3", [])
            .unwrap();
        assert!(!inputs.matches(&connection).unwrap());
        let inputs = Inputs::capture(&connection).unwrap().unwrap();
        connection
            .execute_batch("CREATE INDEX index_value ON workflow_work_items(value)")
            .unwrap();
        assert!(!inputs.matches(&connection).unwrap());
    }

    #[test]
    fn exact_read_cache_repeated_large_payloads_remain_eligible_without_raising_cap() {
        let mut connection = database(":memory:");
        let payload_size = MAX_INPUT_BYTES / 64;
        connection
            .execute(
                "WITH RECURSIVE rows(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM rows WHERE n<65)
                 INSERT INTO workflow_work_items SELECT zeroblob(?1) FROM rows",
                [i64::try_from(payload_size).unwrap()],
            )
            .unwrap();
        assert!(65 * payload_size > MAX_INPUT_BYTES);
        let mut cache = ReadCache::default();
        let calls = Cell::new(0);
        for _ in 0..2 {
            let snapshot = connection.savepoint().unwrap();
            let count: i64 = cache
                .read(&snapshot, true, "row-count", || {
                    calls.set(calls.get() + 1);
                    Ok(snapshot.query_row(
                        "SELECT COUNT(*) FROM workflow_work_items",
                        [],
                        |row| row.get(0),
                    )?)
                })
                .unwrap();
            assert_eq!(count, 66);
            assert!(cache.inputs.as_ref().unwrap().matches(&snapshot).unwrap());
            snapshot.commit().unwrap();
        }
        assert_eq!(calls.get(), 1);
        let rows = &cache.inputs.as_ref().unwrap().tables.last().unwrap().rows;
        let InputValue::Blob(first) = &rows[1][1] else {
            panic!("large blob input");
        };
        assert_eq!(first.len(), payload_size);
        assert!(rows[1..].iter().all(|row| {
            matches!(&row[1], InputValue::Blob(value) if Arc::ptr_eq(first, value))
        }));
    }

    #[test]
    fn exact_read_cache_unique_large_payloads_still_fall_back_at_unchanged_cap() {
        let mut connection = database(":memory:");
        let mut payload = vec![0_u8; MAX_INPUT_BYTES / 64];
        {
            let mut insert = connection
                .prepare("INSERT INTO workflow_work_items VALUES(?1)")
                .unwrap();
            for index in 0..65 {
                payload[0] = index;
                insert.execute([payload.as_slice()]).unwrap();
            }
        }
        let mut cache = ReadCache::default();
        let calls = Cell::new(0);
        for _ in 0..2 {
            let snapshot = connection.savepoint().unwrap();
            assert_eq!(
                cache
                    .read(&snapshot, true, "overbound", || {
                        calls.set(calls.get() + 1);
                        Ok(66_u64)
                    })
                    .unwrap(),
                66
            );
            assert!(cache.inputs.is_none());
            assert!(cache.results.is_empty());
            snapshot.commit().unwrap();
        }
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn exact_input_capture_charges_metadata_even_when_payload_alone_fits() {
        let connection = database(":memory:");
        connection
            .execute(
                "UPDATE workflow_work_items SET value=zeroblob(?1)",
                [i64::try_from(MAX_INPUT_BYTES - 1).unwrap()],
            )
            .unwrap();
        assert!(Inputs::capture(&connection).unwrap().is_none());
    }

    #[test]
    fn exact_input_capture_rejects_invalid_text_even_when_blob_bytes_are_interned() {
        let connection = database(":memory:");
        connection
            .execute_batch("INSERT INTO workflow_work_items VALUES(X'ff')")
            .unwrap();
        assert!(Inputs::capture(&connection).unwrap().is_some());
        connection
            .execute_batch("INSERT INTO workflow_work_items VALUES(CAST(X'ff' AS TEXT))")
            .unwrap();
        assert!(Inputs::capture(&connection).unwrap().is_none());
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
