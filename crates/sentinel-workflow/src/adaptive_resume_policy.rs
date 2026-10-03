//! One finite operator-issued policy; immutable leaf proofs do not grant execution authority.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::digest::canonical_sha256;
use crate::model::{validate_digest, validate_identifier};
use crate::{
    AdaptiveEffectV1, AuthenticatedCompanyPrincipalV1, CompanyPrincipalKindV1, CompanyRoleV1,
    ProjectId, RuntimeAuthoritySnapshotV1, TenantId, WorkItemId, WorkflowError, WorkflowErrorCode,
    ADAPTIVE_LEADERSHIP_MAX_DURATION_MS, ADAPTIVE_SESSION_MAX_CALLS,
};

pub const ADAPTIVE_RESUME_MAX_REVIEWS: u16 = 128;
pub const ADAPTIVE_RESUME_MAX_WINDOW_MS: u64 = 300_000;
pub const ADAPTIVE_RESUME_MAX_POLICY_MS: u64 = 24 * 60 * 60 * 1_000;
pub const ADAPTIVE_RESUME_DISPATCH_MARGIN_MS: u64 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdaptiveResumeSubjectV1 {
    ReadyForModel {
        active_allowance_digest: String,
    },
    ModelUnknown {
        effect: AdaptiveEffectV1,
        sealed_unknown_proof_digest: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveResumeSourceV1 {
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub work_item_id: WorkItemId,
    pub session_id: Uuid,
    pub expected_project_version: u64,
    pub expected_session_version: u64,
    pub project_payload_digest: String,
    pub root_entry_digest: String,
    pub head_entry_digest: String,
    pub continuation_history_digest: String,
    pub review_history_digest: String,
    pub assignee_authority: RuntimeAuthoritySnapshotV1,
    pub base_model_calls: u16,
    pub base_tool_calls: u16,
    pub base_review_count: u16,
    pub base_window_count: u16,
    pub subject: AdaptiveResumeSubjectV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveResumePolicyLimitsV1 {
    pub total_review_ceiling: u16,
    pub total_window_ceiling: u16,
    pub max_window_ms: u64,
    pub max_call_duration_ms: u64,
    pub dispatch_margin_ms: u64,
    pub expires_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveResumePolicyRequestV1 {
    pub schema_version: u16,
    pub operation_id: Uuid,
    pub source: AdaptiveResumeSourceV1,
    pub limits: AdaptiveResumePolicyLimitsV1,
    pub reason_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveResumePolicyBindingV1 {
    pub schema_version: u16,
    pub policy_id: String,
    pub receipt_digest: String,
    pub ordinal: u16,
    pub limits: AdaptiveResumePolicyLimitsV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveResumePolicyReceiptV1 {
    pub schema_version: u16,
    pub request: AdaptiveResumePolicyRequestV1,
    pub policy_id: String,
    pub issuer_principal: AuthenticatedCompanyPrincipalV1,
    pub issued_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveResumeReviewMembershipV1 {
    pub schema_version: u16,
    pub policy_id: String,
    pub receipt_digest: String,
    pub ordinal: u16,
    pub review_id: Uuid,
    pub operation_id: Uuid,
    pub grant_digest: String,
    pub context_digest: String,
}

pub fn adaptive_resume_policy_id(
    tenant: &TenantId,
    session_id: Uuid,
) -> Result<String, WorkflowError> {
    tenant.validate()?;
    if session_id.is_nil() {
        return Err(invalid());
    }
    Ok(format!(
        "resume-policy-{}",
        canonical_sha256("sentinel.workflow.adaptive-resume-policy-id.v1", &(tenant, session_id))?
    ))
}

pub fn adaptive_resume_review_membership_id(
    policy_id: &str,
    ordinal: u16,
) -> Result<String, WorkflowError> {
    validate_identifier(policy_id)?;
    if ordinal == 0 || ordinal > ADAPTIVE_RESUME_MAX_REVIEWS {
        return Err(invalid());
    }
    Ok(format!("{policy_id}-review-{ordinal:03}"))
}

pub(crate) fn require_resume_policy_operator(
    principal: &AuthenticatedCompanyPrincipalV1,
    tenant: &TenantId,
) -> Result<(), WorkflowError> {
    principal.validate()?;
    if principal.kind != CompanyPrincipalKindV1::Operator
        || !matches!(principal.role, CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead)
        || principal.tenant_id != *tenant
    {
        return Err(unauthorized());
    }
    Ok(())
}

impl AdaptiveResumeSourceV1 {
    pub fn validate(&self) -> Result<(), WorkflowError> {
        self.tenant_id.validate()?;
        self.project_id.validate()?;
        self.work_item_id.validate()?;
        self.assignee_authority.validate()?;
        if self.session_id.is_nil()
            || self.expected_project_version == 0
            || self.expected_session_version == 0
            || self.assignee_authority.tenant_id != self.tenant_id
            || self.assignee_authority.project_id != self.project_id
            || self.assignee_authority.work_item_id != self.work_item_id
            || self.base_model_calls > ADAPTIVE_SESSION_MAX_CALLS
            || self.base_tool_calls > ADAPTIVE_SESSION_MAX_CALLS
            || self.base_review_count >= ADAPTIVE_RESUME_MAX_REVIEWS
            || self.base_window_count > self.base_review_count
        {
            return Err(invalid());
        }
        for digest in [
            &self.project_payload_digest,
            &self.root_entry_digest,
            &self.head_entry_digest,
            &self.continuation_history_digest,
            &self.review_history_digest,
        ] {
            validate_digest(digest)?;
        }
        match &self.subject {
            AdaptiveResumeSubjectV1::ReadyForModel { active_allowance_digest } => {
                validate_digest(active_allowance_digest)?;
            }
            AdaptiveResumeSubjectV1::ModelUnknown { effect, sealed_unknown_proof_digest } => {
                if effect.id.is_nil() || self.base_model_calls == 0 {
                    return Err(invalid());
                }
                validate_digest(&effect.request_digest)?;
                validate_digest(sealed_unknown_proof_digest)?;
            }
        }
        Ok(())
    }
}

impl AdaptiveResumePolicyLimitsV1 {
    pub fn validate(&self) -> Result<(), WorkflowError> {
        let minimum_window_ms = self.max_call_duration_ms.checked_add(self.dispatch_margin_ms);
        if self.total_review_ceiling == 0
            || self.total_review_ceiling > ADAPTIVE_RESUME_MAX_REVIEWS
            || self.total_window_ceiling == 0
            || self.total_window_ceiling > self.total_review_ceiling
            || self.max_window_ms > ADAPTIVE_RESUME_MAX_WINDOW_MS
            || self.max_call_duration_ms < 1_000
            || self.max_call_duration_ms > ADAPTIVE_LEADERSHIP_MAX_DURATION_MS
            || self.dispatch_margin_ms < ADAPTIVE_RESUME_DISPATCH_MARGIN_MS
            || self.dispatch_margin_ms >= self.max_window_ms
            || self.expires_at_unix_ms == 0
            || self.expires_at_unix_ms > i64::MAX as u64
            || !matches!(minimum_window_ms, Some(duration) if duration <= self.max_window_ms)
        {
            return Err(invalid());
        }
        Ok(())
    }
}

impl AdaptiveResumePolicyRequestV1 {
    pub fn validate_shape(&self) -> Result<(), WorkflowError> {
        self.source.validate()?;
        self.limits.validate()?;
        validate_identifier(&self.reason_ref)?;
        let reviews = self.limits.total_review_ceiling.checked_sub(self.source.base_review_count);
        let windows = self.limits.total_window_ceiling.checked_sub(self.source.base_window_count);
        if self.schema_version != 1 || self.operation_id.is_nil()
            || !matches!((reviews, windows), (Some(reviews), Some(windows)) if windows > 0 && windows <= reviews)
        {
            return Err(invalid());
        }
        Ok(())
    }

    pub fn validate_at(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        now_ms: u64,
    ) -> Result<(), WorkflowError> {
        self.validate_shape()?;
        require_resume_policy_operator(principal, &self.source.tenant_id)?;
        if now_ms == 0 || self.limits.expires_at_unix_ms <= now_ms
            || self.limits.expires_at_unix_ms - now_ms > ADAPTIVE_RESUME_MAX_POLICY_MS
            || !matches!(now_ms.checked_add(self.limits.max_call_duration_ms)
                .and_then(|deadline| deadline.checked_add(self.limits.dispatch_margin_ms)),
                Some(deadline) if deadline <= self.limits.expires_at_unix_ms)
        {
            return Err(invalid());
        }
        Ok(())
    }

    pub fn canonical_digest(&self) -> Result<String, WorkflowError> {
        canonical_sha256("sentinel.workflow.adaptive-resume-policy-request.v1", self)
    }
}

impl AdaptiveResumePolicyBindingV1 {
    pub fn validate(&self) -> Result<(), WorkflowError> {
        validate_identifier(&self.policy_id)?;
        validate_digest(&self.receipt_digest)?;
        self.limits.validate()?;
        if self.schema_version != 1 || self.ordinal == 0
            || self.ordinal > self.limits.total_review_ceiling
        {
            return Err(invalid());
        }
        Ok(())
    }
}

impl AdaptiveResumePolicyReceiptV1 {
    pub fn validate(&self) -> Result<(), WorkflowError> {
        self.request.validate_at(&self.issuer_principal, self.issued_at_unix_ms)?;
        if self.schema_version != 1
            || self.policy_id != adaptive_resume_policy_id(&self.request.source.tenant_id, self.request.source.session_id)?
        {
            return Err(invalid());
        }
        Ok(())
    }

    pub fn receipt_digest(&self) -> Result<String, WorkflowError> {
        canonical_sha256("sentinel.workflow.adaptive-resume-policy-receipt.v1", self)
    }

    pub fn binding(&self, ordinal: u16) -> Result<AdaptiveResumePolicyBindingV1, WorkflowError> {
        self.validate()?;
        let binding = AdaptiveResumePolicyBindingV1 {
            schema_version: 1,
            policy_id: self.policy_id.clone(),
            receipt_digest: self.receipt_digest()?,
            ordinal,
            limits: self.request.limits.clone(),
        };
        self.validate_binding(&binding)?;
        Ok(binding)
    }

    pub fn validate_binding(&self, binding: &AdaptiveResumePolicyBindingV1) -> Result<(), WorkflowError> {
        self.validate()?;
        binding.validate()?;
        if binding.policy_id != self.policy_id || binding.receipt_digest != self.receipt_digest()?
            || binding.limits != self.request.limits
            || binding.ordinal <= self.request.source.base_review_count
        {
            return Err(unauthorized());
        }
        Ok(())
    }
}

impl AdaptiveResumeReviewMembershipV1 {
    pub fn validate(&self) -> Result<(), WorkflowError> {
        adaptive_resume_review_membership_id(&self.policy_id, self.ordinal)?;
        for digest in [&self.receipt_digest, &self.grant_digest, &self.context_digest] {
            validate_digest(digest)?;
        }
        if self.schema_version != 1 || self.review_id.is_nil() || self.operation_id.is_nil() {
            return Err(invalid());
        }
        Ok(())
    }

    pub fn canonical_digest(&self) -> Result<String, WorkflowError> {
        canonical_sha256("sentinel.workflow.adaptive-resume-review-membership.v1", self)
    }
}

fn invalid() -> WorkflowError {
    WorkflowError::new(WorkflowErrorCode::InvalidInput, false, "invalid adaptive resume policy")
}

fn unauthorized() -> WorkflowError {
    WorkflowError::new(WorkflowErrorCode::AuthorityConflict, false, "adaptive resume policy authority changed")
}

#[cfg(test)]
mod tests;
