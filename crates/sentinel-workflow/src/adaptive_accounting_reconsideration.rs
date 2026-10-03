//! A single operator-requested accounting reconsideration, never execution authority.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::digest::canonical_sha256;
use crate::model::{validate_digest, validate_identifier};
use crate::{
    AdaptiveLeadershipReviewCallV1, AdaptiveResumePolicyBindingV1, AdaptiveResumePolicyReceiptV1,
    AdaptiveSessionV1, AuthenticatedCompanyPrincipalV1, PrincipalAuthorityV1, ProjectId, ProjectV1,
    WorkflowError, WorkflowErrorCode,
};

pub const ADAPTIVE_ACCOUNTING_RECONSIDERATION_MAX_MS: u64 = 300_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveAccountingProjectionV1 {
    pub schema_version: u16,
    pub root_model_call_ceiling: u16,
    pub root_tool_call_ceiling: u16,
    pub model_calls_spent: u16,
    pub tool_calls_spent: u16,
    pub root_model_calls_remaining: u16,
    pub root_tool_calls_remaining: u16,
    pub active_window_model_call_ceiling: u16,
    pub active_window_tool_call_ceiling: u16,
    pub active_window_model_calls_remaining: u16,
    pub active_window_tool_calls_remaining: u16,
    pub active_window_deadline_ms: u64,
    pub issued_windows: u16,
    pub window_ceiling: u16,
    pub windows_remaining: u16,
    pub reviews_issued_before: u16,
    pub review_ordinal: u16,
    pub review_ceiling: u16,
    pub reviews_remaining_after_issuance: u16,
    pub policy_expires_at_unix_ms: u64,
}

impl AdaptiveAccountingProjectionV1 {
    pub fn digest(&self) -> Result<String, WorkflowError> {
        canonical_sha256("sentinel.workflow.adaptive-accounting-projection.v1", self)
    }

    pub fn evidence_ref(&self) -> Result<String, WorkflowError> {
        Ok(format!("adaptive-accounting-projection:{}", self.digest()?))
    }
}

/// Pure projection: no clock, review identity, context digest, or caller counters.
pub fn adaptive_accounting_projection(
    session: &AdaptiveSessionV1,
    binding: &AdaptiveResumePolicyBindingV1,
) -> Result<AdaptiveAccountingProjectionV1, WorkflowError> {
    session.grant.validate()?;
    binding.validate()?;
    validate_identifier(&binding.policy_id)?;
    validate_digest(&binding.receipt_digest)?;
    if binding.schema_version != 1 || binding.ordinal == 0 {
        return Err(invalid());
    }
    let active = session.effective_grant();
    let issued_windows = u16::try_from(
        session
            .continuation
            .as_ref()
            .map_or(0, |state| state.authorizations.len()),
    )
    .map_err(|_| invalid())?;
    Ok(AdaptiveAccountingProjectionV1 {
        schema_version: 1,
        root_model_call_ceiling: session.grant.max_model_calls,
        root_tool_call_ceiling: session.grant.max_tool_calls,
        model_calls_spent: session.model_calls,
        tool_calls_spent: session.tool_calls,
        root_model_calls_remaining: session
            .grant
            .max_model_calls
            .checked_sub(session.model_calls)
            .ok_or_else(invalid)?,
        root_tool_calls_remaining: session
            .grant
            .max_tool_calls
            .checked_sub(session.tool_calls)
            .ok_or_else(invalid)?,
        active_window_model_call_ceiling: active.max_model_calls,
        active_window_tool_call_ceiling: active.max_tool_calls,
        active_window_model_calls_remaining: active
            .max_model_calls
            .checked_sub(session.model_calls)
            .ok_or_else(invalid)?,
        active_window_tool_calls_remaining: active
            .max_tool_calls
            .checked_sub(session.tool_calls)
            .ok_or_else(invalid)?,
        active_window_deadline_ms: active.deadline_ms,
        issued_windows,
        window_ceiling: binding.limits.total_window_ceiling,
        windows_remaining: binding
            .limits
            .total_window_ceiling
            .checked_sub(issued_windows)
            .ok_or_else(invalid)?,
        reviews_issued_before: binding.ordinal - 1,
        review_ordinal: binding.ordinal,
        review_ceiling: binding.limits.total_review_ceiling,
        reviews_remaining_after_issuance: binding
            .limits
            .total_review_ceiling
            .checked_sub(binding.ordinal)
            .ok_or_else(invalid)?,
        policy_expires_at_unix_ms: binding.limits.expires_at_unix_ms,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveAccountingReconsiderationRequestV1 {
    pub schema_version: u16,
    pub operation_id: Uuid,
    pub project_id: ProjectId,
    pub session_id: Uuid,
    pub refused_review_id: Uuid,
    pub source_digest: String,
    pub reason_ref: String,
    pub expires_at_unix_ms: u64,
}

impl AdaptiveAccountingReconsiderationRequestV1 {
    pub fn validate_shape(&self) -> Result<(), WorkflowError> {
        self.project_id.validate()?;
        validate_digest(&self.source_digest)?;
        validate_identifier(&self.reason_ref)?;
        if self.schema_version != 1
            || self.operation_id.is_nil()
            || self.session_id.is_nil()
            || self.refused_review_id.is_nil()
            || self.expires_at_unix_ms == 0
            || self.expires_at_unix_ms > i64::MAX as u64
        {
            return Err(invalid());
        }
        Ok(())
    }

    pub fn canonical_digest(&self) -> Result<String, WorkflowError> {
        self.validate_shape()?;
        canonical_sha256(
            "sentinel.workflow.adaptive-accounting-reconsideration-request.v1",
            self,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveAccountingReconsiderationSourceV1 {
    pub schema_version: u16,
    pub source_project: ProjectV1,
    pub source_session: AdaptiveSessionV1,
    pub policy: AdaptiveResumePolicyReceiptV1,
    pub refused_review: AdaptiveLeadershipReviewCallV1,
    pub root_entry_digest: String,
    pub head_entry_digest: String,
    pub review_history_digest: String,
    pub leadership_principal: AuthenticatedCompanyPrincipalV1,
    pub leadership_authority: PrincipalAuthorityV1,
    pub next_ordinal: u16,
    pub accounting: AdaptiveAccountingProjectionV1,
    pub source_digest: String,
    pub evidence_refs: Vec<String>,
}

impl AdaptiveAccountingReconsiderationSourceV1 {
    pub fn computed_digest(&self) -> Result<String, WorkflowError> {
        canonical_sha256(
            "sentinel.workflow.adaptive-accounting-reconsideration-source.v1",
            &(
                self.schema_version,
                &self.source_project,
                &self.source_session,
                &self.policy,
                &self.refused_review,
                &self.root_entry_digest,
                &self.head_entry_digest,
                &self.review_history_digest,
                &self.leadership_principal,
                &self.leadership_authority,
                self.next_ordinal,
                &self.accounting,
            ),
        )
    }

    pub fn evidence_ref(&self) -> Result<String, WorkflowError> {
        Ok(format!(
            "adaptive-accounting-correction:{}",
            self.computed_digest()?
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveAccountingReconsiderationReceiptV1 {
    pub schema_version: u16,
    pub receipt_id: String,
    pub request: AdaptiveAccountingReconsiderationRequestV1,
    pub source: AdaptiveAccountingReconsiderationSourceV1,
    pub issuer_principal: AuthenticatedCompanyPrincipalV1,
    pub issued_at_unix_ms: u64,
    pub review_id: Uuid,
    pub review_operation_id: Uuid,
    pub allowance_id: String,
    pub grant_digest: String,
    pub context_digest: String,
}

impl AdaptiveAccountingReconsiderationReceiptV1 {
    pub fn evidence_ref(&self) -> Result<String, WorkflowError> {
        self.source.evidence_ref()
    }
}

fn invalid() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::InvalidInput,
        false,
        "invalid accounting reconsideration",
    )
}
