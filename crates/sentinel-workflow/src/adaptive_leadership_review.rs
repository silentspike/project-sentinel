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

/// None is schema 1 blocked; schema 2 is recovery, schema 3 normal budget review.
/// Unknown tools are never eligible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdaptiveLeadershipReviewSubjectV2 {
    BudgetWindowExhausted {
        budget: Box<AdaptiveBudgetWindowAuthorityV1>,
    },
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

/// A source snapshot, not proof of persisted root authority. The domain transaction
/// must independently authenticate the original allowance and continuation history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveBudgetWindowAuthorityV1 {
    pub schema_version: u16,
    pub root_allowance: crate::SubscriptionCallAllowanceV1,
    pub active_allowance_digest: String,
    pub continuation_history_digest: String,
    pub observed_at_ms: u64,
    pub model_calls_exhausted: bool,
    pub deadline_expired: bool,
}

pub fn adaptive_budget_allowance_digest(
    allowance: &crate::SubscriptionCallAllowanceV1,
) -> Result<String, WorkflowError> {
    canonical_sha256("sentinel.workflow.adaptive-budget-allowance.v1", allowance)
}

pub fn adaptive_budget_history_digest(
    continuation: &Option<crate::AdaptiveContinuationStateV1>,
) -> Result<String, WorkflowError> {
    canonical_sha256("sentinel.workflow.adaptive-budget-history.v1", continuation)
}

impl AdaptiveBudgetWindowAuthorityV1 {
    fn validate_shape(&self) -> Result<(), WorkflowError> {
        validate_digest(&self.active_allowance_digest)?;
        validate_digest(&self.continuation_history_digest)?;
        validate_identifier(&self.root_allowance.allowance_id)?;
        validate_identifier(&self.root_allowance.created_by)?;
        let root = &self.root_allowance.grant;
        root.work_item_id.validate()?;
        validate_identifier(&root.assignment_id)?;
        validate_identifier(&root.model)?;
        validate_digest(&root.catalog_digest)?;
        if self.schema_version != 1
            || self.observed_at_ms == 0
            || !(self.model_calls_exhausted || self.deadline_expired)
            || root.schema_version != 1
            || root.assignment_version == 0
            || root.provider != "codex-cli"
            || !(1..=crate::ADAPTIVE_SESSION_MAX_CALLS).contains(&root.max_calls)
            || root.max_concurrent != 1
            || !(1_000..=ADAPTIVE_LEADERSHIP_MAX_DURATION_MS).contains(&root.max_duration_ms)
            || self.root_allowance.created_at_unix_ms == 0
            || root.expires_at_unix_ms <= self.root_allowance.created_at_unix_ms
        {
            return Err(invalid());
        }
        Ok(())
    }

    pub fn validate(
        &self,
        context: &AdaptiveLeadershipReviewContextV1,
    ) -> Result<(), WorkflowError> {
        self.validate_allowances(
            &context.source_session,
            context
                .source_project
                .subscription_call
                .as_ref()
                .ok_or_else(invalid)?,
        )?;
        let session = &context.source_session;
        let project = &context.source_project;
        let work = project
            .work_items
            .get(&session.grant.authority.work_item_id)
            .ok_or_else(invalid)?;
        let mut assignments = work
            .assignments
            .iter()
            .filter(|assignment| assignment.active);
        let assignment = assignments.next().ok_or_else(invalid)?;
        let expected = [
            format!(
                "adaptive-budget-root:{}:{}",
                self.root_allowance.allowance_id, session.grant.provider_authority_digest
            ),
            format!("adaptive-budget-current:{}", self.active_allowance_digest),
            format!(
                "adaptive-budget-history:{}",
                self.continuation_history_digest
            ),
        ];
        if project.schema_version != 1
            || project.tenant_id != session.grant.authority.tenant_id
            || project.project_id != session.grant.authority.project_id
            || assignments.next().is_some()
            || assignment.assignment_id != self.root_allowance.grant.assignment_id
            || assignment.canonical_digest()? != session.grant.authority.assignment_digest
            || project.governance.project_profile.generation
                != session.grant.authority.policy_generation
            || project.governance.project_profile.digest != session.grant.authority.policy_digest
            || expected
                .iter()
                .any(|reference| !context.evidence_refs.contains(reference))
            || context.evidence_refs.iter().any(|reference| {
                if reference.starts_with("adaptive-budget-root:")
                    || reference.starts_with("adaptive-budget-current:")
                    || reference.starts_with("adaptive-budget-history:")
                {
                    !expected.contains(reference)
                } else if reference.starts_with("adaptive-model-result:") {
                    session
                        .last_model_result_digest
                        .as_ref()
                        .is_none_or(|digest| {
                            *reference != format!("adaptive-model-result:{digest}")
                        })
                } else if reference.starts_with("workbench-observation:") {
                    session.last_observation.as_ref().is_none_or(|observation| {
                        *reference
                            != format!(
                                "workbench-observation:{}:{}",
                                observation.effect.id, observation.observation_digest
                            )
                    })
                } else {
                    false
                }
            })
        {
            return Err(invalid());
        }
        Ok(())
    }

    fn validate_allowances(
        &self,
        session: &AdaptiveSessionV1,
        active: &crate::SubscriptionCallAllowanceV1,
    ) -> Result<(), WorkflowError> {
        self.validate_shape()?;
        session.grant.validate()?;
        let root = &self.root_allowance;
        let grant = &root.grant;
        let current = &active.grant;
        let authority = &session.grant.authority;
        let latest = session
            .continuation
            .as_ref()
            .and_then(|state| state.authorizations.last());
        if let Some(state) = &session.continuation {
            for authorization in &state.authorizations {
                authorization.validate()?;
            }
        }
        let duration = session.effective_call_duration_ms();
        let (allowance_id, provider_digest, issued_at, deadline, calls) = match latest {
            Some(last) => (
                &last.provider_allowance_id,
                &last.provider_authority_digest,
                last.issued_at_ms,
                last.deadline_ms,
                last.additional_model_calls,
            ),
            None => (
                &session.grant.provider_allowance_id,
                &session.grant.provider_authority_digest,
                session.grant.created_at_ms,
                session.grant.deadline_ms,
                grant.max_calls,
            ),
        };
        if !session.model_window_exhausted_at(self.observed_at_ms)
            || self.model_calls_exhausted != (session.model_calls >= session.active_model_ceiling())
            || self.deadline_expired != (self.observed_at_ms >= session.active_deadline_ms())
            || self.observed_at_ms < issued_at
            || self.active_allowance_digest != adaptive_budget_allowance_digest(active)?
            || self.continuation_history_digest
                != adaptive_budget_history_digest(&session.continuation)?
            || root.allowance_id != session.grant.provider_allowance_id
            || crate::adaptive_continuation_provider_digest(root, authority)?
                != session.grant.provider_authority_digest
            || grant.work_item_id != authority.work_item_id
            || grant.assignment_version != authority.assignment_version
            || grant.agent_id != authority.agent_id
            || grant.provider != session.grant.provider
            || grant.model != session.grant.model
            || grant.catalog_digest != session.grant.catalog_digest
            || grant.max_duration_ms != session.grant.max_call_duration_ms
            || root.created_at_unix_ms != session.grant.created_at_ms
            || grant.expires_at_unix_ms != session.grant.deadline_ms
            || session.model_calls > session.grant.max_model_calls
            || session.active_model_ceiling() > session.grant.max_model_calls
            || session.model_calls > session.active_model_ceiling()
            || session.continuation.as_ref().is_some_and(|state| {
                state.authorizations.is_empty()
                    || state.authorizations.len() > crate::ADAPTIVE_CONTINUATION_MAX_WINDOWS
            })
            || active.allowance_id != *allowance_id
            || crate::adaptive_continuation_provider_digest(active, authority)? != *provider_digest
            || current.schema_version != 1
            || current.work_item_id != grant.work_item_id
            || current.assignment_id != grant.assignment_id
            || current.assignment_version != grant.assignment_version
            || current.agent_id != grant.agent_id
            || current.provider != grant.provider
            || current.model != grant.model
            || current.catalog_digest != grant.catalog_digest
            || current.token_policy != grant.token_policy
            || current.max_concurrent != grant.max_concurrent
            || current.max_calls != calls
            || current.max_duration_ms != duration
            || current.expires_at_unix_ms != deadline
            || active.created_at_unix_ms != issued_at
        {
            return Err(invalid());
        }
        Ok(())
    }
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
        let (call_limit, duration_limit) = match (&self.grant.subject, self.grant.schema_version) {
            (Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }), 3 | 4) => {
                self.context.validate(&self.grant)?;
                if self.schema_version != self.grant.schema_version
                    || (self.grant.schema_version == 3 && self.grant.recovery_epoch.is_some())
                    || (self.grant.schema_version == 4
                        && self
                            .grant
                            .recovery_epoch
                            .as_ref()
                            .is_none_or(|binding| binding.schema_version != 2))
                    || issued_at_ms < budget.observed_at_ms
                    || source.continuation.as_ref().is_some_and(|state| {
                        state.authorizations.len() >= crate::ADAPTIVE_CONTINUATION_MAX_WINDOWS
                    })
                    || self.grant.provider != current.grant.provider
                    || self.grant.model != current.grant.model
                    || self.grant.catalog_digest != current.grant.catalog_digest
                    || self.grant.token_policy != current.grant.token_policy
                {
                    return Err(invalid());
                }
                (
                    budget.root_allowance.grant.max_calls,
                    budget.root_allowance.grant.max_duration_ms,
                )
            }
            (_, 1 | 2)
                if !matches!(
                    &self.grant.subject,
                    Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { .. })
                ) =>
            {
                (current.grant.max_calls, current.grant.max_duration_ms)
            }
            _ => return Err(invalid()),
        };
        if issued_at_ms == 0
            || !(1_000..=ADAPTIVE_LEADERSHIP_MAX_GRANT_MS).contains(&window_ms)
            || !(1..=crate::ADAPTIVE_SESSION_MAX_CALLS).contains(&additional_model_calls)
            || additional_model_calls > remaining.min(call_limit)
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
                    .min(duration_limit)
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
    DeferBudget {
        rationale: String,
        evidence_refs: Vec<String>,
    },
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
            AdaptiveLeadershipReviewDecisionKindV1::DeferBudget {
                rationale,
                evidence_refs,
            }
            | AdaptiveLeadershipReviewDecisionKindV1::ResolveBlocked {
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
            AdaptiveLeadershipReviewDecisionKindV1::DeferBudget { .. } => {
                matches!(self.schema_version, 3 | 4)
            }
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
                matches!(self.schema_version, 2 | 3 | 4)
                    && (1..=crate::ADAPTIVE_SESSION_MAX_CALLS).contains(&additional_model_calls)
                    && (1_000..=ADAPTIVE_LEADERSHIP_MAX_GRANT_MS).contains(&window_ms)
            }
        };
        if !version_matches
            || !valid_text(rationale, ADAPTIVE_LEADERSHIP_MAX_RATIONALE_BYTES)
            || (matches!(self.schema_version, 2 | 3 | 4) && refs.is_empty())
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
        if matches!(grant.schema_version, 3 | 4)
            != matches!(
                &grant.subject,
                Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { .. })
            )
            || (grant.schema_version == 3 && grant.recovery_epoch.is_some())
        {
            return Err(invalid());
        }
        if let Some(binding) = &grant.recovery_epoch {
            binding.validate_for(
                &grant.leadership_principal.tenant_id,
                grant.session_id,
                grant.review_id,
            )?;
            if !matches!(
                (grant.schema_version, binding.schema_version),
                (2, 1) | (4, 2)
            ) || (binding.schema_version == 1
                && !matches!(
                    &grant.subject,
                    Some(AdaptiveLeadershipReviewSubjectV2::UnknownModel { .. })
                        | Some(AdaptiveLeadershipReviewSubjectV2::BlockedContinuation {
                            resolution_event_id: None,
                            ..
                        })
                ))
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
        if grant.schema_version == 4 && grant.recovery_epoch.is_none() {
            return Err(invalid());
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
            ) | (
                Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { .. }),
                AdaptiveLeadershipReviewDecisionKindV1::DeferBudget { .. }
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
            if !matches!(
                (self.schema_version, binding.schema_version),
                (2, 1) | (4, 2)
            ) || (binding.schema_version == 1
                && !matches!(
                    &self.subject,
                    Some(AdaptiveLeadershipReviewSubjectV2::UnknownModel { .. })
                        | Some(AdaptiveLeadershipReviewSubjectV2::BlockedContinuation {
                            resolution_event_id: None,
                            ..
                        })
                ))
            {
                return Err(invalid());
            }
        }
        let valid_subject = match (&self.subject, self.schema_version) {
            (Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }), 3 | 4) => {
                budget.validate_shape()?;
                self.expected_reason_code.is_empty()
                    && budget.observed_at_ms <= issued_at_ms
                    && ((self.schema_version == 3 && self.recovery_epoch.is_none())
                        || (self.schema_version == 4
                            && self
                                .recovery_epoch
                                .as_ref()
                                .is_some_and(|binding| binding.schema_version == 2)))
            }
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
        if !matches!(
            (&grant.subject, grant.schema_version),
            (None, 1)
                | (
                    Some(
                        AdaptiveLeadershipReviewSubjectV2::UnknownModel { .. }
                            | AdaptiveLeadershipReviewSubjectV2::BlockedContinuation { .. }
                    ),
                    2
                )
                | (
                    Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { .. }),
                    3 | 4
                )
        ) || (grant.schema_version == 3 && grant.recovery_epoch.is_some())
            || (grant.schema_version == 4
                && grant
                    .recovery_epoch
                    .as_ref()
                    .is_none_or(|binding| binding.schema_version != 2))
        {
            return Err(invalid());
        }
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
            Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }) => {
                budget.validate(self)?;
                matches!(grant.schema_version, 3 | 4)
                    && ((grant.schema_version == 3 && grant.recovery_epoch.is_none())
                        || (grant.schema_version == 4
                            && grant
                                .recovery_epoch
                                .as_ref()
                                .is_some_and(|binding| binding.schema_version == 2)))
                    && grant.expected_reason_code.is_empty()
                    && grant.assignment_id == budget.root_allowance.grant.assignment_id
                    && grant.provider == session.grant.provider
                    && grant.model == session.grant.model
                    && grant.catalog_digest == session.grant.catalog_digest
                    && grant.token_policy == budget.root_allowance.grant.token_policy
            }
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

#[cfg(test)]
mod budget_tests {
    use super::*;
    use crate::adaptive::continuation_tests::{authorization, grant, NOW};

    fn fixture() -> AdaptiveLeadershipReviewCallV1 {
        let mut session = AdaptiveSessionV1::initial(grant()).unwrap();
        let assignment: crate::AssignmentV1 = serde_json::from_value(serde_json::json!({
            "assignment_id": "assignment-01", "agent_id": 7, "role": "developer",
            "specialties": ["rust"],
            "profile": {"profile_id": "coding-agent-v1", "generation": 2, "digest": "3".repeat(64)},
            "organization_generation": 9, "organization_digest": "2".repeat(64),
            "assignment_version": 3, "delegated_by": null, "reason_ref": "Implement source",
            "active": true, "assigned_by": "pm-01", "created_at_unix_ms": NOW,
            "ended_at_unix_ms": null
        }))
        .unwrap();
        session.grant.authority.assignment_digest = assignment.canonical_digest().unwrap();
        let root = crate::SubscriptionCallAllowanceV1 {
            allowance_id: session.grant.provider_allowance_id.clone(),
            grant: crate::SubscriptionCallGrantV1 {
                schema_version: 1,
                work_item_id: session.grant.authority.work_item_id.clone(),
                assignment_id: assignment.assignment_id.clone(),
                assignment_version: 3,
                agent_id: session.grant.authority.agent_id,
                provider: session.grant.provider.clone(),
                model: session.grant.model.clone(),
                catalog_digest: session.grant.catalog_digest.clone(),
                max_calls: 16,
                max_concurrent: 1,
                max_duration_ms: 120_000,
                token_policy: SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,
                expires_at_unix_ms: session.grant.deadline_ms,
            },
            created_by: "pm-01".into(),
            created_at_unix_ms: NOW,
            dispatch: None,
        };
        session.grant.provider_authority_digest =
            crate::adaptive_continuation_provider_digest(&root, &session.grant.authority).unwrap();
        let mut prior = authorization(&session);
        prior.additional_model_calls = 1;
        let mut current = root.clone();
        current.allowance_id = prior.provider_allowance_id.clone();
        current.created_at_unix_ms = prior.issued_at_ms;
        current.grant.max_calls = 1;
        current.grant.max_duration_ms = 10_000;
        current.grant.expires_at_unix_ms = prior.deadline_ms;
        prior.provider_authority_digest =
            crate::adaptive_continuation_provider_digest(&current, &session.grant.authority)
                .unwrap();
        session.continuation = Some(crate::AdaptiveContinuationStateV1 {
            authorizations: vec![prior],
            model_ceiling: 2,
            observation_required: false,
        });
        session.model_calls = 2;
        session.version = 2;
        session.updated_at_ms = NOW + 1_001;
        let budget = AdaptiveBudgetWindowAuthorityV1 {
            schema_version: 1,
            root_allowance: root,
            active_allowance_digest: adaptive_budget_allowance_digest(&current).unwrap(),
            continuation_history_digest: adaptive_budget_history_digest(&session.continuation)
                .unwrap(),
            observed_at_ms: session.updated_at_ms,
            model_calls_exhausted: true,
            deadline_expired: false,
        };
        let project: ProjectV1 = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "tenant_id": "tenant-01", "project_id": "project-01",
            "agreement_id": "agreement-01", "agreement_digest": "a".repeat(64),
            "governance": {"owner": 1, "participants": [{
                "agent_id": 1, "principal_id": "pm-01", "role": "project_manager",
                "specialties": ["planning"], "reports_to": null,
                "profile": {"profile_id": "pm-v1", "generation": 1, "digest": "a".repeat(64)}
            }], "project_profile": {"profile_id": "project-v1", "generation": 6, "digest": "5".repeat(64)}},
            "cost_ceiling_micros": 100, "provider_cost_ceilings_micros": {},
            "lifecycle_state": "active", "reserved_cost_micros": 0, "committed_cost_micros": 0,
            "work_items": {"work-01": {
                "spec": {"work_item_id": "work-01", "title": "Implement source",
                    "objective": "Deliver inspected source", "required_role": "developer",
                    "required_specialties": ["rust"], "dependency_ids": [], "owner": 7,
                    "inputs": [], "outputs": [{"name": "source", "media_type": "application/octet-stream",
                        "digest_algorithm": "sha256", "contract_generation": 1, "contract_digest": "a".repeat(64)}],
                    "quality_gate": {"gate_id": "source-qa-v1", "generation": 1, "digest": "a".repeat(64)},
                    "budget_micros": 100, "rework": null},
                "state": "assigned", "version": 1, "assignments": [assignment],
                "output_receipts": [], "gate_receipt": null, "transition_history": []
            }},
            "decisions": [], "handoffs": [], "blockers": [], "approvals": [], "reservations": [],
            "subscription_call": current, "rooms": [], "questions": [], "actions": [],
            "version": 1, "created_at_unix_ms": NOW, "updated_at_unix_ms": NOW
        })).unwrap();
        let evidence_refs = vec![
            format!(
                "adaptive-budget-root:{}:{}",
                budget.root_allowance.allowance_id, session.grant.provider_authority_digest
            ),
            format!("adaptive-budget-current:{}", budget.active_allowance_digest),
            format!(
                "adaptive-budget-history:{}",
                budget.continuation_history_digest
            ),
        ];
        let context = AdaptiveLeadershipReviewContextV1 {
            source_project: project,
            source_session: session,
            tool_catalog: serde_json::json!({"tools": ["list_directory"]}),
            evidence_refs,
        };
        let leader_authority = PrincipalAuthorityV1::derive("pm-01", 1, &[1; 32]).unwrap();
        let fingerprint =
            adaptive_leadership_evidence_fingerprint(&context.tool_catalog, &context.evidence_refs)
                .unwrap();
        let review = AdaptiveLeadershipReviewGrantV1 {
            schema_version: 3,
            review_id: adaptive_leadership_review_id(
                context.source_session.grant.session_id,
                context.source_session.version,
                &fingerprint,
            )
            .unwrap(),
            project_id: context.source_project.project_id.clone(),
            expected_project_version: 1,
            work_item_id: context.source_session.grant.authority.work_item_id.clone(),
            session_id: context.source_session.grant.session_id,
            expected_session_version: context.source_session.version,
            expected_reason_code: String::new(),
            evidence_fingerprint: fingerprint,
            leadership_principal: AuthenticatedCompanyPrincipalV1 {
                schema_version: 1,
                tenant_id: context.source_project.tenant_id.clone(),
                principal_id: "pm-01".into(),
                kind: CompanyPrincipalKindV1::Agent,
                role: CompanyRoleV1::ProjectManager,
                customer_id: None,
                agent_id: Some(crate::AgentId(1)),
                authority_generation: 1,
                authority_digest: leader_authority.authority_digest.clone(),
            },
            leadership_authority: leader_authority,
            assignment_id: "assignment-01".into(),
            assignee_authority: context.source_session.grant.authority.clone(),
            provider: context.source_session.grant.provider.clone(),
            model: context.source_session.grant.model.clone(),
            catalog_digest: context.source_session.grant.catalog_digest.clone(),
            max_duration_ms: 120_000,
            token_policy: SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,
            expires_at_unix_ms: NOW + 120_000,
            subject: Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted {
                budget: Box::new(budget),
            }),
            recovery_epoch: None,
        };
        AdaptiveLeadershipReviewCallV1 {
            schema_version: 3,
            review_key: "budget-review".into(),
            allowance_id: "leadership-allowance".into(),
            operation_id: Uuid::from_u128(501),
            grant: review,
            context,
            version: 1,
            created_at_unix_ms: NOW + 1_001,
            grant_issued_at_unix_ms: NOW + 1_001,
            updated_at_unix_ms: NOW + 1_001,
            dispatch: None,
            decision: None,
            model_response_digest: None,
            resolution_event_id: None,
            retired_at_unix_ms: None,
            continuation: None,
        }
    }

    fn budget(call: &AdaptiveLeadershipReviewCallV1) -> &AdaptiveBudgetWindowAuthorityV1 {
        let Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }) =
            &call.grant.subject
        else {
            panic!("budget subject")
        };
        budget
    }

    fn replace_budget(
        call: &mut AdaptiveLeadershipReviewCallV1,
        authority: AdaptiveBudgetWindowAuthorityV1,
    ) {
        call.context.evidence_refs = vec![
            format!(
                "adaptive-budget-root:{}:{}",
                authority.root_allowance.allowance_id,
                call.context.source_session.grant.provider_authority_digest
            ),
            format!(
                "adaptive-budget-current:{}",
                authority.active_allowance_digest
            ),
            format!(
                "adaptive-budget-history:{}",
                authority.continuation_history_digest
            ),
        ];
        call.grant.evidence_fingerprint = adaptive_leadership_evidence_fingerprint(
            &call.context.tool_catalog,
            &call.context.evidence_refs,
        )
        .unwrap();
        call.grant.expected_session_version = call.context.source_session.version;
        call.grant.review_id = adaptive_leadership_review_id(
            call.grant.session_id,
            call.grant.expected_session_version,
            &call.grant.evidence_fingerprint,
        )
        .unwrap();
        call.grant_issued_at_unix_ms = authority.observed_at_ms;
        call.grant.subject = Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted {
            budget: Box::new(authority),
        });
    }

    #[test]
    fn normal_budget_current_one_allows_larger_root_bounded_window() {
        let call = fixture();
        call.grant.validate(call.grant_issued_at_unix_ms).unwrap();
        call.context.validate(&call.grant).unwrap();
        let allowance = call
            .continuation_allowance(NOW + 1_002, NOW + 121_002, 4)
            .unwrap();
        assert_eq!(allowance.grant.max_calls, 4);
        assert_eq!(allowance.grant.max_duration_ms, 120_000);
        assert_eq!(allowance.grant.provider, call.grant.provider);
        assert_eq!(allowance.grant.model, call.grant.model);
        assert_eq!(allowance.grant.catalog_digest, call.grant.catalog_digest);
        assert_eq!(allowance.grant.max_concurrent, 1);
        assert_eq!(allowance.grant.token_policy, call.grant.token_policy);
        assert!(call
            .continuation_allowance(NOW + 1_002, NOW + 121_002, 14)
            .is_ok());
        assert!(call
            .continuation_allowance(NOW + 1_002, NOW + 121_002, 15)
            .is_err());
        assert!(call
            .continuation_allowance(NOW + 1_000, NOW + 121_000, 4)
            .is_err());
        let mut recovery = call.clone();
        recovery.schema_version = 2;
        recovery.grant.schema_version = 2;
        recovery.grant.subject = Some(AdaptiveLeadershipReviewSubjectV2::BlockedContinuation {
            reason_code: "needs_review".into(),
            resolution_event_id: None,
        });
        assert!(recovery
            .continuation_allowance(NOW + 11_000, NOW + 131_000, 4)
            .is_err());
    }

    #[test]
    fn budget_requires_exact_flags_digests_clocks_and_ready_cursor() {
        let call = fixture();
        let original = budget(&call);
        for field in 0..7 {
            let mut changed = original.clone();
            match field {
                0 => changed.schema_version = 2,
                1 => changed.model_calls_exhausted = false,
                2 => changed.deadline_expired = true,
                3 => changed.active_allowance_digest = "a".repeat(64),
                4 => changed.continuation_history_digest = "b".repeat(64),
                5 => changed.observed_at_ms -= 1,
                _ => changed.root_allowance.grant.max_calls = 32,
            }
            assert!(changed.validate(&call.context).is_err(), "field {field}");
        }
        for cursor in [
            AdaptiveCursorV1::ModelUnknown {
                effect: crate::adaptive::continuation_tests::effect(102),
            },
            AdaptiveCursorV1::Blocked {
                reason_code: "needs_review".into(),
            },
        ] {
            let mut context = call.context.clone();
            context.source_session.cursor = cursor;
            assert!(original.validate(&context).is_err());
        }
        let mut context = call.context.clone();
        context.source_session.model_calls = 17;
        assert!(original.validate(&context).is_err());
    }

    #[test]
    fn budget_rejects_rebound_active_allowance_and_root_policy() {
        let call = fixture();
        for field in 0..10 {
            let mut context = call.context.clone();
            let active = context.source_project.subscription_call.as_mut().unwrap();
            match field {
                0 => active.allowance_id = "other-allowance".into(),
                1 => active.grant.max_calls = 2,
                2 => active.grant.max_concurrent = 2,
                3 => active.grant.max_duration_ms += 1,
                4 => active.grant.model = "other-model".into(),
                5 => active.grant.assignment_id = "other-assignment".into(),
                6 => active.grant.expires_at_unix_ms += 1,
                7 => active.created_at_unix_ms += 1,
                8 => context.source_session.grant.authority.policy_digest = "a".repeat(64),
                _ => context.source_project.governance.project_profile.generation += 1,
            }
            let mut authority = budget(&call).clone();
            authority.active_allowance_digest = adaptive_budget_allowance_digest(
                context.source_project.subscription_call.as_ref().unwrap(),
            )
            .unwrap();
            assert!(authority.validate(&context).is_err(), "field {field}");
        }
    }

    #[test]
    fn budget_refusal_and_continue_require_schema_three_and_exact_subject() {
        let call = fixture();
        for kind in [
            AdaptiveLeadershipReviewDecisionKindV1::DeferBudget {
                rationale: "Retain the bounded window".into(),
                evidence_refs: call.context.evidence_refs.clone(),
            },
            AdaptiveLeadershipReviewDecisionKindV1::Continue {
                additional_model_calls: 4,
                window_ms: 120_000,
                rationale: "Inspect before continuing delivery".into(),
                evidence_refs: call.context.evidence_refs.clone(),
            },
        ] {
            let mut decision = AdaptiveLeadershipReviewDecisionV1 {
                schema_version: 3,
                decision: kind,
            };
            decision.validate(&call.context.evidence_refs).unwrap();
            decision.validate_subject(&call.grant).unwrap();
            decision.schema_version = 2;
            assert!(decision.validate_subject(&call.grant).is_err());
            if matches!(
                decision.decision,
                AdaptiveLeadershipReviewDecisionKindV1::DeferBudget { .. }
            ) {
                assert!(decision.validate(&call.context.evidence_refs).is_err());
            }
        }
        for kind in [
            AdaptiveLeadershipReviewDecisionKindV1::DeferBudget {
                rationale: "Wait".into(),
                evidence_refs: vec![],
            },
            AdaptiveLeadershipReviewDecisionKindV1::DeferBudget {
                rationale: "Wait".into(),
                evidence_refs: vec!["invented-evidence".into()],
            },
            AdaptiveLeadershipReviewDecisionKindV1::KeepBlocked {
                rationale: "Wait".into(),
                evidence_refs: call.context.evidence_refs.clone(),
            },
        ] {
            assert!(AdaptiveLeadershipReviewDecisionV1 {
                schema_version: 3,
                decision: kind
            }
            .validate(&call.context.evidence_refs)
            .is_err());
        }
        let mut wrong = call.grant.clone();
        wrong.schema_version = 2;
        assert!(wrong.validate(call.grant_issued_at_unix_ms).is_err());
        wrong = call.grant.clone();
        wrong.subject = None;
        assert!(wrong.validate(call.grant_issued_at_unix_ms).is_err());
        assert!(call.context.validate(&wrong).is_err());
        wrong = call.grant.clone();
        wrong.recovery_epoch = Some(crate::AdaptiveLeadershipRecoveryBindingV1 {
            schema_version: 1,
            epoch_key: "recovery-epoch".into(),
            epoch_digest: "a".repeat(64),
            review_id: wrong.review_id,
            max_window_ms: 120_000,
            max_additional_model_calls: 1,
        });
        assert!(call.context.validate(&wrong).is_err());
    }

    #[test]
    fn budget_evidence_is_exact_and_prior_result_is_optional() {
        let call = fixture();
        assert!(call
            .context
            .source_session
            .last_model_result_digest
            .is_none());
        assert!(call.context.source_session.last_observation.is_none());
        budget(&call).validate(&call.context).unwrap();
        for reference in [
            "adaptive-model-result:invented",
            "workbench-observation:invented",
            "adaptive-budget-current:invented",
        ] {
            let mut context = call.context.clone();
            context.evidence_refs.push(reference.into());
            assert!(budget(&call).validate(&context).is_err());
        }
        let mut context = call.context.clone();
        context.evidence_refs.remove(0);
        assert!(budget(&call).validate(&context).is_err());
    }

    #[test]
    fn budget_deadline_expiry_before_first_model_needs_no_prior_observation() {
        let call = fixture();
        let authority = budget(&call);
        let session =
            AdaptiveSessionV1::initial(call.context.source_session.grant.clone()).unwrap();
        let mut initial = authority.clone();
        initial.active_allowance_digest =
            adaptive_budget_allowance_digest(&initial.root_allowance).unwrap();
        initial.continuation_history_digest = adaptive_budget_history_digest(&None).unwrap();
        initial.observed_at_ms = session.grant.deadline_ms;
        initial.model_calls_exhausted = false;
        initial.deadline_expired = true;
        initial
            .validate_allowances(&session, &initial.root_allowance)
            .unwrap();
        assert_eq!(session.model_calls, 0);
        assert!(session.last_model_result_digest.is_none());
        assert!(session.last_observation.is_none());
        initial.observed_at_ms -= 1;
        assert!(initial
            .validate_allowances(&session, &initial.root_allowance)
            .is_err());
    }

    #[test]
    fn budget_terminal_policy_heads_validate_but_cannot_mint_continuations() {
        let call = fixture();
        let mut terminal = call.clone();
        terminal.context.source_session.model_calls = 16;
        terminal
            .context
            .source_session
            .continuation
            .as_mut()
            .unwrap()
            .model_ceiling = 16;
        let mut authority = budget(&call).clone();
        authority.continuation_history_digest =
            adaptive_budget_history_digest(&terminal.context.source_session.continuation).unwrap();
        authority
            .validate_allowances(
                &terminal.context.source_session,
                terminal
                    .context
                    .source_project
                    .subscription_call
                    .as_ref()
                    .unwrap(),
            )
            .unwrap();
        replace_budget(&mut terminal, authority);
        budget(&terminal).validate(&terminal.context).unwrap();
        assert!(terminal
            .continuation_allowance(NOW + 1_002, NOW + 121_002, 1)
            .is_err());
        assert!(terminal
            .continuation_allowance(NOW + 1_002, NOW + 121_002, 0)
            .is_err());

        let mut terminal = call.clone();
        let state = terminal
            .context
            .source_session
            .continuation
            .as_mut()
            .unwrap();
        let prior = state.authorizations[0].clone();
        state.authorizations.extend([prior.clone(), prior.clone()]);
        let mut authority = budget(&call).clone();
        authority.continuation_history_digest =
            adaptive_budget_history_digest(&terminal.context.source_session.continuation).unwrap();
        // Historical authenticity is checked by the transaction, not this snapshot.
        authority
            .validate_allowances(
                &terminal.context.source_session,
                terminal
                    .context
                    .source_project
                    .subscription_call
                    .as_ref()
                    .unwrap(),
            )
            .unwrap();
        replace_budget(&mut terminal, authority.clone());
        budget(&terminal).validate(&terminal.context).unwrap();
        assert!(terminal
            .continuation_allowance(NOW + 1_002, NOW + 121_002, 1)
            .is_err());
        terminal
            .context
            .source_session
            .continuation
            .as_mut()
            .unwrap()
            .authorizations
            .push(prior);
        authority.continuation_history_digest =
            adaptive_budget_history_digest(&terminal.context.source_session.continuation).unwrap();
        assert!(authority
            .validate_allowances(
                &terminal.context.source_session,
                terminal
                    .context
                    .source_project
                    .subscription_call
                    .as_ref()
                    .unwrap()
            )
            .is_err());
    }

    #[test]
    fn normal_duration_can_widen_after_tight_recovery_and_validate_next_head() {
        let mut call = fixture();
        let session = &mut call.context.source_session;
        let last = session
            .continuation
            .as_mut()
            .unwrap()
            .authorizations
            .last_mut()
            .unwrap();
        last.deadline_ms = last.issued_at_ms + 1_000;
        let current = call
            .context
            .source_project
            .subscription_call
            .as_mut()
            .unwrap();
        current.grant.expires_at_unix_ms = last.deadline_ms;
        current.grant.max_duration_ms = 1_000;
        last.provider_authority_digest =
            crate::adaptive_continuation_provider_digest(current, &session.grant.authority)
                .unwrap();
        let mut authority = budget(&call).clone();
        authority.active_allowance_digest = adaptive_budget_allowance_digest(
            call.context
                .source_project
                .subscription_call
                .as_ref()
                .unwrap(),
        )
        .unwrap();
        authority.continuation_history_digest =
            adaptive_budget_history_digest(&call.context.source_session.continuation).unwrap();
        replace_budget(&mut call, authority);
        call.context.validate(&call.grant).unwrap();
        let issued_at_ms = NOW + 1_002;
        let allowance = call
            .continuation_allowance(issued_at_ms, issued_at_ms + 120_000, 4)
            .unwrap();
        assert_eq!(allowance.grant.max_duration_ms, 120_000);
        let next = crate::AdaptiveContinuationAuthorizationV1 {
            schema_version: 1,
            operation_id: call.operation_id,
            review_id: call.grant.review_id,
            resolution_event_id: Uuid::from_u128(701),
            session_id: call.grant.session_id,
            source_session_version: call.context.source_session.version,
            source: crate::AdaptiveContinuationSourceV1::BudgetWindowExhausted {
                active_allowance_digest: budget(&call).active_allowance_digest.clone(),
                continuation_history_digest: budget(&call).continuation_history_digest.clone(),
            },
            abandoned_model_effect: None,
            provider_allowance_id: allowance.allowance_id.clone(),
            provider_authority_digest: crate::adaptive_continuation_provider_digest(
                &allowance,
                &call.grant.assignee_authority,
            )
            .unwrap(),
            issued_at_ms,
            deadline_ms: issued_at_ms + 120_000,
            additional_model_calls: 4,
            local_adoption: None,
        };
        call.context.source_session = call
            .context
            .source_session
            .transition(
                &crate::AdaptiveTransitionV1::ContinueGoverned {
                    authorization: next,
                },
                issued_at_ms,
            )
            .unwrap();
        call.context.source_session.model_calls =
            call.context.source_session.active_model_ceiling();
        call.context.source_project.subscription_call = Some(allowance);
        assert_eq!(
            call.context
                .source_session
                .effective_grant()
                .max_call_duration_ms,
            call.context
                .source_project
                .subscription_call
                .as_ref()
                .unwrap()
                .grant
                .max_duration_ms
        );
        let mut authority = budget(&call).clone();
        authority.observed_at_ms = issued_at_ms;
        authority.active_allowance_digest = adaptive_budget_allowance_digest(
            call.context
                .source_project
                .subscription_call
                .as_ref()
                .unwrap(),
        )
        .unwrap();
        authority.continuation_history_digest =
            adaptive_budget_history_digest(&call.context.source_session.continuation).unwrap();
        replace_budget(&mut call, authority);
        call.context.validate(&call.grant).unwrap();
        assert_eq!(
            call.continuation_allowance(issued_at_ms + 1, issued_at_ms + 120_001, 4)
                .unwrap()
                .grant
                .max_duration_ms,
            120_000
        );
    }

    #[test]
    fn budget_domain_hashes_bind_allowance_and_entire_continuation_state() {
        let call = fixture();
        let active = call
            .context
            .source_project
            .subscription_call
            .as_ref()
            .unwrap();
        assert_eq!(
            adaptive_budget_allowance_digest(active).unwrap(),
            budget(&call).active_allowance_digest
        );
        let mut changed = active.clone();
        changed.created_by = "other-leader".into();
        assert_ne!(
            adaptive_budget_allowance_digest(&changed).unwrap(),
            budget(&call).active_allowance_digest
        );
        let mut history = call.context.source_session.continuation.clone();
        history.as_mut().unwrap().observation_required = true;
        assert_ne!(
            adaptive_budget_history_digest(&history).unwrap(),
            budget(&call).continuation_history_digest
        );
        assert_ne!(
            adaptive_budget_history_digest(&None).unwrap(),
            budget(&call).continuation_history_digest
        );
    }
}
