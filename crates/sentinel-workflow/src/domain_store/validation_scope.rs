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

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ValidationFailureReason {
    BytesLimit,
    BytesOverflow,
    NodeLimit,
    DepthLimit,
    Visiting,
    InventoryVisiting,
    Poisoned,
    ConnectionMismatch,
    MissingScope,
    WriteDuringValidation,
    CacheTypeMismatch,
    ValidationError,
    ScopeError,
    Unwind,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ValidationChargePhase {
    Key,
    Body,
    Result,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ValidationChargeTotals {
    pub(crate) key: usize,
    pub(crate) body: usize,
    pub(crate) result: usize,
}

#[cfg(test)]
impl ValidationChargeTotals {
    fn add(&mut self, phase: ValidationChargePhase, bytes: usize) {
        let counter = match phase {
            ValidationChargePhase::Key => &mut self.key,
            ValidationChargePhase::Body => &mut self.body,
            ValidationChargePhase::Result => &mut self.result,
        };
        *counter += bytes;
    }

    fn bytes(self) -> usize {
        self.key + self.body + self.result
    }
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ValidationChargeContributor {
    pub(crate) domain: Option<&'static str>,
    pub(crate) totals: ValidationChargeTotals,
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ValidationFailure {
    pub(crate) reason: ValidationFailureReason,
    pub(crate) domain: Option<&'static str>,
    pub(crate) nodes: usize,
    pub(crate) depth: usize,
    // For byte failures this is the counter before attempted_bytes is charged.
    pub(crate) bytes: usize,
    pub(crate) inventory_bytes: usize,
    pub(crate) inventory_depth: usize,
    pub(crate) attempted_bytes: Option<usize>,
    pub(crate) charge_phase: Option<ValidationChargePhase>,
    pub(crate) charge_totals: ValidationChargeTotals,
    pub(crate) top_contributors: Vec<ValidationChargeContributor>,
    // Successful charges in retained buckets outside the top eight.
    pub(crate) other_bytes: usize,
    // Successful charges whose domain could not fit in the 64-bucket map.
    pub(crate) overflow_bytes: usize,
    // Only the last retained record is marked if later first failures were omitted.
    pub(crate) truncated: bool,
}

#[cfg(test)]
const MAX_FAILURE_RECORDS: usize = 256;
#[cfg(test)]
const MAX_CHARGE_DOMAINS: usize = 64;
#[cfg(test)]
const MAX_CHARGE_CONTRIBUTORS: usize = 8;

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
    #[cfg(test)]
    domains: Vec<&'static str>,
    #[cfg(test)]
    failure_recorded: bool,
    #[cfg(test)]
    charge_phase: Option<ValidationChargePhase>,
    #[cfg(test)]
    charge_totals: ValidationChargeTotals,
    #[cfg(test)]
    charge_domains: BTreeMap<Option<&'static str>, ValidationChargeTotals>,
    #[cfg(test)]
    charge_overflow_bytes: usize,
}

thread_local! {
    // This slot exists only while its owner's SQLite savepoint is open.
    static ACTIVE: RefCell<Option<State>> = const { RefCell::new(None) };
    #[cfg(test)]
    static COMPLETED: RefCell<Option<Vec<BTreeMap<&'static str, usize>>>> = const { RefCell::new(None) };
    #[cfg(test)]
    static FAILURES: RefCell<Option<Vec<ValidationFailure>>> = const { RefCell::new(None) };
}

#[cfg(test)]
struct ChargePhaseGuard(Option<ValidationChargePhase>);

#[cfg(test)]
impl ChargePhaseGuard {
    fn enter(phase: ValidationChargePhase) -> Self {
        Self(ACTIVE.with(|active| {
            active
                .borrow_mut()
                .as_mut()
                .and_then(|state| state.charge_phase.replace(phase))
        }))
    }
}

#[cfg(test)]
impl Drop for ChargePhaseGuard {
    fn drop(&mut self) {
        ACTIVE.with(|active| {
            if let Some(state) = active.borrow_mut().as_mut() {
                state.charge_phase = self.0;
            }
        });
    }
}

#[cfg(test)]
struct DiagnosticDomainGuard(usize);

#[cfg(test)]
impl DiagnosticDomainGuard {
    fn enter(domain: &'static str) -> Self {
        Self(ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            let state = active.as_mut().unwrap();
            let previous = state.domains.len();
            state.domains.push(domain);
            previous
        }))
    }
}

#[cfg(test)]
impl Drop for DiagnosticDomainGuard {
    fn drop(&mut self) {
        ACTIVE.with(|active| {
            if let Some(state) = active.borrow_mut().as_mut() {
                if std::thread::panicking() {
                    record_failure(
                        Some(&mut *state),
                        ValidationFailureReason::Unwind,
                        None,
                        None,
                    );
                }
                state.domains.truncate(self.0);
            }
        });
    }
}

#[cfg(test)]
fn charge_memoized_bytes(
    connection: &Connection,
    bytes: usize,
    phase: ValidationChargePhase,
) -> Result<(), WorkflowError> {
    let _phase = ChargePhaseGuard::enter(phase);
    charge_bytes(connection, bytes)
}

#[cfg(test)]
fn attribute_charge(state: &mut State, bytes: usize) {
    let phase = state.charge_phase.unwrap_or(ValidationChargePhase::Body);
    state.charge_totals.add(phase, bytes);
    let domain = state.domains.last().copied();
    if state.charge_domains.contains_key(&domain) || state.charge_domains.len() < MAX_CHARGE_DOMAINS
    {
        state
            .charge_domains
            .entry(domain)
            .or_default()
            .add(phase, bytes);
    } else {
        state.charge_overflow_bytes += bytes;
    }
}

#[cfg(test)]
fn record_failure(
    state: Option<&mut State>,
    reason: ValidationFailureReason,
    domain: Option<&'static str>,
    attempted_bytes: Option<usize>,
) {
    FAILURES.with(|failures| {
        let mut failures = failures.borrow_mut();
        let Some(records) = failures.as_mut() else {
            return;
        };
        let record = if let Some(state) = state {
            if state.failure_recorded {
                return;
            }
            state.failure_recorded = true;
            let mut contributors: Vec<_> = state
                .charge_domains
                .iter()
                .map(|(domain, totals)| ValidationChargeContributor {
                    domain: *domain,
                    totals: *totals,
                })
                .collect();
            contributors.sort_by(|left, right| {
                right
                    .totals
                    .bytes()
                    .cmp(&left.totals.bytes())
                    .then_with(|| left.domain.cmp(&right.domain))
            });
            let other_bytes = contributors
                .iter()
                .skip(MAX_CHARGE_CONTRIBUTORS)
                .map(|entry| entry.totals.bytes())
                .sum();
            contributors.truncate(MAX_CHARGE_CONTRIBUTORS);
            ValidationFailure {
                reason,
                domain: domain.or_else(|| state.domains.last().copied()),
                nodes: state.nodes,
                depth: state.depth,
                bytes: state.bytes,
                inventory_bytes: state.inventory_bytes,
                inventory_depth: state.inventory_depth,
                attempted_bytes,
                charge_phase: attempted_bytes
                    .map(|_| state.charge_phase.unwrap_or(ValidationChargePhase::Body)),
                charge_totals: state.charge_totals,
                top_contributors: contributors,
                other_bytes,
                overflow_bytes: state.charge_overflow_bytes,
                truncated: false,
            }
        } else {
            ValidationFailure {
                reason,
                domain,
                nodes: 0,
                depth: 0,
                bytes: 0,
                inventory_bytes: 0,
                inventory_depth: 0,
                attempted_bytes,
                charge_phase: attempted_bytes.map(|_| ValidationChargePhase::Body),
                charge_totals: ValidationChargeTotals::default(),
                top_contributors: Vec::new(),
                other_bytes: 0,
                overflow_bytes: 0,
                truncated: false,
            }
        };
        if records.len() >= MAX_FAILURE_RECORDS {
            if let Some(last) = records.last_mut() {
                last.truncated = true;
            }
            return;
        }
        records.push(record);
    });
}

#[cfg(test)]
fn record_active_failure(reason: ValidationFailureReason) {
    ACTIVE.with(|active| record_failure(active.borrow_mut().as_mut(), reason, None, None));
}

#[cfg(test)]
fn record_connection_or_poison(
    state: &mut State,
    connection: &Connection,
    attempted_bytes: Option<usize>,
) {
    let reason = if state.connection != connection_id(connection) {
        ValidationFailureReason::ConnectionMismatch
    } else {
        ValidationFailureReason::Poisoned
    };
    record_failure(Some(&mut *state), reason, None, attempted_bytes);
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
        let state = active.as_mut();
        #[cfg(test)]
        if state.is_none() {
            record_failure(None, ValidationFailureReason::MissingScope, None, None);
        }
        let state = state.ok_or_else(corrupt)?;
        if state.connection != connection_id(connection) || state.poisoned {
            #[cfg(test)]
            record_connection_or_poison(state, connection, None);
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
                #[cfg(test)]
                record_failure(
                    Some(&mut *state),
                    ValidationFailureReason::WriteDuringValidation,
                    None,
                    None,
                );
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
            let synchronized = synchronize(self.connection);
            #[cfg(test)]
            if synchronized.is_err() {
                record_active_failure(ValidationFailureReason::ScopeError);
            }
            synchronized?;
            let released = self
                .connection
                .execute_batch("RELEASE sentinel_validation_scope");
            #[cfg(test)]
            if released.is_err() {
                record_active_failure(ValidationFailureReason::ScopeError);
            }
            released?;
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
            #[cfg(test)]
            if std::thread::panicking() {
                record_active_failure(ValidationFailureReason::Unwind);
            }
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
            #[cfg(test)]
            record_active_failure(ValidationFailureReason::ConnectionMismatch);
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
            #[cfg(test)]
            domains: Vec::new(),
            #[cfg(test)]
            failure_recorded: false,
            #[cfg(test)]
            charge_phase: None,
            #[cfg(test)]
            charge_totals: ValidationChargeTotals::default(),
            #[cfg(test)]
            charge_domains: BTreeMap::new(),
            #[cfg(test)]
            charge_overflow_bytes: 0,
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
    let scope = enter(connection);
    #[cfg(test)]
    if scope.is_err() {
        record_active_failure(ValidationFailureReason::ScopeError);
    }
    let scope = scope?;
    let result = action();
    #[cfg(test)]
    if result.is_err() {
        record_active_failure(ValidationFailureReason::ValidationError);
    }
    let result = result?;
    scope.finish()?;
    Ok(result)
}

pub(crate) fn charge_bytes(connection: &Connection, bytes: usize) -> Result<(), WorkflowError> {
    ACTIVE.with(|active| {
        let mut active = active.borrow_mut();
        let state = active.as_mut();
        #[cfg(test)]
        if state.is_none() {
            record_failure(
                None,
                ValidationFailureReason::MissingScope,
                None,
                Some(bytes),
            );
        }
        let state = state.ok_or_else(corrupt)?;
        if state.connection != connection_id(connection) || state.poisoned {
            #[cfg(test)]
            record_connection_or_poison(state, connection, Some(bytes));
            return Err(corrupt());
        }
        #[cfg(test)]
        {
            let reason = match state.bytes.checked_add(bytes) {
                None => Some(ValidationFailureReason::BytesOverflow),
                Some(total) if total > MAX_BYTES => Some(ValidationFailureReason::BytesLimit),
                Some(_) => None,
            };
            if let Some(reason) = reason {
                record_failure(Some(&mut *state), reason, None, Some(bytes));
            }
        }
        state.bytes = state.bytes.checked_add(bytes).ok_or_else(corrupt)?;
        if state.bytes > MAX_BYTES {
            state.poisoned = true;
            return Err(corrupt());
        }
        #[cfg(test)]
        attribute_charge(state, bytes);
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
        #[cfg(test)]
        let _domain = DiagnosticDomainGuard::enter(domain);
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
                    #[cfg(test)]
                    record_failure(
                        Some(&mut *state),
                        ValidationFailureReason::InventoryVisiting,
                        Some(domain),
                        None,
                    );
                    state.poisoned = true;
                    return Err(corrupt());
                }
                Some(Entry::Complete(value)) => {
                    let cached = value
                        .downcast_ref::<Option<T>>()
                        .cloned()
                        .map(|value| (Some(value), None));
                    #[cfg(test)]
                    if cached.is_none() {
                        record_failure(
                            Some(&mut *state),
                            ValidationFailureReason::CacheTypeMismatch,
                            Some(domain),
                            None,
                        );
                    }
                    return cached.ok_or_else(corrupt);
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
            #[cfg(test)]
            {
                if result.is_err() {
                    record_failure(
                        Some(&mut *state),
                        ValidationFailureReason::ValidationError,
                        Some(domain),
                        None,
                    );
                }
            }
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
            #[cfg(test)]
            record_connection_or_poison(state, connection, None);
            return Err(corrupt());
        }
        state.entries.clear();
        state.inventories.clear();
        state.inventory_bytes = 0;
        state.reuse = false;
        if state.depth != 0 || state.inventory_depth != 0 {
            #[cfg(test)]
            record_failure(
                Some(&mut *state),
                ValidationFailureReason::WriteDuringValidation,
                None,
                None,
            );
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
        #[cfg(test)]
        let _domain = DiagnosticDomainGuard::enter(domain);
        let input = serde_json::to_vec(input).map_err(|_| persistence())?;
        let key = (domain, TypeId::of::<T>(), input);
        let cached = ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            let state = active.as_mut().ok_or_else(corrupt)?;
            match state.entries.get(&key) {
                Some(Entry::Visiting) => {
                    #[cfg(test)]
                    record_failure(
                        Some(&mut *state),
                        ValidationFailureReason::Visiting,
                        Some(domain),
                        None,
                    );
                    state.poisoned = true;
                    return Err(corrupt());
                }
                Some(Entry::Complete(value)) => {
                    let cached = value.downcast_ref::<T>().cloned().map(Some);
                    #[cfg(test)]
                    if cached.is_none() {
                        record_failure(
                            Some(&mut *state),
                            ValidationFailureReason::CacheTypeMismatch,
                            Some(domain),
                            None,
                        );
                    }
                    return cached.ok_or_else(corrupt);
                }
                None => {}
            }
            if state.nodes >= MAX_NODES || state.depth >= MAX_DEPTH {
                #[cfg(test)]
                {
                    let reason = if state.nodes >= MAX_NODES {
                        ValidationFailureReason::NodeLimit
                    } else {
                        ValidationFailureReason::DepthLimit
                    };
                    record_failure(Some(&mut *state), reason, Some(domain), None);
                }
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
            #[cfg(not(test))]
            charge_bytes(connection, key.2.len().saturating_add(256))?;
            #[cfg(test)]
            charge_memoized_bytes(
                connection,
                key.2.len().saturating_add(256),
                ValidationChargePhase::Key,
            )?;
            let value = validate()?;
            synchronize(connection)?;
            let bytes = serialized_json_size(&value).map_err(|_| persistence())?;
            #[cfg(not(test))]
            charge_bytes(connection, bytes)?;
            #[cfg(test)]
            charge_memoized_bytes(connection, bytes, ValidationChargePhase::Result)?;
            Ok(value)
        })();
        ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            let state = active.as_mut().ok_or_else(corrupt)?;
            #[cfg(test)]
            {
                if result.is_err() {
                    record_failure(
                        Some(&mut *state),
                        ValidationFailureReason::ValidationError,
                        Some(domain),
                        None,
                    );
                }
            }
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
// Capture first failures only, without keys/payloads/error text. At most 256 records,
// each with eight static-domain contributors and scalar remainder/overflow totals;
// last().truncated signals omitted later failures without replacing a captured cause.
pub(crate) fn with_validation_failures<T>(
    action: impl FnOnce() -> T,
) -> (T, Vec<ValidationFailure>) {
    struct Restore(Option<Vec<ValidationFailure>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FAILURES.with(|failures| *failures.borrow_mut() = self.0.take());
        }
    }

    let _restore = Restore(FAILURES.with(|failures| failures.replace(Some(Vec::new()))));
    let result = action();
    let records = FAILURES.with(|failures| failures.borrow_mut().take().unwrap());
    (result, records)
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

    fn assert_charge_accounting(record: &ValidationFailure) {
        assert_eq!(record.charge_totals.bytes(), record.bytes);
        assert_eq!(
            record
                .top_contributors
                .iter()
                .map(|entry| entry.totals.bytes())
                .sum::<usize>()
                + record.other_bytes
                + record.overflow_bytes,
            record.bytes
        );
        assert!(record.top_contributors.len() <= MAX_CHARGE_CONTRIBUTORS);
    }

    #[test]
    fn charge_attribution_identifies_each_failed_phase_before_attempt() {
        let connection = database();
        for phase in [
            ValidationChargePhase::Key,
            ValidationChargePhase::Body,
            ValidationChargePhase::Result,
        ] {
            let (result, records) = with_validation_failures(|| {
                with_scope(&connection, || {
                    match phase {
                        ValidationChargePhase::Key => charge_bytes(&connection, MAX_BYTES)?,
                        ValidationChargePhase::Result => {
                            charge_bytes(&connection, MAX_BYTES - 257)?
                        }
                        ValidationChargePhase::Body => {}
                    }
                    memoize::<i64>(&connection, "phase-test", &0, || {
                        if phase == ValidationChargePhase::Body {
                            charge_bytes(&connection, MAX_BYTES)?;
                        }
                        Ok(0)
                    })
                })
            });
            assert!(result.is_err());
            assert_eq!(records.len(), 1);
            let record = &records[0];
            assert_eq!(record.reason, ValidationFailureReason::BytesLimit);
            assert_eq!(record.domain, Some("phase-test"));
            assert_eq!(record.charge_phase, Some(phase));
            assert_eq!(
                record.attempted_bytes,
                Some(match phase {
                    ValidationChargePhase::Key => 257,
                    ValidationChargePhase::Body => MAX_BYTES,
                    ValidationChargePhase::Result => 1,
                })
            );
            assert_eq!(
                record.bytes,
                if phase == ValidationChargePhase::Body {
                    257
                } else {
                    MAX_BYTES
                }
            );
            assert_charge_accounting(record);
        }
    }

    #[test]
    fn charge_attribution_separates_nested_domains_and_does_not_charge_cache_hits() {
        let connection = database();
        let (result, records) = with_validation_failures(|| {
            with_scope(&connection, || {
                charge_bytes(&connection, 5)?;
                memoize(&connection, "outer-charge", &0, || {
                    charge_bytes(&connection, 11)?;
                    memoize(&connection, "inner-charge", &0, || {
                        charge_bytes(&connection, 7)?;
                        Ok(0)
                    })?;
                    charge_bytes(&connection, 13)?;
                    Ok(0)
                })?;
                assert_eq!(
                    memoize(
                        &connection,
                        "outer-charge",
                        &0,
                        || -> Result<i32, WorkflowError> {
                            panic!("cached proof must not validate")
                        }
                    )?,
                    0
                );
                charge_bytes(&connection, MAX_BYTES)
            })
        });
        assert!(result.is_err());
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.domain, None);
        assert_eq!(record.charge_phase, Some(ValidationChargePhase::Body));
        assert_eq!(
            record.charge_totals,
            ValidationChargeTotals {
                key: 514,
                body: 36,
                result: 2
            }
        );
        assert_eq!(
            record.top_contributors,
            vec![
                ValidationChargeContributor {
                    domain: Some("outer-charge"),
                    totals: ValidationChargeTotals {
                        key: 257,
                        body: 24,
                        result: 1
                    }
                },
                ValidationChargeContributor {
                    domain: Some("inner-charge"),
                    totals: ValidationChargeTotals {
                        key: 257,
                        body: 7,
                        result: 1
                    }
                },
                ValidationChargeContributor {
                    domain: None,
                    totals: ValidationChargeTotals {
                        key: 0,
                        body: 5,
                        result: 0
                    }
                },
            ]
        );
        assert_charge_accounting(record);
    }

    #[test]
    fn charge_attribution_bounds_domains_and_reports_top_other_and_overflow() {
        const DOMAINS: &str = "d00 d01 d02 d03 d04 d05 d06 d07 d08 d09 d10 d11 d12 d13 d14 d15 d16 d17 d18 d19 d20 d21 d22 d23 d24 d25 d26 d27 d28 d29 d30 d31 d32 d33 d34 d35 d36 d37 d38 d39 d40 d41 d42 d43 d44 d45 d46 d47 d48 d49 d50 d51 d52 d53 d54 d55 d56 d57 d58 d59 d60 d61 d62 d63 d64";
        let connection = database();
        let (result, records) = with_validation_failures(|| {
            with_scope(&connection, || {
                for (index, domain) in DOMAINS.split_whitespace().enumerate() {
                    let _domain = DiagnosticDomainGuard::enter(domain);
                    charge_bytes(&connection, index + 1)?;
                }
                // A full map still updates existing buckets; new domains aggregate only.
                let _domain = DiagnosticDomainGuard::enter("d00");
                charge_bytes(&connection, 100)?;
                ACTIVE.with(|active| {
                    assert_eq!(
                        active.borrow().as_ref().unwrap().charge_domains.len(),
                        MAX_CHARGE_DOMAINS
                    )
                });
                charge_bytes(&connection, MAX_BYTES)
            })
        });
        assert!(result.is_err());
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.bytes, 2245);
        assert_eq!(record.top_contributors.len(), MAX_CHARGE_CONTRIBUTORS);
        assert_eq!(record.top_contributors[0].domain, Some("d00"));
        assert_eq!(record.top_contributors[0].totals.body, 101);
        assert_eq!(record.other_bytes, 1652);
        assert_eq!(record.overflow_bytes, 65);
        assert_charge_accounting(record);
    }

    #[test]
    fn charge_attribution_guards_restore_on_nested_unwind() {
        let connection = database();
        let (_, records) = with_validation_failures(|| {
            with_scope(&connection, || {
                {
                    let _domain = DiagnosticDomainGuard::enter("outer-guard");
                    let _phase = ChargePhaseGuard::enter(ValidationChargePhase::Result);
                    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let _domain = DiagnosticDomainGuard::enter("inner-guard");
                        let _phase = ChargePhaseGuard::enter(ValidationChargePhase::Key);
                        charge_bytes(&connection, 3).unwrap();
                        panic!("diagnostic guard restoration");
                    }));
                    assert!(panic.is_err());
                    ACTIVE.with(|active| {
                        let active = active.borrow();
                        let state = active.as_ref().unwrap();
                        assert_eq!(state.domains, vec!["outer-guard"]);
                        assert_eq!(state.charge_phase, Some(ValidationChargePhase::Result));
                    });
                    charge_bytes(&connection, 5)?;
                }
                ACTIVE.with(|active| {
                    let active = active.borrow();
                    let state = active.as_ref().unwrap();
                    assert!(state.domains.is_empty());
                    assert_eq!(state.charge_phase, None);
                });
                charge_bytes(&connection, 7)?;
                ACTIVE.with(|active| {
                    assert_eq!(
                        active.borrow().as_ref().unwrap().charge_totals,
                        ValidationChargeTotals {
                            key: 3,
                            body: 7,
                            result: 5
                        }
                    )
                });
                Ok(())
            })
            .unwrap();
        });
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].reason, ValidationFailureReason::Unwind);
        assert_eq!(records[0].domain, Some("inner-guard"));
        assert_eq!(records[0].charge_totals.key, 3);
        assert_charge_accounting(&records[0]);
        assert!(ACTIVE.with(|active| active.borrow().is_none()));
        assert!(connection.is_autocommit());
    }

    #[test]
    fn validation_failures_preserve_first_byte_failure_after_cleanup() {
        let connection = database();
        let (result, records) = with_validation_failures(|| {
            memoize::<i64>(&connection, "byte-test", &0, || {
                let result = charge_bytes(&connection, MAX_BYTES);
                assert!(charge_bytes(&connection, 1).is_err());
                result?;
                Ok(0)
            })
        });
        assert!(result.is_err());
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.reason, ValidationFailureReason::BytesLimit);
        assert_eq!(record.domain, Some("byte-test"));
        assert_eq!(record.nodes, 1);
        assert_eq!(record.depth, 1);
        assert_eq!(record.bytes, 257);
        assert_eq!(record.attempted_bytes, Some(MAX_BYTES));
        assert_eq!(record.inventory_bytes, 0);
        assert_eq!(record.inventory_depth, 0);
        assert!(!record.truncated);
        assert!(ACTIVE.with(|active| active.borrow().is_none()));
        assert!(connection.is_autocommit());
        let (result, records) = with_validation_failures(|| value(&connection));
        assert_eq!(result.unwrap(), 1);
        assert!(records.is_empty());
    }

    #[test]
    fn validation_failures_distinguish_byte_overflow_nodes_and_depth() {
        let connection = database();
        let (result, records) = with_validation_failures(|| {
            with_scope(&connection, || {
                ACTIVE.with(|active| {
                    let mut active = active.borrow_mut();
                    let state = active.as_mut().unwrap();
                    state.bytes = usize::MAX;
                    state.charge_totals.body = usize::MAX;
                    state.charge_overflow_bytes = usize::MAX;
                });
                charge_bytes(&connection, 1)
            })
        });
        assert!(result.is_err());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].reason, ValidationFailureReason::BytesOverflow);
        assert_eq!(records[0].bytes, usize::MAX);
        assert_eq!(records[0].attempted_bytes, Some(1));
        assert_charge_accounting(&records[0]);

        for reason in [
            ValidationFailureReason::NodeLimit,
            ValidationFailureReason::DepthLimit,
        ] {
            let (result, records) = with_validation_failures(|| {
                with_scope(&connection, || {
                    ACTIVE.with(|active| {
                        let mut active = active.borrow_mut();
                        let state = active.as_mut().unwrap();
                        if reason == ValidationFailureReason::NodeLimit {
                            state.nodes = MAX_NODES;
                        } else {
                            state.depth = MAX_DEPTH;
                        }
                    });
                    memoize::<i64>(&connection, "capacity-test", &0, || panic!("must reject"))
                })
            });
            assert!(result.is_err());
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].reason, reason);
            assert_eq!(records[0].domain, Some("capacity-test"));
            assert_eq!(records[0].attempted_bytes, None);
            if reason == ValidationFailureReason::NodeLimit {
                assert_eq!(records[0].nodes, MAX_NODES);
            } else {
                assert_eq!(records[0].depth, MAX_DEPTH);
            }
            assert!(connection.is_autocommit());
        }
    }

    #[test]
    fn validation_failures_distinguish_mandatory_and_inventory_visiting() {
        let connection = database();
        let (result, records) = with_validation_failures(|| {
            memoize::<i64>(&connection, "cycle-test", &0, || {
                memoize(&connection, "cycle-test", &0, || Ok(0))
            })
        });
        assert!(result.is_err());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].reason, ValidationFailureReason::Visiting);
        assert_eq!(records[0].domain, Some("cycle-test"));
        assert_eq!(records[0].depth, 1);

        let (result, records) = with_validation_failures(|| {
            inventory::<i64>(&connection, "inventory-cycle-test", &0, || {
                inventory(&connection, "inventory-cycle-test", &0, || Ok(Some(0)))
            })
        });
        assert!(result.is_err());
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].reason,
            ValidationFailureReason::InventoryVisiting
        );
        assert_eq!(records[0].domain, Some("inventory-cycle-test"));
        assert_eq!(records[0].inventory_depth, 1);

        let (result, records) = with_validation_failures(|| {
            inventory(&connection, "optional-overflow-test", &0, || {
                Ok(Some("x".repeat(MAX_INVENTORY_BYTES)))
            })
        });
        assert!(result.unwrap().is_none());
        assert!(records.is_empty());
    }

    #[test]
    fn validation_failures_distinguish_poison_connection_writes_and_validator_errors() {
        let connection = database();
        let other = database();
        let (result, records) = with_validation_failures(|| {
            with_scope(&connection, || {
                ACTIVE.with(|active| active.borrow_mut().as_mut().unwrap().poisoned = true);
                charge_bytes(&connection, 1)
            })
        });
        assert!(result.is_err());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].reason, ValidationFailureReason::Poisoned);

        let (result, records) =
            with_validation_failures(|| with_scope(&connection, || value(&other)));
        assert!(result.is_err());
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].reason,
            ValidationFailureReason::ConnectionMismatch
        );

        for announced in [false, true] {
            let (result, records) = with_validation_failures(|| {
                memoize::<i64>(&connection, "write-test", &0, || {
                    if announced {
                        before_write(&connection)?;
                    }
                    connection.execute("UPDATE records SET value=2 WHERE id=1", [])?;
                    Ok(2)
                })
            });
            assert!(result.is_err());
            assert_eq!(records.len(), 1);
            assert_eq!(
                records[0].reason,
                ValidationFailureReason::WriteDuringValidation
            );
            assert_eq!(records[0].domain, Some("write-test"));
            assert_eq!(value(&connection).unwrap(), 1);
        }

        let (result, records) = with_validation_failures(|| {
            memoize::<i64>(&connection, "validator-test", &0, || Err(corrupt()))
        });
        assert!(result.is_err());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].reason, ValidationFailureReason::ValidationError);
        assert_eq!(records[0].domain, Some("validator-test"));
        assert_eq!(records[0].depth, 1);

        let (result, records) = with_validation_failures(|| charge_bytes(&connection, 1));
        assert!(result.is_err());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].reason, ValidationFailureReason::MissingScope);
    }

    #[test]
    fn validation_failures_are_bounded_and_restore_trackers_on_return_and_unwind() {
        let connection = database();
        for count in [MAX_FAILURE_RECORDS, MAX_FAILURE_RECORDS + 2] {
            let (_, records) = with_validation_failures(|| {
                for _ in 0..count {
                    assert!(with_scope::<()>(&connection, || {
                        let result = charge_bytes(&connection, MAX_BYTES + 1);
                        assert!(charge_bytes(&connection, 1).is_err());
                        result
                    })
                    .is_err());
                }
            });
            assert_eq!(records.len(), MAX_FAILURE_RECORDS);
            assert!(records.iter().all(|record| {
                record.reason == ValidationFailureReason::BytesLimit
                    && record.bytes == 0
                    && record.attempted_bytes == Some(MAX_BYTES + 1)
            }));
            for record in &records {
                assert_charge_accounting(record);
            }
            assert!(records[..MAX_FAILURE_RECORDS - 1]
                .iter()
                .all(|record| !record.truncated));
            assert_eq!(
                records.last().unwrap().truncated,
                count > MAX_FAILURE_RECORDS
            );
        }
        assert!(FAILURES.with(|failures| failures.borrow().is_none()));

        let (_, outer) = with_validation_failures(|| {
            assert!(with_scope::<()>(&connection, || Err(corrupt())).is_err());
            let (result, inner) =
                with_validation_failures(|| with_scope::<()>(&connection, || Err(corrupt())));
            assert!(result.is_err());
            assert_eq!(inner.len(), 1);
            let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                with_validation_failures(|| {
                    with_scope::<()>(&connection, || panic!("tracker restoration"))
                })
            }));
            assert!(panic.is_err());
            assert!(ACTIVE.with(|active| active.borrow().is_none()));
            assert!(with_scope::<()>(&connection, || Err(corrupt())).is_err());
        });
        assert_eq!(outer.len(), 2);
        assert!(FAILURES.with(|failures| failures.borrow().is_none()));
        assert!(connection.is_autocommit());

        let (result, records) = with_validation_failures(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                memoize::<i64>(&connection, "unwind-test", &0, || panic!("scope cleanup"))
            }))
        });
        assert!(result.is_err());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].reason, ValidationFailureReason::Unwind);
        assert_eq!(records[0].domain, Some("unwind-test"));
        assert_eq!(value(&connection).unwrap(), 1);
    }

    #[test]
    fn validation_failures_capture_cache_type_and_scope_finish_errors() {
        let connection = database();
        let (result, records) = with_validation_failures(|| {
            with_scope(&connection, || {
                ACTIVE.with(|active| {
                    active.borrow_mut().as_mut().unwrap().entries.insert(
                        (
                            "type-test",
                            TypeId::of::<i64>(),
                            serde_json::to_vec(&0).unwrap(),
                        ),
                        Entry::Complete(Box::new(false)),
                    );
                });
                memoize::<i64>(&connection, "type-test", &0, || panic!("must reject"))
            })
        });
        assert!(result.is_err());
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].reason,
            ValidationFailureReason::CacheTypeMismatch
        );
        assert_eq!(records[0].domain, Some("type-test"));

        let (result, records) = with_validation_failures(|| {
            with_scope(&connection, || {
                connection.execute_batch("RELEASE sentinel_validation_scope")?;
                Ok(())
            })
        });
        assert!(result.is_err());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].reason, ValidationFailureReason::ScopeError);
        assert!(connection.is_autocommit());
        assert_eq!(value(&connection).unwrap(), 1);
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
