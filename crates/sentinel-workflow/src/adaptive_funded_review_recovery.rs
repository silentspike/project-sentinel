//! One operator-authorized, normally charged review after proven pre-I/O expiry.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::digest::canonical_sha256;
use crate::model::{validate_digest, validate_identifier};
use crate::{
    AdaptiveLeadershipReviewCallV1, AdaptiveLeadershipReviewContextV1,
    AdaptiveLeadershipReviewGrantV1, AdaptiveRecoveryReleaseV1, AdaptiveSessionV1,
    AdaptiveWorkFundingReceiptV1, AuthenticatedCompanyPrincipalV1, ProjectId, ProjectV1, TenantId,
    WorkItemId, WorkflowError, WorkflowErrorCode,
};

/// HTTP-safe source metadata. Snapshots are loaded only inside store transactions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveFundedReviewRecoverySourceV1 {
    pub schema_version: u16,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub work_item_id: WorkItemId,
    pub session_id: Uuid,
    pub expected_project_version: u64,
    pub expected_session_version: u64,
    pub project_digest: String,
    pub session_digest: String,
    pub session_head_digest: String,
    pub funding_receipt_digest: String,
    pub review_history_digest: String,
    pub review_count: u16,
    pub retired_review_id: Uuid,
    pub ordinal: u16,
}

/// Server-only snapshots for the protected verifier; never an HTTP draft/body.
#[derive(Debug)]
pub struct AdaptiveFundedReviewRecoveryVerificationSourceV1 {
    pub source: AdaptiveFundedReviewRecoverySourceV1,
    pub project: Box<ProjectV1>,
    pub session: Box<AdaptiveSessionV1>,
    pub funding: Box<AdaptiveWorkFundingReceiptV1>,
    pub calls: Vec<AdaptiveLeadershipReviewCallV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveFundedReviewRecoveryMemberV1 {
    pub review_id: Uuid,
    pub version: u64,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveFundedReviewRecoveryRequestV1 {
    pub schema_version: u16,
    pub operation_id: Uuid,
    pub source: Box<AdaptiveFundedReviewRecoverySourceV1>,
    pub source_digest: String,
    pub release: AdaptiveRecoveryReleaseV1,
    pub proof_digest: String,
    pub reason_ref: String,
    pub expires_at_unix_ms: u64,
}

/// The daemon verifies these references against protected history while holding
/// admission/mutation exclusion. Shape validation alone is never NoIO proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum AdaptiveFundedReviewNoIoV1 {
    NeverReserved {
        request_id: String,
    },
    ReleasedBeforeIo {
        request_id: String,
        request_digest: String,
        release_digest: String,
        released_at_unix_ms: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveFundedReviewRecoveryEvidenceV1 {
    pub source_digest: String,
    pub proof_digest: String,
    pub release: AdaptiveRecoveryReleaseV1,
    pub no_io: AdaptiveFundedReviewNoIoV1,
    /// Trusted daemon clock sampled after protected evidence verification.
    pub verified_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveFundedReviewRecoveryReceiptV1 {
    pub schema_version: u16,
    pub receipt_key: String,
    pub request: AdaptiveFundedReviewRecoveryRequestV1,
    pub issuer_principal: AuthenticatedCompanyPrincipalV1,
    /// Stored without the recovery binding to avoid a self-referential digest.
    pub grant_template: AdaptiveLeadershipReviewGrantV1,
    pub context: AdaptiveLeadershipReviewContextV1,
    pub review_members: Vec<AdaptiveFundedReviewRecoveryMemberV1>,
    pub evidence: AdaptiveFundedReviewRecoveryEvidenceV1,
    pub issued_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveFundedReviewRecoveryBindingV1 {
    pub schema_version: u16,
    pub receipt_key: String,
    pub receipt_digest: String,
    pub source_digest: String,
    pub funding_receipt_digest: String,
    pub review_id: Uuid,
    pub retired_review_id: Uuid,
    pub ordinal: u16,
}

fn invalid() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::InvalidTransition,
        false,
        "funded review recovery rejected",
    )
}

impl AdaptiveFundedReviewRecoverySourceV1 {
    pub fn canonical_digest(&self) -> Result<String, WorkflowError> {
        canonical_sha256("sentinel.workflow.funded-review-recovery-source.v1", self)
    }

    pub fn receipt_key(&self) -> Result<String, WorkflowError> {
        Ok(format!(
            "funded-review-recovery-{}",
            canonical_sha256(
                "sentinel.workflow.funded-review-recovery-key.v1",
                &(
                    &self.tenant_id,
                    self.session_id,
                    &self.session_head_digest,
                    &self.funding_receipt_digest
                ),
            )?
        ))
    }
}

impl AdaptiveFundedReviewRecoveryBindingV1 {
    pub fn validate_for(
        &self,
        grant: &AdaptiveLeadershipReviewGrantV1,
    ) -> Result<(), WorkflowError> {
        validate_identifier(&self.receipt_key)?;
        validate_digest(&self.receipt_digest)?;
        validate_digest(&self.source_digest)?;
        validate_digest(&self.funding_receipt_digest)?;
        let epoch = grant.work_funding.as_ref().ok_or_else(invalid)?;
        if self.schema_version != 1
            || grant.schema_version != 6
            || self.review_id.is_nil()
            || self.retired_review_id.is_nil()
            || self.review_id == self.retired_review_id
            || self.review_id != grant.review_id
            || self.ordinal != epoch.binding.ordinal
            || self.funding_receipt_digest != epoch.binding.receipt_digest
            || grant.recovery_epoch.is_some()
            || grant.resume_policy.is_some()
        {
            return Err(invalid());
        }
        Ok(())
    }
}

impl AdaptiveFundedReviewRecoveryReceiptV1 {
    pub fn receipt_digest(&self) -> Result<String, WorkflowError> {
        canonical_sha256("sentinel.workflow.funded-review-recovery-receipt.v1", self)
    }

    pub fn bound_grant(&self) -> Result<AdaptiveLeadershipReviewGrantV1, WorkflowError> {
        let mut grant = self.grant_template.clone();
        grant.schema_version = 6;
        grant.funded_review_recovery = Some(Box::new(AdaptiveFundedReviewRecoveryBindingV1 {
            schema_version: 1,
            receipt_key: self.receipt_key.clone(),
            receipt_digest: self.receipt_digest()?,
            source_digest: self.request.source_digest.clone(),
            funding_receipt_digest: self.request.source.funding_receipt_digest.clone(),
            review_id: grant.review_id,
            retired_review_id: self.request.source.retired_review_id,
            ordinal: self.request.source.ordinal,
        }));
        Ok(grant)
    }
}
