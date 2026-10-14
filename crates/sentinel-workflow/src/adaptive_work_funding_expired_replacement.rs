//! Explicit replacement proposals, not execution or provider authority.

use crate::digest::canonical_sha256;
use crate::model::{validate_digest, validate_identifier};
use crate::{
    AdaptiveFundedReviewNoIoV1, AdaptiveFundedReviewRecoveryMemberV1,
    AdaptiveLeadershipReviewCallV1, AdaptiveRecoveryReleaseV1, AdaptiveResumeSourceV1,
    AdaptiveSessionV1, AdaptiveWorkFundingLimitsV1, AdaptiveWorkFundingReceiptV1,
    AdaptiveWorkFundingRequestV1, AuthenticatedCompanyPrincipalV1, ProjectId, ProjectV1, TenantId,
    WorkflowError, WorkflowErrorCode,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

fn invalid() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::InvalidTransition,
        false,
        "expired reviewed funding replacement rejected",
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveWorkFundingExpiredReplacementSourceV1 {
    pub schema_version: u16,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub session_id: Uuid,
    pub old_funding_id: String,
    pub old_funding_receipt_digest: String,
    pub current_resume_source: AdaptiveResumeSourceV1,
    pub review_members_digest: String,
    pub prior_review_count: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveWorkFundingExpiredReplacementBindingV1 {
    pub schema_version: u16,
    pub replacement_key: String,
    pub old_funding_receipt_digest: String,
    pub source_digest: String,
    pub review_members_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveWorkFundingExpiredReplacementDraftV1 {
    pub old_funding_receipt_digest: String,
    pub operation_id: Uuid,
    pub reason_ref: String,
    pub limits: AdaptiveWorkFundingLimitsV1,
    pub release: AdaptiveRecoveryReleaseV1,
    pub proof_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveWorkFundingExpiredReplacementRequestV1 {
    pub schema_version: u16,
    pub source: AdaptiveWorkFundingExpiredReplacementSourceV1,
    pub source_digest: String,
    pub funding_request: AdaptiveWorkFundingRequestV1,
    pub proof_digest: String,
    pub release: AdaptiveRecoveryReleaseV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveWorkFundingExpiredReplacementReviewEvidenceV1 {
    pub member: AdaptiveFundedReviewRecoveryMemberV1,
    pub proof_digest: String,
    pub release: AdaptiveRecoveryReleaseV1,
    pub no_io: AdaptiveFundedReviewNoIoV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveWorkFundingExpiredReplacementEvidenceV1 {
    pub source_digest: String,
    pub proof_digest: String,
    pub per_review: Vec<AdaptiveWorkFundingExpiredReplacementReviewEvidenceV1>,
    pub verified_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveWorkFundingExpiredReplacementReceiptV1 {
    pub schema_version: u16,
    pub replacement_key: String,
    pub request: AdaptiveWorkFundingExpiredReplacementRequestV1,
    pub issuer_principal: AuthenticatedCompanyPrincipalV1,
    pub review_members: Vec<AdaptiveFundedReviewRecoveryMemberV1>,
    pub evidence: AdaptiveWorkFundingExpiredReplacementEvidenceV1,
    pub new_funding_id: String,
    pub new_funding_receipt_digest: String,
    pub issued_at_unix_ms: u64,
}

/// Server-only snapshots; never an HTTP source or a cached authority grant.
#[derive(Debug, Clone)]
pub struct AdaptiveWorkFundingExpiredReplacementVerificationSourceV1 {
    pub source: AdaptiveWorkFundingExpiredReplacementSourceV1,
    pub project: Box<ProjectV1>,
    pub session: Box<AdaptiveSessionV1>,
    pub old_funding: Box<AdaptiveWorkFundingReceiptV1>,
    pub calls: Vec<AdaptiveLeadershipReviewCallV1>,
    pub review_members: Vec<AdaptiveFundedReviewRecoveryMemberV1>,
}

pub fn adaptive_work_funding_expired_replacement_key(
    tenant: &TenantId,
    session_id: Uuid,
    old_receipt_digest: &str,
) -> Result<String, WorkflowError> {
    tenant.validate()?;
    validate_digest(old_receipt_digest)?;
    if session_id.is_nil() {
        return Err(invalid());
    }
    Ok(format!(
        "expired-funding-replacement-{}",
        canonical_sha256(
            "sentinel.workflow.expired-funding-replacement-key.v1",
            &(tenant, session_id, old_receipt_digest)
        )?
    ))
}

impl AdaptiveWorkFundingExpiredReplacementSourceV1 {
    pub fn validate(&self) -> Result<(), WorkflowError> {
        self.current_resume_source.validate()?;
        validate_identifier(&self.old_funding_id)?;
        validate_digest(&self.old_funding_receipt_digest)?;
        validate_digest(&self.review_members_digest)?;
        if self.schema_version != 1
            || self.tenant_id != self.current_resume_source.tenant_id
            || self.project_id != self.current_resume_source.project_id
            || self.session_id != self.current_resume_source.session_id
            || self.prior_review_count == 0
            || self.prior_review_count > 128
            || self.prior_review_count > self.current_resume_source.base_review_count
        {
            return Err(invalid());
        }
        Ok(())
    }
    pub fn canonical_digest(&self) -> Result<String, WorkflowError> {
        self.validate()?;
        canonical_sha256(
            "sentinel.workflow.expired-funding-replacement-source.v1",
            self,
        )
    }
    pub fn replacement_key(&self) -> Result<String, WorkflowError> {
        adaptive_work_funding_expired_replacement_key(
            &self.tenant_id,
            self.session_id,
            &self.old_funding_receipt_digest,
        )
    }
    pub fn binding(&self) -> Result<AdaptiveWorkFundingExpiredReplacementBindingV1, WorkflowError> {
        Ok(AdaptiveWorkFundingExpiredReplacementBindingV1 {
            schema_version: 1,
            replacement_key: self.replacement_key()?,
            old_funding_receipt_digest: self.old_funding_receipt_digest.clone(),
            source_digest: self.canonical_digest()?,
            review_members_digest: self.review_members_digest.clone(),
        })
    }
}

impl AdaptiveWorkFundingExpiredReplacementBindingV1 {
    pub fn validate(&self, source: &AdaptiveResumeSourceV1) -> Result<(), WorkflowError> {
        validate_digest(&self.source_digest)?;
        validate_digest(&self.review_members_digest)?;
        if self.schema_version != 1
            || self.replacement_key
                != adaptive_work_funding_expired_replacement_key(
                    &source.tenant_id,
                    source.session_id,
                    &self.old_funding_receipt_digest,
                )?
        {
            return Err(invalid());
        }
        Ok(())
    }
}

impl AdaptiveWorkFundingExpiredReplacementRequestV1 {
    pub fn validate_shape(&self) -> Result<(), WorkflowError> {
        self.source.validate()?;
        self.release.validate()?;
        validate_digest(&self.proof_digest)?;
        self.funding_request.validate_shape()?;
        if self.schema_version != 1
            || self.source_digest != self.source.canonical_digest()?
            || self.funding_request.source.resume_source != self.source.current_resume_source
            || self
                .funding_request
                .source
                .expired_reviewed_replacement
                .as_ref()
                != Some(&self.source.binding()?)
            || self
                .funding_request
                .source
                .supersedes_unused_receipt_digest
                .is_some()
        {
            return Err(invalid());
        }
        Ok(())
    }
    pub fn validate_at(
        &self,
        operator: &AuthenticatedCompanyPrincipalV1,
        now_ms: u64,
    ) -> Result<(), WorkflowError> {
        self.validate_shape()?;
        self.funding_request.validate_at(operator, now_ms)
    }
    pub fn canonical_digest(&self) -> Result<String, WorkflowError> {
        self.validate_shape()?;
        canonical_sha256(
            "sentinel.workflow.expired-funding-replacement-request.v1",
            self,
        )
    }
}
