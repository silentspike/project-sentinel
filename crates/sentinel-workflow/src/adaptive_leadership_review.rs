//! Bounded leadership inference for an immutable execution head and evidence set.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::digest::canonical_sha256;
use crate::model::{validate_digest, validate_identifier};
use crate::{
    AdaptiveCursorV1, AdaptiveEffectV1, AdaptiveSessionV1, AuthenticatedCompanyPrincipalV1,
    CompanyPrincipalKindV1, CompanyRoleV1, CompanyWorkStateV1, PrincipalAuthorityV1, ProjectId,
    ProjectV1, RequestProviderDispatchV1, RuntimeAuthoritySnapshotV1, SubscriptionTokenPolicyV1,
    WorkItemId, WorkflowError, WorkflowErrorCode,
};

pub const ADAPTIVE_LEADERSHIP_MAX_REVIEWS: usize = 3;
pub const ADAPTIVE_LEADERSHIP_MAX_CONTEXT_BYTES: usize = 128 * 1024;
pub const ADAPTIVE_LEADERSHIP_MAX_CATALOG_BYTES: usize = 32 * 1024;
pub const ADAPTIVE_LEADERSHIP_MAX_SUPPLIED_REFS: usize = 32;
pub const ADAPTIVE_LEADERSHIP_MAX_DECISION_REFS: usize = 8;
pub const ADAPTIVE_LEADERSHIP_MAX_RATIONALE_BYTES: usize = 2048;
pub const ADAPTIVE_LEADERSHIP_MAX_DURATION_MS: u64 = 120_000;
pub const ADAPTIVE_LEADERSHIP_MAX_GRANT_MS: u64 = 300_000;

/// None is the historical schema 1 blocked subject. Unknown tools are never eligible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdaptiveLeadershipReviewSubjectV2 {
    UnknownModel {
        effect: AdaptiveEffectV1,
        sealed_unknown_proof_digest: String,
    },
    BlockedContinuation {
        reason_code: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolution_event_id: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveLeadershipReviewContextV1 {
    pub source_project: ProjectV1,
    pub source_session: AdaptiveSessionV1,
    pub tool_catalog: Value,
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveLeadershipReviewGrantV1 {
    pub schema_version: u16,
    pub review_id: Uuid,
    pub project_id: ProjectId,
    pub expected_project_version: u64,
    pub work_item_id: WorkItemId,
    pub session_id: Uuid,
    pub expected_session_version: u64,
    pub expected_reason_code: String,
    pub evidence_fingerprint: String,
    pub leadership_principal: AuthenticatedCompanyPrincipalV1,
    pub leadership_authority: PrincipalAuthorityV1,
    pub assignment_id: String,
    pub assignee_authority: RuntimeAuthoritySnapshotV1,
    pub provider: String,
    pub model: String,
    pub catalog_digest: String,
    pub max_duration_ms: u64,
    pub token_policy: SubscriptionTokenPolicyV1,
    pub expires_at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<AdaptiveLeadershipReviewSubjectV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_epoch: Option<crate::AdaptiveLeadershipRecoveryBindingV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveLeadershipReviewCallV1 {
    pub schema_version: u16,
    pub review_key: String,
    pub allowance_id: String,
    pub operation_id: Uuid,
    pub grant: AdaptiveLeadershipReviewGrantV1,
    pub context: AdaptiveLeadershipReviewContextV1,
    pub version: u64,
    pub created_at_unix_ms: u64,
    pub grant_issued_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
    pub dispatch: Option<RequestProviderDispatchV1>,
    pub decision: Option<AdaptiveLeadershipReviewDecisionV1>,
    pub model_response_digest: Option<String>,
    pub resolution_event_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired_at_unix_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<crate::adaptive::AdaptiveContinuationAuthorizationV1>,
}

/// Adaptive spending is recorded in the journal, never as a fabricated subscription dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveLeadershipAbandonedAllowanceV2 {
    pub schema_version: u16,
    pub allowance_id: String,
    pub review: AdaptiveLeadershipReviewCallV1,
}

impl AdaptiveLeadershipReviewCallV1 {
    pub fn context_digest(&self) -> Result<String, WorkflowError> {
        canonical_sha256(
            "sentinel.workflow.adaptive-leadership-context.v1",
            &(&self.grant, &self.context),
        )
    }

    pub fn request_id(&self) -> String {
        format!("company-leadership-{}", self.grant.review_id)
    }

    pub fn continuation_allowance(
        &self,
        issued_at_ms: u64,
        deadline_ms: u64,
        additional_model_calls: u16,
    ) -> Result<crate::SubscriptionCallAllowanceV1, WorkflowError> {
        let window_ms = deadline_ms.checked_sub(issued_at_ms).ok_or_else(invalid)?;
        let source = &self.context.source_session;
        let current = self
            .context
            .source_project
            .subscription_call
            .as_ref()
            .ok_or_else(invalid)?;
        let remaining = source
            .grant
            .max_model_calls
            .checked_sub(source.model_calls)
            .ok_or_else(invalid)?;
        if issued_at_ms == 0
            || !(1_000..=ADAPTIVE_LEADERSHIP_MAX_GRANT_MS).contains(&window_ms)
            || !(1..=crate::ADAPTIVE_SESSION_MAX_CALLS).contains(&additional_model_calls)
            || additional_model_calls > remaining.min(current.grant.max_calls)
            || current.grant.max_concurrent != 1
            || self.grant.recovery_epoch.as_ref().is_some_and(|binding| {
                additional_model_calls > binding.max_additional_model_calls
                    || window_ms > binding.max_window_ms
            })
        {
            return Err(invalid());
        }
        Ok(crate::SubscriptionCallAllowanceV1 {
            allowance_id: crate::domain::stable_domain_id(
                "subscription",
                &self.grant.leadership_principal.tenant_id,
                self.operation_id,
            )?,
            grant: crate::SubscriptionCallGrantV1 {
                schema_version: 1,
                work_item_id: self.grant.work_item_id.clone(),
                assignment_id: self.grant.assignment_id.clone(),
                assignment_version: self.grant.assignee_authority.assignment_version,
                agent_id: self.grant.assignee_authority.agent_id,
                provider: self.grant.provider.clone(),
                model: self.grant.model.clone(),
                catalog_digest: self.grant.catalog_digest.clone(),
                max_calls: additional_model_calls,
                max_concurrent: 1,
                max_duration_ms: self
                    .context
                    .source_session
                    .grant
                    .max_call_duration_ms
                    .min(current.grant.max_duration_ms)
                    .min(window_ms),
                token_policy: self.grant.token_policy,
                expires_at_unix_ms: deadline_ms,
            },
            created_by: self.grant.leadership_principal.principal_id.clone(),
            created_at_unix_ms: issued_at_ms,
            dispatch: None,
        })
    }
}

/// Matches the production adaptive-provider authority encoding, not a new grant namespace.
pub fn adaptive_leadership_continuation_provider_authority_digest(
    allowance: &crate::SubscriptionCallAllowanceV1,
    authority: &RuntimeAuthoritySnapshotV1,
) -> Result<String, WorkflowError> {
    crate::adaptive::adaptive_continuation_provider_digest(allowance, authority)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimAdaptiveLeadershipReviewCallV1 {
    pub review_id: Uuid,
    pub allowance_id: String,
    pub request_id: String,
    pub request_digest: String,
    pub context_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteAdaptiveLeadershipReviewCallV1 {
    pub review_id: Uuid,
    pub allowance_id: String,
    pub request_digest: String,
    pub model_response_digest: String,
    pub decision: AdaptiveLeadershipReviewDecisionV1,
    pub resolution_event_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<crate::adaptive::AdaptiveContinuationAuthorizationV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveLeadershipReviewDecisionV1 {
    pub schema_version: u16,
    pub decision: AdaptiveLeadershipReviewDecisionKindV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdaptiveLeadershipReviewDecisionKindV1 {
    ResolveBlocked {
        rationale: String,
        evidence_refs: Vec<String>,
    },
    KeepBlocked {
        rationale: String,
        evidence_refs: Vec<String>,
    },
    KeepUnknown {
        rationale: String,
        evidence_refs: Vec<String>,
    },
    Continue {
        additional_model_calls: u16,
        window_ms: u64,
        rationale: String,
        evidence_refs: Vec<String>,
    },
}

impl AdaptiveLeadershipReviewDecisionV1 {
    pub fn resolves_blocked(&self) -> bool {
        matches!(
            self.decision,
            AdaptiveLeadershipReviewDecisionKindV1::ResolveBlocked { .. }
        )
    }

    pub fn validate(&self, supplied_refs: &[String]) -> Result<(), WorkflowError> {
        validate_refs(supplied_refs, ADAPTIVE_LEADERSHIP_MAX_SUPPLIED_REFS)?;
        let (rationale, refs) = match &self.decision {
            AdaptiveLeadershipReviewDecisionKindV1::ResolveBlocked {
                rationale,
                evidence_refs,
            }
            | AdaptiveLeadershipReviewDecisionKindV1::KeepBlocked {
                rationale,
                evidence_refs,
            }
            | AdaptiveLeadershipReviewDecisionKindV1::KeepUnknown {
                rationale,
                evidence_refs,
            }
            | AdaptiveLeadershipReviewDecisionKindV1::Continue {
                rationale,
                evidence_refs,
                ..
            } => (rationale, evidence_refs),
        };
        let version_matches = match self.decision {
            AdaptiveLeadershipReviewDecisionKindV1::ResolveBlocked { .. } => {
                self.schema_version == 1
            }
            AdaptiveLeadershipReviewDecisionKindV1::KeepBlocked { .. } => {
                matches!(self.schema_version, 1 | 2)
            }
            AdaptiveLeadershipReviewDecisionKindV1::KeepUnknown { .. } => self.schema_version == 2,
            AdaptiveLeadershipReviewDecisionKindV1::Continue {
                additional_model_calls,
                window_ms,
                ..
            } => {
                self.schema_version == 2
                    && (1..=crate::ADAPTIVE_SESSION_MAX_CALLS).contains(&additional_model_calls)
                    && (1_000..=ADAPTIVE_LEADERSHIP_MAX_GRANT_MS).contains(&window_ms)
            }
        };
        if !version_matches
            || !valid_text(rationale, ADAPTIVE_LEADERSHIP_MAX_RATIONALE_BYTES)
            || (self.schema_version == 2 && refs.is_empty())
        {
            return Err(invalid());
        }
        validate_refs(refs, ADAPTIVE_LEADERSHIP_MAX_DECISION_REFS)?;
        if refs
            .iter()
            .any(|reference| !supplied_refs.contains(reference))
        {
            return Err(invalid());
        }
        Ok(())
    }

    pub fn validate_subject(
        &self,
        grant: &AdaptiveLeadershipReviewGrantV1,
    ) -> Result<(), WorkflowError> {
        if self.schema_version != grant.schema_version {
            return Err(invalid());
        }
        if let Some(binding) = &grant.recovery_epoch {
            binding.validate_for(
                &grant.leadership_principal.tenant_id,
                grant.session_id,
                grant.review_id,
            )?;
            if grant.schema_version != 2
                || !matches!(
                    &grant.subject,
                    Some(AdaptiveLeadershipReviewSubjectV2::UnknownModel { .. })
                        | Some(AdaptiveLeadershipReviewSubjectV2::BlockedContinuation {
                            resolution_event_id: None,
                            ..
                        })
                )
                || matches!(
                    &self.decision,
                    AdaptiveLeadershipReviewDecisionKindV1::Continue {
                        additional_model_calls, window_ms, ..
                    } if *additional_model_calls > binding.max_additional_model_calls
                        || *window_ms > binding.max_window_ms
                )
            {
                return Err(invalid());
            }
        }
        let valid = matches!(
            (&grant.subject, &self.decision),
            (
                None,
                AdaptiveLeadershipReviewDecisionKindV1::ResolveBlocked { .. }
                    | AdaptiveLeadershipReviewDecisionKindV1::KeepBlocked { .. },
            ) | (
                Some(AdaptiveLeadershipReviewSubjectV2::UnknownModel { .. }),
                AdaptiveLeadershipReviewDecisionKindV1::KeepUnknown { .. }
                    | AdaptiveLeadershipReviewDecisionKindV1::Continue { .. },
            ) | (
                Some(AdaptiveLeadershipReviewSubjectV2::BlockedContinuation { .. }),
                AdaptiveLeadershipReviewDecisionKindV1::KeepBlocked { .. }
                    | AdaptiveLeadershipReviewDecisionKindV1::Continue { .. },
            )
        );
        if !valid {
            return Err(invalid());
        }
        Ok(())
    }
}

/// The audit identity binds the actual dispatched request, raw response and model decision.
pub fn adaptive_leadership_continuation_audit_id(
    review_id: Uuid,
    request_digest: &str,
    model_response_digest: &str,
    decision: &AdaptiveLeadershipReviewDecisionV1,
) -> Result<Uuid, WorkflowError> {
    validate_digest(request_digest)?;
    validate_digest(model_response_digest)?;
    if review_id.is_nil()
        || !matches!(
            decision.decision,
            AdaptiveLeadershipReviewDecisionKindV1::Continue { .. }
        )
    {
        return Err(invalid());
    }
    let digest = canonical_sha256(
        "sentinel.workflow.adaptive-leadership-continuation-audit.v1",
        &(review_id, request_digest, model_response_digest, decision),
    )?;
    adaptive_leadership_review_id(review_id, 1, &digest)
}

/// Evidence refs must name concrete daemon-supplied evidence, not model-authored claims.
pub fn adaptive_leadership_evidence_fingerprint(
    tool_catalog: &Value,
    evidence_refs: &[String],
) -> Result<String, WorkflowError> {
    validate_refs(evidence_refs, ADAPTIVE_LEADERSHIP_MAX_SUPPLIED_REFS)?;
    if tool_catalog
        .as_object()
        .is_none_or(|object| object.is_empty())
        || serde_json::to_vec(tool_catalog)
            .map_err(|_| invalid())?
            .len()
            > ADAPTIVE_LEADERSHIP_MAX_CATALOG_BYTES
    {
        return Err(invalid());
    }
    // Ref order is presentation, not a new evidence set or permission for another call.
    let refs: BTreeSet<_> = evidence_refs.iter().collect();
    canonical_sha256(
        "sentinel.workflow.adaptive-leadership-evidence.v1",
        &(tool_catalog, refs),
    )
}

pub fn adaptive_leadership_review_id(
    session_id: Uuid,
    session_version: u64,
    evidence_fingerprint: &str,
) -> Result<Uuid, WorkflowError> {
    validate_digest(evidence_fingerprint)?;
    if session_id.is_nil() || session_version == 0 || session_version.checked_add(1).is_none() {
        return Err(invalid());
    }
    let mut hash = Sha256::new();
    hash.update(b"sentinel.workflow.adaptive-leadership-review-id.v1\0");
    hash.update(session_id.as_bytes());
    hash.update(session_version.to_be_bytes());
    hash.update(evidence_fingerprint.as_bytes());
    let digest = hash.finalize();
    // Fixed epoch UUIDv7 namespace; this encodes identity, not a decision timestamp.
    let mut bytes = [0_u8; 16];
    bytes[6..].copy_from_slice(&digest[..10]);
    bytes[6] = (bytes[6] & 0x0f) | 0x70;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(Uuid::from_bytes(bytes))
}

impl AdaptiveLeadershipReviewGrantV1 {
    pub fn validate(&self, issued_at_ms: u64) -> Result<(), WorkflowError> {
        self.project_id.validate()?;
        self.work_item_id.validate()?;
        self.leadership_principal.validate()?;
        self.leadership_authority.validate()?;
        self.assignee_authority.validate()?;
        validate_identifier(&self.assignment_id)?;
        validate_identifier(&self.model)?;
        validate_digest(&self.catalog_digest)?;
        let leader = &self.leadership_principal;
        if let Some(binding) = &self.recovery_epoch {
            binding.validate_for(&leader.tenant_id, self.session_id, self.review_id)?;
            if self.schema_version != 2
                || !matches!(
                    &self.subject,
                    Some(AdaptiveLeadershipReviewSubjectV2::UnknownModel { .. })
                        | Some(AdaptiveLeadershipReviewSubjectV2::BlockedContinuation {
                            resolution_event_id: None,
                            ..
                        })
                )
            {
                return Err(invalid());
            }
        }
        let valid_subject = match (&self.subject, self.schema_version) {
            (None, 1) => valid_reason(&self.expected_reason_code),
            (
                Some(AdaptiveLeadershipReviewSubjectV2::UnknownModel {
                    effect,
                    sealed_unknown_proof_digest,
                }),
                2,
            ) => {
                validate_digest(&effect.request_digest)?;
                validate_digest(sealed_unknown_proof_digest)?;
                !effect.id.is_nil() && self.expected_reason_code.is_empty()
            }
            (
                Some(AdaptiveLeadershipReviewSubjectV2::BlockedContinuation {
                    reason_code,
                    resolution_event_id,
                }),
                2,
            ) => {
                valid_reason(reason_code)
                    && reason_code == &self.expected_reason_code
                    && resolution_event_id
                        .as_ref()
                        .is_none_or(|id| Uuid::parse_str(id).is_ok_and(|id| !id.is_nil()))
            }
            _ => false,
        };
        if !valid_subject
            || self.review_id
                != adaptive_leadership_review_id(
                    self.session_id,
                    self.expected_session_version,
                    &self.evidence_fingerprint,
                )?
            || self.expected_project_version == 0
            || leader.kind != CompanyPrincipalKindV1::Agent
            || !matches!(
                leader.role,
                CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
            )
            || self.leadership_authority.principal_id != leader.principal_id
            || self.leadership_authority.principal_generation != leader.authority_generation
            || self.leadership_authority.authority_digest != leader.authority_digest
            || self.assignee_authority.tenant_id != leader.tenant_id
            || self.assignee_authority.project_id != self.project_id
            || self.assignee_authority.work_item_id != self.work_item_id
            || self.provider != "codex-cli"
            || !(1_000..=ADAPTIVE_LEADERSHIP_MAX_DURATION_MS).contains(&self.max_duration_ms)
            || issued_at_ms == 0
            || self.expires_at_unix_ms <= issued_at_ms
            || self.expires_at_unix_ms - issued_at_ms > ADAPTIVE_LEADERSHIP_MAX_GRANT_MS
        {
            return Err(invalid());
        }
        Ok(())
    }
}

impl AdaptiveLeadershipReviewContextV1 {
    pub fn validate(&self, grant: &AdaptiveLeadershipReviewGrantV1) -> Result<(), WorkflowError> {
        let project = &self.source_project;
        let session = &self.source_session;
        session.grant.validate()?;
        project.governance.validate()?;
        let work = project
            .work_items
            .get(&grant.work_item_id)
            .ok_or_else(invalid)?;
        work.spec.validate()?;
        let assignments: Vec<_> = work
            .assignments
            .iter()
            .filter(|assignment| assignment.active)
            .collect();
        let assignment = assignments.first().ok_or_else(invalid)?;
        let authority = &grant.assignee_authority;
        let valid_subject = match &grant.subject {
            None => {
                matches!(&session.cursor, AdaptiveCursorV1::Blocked { reason_code }
                if reason_code == &grant.expected_reason_code)
                    && session.last_model_result_digest.is_some()
            }
            Some(AdaptiveLeadershipReviewSubjectV2::UnknownModel {
                effect,
                sealed_unknown_proof_digest,
            }) => {
                matches!(&session.cursor, AdaptiveCursorV1::ModelUnknown { effect: actual } if actual == effect)
                    && self.evidence_refs.contains(&format!(
                        "adaptive-model-unknown:{}:{}",
                        effect.id, effect.request_digest
                    ))
                    && self.evidence_refs.contains(&format!(
                        "sealed-provider-unknown:{sealed_unknown_proof_digest}"
                    ))
            }
            Some(AdaptiveLeadershipReviewSubjectV2::BlockedContinuation {
                reason_code,
                resolution_event_id,
            }) => {
                session.last_model_result_digest.is_some()
                    && session
                        .last_model_result_digest
                        .as_ref()
                        .is_some_and(|digest| {
                            self.evidence_refs
                                .contains(&format!("adaptive-model-result:{digest}"))
                        })
                    && match (&session.cursor, resolution_event_id) {
                        (
                            AdaptiveCursorV1::Blocked {
                                reason_code: actual,
                            },
                            None,
                        ) => actual == reason_code,
                        (
                            AdaptiveCursorV1::BlockedResolved {
                                reason_code: actual,
                                resolution_event_id: actual_id,
                            },
                            Some(id),
                        ) => actual == reason_code && actual_id == id,
                        _ => false,
                    }
            }
        };
        if project.schema_version != 1
            || project.tenant_id != grant.leadership_principal.tenant_id
            || project.project_id != grant.project_id
            || project.version != grant.expected_project_version
            || !project.governance.participants.iter().any(|participant| {
                Some(participant.agent_id) == grant.leadership_principal.agent_id
                    && participant.principal_id == grant.leadership_principal.principal_id
                    && participant.role == grant.leadership_principal.role
            })
            || work.state != CompanyWorkStateV1::Assigned
            || !matches!(
                work.spec.required_role,
                CompanyRoleV1::Designer | CompanyRoleV1::Developer
            )
            || assignments.len() != 1
            || assignment.assignment_id != grant.assignment_id
            || assignment.agent_id != work.spec.owner
            || assignment.agent_id != authority.agent_id
            || assignment.role != work.spec.required_role
            || assignment.assignment_version != authority.assignment_version
            || assignment.canonical_digest()? != authority.assignment_digest
            || assignment.profile.profile_id != authority.profile_id
            || assignment.profile.generation != authority.profile_generation
            || assignment.profile.digest != authority.profile_digest
            || assignment.organization_generation != authority.organization_generation
            || assignment.organization_digest != authority.organization_digest
            || session.grant.session_id != grant.session_id
            || session.version != grant.expected_session_version
            || session.grant.authority != *authority
            || !valid_subject
            || grant.evidence_fingerprint
                != adaptive_leadership_evidence_fingerprint(
                    &self.tool_catalog,
                    &self.evidence_refs,
                )?
            || serde_json::to_vec(self).map_err(|_| invalid())?.len()
                > ADAPTIVE_LEADERSHIP_MAX_CONTEXT_BYTES
        {
            return Err(invalid());
        }
        Ok(())
    }
}

fn validate_refs(refs: &[String], max: usize) -> Result<(), WorkflowError> {
    let unique: BTreeSet<_> = refs.iter().collect();
    if refs.len() > max
        || unique.len() != refs.len()
        || refs.iter().any(|value| !valid_text(value, 4096))
    {
        return Err(invalid());
    }
    Ok(())
}

fn valid_text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn valid_reason(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn invalid() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::InvalidInput,
        false,
        "invalid adaptive leadership review",
    )
}
