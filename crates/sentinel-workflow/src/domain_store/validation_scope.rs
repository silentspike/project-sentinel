use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::BTreeMap;

use rusqlite::Connection;
use serde::Serialize;

use super::{corrupt, persistence};
use crate::digest::serialized_json_size;
use crate::WorkflowError;

const MAX_NODES: usize = 16_384;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_DEPTH: usize = 64;
const MAX_INVENTORY_BYTES: usize = 512 * 1024;
const MAX_INVENTORY_NODES: usize = 64;
const SAVEPOINT: &str = "sentinel_validation_scope";

type Key = (&'static str, TypeId, Vec<u8>);

enum Entry {
    Visiting,
    Complete(Box<dyn Any>),
}

struct State {
    connection: usize,
    changes: i64,
    entries: BTreeMap<Key, Entry>,
    inventories: BTreeMap<Key, Entry>,
    inventory_bytes: usize,
    inventory_depth: usize,
    nodes: usize,
    bytes: usize,
    depth: usize,
    poisoned: bool,
    reuse: bool,
    #[cfg(test)]
    validations: BTreeMap<&'static str, usize>,
}

thread_local! {
    // This slot exists only while its owner's SQLite savepoint is open.
    static ACTIVE: RefCell<Option<State>> = const { RefCell::new(None) };
    #[cfg(test)]
    static COMPLETED: RefCell<Option<Vec<BTreeMap<&'static str, usize>>>> = const { RefCell::new(None) };
}

fn connection_id(connection: &Connection) -> usize {
    connection as *const Connection as usize
}

fn changes(connection: &Connection) -> Result<i64, WorkflowError> {
    Ok(connection.query_row("SELECT total_changes()", [], |row| row.get(0))?)
}

fn synchronize(connection: &Connection) -> Result<(), WorkflowError> {
    let current = changes(connection)?;
    ACTIVE.with(|active| {
        let mut active = active.borrow_mut();
        let state = active.as_mut().ok_or_else(corrupt)?;
        if state.connection != connection_id(connection) || state.poisoned {
            return Err(corrupt());
        }
        if state.changes != current {
            state.entries.clear();
            state.inventories.clear();
            state.inventory_bytes = 0;
            state.changes = current;
            state.reuse = false;
            // Validators are read-only. A write during one cannot produce a reusable proof.
            if state.depth != 0 || state.inventory_depth != 0 {
                state.poisoned = true;
                return Err(corrupt());
            }
        }
        Ok(())
    })
}

pub(crate) struct Scope<'a> {
    connection: &'a Connection,
    owner: bool,
}

impl Scope<'_> {
    pub(crate) fn finish(mut self) -> Result<(), WorkflowError> {
        if self.owner {
            synchronize(self.connection)?;
            self.connection
                .execute_batch("RELEASE sentinel_validation_scope")?;
            #[cfg(test)]
            COMPLETED.with(|completed| {
                if let Some(scopes) = completed.borrow_mut().as_mut() {
                    ACTIVE.with(|active| {
                        scopes.push(active.borrow().as_ref().unwrap().validations.clone());
                    });
                }
            });
            ACTIVE.with(|active| *active.borrow_mut() = None);
            self.owner = false;
        }
        Ok(())
    }
}

impl Drop for Scope<'_> {
    fn drop(&mut self) {
        if self.owner {
            ACTIVE.with(|active| *active.borrow_mut() = None);
            // Error/unwind cleanup rolls back only this scope, never the caller's transaction.
            let _ = self.connection.execute_batch(
                "ROLLBACK TO sentinel_validation_scope; RELEASE sentinel_validation_scope",
            );
        }
    }
}

pub(crate) fn enter(connection: &Connection) -> Result<Scope<'_>, WorkflowError> {
    let nested = ACTIVE.with(|active| active.borrow().as_ref().map(|state| state.connection));
    if let Some(id) = nested {
        if id != connection_id(connection) {
            return Err(corrupt());
        }
        synchronize(connection)?;
        return Ok(Scope {
            connection,
            owner: false,
        });
    }
    connection.execute_batch(&format!("SAVEPOINT {SAVEPOINT}"))?;
    let scope = Scope {
        connection,
        owner: true,
    };
    // Force a main-database read now: SAVEPOINT alone does not acquire a read snapshot.
    connection.query_row("SELECT COUNT(*) FROM sqlite_schema", [], |row| {
        row.get::<_, i64>(0)
    })?;
    ACTIVE.with(|active| {
        *active.borrow_mut() = Some(State {
            connection: connection_id(connection),
            changes: 0,
            entries: BTreeMap::new(),
            inventories: BTreeMap::new(),
            inventory_bytes: 0,
            inventory_depth: 0,
            nodes: 0,
            bytes: 0,
            depth: 0,
            poisoned: false,
            reuse: true,
            #[cfg(test)]
            validations: BTreeMap::new(),
        });
    });
    let initial_changes = changes(connection)?;
    ACTIVE.with(|active| active.borrow_mut().as_mut().unwrap().changes = initial_changes);
    Ok(scope)
}

pub(crate) fn with_scope<T>(
    connection: &Connection,
    action: impl FnOnce() -> Result<T, WorkflowError>,
) -> Result<T, WorkflowError> {
    let scope = enter(connection)?;
    let result = action()?;
    scope.finish()?;
    Ok(result)
}

pub(crate) fn charge_bytes(connection: &Connection, bytes: usize) -> Result<(), WorkflowError> {
    ACTIVE.with(|active| {
        let mut active = active.borrow_mut();
        let state = active.as_mut().ok_or_else(corrupt)?;
        if state.connection != connection_id(connection) || state.poisoned {
            return Err(corrupt());
        }
        state.bytes = state.bytes.checked_add(bytes).ok_or_else(corrupt)?;
        if state.bytes > MAX_BYTES {
            state.poisoned = true;
            return Err(corrupt());
        }
        Ok(())
    })
}

// Advisory discovery is not a proof. Its bounded storage must never consume
// the mandatory replay budget or turn optional overflow into corruption.
pub(crate) fn inventory<T: Clone + Serialize + 'static>(
    connection: &Connection,
    domain: &'static str,
    input: &impl Serialize,
    discover: impl FnOnce() -> Result<Option<T>, WorkflowError>,
) -> Result<Option<T>, WorkflowError> {
    with_scope(connection, || {
        synchronize(connection)?;
        let key = (
            domain,
            TypeId::of::<T>(),
            serde_json::to_vec(input).map_err(|_| persistence())?,
        );
        let key_cost = key.2.len().saturating_add(256);
        let admission = ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            let state = active.as_mut().ok_or_else(corrupt)?;
            match state.inventories.get(&key) {
                Some(Entry::Visiting) => {
                    state.poisoned = true;
                    return Err(corrupt());
                }
                Some(Entry::Complete(value)) => {
                    return value
                        .downcast_ref::<Option<T>>()
                        .cloned()
                        .map(|value| (Some(value), None))
                        .ok_or_else(corrupt);
                }
                None => {}
            }
            let capacity = MAX_INVENTORY_BYTES.saturating_sub(state.inventory_bytes);
            if !state.reuse
                || state.inventories.len() >= MAX_INVENTORY_NODES
                || state.inventory_depth >= MAX_DEPTH
                || key_cost.saturating_add(4) > capacity
            {
                return Ok((Some(None), None));
            }
            state.inventory_bytes += key_cost;
            state.inventory_depth += 1;
            state.inventories.insert(key.clone(), Entry::Visiting);
            #[cfg(test)]
            {
                *state.validations.entry(domain).or_default() += 1;
            }
            Ok((None, Some(capacity - key_cost)))
        })?;
        if let (Some(value), _) = admission {
            return Ok(value);
        }
        let capacity = admission.1.ok_or_else(corrupt)?;
        let result = (|| {
            let value = discover()?;
            synchronize(connection)?;
            let encoded_size = serialized_json_size(&value).map_err(|_| persistence())?;
            if encoded_size > capacity {
                Ok((None, 4))
            } else {
                Ok((value, encoded_size))
            }
        })();
        let retained = ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            let state = active.as_mut().ok_or_else(corrupt)?;
            state.inventory_depth = state.inventory_depth.saturating_sub(1);
            match &result {
                Ok((value, bytes))
                    if !state.poisoned
                        && state.reuse
                        && state.inventory_bytes.saturating_add(*bytes) <= MAX_INVENTORY_BYTES =>
                {
                    state.inventory_bytes += bytes;
                    state
                        .inventories
                        .insert(key.clone(), Entry::Complete(Box::new(value.clone())));
                    Ok::<_, WorkflowError>(true)
                }
                _ => {
                    state.inventories.remove(&key);
                    state.inventory_bytes = state.inventory_bytes.saturating_sub(key_cost);
                    Ok(false)
                }
            }
        })?;
        result.map(|(value, _)| if retained { value } else { None })
    })
}

pub(crate) fn before_write(connection: &Connection) -> Result<(), WorkflowError> {
    ACTIVE.with(|active| {
        let mut active = active.borrow_mut();
        let Some(state) = active.as_mut() else {
            return Ok(());
        };
        if state.connection != connection_id(connection) || state.poisoned {
            return Err(corrupt());
        }
        state.entries.clear();
        state.inventories.clear();
        state.inventory_bytes = 0;
        state.reuse = false;
        if state.depth != 0 || state.inventory_depth != 0 {
            state.poisoned = true;
            return Err(corrupt());
        }
        Ok(())
    })
}

pub(crate) fn memoize<T: Clone + Serialize + 'static>(
    connection: &Connection,
    domain: &'static str,
    input: &impl Serialize,
    validate: impl FnOnce() -> Result<T, WorkflowError>,
) -> Result<T, WorkflowError> {
    with_scope(connection, || {
        synchronize(connection)?;
        let input = serde_json::to_vec(input).map_err(|_| persistence())?;
        let key = (domain, TypeId::of::<T>(), input);
        let cached = ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            let state = active.as_mut().ok_or_else(corrupt)?;
            match state.entries.get(&key) {
                Some(Entry::Visiting) => {
                    state.poisoned = true;
                    return Err(corrupt());
                }
                Some(Entry::Complete(value)) => {
                    return value
                        .downcast_ref::<T>()
                        .cloned()
                        .map(Some)
                        .ok_or_else(corrupt);
                }
                None => {}
            }
            if state.nodes >= MAX_NODES || state.depth >= MAX_DEPTH {
                state.poisoned = true;
                return Err(corrupt());
            }
            state.nodes += 1;
            state.depth += 1;
            #[cfg(test)]
            {
                *state.validations.entry(domain).or_default() += 1;
            }
            state.entries.insert(key.clone(), Entry::Visiting);
            Ok(None)
        })?;
        if let Some(value) = cached {
            return Ok(value);
        }
        let result = (|| {
            // Charge exact keys and serialized results, plus per-node bookkeeping.
            charge_bytes(connection, key.2.len().saturating_add(256))?;
            let value = validate()?;
            synchronize(connection)?;
            let bytes = serialized_json_size(&value).map_err(|_| persistence())?;
            charge_bytes(connection, bytes)?;
            Ok(value)
        })();
        ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            let state = active.as_mut().ok_or_else(corrupt)?;
            state.depth = state.depth.saturating_sub(1);
            match &result {
                Ok(value) if !state.poisoned && state.reuse => {
                    state
                        .entries
                        .insert(key.clone(), Entry::Complete(Box::new(value.clone())));
                }
                _ => {
                    state.entries.remove(&key);
                }
            }
            Ok::<_, WorkflowError>(())
        })?;
        result
    })
}

#[cfg(test)]
pub(crate) fn with_completed_validations<T>(
    action: impl FnOnce() -> T,
) -> (T, Vec<BTreeMap<&'static str, usize>>) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            COMPLETED.with(|completed| *completed.borrow_mut() = None);
        }
    }

    assert!(ACTIVE.with(|active| active.borrow().is_none()));
    COMPLETED.with(|completed| {
        assert!(completed.borrow().is_none());
        *completed.borrow_mut() = Some(Vec::new());
    });
    let _reset = Reset;
    let result = action();
    let scopes = COMPLETED.with(|completed| completed.borrow_mut().take().unwrap());
    (result, scopes)
}

#[cfg(test)]
pub(crate) fn validations(domain: &'static str) -> usize {
    ACTIVE.with(|active| {
        active
            .borrow()
            .as_ref()
            .and_then(|state| state.validations.get(domain))
            .copied()
            .unwrap_or(0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn database() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE records(id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO records VALUES(1, 1)").unwrap();
        connection
    }

    fn value(connection: &Connection) -> Result<i64, WorkflowError> {
        memoize(connection, "record", &1, || {
            Ok(connection
                .query_row("SELECT value FROM records WHERE id=1", [], |row| row.get(0))?)
        })
    }

    #[test]
    fn optional_inventory_does_not_consume_near_limit_mandatory_proof_capacity() {
        for accelerated in [false, true] {
            let connection = database();
            with_scope(&connection, || {
                charge_bytes(&connection, MAX_BYTES - 4096)?;
                ACTIVE.with(|active| active.borrow_mut().as_mut().unwrap().nodes = MAX_NODES - 3);
                if accelerated {
                    let first =
                        inventory(&connection, "optional", &1, || Ok(Some("x".repeat(3072))))?;
                    assert_eq!(first.as_ref().unwrap().len(), 3072);
                    assert_eq!(
                        inventory(&connection, "optional", &1, || panic!("must reuse"))?,
                        first
                    );
                    ACTIVE.with(|active| {
                        let state = active.borrow();
                        let state = state.as_ref().unwrap();
                        assert_eq!(state.nodes, MAX_NODES - 3);
                        assert_eq!(state.bytes, MAX_BYTES - 4096);
                    });
                }
                charge_bytes(&connection, 3000)?;
                for key in 0..3 {
                    assert_eq!(memoize(&connection, "mandatory", &key, || Ok(1))?, 1);
                }
                Ok(())
            })
            .unwrap();
        }
    }

    #[test]
    fn optional_inventory_overflow_cycles_and_writes_preserve_scope_contracts() {
        let connection = database();
        with_scope(&connection, || {
            let oversized = inventory(&connection, "oversized", &1, || {
                Ok(Some("x".repeat(MAX_INVENTORY_BYTES)))
            })?;
            assert!(oversized.is_none());
            assert_eq!(value(&connection)?, 1);
            before_write(&connection)?;
            connection.execute("UPDATE records SET value=2 WHERE id=1", [])?;
            assert!(inventory::<i64>(&connection, "after-write", &1, || panic!(
                "no discovery after write"
            ))?
            .is_none());
            assert_eq!(value(&connection)?, 2);
            Ok(())
        })
        .unwrap();
        assert!(inventory::<i64>(&connection, "cycle", &1, || inventory(
            &connection,
            "cycle",
            &1,
            || Ok(Some(1))
        ))
        .is_err());
        assert!(inventory::<i64>(&connection, "write", &1, || {
            connection.execute("UPDATE records SET value=3 WHERE id=1", [])?;
            Ok(Some(3))
        })
        .is_err());
        assert_eq!(value(&connection).unwrap(), 2);
        assert!(connection.is_autocommit());
    }

    #[test]
    fn optional_inventory_nested_overflow_returns_fallback_and_retains_child() {
        let connection = database();
        with_scope(&connection, || {
            let outer = inventory(&connection, "outer", &1, || {
                assert!(inventory(&connection, "child", &1, || Ok(Some(
                    "x".repeat(400 * 1024)
                )))?
                .is_some());
                Ok(Some("y".repeat(200 * 1024)))
            })?;
            assert!(outer.is_none());
            let child: Option<String> =
                inventory(&connection, "child", &1, || panic!("retain child"))?;
            assert_eq!(child.unwrap().len(), 400 * 1024);
            ACTIVE.with(|active| {
                let state = active.borrow();
                let state = state.as_ref().unwrap();
                assert!(state.inventory_bytes <= MAX_INVENTORY_BYTES);
                assert_eq!(state.inventories.len(), 1);
                assert_eq!(state.inventory_depth, 0);
                assert_eq!(state.bytes, 0);
            });
            assert_eq!(value(&connection)?, 1);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn validation_scope_completed_counters_capture_owners_without_retaining_proofs() {
        let connection = database();
        let (_, scopes) = with_completed_validations(|| {
            for _ in 0..2 {
                with_scope(&connection, || {
                    assert_eq!(value(&connection)?, 1);
                    assert_eq!(value(&connection)?, 1);
                    Ok(())
                })
                .unwrap();
            }
        });
        assert_eq!(scopes.len(), 2);
        assert!(scopes.iter().all(|scope| scope.get("record") == Some(&1)));
        assert_eq!(validations("record"), 0);
        let (failed, scopes) =
            with_completed_validations(|| with_scope::<()>(&connection, || Err(corrupt())));
        assert!(failed.is_err());
        assert!(scopes.is_empty());
        assert!(connection.is_autocommit());
        assert_eq!(value(&connection).unwrap(), 1);
        assert!(COMPLETED.with(|completed| completed.borrow().is_none()));
    }

    #[test]
    fn validation_scope_reuses_only_completed_nodes_in_one_operation() {
        let connection = database();
        with_scope(&connection, || {
            assert_eq!(value(&connection)?, 1);
            assert_eq!(value(&connection)?, 1);
            assert_eq!(validations("record"), 1);
            Ok(())
        })
        .unwrap();
        with_scope(&connection, || {
            assert_eq!(value(&connection)?, 1);
            assert_eq!(validations("record"), 1);
            Ok(())
        })
        .unwrap();
        assert!(connection.is_autocommit());
        assert_eq!(validations("record"), 0);
    }

    #[test]
    fn validation_scope_external_writer_cannot_change_the_owned_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("snapshot.sqlite");
        let reader = Connection::open(&path).unwrap();
        reader.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE records(id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO records VALUES(1,1)").unwrap();
        let writer = Connection::open(&path).unwrap();
        with_scope(&reader, || {
            assert_eq!(value(&reader)?, 1);
            writer
                .execute("UPDATE records SET value=2 WHERE id=1", [])
                .unwrap();
            assert_eq!(value(&reader)?, 1);
            assert_eq!(
                reader
                    .query_row("SELECT value FROM records", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                1
            );
            assert_eq!(validations("record"), 1);
            Ok(())
        })
        .unwrap();
        assert_eq!(value(&reader).unwrap(), 2);
    }

    #[test]
    fn validation_scope_same_connection_write_discards_pre_write_proofs() {
        let connection = database();
        with_scope(&connection, || {
            assert_eq!(value(&connection)?, 1);
            before_write(&connection)?;
            connection.execute("UPDATE records SET value=2 WHERE id=1", [])?;
            assert_eq!(value(&connection)?, 2);
            assert_eq!(value(&connection)?, 2);
            // Reuse stays disabled after a write until a new scope is acquired.
            assert_eq!(validations("record"), 3);
            Ok(())
        })
        .unwrap();
        assert_eq!(value(&connection).unwrap(), 2);
    }

    #[test]
    fn validation_scope_detects_unannounced_writes_and_revalidates() {
        let connection = database();
        with_scope(&connection, || {
            assert_eq!(value(&connection)?, 1);
            connection.execute("UPDATE records SET value=3 WHERE id=1", [])?;
            assert_eq!(value(&connection)?, 3);
            assert_eq!(validations("record"), 2);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn validation_scope_never_treats_visiting_as_validated() {
        let connection = database();
        let result = memoize::<i64>(&connection, "cycle", &1, || {
            memoize(&connection, "cycle", &1, || Ok(1))
        });
        assert!(result.is_err());
        assert!(connection.is_autocommit());
        assert_eq!(value(&connection).unwrap(), 1);
    }

    #[test]
    fn validation_scope_failed_validation_is_not_cached() {
        let connection = database();
        let attempts = Cell::new(0);
        with_scope(&connection, || {
            for _ in 0..2 {
                assert!(memoize::<i64>(&connection, "failure", &1, || {
                    attempts.set(attempts.get() + 1);
                    Err(corrupt())
                })
                .is_err());
            }
            assert_eq!(attempts.get(), 2);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn validation_scope_write_inside_validator_rolls_back_and_never_certifies() {
        let connection = database();
        let result = memoize(&connection, "illegal-write", &1, || {
            connection.execute("UPDATE records SET value=4 WHERE id=1", [])?;
            Ok(4_i64)
        });
        assert!(result.is_err());
        assert_eq!(value(&connection).unwrap(), 1);
    }

    #[test]
    fn validation_scope_failure_preserves_the_callers_outer_transaction() {
        let mut connection = database();
        let transaction = connection.transaction().unwrap();
        transaction
            .execute("UPDATE records SET value=5 WHERE id=1", [])
            .unwrap();
        assert!(with_scope::<()>(&transaction, || Err(corrupt())).is_err());
        assert_eq!(value(&transaction).unwrap(), 5);
        transaction.commit().unwrap();
        assert_eq!(value(&connection).unwrap(), 5);
    }

    #[test]
    fn validation_scope_distinct_connection_cannot_borrow_another_proof() {
        let first = database();
        let second = database();
        with_scope(&first, || {
            assert!(value(&second).is_err());
            assert_eq!(value(&first)?, 1);
            Ok(())
        })
        .unwrap();
    }

    fn recurse(connection: &Connection, depth: usize) -> Result<i64, WorkflowError> {
        memoize(connection, "depth", &depth, || {
            if depth == 0 {
                Ok(0)
            } else {
                recurse(connection, depth - 1)
            }
        })
    }

    #[test]
    fn validation_scope_depth_nodes_and_bytes_are_bounded() {
        let connection = database();
        assert!(recurse(&connection, MAX_DEPTH).is_err());
        assert!(with_scope(&connection, || {
            for id in 0..=MAX_NODES {
                memoize(&connection, "many", &id, || Ok(id))?;
            }
            Ok(())
        })
        .is_err());
        assert!(with_scope(&connection, || charge_bytes(&connection, MAX_BYTES + 1)).is_err());
        assert_eq!(value(&connection).unwrap(), 1);
    }
}
