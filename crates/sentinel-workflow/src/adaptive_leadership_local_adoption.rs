//! One exact known leadership response, not another review or renewed epoch.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::digest::canonical_sha256;
use crate::model::validate_digest;
use crate::{
    adaptive_leadership_recovery_project_digest, adaptive_leadership_recovery_session_digest,
    AdaptiveLeadershipRecoveryEpochV1, AdaptiveLeadershipReviewCallV1,
    AdaptiveLeadershipReviewDecisionKindV1, AdaptiveLeadershipReviewDecisionV1,
    AdaptiveRecoveryReleaseV1, AuthenticatedCompanyPrincipalV1, CompanyPrincipalKindV1,
    CompanyRoleV1, PrincipalAuthorityV1, ProjectId, TenantId, WorkItemId, WorkflowError,
    WorkflowErrorCode, ADAPTIVE_LEADERSHIP_MAX_GRANT_MS,
};

pub const ADAPTIVE_LEADERSHIP_LOCAL_ADOPTION_MAX_COMPLETION_ATTEMPTS: u32 = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveLeadershipLocalAdoptionRequestV1 {
    pub schema_version: u16,
    pub operation_id: Uuid,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub work_item_id: WorkItemId,
    pub session_id: Uuid,
    pub review_id: Uuid,
    pub epoch_digest: String,
    pub original_call_digest: String,
    pub project_digest: String,
    pub session_digest: String,
    pub session_head_digest: String,
    pub request_id: String,
    pub request_digest: String,
    pub context_digest: String,
    pub payload_digest: String,
    pub model_response_digest: String,
    pub usage_event_digest: String,
    pub original_completion_error: String,
    pub completion_attempts: u32,
    pub release: AdaptiveRecoveryReleaseV1,
    pub repair_digest: String,
    pub decision: AdaptiveLeadershipReviewDecisionV1,
    pub expires_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveLeadershipLocalAdoptionV1 {
    pub request: AdaptiveLeadershipLocalAdoptionRequestV1,
    pub issuer_principal: AuthenticatedCompanyPrincipalV1,
    pub issuer_authority: PrincipalAuthorityV1,
    pub issued_at_unix_ms: u64,
    pub continuation_deadline_ms: u64,
    pub adoption_key: String,
}

pub fn local_adoption_key(tenant: &TenantId, review_id: Uuid) -> Result<String, WorkflowError> {
    tenant.validate()?;
    if review_id.is_nil() {
        return Err(invalid());
    }
    Ok(format!(
        "local-adoption-{}",
        canonical_sha256(
            "sentinel.workflow.leadership-local-adoption-key.v1",
            &(tenant, review_id),
        )?
    ))
}

pub fn adaptive_leadership_local_adoption_source_call_digest(
    call: &AdaptiveLeadershipReviewCallV1,
) -> Result<String, WorkflowError> {
    canonical_sha256(
        "sentinel.workflow.leadership-local-adoption-source-call.v1",
        &dispatch_source_call(call)?,
    )
}

fn dispatch_source_call(
    call: &AdaptiveLeadershipReviewCallV1,
) -> Result<AdaptiveLeadershipReviewCallV1, WorkflowError> {
    let dispatched_at = call
        .dispatch
        .as_ref()
        .ok_or_else(invalid)?
        .dispatched_at_unix_ms;
    let mut source = call.clone();
    source.version = 2;
    source.updated_at_unix_ms = dispatched_at;
    source.decision = None;
    source.model_response_digest = None;
    source.resolution_event_id = None;
    source.continuation = None;
    source.retired_at_unix_ms = None;
    Ok(source)
}

impl AdaptiveLeadershipLocalAdoptionRequestV1 {
    pub fn key(&self) -> Result<String, WorkflowError> {
        local_adoption_key(&self.tenant_id, self.review_id)
    }

    pub fn canonical_digest(&self) -> Result<String, WorkflowError> {
        canonical_sha256(
            "sentinel.workflow.leadership-local-adoption-request.v1",
            self,
        )
    }

    pub fn validate(
        &self,
        operator: &AuthenticatedCompanyPrincipalV1,
        now_ms: u64,
    ) -> Result<(), WorkflowError> {
        operator.validate()?;
        self.tenant_id.validate()?;
        self.project_id.validate()?;
        self.work_item_id.validate()?;
        self.release.validate()?;
        for digest in [
            &self.epoch_digest,
            &self.original_call_digest,
            &self.project_digest,
            &self.session_digest,
            &self.session_head_digest,
            &self.request_digest,
            &self.context_digest,
            &self.payload_digest,
            &self.model_response_digest,
            &self.usage_event_digest,
            &self.repair_digest,
        ] {
            validate_digest(digest)?;
        }
        continuation_bounds(&self.decision)?;
        if self.schema_version != 1
            || operator.kind != CompanyPrincipalKindV1::Operator
            || !matches!(
                operator.role,
                CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
            )
            || operator.tenant_id != self.tenant_id
            || self.operation_id.is_nil()
            || self.session_id.is_nil()
            || self.review_id.is_nil()
            || self.request_id != format!("company-leadership-{}", self.review_id)
            || self.original_completion_error != "continuation audit invalid"
            || !(1..=ADAPTIVE_LEADERSHIP_LOCAL_ADOPTION_MAX_COMPLETION_ATTEMPTS)
                .contains(&self.completion_attempts)
            || now_ms == 0
            || self
                .expires_at_unix_ms
                .checked_sub(now_ms)
                .is_none_or(|window| window == 0 || window > ADAPTIVE_LEADERSHIP_MAX_GRANT_MS)
        {
            return Err(invalid());
        }
        Ok(())
    }

    fn validate_source_limits(
        &self,
        model_calls: u16,
        root_max_calls: u16,
        root_window_ms: u64,
        epoch_max_calls: u16,
        epoch_window_ms: u64,
    ) -> Result<(), WorkflowError> {
        let (calls, window) = continuation_bounds(&self.decision)?;
        if model_calls
            .checked_add(calls)
            .is_none_or(|total| total > root_max_calls)
            || calls > epoch_max_calls
            || window > root_window_ms
            || window > epoch_window_ms
        {
            return Err(invalid());
        }
        Ok(())
    }
}

impl AdaptiveLeadershipLocalAdoptionV1 {
    pub fn key(&self) -> Result<String, WorkflowError> {
        self.request.key()
    }

    pub fn canonical_digest(&self) -> Result<String, WorkflowError> {
        self.validate()?;
        canonical_sha256(
            "sentinel.workflow.leadership-local-adoption-authority.v1",
            self,
        )
    }

    /// Snapshot validation only; replay never substitutes a fresh issuance clock.
    pub fn validate(&self) -> Result<(), WorkflowError> {
        self.request
            .validate(&self.issuer_principal, self.issued_at_unix_ms)?;
        self.issuer_authority.validate()?;
        let (_, window) = continuation_bounds(&self.request.decision)?;
        if self.adoption_key != self.key()?
            || self.issuer_authority.principal_id != self.issuer_principal.principal_id
            || self.issuer_authority.principal_generation
                != self.issuer_principal.authority_generation
            || self.issuer_authority.authority_digest != self.issuer_principal.authority_digest
            || self.issued_at_unix_ms.checked_add(window) != Some(self.continuation_deadline_ms)
            || self.request.expires_at_unix_ms > self.continuation_deadline_ms
        {
            return Err(invalid());
        }
        Ok(())
    }

    /// Pure source binding, not proof of persisted authority or current admission.
    pub fn validate_call(
        &self,
        original_call: &AdaptiveLeadershipReviewCallV1,
    ) -> Result<(), WorkflowError> {
        self.validate()?;
        self.validate_call_phase(original_call)?;
        let source_call = dispatch_source_call(original_call)?;
        source_call.validate_continuation_source()?;
        let request = &self.request;
        let source = &original_call.context.source_session;
        let grant = &original_call.grant;
        let binding = grant.recovery_epoch.as_ref().ok_or_else(invalid)?;
        binding.validate()?;
        let dispatch = original_call.dispatch.as_ref().ok_or_else(invalid)?;
        if original_call.schema_version != 2
            || grant.schema_version != 2
            || request.tenant_id != grant.leadership_principal.tenant_id
            || request.project_id != grant.project_id
            || request.work_item_id != grant.work_item_id
            || request.session_id != grant.session_id
            || request.review_id != grant.review_id
            || request.review_id != binding.review_id
            || request.epoch_digest != binding.epoch_digest
            || request.original_call_digest
                != adaptive_leadership_local_adoption_source_call_digest(original_call)?
            || request.project_digest
                != adaptive_leadership_recovery_project_digest(
                    &original_call.context.source_project,
                )?
            || request.session_digest != adaptive_leadership_recovery_session_digest(source)?
            || request.request_id != original_call.request_id()
            || request.request_id != dispatch.request_id
            || request.request_digest != dispatch.request_digest
            || request.context_digest != original_call.context_digest()?
            || request.context_digest != dispatch.context_digest
            || self.issued_at_unix_ms < dispatch.dispatched_at_unix_ms
            || self.issued_at_unix_ms < source.active_deadline_ms()
        {
            return Err(invalid());
        }
        request
            .decision
            .validate(&original_call.context.evidence_refs)?;
        request.decision.validate_subject(&original_call.grant)?;
        let (calls, _) = continuation_bounds(&request.decision)?;
        original_call.continuation_allowance(
            self.issued_at_unix_ms,
            self.continuation_deadline_ms,
            calls,
        )?;
        request.validate_source_limits(
            source.model_calls,
            source.grant.max_model_calls,
            source
                .grant
                .deadline_ms
                .checked_sub(source.grant.created_at_ms)
                .ok_or_else(invalid)?,
            binding.max_additional_model_calls,
            binding.max_window_ms,
        )
    }

    fn validate_call_phase(
        &self,
        call: &AdaptiveLeadershipReviewCallV1,
    ) -> Result<(), WorkflowError> {
        if call.retired_at_unix_ms.is_some() {
            return Err(invalid());
        }
        match call.version {
            2 if call.decision.is_none()
                && call.model_response_digest.is_none()
                && call.resolution_event_id.is_none()
                && call.continuation.is_none() =>
            {
                Ok(())
            }
            3 => {
                let continuation = call.continuation.as_ref().ok_or_else(invalid)?;
                if continuation.local_adoption.as_deref() != Some(self)
                    || call.decision.as_ref() != Some(&self.request.decision)
                    || call.model_response_digest.as_ref()
                        != Some(&self.request.model_response_digest)
                    || call.resolution_event_id != Some(continuation.resolution_event_id)
                    || continuation.source_session_version != call.grant.expected_session_version
                {
                    return Err(invalid());
                }
                continuation.validate()
            }
            _ => Err(invalid()),
        }
    }

    /// The store separately verifies live head, installed repair, durable payload,
    /// response/usage digests and current admission. Historical replay uses issuance.
    pub fn validate_against(
        &self,
        epoch: &AdaptiveLeadershipRecoveryEpochV1,
        original_call: &AdaptiveLeadershipReviewCallV1,
    ) -> Result<(), WorkflowError> {
        self.validate_call(original_call)?;
        epoch.validate_review(
            &original_call.grant,
            &original_call.context,
            original_call.grant_issued_at_unix_ms,
        )?;
        epoch.validate_decision(&self.request.decision)?;
        let request = &self.request;
        if request.tenant_id != epoch.request.tenant_id
            || request.project_id != epoch.request.project_id
            || request.work_item_id != epoch.request.work_item_id
            || request.session_id != epoch.request.session_id
            || request.review_id != epoch.review_id
            || request.epoch_digest != epoch.canonical_digest()?
            || request.session_head_digest != epoch.request.session_head_digest
            || original_call.operation_id != epoch.request.operation_id
            || original_call.allowance_id != format!("leadership-recovery-{}", epoch.epoch_key)
            || original_call.created_at_unix_ms != epoch.issued_at_unix_ms
        {
            return Err(invalid());
        }
        // The repaired release is intentionally independent of epoch.request.release.
        Ok(())
    }
}

fn continuation_bounds(
    decision: &AdaptiveLeadershipReviewDecisionV1,
) -> Result<(u16, u64), WorkflowError> {
    if let AdaptiveLeadershipReviewDecisionKindV1::Continue {
        additional_model_calls,
        window_ms,
        evidence_refs,
        ..
    } = &decision.decision
    {
        // This checks decision shape/uniqueness, not supplied-reference identity.
        // validate_call checks the original source's actual supplied references.
        decision.validate(evidence_refs)?;
        Ok((*additional_model_calls, *window_ms))
    } else {
        Err(invalid())
    }
}

fn invalid() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::InvalidInput,
        false,
        "invalid leadership local adoption authority",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> AdaptiveLeadershipLocalAdoptionV1 {
        let authority = PrincipalAuthorityV1::derive("adoption-operator", 1, &[1; 32]).unwrap();
        let principal = AuthenticatedCompanyPrincipalV1 {
            schema_version: 1,
            tenant_id: TenantId::parse("adoption-tenant").unwrap(),
            principal_id: authority.principal_id.clone(),
            kind: CompanyPrincipalKindV1::Operator,
            role: CompanyRoleV1::TechnicalLead,
            customer_id: None,
            agent_id: None,
            authority_generation: authority.principal_generation,
            authority_digest: authority.authority_digest.clone(),
        };
        let request = AdaptiveLeadershipLocalAdoptionRequestV1 {
            schema_version: 1,
            operation_id: Uuid::from_u128(1),
            tenant_id: principal.tenant_id.clone(),
            project_id: ProjectId::parse("adoption-project").unwrap(),
            work_item_id: WorkItemId::parse("adoption-work").unwrap(),
            session_id: Uuid::from_u128(2),
            review_id: Uuid::from_u128(3),
            epoch_digest: "a".repeat(64),
            original_call_digest: "b".repeat(64),
            project_digest: "c".repeat(64),
            session_digest: "d".repeat(64),
            session_head_digest: "e".repeat(64),
            request_id: format!("company-leadership-{}", Uuid::from_u128(3)),
            request_digest: "f".repeat(64),
            context_digest: "1".repeat(64),
            payload_digest: "2".repeat(64),
            model_response_digest: "3".repeat(64),
            usage_event_digest: "4".repeat(64),
            original_completion_error: "continuation audit invalid".into(),
            completion_attempts: 5,
            release: AdaptiveRecoveryReleaseV1 {
                schema_version: 1,
                source_git_sha: "5".repeat(40),
                release_manifest_digest: "6".repeat(64),
                gateway_binary_digest: "7".repeat(64),
            },
            repair_digest: "8".repeat(64),
            decision: AdaptiveLeadershipReviewDecisionV1 {
                schema_version: 2,
                decision: AdaptiveLeadershipReviewDecisionKindV1::Continue {
                    additional_model_calls: 1,
                    window_ms: 120_000,
                    rationale: "Adopt the exact retained leadership response".into(),
                    evidence_refs: vec!["source:retained".into()],
                },
            },
            expires_at_unix_ms: 121_000,
        };
        AdaptiveLeadershipLocalAdoptionV1 {
            adoption_key: request.key().unwrap(),
            request,
            issuer_principal: principal,
            issuer_authority: authority,
            issued_at_unix_ms: 1_000,
            continuation_deadline_ms: 121_000,
        }
    }

    #[test]
    fn roundtrip_retains_fixed_clock_key_and_digest() {
        let original = record();
        original.validate().unwrap();
        let decoded: AdaptiveLeadershipLocalAdoptionV1 =
            serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
        assert_eq!(decoded, original);
        assert_eq!(
            decoded.canonical_digest().unwrap(),
            original.canonical_digest().unwrap()
        );
        let mut changed = original.clone();
        changed.request.operation_id = Uuid::from_u128(4);
        assert_eq!(changed.key().unwrap(), original.key().unwrap());
        assert_ne!(
            changed.canonical_digest().unwrap(),
            original.canonical_digest().unwrap()
        );
        changed.request.review_id = Uuid::from_u128(5);
        assert_ne!(changed.key().unwrap(), original.key().unwrap());
        assert!(changed.validate().is_err());
    }

    #[test]
    fn operator_and_exact_issuer_binding_are_required() {
        for role in [CompanyRoleV1::ProjectManager, CompanyRoleV1::TechnicalLead] {
            let mut value = record();
            value.issuer_principal.role = role;
            value.validate().unwrap();
        }
        for role in [
            CompanyRoleV1::Developer,
            CompanyRoleV1::Sales,
            CompanyRoleV1::Gaia,
        ] {
            let mut value = record();
            value.issuer_principal.role = role;
            assert!(value.validate().is_err());
        }
        let original = record();
        for field in ["kind", "tenant", "principal", "generation", "digest"] {
            let mut value = original.clone();
            match field {
                "kind" => {
                    value.issuer_principal.kind = CompanyPrincipalKindV1::Agent;
                    value.issuer_principal.agent_id = Some(crate::AgentId(1));
                }
                "tenant" => value.issuer_principal.tenant_id = TenantId::parse("foreign").unwrap(),
                "principal" => value.issuer_authority.principal_id = "foreign".into(),
                "generation" => value.issuer_authority.principal_generation += 1,
                _ => value.issuer_authority.authority_digest = "9".repeat(64),
            }
            assert!(value.validate().is_err(), "{field}");
        }
    }

    #[test]
    fn issuance_and_continuation_clocks_cannot_be_renewed_or_overflow() {
        let original = record();
        for expiry in [0, 999, 1_000, 121_001, 301_001, u64::MAX] {
            let mut value = original.clone();
            value.request.expires_at_unix_ms = expiry;
            assert!(value.validate().is_err(), "expiry {expiry}");
        }
        for issued in [0, 999, 1_001, 121_000, u64::MAX] {
            let mut value = original.clone();
            value.issued_at_unix_ms = issued;
            assert!(value.validate().is_err(), "issued {issued}");
        }
        let mut value = original.clone();
        value.continuation_deadline_ms += 1;
        assert!(value.validate().is_err());
        value = original;
        value.issued_at_unix_ms = u64::MAX - 100;
        value.request.expires_at_unix_ms = u64::MAX;
        value.continuation_deadline_ms = u64::MAX;
        assert!(value.validate().is_err());
    }

    #[test]
    fn only_exact_failure_and_canonical_request_bindings_are_accepted() {
        let original = serde_json::to_value(record()).unwrap();
        for (pointer, replacement) in [
            ("/request/schema_version", serde_json::json!(2)),
            ("/request/completion_attempts", serde_json::json!(0)),
            ("/request/completion_attempts", serde_json::json!(6)),
            (
                "/request/original_completion_error",
                serde_json::json!("UnknownOutcome: bridge_task_ended_without_durable_response"),
            ),
            (
                "/request/request_id",
                serde_json::json!("company-leadership-foreign"),
            ),
            ("/request/session_id", serde_json::json!(Uuid::nil())),
            ("/request/project_id", serde_json::json!("Non-Canonical")),
            ("/request/payload_digest", serde_json::json!("A".repeat(64))),
            (
                "/request/release/source_git_sha",
                serde_json::json!("not-a-release"),
            ),
        ] {
            let mut encoded = original.clone();
            *encoded.pointer_mut(pointer).unwrap() = replacement;
            let value: AdaptiveLeadershipLocalAdoptionV1 = serde_json::from_value(encoded).unwrap();
            assert!(value.validate().is_err(), "{pointer}");
        }
        let mut encoded = original;
        encoded["request"]["extra"] = serde_json::json!(true);
        assert!(serde_json::from_value::<AdaptiveLeadershipLocalAdoptionV1>(encoded).is_err());
    }

    #[test]
    fn genuine_continue_and_canonical_reference_constraints_are_retained() {
        let original = serde_json::to_value(record()).unwrap();
        for (pointer, replacement) in [
            ("/request/decision/schema_version", serde_json::json!(1)),
            (
                "/request/decision/decision/additional_model_calls",
                serde_json::json!(0),
            ),
            (
                "/request/decision/decision/additional_model_calls",
                serde_json::json!(65),
            ),
            (
                "/request/decision/decision/window_ms",
                serde_json::json!(999),
            ),
            (
                "/request/decision/decision/window_ms",
                serde_json::json!(300_001),
            ),
            (
                "/request/decision/decision/rationale",
                serde_json::json!(" "),
            ),
            (
                "/request/decision/decision/evidence_refs",
                serde_json::json!([]),
            ),
            (
                "/request/decision/decision/evidence_refs",
                serde_json::json!(["source:retained", "source:retained"]),
            ),
        ] {
            let mut encoded = original.clone();
            *encoded.pointer_mut(pointer).unwrap() = replacement;
            let value: AdaptiveLeadershipLocalAdoptionV1 = serde_json::from_value(encoded).unwrap();
            assert!(value.validate().is_err(), "{pointer}");
        }
        let mut value = record();
        value.request.decision.decision = AdaptiveLeadershipReviewDecisionKindV1::KeepUnknown {
            rationale: "Do not continue".into(),
            evidence_refs: vec!["source:retained".into()],
        };
        assert!(value.validate().is_err());
        value = record();
        assert!(value
            .request
            .decision
            .validate(&["source:foreign".into()])
            .is_err());
    }

    #[test]
    fn root_remaining_calls_and_both_window_caps_are_preserved() {
        let request = record().request;
        request
            .validate_source_limits(3, 4, 300_000, 1, 120_000)
            .unwrap();
        for limits in [
            (4, 4, 300_000, 1, 120_000),
            (u16::MAX, u16::MAX, 300_000, 1, 120_000),
            (3, 4, 119_999, 1, 120_000),
            (3, 4, 300_000, 0, 120_000),
            (3, 4, 300_000, 1, 119_999),
        ] {
            assert!(request
                .validate_source_limits(limits.0, limits.1, limits.2, limits.3, limits.4)
                .is_err());
        }
    }

    #[test]
    fn completed_continuation_retains_exact_authority_and_historical_clock() {
        let original = record();
        let continuation = crate::AdaptiveContinuationAuthorizationV1 {
            schema_version: 1,
            operation_id: original.request.operation_id,
            review_id: original.request.review_id,
            resolution_event_id: Uuid::from_u128(6),
            session_id: original.request.session_id,
            source_session_version: 1,
            source: crate::AdaptiveContinuationSourceV1::Blocked {
                reason_code: "leadership_required".into(),
            },
            abandoned_model_effect: None,
            provider_allowance_id: "retained-continuation".into(),
            provider_authority_digest: "9".repeat(64),
            issued_at_ms: original.issued_at_unix_ms,
            deadline_ms: original.continuation_deadline_ms,
            additional_model_calls: 1,
            local_adoption: Some(Box::new(original.clone())),
            resume_policy: None,
        };
        continuation.validate().unwrap();
        let replay: crate::AdaptiveContinuationAuthorizationV1 =
            serde_json::from_slice(&serde_json::to_vec(&continuation).unwrap()).unwrap();
        replay.validate().unwrap();
        assert_eq!(replay.local_adoption.as_deref(), Some(&original));
        assert!(original
            .request
            .validate(
                &original.issuer_principal,
                original.request.expires_at_unix_ms + 1,
            )
            .is_err());
        original.validate().unwrap();
        for field in ["review", "session", "issued", "deadline", "calls", "issuer"] {
            let mut changed = continuation.clone();
            match field {
                "review" => changed.review_id = Uuid::from_u128(7),
                "session" => changed.session_id = Uuid::from_u128(8),
                "issued" => changed.issued_at_ms += 1,
                "deadline" => changed.deadline_ms += 1,
                "calls" => changed.additional_model_calls += 1,
                _ => {
                    changed
                        .local_adoption
                        .as_mut()
                        .unwrap()
                        .issuer_authority
                        .principal_generation += 1
                }
            }
            assert!(changed.validate().is_err(), "{field}");
        }
    }
}
