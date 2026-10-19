//! Descriptive material offers; membership and genuine leadership adoption are separate.
use crate::adaptive_resume_policy::{AdaptiveResumeSourceV1, ADAPTIVE_RESUME_MAX_POLICY_MS};
use crate::digest::canonical_sha256;
use crate::model::validate_digest;
use crate::{
    AdaptiveWorkFundingBindingLimitsV1, AdaptiveWorkFundingLimitsV1, AdaptiveWorkFundingSourceV1,
    WorkflowError, WorkflowErrorCode,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveWorkFundingMaterialExpansionBindingV1 {
    pub schema_version: u16,
    pub slot_key: String,
    pub declined_funding_id: String,
    pub declined_receipt_digest: String,
    pub refused_review_id: Uuid,
    pub refused_call_digest: String,
    pub model_response_digest: String,
    pub usage_event_digest: String,
    pub declined_limits: AdaptiveWorkFundingBindingLimitsV1,
    pub authoritative_floor: AdaptiveWorkFundingBindingLimitsV1,
}

pub fn adaptive_material_window_slot(
    source: &AdaptiveResumeSourceV1,
) -> Result<String, WorkflowError> {
    source.validate()?;
    Ok(format!(
        "material-window-{}",
        canonical_sha256(
            "sentinel.workflow.material-window-slot.v1",
            &(
                &source.tenant_id,
                &source.project_id,
                &source.work_item_id,
                source.session_id,
                &source.head_entry_digest,
            ),
        )?
    ))
}

impl AdaptiveWorkFundingMaterialExpansionBindingV1 {
    pub fn validate(&self, source: &AdaptiveResumeSourceV1) -> Result<(), WorkflowError> {
        if self.schema_version != 1
            || self.refused_review_id.is_nil()
            || self.slot_key != adaptive_material_window_slot(source)?
        {
            return Err(invalid());
        }
        crate::model::validate_identifier(&self.declined_funding_id)?;
        for digest in [
            &self.declined_receipt_digest,
            &self.refused_call_digest,
            &self.model_response_digest,
            &self.usage_event_digest,
        ] {
            validate_digest(digest)?;
        }
        self.declined_limits.validate()?;
        self.authoritative_floor.validate()
    }

    pub fn evidence_ref(&self) -> Result<String, WorkflowError> {
        Ok(format!(
            "material-window-offer:{}",
            canonical_sha256("sentinel.workflow.material-window-binding.v1", self)?
        ))
    }
}

/// Computes descriptive floors only. Callers must authenticate the complete current set.
pub fn material_window_floor(
    authorities: &[AdaptiveWorkFundingBindingLimitsV1],
) -> Result<AdaptiveWorkFundingBindingLimitsV1, WorkflowError> {
    let mut floor = authorities.first().ok_or_else(invalid)?.clone();
    if authorities.len() > 128 {
        return Err(invalid());
    }
    for authority in authorities {
        authority.validate()?;
        if authority.max_call_duration_ms != floor.max_call_duration_ms
            || authority.dispatch_margin_ms != floor.dispatch_margin_ms
        {
            return Err(invalid());
        }
        floor.total_model_call_ceiling = floor
            .total_model_call_ceiling
            .max(authority.total_model_call_ceiling);
        floor.total_tool_call_ceiling = floor
            .total_tool_call_ceiling
            .max(authority.total_tool_call_ceiling);
        floor.total_review_ceiling = floor
            .total_review_ceiling
            .max(authority.total_review_ceiling);
        floor.total_window_ceiling = floor
            .total_window_ceiling
            .max(authority.total_window_ceiling);
        floor.max_window_ms = floor.max_window_ms.max(authority.max_window_ms);
        floor.expires_at_unix_ms = floor.expires_at_unix_ms.max(authority.expires_at_unix_ms);
    }
    floor.validate()?;
    Ok(floor)
}

/// Shape/arithmetic only, never proof of issuer, current source or native usage.
pub fn validate_material_window_limits(
    source: &AdaptiveWorkFundingSourceV1,
    limits: &AdaptiveWorkFundingLimitsV1,
    floor: &AdaptiveWorkFundingBindingLimitsV1,
    now: u64,
) -> Result<(), WorkflowError> {
    source.validate()?;
    limits.validate()?;
    floor.validate()?;
    let head = &source.resume_source;
    let windows = head
        .base_window_count
        .checked_add(limits.additional_windows)
        .ok_or_else(invalid)?;
    let reviews = head
        .base_review_count
        .checked_add(limits.additional_reviews)
        .ok_or_else(invalid)?;
    if limits.additional_model_calls != 0
        || limits.additional_tool_calls != 0
        || source.predecessor_receipt_digest.is_none()
        || source.current_model_call_ceiling != floor.total_model_call_ceiling
        || source.current_tool_call_ceiling != floor.total_tool_call_ceiling
        || source.current_model_call_ceiling <= head.base_model_calls
        || source.current_tool_call_ceiling <= head.base_tool_calls
        || windows <= floor.total_window_ceiling.max(head.base_window_count)
        || reviews < floor.total_review_ceiling
        || reviews <= head.base_review_count
        || windows > reviews
        || reviews > crate::ADAPTIVE_RESUME_MAX_REVIEWS
        || limits.max_window_ms != floor.max_window_ms
        || limits.max_call_duration_ms != floor.max_call_duration_ms
        || limits.dispatch_margin_ms != floor.dispatch_margin_ms
        || limits.expires_at_unix_ms < floor.expires_at_unix_ms
        || limits
            .expires_at_unix_ms
            .checked_sub(now)
            .is_none_or(|duration| !(1_000..=ADAPTIVE_RESUME_MAX_POLICY_MS).contains(&duration))
        || now
            .checked_add(limits.max_call_duration_ms)
            .and_then(|time| time.checked_add(limits.dispatch_margin_ms))
            .is_none_or(|time| time > limits.expires_at_unix_ms)
    {
        return Err(invalid());
    }
    Ok(())
}

/// Native proof is supplied by a trusted coordinator callback, never HTTP input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveMaterialWindowUsageProofV1 {
    pub request_id: String,
    pub request_digest: String,
    pub usage_event_digest: String,
    pub verified_at_unix_ms: u64,
}

/// Private coordinator observation, never deserialized from an HTTP request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdaptiveMaterialWindowVerificationSourceV1 {
    pub project: crate::ProjectV1,
    pub session: crate::AdaptiveSessionV1,
    pub declined: crate::AdaptiveWorkFundingReceiptV1,
    pub refused: crate::AdaptiveLeadershipReviewCallV1,
    pub resume_source: AdaptiveResumeSourceV1,
    pub floor: AdaptiveWorkFundingBindingLimitsV1,
}

fn invalid() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::InvalidInput,
        false,
        "invalid material window expansion",
    )
}
