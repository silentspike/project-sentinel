//! Store-sealed V2 event append boundary.

use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, OptionalExtension};
use sentinel_common::{
    canonical_json, sha256_json, validate_sha256, AppendDispositionV2, AppendOutcomeV2,
    AppendProposalV2, CausationPolicyV1, CompatibleEventEnvelope, DecodedEventAuthorityState,
    DomainEvent, EventContractError, EventDurability, EventEnvelopeV2, EventPayloadCodec,
    EventSchemaRegistry, ExpectedStreamRevision, FencedStore, LegacyEventEnvelopeV1,
    StateTransferScope,
};
use serde::Serialize;
use thiserror::Error;

use crate::EventStore;

/// Explicit compatibility owners for pre-V2 event producers. New authority
/// producers use [`EventAppendGateway`]; this enum keeps every remaining legacy
/// writer visible to the repository-wide callsite audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyEventProducer {
    EcsTickBatch,
    GaiaReadiness,
    NightRun,
    RuntimeAgent,
    DaemonOrchestrator,
    DaemonOperatorApi,
    DaemonWorkflow,
    DaemonWorkbench,
    PlatformControlPlane,
    ResourceManager,
    TestHarness,
    BenchmarkHarness,
}

pub struct LegacyEventAppendGateway<'a> {
    store: &'a EventStore,
    #[allow(dead_code)]
    producer: LegacyEventProducer,
}

impl LegacyEventAppendGateway<'_> {
    pub fn append_event(&self, event: &DomainEvent) -> anyhow::Result<i64> {
        self.store.append_event(event)
    }

    pub fn append_with_outbox(&self, event: &DomainEvent, topic: &str) -> anyhow::Result<i64> {
        self.store.append_with_outbox(event, topic)
    }

    pub fn append_with_outbox_batch<'a, I>(&self, entries: I) -> anyhow::Result<usize>
    where
        I: IntoIterator<Item = (&'a DomainEvent, &'a str)>,
    {
        self.store.append_with_outbox_batch(entries)
    }
}

const EVENT_SCHEMA_VERSION: i64 = 2;
const EVENT_V2_MIGRATION: &str =
    include_str!("../migrations/event-store/0001-event-envelope-v2.sql");
const EVENT_V2_MIGRATION_NAME: &str = "event-envelope-v2";
const EVENT_V2_SCHEMA_FINGERPRINT: &str =
    "d7e51ea21faf194fa85b894534f816cfb6b5ca530be5d73cfabeac3ae22c88b4";

const CREATE_EVENT_SCHEMA_MIGRATIONS: &str = "
CREATE TABLE IF NOT EXISTS event_schema_migrations (
    version INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    sha256 TEXT NOT NULL,
    applied_at_ms INTEGER NOT NULL
)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedEventCallerV1 {
    pub service_id: String,
    pub producer: String,
    pub authority_scope_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EventContractSchemaStatus {
    pub schema_version: u32,
    pub migration_name: String,
    pub migration_sha256: String,
    pub event_truth_generation: u64,
    pub next_global_position: u64,
}

#[derive(Debug, Error)]
pub enum EventAppendError {
    #[error(transparent)]
    Contract(#[from] EventContractError),
    #[error("authenticated caller is not bound to the proposal producer and authority scope")]
    UnauthorizedCaller,
    #[error("operation is already bound to a different canonical request")]
    OperationConflict,
    #[error("expected stream revision {expected}, actual revision {actual}")]
    WrongExpectedRevision { expected: String, actual: u64 },
    #[error("proposal owner term does not match the store-issued write guard")]
    StaleOwnerTerm,
    #[error("direct causation does not reference a committed event in the same project authority")]
    InvalidCausationEvent,
    #[error("stored V2 event outcome is corrupt: {0}")]
    CorruptOutcome(String),
    #[error("event store SQL failure: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("event store failure: {0}")]
    Store(#[from] anyhow::Error),
}

pub struct EventAppendGateway<'a> {
    store: &'a EventStore,
    registry: &'a EventSchemaRegistry,
}

impl EventStore {
    pub fn append_gateway<'a>(
        &'a self,
        registry: &'a EventSchemaRegistry,
    ) -> EventAppendGateway<'a> {
        EventAppendGateway {
            store: self,
            registry,
        }
    }

    pub fn legacy_append_gateway(
        &self,
        producer: LegacyEventProducer,
    ) -> LegacyEventAppendGateway<'_> {
        LegacyEventAppendGateway {
            store: self,
            producer,
        }
    }

    pub fn event_v2_by_id(&self, event_id: &str) -> anyhow::Result<Option<EventEnvelopeV2>> {
        let conn = self
            .conn
            .lock()
            .map_err(|error| anyhow::anyhow!("event store lock poisoned: {error}"))?;
        read_envelope_by_id(&conn, event_id).map_err(anyhow::Error::from)
    }

    pub fn event_contract_schema_status(&self) -> anyhow::Result<EventContractSchemaStatus> {
        let conn = self
            .conn
            .lock()
            .map_err(|error| anyhow::anyhow!("event store lock poisoned: {error}"))?;
        verify_event_contract_schema(&conn)
    }

    /// Reads either event format without inventing V2 authority for legacy rows.
    pub fn event_by_id_compatible(
        &self,
        event_id: &str,
    ) -> anyhow::Result<Option<CompatibleEventEnvelope>> {
        let conn = self
            .conn
            .lock()
            .map_err(|error| anyhow::anyhow!("event store lock poisoned: {error}"))?;
        let current = read_envelope_by_id(&conn, event_id)?;
        let legacy = read_legacy_envelope_by_id(&conn, event_id)?;
        anyhow::ensure!(
            current.is_none() || legacy.is_none(),
            "event identity exists in both V1 and V2 stores"
        );
        Ok(current
            .map(|event| CompatibleEventEnvelope::V2(Box::new(event)))
            .or_else(|| legacy.map(|event| CompatibleEventEnvelope::V1(Box::new(event)))))
    }
}

impl EventAppendGateway<'_> {
    pub fn append(
        &self,
        caller: &AuthenticatedEventCallerV1,
        proposal: &AppendProposalV2,
    ) -> Result<AppendOutcomeV2, EventAppendError> {
        self.registry.validate_proposal(proposal)?;
        let causation_policy = self.registry.causation_policy_for(proposal)?;
        validate_caller(caller, proposal)?;
        let authority_scope_digest = proposal.causal_context.authority_scope_digest()?;
        let request_digest = proposal.canonical_request_digest()?;

        if let Some(outcome) = self.lookup_operation(
            &authority_scope_digest,
            &proposal.causal_context.operation_id,
            &request_digest,
        )? {
            return Ok(outcome);
        }

        let guard = self
            .store
            .owner_registry
            .issue(StateTransferScope::World)
            .map_err(anyhow::Error::from)?;
        if let Some(term) = &proposal.owner_term {
            if term.scope != *guard.scope()
                || term.owner_node != guard.owner_node()
                || term.epoch != guard.epoch()
                || term.coordinator_generation != guard.coordinator_generation()
            {
                return Err(EventAppendError::StaleOwnerTerm);
            }
        }

        let transaction = self.store.begin_fenced_write(&guard)?;
        if let Some(outcome) = lookup_operation_in(
            &transaction,
            &authority_scope_digest,
            &proposal.causal_context.operation_id,
            &request_digest,
        )? {
            transaction.commit()?;
            return Ok(outcome);
        }

        let current_revision = transaction
            .query_row(
                "SELECT stream_revision FROM event_stream_heads_v2 WHERE stream_namespace = ?1",
                params![authority_scope_digest],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .map(sql_u64)
            .transpose()?
            .unwrap_or(0);
        validate_expected_revision(proposal.expected_stream_revision, current_revision)?;

        let (generation, global_position) = transaction.query_row(
            "SELECT event_truth_generation, next_global_position
             FROM event_truth_metadata WHERE singleton_id = 1",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )?;
        let generation = sql_u64(generation)?;
        let global_position = sql_u64(global_position)?;
        validate_causation_in(
            &transaction,
            proposal,
            causation_policy,
            generation,
            global_position,
        )?;
        let stream_revision = current_revision
            .checked_add(1)
            .ok_or_else(|| EventAppendError::CorruptOutcome("stream revision overflow".into()))?;
        let event_id = proposal
            .requested_event_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
        let appended_at_ms = now_ms()?;
        let causal_context_json = String::from_utf8(canonical_json(&proposal.causal_context)?)
            .map_err(|error| EventAppendError::CorruptOutcome(error.to_string()))?;
        let causal_context_digest = sha256_json(&proposal.causal_context)?;

        let mut envelope = EventEnvelopeV2 {
            event_id,
            event_truth_generation: generation,
            stream_namespace: authority_scope_digest.clone(),
            stream_revision,
            global_position,
            event_type: proposal.event_type.clone(),
            schema_version: proposal.schema_version,
            payload_codec: proposal.payload_codec,
            payload_digest: proposal.payload_digest.clone(),
            payload: proposal.payload.clone(),
            causal_context: proposal.causal_context.clone(),
            producer: proposal.producer.clone(),
            owner_term: proposal.owner_term.clone(),
            tick: proposal.tick,
            appended_at_ms,
            durability: proposal.requested_durability,
            canonical_request_digest: request_digest.clone(),
            append_receipt_digest: String::new(),
            sealed_envelope_digest: String::new(),
        };
        envelope.append_receipt_digest = envelope.expected_append_receipt_digest()?;
        envelope.sealed_envelope_digest = envelope.expected_sealed_envelope_digest()?;
        let outcome_digest = committed_outcome_digest(&envelope)?;
        let owner_term_json = proposal
            .owner_term
            .as_ref()
            .map(canonical_json)
            .transpose()?
            .map(String::from_utf8)
            .transpose()
            .map_err(|error| EventAppendError::CorruptOutcome(error.to_string()))?;

        transaction.execute(
            "INSERT INTO events_v2 (
                event_id, event_truth_generation, stream_namespace, stream_revision,
                global_position, event_type, schema_version, payload_codec,
                payload_digest, payload, causal_context_json, causal_context_digest,
                producer, owner_term_json, tick, appended_at_ms, durability,
                canonical_request_digest, append_receipt_digest, sealed_envelope_digest
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20
             )",
            params![
                envelope.event_id,
                sql_i64(envelope.event_truth_generation)?,
                envelope.stream_namespace,
                sql_i64(envelope.stream_revision)?,
                sql_i64(envelope.global_position)?,
                envelope.event_type,
                i64::from(envelope.schema_version),
                codec_name(envelope.payload_codec),
                envelope.payload_digest,
                envelope.payload,
                causal_context_json,
                causal_context_digest,
                envelope.producer,
                owner_term_json,
                envelope.tick.map(sql_i64).transpose()?,
                envelope.appended_at_ms,
                durability_name(envelope.durability),
                envelope.canonical_request_digest,
                envelope.append_receipt_digest,
                envelope.sealed_envelope_digest,
            ],
        )?;

        for intent in &proposal.delivery_intents {
            transaction.execute(
                "INSERT INTO delivery_intents_v2 (
                    intent_id, event_id, authority_scope_digest, causal_context_digest,
                    topic, payload_digest, status
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending')",
                params![
                    intent.intent_id,
                    envelope.event_id,
                    authority_scope_digest,
                    causal_context_digest,
                    intent.topic,
                    intent.payload_digest,
                ],
            )?;
        }
        for effect in &proposal.effect_reservations {
            transaction.execute(
                "INSERT INTO local_effect_reservations_v2 (
                    effect_id, event_id, authority_scope_digest, causal_context_digest,
                    effect_kind, request_digest, status
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'reserved')",
                params![
                    effect.effect_id,
                    envelope.event_id,
                    authority_scope_digest,
                    causal_context_digest,
                    effect.effect_kind,
                    effect.request_digest,
                ],
            )?;
        }
        transaction.execute(
            "INSERT INTO event_operations_v2 (
                authority_scope_digest, operation_id, canonical_request_digest,
                event_id, outcome_digest
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                authority_scope_digest,
                proposal.causal_context.operation_id,
                request_digest,
                envelope.event_id,
                outcome_digest,
            ],
        )?;
        transaction.execute(
            "INSERT INTO event_stream_heads_v2 (stream_namespace, stream_revision)
             VALUES (?1, ?2)
             ON CONFLICT(stream_namespace) DO UPDATE SET stream_revision = excluded.stream_revision",
            params![authority_scope_digest, sql_i64(stream_revision)?],
        )?;
        transaction.execute(
            "UPDATE event_truth_metadata SET next_global_position = ?1 WHERE singleton_id = 1",
            params![sql_i64(global_position.checked_add(1).ok_or_else(
                || { EventAppendError::CorruptOutcome("global position overflow".into()) }
            )?)?],
        )?;
        #[cfg(test)]
        abort_at_test_stage("before_commit");
        transaction.commit()?;
        #[cfg(test)]
        abort_at_test_stage("after_commit_before_reply");

        Ok(AppendOutcomeV2 {
            disposition: AppendDispositionV2::Appended,
            envelope,
            outcome_digest,
        })
    }

    fn lookup_operation(
        &self,
        authority_scope_digest: &str,
        operation_id: &str,
        request_digest: &str,
    ) -> Result<Option<AppendOutcomeV2>, EventAppendError> {
        let conn = self
            .store
            .conn
            .lock()
            .map_err(|error| EventAppendError::CorruptOutcome(error.to_string()))?;
        lookup_operation_in(&conn, authority_scope_digest, operation_id, request_digest)
    }
}

#[cfg(test)]
fn abort_at_test_stage(stage: &str) {
    if std::env::var("SENTINEL_EVENT_APPEND_TEST_ABORT_STAGE").as_deref() == Ok(stage) {
        std::process::abort();
    }
}

pub(crate) fn apply_event_contract_migrations(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    let migration_digest = sentinel_common::sha256_hex(EVENT_V2_MIGRATION.as_bytes());
    let migration_ledger_exists = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'event_schema_migrations')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    let existing = if migration_ledger_exists {
        conn.query_row(
            "SELECT name, sha256 FROM event_schema_migrations WHERE version = ?1",
            params![EVENT_SCHEMA_VERSION],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
    } else {
        None
    };
    if let Some((name, digest)) = existing {
        anyhow::ensure!(
            name == EVENT_V2_MIGRATION_NAME && digest == migration_digest,
            "event schema migration checksum mismatch for version {EVENT_SCHEMA_VERSION}"
        );
        return Ok(());
    }
    let transaction = conn.unchecked_transaction()?;
    transaction.execute_batch(CREATE_EVENT_SCHEMA_MIGRATIONS)?;
    transaction.execute_batch(EVENT_V2_MIGRATION)?;
    transaction.execute(
        "INSERT INTO event_schema_migrations (version, name, sha256, applied_at_ms)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            EVENT_SCHEMA_VERSION,
            EVENT_V2_MIGRATION_NAME,
            migration_digest,
            now_ms().map_err(anyhow::Error::from)?,
        ],
    )?;
    transaction.commit()?;
    Ok(())
}

pub(crate) fn verify_event_contract_schema(
    conn: &rusqlite::Connection,
) -> anyhow::Result<EventContractSchemaStatus> {
    let expected_digest = sentinel_common::sha256_hex(EVENT_V2_MIGRATION.as_bytes());
    let (name, digest) = conn
        .query_row(
            "SELECT name, sha256 FROM event_schema_migrations WHERE version = ?1",
            params![EVENT_SCHEMA_VERSION],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .map_err(|error| anyhow::anyhow!("event schema migration v2 is unavailable: {error}"))?;
    anyhow::ensure!(
        name == EVENT_V2_MIGRATION_NAME && digest == expected_digest,
        "event schema migration checksum mismatch for version {EVENT_SCHEMA_VERSION}"
    );
    let schema_fingerprint = event_contract_schema_fingerprint(conn)?;
    anyhow::ensure!(
        schema_fingerprint == EVENT_V2_SCHEMA_FINGERPRINT,
        "event contract schema object fingerprint mismatch: expected {}, found {}",
        EVENT_V2_SCHEMA_FINGERPRINT,
        schema_fingerprint
    );
    for table in [
        "event_truth_metadata",
        "event_stream_heads_v2",
        "events_v2",
        "event_operations_v2",
        "delivery_intents_v2",
        "local_effect_reservations_v2",
    ] {
        let exists = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
            params![table],
            |row| row.get::<_, bool>(0),
        )?;
        anyhow::ensure!(exists, "event contract table {table} is missing");
    }
    for (table, required_columns) in [
        (
            "event_truth_metadata",
            &[
                "singleton_id",
                "schema_version",
                "event_truth_generation",
                "next_global_position",
            ][..],
        ),
        (
            "event_stream_heads_v2",
            &["stream_namespace", "stream_revision"][..],
        ),
        (
            "events_v2",
            &[
                "event_id",
                "event_truth_generation",
                "stream_namespace",
                "stream_revision",
                "global_position",
                "event_type",
                "schema_version",
                "payload_codec",
                "payload_digest",
                "payload",
                "causal_context_json",
                "causal_context_digest",
                "producer",
                "owner_term_json",
                "tick",
                "appended_at_ms",
                "durability",
                "canonical_request_digest",
                "append_receipt_digest",
                "sealed_envelope_digest",
            ][..],
        ),
        (
            "event_operations_v2",
            &[
                "authority_scope_digest",
                "operation_id",
                "canonical_request_digest",
                "event_id",
                "outcome_digest",
            ][..],
        ),
        (
            "delivery_intents_v2",
            &[
                "intent_id",
                "event_id",
                "authority_scope_digest",
                "causal_context_digest",
                "topic",
                "payload_digest",
                "status",
            ][..],
        ),
        (
            "local_effect_reservations_v2",
            &[
                "effect_id",
                "event_id",
                "authority_scope_digest",
                "causal_context_digest",
                "effect_kind",
                "request_digest",
                "status",
            ][..],
        ),
    ] {
        let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let actual = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
        for column in required_columns {
            anyhow::ensure!(
                actual.contains(*column),
                "event contract table {table} is missing column {column}"
            );
        }
    }
    let (schema_version, generation, next_position) = conn.query_row(
        "SELECT schema_version, event_truth_generation, next_global_position
         FROM event_truth_metadata WHERE singleton_id = 1",
        [],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        },
    )?;
    anyhow::ensure!(
        schema_version == EVENT_SCHEMA_VERSION,
        "event truth schema metadata does not match migration version"
    );
    let event_truth_generation = sql_u64(generation).map_err(anyhow::Error::from)?;
    let next_global_position = sql_u64(next_position).map_err(anyhow::Error::from)?;
    anyhow::ensure!(
        event_truth_generation > 0 && next_global_position > 0,
        "event truth counters must be positive"
    );
    Ok(EventContractSchemaStatus {
        schema_version: u32::try_from(schema_version)?,
        migration_name: name,
        migration_sha256: digest,
        event_truth_generation,
        next_global_position,
    })
}

fn event_contract_schema_fingerprint(conn: &rusqlite::Connection) -> anyhow::Result<String> {
    let mut statement = conn.prepare(
        "SELECT type, name, tbl_name, COALESCE(sql, '')
         FROM sqlite_schema
         WHERE tbl_name IN (
             'event_schema_migrations',
             'event_truth_metadata',
             'event_stream_heads_v2',
             'events_v2',
             'event_operations_v2',
             'delivery_intents_v2',
             'local_effect_reservations_v2'
         )
         ORDER BY type, name, tbl_name",
    )?;
    let rows = statement.query_map([], |row| {
        Ok([
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ])
    })?;
    let mut canonical = Vec::new();
    for row in rows {
        for value in row? {
            canonical.extend_from_slice(value.len().to_string().as_bytes());
            canonical.push(b':');
            canonical.extend_from_slice(value.as_bytes());
            canonical.push(b'\n');
        }
    }
    Ok(sentinel_common::sha256_hex(&canonical))
}

fn validate_caller(
    caller: &AuthenticatedEventCallerV1,
    proposal: &AppendProposalV2,
) -> Result<(), EventAppendError> {
    validate_sha256(
        "caller.authority_scope_digest",
        &caller.authority_scope_digest,
    )?;
    let expected_scope = proposal.causal_context.authority_scope_digest()?;
    if caller.service_id.is_empty()
        || caller.service_id.len() > 128
        || caller.producer != proposal.producer
        || caller.authority_scope_digest != expected_scope
    {
        return Err(EventAppendError::UnauthorizedCaller);
    }
    Ok(())
}

fn lookup_operation_in(
    conn: &rusqlite::Connection,
    authority_scope_digest: &str,
    operation_id: &str,
    request_digest: &str,
) -> Result<Option<AppendOutcomeV2>, EventAppendError> {
    let operation = conn
        .query_row(
            "SELECT canonical_request_digest, event_id, outcome_digest
             FROM event_operations_v2
             WHERE authority_scope_digest = ?1 AND operation_id = ?2",
            params![authority_scope_digest, operation_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((stored_request_digest, event_id, outcome_digest)) = operation else {
        return Ok(None);
    };
    if stored_request_digest != request_digest {
        return Err(EventAppendError::OperationConflict);
    }
    let envelope = read_envelope_by_id(conn, &event_id)?.ok_or_else(|| {
        EventAppendError::CorruptOutcome("operation references a missing event".to_string())
    })?;
    if envelope.canonical_request_digest != stored_request_digest
        || committed_outcome_digest(&envelope)? != outcome_digest
        || envelope.expected_sealed_envelope_digest()? != envelope.sealed_envelope_digest
    {
        return Err(EventAppendError::CorruptOutcome(
            "stored digest binding does not verify".to_string(),
        ));
    }
    Ok(Some(AppendOutcomeV2 {
        disposition: AppendDispositionV2::ReplayOfPriorOperation,
        envelope,
        outcome_digest,
    }))
}

#[derive(Debug)]
struct StoredEnvelopeRow {
    event_id: String,
    event_truth_generation: i64,
    stream_namespace: String,
    stream_revision: i64,
    global_position: i64,
    event_type: String,
    schema_version: i64,
    payload_codec: String,
    payload_digest: String,
    payload: Vec<u8>,
    causal_context_json: String,
    causal_context_digest: String,
    producer: String,
    owner_term_json: Option<String>,
    tick: Option<i64>,
    appended_at_ms: i64,
    durability: String,
    canonical_request_digest: String,
    append_receipt_digest: String,
    sealed_envelope_digest: String,
}

fn read_envelope_by_id(
    conn: &rusqlite::Connection,
    event_id: &str,
) -> Result<Option<EventEnvelopeV2>, EventAppendError> {
    let stored = conn
        .query_row(
            "SELECT event_id, event_truth_generation, stream_namespace, stream_revision,
                    global_position, event_type, schema_version, payload_codec,
                    payload_digest, payload, causal_context_json, causal_context_digest, producer,
                    owner_term_json, tick, appended_at_ms, durability,
                    canonical_request_digest, append_receipt_digest, sealed_envelope_digest
             FROM events_v2 WHERE event_id = ?1",
            params![event_id],
            |row| {
                Ok(StoredEnvelopeRow {
                    event_id: row.get(0)?,
                    event_truth_generation: row.get(1)?,
                    stream_namespace: row.get(2)?,
                    stream_revision: row.get(3)?,
                    global_position: row.get(4)?,
                    event_type: row.get(5)?,
                    schema_version: row.get(6)?,
                    payload_codec: row.get(7)?,
                    payload_digest: row.get(8)?,
                    payload: row.get(9)?,
                    causal_context_json: row.get(10)?,
                    causal_context_digest: row.get(11)?,
                    producer: row.get(12)?,
                    owner_term_json: row.get(13)?,
                    tick: row.get(14)?,
                    appended_at_ms: row.get(15)?,
                    durability: row.get(16)?,
                    canonical_request_digest: row.get(17)?,
                    append_receipt_digest: row.get(18)?,
                    sealed_envelope_digest: row.get(19)?,
                })
            },
        )
        .optional()?;
    stored.map(decode_envelope).transpose()
}

fn read_legacy_envelope_by_id(
    conn: &rusqlite::Connection,
    event_id: &str,
) -> Result<Option<LegacyEventEnvelopeV1>, EventAppendError> {
    conn.query_row(
        "SELECT event_id, event_type, aggregate_id, payload, correlation_id,
                causation_id, operation_id, tick, timestamp_ms, schema_version,
                compensation_type
         FROM events WHERE event_id = ?1",
        params![event_id],
        |row| {
            let tick = row.get::<_, i64>(7)?;
            let timestamp_ms = row.get::<_, i64>(8)?;
            let schema_version = row.get::<_, i64>(9)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
                tick,
                timestamp_ms,
                schema_version,
                row.get::<_, String>(10)?,
            ))
        },
    )
    .optional()?
    .map(
        |(
            event_id,
            event_type,
            aggregate_id,
            payload,
            correlation_id,
            causation_id,
            operation_id,
            tick,
            timestamp_ms,
            schema_version,
            compensation_type,
        )| {
            Ok(LegacyEventEnvelopeV1 {
                event_id,
                event_type,
                aggregate_id,
                payload,
                correlation_id,
                causation_id,
                operation_id,
                tick: sql_u64(tick)?,
                timestamp_ms: sql_u64(timestamp_ms)?,
                schema_version: u32::try_from(schema_version).map_err(|_| {
                    EventAppendError::CorruptOutcome("invalid V1 schema version".into())
                })?,
                compensation_type,
                authority_state: DecodedEventAuthorityState::UnknownV1NonAuthorizing,
            })
        },
    )
    .transpose()
}

fn decode_envelope(stored: StoredEnvelopeRow) -> Result<EventEnvelopeV2, EventAppendError> {
    let causal_context = serde_json::from_str(&stored.causal_context_json)
        .map_err(|error| EventAppendError::CorruptOutcome(error.to_string()))?;
    let owner_term = stored
        .owner_term_json
        .map(|value| serde_json::from_str(&value))
        .transpose()
        .map_err(|error| EventAppendError::CorruptOutcome(error.to_string()))?;
    let schema_version = u32::try_from(stored.schema_version)
        .map_err(|_| EventAppendError::CorruptOutcome("invalid schema version".into()))?;
    let tick = stored.tick.map(sql_u64).transpose()?;
    let envelope = EventEnvelopeV2 {
        event_id: stored.event_id,
        event_truth_generation: sql_u64(stored.event_truth_generation)?,
        stream_namespace: stored.stream_namespace,
        stream_revision: sql_u64(stored.stream_revision)?,
        global_position: sql_u64(stored.global_position)?,
        event_type: stored.event_type,
        schema_version,
        payload_codec: codec_from_name(&stored.payload_codec)?,
        payload_digest: stored.payload_digest,
        payload: stored.payload,
        causal_context,
        producer: stored.producer,
        owner_term,
        tick,
        appended_at_ms: stored.appended_at_ms,
        durability: durability_from_name(&stored.durability)?,
        canonical_request_digest: stored.canonical_request_digest,
        append_receipt_digest: stored.append_receipt_digest,
        sealed_envelope_digest: stored.sealed_envelope_digest,
    };
    verify_stored_envelope(&envelope, &stored.causal_context_digest)?;
    Ok(envelope)
}

fn verify_stored_envelope(
    envelope: &EventEnvelopeV2,
    stored_causal_context_digest: &str,
) -> Result<(), EventAppendError> {
    envelope.validate_seals()?;
    if sha256_json(&envelope.causal_context)? != stored_causal_context_digest {
        return Err(EventAppendError::CorruptOutcome(
            "stored causal-context digest does not verify".to_string(),
        ));
    }
    Ok(())
}

fn validate_expected_revision(
    expected: ExpectedStreamRevision,
    actual: u64,
) -> Result<(), EventAppendError> {
    let accepted = match expected {
        ExpectedStreamRevision::NoStream => actual == 0,
        ExpectedStreamRevision::Exact(value) => value == actual,
    };
    if accepted {
        return Ok(());
    }
    Err(EventAppendError::WrongExpectedRevision {
        expected: match expected {
            ExpectedStreamRevision::NoStream => "no_stream".to_string(),
            ExpectedStreamRevision::Exact(value) => value.to_string(),
        },
        actual,
    })
}

fn validate_causation_in(
    conn: &rusqlite::Connection,
    proposal: &AppendProposalV2,
    policy: CausationPolicyV1,
    generation: u64,
    next_global_position: u64,
) -> Result<(), EventAppendError> {
    let CausationPolicyV1::DirectRequired = policy else {
        return Ok(());
    };
    let causation_event_id = proposal
        .causal_context
        .causation_event_id
        .as_deref()
        .ok_or(EventAppendError::InvalidCausationEvent)?;
    let cause = read_envelope_by_id(conn, causation_event_id)?
        .ok_or(EventAppendError::InvalidCausationEvent)?;
    if cause.event_truth_generation != generation
        || cause.global_position >= next_global_position
        || cause.causal_context.tenant != proposal.causal_context.tenant
        || cause.causal_context.company != proposal.causal_context.company
        || cause.causal_context.project != proposal.causal_context.project
    {
        return Err(EventAppendError::InvalidCausationEvent);
    }
    Ok(())
}

fn committed_outcome_digest(envelope: &EventEnvelopeV2) -> Result<String, EventContractError> {
    #[derive(Serialize)]
    struct Outcome<'a> {
        canonical_request_digest: &'a str,
        sealed_envelope_digest: &'a str,
        append_receipt_digest: &'a str,
        stream_revision: u64,
        global_position: u64,
    }
    sha256_json(&Outcome {
        canonical_request_digest: &envelope.canonical_request_digest,
        sealed_envelope_digest: &envelope.sealed_envelope_digest,
        append_receipt_digest: &envelope.append_receipt_digest,
        stream_revision: envelope.stream_revision,
        global_position: envelope.global_position,
    })
}

fn codec_name(codec: EventPayloadCodec) -> &'static str {
    match codec {
        EventPayloadCodec::Json => "json",
        EventPayloadCodec::DeterministicCbor => "deterministic_cbor",
    }
}

fn codec_from_name(value: &str) -> Result<EventPayloadCodec, EventAppendError> {
    match value {
        "json" => Ok(EventPayloadCodec::Json),
        "deterministic_cbor" => Ok(EventPayloadCodec::DeterministicCbor),
        _ => Err(EventAppendError::CorruptOutcome(
            "unknown payload codec".to_string(),
        )),
    }
}

fn durability_name(durability: EventDurability) -> &'static str {
    match durability {
        EventDurability::Authoritative => "authoritative",
        EventDurability::DurableOperational => "durable_operational",
        EventDurability::RebuildableTelemetry => "rebuildable_telemetry",
    }
}

fn durability_from_name(value: &str) -> Result<EventDurability, EventAppendError> {
    match value {
        "authoritative" => Ok(EventDurability::Authoritative),
        "durable_operational" => Ok(EventDurability::DurableOperational),
        "rebuildable_telemetry" => Ok(EventDurability::RebuildableTelemetry),
        _ => Err(EventAppendError::CorruptOutcome(
            "unknown event durability".to_string(),
        )),
    }
}

fn now_ms() -> Result<i64, EventAppendError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| EventAppendError::CorruptOutcome(error.to_string()))?
        .as_millis();
    i64::try_from(millis)
        .map_err(|_| EventAppendError::CorruptOutcome("wall clock overflow".to_string()))
}

fn sql_i64(value: u64) -> Result<i64, EventAppendError> {
    i64::try_from(value)
        .map_err(|_| EventAppendError::CorruptOutcome("unsigned value exceeds SQLite".into()))
}

fn sql_u64(value: i64) -> Result<u64, EventAppendError> {
    u64::try_from(value)
        .map_err(|_| EventAppendError::CorruptOutcome("negative SQLite counter".into()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::{Arc, Barrier};

    use sentinel_common::{
        sha256_hex, validate_json_object_payload, AuthorityKindV1, AuthorityRefV1, CausalContextV1,
        DeliveryIntentV1, EventSchemaDefinition, LocalEffectReservationV1,
        CAUSAL_CONTEXT_VERSION_V1, EVENT_PROPOSAL_VERSION_V2,
    };

    use super::*;

    fn digest(value: &str) -> String {
        sha256_hex(value.as_bytes())
    }

    fn authority(kind: AuthorityKindV1, id: &str) -> AuthorityRefV1 {
        AuthorityRefV1 {
            kind,
            id: id.to_string(),
            authority_generation: 1,
            authority_digest: digest(id),
        }
    }

    fn proposal(operation_id: &str, expected: ExpectedStreamRevision) -> AppendProposalV2 {
        let payload = br#"{"status":"accepted"}"#.to_vec();
        AppendProposalV2 {
            proposal_version: EVENT_PROPOSAL_VERSION_V2,
            requested_event_id: None,
            event_type: "project_accepted".to_string(),
            schema_version: 1,
            payload_codec: EventPayloadCodec::Json,
            payload_digest: sha256_hex(&payload),
            payload,
            causal_context: CausalContextV1 {
                schema_version: CAUSAL_CONTEXT_VERSION_V1,
                tenant: authority(AuthorityKindV1::Tenant, "tenant-a"),
                company: authority(AuthorityKindV1::Company, "company-a"),
                project: authority(AuthorityKindV1::Project, "project-a"),
                workflow: Some(authority(AuthorityKindV1::Workflow, "workflow-a")),
                work_item: None,
                request_id: "request-a".to_string(),
                request_digest: digest("request-a"),
                correlation_id: "correlation-a".to_string(),
                causation_event_id: None,
                operation_id: operation_id.to_string(),
                attempt: 1,
                source_generation: 1,
                source_digest: digest("source-a"),
                invocation_id: None,
                agent_id: None,
                tick: Some(7),
                artifact_id: None,
                artifact_digest: None,
                qa_run_id: None,
                release_id: None,
                delivery_id: None,
                diagnostic_trace_id: None,
                diagnostic_span_id: None,
            },
            producer: "workflow".to_string(),
            owner_term: None,
            tick: Some(7),
            requested_durability: EventDurability::Authoritative,
            expected_stream_revision: expected,
            delivery_intents: vec![DeliveryIntentV1 {
                intent_id: format!("delivery-{operation_id}"),
                topic: "sentinel/workflow".to_string(),
                payload_digest: digest("delivery"),
            }],
            effect_reservations: vec![LocalEffectReservationV1 {
                effect_id: format!("effect-{operation_id}"),
                effect_kind: "workbench".to_string(),
                request_digest: digest("effect"),
            }],
        }
    }

    fn registry() -> EventSchemaRegistry {
        EventSchemaRegistry::new([EventSchemaDefinition {
            event_type: "project_accepted".to_string(),
            schema_version: 1,
            durability: EventDurability::Authoritative,
            payload_codec: EventPayloadCodec::Json,
            causation_policy: CausationPolicyV1::RootRequired,
            allowed_producers: BTreeSet::from(["workflow".to_string()]),
            deterministic_event_id_producers: BTreeSet::new(),
            validator_id: "project-accepted-v1".to_string(),
            validate_payload: validate_json_object_payload,
            upcast: None,
        }])
        .unwrap()
    }

    fn registry_with_direct_event() -> EventSchemaRegistry {
        EventSchemaRegistry::new([
            EventSchemaDefinition {
                event_type: "project_accepted".to_string(),
                schema_version: 1,
                durability: EventDurability::Authoritative,
                payload_codec: EventPayloadCodec::Json,
                causation_policy: CausationPolicyV1::RootRequired,
                allowed_producers: BTreeSet::from(["workflow".to_string()]),
                deterministic_event_id_producers: BTreeSet::new(),
                validator_id: "project-accepted-v1".to_string(),
                validate_payload: validate_json_object_payload,
                upcast: None,
            },
            EventSchemaDefinition {
                event_type: "project_followup".to_string(),
                schema_version: 1,
                durability: EventDurability::Authoritative,
                payload_codec: EventPayloadCodec::Json,
                causation_policy: CausationPolicyV1::DirectRequired,
                allowed_producers: BTreeSet::from(["workflow".to_string()]),
                deterministic_event_id_producers: BTreeSet::new(),
                validator_id: "project-followup-v1".to_string(),
                validate_payload: validate_json_object_payload,
                upcast: None,
            },
        ])
        .unwrap()
    }

    fn caller(proposal: &AppendProposalV2) -> AuthenticatedEventCallerV1 {
        AuthenticatedEventCallerV1 {
            service_id: "sentinel-daemon".to_string(),
            producer: proposal.producer.clone(),
            authority_scope_digest: proposal.causal_context.authority_scope_digest().unwrap(),
        }
    }

    #[test]
    fn append_seals_store_fields_and_commits_intents_atomically() {
        let store = EventStore::open(":memory:").unwrap();
        let registry = registry();
        let proposal = proposal("operation-a", ExpectedStreamRevision::NoStream);
        let outcome = store
            .append_gateway(&registry)
            .append(&caller(&proposal), &proposal)
            .unwrap();
        assert_eq!(outcome.disposition, AppendDispositionV2::Appended);
        assert_eq!(outcome.envelope.stream_revision, 1);
        assert_eq!(outcome.envelope.global_position, 1);
        assert_eq!(
            uuid::Uuid::parse_str(&outcome.envelope.event_id)
                .unwrap()
                .get_version_num(),
            7
        );

        let conn = store.conn.lock().unwrap();
        let counts = (
            conn.query_row("SELECT count(*) FROM events_v2", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            conn.query_row("SELECT count(*) FROM delivery_intents_v2", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            conn.query_row(
                "SELECT count(*) FROM local_effect_reservations_v2",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        );
        assert_eq!(counts, (1, 1, 1));
    }

    #[test]
    fn exact_replay_precedes_revision_check_and_conflict_is_typed() {
        let store = EventStore::open(":memory:").unwrap();
        let registry = registry();
        let first = proposal("operation-a", ExpectedStreamRevision::NoStream);
        let appended = store
            .append_gateway(&registry)
            .append(&caller(&first), &first)
            .unwrap();

        let second = proposal("operation-b", ExpectedStreamRevision::Exact(1));
        store
            .append_gateway(&registry)
            .append(&caller(&second), &second)
            .unwrap();

        let replay = store
            .append_gateway(&registry)
            .append(&caller(&first), &first)
            .unwrap();
        assert_eq!(
            replay.disposition,
            AppendDispositionV2::ReplayOfPriorOperation
        );
        assert_eq!(replay.envelope, appended.envelope);
        assert_eq!(replay.outcome_digest, appended.outcome_digest);

        let mut conflict = first.clone();
        conflict.tick = Some(8);
        conflict.causal_context.tick = Some(8);
        assert!(matches!(
            store
                .append_gateway(&registry)
                .append(&caller(&conflict), &conflict),
            Err(EventAppendError::OperationConflict)
        ));
    }

    #[test]
    fn direct_causation_requires_a_committed_event_in_the_same_project() {
        let store = EventStore::open(":memory:").unwrap();
        let registry = registry_with_direct_event();
        let root = proposal("operation-root", ExpectedStreamRevision::NoStream);
        let root_outcome = store
            .append_gateway(&registry)
            .append(&caller(&root), &root)
            .unwrap();

        let mut invented = proposal("operation-invented", ExpectedStreamRevision::Exact(1));
        invented.event_type = "project_followup".to_string();
        invented.causal_context.causation_event_id =
            Some("01890f3d-0000-7000-8000-000000000099".to_string());
        assert!(matches!(
            store
                .append_gateway(&registry)
                .append(&caller(&invented), &invented),
            Err(EventAppendError::InvalidCausationEvent)
        ));

        let mut cross_project = invented.clone();
        cross_project.causal_context.operation_id = "operation-cross-project".to_string();
        cross_project.causal_context.project = authority(AuthorityKindV1::Project, "project-b");
        cross_project.causal_context.causation_event_id =
            Some(root_outcome.envelope.event_id.clone());
        cross_project.expected_stream_revision = ExpectedStreamRevision::NoStream;
        assert!(matches!(
            store
                .append_gateway(&registry)
                .append(&caller(&cross_project), &cross_project),
            Err(EventAppendError::InvalidCausationEvent)
        ));

        let mut valid = invented;
        valid.causal_context.operation_id = "operation-direct".to_string();
        valid.causal_context.causation_event_id = Some(root_outcome.envelope.event_id);
        let outcome = store
            .append_gateway(&registry)
            .append(&caller(&valid), &valid)
            .unwrap();
        assert_eq!(outcome.envelope.stream_revision, 2);
    }

    #[test]
    fn wrong_revision_and_cross_scope_replay_do_not_mutate() {
        let store = EventStore::open(":memory:").unwrap();
        let registry = registry();
        let first = proposal("operation-a", ExpectedStreamRevision::NoStream);
        store
            .append_gateway(&registry)
            .append(&caller(&first), &first)
            .unwrap();

        let wrong = proposal("operation-b", ExpectedStreamRevision::NoStream);
        assert!(matches!(
            store
                .append_gateway(&registry)
                .append(&caller(&wrong), &wrong),
            Err(EventAppendError::WrongExpectedRevision { actual: 1, .. })
        ));

        let mut rebound = first.clone();
        rebound.causal_context.project = authority(AuthorityKindV1::Project, "project-b");
        let original_caller = caller(&first);
        assert!(matches!(
            store
                .append_gateway(&registry)
                .append(&original_caller, &rebound),
            Err(EventAppendError::UnauthorizedCaller)
        ));
        let conn = store.conn.lock().unwrap();
        assert_eq!(
            conn.query_row("SELECT count(*) FROM events_v2", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn migration_checksum_is_verified_fail_closed() {
        let store = EventStore::open(":memory:").unwrap();
        {
            let conn = store.conn.lock().unwrap();
            conn.execute(
                "UPDATE event_schema_migrations SET sha256 = ?1 WHERE version = ?2",
                params!["0".repeat(64), EVENT_SCHEMA_VERSION],
            )
            .unwrap();
            assert!(apply_event_contract_migrations(&conn).is_err());
        }
    }

    #[test]
    fn failed_v2_migration_does_not_publish_its_ledger_entry() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE events_v2 (invalid INTEGER)")
            .unwrap();
        assert!(apply_event_contract_migrations(&conn).is_err());
        let ledger_exists = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'event_schema_migrations')",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap();
        assert!(!ledger_exists);
    }

    #[test]
    fn compatibility_reader_never_fabricates_v2_authority_for_legacy_rows() {
        let store = EventStore::open(":memory:").unwrap();
        let legacy = sentinel_common::DomainEvent::new(
            "legacy_event",
            "legacy-aggregate",
            r#"{"legacy":true}"#,
            "legacy-correlation",
            9,
        );
        store.append_event(&legacy).unwrap();

        let decoded = store
            .event_by_id_compatible(&legacy.event_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            decoded.authority_state(),
            DecodedEventAuthorityState::UnknownV1NonAuthorizing
        );
        let CompatibleEventEnvelope::V1(decoded) = decoded else {
            panic!("legacy row was promoted to a V2 envelope")
        };
        assert_eq!(decoded.event_id, legacy.event_id);
        assert_eq!(decoded.operation_id, legacy.operation_id);
        assert_eq!(
            decoded.authority_state,
            DecodedEventAuthorityState::UnknownV1NonAuthorizing
        );
    }

    #[test]
    fn downstream_constraint_failure_rolls_back_event_intent_effect_and_heads() {
        let store = EventStore::open(":memory:").unwrap();
        let registry = registry();
        let first = proposal("operation-a", ExpectedStreamRevision::NoStream);
        store
            .append_gateway(&registry)
            .append(&caller(&first), &first)
            .unwrap();

        let mut second = proposal("operation-b", ExpectedStreamRevision::Exact(1));
        second.delivery_intents[0].intent_id = first.delivery_intents[0].intent_id.clone();
        assert!(matches!(
            store
                .append_gateway(&registry)
                .append(&caller(&second), &second),
            Err(EventAppendError::Sqlite(_))
        ));

        let conn = store.conn.lock().unwrap();
        for table in [
            "events_v2",
            "event_operations_v2",
            "delivery_intents_v2",
            "local_effect_reservations_v2",
        ] {
            let count = conn
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap();
            assert_eq!(count, 1, "partial transaction state in {table}");
        }
        assert_eq!(
            conn.query_row(
                "SELECT stream_revision FROM event_stream_heads_v2",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row(
                "SELECT next_global_position FROM event_truth_metadata WHERE singleton_id = 1",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
    }

    #[test]
    fn every_transaction_boundary_rolls_back_without_partial_visibility() {
        let stages = [
            ("events", "INSERT", "events_v2"),
            ("delivery", "INSERT", "delivery_intents_v2"),
            ("effect", "INSERT", "local_effect_reservations_v2"),
            ("operation", "INSERT", "event_operations_v2"),
            ("head", "INSERT", "event_stream_heads_v2"),
            ("position", "UPDATE", "event_truth_metadata"),
        ];
        for (name, operation, table) in stages {
            let store = EventStore::open(":memory:").unwrap();
            let registry = registry();
            store
                .conn
                .lock()
                .unwrap()
                .execute_batch(&format!(
                    "CREATE TEMP TRIGGER fail_{name} BEFORE {operation} ON {table}
                     BEGIN SELECT RAISE(ABORT, 'injected {name} failure'); END;"
                ))
                .unwrap();
            let proposal = proposal(
                &format!("operation-{name}"),
                ExpectedStreamRevision::NoStream,
            );
            assert!(matches!(
                store
                    .append_gateway(&registry)
                    .append(&caller(&proposal), &proposal),
                Err(EventAppendError::Sqlite(_))
            ));
            let conn = store.conn.lock().unwrap();
            for table in [
                "events_v2",
                "event_operations_v2",
                "delivery_intents_v2",
                "local_effect_reservations_v2",
                "event_stream_heads_v2",
            ] {
                assert_eq!(
                    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .unwrap(),
                    0,
                    "{name} failure exposed partial state in {table}"
                );
            }
            assert_eq!(
                conn.query_row(
                    "SELECT next_global_position FROM event_truth_metadata WHERE singleton_id = 1",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
                1,
                "{name} failure advanced the global position"
            );
        }
    }

    #[test]
    fn readonly_io_failure_exposes_no_partial_append_state() {
        let store = EventStore::open(":memory:").unwrap();
        let registry = registry();
        store
            .conn
            .lock()
            .unwrap()
            .execute_batch("PRAGMA query_only = ON")
            .unwrap();
        let candidate = proposal("operation-readonly", ExpectedStreamRevision::NoStream);
        assert!(matches!(
            store
                .append_gateway(&registry)
                .append(&caller(&candidate), &candidate),
            Err(EventAppendError::Sqlite(_)) | Err(EventAppendError::Store(_))
        ));
        let conn = store.conn.lock().unwrap();
        assert_eq!(
            conn.query_row("SELECT count(*) FROM events_v2", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row(
                "SELECT next_global_position FROM event_truth_metadata WHERE singleton_id = 1",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn sqlite_full_failure_exposes_no_partial_append_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("events.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();
        let registry = registry();
        let page_count = store
            .conn
            .lock()
            .unwrap()
            .query_row("PRAGMA page_count", [], |row| row.get::<_, i64>(0))
            .unwrap();
        store
            .conn
            .lock()
            .unwrap()
            .execute_batch(&format!(
                "PRAGMA wal_checkpoint(TRUNCATE); PRAGMA max_page_count = {page_count};"
            ))
            .unwrap();
        let mut candidate = proposal("operation-full", ExpectedStreamRevision::NoStream);
        candidate.payload = serde_json::to_vec(&serde_json::json!({
            "padding": "x".repeat(512 * 1024)
        }))
        .unwrap();
        candidate.payload_digest = sentinel_common::sha256_hex(&candidate.payload);
        assert!(matches!(
            store
                .append_gateway(&registry)
                .append(&caller(&candidate), &candidate),
            Err(EventAppendError::Sqlite(_)) | Err(EventAppendError::Store(_))
        ));
        let conn = store.conn.lock().unwrap();
        assert_eq!(
            conn.query_row("SELECT count(*) FROM events_v2", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row(
                "SELECT next_global_position FROM event_truth_metadata WHERE singleton_id = 1",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn full_checkpoint_restart_preserves_exact_operation_replay() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("events.db");
        let proposal = proposal("operation-checkpoint", ExpectedStreamRevision::NoStream);
        let appended = {
            let store = EventStore::open(path.to_str().unwrap()).unwrap();
            let registry = registry();
            let appended = store
                .append_gateway(&registry)
                .append(&caller(&proposal), &proposal)
                .unwrap();
            store
                .conn
                .lock()
                .unwrap()
                .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
                .unwrap();
            appended
        };
        let store = EventStore::open_compatible(path.to_str().unwrap()).unwrap();
        let registry = registry();
        let replay = store
            .append_gateway(&registry)
            .append(&caller(&proposal), &proposal)
            .unwrap();
        assert_eq!(
            replay.disposition,
            AppendDispositionV2::ReplayOfPriorOperation
        );
        assert_eq!(replay.envelope, appended.envelope);
        assert_eq!(replay.outcome_digest, appended.outcome_digest);
    }

    #[test]
    fn child_process_crash_cut_distinguishes_uncommitted_from_durable_replay() {
        const CHILD_STAGE: &str = "SENTINEL_EVENT_APPEND_CHILD_STAGE";
        const CHILD_PATH: &str = "SENTINEL_EVENT_APPEND_CHILD_PATH";
        if let (Ok(stage), Ok(path)) = (std::env::var(CHILD_STAGE), std::env::var(CHILD_PATH)) {
            let store = EventStore::open(&path).unwrap();
            let registry = registry();
            let proposal = proposal("operation-crash", ExpectedStreamRevision::NoStream);
            let _ = store
                .append_gateway(&registry)
                .append(&caller(&proposal), &proposal);
            panic!("append failpoint {stage} did not abort the child process");
        }

        for (stage, committed) in [
            ("before_commit", false),
            ("after_commit_before_reply", true),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("events.db");
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg("event_gateway::tests::child_process_crash_cut_distinguishes_uncommitted_from_durable_replay")
                .arg("--nocapture")
                .env(CHILD_STAGE, stage)
                .env(CHILD_PATH, &path)
                .env("SENTINEL_EVENT_APPEND_TEST_ABORT_STAGE", stage)
                .status()
                .unwrap();
            assert!(!status.success(), "child did not crash at {stage}");

            let store = EventStore::open_compatible(path.to_str().unwrap()).unwrap();
            let conn = store.conn.lock().unwrap();
            let count = conn
                .query_row("SELECT count(*) FROM events_v2", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap();
            drop(conn);
            assert_eq!(count, i64::from(committed), "wrong WAL state at {stage}");
            if committed {
                let registry = registry();
                let proposal = proposal("operation-crash", ExpectedStreamRevision::NoStream);
                let replay = store
                    .append_gateway(&registry)
                    .append(&caller(&proposal), &proposal)
                    .unwrap();
                assert_eq!(
                    replay.disposition,
                    AppendDispositionV2::ReplayOfPriorOperation
                );
            }
        }
    }

    #[test]
    fn concurrent_same_operation_appends_once_and_replays_once() {
        let store = EventStore::open(":memory:").unwrap();
        let registry = registry();
        let proposal = proposal("operation-race", ExpectedStreamRevision::NoStream);
        let barrier = Arc::new(Barrier::new(3));
        let mut joins = Vec::new();
        for _ in 0..2 {
            let store = store.clone();
            let registry = registry.clone();
            let proposal = proposal.clone();
            let barrier = Arc::clone(&barrier);
            joins.push(std::thread::spawn(move || {
                let caller = caller(&proposal);
                barrier.wait();
                store.append_gateway(&registry).append(&caller, &proposal)
            }));
        }
        barrier.wait();
        let outcomes: Vec<_> = joins
            .into_iter()
            .map(|join| join.join().unwrap().unwrap())
            .collect();
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| outcome.disposition == AppendDispositionV2::Appended)
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| {
                    outcome.disposition == AppendDispositionV2::ReplayOfPriorOperation
                })
                .count(),
            1
        );
        assert_eq!(outcomes[0].envelope, outcomes[1].envelope);
        assert_eq!(outcomes[0].outcome_digest, outcomes[1].outcome_digest);
    }

    #[test]
    fn concurrent_new_operations_cannot_both_claim_the_same_revision() {
        let store = EventStore::open(":memory:").unwrap();
        let registry = registry();
        let barrier = Arc::new(Barrier::new(3));
        let proposals = [
            proposal("operation-race-a", ExpectedStreamRevision::NoStream),
            proposal("operation-race-b", ExpectedStreamRevision::NoStream),
        ];
        let mut joins = Vec::new();
        for proposal in proposals {
            let store = store.clone();
            let registry = registry.clone();
            let barrier = Arc::clone(&barrier);
            joins.push(std::thread::spawn(move || {
                let caller = caller(&proposal);
                barrier.wait();
                store.append_gateway(&registry).append(&caller, &proposal)
            }));
        }
        barrier.wait();
        let outcomes: Vec<_> = joins.into_iter().map(|join| join.join().unwrap()).collect();
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(
                    outcome,
                    Err(EventAppendError::WrongExpectedRevision { actual: 1, .. })
                ))
                .count(),
            1
        );
        let conn = store.conn.lock().unwrap();
        assert_eq!(
            conn.query_row("SELECT count(*) FROM events_v2", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            1
        );
    }

    #[test]
    fn compatible_writer_requires_exact_migration_and_strict_pragmas() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("events.db");
        let path = path.to_str().unwrap();
        let authority = EventStore::open(path).unwrap();
        let status = authority.event_contract_schema_status().unwrap();
        assert_eq!(status.schema_version, 2);
        assert_eq!(status.migration_name, EVENT_V2_MIGRATION_NAME);
        assert_eq!(status.migration_sha256.len(), 64);
        drop(authority);

        let compatible = EventStore::open_compatible(path).unwrap();
        let conn = compatible.conn.lock().unwrap();
        assert_eq!(
            conn.query_row("PRAGMA synchronous", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            conn.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn compatible_writer_rejects_without_changing_persistent_journal_mode() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal-mode.db");
        drop(EventStore::open(path.to_str().unwrap()).unwrap());
        let conn = rusqlite::Connection::open(&path).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0))
            .unwrap();
        assert!(mode.eq_ignore_ascii_case("delete"));
        drop(conn);

        assert!(EventStore::open_compatible(path.to_str().unwrap()).is_err());

        let conn = rusqlite::Connection::open(&path).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert!(mode.eq_ignore_ascii_case("delete"));
    }

    #[test]
    fn compatible_writer_rejects_missing_and_tampered_migration_authority() {
        let directory = tempfile::tempdir().unwrap();
        let missing_path = directory.path().join("missing.db");
        rusqlite::Connection::open(&missing_path).unwrap();
        assert!(EventStore::open_compatible(missing_path.to_str().unwrap()).is_err());

        let path = directory.path().join("tampered.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();
        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE event_schema_migrations SET sha256 = ?1 WHERE version = 2",
                ["0".repeat(64)],
            )
            .unwrap();
        drop(store);
        assert!(EventStore::open_compatible(path.to_str().unwrap()).is_err());
    }

    #[test]
    fn compatible_writer_rejects_a_matching_ledger_with_partial_schema() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("partial.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();
        store
            .conn
            .lock()
            .unwrap()
            .execute("ALTER TABLE events_v2 DROP COLUMN producer", [])
            .unwrap();
        drop(store);
        assert!(EventStore::open_compatible(path.to_str().unwrap()).is_err());
    }

    #[test]
    fn compatible_writer_rejects_tampered_schema_shape_with_matching_columns() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("shape-tampered.db");
        drop(EventStore::open(path.to_str().unwrap()).unwrap());
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute("DROP INDEX idx_events_v2_type_position", [])
            .unwrap();
        drop(conn);

        let error = match EventStore::open_compatible(path.to_str().unwrap()) {
            Ok(_) => panic!("compatible writer accepted a tampered schema shape"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("schema object fingerprint mismatch"));
    }

    #[test]
    fn compatible_writer_rejects_zero_event_truth_counters() {
        for column in ["event_truth_generation", "next_global_position"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join(format!("zero-{column}.db"));
            let store = EventStore::open(path.to_str().unwrap()).unwrap();
            store
                .conn
                .lock()
                .unwrap()
                .execute_batch(&format!(
                    "PRAGMA ignore_check_constraints = ON;
                     UPDATE event_truth_metadata SET {column} = 0 WHERE singleton_id = 1;
                     PRAGMA ignore_check_constraints = OFF;"
                ))
                .unwrap();
            drop(store);
            assert!(EventStore::open_compatible(path.to_str().unwrap()).is_err());
        }
    }

    #[test]
    fn v2_reader_rejects_tampered_payload_and_context_bindings() {
        let store = EventStore::open(":memory:").unwrap();
        let registry = registry();
        let proposal = proposal("operation-tamper", ExpectedStreamRevision::NoStream);
        let outcome = store
            .append_gateway(&registry)
            .append(&caller(&proposal), &proposal)
            .unwrap();
        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE events_v2 SET payload = ?1 WHERE event_id = ?2",
                params![b"{}".as_slice(), outcome.envelope.event_id],
            )
            .unwrap();
        assert!(store
            .event_v2_by_id(&outcome.envelope.event_id)
            .unwrap_err()
            .to_string()
            .contains("binding does not verify"));
    }
}
