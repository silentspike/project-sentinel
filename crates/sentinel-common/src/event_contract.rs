//! Canonical event-truth contracts shared by producers and the event store.
//!
//! Callers construct [`AppendProposalV2`]. Only the event store may construct
//! [`EventEnvelopeV2`] and its receipt fields.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::OwnerTerm;

pub const EVENT_PROPOSAL_VERSION_V2: u16 = 2;
pub const CAUSAL_CONTEXT_VERSION_V1: u16 = 1;
pub const MAX_AUTHORITY_ID_BYTES: usize = 128;
pub const MAX_WIRE_ID_BYTES: usize = 192;
pub const MAX_EVENT_TYPE_BYTES: usize = 128;
pub const MAX_PRODUCER_ID_BYTES: usize = 128;
pub const MAX_CAUSAL_CONTEXT_BYTES: usize = 8 * 1024;
pub const MAX_EVENT_PAYLOAD_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityKindV1 {
    Tenant,
    Company,
    Project,
    Workflow,
    WorkItem,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityRefV1 {
    pub kind: AuthorityKindV1,
    pub id: String,
    pub authority_generation: u64,
    pub authority_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CausalContextV1 {
    pub schema_version: u16,
    pub tenant: AuthorityRefV1,
    pub company: AuthorityRefV1,
    pub project: AuthorityRefV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<AuthorityRefV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item: Option<AuthorityRefV1>,
    pub request_id: String,
    pub request_digest: String,
    pub correlation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_event_id: Option<String>,
    pub operation_id: String,
    pub attempt: u32,
    pub source_generation: u64,
    pub source_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qa_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic_trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic_span_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventDurability {
    Authoritative,
    DurableOperational,
    RebuildableTelemetry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventPayloadCodec {
    Json,
    DeterministicCbor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CausationPolicyV1 {
    RootRequired,
    DirectRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "revision")]
pub enum ExpectedStreamRevision {
    NoStream,
    Exact(u64),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryIntentV1 {
    pub intent_id: String,
    pub topic: String,
    pub payload_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalEffectReservationV1 {
    pub effect_id: String,
    pub effect_kind: String,
    pub request_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppendProposalV2 {
    pub proposal_version: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_event_id: Option<String>,
    pub event_type: String,
    pub schema_version: u32,
    pub payload_codec: EventPayloadCodec,
    pub payload_digest: String,
    pub payload: Vec<u8>,
    pub causal_context: CausalContextV1,
    pub producer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_term: Option<OwnerTerm>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick: Option<u64>,
    pub requested_durability: EventDurability,
    pub expected_stream_revision: ExpectedStreamRevision,
    #[serde(default)]
    pub delivery_intents: Vec<DeliveryIntentV1>,
    #[serde(default)]
    pub effect_reservations: Vec<LocalEffectReservationV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventEnvelopeV2 {
    pub event_id: String,
    pub event_truth_generation: u64,
    pub stream_namespace: String,
    pub stream_revision: u64,
    pub global_position: u64,
    pub event_type: String,
    pub schema_version: u32,
    pub payload_codec: EventPayloadCodec,
    pub payload_digest: String,
    pub payload: Vec<u8>,
    pub causal_context: CausalContextV1,
    pub producer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_term: Option<OwnerTerm>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick: Option<u64>,
    pub appended_at_ms: i64,
    pub durability: EventDurability,
    pub canonical_request_digest: String,
    pub append_receipt_digest: String,
    pub sealed_envelope_digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppendDispositionV2 {
    Appended,
    ReplayOfPriorOperation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppendOutcomeV2 {
    pub disposition: AppendDispositionV2,
    pub envelope: EventEnvelopeV2,
    pub outcome_digest: String,
}

/// Authority state exposed by the compatibility reader.
///
/// A V1 row never acquires authority merely because a newer binary reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecodedEventAuthorityState {
    CanonicalV2,
    UnknownV1NonAuthorizing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyEventEnvelopeV1 {
    pub event_id: String,
    pub event_type: String,
    pub aggregate_id: String,
    pub payload: String,
    pub correlation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<String>,
    pub operation_id: String,
    pub tick: u64,
    pub timestamp_ms: u64,
    pub schema_version: u32,
    pub compensation_type: String,
    pub authority_state: DecodedEventAuthorityState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "envelope_version", content = "event")]
pub enum CompatibleEventEnvelope {
    V1(Box<LegacyEventEnvelopeV1>),
    V2(Box<EventEnvelopeV2>),
}

impl CompatibleEventEnvelope {
    pub fn authority_state(&self) -> DecodedEventAuthorityState {
        match self {
            Self::V1(_) => DecodedEventAuthorityState::UnknownV1NonAuthorizing,
            Self::V2(_) => DecodedEventAuthorityState::CanonicalV2,
        }
    }
}

pub type EventPayloadValidator = fn(&[u8]) -> Result<(), EventContractError>;
pub type EventPayloadUpcaster = fn(&[u8]) -> Result<Vec<u8>, EventContractError>;

#[derive(Debug, Clone)]
pub struct EventSchemaUpcast {
    pub target_version: u32,
    pub upcaster_id: String,
    pub upcast: EventPayloadUpcaster,
}

#[derive(Debug, Clone)]
pub struct EventSchemaDefinition {
    pub event_type: String,
    pub schema_version: u32,
    pub durability: EventDurability,
    pub payload_codec: EventPayloadCodec,
    pub causation_policy: CausationPolicyV1,
    pub allowed_producers: BTreeSet<String>,
    pub deterministic_event_id_producers: BTreeSet<String>,
    pub validator_id: String,
    pub validate_payload: EventPayloadValidator,
    pub upcast: Option<EventSchemaUpcast>,
}

#[derive(Debug, Clone, Default)]
pub struct EventSchemaRegistry {
    definitions: BTreeMap<(String, u32), EventSchemaDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpcastedEventPayload {
    pub event_type: String,
    pub source_version: u32,
    pub target_version: u32,
    pub payload: Vec<u8>,
    pub payload_digest: String,
    pub applied_upcasters: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct EventSchemaManifestEntry<'a> {
    event_type: &'a str,
    schema_version: u32,
    durability: EventDurability,
    payload_codec: EventPayloadCodec,
    causation_policy: CausationPolicyV1,
    allowed_producers: &'a BTreeSet<String>,
    deterministic_event_id_producers: &'a BTreeSet<String>,
    validator_id: &'a str,
    upcast_target_version: Option<u32>,
    upcaster_id: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EventContractError {
    #[error("unsupported event proposal version {0}")]
    UnsupportedProposalVersion(u16),
    #[error("unsupported causal-context version {0}")]
    UnsupportedCausalContextVersion(u16),
    #[error("invalid {field}: {reason}")]
    InvalidField {
        field: &'static str,
        reason: &'static str,
    },
    #[error("invalid authority hierarchy at {0}")]
    InvalidAuthorityHierarchy(&'static str),
    #[error("payload digest does not match payload bytes")]
    PayloadDigestMismatch,
    #[error("causal context exceeds its canonical size bound")]
    CausalContextTooLarge,
    #[error("unknown event schema {event_type}@{schema_version}")]
    UnknownSchema {
        event_type: String,
        schema_version: u32,
    },
    #[error("unsupported known event schema version {event_type}@{schema_version}")]
    UnsupportedKnownSchemaVersion {
        event_type: String,
        schema_version: u32,
    },
    #[error("event payload is invalid for schema {event_type}@{schema_version}: {reason}")]
    InvalidSchemaPayload {
        event_type: String,
        schema_version: u32,
        reason: String,
    },
    #[error("invalid event schema upcast chain for {event_type}@{schema_version}")]
    InvalidUpcastChain {
        event_type: String,
        schema_version: u32,
    },
    #[error("producer {producer} is not authorized for {event_type}@{schema_version}")]
    UnauthorizedProducer {
        producer: String,
        event_type: String,
        schema_version: u32,
    },
    #[error("event durability does not match the schema registry")]
    DurabilityMismatch,
    #[error("payload codec does not match the schema registry")]
    CodecMismatch,
    #[error("requested event identity is not authorized for this producer")]
    RequestedEventIdNotAuthorized,
    #[error("duplicate identifier in {0}")]
    DuplicateIdentifier(&'static str),
    #[error("canonical serialization failed: {0}")]
    CanonicalEncoding(String),
}

impl AuthorityRefV1 {
    fn validate(&self, expected: AuthorityKindV1) -> Result<(), EventContractError> {
        if self.kind != expected {
            return Err(EventContractError::InvalidAuthorityHierarchy(
                authority_field(expected),
            ));
        }
        validate_wire_id("authority.id", &self.id, MAX_AUTHORITY_ID_BYTES)?;
        if self.authority_generation == 0 {
            return Err(EventContractError::InvalidField {
                field: "authority_generation",
                reason: "must be greater than zero",
            });
        }
        validate_digest("authority_digest", &self.authority_digest)
    }
}

impl CausalContextV1 {
    pub fn validate(&self) -> Result<(), EventContractError> {
        if self.schema_version != CAUSAL_CONTEXT_VERSION_V1 {
            return Err(EventContractError::UnsupportedCausalContextVersion(
                self.schema_version,
            ));
        }
        self.tenant.validate(AuthorityKindV1::Tenant)?;
        self.company.validate(AuthorityKindV1::Company)?;
        self.project.validate(AuthorityKindV1::Project)?;
        if let Some(workflow) = &self.workflow {
            workflow.validate(AuthorityKindV1::Workflow)?;
        }
        if let Some(work_item) = &self.work_item {
            if self.workflow.is_none() {
                return Err(EventContractError::InvalidAuthorityHierarchy(
                    "work_item_without_workflow",
                ));
            }
            work_item.validate(AuthorityKindV1::WorkItem)?;
        }
        validate_wire_id("request_id", &self.request_id, MAX_WIRE_ID_BYTES)?;
        validate_digest("request_digest", &self.request_digest)?;
        validate_wire_id("correlation_id", &self.correlation_id, MAX_WIRE_ID_BYTES)?;
        validate_wire_id("operation_id", &self.operation_id, MAX_WIRE_ID_BYTES)?;
        if let Some(causation) = &self.causation_event_id {
            validate_uuid("causation_event_id", causation, None)?;
        }
        if self.attempt == 0 {
            return Err(EventContractError::InvalidField {
                field: "attempt",
                reason: "must be greater than zero",
            });
        }
        if self.source_generation == 0 {
            return Err(EventContractError::InvalidField {
                field: "source_generation",
                reason: "must be greater than zero",
            });
        }
        validate_digest("source_digest", &self.source_digest)?;
        validate_optional_id("invocation_id", self.invocation_id.as_deref())?;
        validate_optional_id("agent_id", self.agent_id.as_deref())?;
        validate_optional_id("artifact_id", self.artifact_id.as_deref())?;
        validate_optional_id("qa_run_id", self.qa_run_id.as_deref())?;
        validate_optional_id("release_id", self.release_id.as_deref())?;
        validate_optional_id("delivery_id", self.delivery_id.as_deref())?;
        validate_optional_id("diagnostic_trace_id", self.diagnostic_trace_id.as_deref())?;
        validate_optional_id("diagnostic_span_id", self.diagnostic_span_id.as_deref())?;
        match (&self.artifact_id, &self.artifact_digest) {
            (Some(_), Some(digest)) => validate_digest("artifact_digest", digest)?,
            (None, None) => {}
            _ => {
                return Err(EventContractError::InvalidField {
                    field: "artifact",
                    reason: "identity and digest must be present together",
                })
            }
        }
        let encoded = canonical_json(self)?;
        if encoded.len() > MAX_CAUSAL_CONTEXT_BYTES {
            return Err(EventContractError::CausalContextTooLarge);
        }
        Ok(())
    }

    pub fn authority_scope_digest(&self) -> Result<String, EventContractError> {
        self.validate()?;
        #[derive(Serialize)]
        struct Scope<'a> {
            tenant: &'a AuthorityRefV1,
            company: &'a AuthorityRefV1,
            project: &'a AuthorityRefV1,
            workflow: &'a Option<AuthorityRefV1>,
            work_item: &'a Option<AuthorityRefV1>,
        }
        sha256_json(&Scope {
            tenant: &self.tenant,
            company: &self.company,
            project: &self.project,
            workflow: &self.workflow,
            work_item: &self.work_item,
        })
    }
}

impl AppendProposalV2 {
    pub fn validate(&self) -> Result<(), EventContractError> {
        if self.proposal_version != EVENT_PROPOSAL_VERSION_V2 {
            return Err(EventContractError::UnsupportedProposalVersion(
                self.proposal_version,
            ));
        }
        validate_wire_id("event_type", &self.event_type, MAX_EVENT_TYPE_BYTES)?;
        validate_wire_id("producer", &self.producer, MAX_PRODUCER_ID_BYTES)?;
        if self.schema_version == 0 {
            return Err(EventContractError::InvalidField {
                field: "schema_version",
                reason: "must be greater than zero",
            });
        }
        if self.payload.is_empty() || self.payload.len() > MAX_EVENT_PAYLOAD_BYTES {
            return Err(EventContractError::InvalidField {
                field: "payload",
                reason: "must be non-empty and at most 1 MiB",
            });
        }
        validate_digest("payload_digest", &self.payload_digest)?;
        if sha256_hex(&self.payload) != self.payload_digest {
            return Err(EventContractError::PayloadDigestMismatch);
        }
        self.causal_context.validate()?;
        if self.tick != self.causal_context.tick {
            return Err(EventContractError::InvalidField {
                field: "tick",
                reason: "must match causal_context.tick",
            });
        }
        if let Some(event_id) = &self.requested_event_id {
            validate_uuid("requested_event_id", event_id, Some(7))?;
        }
        let mut delivery_ids = BTreeSet::new();
        for intent in &self.delivery_intents {
            validate_wire_id("delivery_intent.id", &intent.intent_id, MAX_WIRE_ID_BYTES)?;
            validate_wire_id("delivery_intent.topic", &intent.topic, MAX_WIRE_ID_BYTES)?;
            validate_digest("delivery_intent.payload_digest", &intent.payload_digest)?;
            if !delivery_ids.insert(intent.intent_id.as_str()) {
                return Err(EventContractError::DuplicateIdentifier("delivery_intents"));
            }
        }
        let mut effect_ids = BTreeSet::new();
        for effect in &self.effect_reservations {
            validate_wire_id("effect.id", &effect.effect_id, MAX_WIRE_ID_BYTES)?;
            validate_wire_id("effect.kind", &effect.effect_kind, MAX_WIRE_ID_BYTES)?;
            validate_digest("effect.request_digest", &effect.request_digest)?;
            if !effect_ids.insert(effect.effect_id.as_str()) {
                return Err(EventContractError::DuplicateIdentifier(
                    "effect_reservations",
                ));
            }
        }
        Ok(())
    }

    pub fn canonical_request_digest(&self) -> Result<String, EventContractError> {
        self.validate()?;
        sha256_json(self)
    }
}

impl EventEnvelopeV2 {
    pub fn expected_append_receipt_digest(&self) -> Result<String, EventContractError> {
        #[derive(Serialize)]
        struct Receipt<'a> {
            event_id: &'a str,
            event_truth_generation: u64,
            stream_namespace: &'a str,
            stream_revision: u64,
            global_position: u64,
            canonical_request_digest: &'a str,
            appended_at_ms: i64,
            durability: EventDurability,
        }

        sha256_json(&Receipt {
            event_id: &self.event_id,
            event_truth_generation: self.event_truth_generation,
            stream_namespace: &self.stream_namespace,
            stream_revision: self.stream_revision,
            global_position: self.global_position,
            canonical_request_digest: &self.canonical_request_digest,
            appended_at_ms: self.appended_at_ms,
            durability: self.durability,
        })
    }

    pub fn expected_sealed_envelope_digest(&self) -> Result<String, EventContractError> {
        let mut material = self.clone();
        material.sealed_envelope_digest.clear();
        sha256_json(&material)
    }

    pub fn canonical_envelope_digest(&self) -> Result<String, EventContractError> {
        sha256_json(self)
    }

    pub fn validate_seals(&self) -> Result<(), EventContractError> {
        validate_uuid("event_id", &self.event_id, Some(7))?;
        if self.event_truth_generation == 0
            || self.stream_revision == 0
            || self.global_position == 0
            || self.schema_version == 0
            || self.appended_at_ms < 0
        {
            return Err(EventContractError::InvalidField {
                field: "event_envelope",
                reason: "contains an invalid store-owned field",
            });
        }
        validate_wire_id("event_type", &self.event_type, MAX_EVENT_TYPE_BYTES)?;
        validate_wire_id("producer", &self.producer, MAX_PRODUCER_ID_BYTES)?;
        if self.payload.is_empty() || self.payload.len() > MAX_EVENT_PAYLOAD_BYTES {
            return Err(EventContractError::InvalidField {
                field: "payload",
                reason: "must be non-empty and at most 1 MiB",
            });
        }
        validate_digest("payload_digest", &self.payload_digest)?;
        validate_digest("canonical_request_digest", &self.canonical_request_digest)?;
        validate_digest("append_receipt_digest", &self.append_receipt_digest)?;
        validate_digest("sealed_envelope_digest", &self.sealed_envelope_digest)?;
        self.causal_context.validate()?;
        if self.tick != self.causal_context.tick
            || sha256_hex(&self.payload) != self.payload_digest
            || self.stream_namespace != self.causal_context.authority_scope_digest()?
        {
            return Err(EventContractError::InvalidField {
                field: "event_envelope",
                reason: "payload, tick, or authority binding does not verify",
            });
        }
        if self.expected_append_receipt_digest()? != self.append_receipt_digest
            || self.expected_sealed_envelope_digest()? != self.sealed_envelope_digest
        {
            return Err(EventContractError::InvalidField {
                field: "event_envelope",
                reason: "receipt or sealed-envelope digest does not verify",
            });
        }
        Ok(())
    }
}

impl EventSchemaRegistry {
    pub fn new(
        definitions: impl IntoIterator<Item = EventSchemaDefinition>,
    ) -> Result<Self, EventContractError> {
        let mut registry = Self::default();
        for definition in definitions {
            validate_wire_id(
                "event_schema.event_type",
                &definition.event_type,
                MAX_EVENT_TYPE_BYTES,
            )?;
            if definition.schema_version == 0 || definition.allowed_producers.is_empty() {
                return Err(EventContractError::InvalidField {
                    field: "event_schema",
                    reason: "requires a version and at least one producer",
                });
            }
            validate_wire_id(
                "event_schema.validator_id",
                &definition.validator_id,
                MAX_WIRE_ID_BYTES,
            )?;
            if let Some(upcast) = &definition.upcast {
                validate_wire_id(
                    "event_schema.upcaster_id",
                    &upcast.upcaster_id,
                    MAX_WIRE_ID_BYTES,
                )?;
                if upcast.target_version <= definition.schema_version {
                    return Err(EventContractError::InvalidUpcastChain {
                        event_type: definition.event_type.clone(),
                        schema_version: definition.schema_version,
                    });
                }
            }
            for producer in definition
                .allowed_producers
                .iter()
                .chain(definition.deterministic_event_id_producers.iter())
            {
                validate_wire_id("event_schema.producer", producer, MAX_PRODUCER_ID_BYTES)?;
            }
            if !definition
                .deterministic_event_id_producers
                .is_subset(&definition.allowed_producers)
            {
                return Err(EventContractError::InvalidField {
                    field: "deterministic_event_id_producers",
                    reason: "must be a subset of allowed producers",
                });
            }
            let key = (definition.event_type.clone(), definition.schema_version);
            if registry.definitions.insert(key, definition).is_some() {
                return Err(EventContractError::DuplicateIdentifier("event_schema"));
            }
        }
        for definition in registry.definitions.values() {
            if let Some(upcast) = &definition.upcast {
                if !registry
                    .definitions
                    .contains_key(&(definition.event_type.clone(), upcast.target_version))
                {
                    return Err(EventContractError::InvalidUpcastChain {
                        event_type: definition.event_type.clone(),
                        schema_version: definition.schema_version,
                    });
                }
            }
        }
        Ok(registry)
    }

    pub fn validate_proposal(&self, proposal: &AppendProposalV2) -> Result<(), EventContractError> {
        proposal.validate()?;
        let definition = self
            .definitions
            .get(&(proposal.event_type.clone(), proposal.schema_version))
            .ok_or_else(|| EventContractError::UnknownSchema {
                event_type: proposal.event_type.clone(),
                schema_version: proposal.schema_version,
            })?;
        if !definition.allowed_producers.contains(&proposal.producer) {
            return Err(EventContractError::UnauthorizedProducer {
                producer: proposal.producer.clone(),
                event_type: proposal.event_type.clone(),
                schema_version: proposal.schema_version,
            });
        }
        if definition.durability != proposal.requested_durability {
            return Err(EventContractError::DurabilityMismatch);
        }
        if definition.payload_codec != proposal.payload_codec {
            return Err(EventContractError::CodecMismatch);
        }
        let has_direct_causation = proposal.causal_context.causation_event_id.is_some();
        match definition.causation_policy {
            CausationPolicyV1::RootRequired if has_direct_causation => {
                return Err(EventContractError::InvalidField {
                    field: "causation_event_id",
                    reason: "root events must not declare a direct causation event",
                });
            }
            CausationPolicyV1::DirectRequired if !has_direct_causation => {
                return Err(EventContractError::InvalidField {
                    field: "causation_event_id",
                    reason: "non-root events require their direct causation event",
                });
            }
            _ => {}
        }
        if proposal.requested_event_id.is_some()
            && !definition
                .deterministic_event_id_producers
                .contains(&proposal.producer)
        {
            return Err(EventContractError::RequestedEventIdNotAuthorized);
        }
        (definition.validate_payload)(&proposal.payload).map_err(|error| {
            EventContractError::InvalidSchemaPayload {
                event_type: proposal.event_type.clone(),
                schema_version: proposal.schema_version,
                reason: error.to_string(),
            }
        })?;
        Ok(())
    }

    pub fn causation_policy_for(
        &self,
        proposal: &AppendProposalV2,
    ) -> Result<CausationPolicyV1, EventContractError> {
        self.definitions
            .get(&(proposal.event_type.clone(), proposal.schema_version))
            .map(|definition| definition.causation_policy)
            .ok_or_else(|| EventContractError::UnknownSchema {
                event_type: proposal.event_type.clone(),
                schema_version: proposal.schema_version,
            })
    }

    pub fn manifest_digest(&self) -> Result<String, EventContractError> {
        let entries: Vec<_> = self
            .definitions
            .values()
            .map(|definition| EventSchemaManifestEntry {
                event_type: &definition.event_type,
                schema_version: definition.schema_version,
                durability: definition.durability,
                payload_codec: definition.payload_codec,
                causation_policy: definition.causation_policy,
                allowed_producers: &definition.allowed_producers,
                deterministic_event_id_producers: &definition.deterministic_event_id_producers,
                validator_id: &definition.validator_id,
                upcast_target_version: definition
                    .upcast
                    .as_ref()
                    .map(|upcast| upcast.target_version),
                upcaster_id: definition
                    .upcast
                    .as_ref()
                    .map(|upcast| upcast.upcaster_id.as_str()),
            })
            .collect();
        sha256_json(&entries)
    }

    pub fn upcast_to_latest(
        &self,
        event_type: &str,
        source_version: u32,
        payload: &[u8],
    ) -> Result<UpcastedEventPayload, EventContractError> {
        let mut version = source_version;
        let mut current = payload.to_vec();
        let mut applied_upcasters = Vec::new();
        let event_type_known = self
            .definitions
            .keys()
            .any(|(registered_type, _)| registered_type == event_type);
        if !event_type_known {
            return Err(EventContractError::UnknownSchema {
                event_type: event_type.to_string(),
                schema_version: source_version,
            });
        }

        loop {
            let definition = self
                .definitions
                .get(&(event_type.to_string(), version))
                .ok_or_else(|| EventContractError::UnsupportedKnownSchemaVersion {
                    event_type: event_type.to_string(),
                    schema_version: version,
                })?;
            (definition.validate_payload)(&current).map_err(|error| {
                EventContractError::InvalidSchemaPayload {
                    event_type: event_type.to_string(),
                    schema_version: version,
                    reason: error.to_string(),
                }
            })?;
            let Some(upcast) = &definition.upcast else {
                break;
            };
            current = (upcast.upcast)(&current).map_err(|error| {
                EventContractError::InvalidSchemaPayload {
                    event_type: event_type.to_string(),
                    schema_version: version,
                    reason: error.to_string(),
                }
            })?;
            version = upcast.target_version;
            applied_upcasters.push(upcast.upcaster_id.clone());
            if applied_upcasters.len() > self.definitions.len() {
                return Err(EventContractError::InvalidUpcastChain {
                    event_type: event_type.to_string(),
                    schema_version: source_version,
                });
            }
        }

        Ok(UpcastedEventPayload {
            event_type: event_type.to_string(),
            source_version,
            target_version: version,
            payload_digest: sha256_hex(&current),
            payload: current,
            applied_upcasters,
        })
    }
}

pub fn validate_json_object_payload(payload: &[u8]) -> Result<(), EventContractError> {
    let value: serde_json::Value = serde_json::from_slice(payload)
        .map_err(|error| EventContractError::CanonicalEncoding(error.to_string()))?;
    if !value.is_object() || canonical_json(&value)? != payload {
        return Err(EventContractError::InvalidField {
            field: "payload",
            reason: "must be one canonical JSON object",
        });
    }
    Ok(())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn sha256_json<T: Serialize>(value: &T) -> Result<String, EventContractError> {
    Ok(sha256_hex(&canonical_json(value)?))
}

pub fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>, EventContractError> {
    serde_json::to_vec(value)
        .map_err(|error| EventContractError::CanonicalEncoding(error.to_string()))
}

pub fn validate_sha256(field: &'static str, value: &str) -> Result<(), EventContractError> {
    validate_digest(field, value)
}

fn validate_optional_id(
    field: &'static str,
    value: Option<&str>,
) -> Result<(), EventContractError> {
    if let Some(value) = value {
        validate_wire_id(field, value, MAX_WIRE_ID_BYTES)?;
    }
    Ok(())
}

fn validate_wire_id(
    field: &'static str,
    value: &str,
    max_bytes: usize,
) -> Result<(), EventContractError> {
    if value.is_empty() || value.len() > max_bytes {
        return Err(EventContractError::InvalidField {
            field,
            reason: "is empty or exceeds its size bound",
        });
    }
    if !value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
    }) {
        return Err(EventContractError::InvalidField {
            field,
            reason: "contains a non-canonical character",
        });
    }
    Ok(())
}

fn validate_digest(field: &'static str, value: &str) -> Result<(), EventContractError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(EventContractError::InvalidField {
            field,
            reason: "must be a lowercase SHA-256 hex digest",
        });
    }
    Ok(())
}

fn validate_uuid(
    field: &'static str,
    value: &str,
    required_version: Option<usize>,
) -> Result<(), EventContractError> {
    let parsed = uuid::Uuid::parse_str(value).map_err(|_| EventContractError::InvalidField {
        field,
        reason: "must be a canonical UUID",
    })?;
    if parsed.hyphenated().to_string() != value
        || required_version.is_some_and(|version| parsed.get_version_num() != version)
    {
        return Err(EventContractError::InvalidField {
            field,
            reason: "has an invalid canonical form or version",
        });
    }
    Ok(())
}

fn authority_field(kind: AuthorityKindV1) -> &'static str {
    match kind {
        AuthorityKindV1::Tenant => "tenant",
        AuthorityKindV1::Company => "company",
        AuthorityKindV1::Project => "project",
        AuthorityKindV1::Workflow => "workflow",
        AuthorityKindV1::WorkItem => "work_item",
    }
}

#[cfg(test)]
mod tests {
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

    fn proposal() -> AppendProposalV2 {
        let payload = br#"{"state":"accepted"}"#.to_vec();
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
                operation_id: "operation-a".to_string(),
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
            expected_stream_revision: ExpectedStreamRevision::NoStream,
            delivery_intents: Vec::new(),
            effect_reservations: Vec::new(),
        }
    }

    #[test]
    fn canonical_proposal_digest_is_stable_and_scope_excludes_operation() {
        let first = proposal();
        let first_request = first.canonical_request_digest().unwrap();
        let first_scope = first.causal_context.authority_scope_digest().unwrap();
        let mut second = first.clone();
        second.causal_context.operation_id = "operation-b".to_string();
        assert_ne!(first_request, second.canonical_request_digest().unwrap());
        assert_eq!(
            first_scope,
            second.causal_context.authority_scope_digest().unwrap()
        );
    }

    #[test]
    fn context_rejects_work_item_without_workflow() {
        let mut value = proposal();
        value.causal_context.workflow = None;
        value.causal_context.work_item = Some(authority(AuthorityKindV1::WorkItem, "work-a"));
        assert_eq!(
            value.validate(),
            Err(EventContractError::InvalidAuthorityHierarchy(
                "work_item_without_workflow"
            ))
        );
    }

    #[test]
    fn payload_and_registry_authority_fail_closed() {
        let mut value = proposal();
        value.payload.push(b' ');
        assert_eq!(
            value.validate(),
            Err(EventContractError::PayloadDigestMismatch)
        );

        let registry = EventSchemaRegistry::new([EventSchemaDefinition {
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
        .unwrap();
        let mut unauthorized = proposal();
        unauthorized.producer = "dashboard".to_string();
        assert!(matches!(
            registry.validate_proposal(&unauthorized),
            Err(EventContractError::UnauthorizedProducer { .. })
        ));

        let mut non_root = proposal();
        non_root.causal_context.causation_event_id =
            Some("01890f3d-0000-7000-8000-000000000001".to_string());
        assert!(matches!(
            registry.validate_proposal(&non_root),
            Err(EventContractError::InvalidField {
                field: "causation_event_id",
                ..
            })
        ));

        let direct_registry = EventSchemaRegistry::new([EventSchemaDefinition {
            event_type: "project_accepted".to_string(),
            schema_version: 1,
            durability: EventDurability::Authoritative,
            payload_codec: EventPayloadCodec::Json,
            causation_policy: CausationPolicyV1::DirectRequired,
            allowed_producers: BTreeSet::from(["workflow".to_string()]),
            deterministic_event_id_producers: BTreeSet::new(),
            validator_id: "project-accepted-v1".to_string(),
            validate_payload: validate_json_object_payload,
            upcast: None,
        }])
        .unwrap();
        assert!(matches!(
            direct_registry.validate_proposal(&proposal()),
            Err(EventContractError::InvalidField {
                field: "causation_event_id",
                ..
            })
        ));
    }

    fn upcast_project_v1(payload: &[u8]) -> Result<Vec<u8>, EventContractError> {
        let mut value: serde_json::Value = serde_json::from_slice(payload)
            .map_err(|error| EventContractError::CanonicalEncoding(error.to_string()))?;
        value
            .as_object_mut()
            .ok_or(EventContractError::InvalidField {
                field: "payload",
                reason: "must be an object",
            })?
            .insert("reviewed".to_string(), serde_json::Value::Bool(false));
        canonical_json(&value)
    }

    #[test]
    fn schema_registry_digest_and_upcast_chain_are_deterministic() {
        let registry = EventSchemaRegistry::new([
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
                upcast: Some(EventSchemaUpcast {
                    target_version: 2,
                    upcaster_id: "project-accepted-v1-to-v2".to_string(),
                    upcast: upcast_project_v1,
                }),
            },
            EventSchemaDefinition {
                event_type: "project_accepted".to_string(),
                schema_version: 2,
                durability: EventDurability::Authoritative,
                payload_codec: EventPayloadCodec::Json,
                causation_policy: CausationPolicyV1::RootRequired,
                allowed_producers: BTreeSet::from(["workflow".to_string()]),
                deterministic_event_id_producers: BTreeSet::new(),
                validator_id: "project-accepted-v2".to_string(),
                validate_payload: validate_json_object_payload,
                upcast: None,
            },
        ])
        .unwrap();
        assert_eq!(registry.manifest_digest().unwrap().len(), 64);

        let result = registry
            .upcast_to_latest("project_accepted", 1, br#"{"state":"accepted"}"#)
            .unwrap();
        assert_eq!(result.source_version, 1);
        assert_eq!(result.target_version, 2);
        assert_eq!(result.applied_upcasters, ["project-accepted-v1-to-v2"]);
        assert_eq!(result.payload, br#"{"reviewed":false,"state":"accepted"}"#);
        assert_eq!(result.payload_digest, sha256_hex(&result.payload));
    }

    #[test]
    fn known_but_unsupported_schema_version_is_typed() {
        let registry = EventSchemaRegistry::new([EventSchemaDefinition {
            event_type: "project_accepted".to_string(),
            schema_version: 2,
            durability: EventDurability::Authoritative,
            payload_codec: EventPayloadCodec::Json,
            causation_policy: CausationPolicyV1::RootRequired,
            allowed_producers: BTreeSet::from(["workflow".to_string()]),
            deterministic_event_id_producers: BTreeSet::new(),
            validator_id: "project-accepted-v2".to_string(),
            validate_payload: validate_json_object_payload,
            upcast: None,
        }])
        .unwrap();
        assert!(matches!(
            registry.upcast_to_latest("project_accepted", 1, b"{}"),
            Err(EventContractError::UnsupportedKnownSchemaVersion {
                schema_version: 1,
                ..
            })
        ));
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct EventGoldenVector {
        case: String,
        causation_policy: CausationPolicyV1,
        causal_context_canonical_json: String,
        causal_context_sha256: String,
        authority_scope_sha256: String,
        proposal: AppendProposalV2,
        proposal_canonical_json: String,
        proposal_sha256: String,
        envelope: EventEnvelopeV2,
        envelope_canonical_json: String,
        envelope_sha256: String,
    }

    #[test]
    fn go_customer_admission_through_delivery_vectors_are_canonical() {
        let vectors: Vec<EventGoldenVector> = serde_json::from_str(include_str!(
            "../../../schemas/event/v2/golden-vectors.json"
        ))
        .unwrap();
        assert_eq!(vectors.len(), 6);

        let mut prior_event_id: Option<String> = None;
        for vector in vectors {
            let context_json = canonical_json(&vector.proposal.causal_context).unwrap();
            assert_eq!(
                context_json,
                vector.causal_context_canonical_json.as_bytes()
            );
            assert_eq!(sha256_hex(&context_json), vector.causal_context_sha256);
            assert_eq!(
                vector
                    .proposal
                    .causal_context
                    .authority_scope_digest()
                    .unwrap(),
                vector.authority_scope_sha256
            );

            let registry = EventSchemaRegistry::new([EventSchemaDefinition {
                event_type: vector.proposal.event_type.clone(),
                schema_version: vector.proposal.schema_version,
                durability: vector.proposal.requested_durability,
                payload_codec: vector.proposal.payload_codec,
                causation_policy: vector.causation_policy,
                allowed_producers: BTreeSet::from([vector.proposal.producer.clone()]),
                deterministic_event_id_producers: BTreeSet::from([vector
                    .proposal
                    .producer
                    .clone()]),
                validator_id: format!("{}-v1", vector.case),
                validate_payload: validate_json_object_payload,
                upcast: None,
            }])
            .unwrap();
            registry.validate_proposal(&vector.proposal).unwrap();
            assert_eq!(
                canonical_json(&vector.proposal).unwrap(),
                vector.proposal_canonical_json.as_bytes()
            );
            assert_eq!(
                vector.proposal.canonical_request_digest().unwrap(),
                vector.proposal_sha256
            );

            match (
                &prior_event_id,
                &vector.proposal.causal_context.causation_event_id,
            ) {
                (None, None) => {
                    assert_eq!(vector.causation_policy, CausationPolicyV1::RootRequired)
                }
                (Some(prior), Some(causation)) => {
                    assert_eq!(causation, prior);
                    assert_eq!(vector.causation_policy, CausationPolicyV1::DirectRequired);
                }
                relation => panic!("invalid causal chain for {}: {relation:?}", vector.case),
            }

            assert_eq!(vector.envelope.event_type, vector.proposal.event_type);
            assert_eq!(vector.envelope.payload, vector.proposal.payload);
            assert_eq!(
                vector.envelope.causal_context,
                vector.proposal.causal_context
            );
            assert_eq!(
                vector.envelope.canonical_request_digest,
                vector.proposal_sha256
            );
            assert_eq!(
                vector.envelope.expected_append_receipt_digest().unwrap(),
                vector.envelope.append_receipt_digest
            );
            assert_eq!(
                vector.envelope.expected_sealed_envelope_digest().unwrap(),
                vector.envelope.sealed_envelope_digest
            );
            vector.envelope.validate_seals().unwrap();
            assert_eq!(
                canonical_json(&vector.envelope).unwrap(),
                vector.envelope_canonical_json.as_bytes()
            );
            assert_eq!(
                vector.envelope.canonical_envelope_digest().unwrap(),
                vector.envelope_sha256
            );
            prior_event_id = Some(vector.envelope.event_id);
        }
    }
}
