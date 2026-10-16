//! Descriptive standing proposals, never operator, funding or execution authority.
//!
//! Production must load a trusted server policy and resolve its actual registered
//! operator. Validating this DTO or deriving an identity proves neither fact.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::adaptive_resume_policy::{
    require_resume_policy_operator, ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
    ADAPTIVE_RESUME_MAX_POLICY_MS,
};
use crate::digest::canonical_sha256;
use crate::model::{validate_digest, validate_identifier};
use crate::{
    AdaptiveWorkFundingLimitsV1, AuthenticatedCompanyPrincipalV1, TenantId, WorkflowError,
    WorkflowErrorCode,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StandingWorkFundingPolicyV1 {
    pub schema_version: u16,
    pub policy_id: String,
    pub generation: u64,
    pub tenant_id: TenantId,
    pub issuer_principal_id: String,
    pub epoch_duration_ms: u64,
    pub additional_reviews: u16,
    pub additional_windows: u16,
    pub max_window_ms: u64,
}

impl StandingWorkFundingPolicyV1 {
    fn validate_shape(&self) -> Result<(), WorkflowError> {
        self.tenant_id.validate()?;
        validate_identifier(&self.policy_id)?;
        validate_identifier(&self.issuer_principal_id)?;
        let minimum_window_ms = 1_000 + ADAPTIVE_RESUME_DISPATCH_MARGIN_MS;
        if self.schema_version != 1
            || self.policy_id.len() > 40
            || self.policy_id.contains(':')
            || self.generation == 0
            || !(minimum_window_ms..=ADAPTIVE_RESUME_MAX_POLICY_MS)
                .contains(&self.epoch_duration_ms)
            || !(1..=4).contains(&self.additional_reviews)
            || !(1..=4).contains(&self.additional_windows)
        {
            return Err(invalid());
        }
        // Reuse the funding contract for window/review relationships and caps.
        AdaptiveWorkFundingLimitsV1 {
            additional_model_calls: 0,
            additional_tool_calls: 0,
            additional_reviews: self.additional_reviews,
            additional_windows: self.additional_windows,
            max_window_ms: self.max_window_ms,
            max_call_duration_ms: 1_000,
            dispatch_margin_ms: ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
            expires_at_unix_ms: self.epoch_duration_ms,
        }
        .validate()
    }

    /// Checks descriptive issuer consistency, not registration or policy provenance.
    pub fn validate_for(
        &self,
        issuer: &AuthenticatedCompanyPrincipalV1,
    ) -> Result<(), WorkflowError> {
        self.validate_shape()?;
        require_resume_policy_operator(issuer, &self.tenant_id)?;
        if issuer.principal_id != self.issuer_principal_id {
            return Err(WorkflowError::new(
                WorkflowErrorCode::AuthorityConflict,
                false,
                "standing funding issuer does not match policy",
            ));
        }
        Ok(())
    }

    /// Stable proposal key only. Review history, refused review and clocks are absent.
    pub fn operation_id(
        &self,
        session_id: Uuid,
        head_entry_digest: &str,
    ) -> Result<Uuid, WorkflowError> {
        self.validate_shape()?;
        validate_digest(head_entry_digest)?;
        if session_id.is_nil() {
            return Err(invalid());
        }
        let digest = canonical_sha256(
            "sentinel.workflow.standing-work-funding-operation.v1",
            &(
                &self.policy_id,
                self.generation,
                &self.tenant_id,
                session_id,
                head_entry_digest,
            ),
        )?;
        let mut bytes = Uuid::parse_str(&digest[..32])
            .map_err(|_| invalid())?
            .into_bytes();
        // UUIDv8 identifies this custom canonical-SHA256 derivation, not a clock.
        bytes[6] = (bytes[6] & 0x0f) | 0x80;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        Ok(Uuid::from_bytes(bytes))
    }

    pub fn reason_ref(&self, refused_review_id: Uuid) -> Result<String, WorkflowError> {
        self.validate_shape()?;
        if refused_review_id.is_nil() {
            return Err(invalid());
        }
        let reason = format!(
            "standing-funding:{}:{}:{}",
            self.policy_id, self.generation, refused_review_id,
        );
        validate_identifier(&reason)?;
        Ok(reason)
    }

    /// Zero-addition bounds only; the store must prove an adopted, unspent predecessor.
    pub fn limits(
        &self,
        max_call_duration_ms: u64,
        issued_at_ms: u64,
    ) -> Result<AdaptiveWorkFundingLimitsV1, WorkflowError> {
        self.validate_shape()?;
        let minimum_epoch_ms = max_call_duration_ms
            .checked_add(ADAPTIVE_RESUME_DISPATCH_MARGIN_MS)
            .ok_or_else(invalid)?;
        if issued_at_ms == 0 || minimum_epoch_ms > self.epoch_duration_ms {
            return Err(invalid());
        }
        let limits = AdaptiveWorkFundingLimitsV1 {
            additional_model_calls: 0,
            additional_tool_calls: 0,
            additional_reviews: self.additional_reviews,
            additional_windows: self.additional_windows,
            max_window_ms: self.max_window_ms,
            max_call_duration_ms,
            dispatch_margin_ms: ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
            expires_at_unix_ms: issued_at_ms
                .checked_add(self.epoch_duration_ms)
                .ok_or_else(invalid)?,
        };
        limits.validate()?;
        Ok(limits)
    }
}

fn invalid() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::InvalidInput,
        false,
        "invalid standing work funding policy",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentId, CompanyPrincipalKindV1, CompanyRoleV1, ADAPTIVE_LEADERSHIP_MAX_DURATION_MS,
        ADAPTIVE_RESUME_MAX_REVIEWS, ADAPTIVE_RESUME_MAX_WINDOW_MS, ADAPTIVE_SESSION_MAX_CALLS,
    };

    fn policy() -> StandingWorkFundingPolicyV1 {
        StandingWorkFundingPolicyV1 {
            schema_version: 1,
            policy_id: "finite-renewal".into(),
            generation: 1,
            tenant_id: TenantId::parse("tenant-a").unwrap(),
            issuer_principal_id: "operator-a".into(),
            epoch_duration_ms: 60_000,
            additional_reviews: 2,
            additional_windows: 1,
            max_window_ms: 30_000,
        }
    }

    // Shape fixtures only: no registered principal, receipt or execution is created.
    fn operator() -> AuthenticatedCompanyPrincipalV1 {
        AuthenticatedCompanyPrincipalV1 {
            schema_version: 1,
            tenant_id: TenantId::parse("tenant-a").unwrap(),
            principal_id: "operator-a".into(),
            kind: CompanyPrincipalKindV1::Operator,
            role: CompanyRoleV1::ProjectManager,
            customer_id: None,
            agent_id: None,
            authority_generation: 1,
            authority_digest: "a".repeat(64),
        }
    }

    #[test]
    fn strict_policy_roundtrip_and_legacy_limit_bytes() {
        let policy = policy();
        policy.validate_for(&operator()).unwrap();
        let bytes = serde_json::to_vec(&policy).unwrap();
        let decoded: StandingWorkFundingPolicyV1 = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded, policy);
        assert_eq!(serde_json::to_vec(&decoded).unwrap(), bytes);
        let limits = policy.limits(10_000, 1_000_000).unwrap();
        let legacy = br#"{"additional_model_calls":0,"additional_tool_calls":0,"additional_reviews":2,"additional_windows":1,"max_window_ms":30000,"max_call_duration_ms":10000,"dispatch_margin_ms":1000,"expires_at_unix_ms":1060000}"#;
        assert_eq!(serde_json::to_vec(&limits).unwrap(), legacy.as_slice());
        assert_eq!(
            serde_json::from_slice::<AdaptiveWorkFundingLimitsV1>(legacy).unwrap(),
            limits
        );
        for extra in [
            "additional_model_calls",
            "review_history_digest",
            "issued_at_ms",
        ] {
            let mut value = serde_json::to_value(&policy).unwrap();
            value[extra] = serde_json::json!(1);
            assert!(serde_json::from_value::<StandingWorkFundingPolicyV1>(value).is_err());
        }
        let mut value = serde_json::to_value(&policy).unwrap();
        value.as_object_mut().unwrap().remove("schema_version");
        assert!(serde_json::from_value::<StandingWorkFundingPolicyV1>(value).is_err());
        let raw = String::from_utf8(bytes).unwrap();
        let duplicate = raw.replacen("\"generation\":1", "\"generation\":1,\"generation\":2", 1);
        assert!(serde_json::from_str::<StandingWorkFundingPolicyV1>(&duplicate).is_err());
    }

    #[test]
    fn operation_key_has_exactly_policy_generation_tenant_session_and_head_inputs() {
        let policy = policy();
        let session = Uuid::from_u128(1);
        let head = "a".repeat(64);
        let expected = policy.operation_id(session, &head).unwrap();
        assert!(!expected.is_nil());
        assert_eq!(expected.as_bytes()[6] >> 4, 8);
        assert_eq!(expected.as_bytes()[8] >> 6, 2);
        assert_eq!(policy.operation_id(session, &head).unwrap(), expected);
        for changed in [
            StandingWorkFundingPolicyV1 {
                policy_id: "another-policy".into(),
                ..policy.clone()
            },
            StandingWorkFundingPolicyV1 {
                generation: 2,
                ..policy.clone()
            },
            StandingWorkFundingPolicyV1 {
                tenant_id: TenantId::parse("tenant-b").unwrap(),
                ..policy.clone()
            },
        ] {
            assert_ne!(changed.operation_id(session, &head).unwrap(), expected);
        }
        assert_ne!(
            policy.operation_id(Uuid::from_u128(2), &head).unwrap(),
            expected
        );
        assert_ne!(
            policy.operation_id(session, &"b".repeat(64)).unwrap(),
            expected
        );
        let changed = StandingWorkFundingPolicyV1 {
            issuer_principal_id: "another-operator".into(),
            epoch_duration_ms: 120_000,
            additional_reviews: 4,
            additional_windows: 4,
            max_window_ms: 60_000,
            ..policy.clone()
        };
        assert_eq!(changed.operation_id(session, &head).unwrap(), expected);
        // Refusal/history and time can change independently; neither enters the key API.
        assert_ne!(
            policy.reason_ref(Uuid::from_u128(3)).unwrap(),
            policy.reason_ref(Uuid::from_u128(4)).unwrap()
        );
        assert_ne!(
            policy.limits(10_000, 1_000).unwrap(),
            policy.limits(10_000, 2_000).unwrap()
        );
        assert_eq!(policy.operation_id(session, &head).unwrap(), expected);
        assert!(policy.operation_id(Uuid::nil(), &head).is_err());
        for invalid in ["", "A", &"A".repeat(64), &"a".repeat(63)] {
            assert!(policy.operation_id(session, invalid).is_err());
        }
    }

    #[test]
    fn reason_is_canonical_and_bounded_at_maximum_identifier_and_generation() {
        let policy = policy();
        let review = Uuid::from_u128(856);
        assert_eq!(
            policy.reason_ref(review).unwrap(),
            format!("standing-funding:finite-renewal:1:{review}")
        );
        let max = StandingWorkFundingPolicyV1 {
            policy_id: "x".repeat(40),
            generation: u64::MAX,
            ..policy
        };
        let reason = max.reason_ref(review).unwrap();
        assert_eq!(reason.len(), 115);
        validate_identifier(&reason).unwrap();
        assert!(max.reason_ref(Uuid::nil()).is_err());
    }

    #[test]
    fn issuer_consistency_requires_existing_operator_kind_role_tenant_and_identity() {
        let policy = policy();
        for role in [CompanyRoleV1::ProjectManager, CompanyRoleV1::TechnicalLead] {
            let mut issuer = operator();
            issuer.role = role;
            policy.validate_for(&issuer).unwrap();
        }
        for change in 0..7 {
            let mut issuer = operator();
            match change {
                0 => issuer.tenant_id = TenantId::parse("tenant-b").unwrap(),
                1 => issuer.principal_id = "operator-b".into(),
                2 => issuer.role = CompanyRoleV1::Developer,
                3 => {
                    issuer.kind = CompanyPrincipalKindV1::Agent;
                    issuer.agent_id = Some(AgentId(5));
                }
                4 => issuer.authority_generation = 0,
                5 => issuer.authority_digest.clear(),
                _ => issuer.schema_version = 2,
            }
            assert!(
                policy.validate_for(&issuer).is_err(),
                "issuer change {change}"
            );
        }
    }

    #[test]
    fn invalid_policy_shapes_fail_all_derivation_entrypoints() {
        let valid = policy();
        let mut invalid = Vec::new();
        for id in [
            "".to_owned(),
            "with:separator".into(),
            "two words".into(),
            "x".repeat(41),
            "\u{e9}".into(),
        ] {
            invalid.push(StandingWorkFundingPolicyV1 {
                policy_id: id,
                ..valid.clone()
            });
        }
        for change in 0..13 {
            let mut value = valid.clone();
            match change {
                0 => value.schema_version = 2,
                1 => value.generation = 0,
                2 => value.additional_reviews = 0,
                3 => value.additional_reviews = 5,
                4 => value.additional_windows = 0,
                5 => value.additional_windows = 5,
                6 => value.additional_windows = 3,
                7 => value.epoch_duration_ms = 1_999,
                8 => value.epoch_duration_ms = ADAPTIVE_RESUME_MAX_POLICY_MS + 1,
                9 => value.max_window_ms = ADAPTIVE_RESUME_MAX_WINDOW_MS + 1,
                10 => value.max_window_ms = 1_999,
                11 => value.issuer_principal_id.clear(),
                _ => value.tenant_id = TenantId("invalid:tenant".into()),
            }
            invalid.push(value);
        }
        for value in invalid {
            assert!(value.validate_for(&operator()).is_err());
            assert!(value
                .operation_id(Uuid::from_u128(1), &"a".repeat(64))
                .is_err());
            assert!(value.reason_ref(Uuid::from_u128(2)).is_err());
            assert!(value.limits(10_000, 1_000).is_err());
        }
    }

    #[test]
    fn limits_preserve_zero_additions_and_existing_dispatch_boundaries() {
        let minimum = StandingWorkFundingPolicyV1 {
            epoch_duration_ms: 2_000,
            max_window_ms: 2_000,
            additional_reviews: 1,
            additional_windows: 1,
            ..policy()
        };
        let limits = minimum.limits(1_000, 1).unwrap();
        assert_eq!(limits.additional_model_calls, 0);
        assert_eq!(limits.additional_tool_calls, 0);
        assert_eq!(limits.expires_at_unix_ms, 2_001);
        assert_eq!(
            limits.dispatch_margin_ms,
            ADAPTIVE_RESUME_DISPATCH_MARGIN_MS
        );
        assert!(minimum.limits(1_001, 1).is_err());
        assert!(minimum.limits(999, 1).is_err());
        assert!(minimum.limits(1_000, 0).is_err());
        let shorter_epoch = StandingWorkFundingPolicyV1 {
            epoch_duration_ms: 10_999,
            ..policy()
        };
        assert!(shorter_epoch.limits(10_000, 1).is_err());
        let exact_epoch = StandingWorkFundingPolicyV1 {
            epoch_duration_ms: 11_000,
            ..policy()
        };
        exact_epoch.limits(10_000, 1).unwrap();
        let max = StandingWorkFundingPolicyV1 {
            epoch_duration_ms: ADAPTIVE_RESUME_MAX_POLICY_MS,
            max_window_ms: ADAPTIVE_RESUME_MAX_WINDOW_MS,
            additional_reviews: 4,
            additional_windows: 4,
            ..policy()
        };
        max.limits(ADAPTIVE_LEADERSHIP_MAX_DURATION_MS, 1).unwrap();
        assert!(max
            .limits(ADAPTIVE_LEADERSHIP_MAX_DURATION_MS + 1, 1)
            .is_err());
    }

    #[test]
    fn expiry_and_dispatch_arithmetic_never_wrap_or_exceed_sqlite_time() {
        let policy = policy();
        assert!(policy.limits(10_000, u64::MAX).is_err());
        assert!(policy.limits(u64::MAX, 1).is_err());
        let latest = i64::MAX as u64 - policy.epoch_duration_ms;
        assert_eq!(
            policy.limits(10_000, latest).unwrap().expires_at_unix_ms,
            i64::MAX as u64
        );
        assert!(policy.limits(10_000, latest + 1).is_err());
    }

    #[test]
    fn legacy_funding_limits_keep_their_existing_64_call_and_128_review_bounds() {
        assert_eq!(ADAPTIVE_SESSION_MAX_CALLS, 64);
        assert_eq!(ADAPTIVE_RESUME_MAX_REVIEWS, 128);
        let mut legacy = policy().limits(10_000, 1_000_000).unwrap();
        legacy.additional_model_calls = ADAPTIVE_SESSION_MAX_CALLS;
        legacy.additional_tool_calls = ADAPTIVE_SESSION_MAX_CALLS;
        legacy.additional_reviews = ADAPTIVE_RESUME_MAX_REVIEWS;
        legacy.additional_windows = ADAPTIVE_RESUME_MAX_REVIEWS;
        legacy.validate().unwrap();
        for change in 0..3 {
            let mut invalid = legacy.clone();
            match change {
                0 => invalid.additional_model_calls += 1,
                1 => invalid.additional_tool_calls += 1,
                _ => invalid.additional_reviews += 1,
            }
            assert!(invalid.validate().is_err());
        }
    }
}
