use std::{collections::BTreeMap, path::Path};

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    digest::ContentDigest,
    error::DeliveryError,
    ports::{PublicationReceiptV1, PublicationRequestV1},
    state::DeliveryAggregateV1,
};

const META: TableDefinition<&str, &[u8]> = TableDefinition::new("delivery_meta");
const AGGREGATES: TableDefinition<&str, &[u8]> = TableDefinition::new("delivery_aggregates");
const JOURNAL: TableDefinition<&str, &[u8]> = TableDefinition::new("delivery_journal");
const IDEMPOTENCY: TableDefinition<&str, &[u8]> = TableDefinition::new("delivery_idempotency");
const OUTBOX: TableDefinition<&str, &[u8]> = TableDefinition::new("delivery_outbox");
const SCHEMA_KEY: &str = "schema_version";
const SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryJournalEntryV1 {
    pub schema_version: u16,
    pub tenant_id: String,
    pub project_id: String,
    pub project_revision: u64,
    pub operation_id: String,
    pub event_type: String,
    pub command_digest: ContentDigest,
    pub event_digest: ContentDigest,
    pub payload: Value,
    pub committed_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryCommitReceiptV1 {
    pub operation_id: String,
    pub project_revision: u64,
    pub event_digest: ContentDigest,
    pub duplicate: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryCommitRequestV1 {
    pub tenant_id: String,
    pub project_id: String,
    pub expected_revision: u64,
    pub principal_id: String,
    pub command_kind: String,
    pub idempotency_key: String,
    pub command_digest: ContentDigest,
    pub aggregate: DeliveryAggregateV1,
    pub event_type: String,
    pub event_payload: Value,
    pub committed_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdempotencyRecordV1 {
    command_digest: ContentDigest,
    receipt: DeliveryCommitReceiptV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryOutboxEntryV1 {
    pub request: PublicationRequestV1,
    pub project_revision: u64,
    pub published_receipt: Option<PublicationReceiptV1>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeliveryEventEnvelopeV1 {
    schema_version: u16,
    record_type: String,
    tenant_id: String,
    project_id: String,
    project_revision: u64,
    operation_id: String,
    event_type: String,
    command_digest: ContentDigest,
    payload: Value,
    committed_at_ms: u64,
}

/// Narrow persistence seam. Productive wiring is owned by #732 and must
/// implement this contract with the canonical trajectory authority.
pub trait DeliveryAggregateStorePort: Send + Sync {
    fn load(
        &self,
        tenant_id: &str,
        project_id: &str,
    ) -> Result<Option<DeliveryAggregateV1>, DeliveryError>;

    fn lookup_idempotency(
        &self,
        tenant_id: &str,
        principal_id: &str,
        command_kind: &str,
        idempotency_key: &str,
        command_digest: &ContentDigest,
    ) -> Result<Option<DeliveryCommitReceiptV1>, DeliveryError>;

    fn commit(
        &self,
        request: &DeliveryCommitRequestV1,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError>;
}

/// Publication-state seam owned by #733. Productive implementations read and
/// acknowledge only the outbox rows created by the canonical #732 append.
pub trait DeliveryPublicationStatePort: Send + Sync {
    fn pending_publications(&self) -> Result<Vec<DeliveryOutboxEntryV1>, DeliveryError>;

    fn mark_published(
        &self,
        expected_request_digest: &ContentDigest,
        receipt: PublicationReceiptV1,
    ) -> Result<(), DeliveryError>;
}

/// Deterministic redb adapter used by dependency-independent core tests only.
/// It is not the productive #732 trajectory or #733 publication authority.
pub struct DeliveryStore {
    db: Database,
}

impl DeliveryStore {
    #[doc(hidden)]
    pub fn open_test_only(path: &Path) -> Result<Self, DeliveryError> {
        let db = Database::create(path)?;
        let write = db.begin_write()?;
        {
            let _ = write.open_table(META)?;
            let _ = write.open_table(AGGREGATES)?;
            let _ = write.open_table(JOURNAL)?;
            let _ = write.open_table(IDEMPOTENCY)?;
            let _ = write.open_table(OUTBOX)?;
        }
        write.commit()?;
        let store = Self { db };
        store.initialize_schema()?;
        Ok(store)
    }

    fn initialize_schema(&self) -> Result<(), DeliveryError> {
        let existing = {
            let read = self.db.begin_read()?;
            let table = read.open_table(META)?;
            table.get(SCHEMA_KEY)?.map(|value| value.value().to_vec())
        };
        match existing {
            Some(bytes) => {
                let version: u16 = serde_json::from_slice(&bytes)
                    .map_err(|error| DeliveryError::CorruptStore(error.to_string()))?;
                if version != SCHEMA_VERSION {
                    return Err(DeliveryError::CorruptStore(format!(
                        "unsupported delivery schema {version}, expected {SCHEMA_VERSION}"
                    )));
                }
            }
            None => {
                let encoded = serde_json::to_vec(&SCHEMA_VERSION)?;
                let write = self.db.begin_write()?;
                {
                    let mut table = write.open_table(META)?;
                    table.insert(SCHEMA_KEY, encoded.as_slice())?;
                }
                write.commit()?;
            }
        }
        Ok(())
    }

    pub fn load(
        &self,
        tenant_id: &str,
        project_id: &str,
    ) -> Result<Option<DeliveryAggregateV1>, DeliveryError> {
        let key = aggregate_key(tenant_id, project_id)?;
        let read = self.db.begin_read()?;
        let table = read.open_table(AGGREGATES)?;
        let Some(value) = table.get(key.as_str())? else {
            return Ok(None);
        };
        decode(value.value(), "aggregate").map(Some)
    }

    pub fn lookup_idempotency(
        &self,
        tenant_id: &str,
        principal_id: &str,
        command_kind: &str,
        idempotency_key: &str,
        command_digest: &ContentDigest,
    ) -> Result<Option<DeliveryCommitReceiptV1>, DeliveryError> {
        for (name, value) in [
            ("tenant_id", tenant_id),
            ("principal_id", principal_id),
            ("command_kind", command_kind),
            ("idempotency_key", idempotency_key),
        ] {
            validate_component(name, value)?;
        }
        let key = format!("{tenant_id}:{principal_id}:{command_kind}:{idempotency_key}");
        let read = self.db.begin_read()?;
        let table = read.open_table(IDEMPOTENCY)?;
        let Some(existing) = table.get(key.as_str())? else {
            return Ok(None);
        };
        let record: IdempotencyRecordV1 = decode(existing.value(), "idempotency")?;
        if &record.command_digest != command_digest {
            return Err(DeliveryError::IdempotencyConflict { key });
        }
        let mut receipt = record.receipt;
        receipt.duplicate = true;
        Ok(Some(receipt))
    }

    pub fn commit(
        &self,
        request: &DeliveryCommitRequestV1,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        validate_component("tenant_id", &request.tenant_id)?;
        validate_component("project_id", &request.project_id)?;
        validate_component("principal_id", &request.principal_id)?;
        validate_component("command_kind", &request.command_kind)?;
        validate_component("idempotency_key", &request.idempotency_key)?;
        validate_component("event_type", &request.event_type)?;
        if request.aggregate.tenant_id != request.tenant_id
            || request.aggregate.project_id != request.project_id
        {
            return Err(DeliveryError::Validation(
                "aggregate authority does not match commit authority".to_string(),
            ));
        }

        let aggregate_key = aggregate_key(&request.tenant_id, &request.project_id)?;
        let idempotency_key = format!(
            "{}:{}:{}:{}",
            request.tenant_id, request.principal_id, request.command_kind, request.idempotency_key
        );
        let write = self.db.begin_write()?;

        {
            let table = write.open_table(IDEMPOTENCY)?;
            let existing = table.get(idempotency_key.as_str())?;
            if let Some(existing) = existing {
                let record: IdempotencyRecordV1 = decode(existing.value(), "idempotency")?;
                if record.command_digest != request.command_digest {
                    return Err(DeliveryError::IdempotencyConflict {
                        key: idempotency_key,
                    });
                }
                let mut receipt = record.receipt;
                receipt.duplicate = true;
                return Ok(receipt);
            }
        }

        let actual_revision = {
            let table = write.open_table(AGGREGATES)?;
            let existing = table.get(aggregate_key.as_str())?;
            match existing {
                Some(existing) => {
                    let aggregate: DeliveryAggregateV1 = decode(existing.value(), "aggregate")?;
                    aggregate.revision
                }
                None => 0,
            }
        };
        if actual_revision != request.expected_revision {
            return Err(DeliveryError::RevisionConflict {
                expected: request.expected_revision,
                actual: actual_revision,
            });
        }
        if request.aggregate.revision != actual_revision + 1 {
            return Err(DeliveryError::Validation(format!(
                "aggregate revision {} must be {}",
                request.aggregate.revision,
                actual_revision + 1
            )));
        }

        let operation_id = format!(
            "delivery:{}:{}:{:020}:{}",
            request.tenant_id, request.project_id, request.aggregate.revision, request.event_type
        );
        let envelope = DeliveryEventEnvelopeV1 {
            schema_version: SCHEMA_VERSION,
            record_type: "delivery-event-envelope".to_string(),
            tenant_id: request.tenant_id.clone(),
            project_id: request.project_id.clone(),
            project_revision: request.aggregate.revision,
            operation_id: operation_id.clone(),
            event_type: request.event_type.clone(),
            command_digest: request.command_digest.clone(),
            payload: request.event_payload.clone(),
            committed_at_ms: request.committed_at_ms,
        };
        let envelope_bytes = ContentDigest::canonical_bytes(&envelope)?;
        let event_digest = ContentDigest::of_bytes_domain(
            "delivery-event-envelope",
            SCHEMA_VERSION,
            &envelope_bytes,
        )?;
        let journal = DeliveryJournalEntryV1 {
            schema_version: SCHEMA_VERSION,
            tenant_id: request.tenant_id.clone(),
            project_id: request.project_id.clone(),
            project_revision: request.aggregate.revision,
            operation_id: operation_id.clone(),
            event_type: request.event_type.clone(),
            command_digest: request.command_digest.clone(),
            event_digest: event_digest.clone(),
            payload: request.event_payload.clone(),
            committed_at_ms: request.committed_at_ms,
        };
        let journal_bytes = ContentDigest::canonical_bytes(&journal)?;
        let aggregate_bytes = ContentDigest::canonical_bytes(&request.aggregate)?;
        let row_identity = format!("delivery-journal:{operation_id}");
        let publication = PublicationRequestV1 {
            schema_version: SCHEMA_VERSION,
            operation_id: operation_id.clone(),
            event_type: request.event_type.clone(),
            aggregate_id: aggregate_key.clone(),
            row_identity,
            payload_digest: event_digest.clone(),
            payload: envelope_bytes,
            request_digest: ContentDigest::zero(),
        }
        .seal()?;
        let outbox = DeliveryOutboxEntryV1 {
            request: publication,
            project_revision: request.aggregate.revision,
            published_receipt: None,
        };
        let outbox_bytes = serde_json::to_vec(&outbox)?;
        let receipt = DeliveryCommitReceiptV1 {
            operation_id: operation_id.clone(),
            project_revision: request.aggregate.revision,
            event_digest,
            duplicate: false,
        };
        let idempotency = IdempotencyRecordV1 {
            command_digest: request.command_digest.clone(),
            receipt: receipt.clone(),
        };
        let idempotency_bytes = serde_json::to_vec(&idempotency)?;

        {
            let mut table = write.open_table(AGGREGATES)?;
            table.insert(aggregate_key.as_str(), aggregate_bytes.as_slice())?;
        }
        {
            let mut table = write.open_table(JOURNAL)?;
            table.insert(operation_id.as_str(), journal_bytes.as_slice())?;
        }
        {
            let mut table = write.open_table(OUTBOX)?;
            table.insert(operation_id.as_str(), outbox_bytes.as_slice())?;
        }
        {
            let mut table = write.open_table(IDEMPOTENCY)?;
            table.insert(idempotency_key.as_str(), idempotency_bytes.as_slice())?;
        }
        write.commit()?;
        Ok(receipt)
    }

    pub fn journal(
        &self,
        tenant_id: &str,
        project_id: &str,
    ) -> Result<Vec<DeliveryJournalEntryV1>, DeliveryError> {
        let prefix = format!("delivery:{tenant_id}:{project_id}:");
        let read = self.db.begin_read()?;
        let table = read.open_table(JOURNAL)?;
        let mut entries = Vec::new();
        for row in table.iter()? {
            let (key, value) = row?;
            if key.value().starts_with(&prefix) {
                entries.push(decode(value.value(), "journal")?);
            }
        }
        entries.sort_by_key(|entry: &DeliveryJournalEntryV1| entry.project_revision);
        Ok(entries)
    }

    pub fn pending_publications(&self) -> Result<Vec<DeliveryOutboxEntryV1>, DeliveryError> {
        let read = self.db.begin_read()?;
        let table = read.open_table(OUTBOX)?;
        let mut entries = Vec::new();
        for row in table.iter()? {
            let (_, value) = row?;
            let entry: DeliveryOutboxEntryV1 = decode(value.value(), "outbox")?;
            if entry.published_receipt.is_none() {
                entries.push(entry);
            }
        }
        entries.sort_by_key(|entry| entry.project_revision);
        Ok(entries)
    }

    pub fn mark_published(
        &self,
        expected_request_digest: &ContentDigest,
        receipt: PublicationReceiptV1,
    ) -> Result<(), DeliveryError> {
        let write = self.db.begin_write()?;
        let updated = {
            let mut table = write.open_table(OUTBOX)?;
            let Some(existing) = table.get(receipt.operation_id.as_str())? else {
                return Err(DeliveryError::NotFound(format!(
                    "outbox {}",
                    receipt.operation_id
                )));
            };
            let mut entry: DeliveryOutboxEntryV1 = decode(existing.value(), "outbox")?;
            if receipt.schema_version != SCHEMA_VERSION
                || !validate_event_id(&receipt.event_id)
                || &entry.request.request_digest != expected_request_digest
                || receipt.request_digest != *expected_request_digest
                || receipt.operation_id != entry.request.operation_id
                || receipt.aggregate_id != entry.request.aggregate_id
                || receipt.row_identity != entry.request.row_identity
                || receipt.payload_digest != entry.request.payload_digest
            {
                return Err(DeliveryError::Conflict(format!(
                    "publication receipt digest mismatch for {}",
                    receipt.operation_id
                )));
            }
            if let Some(previous) = &entry.published_receipt {
                if previous != &receipt {
                    return Err(DeliveryError::Conflict(format!(
                        "publication receipt changed for {}",
                        receipt.operation_id
                    )));
                }
                false
            } else {
                entry.published_receipt = Some(receipt.clone());
                let bytes = serde_json::to_vec(&entry)?;
                drop(existing);
                table.insert(receipt.operation_id.as_str(), bytes.as_slice())?;
                true
            }
        };
        if updated {
            write.commit()?;
        }
        Ok(())
    }

    pub fn health(&self) -> Result<(), DeliveryError> {
        self.initialize_schema()?;
        let read = self.db.begin_read()?;
        let mut aggregate_revisions = BTreeMap::new();
        let aggregates = read.open_table(AGGREGATES)?;
        for row in aggregates.iter()? {
            let (key, value) = row?;
            let aggregate: DeliveryAggregateV1 = decode(value.value(), "aggregate")?;
            if aggregate.schema_version != SCHEMA_VERSION
                || key.value() != aggregate_key(&aggregate.tenant_id, &aggregate.project_id)?
            {
                return Err(DeliveryError::CorruptStore(
                    "aggregate key or schema mismatch".to_string(),
                ));
            }
            macro_rules! validate_identity_map {
                ($map:expr, $id:ident, $label:literal) => {
                    for (map_key, record) in &$map {
                        if map_key != &record.$id
                            || record.schema_version != SCHEMA_VERSION
                            || record.generation == 0
                        {
                            return Err(DeliveryError::CorruptStore(format!(
                                "{} key, schema, or generation mismatch",
                                $label
                            )));
                        }
                    }
                };
            }
            validate_identity_map!(aggregate.candidates, candidate_id, "candidate");
            validate_identity_map!(aggregate.qa_plans, plan_id, "QA plan");
            validate_identity_map!(aggregate.qa_runs, run_id, "QA run");
            validate_identity_map!(aggregate.reviews, review_id, "review");
            validate_identity_map!(aggregate.test_runs, test_run_id, "test run");
            validate_identity_map!(aggregate.findings, finding_id, "finding");
            validate_identity_map!(aggregate.approvals, approval_id, "approval");
            validate_identity_map!(aggregate.gates, gate_id, "gate");
            validate_identity_map!(aggregate.manifests, manifest_id, "manifest");
            validate_identity_map!(aggregate.releases, release_id, "release");
            validate_identity_map!(aggregate.deliveries, delivery_id, "delivery");
            validate_identity_map!(aggregate.feedback, feedback_id, "feedback");
            validate_identity_map!(aggregate.acceptances, acceptance_id, "acceptance");
            validate_identity_map!(aggregate.rollbacks, rollback_id, "rollback");
            validate_identity_map!(aggregate.closeouts, closeout_id, "closeout");
            for (map_key, receipt) in &aggregate.workbench_receipts {
                if map_key != &receipt.invocation.id
                    || receipt.schema_version != SCHEMA_VERSION
                    || receipt.invocation.generation == 0
                {
                    return Err(DeliveryError::CorruptStore(
                        "workbench receipt key, schema, or generation mismatch".to_string(),
                    ));
                }
            }
            for (map_key, graph) in &aggregate.evidence_graphs {
                if map_key != &graph.run.id
                    || graph.schema_version != SCHEMA_VERSION
                    || graph.run.generation == 0
                {
                    return Err(DeliveryError::CorruptStore(
                        "evidence graph key, schema, or generation mismatch".to_string(),
                    ));
                }
            }
            if aggregate
                .active_release_id
                .as_ref()
                .is_some_and(|release_id| !aggregate.releases.contains_key(release_id))
            {
                return Err(DeliveryError::CorruptStore(
                    "active release identity is missing".to_string(),
                ));
            }
            aggregate_revisions.insert(key.value().to_string(), aggregate.revision);
        }
        drop(aggregates);

        let mut journal_by_operation = BTreeMap::new();
        let mut revisions_by_aggregate: BTreeMap<String, Vec<u64>> = BTreeMap::new();
        let journal = read.open_table(JOURNAL)?;
        for row in journal.iter()? {
            let (key, value) = row?;
            let entry: DeliveryJournalEntryV1 = decode(value.value(), "journal")?;
            let aggregate_id = aggregate_key(&entry.tenant_id, &entry.project_id)?;
            if key.value() != entry.operation_id
                || entry.schema_version != SCHEMA_VERSION
                || entry.operation_id
                    != format!(
                        "delivery:{}:{}:{:020}:{}",
                        entry.tenant_id, entry.project_id, entry.project_revision, entry.event_type
                    )
            {
                return Err(DeliveryError::CorruptStore(
                    "journal key, schema, or operation identity is invalid".to_string(),
                ));
            }
            let envelope = envelope_from_journal(&entry);
            let bytes = ContentDigest::canonical_bytes(&envelope)?;
            let digest =
                ContentDigest::of_bytes_domain("delivery-event-envelope", SCHEMA_VERSION, &bytes)?;
            if digest != entry.event_digest {
                return Err(DeliveryError::CorruptStore(
                    "journal envelope digest mismatch".to_string(),
                ));
            }
            revisions_by_aggregate
                .entry(aggregate_id)
                .or_default()
                .push(entry.project_revision);
            if journal_by_operation
                .insert(entry.operation_id.clone(), entry)
                .is_some()
            {
                return Err(DeliveryError::CorruptStore(
                    "duplicate journal operation identity".to_string(),
                ));
            }
        }
        drop(journal);
        for (aggregate_id, revision) in &aggregate_revisions {
            let mut revisions = revisions_by_aggregate
                .remove(aggregate_id)
                .unwrap_or_default();
            revisions.sort_unstable();
            if revisions.len() as u64 != *revision
                || revisions
                    .iter()
                    .enumerate()
                    .any(|(index, value)| *value != index as u64 + 1)
            {
                return Err(DeliveryError::CorruptStore(
                    "journal continuity does not match aggregate revision".to_string(),
                ));
            }
        }
        if !revisions_by_aggregate.is_empty() {
            return Err(DeliveryError::CorruptStore(
                "journal exists without an aggregate".to_string(),
            ));
        }
        let outbox = read.open_table(OUTBOX)?;
        for row in outbox.iter()? {
            let (key, value) = row?;
            let entry: DeliveryOutboxEntryV1 = decode(value.value(), "outbox")?;
            let journal = journal_by_operation
                .get(&entry.request.operation_id)
                .ok_or_else(|| {
                    DeliveryError::CorruptStore(
                        "outbox exists without matching journal row".to_string(),
                    )
                })?;
            if key.value() != entry.request.operation_id
                || entry.request.request_digest != entry.request.computed_digest()?
                || entry.request.schema_version != SCHEMA_VERSION
                || entry.request.aggregate_id
                    != aggregate_key(&journal.tenant_id, &journal.project_id)?
                || entry.request.row_identity
                    != format!("delivery-journal:{}", journal.operation_id)
                || entry.request.event_type != journal.event_type
                || entry.project_revision != journal.project_revision
                || entry.request.payload_digest != journal.event_digest
                || entry.request.payload
                    != ContentDigest::canonical_bytes(&envelope_from_journal(journal))?
                || entry.request.payload_digest
                    != ContentDigest::of_bytes_domain(
                        "delivery-event-envelope",
                        SCHEMA_VERSION,
                        &entry.request.payload,
                    )?
            {
                return Err(DeliveryError::CorruptStore(
                    "outbox request binding is invalid".to_string(),
                ));
            }
            if let Some(receipt) = &entry.published_receipt {
                if receipt.schema_version != SCHEMA_VERSION
                    || !validate_event_id(&receipt.event_id)
                    || receipt.operation_id != entry.request.operation_id
                    || receipt.aggregate_id != entry.request.aggregate_id
                    || receipt.row_identity != entry.request.row_identity
                    || receipt.payload_digest != entry.request.payload_digest
                    || receipt.request_digest != entry.request.request_digest
                {
                    return Err(DeliveryError::CorruptStore(
                        "published receipt binding is invalid".to_string(),
                    ));
                }
            }
        }
        let idempotency = read.open_table(IDEMPOTENCY)?;
        for row in idempotency.iter()? {
            let (_, value) = row?;
            let record: IdempotencyRecordV1 = decode(value.value(), "idempotency")?;
            let journal = journal_by_operation
                .get(&record.receipt.operation_id)
                .ok_or_else(|| {
                    DeliveryError::CorruptStore(
                        "idempotency receipt exists without journal row".to_string(),
                    )
                })?;
            if record.command_digest == ContentDigest::zero()
                || record.receipt.event_digest == ContentDigest::zero()
                || record.receipt.duplicate
                || record.command_digest != journal.command_digest
                || record.receipt.event_digest != journal.event_digest
                || record.receipt.project_revision != journal.project_revision
            {
                return Err(DeliveryError::CorruptStore(
                    "idempotency record has an empty digest".to_string(),
                ));
            }
        }
        Ok(())
    }
}

impl DeliveryAggregateStorePort for DeliveryStore {
    fn load(
        &self,
        tenant_id: &str,
        project_id: &str,
    ) -> Result<Option<DeliveryAggregateV1>, DeliveryError> {
        DeliveryStore::load(self, tenant_id, project_id)
    }

    fn lookup_idempotency(
        &self,
        tenant_id: &str,
        principal_id: &str,
        command_kind: &str,
        idempotency_key: &str,
        command_digest: &ContentDigest,
    ) -> Result<Option<DeliveryCommitReceiptV1>, DeliveryError> {
        DeliveryStore::lookup_idempotency(
            self,
            tenant_id,
            principal_id,
            command_kind,
            idempotency_key,
            command_digest,
        )
    }

    fn commit(
        &self,
        request: &DeliveryCommitRequestV1,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        DeliveryStore::commit(self, request)
    }
}

impl DeliveryPublicationStatePort for DeliveryStore {
    fn pending_publications(&self) -> Result<Vec<DeliveryOutboxEntryV1>, DeliveryError> {
        DeliveryStore::pending_publications(self)
    }

    fn mark_published(
        &self,
        expected_request_digest: &ContentDigest,
        receipt: PublicationReceiptV1,
    ) -> Result<(), DeliveryError> {
        DeliveryStore::mark_published(self, expected_request_digest, receipt)
    }
}

fn envelope_from_journal(entry: &DeliveryJournalEntryV1) -> DeliveryEventEnvelopeV1 {
    DeliveryEventEnvelopeV1 {
        schema_version: entry.schema_version,
        record_type: "delivery-event-envelope".to_string(),
        tenant_id: entry.tenant_id.clone(),
        project_id: entry.project_id.clone(),
        project_revision: entry.project_revision,
        operation_id: entry.operation_id.clone(),
        event_type: entry.event_type.clone(),
        command_digest: entry.command_digest.clone(),
        payload: entry.payload.clone(),
        committed_at_ms: entry.committed_at_ms,
    }
}

fn aggregate_key(tenant_id: &str, project_id: &str) -> Result<String, DeliveryError> {
    validate_component("tenant_id", tenant_id)?;
    validate_component("project_id", project_id)?;
    Ok(format!("{tenant_id}:{project_id}"))
}

fn validate_component(name: &str, value: &str) -> Result<(), DeliveryError> {
    if value.is_empty()
        || value.len() > 160
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(DeliveryError::Validation(format!(
            "{name} is not a canonical identifier"
        )));
    }
    Ok(())
}

fn validate_event_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 240
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn decode<T: for<'de> Deserialize<'de>>(
    bytes: &[u8],
    record_type: &str,
) -> Result<T, DeliveryError> {
    serde_json::from_slice(bytes)
        .map_err(|error| DeliveryError::CorruptStore(format!("{record_type}: {error}")))
}
