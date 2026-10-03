//! One immutable operator intervention, not a model decision or replenished budget.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::digest::canonical_sha256;
use crate::model::{validate_digest, validate_identifier};
use crate::{
    AdaptiveCursorV1, AdaptiveEffectV1, AdaptiveLeadershipReviewCallV1,
    AdaptiveLeadershipReviewContextV1, AdaptiveLeadershipReviewGrantV1,
    AdaptiveLeadershipReviewSubjectV2, AuthenticatedCompanyPrincipalV1, CompanyPrincipalKindV1,
    CompanyRoleV1, PrincipalAuthorityV1, ProjectId, TenantId, WorkItemId, WorkflowError,
    WorkflowErrorCode, ADAPTIVE_LEADERSHIP_MAX_GRANT_MS, ADAPTIVE_LEADERSHIP_MAX_REVIEWS,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveRecoveryReleaseV1 {
    pub schema_version: u16,
    pub source_git_sha: String,
    pub release_manifest_digest: String,
    pub gateway_binary_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveLeadershipRecoveryBlockedSubjectV1 {
    pub reason_code: String,
    pub model_response_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveLeadershipRecoveryRequestV1 {
    pub schema_version: u16,
    pub operation_id: Uuid,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub work_item_id: WorkItemId,
    pub session_id: Uuid,
    pub expected_project_version: u64,
    pub expected_session_version: u64,
    pub session_head_digest: String,
    pub session_digest: String,
    pub project_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_effect: Option<AdaptiveEffectV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sealed_unknown_proof_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_subject: Option<AdaptiveLeadershipRecoveryBlockedSubjectV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_repair: Option<AdaptiveLeadershipAdmissionRepairV1>,
    pub prior_review_history_digest: String,
    /// Digest of a server-loaded root-attested release/deployment repair receipt.
    /// A caller-provided digest alone is not evidence that the repair is installed.
    pub repair_digest: String,
    pub release: AdaptiveRecoveryReleaseV1,
    pub reason_ref: String,
    pub expires_at_unix_ms: u64,
    pub max_additional_model_calls: u16,
    pub max_window_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveLeadershipRecoveryBindingV1 {
    pub schema_version: u16,
    pub epoch_key: String,
    pub epoch_digest: String,
    pub review_id: Uuid,
    pub max_window_ms: u64,
    pub max_additional_model_calls: u16,
}

/// Insert-only by epoch_key, including after expiry; never a renewable slot.
/// The store verifies trusted evidence and retains all three prior review receipts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveLeadershipRecoveryEpochV1 {
    pub schema_version: u16,
    pub epoch_key: String,
    pub request: AdaptiveLeadershipRecoveryRequestV1,
    pub issuer_principal: AuthenticatedCompanyPrincipalV1,
    pub issuer_authority: PrincipalAuthorityV1,
    /// The exact slot before its digest binding is attached; never renewed.
    pub review_grant: AdaptiveLeadershipReviewGrantV1,
    pub source_context: AdaptiveLeadershipReviewContextV1,
    pub review_id: Uuid,
    pub issued_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
}

/// An extraordinary review-cap exception, never a refunded ordinary review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveLeadershipAdmissionRepairV1 {
    pub schema_version: u16,
    pub source_digest: String,
    pub disposition_digest: String,
    pub failed_release: AdaptiveRecoveryReleaseV1,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveLeadershipAdmissionRepairSourceV1 {
    pub schema_version: u16,
    pub session: crate::AdaptiveSessionV1,
    pub session_head_digest: String,
    pub project: crate::ProjectV1,
    pub calls: Vec<AdaptiveLeadershipReviewCallV1>,
    pub original_extension: Box<crate::AdaptiveBudgetReviewExtensionReceiptV1>,
    pub successor_extension: Box<crate::AdaptiveBudgetReviewExtensionReceiptV1>,
    pub legacy_epoch: Box<AdaptiveLeadershipRecoveryEpochV1>,
    /// All current-head retired undispatched candidates, not a single-release attestation.
    pub qualifying_review_ids: Vec<Uuid>,
    /// Exact completed receipts verified against retained journal and allowance history.
    pub continuation_reviews: Vec<AdaptiveLeadershipReviewCallV1>,
}

/// Produced by a trusted server callback from retained evidence, not HTTP assertions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveLeadershipAdmissionRepairEvidenceV1 {
    pub source_digest: String,
    pub disposition_digest: String,
    pub repair_digest: String,
    pub release: AdaptiveRecoveryReleaseV1,
    pub failed_release: AdaptiveRecoveryReleaseV1,
    /// Nonempty exact subset covered by the failed-release admission evidence.
    pub attested_review_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveLeadershipAdmissionRepairEpochV1 {
    // Heap-backed snapshots keep receipt validation within normal thread stacks.
    pub epoch: Box<AdaptiveLeadershipRecoveryEpochV1>,
    pub source: Box<AdaptiveLeadershipAdmissionRepairSourceV1>,
    pub evidence: AdaptiveLeadershipAdmissionRepairEvidenceV1,
}

impl std::fmt::Debug for AdaptiveLeadershipAdmissionRepairSourceV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AdaptiveLeadershipAdmissionRepairSourceV1")
            .field("session_id", &self.session.grant.session_id)
            .field("session_version", &self.session.version)
            .field("review_count", &self.calls.len())
            .finish_non_exhaustive()
    }
}

pub fn adaptive_leadership_admission_repair_key(
    tenant: &TenantId,
    session_id: Uuid,
) -> Result<String, WorkflowError> {
    tenant.validate()?;
    if session_id.is_nil() {
        return Err(invalid());
    }
    Ok(format!(
        "admission-repair-{}",
        canonical_sha256(
            "sentinel.workflow.adaptive-leadership-admission-repair-key.v1",
            &(tenant, session_id),
        )?
    ))
}

pub fn adaptive_leadership_admission_repair_history_digest(
    calls: &[AdaptiveLeadershipReviewCallV1],
) -> Result<String, WorkflowError> {
    if calls.is_empty() || calls.len() > 4096 {
        return Err(invalid());
    }
    let mut ordered: Vec<_> = calls.iter().collect();
    ordered.sort_by_key(|call| call.grant.review_id);
    if ordered
        .windows(2)
        .any(|pair| pair[0].grant.review_id == pair[1].grant.review_id)
    {
        return Err(invalid());
    }
    canonical_sha256(
        "sentinel.workflow.adaptive-leadership-admission-repair-history.v1",
        &ordered,
    )
}

impl AdaptiveLeadershipAdmissionRepairSourceV1 {
    pub fn canonical_digest(&self) -> Result<String, WorkflowError> {
        canonical_sha256(
            "sentinel.workflow.adaptive-leadership-admission-repair-source.v1",
            self,
        )
    }
}

impl AdaptiveLeadershipAdmissionRepairEpochV1 {
    pub fn validate(&self) -> Result<(), WorkflowError> {
        self.epoch.validate()?;
        let request = &self.epoch.request;
        let repair = request.admission_repair.as_ref().ok_or_else(invalid)?;
        let source = &self.source;
        if self.epoch.schema_version != 2
            || source.schema_version != 1
            || source.session != self.epoch.source_context.source_session
            || source.project != self.epoch.source_context.source_project
            || source.session_head_digest != request.session_head_digest
            || source.canonical_digest()? != repair.source_digest
            || adaptive_leadership_admission_repair_history_digest(&source.calls)?
                != request.prior_review_history_digest
            || self.evidence.source_digest != repair.source_digest
            || self.evidence.disposition_digest != repair.disposition_digest
            || self.evidence.repair_digest != request.repair_digest
            || self.evidence.release != request.release
            || self.evidence.failed_release != repair.failed_release
            || self.evidence.attested_review_ids.is_empty()
            || self
                .evidence
                .attested_review_ids
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
            || self
                .evidence
                .attested_review_ids
                .iter()
                .any(|id| !source.qualifying_review_ids.contains(id))
        {
            return Err(invalid());
        }
        Ok(())
    }
}

impl AdaptiveRecoveryReleaseV1 {
    pub fn validate(&self) -> Result<(), WorkflowError> {
        if self.schema_version != 1
            || self.source_git_sha.len() != 40
            || !self
                .source_git_sha
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(invalid());
        }
        validate_digest(&self.release_manifest_digest)?;
        validate_digest(&self.gateway_binary_digest)
    }
}

pub fn adaptive_leadership_recovery_epoch_key(
    tenant: &TenantId,
    session_id: Uuid,
) -> Result<String, WorkflowError> {
    tenant.validate()?;
    if session_id.is_nil() {
        return Err(invalid());
    }
    Ok(format!(
        "recovery-{}",
        canonical_sha256(
            "sentinel.workflow.adaptive-leadership-recovery-key.v1",
            &(tenant, session_id),
        )?
    ))
}

pub fn adaptive_leadership_recovery_session_digest(
    session: &crate::AdaptiveSessionV1,
) -> Result<String, WorkflowError> {
    canonical_sha256(
        "sentinel.workflow.adaptive-leadership-recovery-session.v1",
        session,
    )
}

pub fn adaptive_leadership_recovery_project_digest(
    project: &crate::ProjectV1,
) -> Result<String, WorkflowError> {
    canonical_sha256(
        "sentinel.workflow.adaptive-leadership-recovery-project.v1",
        project,
    )
}

/// Includes full immutable prior receipts; ordering cannot mint another epoch.
pub fn adaptive_leadership_recovery_history_digest(
    calls: &[AdaptiveLeadershipReviewCallV1],
) -> Result<String, WorkflowError> {
    if calls.len() != ADAPTIVE_LEADERSHIP_MAX_REVIEWS {
        return Err(invalid());
    }
    let mut ordered: Vec<_> = calls.iter().collect();
    ordered.sort_by_key(|call| call.grant.review_id);
    if ordered
        .windows(2)
        .any(|pair| pair[0].grant.review_id == pair[1].grant.review_id)
    {
        return Err(invalid());
    }
    canonical_sha256(
        "sentinel.workflow.adaptive-leadership-recovery-history.v1",
        &ordered,
    )
}

impl AdaptiveLeadershipRecoveryRequestV1 {
    pub fn validate(
        &self,
        operator: &AuthenticatedCompanyPrincipalV1,
        issued_at_ms: u64,
    ) -> Result<(), WorkflowError> {
        operator.validate()?;
        self.tenant_id.validate()?;
        self.project_id.validate()?;
        self.work_item_id.validate()?;
        self.release.validate()?;
        validate_identifier(&self.reason_ref)?;
        for digest in [
            &self.session_head_digest,
            &self.session_digest,
            &self.project_digest,
            &self.prior_review_history_digest,
            &self.repair_digest,
        ] {
            validate_digest(digest)?;
        }
        match (
            self.schema_version,
            &self.unknown_effect,
            &self.sealed_unknown_proof_digest,
            &self.blocked_subject,
            &self.admission_repair,
        ) {
            (1, Some(effect), Some(proof), None, None) => {
                if effect.id.is_nil() {
                    return Err(invalid());
                }
                validate_digest(&effect.request_digest)?;
                validate_digest(proof)?;
            }
            (2, None, None, Some(blocked), None) => {
                if blocked.reason_code.is_empty()
                    || blocked.reason_code.len() > 64
                    || !blocked.reason_code.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                    })
                {
                    return Err(invalid());
                }
                validate_digest(&blocked.model_response_digest)?;
            }
            (3, None, None, None, Some(repair)) => {
                validate_digest(&repair.source_digest)?;
                validate_digest(&repair.disposition_digest)?;
                repair.failed_release.validate()?;
                if repair.schema_version != 1 || repair.failed_release == self.release {
                    return Err(invalid());
                }
            }
            _ => return Err(invalid()),
        }
        if operator.kind != CompanyPrincipalKindV1::Operator
            || !matches!(
                operator.role,
                CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
            )
            || operator.tenant_id != self.tenant_id
            || self.operation_id.is_nil()
            || self.session_id.is_nil()
            || self.expected_project_version == 0
            || self.expected_project_version.checked_add(1).is_none()
            || self.expected_session_version == 0
            || self.expected_session_version.checked_add(1).is_none()
            || issued_at_ms == 0
            || self
                .expires_at_unix_ms
                .checked_sub(issued_at_ms)
                .is_none_or(|window| !(1_000..=ADAPTIVE_LEADERSHIP_MAX_GRANT_MS).contains(&window))
            || !(1..=crate::ADAPTIVE_SESSION_MAX_CALLS).contains(&self.max_additional_model_calls)
            || !(1_000..=ADAPTIVE_LEADERSHIP_MAX_GRANT_MS).contains(&self.max_window_ms)
        {
            return Err(invalid());
        }
        Ok(())
    }

    fn matches_source(&self, session: &crate::AdaptiveSessionV1) -> bool {
        if self.schema_version == 3 && self.admission_repair.is_some() {
            return matches!(session.cursor, AdaptiveCursorV1::ReadyForModel);
        }
        match (&session.cursor, &self.unknown_effect, &self.blocked_subject) {
            (AdaptiveCursorV1::ModelUnknown { effect }, Some(expected), None) => {
                self.schema_version == 1 && effect == expected
            }
            (AdaptiveCursorV1::Blocked { reason_code }, None, Some(blocked)) => {
                self.schema_version == 2
                    && reason_code == &blocked.reason_code
                    && session.last_model_result_digest.as_ref()
                        == Some(&blocked.model_response_digest)
            }
            _ => false,
        }
    }

    fn matches_review_subject(&self, grant: &AdaptiveLeadershipReviewGrantV1) -> bool {
        if self.schema_version == 3 && self.admission_repair.is_some() {
            return grant.schema_version == 4
                && matches!(
                    grant.subject,
                    Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { .. })
                );
        }
        match (&grant.subject, &self.unknown_effect, &self.blocked_subject) {
            (
                Some(AdaptiveLeadershipReviewSubjectV2::UnknownModel {
                    effect,
                    sealed_unknown_proof_digest,
                }),
                Some(expected),
                None,
            ) => {
                self.schema_version == 1
                    && effect == expected
                    && self.sealed_unknown_proof_digest.as_ref()
                        == Some(sealed_unknown_proof_digest)
            }
            (
                Some(AdaptiveLeadershipReviewSubjectV2::BlockedContinuation {
                    reason_code,
                    resolution_event_id: None,
                }),
                None,
                Some(blocked),
            ) => self.schema_version == 2 && reason_code == &blocked.reason_code,
            _ => false,
        }
    }

    pub fn canonical_digest(&self) -> Result<String, WorkflowError> {
        canonical_sha256(
            "sentinel.workflow.adaptive-leadership-recovery-request.v1",
            self,
        )
    }
}

impl AdaptiveLeadershipRecoveryBindingV1 {
    pub fn validate(&self) -> Result<(), WorkflowError> {
        let key_digest = self
            .epoch_key
            .strip_prefix(match self.schema_version {
                1 => "recovery-",
                2 => "admission-repair-",
                _ => return Err(invalid()),
            })
            .ok_or_else(invalid)?;
        validate_digest(key_digest)?;
        validate_digest(&self.epoch_digest)?;
        if self.review_id.is_nil()
            || !(1_000..=ADAPTIVE_LEADERSHIP_MAX_GRANT_MS).contains(&self.max_window_ms)
            || !(1..=crate::ADAPTIVE_SESSION_MAX_CALLS).contains(&self.max_additional_model_calls)
        {
            return Err(invalid());
        }
        Ok(())
    }

    pub fn validate_for(
        &self,
        tenant: &TenantId,
        session_id: Uuid,
        review_id: Uuid,
    ) -> Result<(), WorkflowError> {
        self.validate()?;
        let expected = match self.schema_version {
            1 => adaptive_leadership_recovery_epoch_key(tenant, session_id)?,
            2 => adaptive_leadership_admission_repair_key(tenant, session_id)?,
            _ => return Err(invalid()),
        };
        if self.epoch_key != expected || self.review_id != review_id {
            return Err(invalid());
        }
        Ok(())
    }
}

impl AdaptiveLeadershipRecoveryEpochV1 {
    /// Shape and immutable snapshot validation, not live installed-release attestation.
    pub fn validate(&self) -> Result<(), WorkflowError> {
        self.request
            .validate(&self.issuer_principal, self.issued_at_unix_ms)?;
        self.issuer_authority.validate()?;
        let request = &self.request;
        let grant = &self.review_grant;
        let context = &self.source_context;
        let session = &context.source_session;
        let repair = request.admission_repair.is_some();
        let key = if repair {
            adaptive_leadership_admission_repair_key(&request.tenant_id, request.session_id)?
        } else {
            adaptive_leadership_recovery_epoch_key(&request.tenant_id, request.session_id)?
        };
        if self.schema_version != (if repair { 2 } else { 1 })
            || self.epoch_key != key
            || self.issuer_authority.principal_id != self.issuer_principal.principal_id
            || self.issuer_authority.principal_generation
                != self.issuer_principal.authority_generation
            || self.issuer_authority.authority_digest != self.issuer_principal.authority_digest
            || grant.recovery_epoch.is_some()
            || grant.schema_version != (if repair { 4 } else { 2 })
            || grant.leadership_principal.tenant_id != request.tenant_id
            || grant.project_id != request.project_id
            || grant.work_item_id != request.work_item_id
            || grant.session_id != request.session_id
            || grant.expected_project_version != request.expected_project_version
            || grant.expected_session_version != request.expected_session_version
            || self.review_id != grant.review_id
            || self.expires_at_unix_ms != request.expires_at_unix_ms
            || grant.expires_at_unix_ms != self.expires_at_unix_ms
            || self.issued_at_unix_ms < session.updated_at_ms
            || (!repair && self.issued_at_unix_ms < session.active_deadline_ms())
            || (repair
                && session.model_calls < session.active_model_ceiling()
                && self.issued_at_unix_ms < session.active_deadline_ms())
            || request.session_digest != adaptive_leadership_recovery_session_digest(session)?
            || request.project_digest
                != adaptive_leadership_recovery_project_digest(&context.source_project)?
            || !request.matches_source(session)
            || !request.matches_review_subject(grant)
            || !context
                .evidence_refs
                .contains(&format!("recovery-request:{}", request.canonical_digest()?,))
            || session
                .model_calls
                .checked_add(request.max_additional_model_calls)
                .is_none_or(|ceiling| ceiling > session.grant.max_model_calls)
            || session.continuation.as_ref().is_some_and(|continuation| {
                continuation.authorizations.len()
                    >= crate::adaptive::ADAPTIVE_CONTINUATION_MAX_WINDOWS
            })
        {
            return Err(invalid());
        }
        let mut bound = grant.clone();
        if repair {
            // Shape-only binding avoids a recursive epoch digest while requiring schema 4 authority.
            bound.recovery_epoch = Some(AdaptiveLeadershipRecoveryBindingV1 {
                schema_version: 2,
                epoch_key: self.epoch_key.clone(),
                epoch_digest: "0".repeat(64),
                review_id: self.review_id,
                max_window_ms: request.max_window_ms,
                max_additional_model_calls: request.max_additional_model_calls,
            });
        }
        bound.validate(self.issued_at_unix_ms)?;
        context.validate(&bound)
    }

    pub fn canonical_digest(&self) -> Result<String, WorkflowError> {
        self.validate()?;
        canonical_sha256(
            "sentinel.workflow.adaptive-leadership-recovery-epoch.v1",
            self,
        )
    }

    pub fn binding(&self) -> Result<AdaptiveLeadershipRecoveryBindingV1, WorkflowError> {
        Ok(AdaptiveLeadershipRecoveryBindingV1 {
            schema_version: if self.schema_version == 2 { 2 } else { 1 },
            epoch_key: self.epoch_key.clone(),
            epoch_digest: self.canonical_digest()?,
            review_id: self.review_id,
            max_window_ms: self.request.max_window_ms,
            max_additional_model_calls: self.request.max_additional_model_calls,
        })
    }

    /// Exact slot and original issuance clock; expiry never restores this slot.
    pub fn validate_review(
        &self,
        grant: &AdaptiveLeadershipReviewGrantV1,
        context: &AdaptiveLeadershipReviewContextV1,
        issued_at_ms: u64,
    ) -> Result<(), WorkflowError> {
        if grant.recovery_epoch.as_ref() != Some(&self.binding()?)
            || context != &self.source_context
            || issued_at_ms != self.issued_at_unix_ms
        {
            return Err(invalid());
        }
        let mut unbound = grant.clone();
        unbound.recovery_epoch = None;
        if unbound != self.review_grant {
            return Err(invalid());
        }
        grant.validate(issued_at_ms)?;
        context.validate(grant)
    }

    pub fn validate_history(
        &self,
        calls: &[AdaptiveLeadershipReviewCallV1],
    ) -> Result<(), WorkflowError> {
        self.validate()?;
        if self.request.admission_repair.is_some() {
            return Err(invalid());
        }
        if adaptive_leadership_recovery_history_digest(calls)?
            != self.request.prior_review_history_digest
        {
            return Err(invalid());
        }
        for call in calls {
            call.validate_recovery_history_entity()?;
            if call.schema_version != 2
                || call.grant.schema_version != 2
                || call.grant.recovery_epoch.is_some()
                || call.grant.session_id != self.request.session_id
                || call.grant.project_id != self.request.project_id
                || call.grant.work_item_id != self.request.work_item_id
                || call.grant.leadership_principal.tenant_id != self.request.tenant_id
                || call.grant.review_id == self.review_id
                || call.dispatch.is_none()
                || call
                    .retired_at_unix_ms
                    .is_none_or(|at| at > self.issued_at_unix_ms)
                || call.decision.is_some()
                || call.continuation.is_some()
                || call.model_response_digest.is_some()
                || call.resolution_event_id.is_some()
                || !self.request.matches_review_subject(&call.grant)
                || (self.request.schema_version == 2
                    && (!self.request.matches_source(&call.context.source_session)
                        || call.context.source_session != self.source_context.source_session))
            {
                return Err(invalid());
            }
        }
        Ok(())
    }

    pub fn validate_decision(
        &self,
        decision: &crate::AdaptiveLeadershipReviewDecisionV1,
    ) -> Result<(), WorkflowError> {
        self.validate()?;
        decision.validate(&self.source_context.evidence_refs)?;
        let mut grant = self.review_grant.clone();
        if self.schema_version == 2 {
            grant.recovery_epoch = Some(self.binding()?);
        }
        decision.validate_subject(&grant)?;
        if let crate::AdaptiveLeadershipReviewDecisionKindV1::Continue {
            additional_model_calls,
            window_ms,
            ..
        } = &decision.decision
        {
            if *additional_model_calls > self.request.max_additional_model_calls
                || *window_ms > self.request.max_window_ms
            {
                return Err(invalid());
            }
        }
        Ok(())
    }

    /// Inputs must come from trusted live evidence, never the request body alone.
    /// Subject proof is the sealed unknown proof or the retained blocked model digest.
    pub fn validate_live_evidence(
        &self,
        installed_release: &AdaptiveRecoveryReleaseV1,
        repair_digest: &str,
        session_head_digest: &str,
        subject_proof_digest: &str,
        now_ms: u64,
    ) -> Result<(), WorkflowError> {
        self.validate()?;
        installed_release.validate()?;
        if installed_release != &self.request.release
            || repair_digest != self.request.repair_digest
            || session_head_digest != self.request.session_head_digest
            || Some(subject_proof_digest)
                != self
                    .request
                    .sealed_unknown_proof_digest
                    .as_deref()
                    .or_else(|| {
                        self.request
                            .blocked_subject
                            .as_ref()
                            .map(|blocked| blocked.model_response_digest.as_str())
                    })
                    .or_else(|| {
                        self.request
                            .admission_repair
                            .as_ref()
                            .map(|repair| repair.source_digest.as_str())
                    })
            || now_ms < self.issued_at_unix_ms
            || now_ms >= self.expires_at_unix_ms
        {
            return Err(invalid());
        }
        Ok(())
    }
}

fn invalid() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::InvalidInput,
        false,
        "invalid adaptive leadership recovery authority",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operator() -> AuthenticatedCompanyPrincipalV1 {
        AuthenticatedCompanyPrincipalV1 {
            schema_version: 1,
            tenant_id: TenantId::parse("recovery-tenant").unwrap(),
            principal_id: "recovery-operator".into(),
            kind: CompanyPrincipalKindV1::Operator,
            role: CompanyRoleV1::ProjectManager,
            customer_id: None,
            agent_id: None,
            authority_generation: 1,
            authority_digest: "a".repeat(64),
        }
    }

    fn request() -> AdaptiveLeadershipRecoveryRequestV1 {
        AdaptiveLeadershipRecoveryRequestV1 {
            schema_version: 1,
            operation_id: Uuid::from_u128(1),
            tenant_id: operator().tenant_id,
            project_id: ProjectId::parse("recovery-project").unwrap(),
            work_item_id: WorkItemId::parse("recovery-work").unwrap(),
            session_id: Uuid::from_u128(2),
            expected_project_version: 3,
            expected_session_version: 3,
            session_head_digest: "b".repeat(64),
            session_digest: "c".repeat(64),
            project_digest: "d".repeat(64),
            unknown_effect: Some(AdaptiveEffectV1 {
                id: Uuid::from_u128(3),
                request_digest: "e".repeat(64),
            }),
            sealed_unknown_proof_digest: Some("f".repeat(64)),
            blocked_subject: None,
            admission_repair: None,
            prior_review_history_digest: "1".repeat(64),
            repair_digest: "2".repeat(64),
            release: AdaptiveRecoveryReleaseV1 {
                schema_version: 1,
                source_git_sha: "3".repeat(40),
                release_manifest_digest: "4".repeat(64),
                gateway_binary_digest: "5".repeat(64),
            },
            reason_ref: "repair:gateway-output-compatibility".into(),
            expires_at_unix_ms: 301_000,
            max_additional_model_calls: 1,
            max_window_ms: 300_000,
        }
    }

    fn grant() -> AdaptiveLeadershipReviewGrantV1 {
        let request = request();
        let mut leader = operator();
        leader.kind = CompanyPrincipalKindV1::Agent;
        leader.role = crate::CompanyRoleV1::ProjectManager;
        leader.agent_id = Some(crate::AgentId(7));
        leader.principal_id = "recovery-leader".into();
        let authority = PrincipalAuthorityV1::derive("recovery-leader", 1, &[1; 32]).unwrap();
        leader.authority_digest = authority.authority_digest.clone();
        let assignee = crate::RuntimeAuthoritySnapshotV1 {
            schema_version: 1,
            tenant_id: request.tenant_id.clone(),
            project_id: request.project_id.clone(),
            work_item_id: request.work_item_id.clone(),
            agent_id: crate::AgentId(6),
            assignment_version: 1,
            assignment_digest: "a".repeat(64),
            organization_generation: 1,
            organization_digest: "a".repeat(64),
            principal: PrincipalAuthorityV1::derive("developer", 1, &[2; 32]).unwrap(),
            profile_id: "developer-profile".into(),
            profile_generation: 1,
            profile_digest: "a".repeat(64),
            runtime_key: "developer-runtime".into(),
            runtime_generation: 1,
            runtime_digest: "a".repeat(64),
            policy_generation: 1,
            policy_digest: "a".repeat(64),
            active: true,
            capabilities: std::collections::BTreeSet::from(["inspect".into()]),
        };
        let fingerprint = "a".repeat(64);
        AdaptiveLeadershipReviewGrantV1 {
            schema_version: 2,
            review_id: crate::adaptive_leadership_review_id(
                request.session_id,
                request.expected_session_version,
                &fingerprint,
            )
            .unwrap(),
            project_id: request.project_id,
            expected_project_version: request.expected_project_version,
            work_item_id: request.work_item_id,
            session_id: request.session_id,
            expected_session_version: request.expected_session_version,
            expected_reason_code: String::new(),
            evidence_fingerprint: fingerprint,
            leadership_principal: leader,
            leadership_authority: authority,
            assignment_id: "recovery-assignment".into(),
            assignee_authority: assignee,
            provider: "codex-cli".into(),
            model: "recovery-model".into(),
            catalog_digest: "a".repeat(64),
            max_duration_ms: 120_000,
            token_policy: crate::SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,
            expires_at_unix_ms: request.expires_at_unix_ms,
            subject: Some(AdaptiveLeadershipReviewSubjectV2::UnknownModel {
                effect: request.unknown_effect.unwrap(),
                sealed_unknown_proof_digest: request.sealed_unknown_proof_digest.unwrap(),
            }),
            recovery_epoch: None,
            resume_policy: None,
            work_funding: None,
        }
    }

    #[test]
    fn unknown_request_preserves_legacy_wire_bytes_and_digest() {
        #[derive(Serialize)]
        struct LegacyUnknownRequest<'a> {
            schema_version: u16,
            operation_id: Uuid,
            tenant_id: &'a TenantId,
            project_id: &'a ProjectId,
            work_item_id: &'a WorkItemId,
            session_id: Uuid,
            expected_project_version: u64,
            expected_session_version: u64,
            session_head_digest: &'a str,
            session_digest: &'a str,
            project_digest: &'a str,
            unknown_effect: &'a AdaptiveEffectV1,
            sealed_unknown_proof_digest: &'a str,
            prior_review_history_digest: &'a str,
            repair_digest: &'a str,
            release: &'a AdaptiveRecoveryReleaseV1,
            reason_ref: &'a str,
            expires_at_unix_ms: u64,
            max_additional_model_calls: u16,
            max_window_ms: u64,
        }
        let request = request();
        let legacy = LegacyUnknownRequest {
            schema_version: request.schema_version,
            operation_id: request.operation_id,
            tenant_id: &request.tenant_id,
            project_id: &request.project_id,
            work_item_id: &request.work_item_id,
            session_id: request.session_id,
            expected_project_version: request.expected_project_version,
            expected_session_version: request.expected_session_version,
            session_head_digest: &request.session_head_digest,
            session_digest: &request.session_digest,
            project_digest: &request.project_digest,
            unknown_effect: request.unknown_effect.as_ref().unwrap(),
            sealed_unknown_proof_digest: request.sealed_unknown_proof_digest.as_deref().unwrap(),
            prior_review_history_digest: &request.prior_review_history_digest,
            repair_digest: &request.repair_digest,
            release: &request.release,
            reason_ref: &request.reason_ref,
            expires_at_unix_ms: request.expires_at_unix_ms,
            max_additional_model_calls: request.max_additional_model_calls,
            max_window_ms: request.max_window_ms,
        };
        let bytes = serde_json::to_vec(&legacy).unwrap();
        assert_eq!(bytes, serde_json::to_vec(&request).unwrap());
        assert_eq!(
            request.canonical_digest().unwrap(),
            canonical_sha256(
                "sentinel.workflow.adaptive-leadership-recovery-request.v1",
                &legacy,
            )
            .unwrap()
        );
        let decoded: AdaptiveLeadershipRecoveryRequestV1 = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded, request);
        decoded.validate(&operator(), 1_000).unwrap();
    }

    #[test]
    fn requests_require_disjoint_versioned_subjects_and_retained_blocked_digest() {
        let unknown = request();
        let mut blocked = unknown.clone();
        blocked.schema_version = 2;
        blocked.unknown_effect = None;
        blocked.sealed_unknown_proof_digest = None;
        blocked.blocked_subject = Some(AdaptiveLeadershipRecoveryBlockedSubjectV1 {
            reason_code: "model_blocked".into(),
            model_response_digest: "a".repeat(64),
        });
        blocked.validate(&operator(), 1_000).unwrap();
        let value = serde_json::to_value(&blocked).unwrap();
        assert!(value.get("unknown_effect").is_none());
        assert!(value.get("sealed_unknown_proof_digest").is_none());
        assert_eq!(
            serde_json::from_value::<AdaptiveLeadershipRecoveryRequestV1>(value.clone()).unwrap(),
            blocked
        );
        let mut missing_digest = value;
        missing_digest["blocked_subject"]
            .as_object_mut()
            .unwrap()
            .remove("model_response_digest");
        assert!(
            serde_json::from_value::<AdaptiveLeadershipRecoveryRequestV1>(missing_digest).is_err()
        );
        for mask in 0..8 {
            for version in [1, 2, 3] {
                let mut mixed = unknown.clone();
                mixed.schema_version = version;
                mixed.unknown_effect =
                    (mask & 1 != 0).then(|| unknown.unknown_effect.clone().unwrap());
                mixed.sealed_unknown_proof_digest =
                    (mask & 2 != 0).then(|| unknown.sealed_unknown_proof_digest.clone().unwrap());
                mixed.blocked_subject =
                    (mask & 4 != 0).then(|| blocked.blocked_subject.clone().unwrap());
                assert_eq!(
                    mixed.validate(&operator(), 1_000).is_ok(),
                    (version == 1 && mask == 3) || (version == 2 && mask == 4)
                );
            }
        }
        for reason in ["", "Blocked", "blocked-reason", "blocked reason"] {
            let mut invalid = blocked.clone();
            invalid.blocked_subject.as_mut().unwrap().reason_code = reason.into();
            assert!(invalid.validate(&operator(), 1_000).is_err());
        }
        blocked
            .blocked_subject
            .as_mut()
            .unwrap()
            .model_response_digest
            .clear();
        assert!(blocked.validate(&operator(), 1_000).is_err());
    }

    #[test]
    fn request_requires_exact_authenticated_operator_tenant() {
        let request = request();
        request.validate(&operator(), 1_000).unwrap();
        let mut foreign = operator();
        foreign.tenant_id = TenantId::parse("foreign-tenant").unwrap();
        assert!(request.validate(&foreign, 1_000).is_err());
        let mut agent = operator();
        agent.kind = CompanyPrincipalKindV1::Agent;
        agent.agent_id = Some(crate::AgentId(6));
        assert!(request.validate(&agent, 1_000).is_err());
    }

    #[test]
    fn request_requires_operator_project_manager_or_technical_lead() {
        let request = request();
        for role in [CompanyRoleV1::ProjectManager, CompanyRoleV1::TechnicalLead] {
            let mut principal = operator();
            principal.role = role;
            principal.validate().unwrap();
            request.validate(&principal, 1_000).unwrap();

            principal.kind = CompanyPrincipalKindV1::Agent;
            principal.agent_id = Some(crate::AgentId(6));
            principal.validate().unwrap();
            assert!(request.validate(&principal, 1_000).is_err());
        }
        for role in [
            CompanyRoleV1::Sales,
            CompanyRoleV1::Designer,
            CompanyRoleV1::Developer,
            CompanyRoleV1::Qa,
            CompanyRoleV1::ReleaseManager,
            CompanyRoleV1::Gaia,
        ] {
            let mut principal = operator();
            principal.role = role;
            principal.validate().unwrap();
            assert!(request.validate(&principal, 1_000).is_err());
        }
        let mut customer = operator();
        customer.kind = CompanyPrincipalKindV1::Customer;
        customer.role = CompanyRoleV1::Customer;
        customer.customer_id = Some("recovery-customer".into());
        customer.validate().unwrap();
        assert!(request.validate(&customer, 1_000).is_err());
    }

    #[test]
    fn request_caps_and_fixed_clock_fail_closed() {
        let original = request();
        for expiry in [0, 999, 1_000, 1_999, 301_001, u64::MAX] {
            let mut changed = original.clone();
            changed.expires_at_unix_ms = expiry;
            assert!(changed.validate(&operator(), 1_000).is_err());
        }
        for calls in [0, crate::ADAPTIVE_SESSION_MAX_CALLS + 1, u16::MAX] {
            let mut changed = original.clone();
            changed.max_additional_model_calls = calls;
            assert!(changed.validate(&operator(), 1_000).is_err());
        }
        for window in [0, 999, 300_001, u64::MAX] {
            let mut changed = original.clone();
            changed.max_window_ms = window;
            assert!(changed.validate(&operator(), 1_000).is_err());
        }
        assert!(original.validate(&operator(), 0).is_err());
        assert!(original
            .validate(&operator(), original.expires_at_unix_ms)
            .is_err());
    }

    #[test]
    fn release_requires_full_canonical_source_and_digest_shape() {
        let original = request().release;
        original.validate().unwrap();
        for sha in ["953060b9".to_owned(), "A".repeat(40), "g".repeat(40)] {
            let mut changed = original.clone();
            changed.source_git_sha = sha;
            assert!(changed.validate().is_err());
        }
        let mut changed = original;
        changed.gateway_binary_digest = "not-attested".into();
        assert!(changed.validate().is_err());
    }

    #[test]
    fn admission_repair_request_is_disjoint_and_preserves_absent_legacy_wire_field() {
        let legacy = request();
        let value = serde_json::to_value(&legacy).unwrap();
        assert!(value.get("admission_repair").is_none());
        assert_eq!(
            serde_json::from_value::<AdaptiveLeadershipRecoveryRequestV1>(value).unwrap(),
            legacy
        );
        let mut repair = legacy.clone();
        repair.schema_version = 3;
        repair.unknown_effect = None;
        repair.sealed_unknown_proof_digest = None;
        repair.admission_repair = Some(AdaptiveLeadershipAdmissionRepairV1 {
            schema_version: 1,
            source_digest: "a".repeat(64),
            disposition_digest: "b".repeat(64),
            failed_release: legacy.release.clone(),
        });
        repair.release.source_git_sha = "c".repeat(40);
        repair.validate(&operator(), 1_000).unwrap();
        for schema in [1, 2] {
            let mut invalid = repair.clone();
            invalid.schema_version = schema;
            assert!(invalid.validate(&operator(), 1_000).is_err());
        }
        let mut invalid = repair.clone();
        invalid.admission_repair = None;
        assert!(invalid.validate(&operator(), 1_000).is_err());
        invalid = repair.clone();
        invalid.unknown_effect = legacy.unknown_effect;
        assert!(invalid.validate(&operator(), 1_000).is_err());
        invalid = repair.clone();
        invalid.release = repair.admission_repair.unwrap().failed_release;
        assert!(invalid.validate(&operator(), 1_000).is_err());
    }

    #[test]
    fn admission_repair_key_and_binding_cannot_reuse_legacy_or_foreign_session_slot() {
        let request = request();
        let key = adaptive_leadership_admission_repair_key(&request.tenant_id, request.session_id)
            .unwrap();
        assert_ne!(
            key,
            adaptive_leadership_recovery_epoch_key(&request.tenant_id, request.session_id).unwrap()
        );
        let binding = AdaptiveLeadershipRecoveryBindingV1 {
            schema_version: 2,
            epoch_key: key,
            epoch_digest: "a".repeat(64),
            review_id: Uuid::from_u128(7),
            max_window_ms: 1_000,
            max_additional_model_calls: 1,
        };
        binding
            .validate_for(&request.tenant_id, request.session_id, binding.review_id)
            .unwrap();
        assert!(binding
            .validate_for(&request.tenant_id, Uuid::from_u128(8), binding.review_id)
            .is_err());
        let mut wrong = binding;
        wrong.epoch_key =
            adaptive_leadership_recovery_epoch_key(&request.tenant_id, request.session_id).unwrap();
        assert!(wrong.validate().is_err());
    }

    #[test]
    fn recovery_key_is_tenant_session_scoped_and_binding_names_exact_slot() {
        let request = request();
        let key =
            adaptive_leadership_recovery_epoch_key(&request.tenant_id, request.session_id).unwrap();
        let binding = AdaptiveLeadershipRecoveryBindingV1 {
            schema_version: 1,
            epoch_key: key.clone(),
            epoch_digest: "a".repeat(64),
            review_id: Uuid::from_u128(4),
            max_window_ms: 300_000,
            max_additional_model_calls: 1,
        };
        binding
            .validate_for(&request.tenant_id, request.session_id, binding.review_id)
            .unwrap();
        for window in [0, 999, 300_001, u64::MAX] {
            let mut invalid = binding.clone();
            invalid.max_window_ms = window;
            assert!(invalid.validate().is_err());
        }
        assert!(binding
            .validate_for(&request.tenant_id, request.session_id, Uuid::from_u128(5))
            .is_err());
        assert_ne!(
            key,
            adaptive_leadership_recovery_epoch_key(
                &TenantId::parse("foreign-tenant").unwrap(),
                request.session_id,
            )
            .unwrap()
        );
        assert_ne!(
            key,
            adaptive_leadership_recovery_epoch_key(&request.tenant_id, Uuid::from_u128(5)).unwrap()
        );
        let mut malformed = binding;
        malformed.epoch_key.push('a');
        assert!(malformed.validate().is_err());
    }

    #[test]
    fn request_digest_binds_operation_repair_release_and_unknown_effect() {
        let original = request();
        let digest = original.canonical_digest().unwrap();
        let mut changed = original.clone();
        changed.operation_id = Uuid::from_u128(10);
        assert_ne!(digest, changed.canonical_digest().unwrap());
        changed = original.clone();
        changed.repair_digest = "9".repeat(64);
        assert_ne!(digest, changed.canonical_digest().unwrap());
        changed = original.clone();
        changed.release.gateway_binary_digest = "9".repeat(64);
        assert_ne!(digest, changed.canonical_digest().unwrap());
        changed = original;
        changed.unknown_effect.as_mut().unwrap().id = Uuid::from_u128(10);
        assert_ne!(digest, changed.canonical_digest().unwrap());
    }

    #[test]
    fn request_rejects_unknown_fields_and_insufficient_history() {
        let mut value = serde_json::to_value(request()).unwrap();
        value["restore_slot"] = serde_json::json!(true);
        assert!(serde_json::from_value::<AdaptiveLeadershipRecoveryRequestV1>(value).is_err());
        assert!(adaptive_leadership_recovery_history_digest(&[]).is_err());
    }

    #[test]
    fn absent_binding_preserves_historical_grant_serialization() {
        for version in [1, 2] {
            let mut grant = grant();
            grant.schema_version = version;
            if version == 1 {
                grant.subject = None;
                grant.expected_reason_code = "blocked".into();
            }
            grant.validate(1_000).unwrap();
            let before = serde_json::to_vec(&grant).unwrap();
            let value: serde_json::Value = serde_json::from_slice(&before).unwrap();
            assert!(value.get("recovery_epoch").is_none());
            let historical: AdaptiveLeadershipReviewGrantV1 =
                serde_json::from_slice(&before).unwrap();
            assert!(historical.recovery_epoch.is_none());
            assert_eq!(before, serde_json::to_vec(&historical).unwrap());
        }
    }

    #[test]
    fn binding_requires_exact_recovery_subject_and_bounded_decision() {
        let mut grant = grant();
        grant.recovery_epoch = Some(AdaptiveLeadershipRecoveryBindingV1 {
            schema_version: 1,
            epoch_key: adaptive_leadership_recovery_epoch_key(
                &grant.leadership_principal.tenant_id,
                grant.session_id,
            )
            .unwrap(),
            epoch_digest: "a".repeat(64),
            review_id: grant.review_id,
            max_window_ms: 300_000,
            max_additional_model_calls: 1,
        });
        grant.validate(1_000).unwrap();
        for calls in [1, 2] {
            let decision = crate::AdaptiveLeadershipReviewDecisionV1 {
                schema_version: 2,
                decision: crate::AdaptiveLeadershipReviewDecisionKindV1::Continue {
                    additional_model_calls: calls,
                    window_ms: 1_000,
                    rationale: "A bounded continuation requires a fresh observation.".into(),
                    evidence_refs: vec!["verified-proof".into()],
                },
            };
            assert_eq!(decision.validate_subject(&grant).is_ok(), calls == 1);
        }
        grant.subject = Some(AdaptiveLeadershipReviewSubjectV2::BlockedContinuation {
            reason_code: "blocked".into(),
            resolution_event_id: None,
        });
        grant.expected_reason_code = "blocked".into();
        grant.validate(1_000).unwrap();
        grant.subject = Some(AdaptiveLeadershipReviewSubjectV2::BlockedContinuation {
            reason_code: "blocked".into(),
            resolution_event_id: Some(Uuid::from_u128(11).to_string()),
        });
        assert!(grant.validate(1_000).is_err());
        grant.schema_version = 1;
        grant.subject = None;
        assert!(grant.validate(1_000).is_err());
    }

    #[test]
    fn canonical_decision_references_still_reject_duplicates_and_foreign_identity() {
        let mut decision = crate::AdaptiveLeadershipReviewDecisionV1 {
            schema_version: 2,
            decision: crate::AdaptiveLeadershipReviewDecisionKindV1::KeepUnknown {
                rationale: "The historical effect remains unknown.".into(),
                evidence_refs: vec!["verified-proof".into(), "verified-proof".into()],
            },
        };
        let supplied = vec!["verified-proof".into()];
        assert!(decision.validate(&supplied).is_err());
        decision.decision = crate::AdaptiveLeadershipReviewDecisionKindV1::KeepUnknown {
            rationale: "The historical effect remains unknown.".into(),
            evidence_refs: vec!["foreign-proof".into()],
        };
        assert!(decision.validate(&supplied).is_err());
    }
}
