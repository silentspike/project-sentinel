use super::*;
use crate::{
    AdaptiveEffectV1, AgentId, CompanyPrincipalKindV1, CompanyRoleV1, PrincipalAuthorityV1,
    ProjectId, RuntimeAuthoritySnapshotV1, TenantId, WorkItemId,
    ADAPTIVE_LEADERSHIP_MAX_DURATION_MS, ADAPTIVE_RESUME_MAX_WINDOW_MS,
};
use std::collections::BTreeSet;

const NOW: u64 = 1_000_000;

fn request() -> AdaptiveWorkFundingRequestV1 {
    let tenant_id = TenantId::parse("tenant-a").unwrap();
    let project_id = ProjectId::parse("project-a").unwrap();
    let work_item_id = WorkItemId::parse("work-a").unwrap();
    let assignee_authority = RuntimeAuthoritySnapshotV1 {
        schema_version: 1,
        tenant_id: tenant_id.clone(),
        project_id: project_id.clone(),
        work_item_id: work_item_id.clone(),
        agent_id: AgentId(2),
        assignment_version: 1,
        assignment_digest: "a".repeat(64),
        organization_generation: 1,
        organization_digest: "b".repeat(64),
        principal: PrincipalAuthorityV1::derive("developer-a", 1, &[2; 32]).unwrap(),
        profile_id: "developer-profile".into(),
        profile_generation: 1,
        profile_digest: "c".repeat(64),
        runtime_key: "bwrap-coding-v1".into(),
        runtime_generation: 1,
        runtime_digest: "d".repeat(64),
        policy_generation: 1,
        policy_digest: "e".repeat(64),
        active: true,
        capabilities: BTreeSet::from(["file.inspect".into()]),
    };
    AdaptiveWorkFundingRequestV1 {
        schema_version: 1,
        operation_id: Uuid::from_u128(1),
        source: AdaptiveWorkFundingSourceV1 {
            resume_source: AdaptiveResumeSourceV1 {
                tenant_id,
                project_id,
                work_item_id,
                session_id: Uuid::from_u128(2),
                expected_project_version: 3,
                expected_session_version: 4,
                project_payload_digest: "a".repeat(64),
                root_entry_digest: "b".repeat(64),
                head_entry_digest: "c".repeat(64),
                continuation_history_digest: "d".repeat(64),
                review_history_digest: "e".repeat(64),
                assignee_authority,
                base_model_calls: 8,
                base_tool_calls: 6,
                base_review_count: 2,
                base_window_count: 1,
                subject: AdaptiveResumeSubjectV1::ReadyForModel {
                    active_allowance_digest: "f".repeat(64),
                },
            },
            original_model_call_ceiling: 10,
            original_tool_call_ceiling: 8,
            current_model_call_ceiling: 10,
            current_tool_call_ceiling: 8,
            predecessor_receipt_digest: None,
            supersedes_unused_receipt_digest: None,
        },
        limits: AdaptiveWorkFundingLimitsV1 {
            additional_model_calls: 2,
            additional_tool_calls: 3,
            additional_reviews: 3,
            additional_windows: 2,
            max_window_ms: ADAPTIVE_RESUME_MAX_WINDOW_MS,
            max_call_duration_ms: ADAPTIVE_LEADERSHIP_MAX_DURATION_MS,
            dispatch_margin_ms: ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
            expires_at_unix_ms: NOW + 3_600_000,
        },
        reason_ref: "operator:issue-856.work-funding".into(),
    }
}

fn successor_request() -> AdaptiveWorkFundingRequestV1 {
    let mut value = request();
    value.source.current_model_call_ceiling = 12;
    value.source.current_tool_call_ceiling = 11;
    value.source.predecessor_receipt_digest = Some("a".repeat(64));
    value.source.resume_source.base_model_calls = 11;
    value.source.resume_source.base_tool_calls = 9;
    value
}

#[test]
fn request_rejects_added_capacity_when_the_other_required_counter_is_already_spent() {
    for model in [true, false] {
        let mut value = request();
        if model {
            value.source.resume_source.base_model_calls = value.source.current_model_call_ceiling;
            value.limits.additional_model_calls = 0;
        } else {
            value.source.resume_source.base_tool_calls = value.source.current_tool_call_ceiling;
            value.limits.additional_tool_calls = 0;
        }
        assert!(value.validate_shape().is_err());
        assert!(value.validate_at(&operator(), NOW).is_err());
    }
}

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

fn receipt_for(request: AdaptiveWorkFundingRequestV1) -> AdaptiveWorkFundingReceiptV1 {
    let source = &request.source.resume_source;
    AdaptiveWorkFundingReceiptV1 {
        schema_version: 1,
        funding_id: adaptive_work_funding_id(
            &source.tenant_id,
            source.session_id,
            request.operation_id,
        )
        .unwrap(),
        request,
        issuer_principal: operator(),
        issued_at_unix_ms: NOW,
    }
}

fn assert_invalid_receipt(value: &AdaptiveWorkFundingReceiptV1) {
    assert!(value.validate().is_err());
    assert!(value.receipt_digest().is_err());
    assert!(value.resulting_model_call_ceiling().is_err());
    assert!(value.resulting_tool_call_ceiling().is_err());
    assert!(value.binding(3).is_err());
}

fn assert_invalid_shape(value: &AdaptiveWorkFundingRequestV1) {
    assert!(value.validate_shape().is_err());
    assert!(value.validate_at(&operator(), NOW).is_err());
    assert!(value.resulting_model_call_ceiling().is_err());
    assert!(value.resulting_tool_call_ceiling().is_err());
    assert!(value.canonical_digest().is_err());
}

#[test]
fn valid_bounded_request_round_trips_and_projects_requested_ceilings() {
    let value = request();
    value.source.validate().unwrap();
    value.limits.validate().unwrap();
    value.validate_shape().unwrap();
    value.validate_at(&operator(), NOW).unwrap();
    assert_eq!(value.resulting_model_call_ceiling().unwrap(), 12);
    assert_eq!(value.resulting_tool_call_ceiling().unwrap(), 11);
    let encoded = serde_json::to_string(&value).unwrap();
    let decoded: AdaptiveWorkFundingRequestV1 = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, value);
    assert_eq!(
        decoded.canonical_digest().unwrap(),
        value.canonical_digest().unwrap()
    );
}

#[test]
fn only_same_tenant_project_manager_or_technical_lead_operators_pass() {
    let value = request();
    for role in [CompanyRoleV1::ProjectManager, CompanyRoleV1::TechnicalLead] {
        let mut principal = operator();
        principal.role = role;
        value.validate_at(&principal, NOW).unwrap();
        principal.kind = CompanyPrincipalKindV1::Agent;
        principal.agent_id = Some(AgentId(1));
        principal.validate().unwrap();
        assert_eq!(
            value.validate_at(&principal, NOW).unwrap_err().code,
            WorkflowErrorCode::AuthorityConflict
        );
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
        assert_eq!(
            value.validate_at(&principal, NOW).unwrap_err().code,
            WorkflowErrorCode::AuthorityConflict
        );
    }
    let mut customer = operator();
    customer.kind = CompanyPrincipalKindV1::Customer;
    customer.role = CompanyRoleV1::Customer;
    customer.customer_id = Some("customer-a".into());
    customer.validate().unwrap();
    assert_eq!(
        value.validate_at(&customer, NOW).unwrap_err().code,
        WorkflowErrorCode::AuthorityConflict
    );
    let mut wrong_tenant = operator();
    wrong_tenant.tenant_id = TenantId::parse("tenant-b").unwrap();
    assert_eq!(
        value.validate_at(&wrong_tenant, NOW).unwrap_err().code,
        WorkflowErrorCode::AuthorityConflict
    );
    let mut malformed = operator();
    malformed.authority_generation = 0;
    assert!(value.validate_at(&malformed, NOW).is_err());
    value.validate_shape().unwrap();
    value.canonical_digest().unwrap();
}

#[test]
fn valid_model_unknown_source_is_not_a_work_funding_subject() {
    let mut value = request();
    value.source.resume_source.subject = AdaptiveResumeSubjectV1::ModelUnknown {
        effect: AdaptiveEffectV1 {
            id: Uuid::from_u128(3),
            request_digest: "a".repeat(64),
        },
        sealed_unknown_proof_digest: "b".repeat(64),
    };
    value.source.resume_source.validate().unwrap();
    assert_invalid_shape(&value);
}

#[test]
fn source_validation_and_original_ceilings_are_not_bypassed() {
    for ceiling in [0, ADAPTIVE_SESSION_MAX_CALLS + 1, u16::MAX] {
        let mut value = request();
        value.source.original_model_call_ceiling = ceiling;
        assert_invalid_shape(&value);
        let mut value = request();
        value.source.original_tool_call_ceiling = ceiling;
        assert_invalid_shape(&value);
    }
    let mut value = request();
    value.source.original_model_call_ceiling = value.source.resume_source.base_model_calls - 1;
    assert_invalid_shape(&value);
    let mut value = request();
    value.source.original_tool_call_ceiling = value.source.resume_source.base_tool_calls - 1;
    assert_invalid_shape(&value);
    let mut value = request();
    value.source.original_model_call_ceiling = value.source.resume_source.base_model_calls;
    value.source.original_tool_call_ceiling = value.source.resume_source.base_tool_calls;
    value.source.current_model_call_ceiling = value.source.original_model_call_ceiling;
    value.source.current_tool_call_ceiling = value.source.original_tool_call_ceiling;
    value.validate_shape().unwrap();
    let mut value = request();
    value.source.resume_source.base_model_calls = 0;
    value.source.resume_source.base_tool_calls = 0;
    value.source.original_model_call_ceiling = 1;
    value.source.original_tool_call_ceiling = 1;
    value.source.current_model_call_ceiling = 1;
    value.source.current_tool_call_ceiling = 1;
    value.validate_at(&operator(), NOW).unwrap();
    let mut value = request();
    value.source.resume_source.head_entry_digest = "not-a-digest".into();
    assert_invalid_shape(&value);
    let mut value = request();
    value.source.resume_source.expected_session_version = 0;
    assert_invalid_shape(&value);
    let mut value = request();
    value.source.resume_source.assignee_authority.tenant_id = TenantId::parse("tenant-b").unwrap();
    assert_invalid_shape(&value);
}

#[test]
fn raised_initial_current_ceilings_require_a_predecessor() {
    let mut value = request();
    value.source.current_model_call_ceiling += 1;
    assert_invalid_shape(&value);
    let mut value = request();
    value.source.current_tool_call_ceiling += 1;
    assert_invalid_shape(&value);
    let mut value = successor_request();
    value.source.predecessor_receipt_digest = None;
    assert_invalid_shape(&value);
    for model_only in [true, false] {
        let mut value = request();
        if model_only {
            value.source.current_model_call_ceiling += 1;
        } else {
            value.source.current_tool_call_ceiling += 1;
        }
        value.source.predecessor_receipt_digest = Some("a".repeat(64));
        value.validate_at(&operator(), NOW).unwrap();
    }
}

#[test]
fn predecessor_digest_must_be_sha256_and_bind_a_prior_addition_shape() {
    for digest in [
        String::new(),
        "a".repeat(63),
        "a".repeat(65),
        "A".repeat(64),
        "g".repeat(64),
        "not-a-receipt-digest".into(),
    ] {
        let mut value = successor_request();
        value.source.predecessor_receipt_digest = Some(digest);
        assert_invalid_shape(&value);
    }
    let mut value = request();
    value.source.predecessor_receipt_digest = Some("a".repeat(64));
    assert_invalid_shape(&value);
}

#[test]
fn successor_spend_can_exceed_original_without_changing_original_grants() {
    let value = successor_request();
    let before = value.clone();
    assert!(value.source.resume_source.base_model_calls > value.source.original_model_call_ceiling);
    assert!(value.source.resume_source.base_tool_calls > value.source.original_tool_call_ceiling);
    value.validate_at(&operator(), NOW).unwrap();
    assert_eq!(value.resulting_model_call_ceiling().unwrap(), 14);
    assert_eq!(value.resulting_tool_call_ceiling().unwrap(), 14);
    assert_eq!(value, before);
    assert_eq!(value.source.original_model_call_ceiling, 10);
    assert_eq!(value.source.original_tool_call_ceiling, 8);
    let encoded = serde_json::to_value(&value).unwrap();
    assert_eq!(
        encoded["source"]["predecessor_receipt_digest"],
        serde_json::json!("a".repeat(64))
    );
    let decoded: AdaptiveWorkFundingRequestV1 = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, value);
    assert_eq!(
        decoded.canonical_digest().unwrap(),
        value.canonical_digest().unwrap()
    );
    let mut exhausted = value;
    exhausted.source.resume_source.base_model_calls = exhausted.source.current_model_call_ceiling;
    exhausted.source.resume_source.base_tool_calls = exhausted.source.current_tool_call_ceiling;
    exhausted.validate_at(&operator(), NOW).unwrap();
}

#[test]
fn current_ceilings_must_cover_originals_and_spend_within_session_cap() {
    for ceiling in [0, 9, ADAPTIVE_SESSION_MAX_CALLS + 1, u16::MAX] {
        let mut value = successor_request();
        value.source.current_model_call_ceiling = ceiling;
        assert_invalid_shape(&value);
    }
    for ceiling in [0, 7, ADAPTIVE_SESSION_MAX_CALLS + 1, u16::MAX] {
        let mut value = successor_request();
        value.source.current_tool_call_ceiling = ceiling;
        assert_invalid_shape(&value);
    }
    let mut value = successor_request();
    value.source.original_model_call_ceiling = value.source.current_model_call_ceiling + 1;
    assert_invalid_shape(&value);
    let mut value = successor_request();
    value.source.original_tool_call_ceiling = value.source.current_tool_call_ceiling + 1;
    assert_invalid_shape(&value);
    let mut value = successor_request();
    value.source.resume_source.base_model_calls = value.source.current_model_call_ceiling + 1;
    assert_invalid_shape(&value);
    let mut value = successor_request();
    value.source.resume_source.base_tool_calls = value.source.current_tool_call_ceiling + 1;
    assert_invalid_shape(&value);
}

#[test]
fn successive_funding_uses_current_totals_and_rejects_cap_overflow() {
    let mut value = successor_request();
    value.source.current_model_call_ceiling = ADAPTIVE_SESSION_MAX_CALLS - 2;
    value.source.current_tool_call_ceiling = ADAPTIVE_SESSION_MAX_CALLS - 3;
    value.validate_at(&operator(), NOW).unwrap();
    assert_eq!(
        value.resulting_model_call_ceiling().unwrap(),
        ADAPTIVE_SESSION_MAX_CALLS
    );
    assert_eq!(
        value.resulting_tool_call_ceiling().unwrap(),
        ADAPTIVE_SESSION_MAX_CALLS
    );
    let mut model_overflow = value.clone();
    model_overflow.source.current_model_call_ceiling += 1;
    model_overflow.source.validate().unwrap();
    assert!(
        model_overflow.source.original_model_call_ceiling
            + model_overflow.limits.additional_model_calls
            <= ADAPTIVE_SESSION_MAX_CALLS
    );
    assert_invalid_shape(&model_overflow);
    let mut tool_overflow = value;
    tool_overflow.source.current_tool_call_ceiling += 1;
    tool_overflow.source.validate().unwrap();
    assert!(
        tool_overflow.source.original_tool_call_ceiling
            + tool_overflow.limits.additional_tool_calls
            <= ADAPTIVE_SESSION_MAX_CALLS
    );
    assert_invalid_shape(&tool_overflow);
    let mut value = successor_request();
    value.limits.additional_model_calls = u16::MAX;
    assert_invalid_shape(&value);
    let mut value = successor_request();
    value.limits.additional_tool_calls = u16::MAX;
    assert_invalid_shape(&value);
}

#[test]
fn absent_predecessor_defaults_to_none_and_is_omitted_from_initial_json() {
    let value = request();
    let mut encoded = serde_json::to_value(&value).unwrap();
    assert!(!encoded["source"]
        .as_object()
        .unwrap()
        .contains_key("predecessor_receipt_digest"));
    let decoded: AdaptiveWorkFundingRequestV1 = serde_json::from_value(encoded.clone()).unwrap();
    assert_eq!(decoded.source.predecessor_receipt_digest, None);
    decoded.validate_shape().unwrap();
    encoded["source"]["predecessor_receipt_digest"] = serde_json::Value::Null;
    let decoded: AdaptiveWorkFundingRequestV1 = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, value);
    assert_eq!(
        decoded.canonical_digest().unwrap(),
        value.canonical_digest().unwrap()
    );
    let mut encoded = serde_json::to_value(successor_request()).unwrap();
    encoded["source"]
        .as_object_mut()
        .unwrap()
        .remove("predecessor_receipt_digest");
    let decoded: AdaptiveWorkFundingRequestV1 = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded.source.predecessor_receipt_digest, None);
    assert_invalid_shape(&decoded);
}

#[test]
fn additive_call_totals_obey_cap_and_checked_arithmetic() {
    let mut value = request();
    value.limits.additional_model_calls =
        ADAPTIVE_SESSION_MAX_CALLS - value.source.current_model_call_ceiling;
    value.limits.additional_tool_calls =
        ADAPTIVE_SESSION_MAX_CALLS - value.source.current_tool_call_ceiling;
    value.validate_at(&operator(), NOW).unwrap();
    assert_eq!(
        value.resulting_model_call_ceiling().unwrap(),
        ADAPTIVE_SESSION_MAX_CALLS
    );
    assert_eq!(
        value.resulting_tool_call_ceiling().unwrap(),
        ADAPTIVE_SESSION_MAX_CALLS
    );
    let mut value = request();
    value.source.original_model_call_ceiling = ADAPTIVE_SESSION_MAX_CALLS;
    value.source.current_model_call_ceiling = ADAPTIVE_SESSION_MAX_CALLS;
    value.limits.additional_model_calls = 0;
    value.validate_at(&operator(), NOW).unwrap();
    let mut value = request();
    value.source.original_tool_call_ceiling = ADAPTIVE_SESSION_MAX_CALLS;
    value.source.current_tool_call_ceiling = ADAPTIVE_SESSION_MAX_CALLS;
    value.limits.additional_tool_calls = 0;
    value.validate_at(&operator(), NOW).unwrap();
    for additional in [55, ADAPTIVE_SESSION_MAX_CALLS, u16::MAX] {
        let mut value = request();
        value.limits.additional_model_calls = additional;
        assert_invalid_shape(&value);
    }
    for additional in [57, ADAPTIVE_SESSION_MAX_CALLS, u16::MAX] {
        let mut value = request();
        value.limits.additional_tool_calls = additional;
        assert_invalid_shape(&value);
    }
    assert!(checked_call_ceiling(u16::MAX, 1).is_err());
    assert!(checked_call_ceiling(1, u16::MAX).is_err());
}

#[test]
fn at_least_one_extra_model_or_tool_call_is_required() {
    let mut value = request();
    value.limits.additional_model_calls = 0;
    value.validate_at(&operator(), NOW).unwrap();
    value.limits.additional_tool_calls = 0;
    assert_invalid_shape(&value);
    value.limits.additional_model_calls = 1;
    value.validate_at(&operator(), NOW).unwrap();
    assert_eq!(
        value.resulting_tool_call_ceiling().unwrap(),
        value.source.current_tool_call_ceiling
    );
}

#[test]
fn reviews_and_windows_are_finite_additions_with_bounded_session_totals() {
    for reviews in [0, ADAPTIVE_RESUME_MAX_REVIEWS + 1, u16::MAX] {
        let mut value = request();
        value.limits.additional_reviews = reviews;
        assert_invalid_shape(&value);
    }
    for windows in [0, 4, ADAPTIVE_RESUME_MAX_REVIEWS + 1, u16::MAX] {
        let mut value = request();
        value.limits.additional_windows = windows;
        assert_invalid_shape(&value);
    }
    let mut value = request();
    value.limits.additional_reviews = 1;
    value.limits.additional_windows = 2;
    assert_invalid_shape(&value);
    value.limits.additional_windows = 1;
    value.validate_at(&operator(), NOW).unwrap();
    let mut value = request();
    value.source.resume_source.base_review_count = ADAPTIVE_RESUME_MAX_REVIEWS - 3;
    value.source.resume_source.base_window_count = ADAPTIVE_RESUME_MAX_REVIEWS - 2;
    assert_invalid_shape(&value);
    value.source.resume_source.base_window_count = ADAPTIVE_RESUME_MAX_REVIEWS - 3;
    value.limits.additional_windows = 3;
    value.validate_at(&operator(), NOW).unwrap();
    value.source.resume_source.base_review_count += 1;
    value.source.resume_source.validate().unwrap();
    assert_invalid_shape(&value);
    value.source.resume_source.base_review_count = u16::MAX;
    value.source.resume_source.base_window_count = u16::MAX;
    assert_invalid_shape(&value);
}

#[test]
fn funding_more_than_three_reviews_and_windows_is_valid_when_totals_fit() {
    let mut value = successor_request();
    value.limits.additional_reviews = 16;
    value.limits.additional_windows = 12;
    value.limits.validate().unwrap();
    value.validate_at(&operator(), NOW).unwrap();
    assert_eq!(value.resulting_model_call_ceiling().unwrap(), 14);
    assert_eq!(value.resulting_tool_call_ceiling().unwrap(), 14);
    assert_eq!(
        value.source.resume_source.base_review_count + value.limits.additional_reviews,
        18
    );
    assert_eq!(
        value.source.resume_source.base_window_count + value.limits.additional_windows,
        13
    );
}

#[test]
fn review_and_window_totals_allow_exact_128_and_reject_overflow() {
    for base in [0, 1, 2, ADAPTIVE_RESUME_MAX_REVIEWS - 1] {
        let mut value = request();
        value.source.resume_source.base_review_count = base;
        value.source.resume_source.base_window_count = base;
        value.limits.additional_reviews = ADAPTIVE_RESUME_MAX_REVIEWS - base;
        value.limits.additional_windows = ADAPTIVE_RESUME_MAX_REVIEWS - base;
        value.limits.validate().unwrap();
        value.validate_at(&operator(), NOW).unwrap();
        assert_eq!(
            value.source.resume_source.base_review_count + value.limits.additional_reviews,
            ADAPTIVE_RESUME_MAX_REVIEWS
        );
        assert_eq!(
            value.source.resume_source.base_window_count + value.limits.additional_windows,
            ADAPTIVE_RESUME_MAX_REVIEWS
        );
        let mut reviews_overflow = value.clone();
        reviews_overflow.limits.additional_reviews += 1;
        if base > 0 {
            reviews_overflow.limits.validate().unwrap();
        }
        assert_invalid_shape(&reviews_overflow);
        let mut windows_exceed_reviews = value.clone();
        windows_exceed_reviews.limits.additional_windows += 1;
        assert_invalid_shape(&windows_exceed_reviews);
        let mut both_overflow = value;
        both_overflow.limits.additional_reviews += 1;
        both_overflow.limits.additional_windows += 1;
        if base > 0 {
            both_overflow.limits.validate().unwrap();
        }
        assert_invalid_shape(&both_overflow);
    }
}

#[test]
fn expiry_requires_a_positive_clock_bounded_lifetime_and_productive_slack() {
    let valid = request();
    assert!(valid.validate_at(&operator(), 0).is_err());
    for expiry in [0, i64::MAX as u64 + 1, u64::MAX] {
        let mut value = valid.clone();
        value.limits.expires_at_unix_ms = expiry;
        assert_invalid_shape(&value);
    }
    for expiry in [
        NOW - 1,
        NOW,
        NOW + 999,
        NOW + 1_000,
        NOW + ADAPTIVE_RESUME_MAX_POLICY_MS + 1,
    ] {
        let mut value = valid.clone();
        value.limits.expires_at_unix_ms = expiry;
        value.validate_shape().unwrap();
        value.canonical_digest().unwrap();
        value.resulting_model_call_ceiling().unwrap();
        value.resulting_tool_call_ceiling().unwrap();
        assert!(value.validate_at(&operator(), NOW).is_err());
    }
    let mut value = valid;
    value.limits.expires_at_unix_ms = NOW + ADAPTIVE_RESUME_MAX_POLICY_MS;
    value.validate_at(&operator(), NOW).unwrap();
    value.limits.expires_at_unix_ms =
        NOW + value.limits.max_call_duration_ms + value.limits.dispatch_margin_ms;
    value.validate_at(&operator(), NOW).unwrap();
    value.limits.expires_at_unix_ms -= 1;
    assert!(value.validate_at(&operator(), NOW).is_err());
    value.limits.max_call_duration_ms = 1_000;
    value.limits.expires_at_unix_ms = NOW + 2_000;
    value.validate_at(&operator(), NOW).unwrap();
    value.limits.expires_at_unix_ms -= 1;
    assert!(value.validate_at(&operator(), NOW).is_err());
    value.limits.expires_at_unix_ms = i64::MAX as u64;
    value.validate_shape().unwrap();
    assert!(value.validate_at(&operator(), u64::MAX).is_err());
    assert!(value.validate_at(&operator(), u64::MAX - 500).is_err());
    assert!(value.validate_at(&operator(), u64::MAX - 1_500).is_err());
    value
        .validate_at(&operator(), i64::MAX as u64 - 2_000)
        .unwrap();
}

#[test]
fn durations_windows_and_exact_dispatch_margin_are_required() {
    for duration in [0, 999, ADAPTIVE_LEADERSHIP_MAX_DURATION_MS + 1, u64::MAX] {
        let mut value = request();
        value.limits.max_call_duration_ms = duration;
        assert_invalid_shape(&value);
    }
    for margin in [
        0,
        ADAPTIVE_RESUME_DISPATCH_MARGIN_MS - 1,
        ADAPTIVE_RESUME_DISPATCH_MARGIN_MS + 1,
        u64::MAX,
    ] {
        let mut value = request();
        value.limits.dispatch_margin_ms = margin;
        assert_invalid_shape(&value);
    }
    for window in [0, 1_000, ADAPTIVE_RESUME_MAX_WINDOW_MS + 1, u64::MAX] {
        let mut value = request();
        value.limits.max_window_ms = window;
        assert_invalid_shape(&value);
    }
    for duration in [1_000, ADAPTIVE_LEADERSHIP_MAX_DURATION_MS] {
        let mut value = request();
        value.limits.max_call_duration_ms = duration;
        value.limits.max_window_ms = duration + ADAPTIVE_RESUME_DISPATCH_MARGIN_MS;
        value.validate_at(&operator(), NOW).unwrap();
        value.limits.max_window_ms -= 1;
        assert_invalid_shape(&value);
    }
}

#[test]
fn operation_schema_and_reason_identifier_are_strict() {
    for schema in [0, 2, u16::MAX] {
        let mut value = request();
        value.schema_version = schema;
        assert_invalid_shape(&value);
    }
    let mut value = request();
    value.operation_id = Uuid::nil();
    assert_invalid_shape(&value);
    for reason in ["", "free text", "bad/ref", "line\nref", "\u{00e4}"] {
        let mut value = request();
        value.reason_ref = reason.into();
        assert_invalid_shape(&value);
    }
    let mut value = request();
    value.reason_ref = "x".repeat(128);
    value.validate_shape().unwrap();
    value.reason_ref.push('x');
    assert_invalid_shape(&value);
}

#[test]
fn canonical_digest_is_stable_tagged_and_binds_request_changes() {
    let value = successor_request();
    let digest = value.canonical_digest().unwrap();
    assert_eq!(digest.len(), 64);
    assert_eq!(digest, value.canonical_digest().unwrap());
    assert_eq!(
        digest,
        canonical_sha256("sentinel.workflow.adaptive-work-funding-request.v1", &value).unwrap()
    );
    assert_ne!(
        digest,
        canonical_sha256(
            "sentinel.workflow.adaptive-resume-policy-request.v1",
            &value
        )
        .unwrap()
    );
    for (path, replacement) in [
        ("/operation_id", serde_json::json!(Uuid::from_u128(4))),
        ("/reason_ref", serde_json::json!("operator:changed")),
        ("/source/original_model_call_ceiling", serde_json::json!(11)),
        ("/source/original_tool_call_ceiling", serde_json::json!(9)),
        ("/source/current_model_call_ceiling", serde_json::json!(13)),
        ("/source/current_tool_call_ceiling", serde_json::json!(12)),
        (
            "/source/predecessor_receipt_digest",
            serde_json::json!("b".repeat(64)),
        ),
        (
            "/source/resume_source/session_id",
            serde_json::json!(Uuid::from_u128(5)),
        ),
        (
            "/source/resume_source/expected_session_version",
            serde_json::json!(5),
        ),
        (
            "/source/resume_source/head_entry_digest",
            serde_json::json!("f".repeat(64)),
        ),
        (
            "/source/resume_source/base_model_calls",
            serde_json::json!(9),
        ),
        (
            "/source/resume_source/base_tool_calls",
            serde_json::json!(7),
        ),
        (
            "/source/resume_source/base_review_count",
            serde_json::json!(3),
        ),
        (
            "/source/resume_source/base_window_count",
            serde_json::json!(2),
        ),
        (
            "/source/resume_source/subject/active_allowance_digest",
            serde_json::json!("a".repeat(64)),
        ),
        ("/limits/additional_model_calls", serde_json::json!(3)),
        ("/limits/additional_tool_calls", serde_json::json!(4)),
        ("/limits/additional_reviews", serde_json::json!(2)),
        ("/limits/additional_windows", serde_json::json!(3)),
        ("/limits/max_window_ms", serde_json::json!(299_999)),
        ("/limits/max_call_duration_ms", serde_json::json!(119_999)),
        (
            "/limits/expires_at_unix_ms",
            serde_json::json!(NOW + 3_600_001),
        ),
    ] {
        let mut encoded = serde_json::to_value(&value).unwrap();
        *encoded.pointer_mut(path).unwrap() = replacement;
        let changed: AdaptiveWorkFundingRequestV1 = serde_json::from_value(encoded).unwrap();
        changed.validate_shape().unwrap();
        assert_ne!(digest, changed.canonical_digest().unwrap(), "{path}");
    }
}

#[test]
fn unknown_json_fields_and_missing_required_fields_are_rejected() {
    let valid = request();
    for path in [
        "",
        "/source",
        "/source/resume_source",
        "/source/resume_source/subject",
        "/source/resume_source/assignee_authority",
        "/limits",
    ] {
        let mut encoded = serde_json::to_value(&valid).unwrap();
        encoded
            .pointer_mut(path)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unexpected".into(), serde_json::json!(true));
        assert!(
            serde_json::from_value::<AdaptiveWorkFundingRequestV1>(encoded).is_err(),
            "{path}"
        );
    }
    for (path, field) in [
        ("", "schema_version"),
        ("", "operation_id"),
        ("", "reason_ref"),
        ("/source", "resume_source"),
        ("/source", "original_model_call_ceiling"),
        ("/source", "original_tool_call_ceiling"),
        ("/source", "current_model_call_ceiling"),
        ("/source", "current_tool_call_ceiling"),
        ("/limits", "additional_model_calls"),
        ("/limits", "additional_tool_calls"),
        ("/limits", "additional_reviews"),
        ("/limits", "additional_windows"),
        ("/limits", "max_window_ms"),
        ("/limits", "max_call_duration_ms"),
        ("/limits", "dispatch_margin_ms"),
        ("/limits", "expires_at_unix_ms"),
    ] {
        let mut encoded = serde_json::to_value(&valid).unwrap();
        encoded
            .pointer_mut(path)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(
            serde_json::from_value::<AdaptiveWorkFundingRequestV1>(encoded).is_err(),
            "{path}/{field}"
        );
    }
}

#[test]
fn proposed_epoch_receipt_and_binding_have_exact_checked_totals() {
    let value = receipt_for(successor_request());
    value.validate().unwrap();
    assert_eq!(value.resulting_model_call_ceiling().unwrap(), 14);
    assert_eq!(value.resulting_tool_call_ceiling().unwrap(), 14);
    let binding = value.binding(3).unwrap();
    binding.validate().unwrap();
    value.validate_binding(&binding).unwrap();
    assert_eq!(binding.funding_id, value.funding_id);
    assert_eq!(binding.receipt_digest, value.receipt_digest().unwrap());
    assert_eq!(binding.limits.total_model_call_ceiling, 14);
    assert_eq!(binding.limits.total_tool_call_ceiling, 14);
    assert_eq!(binding.limits.total_review_ceiling, 5);
    assert_eq!(binding.limits.total_window_ceiling, 3);
    assert_eq!(
        binding.limits.max_window_ms,
        value.request.limits.max_window_ms
    );
    assert_eq!(
        binding.limits.max_call_duration_ms,
        value.request.limits.max_call_duration_ms
    );
    assert_eq!(
        binding.limits.dispatch_margin_ms,
        value.request.limits.dispatch_margin_ms
    );
    assert_eq!(
        binding.limits.expires_at_unix_ms,
        value.request.limits.expires_at_unix_ms
    );
    let encoded = serde_json::to_value(&value).unwrap();
    let decoded: AdaptiveWorkFundingReceiptV1 = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, value);
    assert_eq!(
        decoded.receipt_digest().unwrap(),
        value.receipt_digest().unwrap()
    );
    let encoded = serde_json::to_value(&binding).unwrap();
    assert!(encoded.get("funding_id").is_some());
    assert!(encoded.get("policy_id").is_none());
    assert!(
        serde_json::from_value::<crate::AdaptiveResumePolicyBindingV1>(encoded.clone()).is_err()
    );
    let decoded: AdaptiveWorkFundingBindingV1 = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, binding);
    value.validate_binding(&decoded).unwrap();
}

#[test]
fn funding_id_is_keyed_by_tenant_session_and_operation_not_latest_session() {
    let tenant = TenantId::parse("tenant-a").unwrap();
    let session = Uuid::from_u128(2);
    let operation = Uuid::from_u128(1);
    let id = adaptive_work_funding_id(&tenant, session, operation).unwrap();
    validate_identifier(&id).unwrap();
    assert_eq!(
        id,
        adaptive_work_funding_id(&tenant, session, operation).unwrap()
    );
    assert_eq!(
        id,
        format!(
            "work-funding-{}",
            canonical_sha256(
                "sentinel.workflow.adaptive-work-funding-id.v1",
                &(&tenant, session, operation)
            )
            .unwrap()
        )
    );
    let ids = BTreeSet::from([
        id,
        adaptive_work_funding_id(&tenant, session, Uuid::from_u128(3)).unwrap(),
        adaptive_work_funding_id(&tenant, Uuid::from_u128(4), operation).unwrap(),
        adaptive_work_funding_id(&TenantId::parse("tenant-b").unwrap(), session, operation)
            .unwrap(),
    ]);
    assert_eq!(ids.len(), 4);
    assert!(adaptive_work_funding_id(&tenant, Uuid::nil(), operation).is_err());
    assert!(adaptive_work_funding_id(&tenant, session, Uuid::nil()).is_err());
    assert!(adaptive_work_funding_id(&TenantId("Invalid".into()), session, operation).is_err());
}

#[test]
fn malformed_receipt_schema_id_and_request_are_rejected_by_all_helpers() {
    for schema in [0, 2, u16::MAX] {
        let mut value = receipt_for(request());
        value.schema_version = schema;
        assert_invalid_receipt(&value);
    }
    for id in ["", "free text", "work-funding-other"] {
        let mut value = receipt_for(request());
        value.funding_id = id.into();
        assert_invalid_receipt(&value);
    }
    let mut value = receipt_for(request());
    value.request.operation_id = Uuid::from_u128(3);
    value.request.validate_shape().unwrap();
    assert_invalid_receipt(&value);
    let mut value = receipt_for(request());
    value.request.limits.additional_model_calls = ADAPTIVE_SESSION_MAX_CALLS;
    assert_invalid_receipt(&value);
    let mut value = receipt_for(request());
    value.request.source.resume_source.session_id = Uuid::nil();
    assert_invalid_receipt(&value);
}

#[test]
fn proposed_receipt_issuer_claims_must_satisfy_existing_operator_policy() {
    let value = receipt_for(request());
    for role in [CompanyRoleV1::ProjectManager, CompanyRoleV1::TechnicalLead] {
        let mut changed = value.clone();
        changed.issuer_principal.role = role;
        changed.validate().unwrap();
    }
    let mut changed = value.clone();
    changed.issuer_principal.kind = CompanyPrincipalKindV1::Agent;
    changed.issuer_principal.agent_id = Some(AgentId(1));
    changed.issuer_principal.validate().unwrap();
    assert_invalid_receipt(&changed);
    let mut changed = value.clone();
    changed.issuer_principal.role = CompanyRoleV1::Developer;
    changed.issuer_principal.validate().unwrap();
    assert_invalid_receipt(&changed);
    let mut changed = value.clone();
    changed.issuer_principal.tenant_id = TenantId::parse("tenant-b").unwrap();
    assert_invalid_receipt(&changed);
    let mut changed = value.clone();
    changed.issuer_principal.authority_generation = 0;
    assert_invalid_receipt(&changed);
    let mut changed = value.clone();
    changed.issuer_principal.authority_digest = "not-a-digest".into();
    assert_invalid_receipt(&changed);
    let mut changed = value;
    changed.issuer_principal.schema_version = 2;
    assert_invalid_receipt(&changed);
}

#[test]
fn expired_historical_receipt_remains_valid_only_if_valid_at_issuance() {
    let value = receipt_for(request());
    let binding = value.binding(3).unwrap();
    let digest = value.receipt_digest().unwrap();
    let after_expiry = value.request.limits.expires_at_unix_ms + 1;
    assert!(value
        .request
        .validate_at(&value.issuer_principal, after_expiry)
        .is_err());
    value.validate().unwrap();
    value.validate_binding(&binding).unwrap();
    assert_eq!(value.receipt_digest().unwrap(), digest);
    assert_eq!(value.resulting_model_call_ceiling().unwrap(), 12);
    for issuance in [0, value.request.limits.expires_at_unix_ms, after_expiry] {
        let mut changed = value.clone();
        changed.issued_at_unix_ms = issuance;
        assert_invalid_receipt(&changed);
    }
    let mut changed = value;
    changed.issued_at_unix_ms = changed.request.limits.expires_at_unix_ms
        - changed.request.limits.max_call_duration_ms
        - changed.request.limits.dispatch_margin_ms;
    changed.validate().unwrap();
    changed.issued_at_unix_ms += 1;
    assert_invalid_receipt(&changed);
}

#[test]
fn funding_binding_global_ordinal_is_after_base_and_within_total_reviews() {
    let value = receipt_for(request());
    for ordinal in [3, 4, 5] {
        value
            .validate_binding(&value.binding(ordinal).unwrap())
            .unwrap();
    }
    for ordinal in [0, 1, 2, 6, ADAPTIVE_RESUME_MAX_REVIEWS + 1, u16::MAX] {
        assert!(value.binding(ordinal).is_err());
    }
    let mut stale = value.binding(3).unwrap();
    stale.ordinal = value.request.source.resume_source.base_review_count;
    stale.validate().unwrap();
    assert!(value.validate_binding(&stale).is_err());
    let mut request = request();
    request.source.resume_source.base_review_count = ADAPTIVE_RESUME_MAX_REVIEWS - 1;
    request.limits.additional_reviews = 1;
    request.limits.additional_windows = 1;
    let value = receipt_for(request);
    value.binding(ADAPTIVE_RESUME_MAX_REVIEWS).unwrap();
    assert!(value.binding(ADAPTIVE_RESUME_MAX_REVIEWS - 1).is_err());
    assert!(value.binding(ADAPTIVE_RESUME_MAX_REVIEWS + 1).is_err());
}

#[test]
fn binding_rejects_receipt_substitution_and_every_changed_limit() {
    let value = receipt_for(successor_request());
    let binding = value.binding(3).unwrap();
    let mut another = value.clone();
    another.request.operation_id = Uuid::from_u128(3);
    another.funding_id = adaptive_work_funding_id(
        &another.request.source.resume_source.tenant_id,
        another.request.source.resume_source.session_id,
        another.request.operation_id,
    )
    .unwrap();
    another.validate().unwrap();
    assert!(another.validate_binding(&binding).is_err());
    for (path, replacement) in [
        ("/funding_id", serde_json::json!(another.funding_id.clone())),
        (
            "/receipt_digest",
            serde_json::json!(another.receipt_digest().unwrap()),
        ),
        ("/limits/total_model_call_ceiling", serde_json::json!(15)),
        ("/limits/total_tool_call_ceiling", serde_json::json!(15)),
        ("/limits/total_review_ceiling", serde_json::json!(6)),
        ("/limits/total_window_ceiling", serde_json::json!(4)),
        ("/limits/max_window_ms", serde_json::json!(299_999)),
        ("/limits/max_call_duration_ms", serde_json::json!(119_999)),
        ("/limits/dispatch_margin_ms", serde_json::json!(1_001)),
        (
            "/limits/expires_at_unix_ms",
            serde_json::json!(NOW + 3_600_001),
        ),
    ] {
        let mut encoded = serde_json::to_value(&binding).unwrap();
        *encoded.pointer_mut(path).unwrap() = replacement;
        let changed: AdaptiveWorkFundingBindingV1 = serde_json::from_value(encoded).unwrap();
        assert!(value.validate_binding(&changed).is_err(), "{path}");
    }
    let mut changed_receipt = value.clone();
    changed_receipt.request.reason_ref = "operator:changed".into();
    changed_receipt.validate().unwrap();
    assert_eq!(changed_receipt.funding_id, value.funding_id);
    assert_ne!(
        changed_receipt.receipt_digest().unwrap(),
        value.receipt_digest().unwrap()
    );
    assert!(changed_receipt.validate_binding(&binding).is_err());
    let mut changed_receipt = value;
    changed_receipt.issuer_principal.authority_generation += 1;
    changed_receipt.validate().unwrap();
    assert!(changed_receipt.validate_binding(&binding).is_err());
}

#[test]
fn funding_receipt_and_binding_digests_use_separate_domains() {
    let value = receipt_for(request());
    let binding = value.binding(3).unwrap();
    assert_eq!(
        value.receipt_digest().unwrap(),
        canonical_sha256("sentinel.workflow.adaptive-work-funding-receipt.v1", &value).unwrap()
    );
    assert_eq!(
        binding.canonical_digest().unwrap(),
        canonical_sha256(
            "sentinel.workflow.adaptive-work-funding-binding.v1",
            &binding
        )
        .unwrap()
    );
    assert_ne!(
        value.receipt_digest().unwrap(),
        canonical_sha256(
            "sentinel.workflow.adaptive-resume-policy-receipt.v1",
            &value
        )
        .unwrap()
    );
    let mut changed = binding.clone();
    changed.ordinal += 1;
    value.validate_binding(&changed).unwrap();
    assert_ne!(
        changed.canonical_digest().unwrap(),
        binding.canonical_digest().unwrap()
    );
}

#[test]
fn new_funding_receipt_and_binding_json_are_strict_and_versioned() {
    let value = receipt_for(request());
    let binding = value.binding(3).unwrap();
    for schema in [0, 2, u16::MAX] {
        let mut changed = binding.clone();
        changed.schema_version = schema;
        assert!(changed.validate().is_err());
        assert!(changed.canonical_digest().is_err());
        assert!(value.validate_binding(&changed).is_err());
    }
    for id in ["", "free text"] {
        let mut changed = binding.clone();
        changed.funding_id = id.into();
        assert!(changed.validate().is_err());
        assert!(value.validate_binding(&changed).is_err());
    }
    let mut changed = binding.clone();
    changed.receipt_digest = "not-a-digest".into();
    assert!(changed.validate().is_err());
    assert!(changed.canonical_digest().is_err());
    for ceiling in [0, ADAPTIVE_SESSION_MAX_CALLS + 1, u16::MAX] {
        let mut changed = binding.clone();
        changed.limits.total_model_call_ceiling = ceiling;
        assert!(changed.validate().is_err());
        let mut changed = binding.clone();
        changed.limits.total_tool_call_ceiling = ceiling;
        assert!(changed.validate().is_err());
    }
    for path in ["", "/issuer_principal", "/request"] {
        let mut encoded = serde_json::to_value(&value).unwrap();
        encoded
            .pointer_mut(path)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unexpected".into(), serde_json::json!(true));
        assert!(serde_json::from_value::<AdaptiveWorkFundingReceiptV1>(encoded).is_err());
    }
    for path in ["", "/limits"] {
        let mut encoded = serde_json::to_value(&binding).unwrap();
        encoded
            .pointer_mut(path)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unexpected".into(), serde_json::json!(true));
        assert!(serde_json::from_value::<AdaptiveWorkFundingBindingV1>(encoded).is_err());
    }
    for field in [
        "schema_version",
        "funding_id",
        "request",
        "issuer_principal",
        "issued_at_unix_ms",
    ] {
        let mut encoded = serde_json::to_value(&value).unwrap();
        encoded.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<AdaptiveWorkFundingReceiptV1>(encoded).is_err());
    }
    for field in [
        "schema_version",
        "funding_id",
        "receipt_digest",
        "ordinal",
        "limits",
    ] {
        let mut encoded = serde_json::to_value(&binding).unwrap();
        encoded.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<AdaptiveWorkFundingBindingV1>(encoded).is_err());
    }
    let mut encoded = serde_json::to_value(&binding).unwrap();
    encoded["limits"]
        .as_object_mut()
        .unwrap()
        .remove("total_model_call_ceiling");
    assert!(serde_json::from_value::<AdaptiveWorkFundingBindingV1>(encoded).is_err());
}

#[test]
fn legacy_resume_policy_request_wire_and_funding_request_remain_unchanged() {
    let request = request();
    let before = serde_json::to_value(&request).unwrap();
    let digest = request.canonical_digest().unwrap();
    let value = receipt_for(request.clone());
    value.binding(3).unwrap();
    assert_eq!(serde_json::to_value(&value.request).unwrap(), before);
    assert_eq!(value.request.canonical_digest().unwrap(), digest);
    assert!(before.get("issuer_principal").is_none());
    assert!(before.get("funding_id").is_none());
    let legacy = crate::AdaptiveResumePolicyRequestV1 {
        schema_version: 1,
        operation_id: request.operation_id,
        source: request.source.resume_source.clone(),
        limits: AdaptiveResumePolicyLimitsV1 {
            total_review_ceiling: 5,
            total_window_ceiling: 3,
            max_window_ms: request.limits.max_window_ms,
            max_call_duration_ms: request.limits.max_call_duration_ms,
            dispatch_margin_ms: request.limits.dispatch_margin_ms,
            expires_at_unix_ms: request.limits.expires_at_unix_ms,
        },
        reason_ref: request.reason_ref.clone(),
    };
    legacy.validate_at(&operator(), NOW).unwrap();
    let encoded = serde_json::to_value(&legacy).unwrap();
    assert!(encoded["source"].get("resume_source").is_none());
    assert!(encoded["source"].get("tenant_id").is_some());
    assert!(encoded["source"]
        .get("current_model_call_ceiling")
        .is_none());
    let decoded: crate::AdaptiveResumePolicyRequestV1 =
        serde_json::from_value(encoded.clone()).unwrap();
    assert_eq!(decoded, legacy);
    assert!(serde_json::from_value::<AdaptiveWorkFundingRequestV1>(encoded).is_err());
}
