//! Cross-language inference authority records and deterministic CBOR codec.
//!
//! The schema intentionally exposes a small closed value algebra. It cannot
//! encode floats, negative integers, null, tags, indefinite values, or unknown
//! fields, so invalid authority material fails before hashing or persistence.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

use crate::{
    CausationPolicyV1, EventContractError, EventDurability, EventPayloadCodec,
    EventSchemaDefinition, EventSchemaRegistry,
};

pub const INFERENCE_SCHEMA_VERSION_V1: u64 = 1;
pub const INFERENCE_DIGEST_DOMAIN: &[u8] = b"sentinel.inference.control";
pub const MAX_INFERENCE_RECORD_BYTES: usize = 64 * 1024;
const MAX_AUTHORITY_TEXT_BYTES: usize = 128;
const MAX_PROVIDER_REQUEST_ID_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum InferenceRecordTypeV1 {
    AdmissionIntent,
    BudgetReservation,
    BudgetReservationTransition,
    BudgetExemption,
    InferenceAdmission,
    AdmissionDisposition,
    InferenceAuthorityPort,
    ProviderDispatchReceipt,
    ProviderAttemptOutcome,
    UsageOutcome,
    ProviderCapabilities,
}

impl InferenceRecordTypeV1 {
    pub const ALL: [Self; 11] = [
        Self::AdmissionIntent,
        Self::BudgetReservation,
        Self::BudgetReservationTransition,
        Self::BudgetExemption,
        Self::InferenceAdmission,
        Self::AdmissionDisposition,
        Self::InferenceAuthorityPort,
        Self::ProviderDispatchReceipt,
        Self::ProviderAttemptOutcome,
        Self::UsageOutcome,
        Self::ProviderCapabilities,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AdmissionIntent => "AdmissionIntentV1",
            Self::BudgetReservation => "BudgetReservationV1",
            Self::BudgetReservationTransition => "BudgetReservationTransitionV1",
            Self::BudgetExemption => "BudgetExemptionV1",
            Self::InferenceAdmission => "InferenceAdmissionV1",
            Self::AdmissionDisposition => "AdmissionDispositionV1",
            Self::InferenceAuthorityPort => "InferenceAuthorityPortV1",
            Self::ProviderDispatchReceipt => "ProviderDispatchReceiptV1",
            Self::ProviderAttemptOutcome => "ProviderAttemptOutcomeV1",
            Self::UsageOutcome => "UsageOutcomeV1",
            Self::ProviderCapabilities => "ProviderCapabilitiesV1",
        }
    }

    fn own_digest_field(self) -> &'static str {
        match self {
            Self::AdmissionIntent => "admission_intent_digest",
            Self::BudgetReservation => "reservation_digest",
            Self::BudgetReservationTransition => "transition_payload_digest",
            Self::BudgetExemption => "exemption_digest",
            Self::InferenceAdmission => "admission_digest",
            Self::AdmissionDisposition => "disposition_payload_digest",
            Self::InferenceAuthorityPort => "authority_request_digest",
            Self::ProviderDispatchReceipt => "dispatch_payload_digest",
            Self::ProviderAttemptOutcome => "outcome_payload_digest",
            Self::UsageOutcome => "usage_payload_digest",
            Self::ProviderCapabilities => "capability_digest",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InferenceValueV1 {
    Unsigned(u64),
    Text(String),
    Bytes(Vec<u8>),
    Digest([u8; 32]),
    Bool(bool),
    Array(Vec<InferenceValueV1>),
    Object(BTreeMap<String, InferenceValueV1>),
}

impl InferenceValueV1 {
    fn kind(&self) -> FieldKind {
        match self {
            Self::Unsigned(_) => FieldKind::Unsigned,
            Self::Text(_) => FieldKind::Text,
            Self::Bytes(_) => FieldKind::Bytes,
            Self::Digest(_) => FieldKind::Digest,
            Self::Bool(_) => FieldKind::Bool,
            Self::Array(_) => FieldKind::Array,
            Self::Object(_) => FieldKind::Object,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferenceRecordV1 {
    record_type: InferenceRecordTypeV1,
    fields: BTreeMap<String, InferenceValueV1>,
    digest: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InferenceContractError {
    #[error("unknown inference record type {0}")]
    UnknownRecordType(String),
    #[error("unsupported inference schema version {0}")]
    UnknownVersion(u64),
    #[error("missing required field {0}")]
    MissingField(String),
    #[error("unknown field {0}")]
    UnknownField(String),
    #[error("invalid type for field {0}")]
    InvalidFieldType(String),
    #[error("invalid value for field {field}: {reason}")]
    InvalidValue { field: String, reason: &'static str },
    #[error("inference record digest does not match canonical bytes")]
    DigestMismatch,
    #[error("invalid deterministic CBOR: {0}")]
    InvalidCbor(&'static str),
    #[error("inference record exceeds its size bound")]
    RecordTooLarge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldKind {
    Unsigned,
    Text,
    Bytes,
    Digest,
    Bool,
    Array,
    Object,
}

impl FieldKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Unsigned => "unsigned",
            Self::Text => "text",
            Self::Bytes => "bytes",
            Self::Digest => "digest32",
            Self::Bool => "bool",
            Self::Array => "array",
            Self::Object => "object",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct FieldSpec {
    name: &'static str,
    kind: FieldKind,
    required: bool,
}

impl InferenceRecordV1 {
    pub fn new(
        record_type: InferenceRecordTypeV1,
        fields: BTreeMap<String, InferenceValueV1>,
    ) -> Result<Self, InferenceContractError> {
        validate_fields(record_type, &fields)?;
        let digest = digest_record(record_type, &fields)?;
        Ok(Self {
            record_type,
            fields,
            digest,
        })
    }

    pub fn decode(
        record_type: InferenceRecordTypeV1,
        bytes: &[u8],
    ) -> Result<Self, InferenceContractError> {
        if bytes.len() > MAX_INFERENCE_RECORD_BYTES {
            return Err(InferenceContractError::RecordTooLarge);
        }
        let value = CborDecoder::new(bytes).decode_complete()?;
        let InferenceValueV1::Object(mut fields) = value else {
            return Err(InferenceContractError::InvalidCbor("root must be a map"));
        };
        let version = take_unsigned(&mut fields, "version")?;
        if version != INFERENCE_SCHEMA_VERSION_V1 {
            return Err(InferenceContractError::UnknownVersion(version));
        }
        let digest_value = fields
            .remove(record_type.own_digest_field())
            .ok_or_else(|| {
                InferenceContractError::MissingField(record_type.own_digest_field().to_string())
            })?;
        let digest = match digest_value {
            InferenceValueV1::Bytes(bytes) if bytes.len() == 32 => {
                let mut digest = [0_u8; 32];
                digest.copy_from_slice(&bytes);
                digest
            }
            _ => {
                return Err(InferenceContractError::InvalidFieldType(
                    record_type.own_digest_field().to_string(),
                ))
            }
        };
        normalize_digest_fields(record_type, &mut fields)?;
        validate_fields(record_type, &fields)?;
        if digest_record(record_type, &fields)? != digest {
            return Err(InferenceContractError::DigestMismatch);
        }
        Ok(Self {
            record_type,
            fields,
            digest,
        })
    }

    pub const fn record_type(&self) -> InferenceRecordTypeV1 {
        self.record_type
    }

    pub fn fields(&self) -> &BTreeMap<String, InferenceValueV1> {
        &self.fields
    }

    pub const fn digest(&self) -> [u8; 32] {
        self.digest
    }

    pub fn digest_hex(&self) -> String {
        hex_lower(&self.digest)
    }

    pub fn canonical_hash_payload(&self) -> Result<Vec<u8>, InferenceContractError> {
        canonical_hash_payload(&self.fields)
    }

    pub fn canonical_wire_payload(&self) -> Result<Vec<u8>, InferenceContractError> {
        let mut fields = self.fields.clone();
        fields.insert(
            "version".to_string(),
            InferenceValueV1::Unsigned(INFERENCE_SCHEMA_VERSION_V1),
        );
        fields.insert(
            self.record_type.own_digest_field().to_string(),
            InferenceValueV1::Digest(self.digest),
        );
        canonical_payload(&fields)
    }
}

macro_rules! typed_record {
    ($name:ident, $kind:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct $name(InferenceRecordV1);

        impl $name {
            pub fn new(
                fields: BTreeMap<String, InferenceValueV1>,
            ) -> Result<Self, InferenceContractError> {
                InferenceRecordV1::new(InferenceRecordTypeV1::$kind, fields).map(Self)
            }

            pub fn decode(bytes: &[u8]) -> Result<Self, InferenceContractError> {
                InferenceRecordV1::decode(InferenceRecordTypeV1::$kind, bytes).map(Self)
            }

            pub fn record(&self) -> &InferenceRecordV1 {
                &self.0
            }
        }
    };
}

typed_record!(AdmissionIntentV1, AdmissionIntent);
typed_record!(BudgetReservationV1, BudgetReservation);
typed_record!(BudgetReservationTransitionV1, BudgetReservationTransition);
typed_record!(BudgetExemptionV1, BudgetExemption);
typed_record!(InferenceAdmissionV1, InferenceAdmission);
typed_record!(AdmissionDispositionV1, AdmissionDisposition);
typed_record!(InferenceAuthorityPortV1, InferenceAuthorityPort);
typed_record!(ProviderDispatchReceiptV1, ProviderDispatchReceipt);
typed_record!(ProviderAttemptOutcomeV1, ProviderAttemptOutcome);
typed_record!(UsageOutcomeV1, UsageOutcome);
typed_record!(ProviderCapabilitiesV1, ProviderCapabilities);

pub const INFERENCE_AUTHORITY_PORT_METHODS_V1: [&str; 6] = [
    "RESERVE_OR_EXEMPT",
    "FINALIZE_ADMISSION",
    "WIN_PRE_DISPATCH_DISPOSITION",
    "BEGIN_DISPATCH",
    "COMMIT_ATTEMPT_OUTCOME",
    "RECONCILE_USAGE",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InferenceAuthorityResultV1 {
    Committed,
    ReplayedReadback,
    Denied,
    IdempotencyConflict,
    StalePredecessor,
    IllegalTransition,
    UnknownVersion,
    UnknownMethod,
    Unauthorized,
    Unavailable,
}

impl InferenceAuthorityResultV1 {
    pub const ALL: [Self; 10] = [
        Self::Committed,
        Self::ReplayedReadback,
        Self::Denied,
        Self::IdempotencyConflict,
        Self::StalePredecessor,
        Self::IllegalTransition,
        Self::UnknownVersion,
        Self::UnknownMethod,
        Self::Unauthorized,
        Self::Unavailable,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Committed => "COMMITTED",
            Self::ReplayedReadback => "REPLAYED_READBACK",
            Self::Denied => "DENIED",
            Self::IdempotencyConflict => "IDEMPOTENCY_CONFLICT",
            Self::StalePredecessor => "STALE_PREDECESSOR",
            Self::IllegalTransition => "ILLEGAL_TRANSITION",
            Self::UnknownVersion => "UNKNOWN_VERSION",
            Self::UnknownMethod => "UNKNOWN_METHOD",
            Self::Unauthorized => "UNAUTHORIZED",
            Self::Unavailable => "UNAVAILABLE",
        }
    }

    fn parse(value: &str) -> Result<Self, InferenceContractError> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == value)
            .ok_or_else(|| InferenceContractError::InvalidValue {
                field: "result".to_string(),
                reason: "unknown authority-port result",
            })
    }

    const fn is_committed_readback(self) -> bool {
        matches!(self, Self::Committed | Self::ReplayedReadback)
    }
}

/// Typed response from the Rust inference authority to the Go edge gateway.
///
/// The request method is authenticated transport context rather than a wire
/// field. It is required while constructing or decoding the response so that
/// only a fresh `BEGIN_DISPATCH` commit can authorize provider I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferenceAuthorityResponseV1 {
    result: InferenceAuthorityResultV1,
    committed_operation_id: Option<String>,
    committed_payload_digest: Option<[u8; 32]>,
    aggregate_state: Option<String>,
    provider_io_authorized: bool,
}

impl InferenceAuthorityResponseV1 {
    pub fn new(
        request_method: &str,
        result: InferenceAuthorityResultV1,
        committed_operation_id: Option<String>,
        committed_payload_digest: Option<[u8; 32]>,
        aggregate_state: Option<String>,
        provider_io_authorized: bool,
    ) -> Result<Self, InferenceContractError> {
        validate_authority_response(
            request_method,
            result,
            committed_operation_id.as_deref(),
            committed_payload_digest.as_ref(),
            aggregate_state.as_deref(),
            provider_io_authorized,
        )?;
        Ok(Self {
            result,
            committed_operation_id,
            committed_payload_digest,
            aggregate_state,
            provider_io_authorized,
        })
    }

    pub fn decode(request_method: &str, bytes: &[u8]) -> Result<Self, InferenceContractError> {
        if bytes.len() > MAX_INFERENCE_RECORD_BYTES {
            return Err(InferenceContractError::RecordTooLarge);
        }
        let value = CborDecoder::new(bytes).decode_complete()?;
        let InferenceValueV1::Object(mut fields) = value else {
            return Err(InferenceContractError::InvalidCbor("root must be a map"));
        };
        let version = take_unsigned(&mut fields, "version")?;
        if version != INFERENCE_SCHEMA_VERSION_V1 {
            return Err(InferenceContractError::UnknownVersion(version));
        }
        for field in fields.keys() {
            if ![
                "result",
                "committed_operation_id_optional",
                "committed_payload_digest_optional",
                "aggregate_state_optional",
                "provider_io_authorized",
            ]
            .contains(&field.as_str())
            {
                return Err(InferenceContractError::UnknownField(field.clone()));
            }
        }
        let result = InferenceAuthorityResultV1::parse(text_field(&fields, "result")?)?;
        let committed_operation_id = optional_text(&fields, "committed_operation_id_optional")?;
        let committed_payload_digest =
            optional_digest(&fields, "committed_payload_digest_optional")?;
        let aggregate_state = optional_text(&fields, "aggregate_state_optional")?;
        let provider_io_authorized = match fields.get("provider_io_authorized") {
            Some(InferenceValueV1::Bool(value)) => *value,
            Some(_) => {
                return Err(InferenceContractError::InvalidFieldType(
                    "provider_io_authorized".to_string(),
                ))
            }
            None => {
                return Err(InferenceContractError::MissingField(
                    "provider_io_authorized".to_string(),
                ))
            }
        };
        Self::new(
            request_method,
            result,
            committed_operation_id,
            committed_payload_digest,
            aggregate_state,
            provider_io_authorized,
        )
    }

    pub const fn result(&self) -> InferenceAuthorityResultV1 {
        self.result
    }

    pub const fn provider_io_authorized(&self) -> bool {
        self.provider_io_authorized
    }

    pub fn canonical_wire_payload(&self) -> Result<Vec<u8>, InferenceContractError> {
        let mut fields = BTreeMap::from([
            (
                "version".to_string(),
                InferenceValueV1::Unsigned(INFERENCE_SCHEMA_VERSION_V1),
            ),
            (
                "result".to_string(),
                InferenceValueV1::Text(self.result.as_str().to_string()),
            ),
            (
                "provider_io_authorized".to_string(),
                InferenceValueV1::Bool(self.provider_io_authorized),
            ),
        ]);
        if let Some(value) = &self.committed_operation_id {
            fields.insert(
                "committed_operation_id_optional".to_string(),
                InferenceValueV1::Text(value.clone()),
            );
        }
        if let Some(value) = self.committed_payload_digest {
            fields.insert(
                "committed_payload_digest_optional".to_string(),
                InferenceValueV1::Digest(value),
            );
        }
        if let Some(value) = &self.aggregate_state {
            fields.insert(
                "aggregate_state_optional".to_string(),
                InferenceValueV1::Text(value.clone()),
            );
        }
        canonical_payload(&fields)
    }

    pub fn digest(&self) -> Result<[u8; 32], InferenceContractError> {
        digest_named_payload(
            "InferenceAuthorityResponseV1",
            &self.canonical_wire_payload()?,
        )
    }

    pub fn digest_hex(&self) -> Result<String, InferenceContractError> {
        Ok(hex_lower(&self.digest()?))
    }
}

fn validate_authority_response(
    request_method: &str,
    result: InferenceAuthorityResultV1,
    committed_operation_id: Option<&str>,
    committed_payload_digest: Option<&[u8; 32]>,
    aggregate_state: Option<&str>,
    provider_io_authorized: bool,
) -> Result<(), InferenceContractError> {
    if !INFERENCE_AUTHORITY_PORT_METHODS_V1.contains(&request_method) {
        return invalid("method", "unknown authority-port method");
    }
    if committed_operation_id.is_some() != committed_payload_digest.is_some() {
        return invalid(
            "committed_operation_id_optional",
            "committed operation and payload digest must be paired",
        );
    }
    if result.is_committed_readback() != committed_operation_id.is_some() {
        return invalid(
            "committed_operation_id_optional",
            "only committed readbacks carry committed identity",
        );
    }
    if let Some(value) = committed_operation_id {
        validate_text(value)?;
    }
    if let Some(value) = aggregate_state {
        validate_text(value)?;
        if !value.is_ascii() {
            return invalid("aggregate_state_optional", "aggregate state must be ASCII");
        }
    }
    if provider_io_authorized
        && (request_method != "BEGIN_DISPATCH" || result != InferenceAuthorityResultV1::Committed)
    {
        return invalid(
            "provider_io_authorized",
            "only a fresh BEGIN_DISPATCH commit authorizes provider I/O",
        );
    }
    Ok(())
}

macro_rules! inference_payload_validator {
    ($function:ident, $record_type:ident) => {
        fn $function(payload: &[u8]) -> Result<(), EventContractError> {
            InferenceRecordV1::decode(InferenceRecordTypeV1::$record_type, payload)
                .map(|_| ())
                .map_err(|_| EventContractError::InvalidField {
                    field: "inference_payload",
                    reason: "payload is not canonical for the registered inference schema",
                })
        }
    };
}

inference_payload_validator!(validate_admission_intent_payload, AdmissionIntent);
inference_payload_validator!(validate_budget_reservation_payload, BudgetReservation);
inference_payload_validator!(
    validate_budget_reservation_transition_payload,
    BudgetReservationTransition
);
inference_payload_validator!(validate_budget_exemption_payload, BudgetExemption);
inference_payload_validator!(validate_inference_admission_payload, InferenceAdmission);
inference_payload_validator!(validate_admission_disposition_payload, AdmissionDisposition);
inference_payload_validator!(
    validate_inference_authority_port_payload,
    InferenceAuthorityPort
);
inference_payload_validator!(
    validate_provider_dispatch_receipt_payload,
    ProviderDispatchReceipt
);
inference_payload_validator!(
    validate_provider_attempt_outcome_payload,
    ProviderAttemptOutcome
);
inference_payload_validator!(validate_usage_outcome_payload, UsageOutcome);
inference_payload_validator!(validate_provider_capabilities_payload, ProviderCapabilities);

/// Canonical event-schema registry entries for the S0 inference authority.
///
/// C0 owns authoritative admission, budget, attempt, and usage records. The Go
/// edge owns only authenticated port proposals and measured provider
/// capabilities; it cannot append the resulting authority records directly.
pub fn inference_event_schema_registry() -> Result<EventSchemaRegistry, EventContractError> {
    let c0 = BTreeSet::from(["sentinel-inference-authority".to_string()]);
    let edge = BTreeSet::from(["cortex-gateway".to_string()]);
    let definitions = [
        (
            InferenceRecordTypeV1::AdmissionIntent,
            EventDurability::Authoritative,
            c0.clone(),
            validate_admission_intent_payload as fn(&[u8]) -> Result<(), EventContractError>,
        ),
        (
            InferenceRecordTypeV1::BudgetReservation,
            EventDurability::Authoritative,
            c0.clone(),
            validate_budget_reservation_payload,
        ),
        (
            InferenceRecordTypeV1::BudgetReservationTransition,
            EventDurability::Authoritative,
            c0.clone(),
            validate_budget_reservation_transition_payload,
        ),
        (
            InferenceRecordTypeV1::BudgetExemption,
            EventDurability::Authoritative,
            c0.clone(),
            validate_budget_exemption_payload,
        ),
        (
            InferenceRecordTypeV1::InferenceAdmission,
            EventDurability::Authoritative,
            c0.clone(),
            validate_inference_admission_payload,
        ),
        (
            InferenceRecordTypeV1::AdmissionDisposition,
            EventDurability::Authoritative,
            c0.clone(),
            validate_admission_disposition_payload,
        ),
        (
            InferenceRecordTypeV1::InferenceAuthorityPort,
            EventDurability::DurableOperational,
            edge.clone(),
            validate_inference_authority_port_payload,
        ),
        (
            InferenceRecordTypeV1::ProviderDispatchReceipt,
            EventDurability::Authoritative,
            c0.clone(),
            validate_provider_dispatch_receipt_payload,
        ),
        (
            InferenceRecordTypeV1::ProviderAttemptOutcome,
            EventDurability::Authoritative,
            c0.clone(),
            validate_provider_attempt_outcome_payload,
        ),
        (
            InferenceRecordTypeV1::UsageOutcome,
            EventDurability::Authoritative,
            c0,
            validate_usage_outcome_payload,
        ),
        (
            InferenceRecordTypeV1::ProviderCapabilities,
            EventDurability::DurableOperational,
            edge,
            validate_provider_capabilities_payload,
        ),
    ];
    EventSchemaRegistry::new(definitions.into_iter().map(
        |(record_type, durability, allowed_producers, validate_payload)| EventSchemaDefinition {
            event_type: record_type.as_str().to_string(),
            schema_version: INFERENCE_SCHEMA_VERSION_V1 as u32,
            durability,
            payload_codec: EventPayloadCodec::DeterministicCbor,
            causation_policy: CausationPolicyV1::DirectRequired,
            allowed_producers,
            deterministic_event_id_producers: BTreeSet::new(),
            validator_id: format!("{}-canonical-cbor-v1", record_type.as_str()),
            validate_payload,
            upcast: None,
        },
    ))
}

pub fn inference_schema_digest() -> Result<String, InferenceContractError> {
    let manifest = InferenceRecordTypeV1::ALL
        .into_iter()
        .map(|record_type| {
            let fields = schema(record_type)
                .iter()
                .map(|field| {
                    InferenceValueV1::Object(BTreeMap::from([
                        (
                            "kind".to_string(),
                            InferenceValueV1::Text(field.kind.as_str().to_string()),
                        ),
                        (
                            "name".to_string(),
                            InferenceValueV1::Text(field.name.to_string()),
                        ),
                        (
                            "required".to_string(),
                            InferenceValueV1::Bool(field.required),
                        ),
                    ]))
                })
                .collect();
            InferenceValueV1::Object(BTreeMap::from([
                (
                    "digest_field".to_string(),
                    InferenceValueV1::Text(record_type.own_digest_field().to_string()),
                ),
                ("fields".to_string(), InferenceValueV1::Array(fields)),
                (
                    "record_type".to_string(),
                    InferenceValueV1::Text(record_type.as_str().to_string()),
                ),
            ]))
        })
        .collect();
    let bytes = canonical_payload(&BTreeMap::from([
        (
            "domain".to_string(),
            InferenceValueV1::Text("sentinel.inference.control".to_string()),
        ),
        ("records".to_string(), InferenceValueV1::Array(manifest)),
        (
            "version".to_string(),
            InferenceValueV1::Unsigned(INFERENCE_SCHEMA_VERSION_V1),
        ),
    ]))?;
    Ok(hex_lower(&Sha256::digest(bytes)))
}

fn digest_record(
    record_type: InferenceRecordTypeV1,
    fields: &BTreeMap<String, InferenceValueV1>,
) -> Result<[u8; 32], InferenceContractError> {
    let payload = canonical_hash_payload(fields)?;
    digest_named_payload(record_type.as_str(), &payload)
}

fn digest_named_payload(
    record_type: &str,
    payload: &[u8],
) -> Result<[u8; 32], InferenceContractError> {
    let record_type_bytes = record_type.as_bytes();
    let type_len = u16::try_from(record_type_bytes.len())
        .map_err(|_| InferenceContractError::RecordTooLarge)?;
    let payload_len =
        u32::try_from(payload.len()).map_err(|_| InferenceContractError::RecordTooLarge)?;
    let mut preimage = Vec::with_capacity(
        INFERENCE_DIGEST_DOMAIN.len() + 1 + 2 + record_type_bytes.len() + 2 + 4 + payload.len(),
    );
    preimage.extend_from_slice(INFERENCE_DIGEST_DOMAIN);
    preimage.push(0);
    preimage.extend_from_slice(&type_len.to_be_bytes());
    preimage.extend_from_slice(record_type_bytes);
    preimage.extend_from_slice(&(INFERENCE_SCHEMA_VERSION_V1 as u16).to_be_bytes());
    preimage.extend_from_slice(&payload_len.to_be_bytes());
    preimage.extend_from_slice(payload);
    Ok(Sha256::digest(preimage).into())
}

fn canonical_hash_payload(
    fields: &BTreeMap<String, InferenceValueV1>,
) -> Result<Vec<u8>, InferenceContractError> {
    let mut payload_fields = fields.clone();
    payload_fields.insert(
        "version".to_string(),
        InferenceValueV1::Unsigned(INFERENCE_SCHEMA_VERSION_V1),
    );
    canonical_payload(&payload_fields)
}

fn normalize_digest_fields(
    record_type: InferenceRecordTypeV1,
    fields: &mut BTreeMap<String, InferenceValueV1>,
) -> Result<(), InferenceContractError> {
    for spec in schema(record_type) {
        if spec.kind != FieldKind::Digest {
            continue;
        }
        let Some(value) = fields.remove(spec.name) else {
            continue;
        };
        let normalized = match value {
            InferenceValueV1::Bytes(bytes) if bytes.len() == 32 => {
                let mut digest = [0_u8; 32];
                digest.copy_from_slice(&bytes);
                InferenceValueV1::Digest(digest)
            }
            other => other,
        };
        fields.insert(spec.name.to_string(), normalized);
    }
    Ok(())
}

fn validate_fields(
    record_type: InferenceRecordTypeV1,
    fields: &BTreeMap<String, InferenceValueV1>,
) -> Result<(), InferenceContractError> {
    let specs = schema(record_type);
    for spec in specs {
        match fields.get(spec.name) {
            Some(value) if value.kind() == spec.kind => validate_value(spec.name, value)?,
            Some(_) => {
                return Err(InferenceContractError::InvalidFieldType(
                    spec.name.to_string(),
                ))
            }
            None if spec.required => {
                return Err(InferenceContractError::MissingField(spec.name.to_string()))
            }
            None => {}
        }
    }
    for field in fields.keys() {
        if !specs.iter().any(|spec| spec.name == field) {
            return Err(InferenceContractError::UnknownField(field.clone()));
        }
    }
    validate_relations(record_type, fields)
}

fn validate_value(field: &str, value: &InferenceValueV1) -> Result<(), InferenceContractError> {
    match value {
        InferenceValueV1::Text(text) => {
            let max = if field == "provider_request_id_optional" {
                MAX_PROVIDER_REQUEST_ID_BYTES
            } else {
                MAX_AUTHORITY_TEXT_BYTES
            };
            if text.is_empty() || text.len() > max || text.nfc().ne(text.chars()) {
                return Err(InferenceContractError::InvalidValue {
                    field: field.to_string(),
                    reason: "must be non-empty bounded NFC text",
                });
            }
            if field != "provider_request_id_optional" && !text.is_ascii() {
                return Err(InferenceContractError::InvalidValue {
                    field: field.to_string(),
                    reason: "authority identifiers and enum symbols must be ASCII",
                });
            }
        }
        InferenceValueV1::Bytes(bytes) if bytes.is_empty() => {
            return Err(InferenceContractError::InvalidValue {
                field: field.to_string(),
                reason: "byte payload must be non-empty",
            })
        }
        InferenceValueV1::Array(values) => {
            for value in values {
                validate_nested(value)?;
            }
        }
        InferenceValueV1::Object(values) => {
            for (name, value) in values {
                validate_text(name)?;
                validate_nested(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_nested(value: &InferenceValueV1) -> Result<(), InferenceContractError> {
    match value {
        InferenceValueV1::Text(text) => validate_text(text),
        InferenceValueV1::Bytes(bytes) if bytes.is_empty() => Err(
            InferenceContractError::InvalidCbor("empty nested bytes are forbidden"),
        ),
        InferenceValueV1::Array(values) => {
            for value in values {
                validate_nested(value)?;
            }
            Ok(())
        }
        InferenceValueV1::Object(values) => {
            for (key, value) in values {
                validate_text(key)?;
                validate_nested(value)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validate_text(text: &str) -> Result<(), InferenceContractError> {
    if text.is_empty() || text.len() > MAX_PROVIDER_REQUEST_ID_BYTES || text.nfc().ne(text.chars())
    {
        return Err(InferenceContractError::InvalidCbor(
            "text must be non-empty bounded NFC UTF-8",
        ));
    }
    Ok(())
}

fn validate_relations(
    record_type: InferenceRecordTypeV1,
    fields: &BTreeMap<String, InferenceValueV1>,
) -> Result<(), InferenceContractError> {
    match record_type {
        InferenceRecordTypeV1::BudgetReservation => validate_scopes(fields),
        InferenceRecordTypeV1::BudgetReservationTransition => {
            let to = text_field(fields, "to_state")?;
            let reason = text_field(fields, "transition_reason")?;
            let legal = matches!(
                (to, reason),
                (
                    "PRE_DISPATCH_RELEASED",
                    "QUEUE_FULL_BEFORE_DISPATCH"
                        | "CLIENT_CANCEL_BEFORE_DISPATCH"
                        | "DEADLINE_BEFORE_DISPATCH"
                ) | (
                    "DEFINITIVE_NON_BILLABLE_RELEASED",
                    "PROVIDER_DEFINITIVE_NON_BILLABLE"
                ) | ("RECONCILED", "PROVIDER_USAGE_RECONCILED")
                    | (
                        "QUARANTINED",
                        "CLIENT_CANCEL_AFTER_DISPATCH"
                            | "DEADLINE_AFTER_DISPATCH"
                            | "TRANSPORT_LOST"
                            | "INVALID_PROVIDER_RESPONSE"
                            | "GATEWAY_LOST_AFTER_DISPATCH_COMMIT"
                            | "EXPIRED_WITH_UNKNOWN_OUTCOME"
                    )
            );
            require_text(fields, "expected_predecessor_state", "RESERVED")?;
            require_text(fields, "from_state", "RESERVED")?;
            if !legal {
                return invalid("transition_reason", "illegal reservation transition");
            }
            Ok(())
        }
        InferenceRecordTypeV1::BudgetExemption => require_one_of(
            fields,
            "exemption_kind",
            &["NON_BILLABLE_LOCAL_LOOP", "NON_BILLABLE_FAKE_PROVIDER_TEST"],
        ),
        InferenceRecordTypeV1::InferenceAdmission => validate_authority_pair(fields),
        InferenceRecordTypeV1::AdmissionDisposition => {
            validate_optional_authority_pair(fields)?;
            require_text(fields, "expected_predecessor_state", "FINAL_ADMITTED")?;
            let disposition = text_field(fields, "disposition")?;
            let reason = text_field(fields, "disposition_reason")?;
            let legal = matches!(
                (disposition, reason),
                ("PRE_DISPATCH_REJECTED", "QUEUE_FULL" | "AUTHORITY_DENIED")
                    | ("PRE_DISPATCH_CANCELLED", "CLIENT_CANCELLED")
                    | (
                        "PRE_DISPATCH_DEADLINE_EXCEEDED",
                        "EXECUTION_DEADLINE_EXPIRED"
                    )
            );
            if !legal {
                return invalid("disposition_reason", "illegal disposition state/reason");
            }
            Ok(())
        }
        InferenceRecordTypeV1::InferenceAuthorityPort => {
            require_one_of(
                fields,
                "method",
                &[
                    "RESERVE_OR_EXEMPT",
                    "FINALIZE_ADMISSION",
                    "WIN_PRE_DISPATCH_DISPOSITION",
                    "BEGIN_DISPATCH",
                    "COMMIT_ATTEMPT_OUTCOME",
                    "RECONCILE_USAGE",
                ],
            )?;
            require_one_of(
                fields,
                "record_type",
                &InferenceRecordTypeV1::ALL.map(InferenceRecordTypeV1::as_str),
            )?;
            pair_present(
                fields,
                "expected_predecessor_operation_id_optional",
                "expected_predecessor_state_optional",
            )?;
            Ok(())
        }
        InferenceRecordTypeV1::ProviderDispatchReceipt => {
            validate_optional_authority_pair(fields)?;
            require_text(fields, "expected_predecessor_state", "FINAL_ADMITTED")
        }
        InferenceRecordTypeV1::ProviderAttemptOutcome => {
            validate_optional_authority_pair(fields)?;
            require_text(fields, "expected_predecessor_state", "DISPATCHED")?;
            let state = text_field(fields, "terminal_state")?;
            let reason = text_field(fields, "terminal_reason")?;
            let legal = matches!(
                (state, reason),
                (
                    "DEFINITIVE_REJECT",
                    "PROVIDER_DEFINITIVE_NON_BILLABLE_REJECT"
                ) | ("COMPLETED", "PROVIDER_SUCCESS")
                    | (
                        "AMBIGUOUS",
                        "CLIENT_CANCEL_AFTER_DISPATCH"
                            | "DEADLINE_AFTER_DISPATCH"
                            | "TRANSPORT_LOST"
                            | "INVALID_RESPONSE"
                            | "GATEWAY_LOST_AFTER_DISPATCH_COMMIT"
                    )
            );
            if !legal {
                return invalid("terminal_reason", "illegal terminal state/reason");
            }
            Ok(())
        }
        InferenceRecordTypeV1::UsageOutcome => {
            validate_optional_authority_pair(fields)?;
            require_one_of(
                fields,
                "cost_source",
                &[
                    "PROVIDER_REPORTED",
                    "CATALOG_COMPUTED",
                    "CONSERVATIVE_RESERVED",
                ],
            )?;
            if !matches!(fields.get("terminal"), Some(InferenceValueV1::Bool(true))) {
                return invalid("terminal", "usage must be terminal");
            }
            if fields.contains_key("budget_exemption_id_optional")
                && unsigned_field(fields, "resolved_cost_microusd_u64")? != 0
            {
                return invalid("resolved_cost_microusd_u64", "exempt usage must cost zero");
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validate_authority_pair(
    fields: &BTreeMap<String, InferenceValueV1>,
) -> Result<(), InferenceContractError> {
    let reservation = pair_present(
        fields,
        "budget_reservation_id_optional",
        "budget_reservation_digest_optional",
    )?;
    let exemption = pair_present(
        fields,
        "budget_exemption_id_optional",
        "budget_exemption_digest_optional",
    )?;
    if reservation == exemption {
        return invalid(
            "budget_authority",
            "exactly one reservation or exemption pair is required",
        );
    }
    Ok(())
}

fn validate_optional_authority_pair(
    fields: &BTreeMap<String, InferenceValueV1>,
) -> Result<(), InferenceContractError> {
    validate_authority_pair(fields)
}

fn pair_present(
    fields: &BTreeMap<String, InferenceValueV1>,
    id: &str,
    digest: &str,
) -> Result<bool, InferenceContractError> {
    match (fields.contains_key(id), fields.contains_key(digest)) {
        (true, true) => Ok(true),
        (false, false) => Ok(false),
        _ => invalid(id, "identity and digest must be present together"),
    }
}

fn validate_scopes(
    fields: &BTreeMap<String, InferenceValueV1>,
) -> Result<(), InferenceContractError> {
    let Some(InferenceValueV1::Array(scopes)) = fields.get("scopes") else {
        return Err(InferenceContractError::MissingField("scopes".to_string()));
    };
    if scopes.is_empty() {
        return invalid("scopes", "at least one derived scope is required");
    }
    let expected = BTreeSet::from([
        "scope_kind",
        "scope_id",
        "scope_generation_u64",
        "window_kind",
        "window_start_unix_ms",
    ]);
    let mut prior: Option<(String, String, u64, String, u64, Option<u64>)> = None;
    for scope in scopes {
        let InferenceValueV1::Object(scope) = scope else {
            return invalid("scopes", "scope row must be a map");
        };
        let actual: BTreeSet<_> = scope
            .keys()
            .filter(|key| key.as_str() != "window_end_unix_ms_optional")
            .map(String::as_str)
            .collect();
        if actual != expected {
            return invalid("scopes", "scope row has missing or unknown fields");
        }
        let row = (
            text_field(scope, "scope_kind")?.to_string(),
            text_field(scope, "scope_id")?.to_string(),
            unsigned_field(scope, "scope_generation_u64")?,
            text_field(scope, "window_kind")?.to_string(),
            unsigned_field(scope, "window_start_unix_ms")?,
            optional_unsigned_field(scope, "window_end_unix_ms_optional")?,
        );
        if !matches!(
            row.0.as_str(),
            "TENANT" | "PROJECT" | "WORK_ITEM" | "AGREEMENT" | "CUSTOMER" | "PROVIDER"
        ) {
            return invalid("scope_kind", "unknown scope kind");
        }
        if !matches!(
            row.3.as_str(),
            "LIFETIME" | "CALENDAR_HOUR" | "CALENDAR_DAY" | "FIXED_RANGE"
        ) {
            return invalid("window_kind", "unknown window kind");
        }
        match (row.3.as_str(), row.5) {
            ("FIXED_RANGE", Some(end)) if end > row.4 => {}
            ("FIXED_RANGE", _) => {
                return invalid(
                    "window_end_unix_ms_optional",
                    "fixed range requires an end after its start",
                )
            }
            (_, None) => {}
            (_, Some(_)) => {
                return invalid(
                    "window_end_unix_ms_optional",
                    "only fixed ranges may carry an end",
                )
            }
        }
        if prior.as_ref().is_some_and(|previous| previous >= &row) {
            return invalid("scopes", "scope rows must be sorted and unique");
        }
        prior = Some(row);
    }
    Ok(())
}

fn text_field<'a>(
    fields: &'a BTreeMap<String, InferenceValueV1>,
    name: &str,
) -> Result<&'a str, InferenceContractError> {
    match fields.get(name) {
        Some(InferenceValueV1::Text(value)) => Ok(value),
        Some(_) => Err(InferenceContractError::InvalidFieldType(name.to_string())),
        None => Err(InferenceContractError::MissingField(name.to_string())),
    }
}

fn unsigned_field(
    fields: &BTreeMap<String, InferenceValueV1>,
    name: &str,
) -> Result<u64, InferenceContractError> {
    match fields.get(name) {
        Some(InferenceValueV1::Unsigned(value)) => Ok(*value),
        Some(_) => Err(InferenceContractError::InvalidFieldType(name.to_string())),
        None => Err(InferenceContractError::MissingField(name.to_string())),
    }
}

fn optional_unsigned_field(
    fields: &BTreeMap<String, InferenceValueV1>,
    name: &str,
) -> Result<Option<u64>, InferenceContractError> {
    match fields.get(name) {
        Some(InferenceValueV1::Unsigned(value)) => Ok(Some(*value)),
        Some(_) => Err(InferenceContractError::InvalidFieldType(name.to_string())),
        None => Ok(None),
    }
}

fn optional_text(
    fields: &BTreeMap<String, InferenceValueV1>,
    name: &str,
) -> Result<Option<String>, InferenceContractError> {
    match fields.get(name) {
        Some(InferenceValueV1::Text(value)) => {
            validate_text(value)?;
            Ok(Some(value.clone()))
        }
        Some(_) => Err(InferenceContractError::InvalidFieldType(name.to_string())),
        None => Ok(None),
    }
}

fn optional_digest(
    fields: &BTreeMap<String, InferenceValueV1>,
    name: &str,
) -> Result<Option<[u8; 32]>, InferenceContractError> {
    match fields.get(name) {
        Some(InferenceValueV1::Bytes(value)) if value.len() == 32 => {
            let mut digest = [0_u8; 32];
            digest.copy_from_slice(value);
            Ok(Some(digest))
        }
        Some(InferenceValueV1::Digest(value)) => Ok(Some(*value)),
        Some(_) => Err(InferenceContractError::InvalidFieldType(name.to_string())),
        None => Ok(None),
    }
}

fn take_unsigned(
    fields: &mut BTreeMap<String, InferenceValueV1>,
    name: &str,
) -> Result<u64, InferenceContractError> {
    match fields.remove(name) {
        Some(InferenceValueV1::Unsigned(value)) => Ok(value),
        Some(_) => Err(InferenceContractError::InvalidFieldType(name.to_string())),
        None => Err(InferenceContractError::MissingField(name.to_string())),
    }
}

fn require_text(
    fields: &BTreeMap<String, InferenceValueV1>,
    name: &str,
    expected: &str,
) -> Result<(), InferenceContractError> {
    if text_field(fields, name)? == expected {
        Ok(())
    } else {
        invalid(name, "unexpected closed-enum value")
    }
}

fn require_one_of(
    fields: &BTreeMap<String, InferenceValueV1>,
    name: &str,
    expected: &[&str],
) -> Result<(), InferenceContractError> {
    if expected.contains(&text_field(fields, name)?) {
        Ok(())
    } else {
        invalid(name, "unexpected closed-enum value")
    }
}

fn invalid<T>(field: &str, reason: &'static str) -> Result<T, InferenceContractError> {
    Err(InferenceContractError::InvalidValue {
        field: field.to_string(),
        reason,
    })
}

fn canonical_payload(
    fields: &BTreeMap<String, InferenceValueV1>,
) -> Result<Vec<u8>, InferenceContractError> {
    let mut output = Vec::new();
    encode_value(&InferenceValueV1::Object(fields.clone()), &mut output)?;
    if output.len() > MAX_INFERENCE_RECORD_BYTES {
        return Err(InferenceContractError::RecordTooLarge);
    }
    Ok(output)
}

fn encode_value(
    value: &InferenceValueV1,
    output: &mut Vec<u8>,
) -> Result<(), InferenceContractError> {
    match value {
        InferenceValueV1::Unsigned(value) => encode_head(0, *value, output),
        InferenceValueV1::Text(value) => {
            validate_text(value)?;
            encode_head(3, value.len() as u64, output);
            output.extend_from_slice(value.as_bytes());
        }
        InferenceValueV1::Bytes(value) => {
            encode_head(2, value.len() as u64, output);
            output.extend_from_slice(value);
        }
        InferenceValueV1::Digest(value) => {
            encode_head(2, 32, output);
            output.extend_from_slice(value);
        }
        InferenceValueV1::Bool(value) => output.push(if *value { 0xf5 } else { 0xf4 }),
        InferenceValueV1::Array(values) => {
            encode_head(4, values.len() as u64, output);
            for value in values {
                encode_value(value, output)?;
            }
        }
        InferenceValueV1::Object(values) => {
            let mut encoded = Vec::with_capacity(values.len());
            for (key, value) in values {
                let mut key_bytes = Vec::new();
                encode_value(&InferenceValueV1::Text(key.clone()), &mut key_bytes)?;
                let mut value_bytes = Vec::new();
                encode_value(value, &mut value_bytes)?;
                encoded.push((key_bytes, value_bytes));
            }
            encoded.sort_by(|left, right| deterministic_key_cmp(&left.0, &right.0));
            encode_head(5, encoded.len() as u64, output);
            for (key, value) in encoded {
                output.extend_from_slice(&key);
                output.extend_from_slice(&value);
            }
        }
    }
    Ok(())
}

fn deterministic_key_cmp(left: &[u8], right: &[u8]) -> Ordering {
    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
}

fn encode_head(major: u8, value: u64, output: &mut Vec<u8>) {
    let prefix = major << 5;
    match value {
        0..=23 => output.push(prefix | value as u8),
        24..=0xff => output.extend_from_slice(&[prefix | 24, value as u8]),
        0x100..=0xffff => {
            output.push(prefix | 25);
            output.extend_from_slice(&(value as u16).to_be_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            output.push(prefix | 26);
            output.extend_from_slice(&(value as u32).to_be_bytes());
        }
        _ => {
            output.push(prefix | 27);
            output.extend_from_slice(&value.to_be_bytes());
        }
    }
}

struct CborDecoder<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> CborDecoder<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    fn decode_complete(mut self) -> Result<InferenceValueV1, InferenceContractError> {
        let value = self.decode_value(0)?;
        if self.cursor != self.bytes.len() {
            return Err(InferenceContractError::InvalidCbor("trailing bytes"));
        }
        Ok(value)
    }

    fn decode_value(&mut self, depth: usize) -> Result<InferenceValueV1, InferenceContractError> {
        if depth > 16 {
            return Err(InferenceContractError::InvalidCbor(
                "nesting limit exceeded",
            ));
        }
        let initial = self.take_byte()?;
        let major = initial >> 5;
        let additional = initial & 0x1f;
        match major {
            0 => Ok(InferenceValueV1::Unsigned(
                self.decode_argument(additional)?,
            )),
            2 => {
                let len = self.decode_len(additional)?;
                let bytes = self.take(len)?.to_vec();
                Ok(InferenceValueV1::Bytes(bytes))
            }
            3 => {
                let len = self.decode_len(additional)?;
                let text = std::str::from_utf8(self.take(len)?)
                    .map_err(|_| InferenceContractError::InvalidCbor("invalid UTF-8"))?
                    .to_string();
                validate_text(&text)?;
                Ok(InferenceValueV1::Text(text))
            }
            4 => {
                let len = self.decode_len(additional)?;
                let mut values = Vec::with_capacity(len);
                for _ in 0..len {
                    values.push(self.decode_value(depth + 1)?);
                }
                Ok(InferenceValueV1::Array(values))
            }
            5 => {
                let len = self.decode_len(additional)?;
                let mut values = BTreeMap::new();
                let mut prior_key: Option<Vec<u8>> = None;
                for _ in 0..len {
                    let key_start = self.cursor;
                    let key = self.decode_value(depth + 1)?;
                    let key_end = self.cursor;
                    let InferenceValueV1::Text(key) = key else {
                        return Err(InferenceContractError::InvalidCbor("map keys must be text"));
                    };
                    let encoded_key = self.bytes[key_start..key_end].to_vec();
                    if prior_key.as_ref().is_some_and(|prior| {
                        deterministic_key_cmp(prior, &encoded_key) != Ordering::Less
                    }) {
                        return Err(InferenceContractError::InvalidCbor(
                            "map keys are duplicate or not deterministic",
                        ));
                    }
                    prior_key = Some(encoded_key);
                    let value = self.decode_value(depth + 1)?;
                    if values.insert(key, value).is_some() {
                        return Err(InferenceContractError::InvalidCbor("duplicate map key"));
                    }
                }
                Ok(InferenceValueV1::Object(values))
            }
            7 if additional == 20 => Ok(InferenceValueV1::Bool(false)),
            7 if additional == 21 => Ok(InferenceValueV1::Bool(true)),
            1 | 6 => Err(InferenceContractError::InvalidCbor(
                "negative integers and tags are forbidden",
            )),
            7 => Err(InferenceContractError::InvalidCbor(
                "null, floats, and simple values are forbidden",
            )),
            _ => Err(InferenceContractError::InvalidCbor(
                "unsupported CBOR major type",
            )),
        }
    }

    fn decode_len(&mut self, additional: u8) -> Result<usize, InferenceContractError> {
        usize::try_from(self.decode_argument(additional)?)
            .map_err(|_| InferenceContractError::RecordTooLarge)
    }

    fn decode_argument(&mut self, additional: u8) -> Result<u64, InferenceContractError> {
        match additional {
            value @ 0..=23 => Ok(u64::from(value)),
            24 => {
                let value = u64::from(self.take_byte()?);
                if value < 24 {
                    return Err(InferenceContractError::InvalidCbor(
                        "non-shortest integer or length",
                    ));
                }
                Ok(value)
            }
            25 => {
                let value = u64::from(u16::from_be_bytes(self.take_array()?));
                if value <= 0xff {
                    return Err(InferenceContractError::InvalidCbor(
                        "non-shortest integer or length",
                    ));
                }
                Ok(value)
            }
            26 => {
                let value = u64::from(u32::from_be_bytes(self.take_array()?));
                if value <= 0xffff {
                    return Err(InferenceContractError::InvalidCbor(
                        "non-shortest integer or length",
                    ));
                }
                Ok(value)
            }
            27 => {
                let value = u64::from_be_bytes(self.take_array()?);
                if value <= 0xffff_ffff {
                    return Err(InferenceContractError::InvalidCbor(
                        "non-shortest integer or length",
                    ));
                }
                Ok(value)
            }
            _ => Err(InferenceContractError::InvalidCbor(
                "indefinite or reserved length",
            )),
        }
    }

    fn take_byte(&mut self) -> Result<u8, InferenceContractError> {
        let byte = *self
            .bytes
            .get(self.cursor)
            .ok_or(InferenceContractError::InvalidCbor("truncated input"))?;
        self.cursor += 1;
        Ok(byte)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], InferenceContractError> {
        let end = self
            .cursor
            .checked_add(len)
            .ok_or(InferenceContractError::RecordTooLarge)?;
        let value = self
            .bytes
            .get(self.cursor..end)
            .ok_or(InferenceContractError::InvalidCbor("truncated input"))?;
        self.cursor = end;
        Ok(value)
    }

    fn take_array<const N: usize>(&mut self) -> Result<[u8; N], InferenceContractError> {
        let mut value = [0_u8; N];
        value.copy_from_slice(self.take(N)?);
        Ok(value)
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        value.push(HEX[(byte >> 4) as usize] as char);
        value.push(HEX[(byte & 0x0f) as usize] as char);
    }
    value
}

macro_rules! f {
    ($name:literal, $kind:ident) => {
        FieldSpec {
            name: $name,
            kind: FieldKind::$kind,
            required: true,
        }
    };
    (? $name:literal, $kind:ident) => {
        FieldSpec {
            name: $name,
            kind: FieldKind::$kind,
            required: false,
        }
    };
}

fn schema(record_type: InferenceRecordTypeV1) -> &'static [FieldSpec] {
    match record_type {
        InferenceRecordTypeV1::AdmissionIntent => &[
            f!("admission_intent_id", Text),
            f!("request_id", Text),
            f!("request_digest", Digest),
            f!("request_class", Text),
            f!(? "agent_id_optional", Text),
            f!("caller_service_identity", Text),
            f!("authenticated_principal_digest", Digest),
            f!("tenant_id", Text),
            f!("project_id", Text),
            f!("work_item_id", Text),
            f!("agreement_id", Text),
            f!("customer_id", Text),
            f!("governance_receipt_id", Text),
            f!("governance_receipt_digest", Digest),
            f!("governance_generation_u64", Unsigned),
            f!("provider_id_proposal", Text),
            f!("model_id_proposal", Text),
            f!("catalog_digest_proposal", Digest),
            f!("capability_digest_proposal", Digest),
            f!("pricing_digest_proposal", Digest),
            f!(? "hierarchy_tier_optional", Text),
            f!(? "requested_max_input_tokens_optional_u64", Unsigned),
            f!("requested_max_output_tokens_u64", Unsigned),
            f!("provider_execution_deadline_proposal_unix_ms", Unsigned),
            f!("queue_policy_id", Text),
        ],
        InferenceRecordTypeV1::BudgetReservation => &[
            f!("reservation_id", Text),
            f!("admission_intent_id", Text),
            f!("admission_intent_digest", Digest),
            f!("request_id", Text),
            f!("request_digest", Digest),
            f!("governance_receipt_id", Text),
            f!("governance_receipt_digest", Digest),
            f!("governance_generation_u64", Unsigned),
            f!("provider_id", Text),
            f!("model_id", Text),
            f!("hierarchy_policy_digest", Digest),
            f!("routing_policy_generation_u64", Unsigned),
            f!("catalog_digest", Digest),
            f!("capability_digest", Digest),
            f!("pricing_digest", Digest),
            f!("effective_max_input_tokens_u64", Unsigned),
            f!("effective_max_output_tokens_u64", Unsigned),
            f!("effective_provider_execution_deadline_unix_ms", Unsigned),
            f!("scopes", Array),
            f!("reserved_microusd_u64", Unsigned),
            f!("estimated_input_microusd_u64", Unsigned),
            f!("expires_at_unix_ms", Unsigned),
        ],
        InferenceRecordTypeV1::BudgetReservationTransition => &[
            f!("transition_operation_id", Text),
            f!("reservation_id", Text),
            f!("reservation_digest", Digest),
            f!("expected_predecessor_operation_id", Text),
            f!("expected_predecessor_state", Text),
            f!("from_state", Text),
            f!("to_state", Text),
            f!("transition_reason", Text),
            f!(? "authority_evidence_digest_optional", Digest),
            f!("occurred_at_unix_ms", Unsigned),
        ],
        InferenceRecordTypeV1::BudgetExemption => &[
            f!("exemption_id", Text),
            f!("admission_intent_id", Text),
            f!("admission_intent_digest", Digest),
            f!("governance_receipt_id", Text),
            f!("governance_receipt_digest", Digest),
            f!("governance_generation_u64", Unsigned),
            f!("exemption_kind", Text),
            f!("authorized_service_identity", Text),
            f!("authorized_reason_digest", Digest),
            f!("expires_at_unix_ms", Unsigned),
        ],
        InferenceRecordTypeV1::InferenceAdmission => &[
            f!("admission_id", Text),
            f!("admission_intent_id", Text),
            f!("admission_intent_digest", Digest),
            f!("request_id", Text),
            f!("request_digest", Digest),
            f!("provider_id", Text),
            f!("model_id", Text),
            f!("hierarchy_policy_digest", Digest),
            f!("routing_policy_generation_u64", Unsigned),
            f!("catalog_digest", Digest),
            f!("capability_digest", Digest),
            f!("pricing_digest", Digest),
            f!("effective_max_input_tokens_u64", Unsigned),
            f!("effective_max_output_tokens_u64", Unsigned),
            f!("provider_execution_deadline_unix_ms", Unsigned),
            f!("queue_policy_id", Text),
            f!(? "budget_reservation_id_optional", Text),
            f!(? "budget_reservation_digest_optional", Digest),
            f!(? "budget_exemption_id_optional", Text),
            f!(? "budget_exemption_digest_optional", Digest),
            f!("finalized_at_unix_ms", Unsigned),
        ],
        InferenceRecordTypeV1::AdmissionDisposition => &[
            f!("disposition_operation_id", Text),
            f!("admission_id", Text),
            f!("admission_digest", Digest),
            f!("expected_predecessor_state", Text),
            f!("disposition", Text),
            f!("disposition_reason", Text),
            f!(? "diagnostic_digest_optional", Digest),
            f!(? "budget_reservation_id_optional", Text),
            f!(? "budget_reservation_digest_optional", Digest),
            f!(? "budget_exemption_id_optional", Text),
            f!(? "budget_exemption_digest_optional", Digest),
            f!("occurred_at_unix_ms", Unsigned),
        ],
        InferenceRecordTypeV1::InferenceAuthorityPort => &[
            f!("method", Text),
            f!("caller_service_identity", Text),
            f!("authenticated_principal_digest", Digest),
            f!("idempotency_key", Text),
            f!("record_type", Text),
            f!("record_id", Text),
            f!("record_payload_digest", Digest),
            f!(? "expected_predecessor_operation_id_optional", Text),
            f!(? "expected_predecessor_state_optional", Text),
            f!("typed_payload", Bytes),
        ],
        InferenceRecordTypeV1::ProviderDispatchReceipt => &[
            f!("dispatch_operation_id", Text),
            f!("admission_id", Text),
            f!("admission_digest", Digest),
            f!("attempt_id", Text),
            f!("attempt_binding_digest", Digest),
            f!("expected_predecessor_state", Text),
            f!("provider_id", Text),
            f!("model_id", Text),
            f!("catalog_digest", Digest),
            f!("capability_digest", Digest),
            f!(? "budget_reservation_id_optional", Text),
            f!(? "budget_reservation_digest_optional", Digest),
            f!(? "budget_exemption_id_optional", Text),
            f!(? "budget_exemption_digest_optional", Digest),
            f!(? "provider_request_id_optional", Text),
            f!("occurred_at_unix_ms", Unsigned),
        ],
        InferenceRecordTypeV1::ProviderAttemptOutcome => &[
            f!("outcome_operation_id", Text),
            f!("admission_id", Text),
            f!("admission_digest", Digest),
            f!("request_id", Text),
            f!("request_digest", Digest),
            f!("attempt_id", Text),
            f!("attempt_binding_digest", Digest),
            f!("dispatch_operation_id", Text),
            f!("expected_predecessor_state", Text),
            f!("provider_id", Text),
            f!("model_id", Text),
            f!("catalog_digest", Digest),
            f!("capability_digest", Digest),
            f!(? "budget_reservation_id_optional", Text),
            f!(? "budget_reservation_digest_optional", Digest),
            f!(? "budget_exemption_id_optional", Text),
            f!(? "budget_exemption_digest_optional", Digest),
            f!("terminal_state", Text),
            f!("terminal_reason", Text),
            f!(? "provider_request_id_optional", Text),
            f!(? "authority_evidence_digest_optional", Digest),
            f!("occurred_at_unix_ms", Unsigned),
        ],
        InferenceRecordTypeV1::UsageOutcome => &[
            f!("usage_operation_id", Text),
            f!("attempt_id", Text),
            f!("attempt_binding_digest", Digest),
            f!("terminal_outcome_operation_id", Text),
            f!("terminal_outcome_payload_digest", Digest),
            f!(? "budget_reservation_id_optional", Text),
            f!(? "budget_reservation_digest_optional", Digest),
            f!(? "budget_exemption_id_optional", Text),
            f!(? "budget_exemption_digest_optional", Digest),
            f!("input_tokens_u64", Unsigned),
            f!("output_tokens_u64", Unsigned),
            f!("cache_read_input_tokens_u64", Unsigned),
            f!("cache_creation_input_tokens_u64", Unsigned),
            f!(? "reported_cost_microusd_u64_optional", Unsigned),
            f!("resolved_cost_microusd_u64", Unsigned),
            f!("cost_source", Text),
            f!("terminal", Bool),
            f!("partial_stream", Bool),
            f!("occurred_at_unix_ms", Unsigned),
        ],
        InferenceRecordTypeV1::ProviderCapabilities => &[
            f!("provider_id", Text),
            f!("model_id", Text),
            f!("catalog_digest", Digest),
            f!("request_format_digest", Digest),
            f!("supports_streaming", Bool),
            f!("supports_usage_in_stream", Bool),
            f!("supports_structured_output", Bool),
            f!("supports_tool_use", Bool),
            f!("supports_inventory", Bool),
            f!("supports_cache_accounting", Bool),
            f!("supports_cancellation", Bool),
            f!("supports_definitive_rejection", Bool),
            f!("supports_status_reporting", Bool),
            f!("supports_retry_after", Bool),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    fn digest(value: u8) -> InferenceValueV1 {
        InferenceValueV1::Digest([value; 32])
    }

    fn capability_fields() -> BTreeMap<String, InferenceValueV1> {
        BTreeMap::from([
            (
                "provider_id".into(),
                InferenceValueV1::Text("openai".into()),
            ),
            ("model_id".into(), InferenceValueV1::Text("gpt-5".into())),
            ("catalog_digest".into(), digest(1)),
            ("request_format_digest".into(), digest(2)),
            ("supports_streaming".into(), InferenceValueV1::Bool(true)),
            (
                "supports_usage_in_stream".into(),
                InferenceValueV1::Bool(true),
            ),
            (
                "supports_structured_output".into(),
                InferenceValueV1::Bool(true),
            ),
            ("supports_tool_use".into(), InferenceValueV1::Bool(true)),
            ("supports_inventory".into(), InferenceValueV1::Bool(false)),
            (
                "supports_cache_accounting".into(),
                InferenceValueV1::Bool(true),
            ),
            ("supports_cancellation".into(), InferenceValueV1::Bool(true)),
            (
                "supports_definitive_rejection".into(),
                InferenceValueV1::Bool(true),
            ),
            (
                "supports_status_reporting".into(),
                InferenceValueV1::Bool(true),
            ),
            ("supports_retry_after".into(), InferenceValueV1::Bool(true)),
        ])
    }

    fn fixture_fields(record_type: InferenceRecordTypeV1) -> BTreeMap<String, InferenceValueV1> {
        let mut fields = BTreeMap::new();
        for (index, spec) in schema(record_type).iter().enumerate() {
            if !spec.required {
                continue;
            }
            let value = match spec.kind {
                FieldKind::Unsigned => InferenceValueV1::Unsigned(1_700_000_000_000),
                FieldKind::Text => InferenceValueV1::Text(format!("{}-v1", spec.name)),
                FieldKind::Bytes => InferenceValueV1::Bytes(vec![1, 2, 3, 4]),
                FieldKind::Digest => InferenceValueV1::Digest([(index + 1) as u8; 32]),
                FieldKind::Bool => InferenceValueV1::Bool(true),
                FieldKind::Array | FieldKind::Object => continue,
            };
            fields.insert(spec.name.to_string(), value);
        }

        match record_type {
            InferenceRecordTypeV1::BudgetReservation => {
                fields.insert(
                    "scopes".to_string(),
                    InferenceValueV1::Array(vec![InferenceValueV1::Object(BTreeMap::from([
                        (
                            "scope_kind".to_string(),
                            InferenceValueV1::Text("PROJECT".to_string()),
                        ),
                        (
                            "scope_id".to_string(),
                            InferenceValueV1::Text("project-v1".to_string()),
                        ),
                        (
                            "scope_generation_u64".to_string(),
                            InferenceValueV1::Unsigned(7),
                        ),
                        (
                            "window_kind".to_string(),
                            InferenceValueV1::Text("FIXED_RANGE".to_string()),
                        ),
                        (
                            "window_start_unix_ms".to_string(),
                            InferenceValueV1::Unsigned(1_700_000_000_000),
                        ),
                        (
                            "window_end_unix_ms_optional".to_string(),
                            InferenceValueV1::Unsigned(1_700_000_060_000),
                        ),
                    ]))]),
                );
            }
            InferenceRecordTypeV1::BudgetReservationTransition => {
                for (name, value) in [
                    ("expected_predecessor_state", "RESERVED"),
                    ("from_state", "RESERVED"),
                    ("to_state", "RECONCILED"),
                    ("transition_reason", "PROVIDER_USAGE_RECONCILED"),
                ] {
                    fields.insert(name.to_string(), InferenceValueV1::Text(value.to_string()));
                }
            }
            InferenceRecordTypeV1::BudgetExemption => {
                fields.insert(
                    "exemption_kind".to_string(),
                    InferenceValueV1::Text("NON_BILLABLE_LOCAL_LOOP".to_string()),
                );
            }
            InferenceRecordTypeV1::InferenceAdmission
            | InferenceRecordTypeV1::AdmissionDisposition
            | InferenceRecordTypeV1::ProviderDispatchReceipt
            | InferenceRecordTypeV1::ProviderAttemptOutcome
            | InferenceRecordTypeV1::UsageOutcome => {
                fields.insert(
                    "budget_exemption_id_optional".to_string(),
                    InferenceValueV1::Text("exemption-v1".to_string()),
                );
                let mut exemption_digest = [0_u8; 32];
                exemption_digest[0] = 0xee;
                fields.insert(
                    "budget_exemption_digest_optional".to_string(),
                    InferenceValueV1::Digest(exemption_digest),
                );
            }
            _ => {}
        }

        match record_type {
            InferenceRecordTypeV1::AdmissionDisposition => {
                for (name, value) in [
                    ("expected_predecessor_state", "FINAL_ADMITTED"),
                    ("disposition", "PRE_DISPATCH_REJECTED"),
                    ("disposition_reason", "AUTHORITY_DENIED"),
                ] {
                    fields.insert(name.to_string(), InferenceValueV1::Text(value.to_string()));
                }
            }
            InferenceRecordTypeV1::InferenceAuthorityPort => {
                fields.insert(
                    "method".to_string(),
                    InferenceValueV1::Text("FINALIZE_ADMISSION".to_string()),
                );
                fields.insert(
                    "record_type".to_string(),
                    InferenceValueV1::Text("InferenceAdmissionV1".to_string()),
                );
            }
            InferenceRecordTypeV1::ProviderDispatchReceipt => {
                fields.insert(
                    "expected_predecessor_state".to_string(),
                    InferenceValueV1::Text("FINAL_ADMITTED".to_string()),
                );
            }
            InferenceRecordTypeV1::ProviderAttemptOutcome => {
                for (name, value) in [
                    ("expected_predecessor_state", "DISPATCHED"),
                    ("terminal_state", "COMPLETED"),
                    ("terminal_reason", "PROVIDER_SUCCESS"),
                ] {
                    fields.insert(name.to_string(), InferenceValueV1::Text(value.to_string()));
                }
            }
            InferenceRecordTypeV1::UsageOutcome => {
                fields.insert(
                    "resolved_cost_microusd_u64".to_string(),
                    InferenceValueV1::Unsigned(0),
                );
                fields.insert(
                    "cost_source".to_string(),
                    InferenceValueV1::Text("CATALOG_COMPUTED".to_string()),
                );
                fields.insert("terminal".to_string(), InferenceValueV1::Bool(true));
            }
            _ => {}
        }
        fields
    }

    #[derive(Debug, Deserialize)]
    struct GoldenVector {
        record_type: String,
        canonical_cbor_hex: String,
        sha256: String,
    }

    #[derive(Debug, Deserialize)]
    struct ControlVector {
        kind: String,
        case: String,
        #[serde(default)]
        record_type: String,
        #[serde(default)]
        method: String,
        #[serde(default)]
        result: String,
        canonical_cbor_hex: String,
        sha256: String,
    }

    fn decode_hex(value: &str) -> Vec<u8> {
        value
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let high = (pair[0] as char).to_digit(16).unwrap() as u8;
                let low = (pair[1] as char).to_digit(16).unwrap() as u8;
                high << 4 | low
            })
            .collect()
    }

    #[test]
    fn go_and_rust_golden_vectors_are_byte_identical() {
        let vectors: Vec<GoldenVector> = serde_json::from_str(include_str!(
            "../../../schemas/inference/v1/golden-vectors.json"
        ))
        .unwrap();
        assert_eq!(vectors.len(), InferenceRecordTypeV1::ALL.len());
        for (record_type, vector) in InferenceRecordTypeV1::ALL.into_iter().zip(vectors) {
            assert_eq!(vector.record_type, record_type.as_str());
            let record = InferenceRecordV1::new(record_type, fixture_fields(record_type)).unwrap();
            assert_eq!(record.digest_hex(), vector.sha256);
            assert_eq!(
                record.canonical_wire_payload().unwrap(),
                decode_hex(&vector.canonical_cbor_hex)
            );
        }
    }

    #[test]
    fn complete_transition_and_port_vectors_are_cross_language_identical() {
        let vectors: Vec<ControlVector> = serde_json::from_str(include_str!(
            "../../../schemas/inference/v1/control-vectors.json"
        ))
        .unwrap();
        assert_eq!(vectors.len(), 27);
        let mut transitions = 0;
        let mut requests = 0;
        let mut responses = 0;
        for vector in vectors {
            let bytes = decode_hex(&vector.canonical_cbor_hex);
            match vector.kind.as_str() {
                "reservation_transition" | "port_request" => {
                    let record_type = if vector.kind == "reservation_transition" {
                        transitions += 1;
                        assert!(vector.case.contains(':'));
                        InferenceRecordTypeV1::BudgetReservationTransition
                    } else {
                        requests += 1;
                        assert_eq!(vector.case, vector.method);
                        InferenceRecordTypeV1::InferenceAuthorityPort
                    };
                    assert_eq!(vector.record_type, record_type.as_str());
                    let record = InferenceRecordV1::decode(record_type, &bytes).unwrap();
                    assert_eq!(record.digest_hex(), vector.sha256);
                }
                "port_response" => {
                    responses += 1;
                    assert_eq!(vector.case, vector.result);
                    let response =
                        InferenceAuthorityResponseV1::decode(&vector.method, &bytes).unwrap();
                    assert_eq!(response.result().as_str(), vector.result);
                    assert_eq!(response.digest_hex().unwrap(), vector.sha256);
                }
                kind => panic!("unknown control-vector kind {kind}"),
            }
        }
        assert_eq!(transitions, 11);
        assert_eq!(requests, INFERENCE_AUTHORITY_PORT_METHODS_V1.len());
        assert_eq!(responses, InferenceAuthorityResultV1::ALL.len());
    }

    #[test]
    fn authority_response_never_reauthorizes_replay_or_failure() {
        assert!(InferenceAuthorityResponseV1::new(
            "BEGIN_DISPATCH",
            InferenceAuthorityResultV1::ReplayedReadback,
            Some("operation-v1".to_string()),
            Some([0x44; 32]),
            None,
            true,
        )
        .is_err());
        assert!(InferenceAuthorityResponseV1::new(
            "FINALIZE_ADMISSION",
            InferenceAuthorityResultV1::Committed,
            Some("operation-v1".to_string()),
            Some([0x44; 32]),
            None,
            true,
        )
        .is_err());
        assert!(InferenceAuthorityResponseV1::new(
            "BEGIN_DISPATCH",
            InferenceAuthorityResultV1::Denied,
            Some("operation-v1".to_string()),
            Some([0x44; 32]),
            None,
            false,
        )
        .is_err());
    }

    #[test]
    fn canonical_capability_record_round_trips_and_detects_mutation() {
        let record = ProviderCapabilitiesV1::new(capability_fields()).unwrap();
        let bytes = record.record().canonical_wire_payload().unwrap();
        let decoded = ProviderCapabilitiesV1::decode(&bytes).unwrap();
        assert_eq!(decoded, record);

        let mut changed = bytes;
        *changed.last_mut().unwrap() ^= 1;
        assert!(ProviderCapabilitiesV1::decode(&changed).is_err());
    }

    #[test]
    fn thirty_two_byte_typed_payload_does_not_become_a_digest() {
        let mut fields = fixture_fields(InferenceRecordTypeV1::InferenceAuthorityPort);
        fields.insert(
            "typed_payload".to_string(),
            InferenceValueV1::Bytes(vec![0x42; 32]),
        );
        let record = InferenceAuthorityPortV1::new(fields).unwrap();
        let decoded =
            InferenceAuthorityPortV1::decode(&record.record().canonical_wire_payload().unwrap())
                .unwrap();
        assert!(matches!(
            decoded.record().fields().get("typed_payload"),
            Some(InferenceValueV1::Bytes(value)) if value == &vec![0x42; 32]
        ));
    }

    #[test]
    fn decoder_rejects_null_float_non_shortest_and_unknown_fields() {
        for bytes in [&[0xf6][..], &[0xfa, 0, 0, 0, 0], &[0x18, 0x01]] {
            assert!(CborDecoder::new(bytes).decode_complete().is_err());
        }
        let mut fields = capability_fields();
        fields.insert("Capability_Digest".into(), digest(7));
        assert!(matches!(
            ProviderCapabilitiesV1::new(fields),
            Err(InferenceContractError::UnknownField(_))
        ));
    }

    #[test]
    fn admission_requires_exactly_one_budget_authority() {
        let mut fields = BTreeMap::new();
        for spec in schema(InferenceRecordTypeV1::InferenceAdmission) {
            if spec.required {
                fields.insert(
                    spec.name.to_string(),
                    match spec.kind {
                        FieldKind::Unsigned => InferenceValueV1::Unsigned(1),
                        FieldKind::Digest => digest(3),
                        FieldKind::Text => InferenceValueV1::Text("x".into()),
                        _ => unreachable!(),
                    },
                );
            }
        }
        assert!(InferenceAdmissionV1::new(fields.clone()).is_err());
        fields.insert(
            "budget_reservation_id_optional".into(),
            InferenceValueV1::Text("reservation-1".into()),
        );
        fields.insert("budget_reservation_digest_optional".into(), digest(4));
        assert!(InferenceAdmissionV1::new(fields).is_ok());
    }

    #[test]
    fn terminal_reason_matrix_rejects_ambiguous_success() {
        let mut fields = BTreeMap::new();
        for spec in schema(InferenceRecordTypeV1::ProviderAttemptOutcome) {
            if spec.required {
                let value = match spec.kind {
                    FieldKind::Unsigned => InferenceValueV1::Unsigned(1),
                    FieldKind::Digest => digest(5),
                    FieldKind::Text => InferenceValueV1::Text("x".into()),
                    _ => unreachable!(),
                };
                fields.insert(spec.name.to_string(), value);
            }
        }
        fields.insert(
            "expected_predecessor_state".into(),
            InferenceValueV1::Text("DISPATCHED".into()),
        );
        fields.insert(
            "terminal_state".into(),
            InferenceValueV1::Text("AMBIGUOUS".into()),
        );
        fields.insert(
            "terminal_reason".into(),
            InferenceValueV1::Text("PROVIDER_SUCCESS".into()),
        );
        fields.insert(
            "budget_exemption_id_optional".into(),
            InferenceValueV1::Text("exemption-1".into()),
        );
        fields.insert("budget_exemption_digest_optional".into(), digest(6));
        assert!(ProviderAttemptOutcomeV1::new(fields).is_err());
    }

    #[test]
    fn schema_digest_covers_all_record_types() {
        let digest = inference_schema_digest().unwrap();
        assert_eq!(
            digest,
            "70a633b63d734fe01e6a0d546148850d405aa362f6a87ede246443f9609457db"
        );
    }

    #[test]
    fn inference_event_registry_has_a_stable_manifest() {
        let registry = inference_event_schema_registry().unwrap();
        assert_eq!(registry.manifest_digest().unwrap().len(), 64);
    }
}
