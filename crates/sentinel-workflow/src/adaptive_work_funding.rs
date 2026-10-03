//! Descriptive requests and proposed-epoch leaves for finite same-session work.
//!
//! These values grant NOTHING, even after validation or digest computation.
//! Funding requires authoritative store membership, a genuine leadership
//! decision, and productive adoption. Constructing a receipt or binding here
//! only describes a proposed epoch; it does not issue funding or provide a
//! store, journal, or runtime authorization bypass.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::adaptive_resume_policy::{
    require_resume_policy_operator, AdaptiveResumePolicyLimitsV1, AdaptiveResumeSourceV1,
    AdaptiveResumeSubjectV1, ADAPTIVE_RESUME_DISPATCH_MARGIN_MS, ADAPTIVE_RESUME_MAX_POLICY_MS,
    ADAPTIVE_RESUME_MAX_REVIEWS,
};
use crate::digest::canonical_sha256;
use crate::model::{validate_digest, validate_identifier};
use crate::{
    AuthenticatedCompanyPrincipalV1, TenantId, WorkflowError, WorkflowErrorCode,
    ADAPTIVE_SESSION_MAX_CALLS,
};

/// Caller-supplied source binding; validation does not establish store membership.
/// The store must prove the predecessor and authenticate current totals while
/// preserving the separately recorded original grants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveWorkFundingSourceV1 {
    pub resume_source: AdaptiveResumeSourceV1,
    pub original_model_call_ceiling: u16,
    pub original_tool_call_ceiling: u16,
    pub current_model_call_ceiling: u16,
    pub current_tool_call_ceiling: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predecessor_receipt_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveWorkFundingLimitsV1 {
    pub additional_model_calls: u16,
    pub additional_tool_calls: u16,
    pub additional_reviews: u16,
    pub additional_windows: u16,
    pub max_window_ms: u64,
    pub max_call_duration_ms: u64,
    pub dispatch_margin_ms: u64,
    pub expires_at_unix_ms: u64,
}

/// Requested bounds only, not a funding receipt or execution authorization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveWorkFundingRequestV1 {
    pub schema_version: u16,
    pub operation_id: Uuid,
    pub source: AdaptiveWorkFundingSourceV1,
    pub limits: AdaptiveWorkFundingLimitsV1,
    pub reason_ref: String,
}

/// Descriptive totals for one funding epoch, not the resume-policy v1 wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveWorkFundingBindingLimitsV1 {
    pub total_model_call_ceiling: u16,
    pub total_tool_call_ceiling: u16,
    pub total_review_ceiling: u16,
    pub total_window_ceiling: u16,
    pub max_window_ms: u64,
    pub max_call_duration_ms: u64,
    pub dispatch_margin_ms: u64,
    pub expires_at_unix_ms: u64,
}

/// Describes an immutable proposed-epoch leaf; constructing it issues no funding.
/// Issuer fields are claims until checked by the separately persisted issuer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveWorkFundingReceiptV1 {
    pub schema_version: u16,
    pub funding_id: String,
    pub request: AdaptiveWorkFundingRequestV1,
    pub issuer_principal: AuthenticatedCompanyPrincipalV1,
    pub issued_at_unix_ms: u64,
}

/// Describes one global review ordinal in a proposed funding epoch.
/// Neither construction nor receipt matching proves membership or authorizes work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveWorkFundingBindingV1 {
    pub schema_version: u16,
    pub funding_id: String,
    pub receipt_digest: String,
    /// Global review ordinal, not an ordinal relative to this funding epoch.
    pub ordinal: u16,
    pub limits: AdaptiveWorkFundingBindingLimitsV1,
}

/// Exact immutable epoch carried by a reviewed continuation. Shape validation
/// is not membership: the store must prove issuance, review and adoption.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveWorkFundingEpochV1 {
    pub receipt: AdaptiveWorkFundingReceiptV1,
    pub binding: AdaptiveWorkFundingBindingV1,
}

impl AdaptiveWorkFundingEpochV1 {
    pub fn validate(&self) -> Result<(), WorkflowError> {
        self.receipt.validate_binding(&self.binding)
    }

    pub fn same_epoch(&self, other: &Self) -> bool {
        self.receipt == other.receipt
            && self.binding.funding_id == other.binding.funding_id
            && self.binding.receipt_digest == other.binding.receipt_digest
            && self.binding.limits == other.binding.limits
    }

    pub fn evidence_ref(&self) -> Result<String, WorkflowError> {
        self.validate()?;
        Ok(format!(
            "adaptive-work-funding:{}:{}:{}",
            self.binding.funding_id, self.binding.receipt_digest, self.binding.ordinal,
        ))
    }
}

/// Pure epoch key; deriving it does not create or prove an authoritative leaf.
pub fn adaptive_work_funding_id(
    tenant: &TenantId,
    session_id: Uuid,
    operation_id: Uuid,
) -> Result<String, WorkflowError> {
    tenant.validate()?;
    if session_id.is_nil() || operation_id.is_nil() {
        return Err(invalid());
    }
    Ok(format!(
        "work-funding-{}",
        canonical_sha256(
            "sentinel.workflow.adaptive-work-funding-id.v1",
            &(tenant, session_id, operation_id)
        )?
    ))
}

impl AdaptiveWorkFundingSourceV1 {
    pub fn validate(&self) -> Result<(), WorkflowError> {
        self.resume_source.validate()?;
        if !matches!(
            self.resume_source.subject,
            AdaptiveResumeSubjectV1::ReadyForModel { .. }
        ) || !(1..=ADAPTIVE_SESSION_MAX_CALLS).contains(&self.original_model_call_ceiling)
            || !(1..=ADAPTIVE_SESSION_MAX_CALLS).contains(&self.original_tool_call_ceiling)
            || self.current_model_call_ceiling < self.original_model_call_ceiling
            || self.current_tool_call_ceiling < self.original_tool_call_ceiling
            || self.current_model_call_ceiling > ADAPTIVE_SESSION_MAX_CALLS
            || self.current_tool_call_ceiling > ADAPTIVE_SESSION_MAX_CALLS
            || self.current_model_call_ceiling < self.resume_source.base_model_calls
            || self.current_tool_call_ceiling < self.resume_source.base_tool_calls
        {
            return Err(invalid());
        }
        let has_prior_addition = self.current_model_call_ceiling > self.original_model_call_ceiling
            || self.current_tool_call_ceiling > self.original_tool_call_ceiling;
        match &self.predecessor_receipt_digest {
            None if has_prior_addition => return Err(invalid()),
            Some(digest) => {
                validate_digest(digest)?;
                if !has_prior_addition {
                    return Err(invalid());
                }
            }
            None => {}
        }
        Ok(())
    }
}

impl AdaptiveWorkFundingLimitsV1 {
    pub fn validate(&self) -> Result<(), WorkflowError> {
        if self.additional_model_calls > ADAPTIVE_SESSION_MAX_CALLS
            || self.additional_tool_calls > ADAPTIVE_SESSION_MAX_CALLS
            || (self.additional_model_calls == 0 && self.additional_tool_calls == 0)
            || !(1..=ADAPTIVE_RESUME_MAX_REVIEWS).contains(&self.additional_reviews)
            || self.dispatch_margin_ms != ADAPTIVE_RESUME_DISPATCH_MARGIN_MS
        {
            return Err(invalid());
        }
        AdaptiveResumePolicyLimitsV1 {
            total_review_ceiling: self.additional_reviews,
            total_window_ceiling: self.additional_windows,
            max_window_ms: self.max_window_ms,
            max_call_duration_ms: self.max_call_duration_ms,
            dispatch_margin_ms: self.dispatch_margin_ms,
            expires_at_unix_ms: self.expires_at_unix_ms,
        }
        .validate()
    }
}

impl AdaptiveWorkFundingRequestV1 {
    /// Checks public request shape only, without a clock or issuer authentication.
    /// Source evidence remains untrusted until checked against authoritative state.
    pub fn validate_shape(&self) -> Result<(), WorkflowError> {
        self.source.validate()?;
        self.limits.validate()?;
        validate_identifier(&self.reason_ref)?;
        if self.schema_version != 1 || self.operation_id.is_nil() {
            return Err(invalid());
        }
        let model_ceiling = checked_call_ceiling(
            self.source.current_model_call_ceiling,
            self.limits.additional_model_calls,
        )?;
        let tool_ceiling = checked_call_ceiling(
            self.source.current_tool_call_ceiling,
            self.limits.additional_tool_calls,
        )?;
        let source = &self.source.resume_source;
        if model_ceiling <= source.base_model_calls || tool_ceiling <= source.base_tool_calls {
            return Err(invalid());
        }
        let total_reviews = source
            .base_review_count
            .checked_add(self.limits.additional_reviews)
            .ok_or_else(invalid)?;
        let total_windows = source
            .base_window_count
            .checked_add(self.limits.additional_windows)
            .ok_or_else(invalid)?;
        if total_reviews > ADAPTIVE_RESUME_MAX_REVIEWS
            || total_windows > ADAPTIVE_RESUME_MAX_REVIEWS
            || total_windows > total_reviews
        {
            return Err(invalid());
        }
        Ok(())
    }

    /// Checks caller policy and current request validity, never grants funding.
    /// The principal must come from the caller's authenticated boundary.
    pub fn validate_at(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        now_ms: u64,
    ) -> Result<(), WorkflowError> {
        self.validate_shape()?;
        require_resume_policy_operator(principal, &self.source.resume_source.tenant_id)?;
        let lifetime_ms = self.limits.expires_at_unix_ms.checked_sub(now_ms);
        let minimum_deadline_ms = now_ms
            .checked_add(self.limits.max_call_duration_ms)
            .and_then(|deadline| deadline.checked_add(self.limits.dispatch_margin_ms));
        if now_ms == 0
            || !matches!(lifetime_ms, Some(duration) if (1_000..=ADAPTIVE_RESUME_MAX_POLICY_MS).contains(&duration))
            || !matches!(minimum_deadline_ms, Some(deadline) if deadline <= self.limits.expires_at_unix_ms)
        {
            return Err(invalid());
        }
        Ok(())
    }

    /// Pure requested ceiling, not an adopted or authorized runtime allowance.
    pub fn resulting_model_call_ceiling(&self) -> Result<u16, WorkflowError> {
        self.validate_shape()?;
        checked_call_ceiling(
            self.source.current_model_call_ceiling,
            self.limits.additional_model_calls,
        )
    }

    /// Pure requested ceiling, not an adopted or authorized runtime allowance.
    pub fn resulting_tool_call_ceiling(&self) -> Result<u16, WorkflowError> {
        self.validate_shape()?;
        checked_call_ceiling(
            self.source.current_tool_call_ceiling,
            self.limits.additional_tool_calls,
        )
    }

    /// Stable shape digest, not a membership proof or authorization token.
    pub fn canonical_digest(&self) -> Result<String, WorkflowError> {
        self.validate_shape()?;
        canonical_sha256("sentinel.workflow.adaptive-work-funding-request.v1", self)
    }

    fn binding_limits(&self) -> Result<AdaptiveWorkFundingBindingLimitsV1, WorkflowError> {
        self.validate_shape()?;
        let source = &self.source.resume_source;
        Ok(AdaptiveWorkFundingBindingLimitsV1 {
            total_model_call_ceiling: self.resulting_model_call_ceiling()?,
            total_tool_call_ceiling: self.resulting_tool_call_ceiling()?,
            total_review_ceiling: source
                .base_review_count
                .checked_add(self.limits.additional_reviews)
                .ok_or_else(invalid)?,
            total_window_ceiling: source
                .base_window_count
                .checked_add(self.limits.additional_windows)
                .ok_or_else(invalid)?,
            max_window_ms: self.limits.max_window_ms,
            max_call_duration_ms: self.limits.max_call_duration_ms,
            dispatch_margin_ms: self.limits.dispatch_margin_ms,
            expires_at_unix_ms: self.limits.expires_at_unix_ms,
        })
    }
}

impl AdaptiveWorkFundingBindingLimitsV1 {
    pub fn validate(&self) -> Result<(), WorkflowError> {
        if !(1..=ADAPTIVE_SESSION_MAX_CALLS).contains(&self.total_model_call_ceiling)
            || !(1..=ADAPTIVE_SESSION_MAX_CALLS).contains(&self.total_tool_call_ceiling)
            || self.dispatch_margin_ms != ADAPTIVE_RESUME_DISPATCH_MARGIN_MS
        {
            return Err(invalid());
        }
        AdaptiveResumePolicyLimitsV1 {
            total_review_ceiling: self.total_review_ceiling,
            total_window_ceiling: self.total_window_ceiling,
            max_window_ms: self.max_window_ms,
            max_call_duration_ms: self.max_call_duration_ms,
            dispatch_margin_ms: self.dispatch_margin_ms,
            expires_at_unix_ms: self.expires_at_unix_ms,
        }
        .validate()
    }
}

impl AdaptiveWorkFundingReceiptV1 {
    /// Validates historical shape at recorded issuance, not current expiry or
    /// issuer authenticity, store membership, leadership decision, or adoption.
    pub fn validate(&self) -> Result<(), WorkflowError> {
        self.request
            .validate_at(&self.issuer_principal, self.issued_at_unix_ms)?;
        let source = &self.request.source.resume_source;
        if self.schema_version != 1
            || self.funding_id
                != adaptive_work_funding_id(
                    &source.tenant_id,
                    source.session_id,
                    self.request.operation_id,
                )?
        {
            return Err(invalid());
        }
        Ok(())
    }

    /// Immutable leaf content digest, not proof that the leaf was issued or stored.
    pub fn receipt_digest(&self) -> Result<String, WorkflowError> {
        self.validate()?;
        canonical_sha256("sentinel.workflow.adaptive-work-funding-receipt.v1", self)
    }

    /// Describes requested ceilings only; no runtime allowance is issued here.
    pub fn resulting_model_call_ceiling(&self) -> Result<u16, WorkflowError> {
        self.validate()?;
        self.request.resulting_model_call_ceiling()
    }

    /// Describes requested ceilings only; no runtime allowance is issued here.
    pub fn resulting_tool_call_ceiling(&self) -> Result<u16, WorkflowError> {
        self.validate()?;
        self.request.resulting_tool_call_ceiling()
    }

    /// Constructs a descriptive binding, never a review grant or adoption receipt.
    pub fn binding(&self, ordinal: u16) -> Result<AdaptiveWorkFundingBindingV1, WorkflowError> {
        self.validate()?;
        let binding = AdaptiveWorkFundingBindingV1 {
            schema_version: 1,
            funding_id: self.funding_id.clone(),
            receipt_digest: self.receipt_digest()?,
            ordinal,
            limits: self.request.binding_limits()?,
        };
        self.validate_binding(&binding)?;
        Ok(binding)
    }

    /// Checks exact descriptive consistency only, not authoritative membership.
    pub fn validate_binding(
        &self,
        binding: &AdaptiveWorkFundingBindingV1,
    ) -> Result<(), WorkflowError> {
        self.validate()?;
        binding.validate()?;
        if binding.funding_id != self.funding_id
            || binding.receipt_digest != self.receipt_digest()?
            || binding.limits != self.request.binding_limits()?
            || binding.ordinal <= self.request.source.resume_source.base_review_count
        {
            return Err(WorkflowError::new(
                WorkflowErrorCode::AuthorityConflict,
                false,
                "adaptive work funding binding does not match receipt",
            ));
        }
        Ok(())
    }
}

impl AdaptiveWorkFundingBindingV1 {
    /// Public shape only; use receipt matching and separate store checks as well.
    pub fn validate(&self) -> Result<(), WorkflowError> {
        validate_identifier(&self.funding_id)?;
        validate_digest(&self.receipt_digest)?;
        self.limits.validate()?;
        if self.schema_version != 1
            || self.ordinal == 0
            || self.ordinal > self.limits.total_review_ceiling
        {
            return Err(invalid());
        }
        Ok(())
    }

    pub fn canonical_digest(&self) -> Result<String, WorkflowError> {
        self.validate()?;
        canonical_sha256("sentinel.workflow.adaptive-work-funding-binding.v1", self)
    }
}

fn checked_call_ceiling(current: u16, additional: u16) -> Result<u16, WorkflowError> {
    current
        .checked_add(additional)
        .filter(|total| *total <= ADAPTIVE_SESSION_MAX_CALLS)
        .ok_or_else(invalid)
}

fn invalid() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::InvalidInput,
        false,
        "invalid adaptive work funding request",
    )
}

#[cfg(test)]
mod tests;
