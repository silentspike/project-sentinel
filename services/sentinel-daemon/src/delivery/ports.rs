use serde::{Deserialize, Serialize};

use super::{
    digest::ContentDigest,
    error::DeliveryError,
    schema::{AuthorityRole, PrincipalV1, QaHarnessOutcome, VersionedRefV1, DELIVERY_SCHEMA_V1},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterReadiness {
    Ready {
        contract_version: u16,
        authority_generation: u64,
        contract_digest: ContentDigest,
    },
    Unavailable {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateAuthorityQueryV1 {
    pub tenant_id: String,
    pub agreement: VersionedRefV1,
    pub project: VersionedRefV1,
    pub work_items_digest: ContentDigest,
    pub candidate_digest: ContentDigest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateAuthoritySnapshotV1 {
    pub schema_version: u16,
    pub authority_generation: u64,
    pub agreement: VersionedRefV1,
    pub project: VersionedRefV1,
    pub work_items_digest: ContentDigest,
    pub current_candidate_generation: u64,
    pub current_candidate_digest: ContentDigest,
    pub participant_principals: Vec<PrincipalV1>,
    pub snapshot_digest: ContentDigest,
}

impl CandidateAuthoritySnapshotV1 {
    pub fn computed_digest(&self) -> Result<ContentDigest, DeliveryError> {
        let mut unsigned = self.clone();
        unsigned.snapshot_digest = ContentDigest::zero();
        ContentDigest::of_domain(
            "candidate-authority-snapshot",
            DELIVERY_SCHEMA_V1,
            &unsigned,
        )
    }

    pub fn seal(mut self) -> Result<Self, DeliveryError> {
        self.snapshot_digest = self.computed_digest()?;
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityValidationRequestV1 {
    pub schema_version: u16,
    pub tenant_id: String,
    pub principal_id: String,
    pub claimed_authority_generation: u64,
    pub required_role: AuthorityRole,
    pub operation: String,
    pub contract_version: u16,
    pub contract_digest: ContentDigest,
    pub request_digest: ContentDigest,
}

impl AuthorityValidationRequestV1 {
    pub fn computed_digest(&self) -> Result<ContentDigest, DeliveryError> {
        let mut unsigned = self.clone();
        unsigned.request_digest = ContentDigest::zero();
        ContentDigest::of_domain(
            "authority-validation-request",
            DELIVERY_SCHEMA_V1,
            &unsigned,
        )
    }

    pub fn seal(mut self) -> Result<Self, DeliveryError> {
        self.request_digest = self.computed_digest()?;
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityReceiptV1 {
    pub schema_version: u16,
    pub request_digest: ContentDigest,
    pub principal: PrincipalV1,
    pub contract_version: u16,
    pub contract_authority_generation: u64,
    pub contract_digest: ContentDigest,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub issuer: String,
    pub receipt_digest: ContentDigest,
}

impl AuthorityReceiptV1 {
    pub fn computed_digest(&self) -> Result<ContentDigest, DeliveryError> {
        let mut unsigned = self.clone();
        unsigned.receipt_digest = ContentDigest::zero();
        ContentDigest::of_domain("authority-receipt", DELIVERY_SCHEMA_V1, &unsigned)
    }

    pub fn seal(mut self) -> Result<Self, DeliveryError> {
        self.receipt_digest = self.computed_digest()?;
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkbenchEvidenceRequestV1 {
    pub schema_version: u16,
    pub tenant_id: String,
    pub project: VersionedRefV1,
    pub candidate: VersionedRefV1,
    pub qa_plan: VersionedRefV1,
    pub qa_run: VersionedRefV1,
    pub assigned_qa: PrincipalV1,
    pub authority_receipt_digest: ContentDigest,
    pub invocation: VersionedRefV1,
    pub request_digest: ContentDigest,
}

impl WorkbenchEvidenceRequestV1 {
    pub fn computed_digest(&self) -> Result<ContentDigest, DeliveryError> {
        let mut unsigned = self.clone();
        unsigned.request_digest = ContentDigest::zero();
        ContentDigest::of_domain("workbench-evidence-request", DELIVERY_SCHEMA_V1, &unsigned)
    }

    pub fn seal(mut self) -> Result<Self, DeliveryError> {
        self.request_digest = self.computed_digest()?;
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkbenchEvidenceReceiptV1 {
    pub schema_version: u16,
    pub invocation: VersionedRefV1,
    pub assignment: VersionedRefV1,
    pub qa_run: VersionedRefV1,
    pub assigned_qa: PrincipalV1,
    pub authority_receipt_digest: ContentDigest,
    pub input_digest: ContentDigest,
    pub output_digest: ContentDigest,
    pub artifact_ownership_digest: ContentDigest,
    pub result_inventory_digest: ContentDigest,
    pub logs_digest: ContentDigest,
    pub screenshots_digest: Option<ContentDigest>,
    pub failure_classification_digest: ContentDigest,
    pub harness_outcome: QaHarnessOutcome,
    pub required_cases_complete: bool,
    pub contaminated: bool,
    pub needs_human_review: bool,
    pub flaky_unresolved: bool,
    pub cleanup_receipt: VersionedRefV1,
    pub receipt_digest: ContentDigest,
}

impl WorkbenchEvidenceReceiptV1 {
    pub fn computed_digest(&self) -> Result<ContentDigest, DeliveryError> {
        let mut unsigned = self.clone();
        unsigned.receipt_digest = ContentDigest::zero();
        ContentDigest::of_domain("workbench-evidence-receipt", DELIVERY_SCHEMA_V1, &unsigned)
    }

    pub fn seal(mut self) -> Result<Self, DeliveryError> {
        self.receipt_digest = self.computed_digest()?;
        Ok(self)
    }
}

/// Narrow, fail-closed seam implemented later by the productive #694/#695 adapter.
///
/// The dependency-independent core calls only this versioned contract. A daemon may
/// construct the core with an unavailable adapter and still start normally; every
/// command that needs workflow or workbench authority returns a typed error.
pub trait DeliveryIntegrationPort: Send + Sync {
    fn readiness(&self) -> AdapterReadiness;

    fn candidate_authority(
        &self,
        query: &CandidateAuthorityQueryV1,
    ) -> Result<CandidateAuthoritySnapshotV1, DeliveryError>;

    fn authorize(
        &self,
        request: &AuthorityValidationRequestV1,
    ) -> Result<AuthorityReceiptV1, DeliveryError>;

    fn execute_qa(
        &self,
        request: &WorkbenchEvidenceRequestV1,
    ) -> Result<WorkbenchEvidenceReceiptV1, DeliveryError>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationRequestV1 {
    pub schema_version: u16,
    pub operation_id: String,
    pub event_type: String,
    pub aggregate_id: String,
    pub row_identity: String,
    pub payload_digest: ContentDigest,
    pub payload: Vec<u8>,
    pub request_digest: ContentDigest,
}

impl PublicationRequestV1 {
    pub fn computed_digest(&self) -> Result<ContentDigest, DeliveryError> {
        let mut unsigned = self.clone();
        unsigned.request_digest = ContentDigest::zero();
        ContentDigest::of_domain("publication-request", DELIVERY_SCHEMA_V1, &unsigned)
    }

    pub fn seal(mut self) -> Result<Self, DeliveryError> {
        self.request_digest = self.computed_digest()?;
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationReceiptV1 {
    pub schema_version: u16,
    pub operation_id: String,
    pub event_id: String,
    pub aggregate_id: String,
    pub row_identity: String,
    pub payload_digest: ContentDigest,
    pub request_digest: ContentDigest,
}

/// Adapter into the canonical #733 publication chain. Productive durability and
/// receipt authority belong to the injected store/publication adapters; the
/// local redb implementation in this module's test harness is not an event SSOT.
pub trait DeliveryPublicationPort: Send + Sync {
    fn publish(
        &self,
        request: &PublicationRequestV1,
    ) -> Result<PublicationReceiptV1, DeliveryError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryEffectKind {
    Rollout,
    Rollback,
    GovernedRework,
    MemoryPublication,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryEffectRequestV1 {
    pub schema_version: u16,
    pub kind: DeliveryEffectKind,
    pub tenant_id: String,
    pub project: VersionedRefV1,
    pub candidate: Option<VersionedRefV1>,
    pub subject: VersionedRefV1,
    pub actor_authority_receipt_digest: ContentDigest,
    pub request_digest: ContentDigest,
}

impl DeliveryEffectRequestV1 {
    pub fn computed_digest(&self) -> Result<ContentDigest, DeliveryError> {
        let mut unsigned = self.clone();
        unsigned.request_digest = ContentDigest::zero();
        ContentDigest::of_domain("delivery-effect-request", DELIVERY_SCHEMA_V1, &unsigned)
    }

    pub fn seal(mut self) -> Result<Self, DeliveryError> {
        self.request_digest = self.computed_digest()?;
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryEffectReceiptV1 {
    pub schema_version: u16,
    pub kind: DeliveryEffectKind,
    pub tenant_id: String,
    pub project: VersionedRefV1,
    pub candidate: Option<VersionedRefV1>,
    pub request_digest: ContentDigest,
    pub actor_authority_receipt_digest: ContentDigest,
    pub effect_ref: VersionedRefV1,
    pub issuer: String,
    pub issued_at_ms: u64,
    pub receipt_digest: ContentDigest,
}

impl DeliveryEffectReceiptV1 {
    pub fn computed_digest(&self) -> Result<ContentDigest, DeliveryError> {
        let mut unsigned = self.clone();
        unsigned.receipt_digest = ContentDigest::zero();
        ContentDigest::of_domain("delivery-effect-receipt", DELIVERY_SCHEMA_V1, &unsigned)
    }

    pub fn seal(mut self) -> Result<Self, DeliveryError> {
        self.receipt_digest = self.computed_digest()?;
        Ok(self)
    }
}

pub trait DeliveryEffectPort: Send + Sync {
    fn apply(
        &self,
        request: &DeliveryEffectRequestV1,
    ) -> Result<DeliveryEffectReceiptV1, DeliveryError>;
}

#[derive(Clone, Debug, Default)]
pub struct UnavailableDeliveryIntegration;

impl DeliveryIntegrationPort for UnavailableDeliveryIntegration {
    fn readiness(&self) -> AdapterReadiness {
        AdapterReadiness::Unavailable {
            reason: "productive #694/#695 adapter is not provisioned".to_string(),
        }
    }

    fn candidate_authority(
        &self,
        _query: &CandidateAuthorityQueryV1,
    ) -> Result<CandidateAuthoritySnapshotV1, DeliveryError> {
        Err(DeliveryError::AdapterUnavailable {
            dependency: "workflow_authority",
            reason: "productive #695 adapter is not provisioned".to_string(),
        })
    }

    fn authorize(
        &self,
        _request: &AuthorityValidationRequestV1,
    ) -> Result<AuthorityReceiptV1, DeliveryError> {
        Err(DeliveryError::AdapterUnavailable {
            dependency: "authenticated_authority",
            reason: "productive authenticated principal adapter is not provisioned".to_string(),
        })
    }

    fn execute_qa(
        &self,
        _request: &WorkbenchEvidenceRequestV1,
    ) -> Result<WorkbenchEvidenceReceiptV1, DeliveryError> {
        Err(DeliveryError::AdapterUnavailable {
            dependency: "workbench",
            reason: "productive #694 adapter is not provisioned".to_string(),
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct UnavailableDeliveryEffects;

impl DeliveryEffectPort for UnavailableDeliveryEffects {
    fn apply(
        &self,
        request: &DeliveryEffectRequestV1,
    ) -> Result<DeliveryEffectReceiptV1, DeliveryError> {
        let dependency = match request.kind {
            DeliveryEffectKind::Rollout | DeliveryEffectKind::Rollback => "release_effects",
            DeliveryEffectKind::GovernedRework => "governed_rework",
            DeliveryEffectKind::MemoryPublication => "memory_publication",
        };
        Err(DeliveryError::AdapterUnavailable {
            dependency,
            reason: "productive effect adapter is not provisioned".to_string(),
        })
    }
}

pub fn expected_integration_contract_digest() -> ContentDigest {
    ContentDigest::of_domain(
        "integration-contract",
        DELIVERY_SCHEMA_V1,
        &"delivery-integration-v1",
    )
    .expect("constant integration contract must be canonical")
}
