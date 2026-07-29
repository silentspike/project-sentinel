use serde::{Deserialize, Serialize};

use super::{
    digest::ContentDigest,
    error::DeliveryError,
    schema::{PrincipalV1, VersionedRefV1},
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
pub struct CandidateAuthorityQueryV1 {
    pub tenant_id: String,
    pub agreement: VersionedRefV1,
    pub project: VersionedRefV1,
    pub work_items_digest: ContentDigest,
    pub candidate_digest: ContentDigest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
        ContentDigest::of(&unsigned)
    }

    pub fn seal(mut self) -> Result<Self, DeliveryError> {
        self.snapshot_digest = self.computed_digest()?;
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkbenchEvidenceRequestV1 {
    pub tenant_id: String,
    pub project: VersionedRefV1,
    pub candidate: VersionedRefV1,
    pub qa_plan: VersionedRefV1,
    pub request_digest: ContentDigest,
}

impl WorkbenchEvidenceRequestV1 {
    pub fn computed_digest(&self) -> Result<ContentDigest, DeliveryError> {
        let mut unsigned = self.clone();
        unsigned.request_digest = ContentDigest::zero();
        ContentDigest::of(&unsigned)
    }

    pub fn seal(mut self) -> Result<Self, DeliveryError> {
        self.request_digest = self.computed_digest()?;
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkbenchEvidenceReceiptV1 {
    pub schema_version: u16,
    pub invocation: VersionedRefV1,
    pub assignment: VersionedRefV1,
    pub input_digest: ContentDigest,
    pub output_digest: ContentDigest,
    pub artifact_ownership_digest: ContentDigest,
    pub result_inventory_digest: ContentDigest,
    pub logs_digest: ContentDigest,
    pub screenshots_digest: Option<ContentDigest>,
    pub failure_classification_digest: ContentDigest,
    pub passed: bool,
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
        ContentDigest::of(&unsigned)
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

    fn execute_qa(
        &self,
        request: &WorkbenchEvidenceRequestV1,
    ) -> Result<WorkbenchEvidenceReceiptV1, DeliveryError>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationRequestV1 {
    pub operation_id: String,
    pub event_type: String,
    pub aggregate_id: String,
    pub payload_digest: ContentDigest,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationReceiptV1 {
    pub operation_id: String,
    pub event_id: String,
    pub row_identity: String,
    pub payload_digest: ContentDigest,
}

/// Adapter into the canonical #709 event/effect chain. This core owns a durable
/// outbox, not a second event store.
pub trait DeliveryPublicationPort: Send + Sync {
    fn publish(
        &self,
        request: &PublicationRequestV1,
    ) -> Result<PublicationReceiptV1, DeliveryError>;
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
