use super::*;

fn limits() -> AdaptiveResumePolicyLimitsV1 {
    AdaptiveResumePolicyLimitsV1 {
        total_review_ceiling: 4,
        total_window_ceiling: 4,
        max_window_ms: ADAPTIVE_RESUME_MAX_WINDOW_MS,
        max_call_duration_ms: ADAPTIVE_LEADERSHIP_MAX_DURATION_MS,
        dispatch_margin_ms: ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
        expires_at_unix_ms: 1_000_000,
    }
}

#[test]
fn limits_require_productive_bounded_window_and_sqlite_timestamp() {
    let valid = limits();
    valid.validate().unwrap();
    let mut short = valid.clone();
    short.max_window_ms = short.max_call_duration_ms + short.dispatch_margin_ms - 1;
    assert!(short.validate().is_err());
    short.max_window_ms += 1;
    short.validate().unwrap();
    for duration in [0, 999, ADAPTIVE_LEADERSHIP_MAX_DURATION_MS + 1] {
        let mut invalid = valid.clone();
        invalid.max_call_duration_ms = duration;
        assert!(invalid.validate().is_err());
    }
    let mut overflow = valid.clone();
    overflow.dispatch_margin_ms = u64::MAX;
    assert!(overflow.validate().is_err());
    let mut expiry = valid.clone();
    expiry.expires_at_unix_ms = i64::MAX as u64;
    expiry.validate().unwrap();
    expiry.expires_at_unix_ms += 1;
    assert!(expiry.validate().is_err());
    let mut windows = valid;
    windows.total_window_ceiling += 1;
    assert!(windows.validate().is_err());
}

#[test]
fn membership_is_strict_and_versioned() {
    let membership = AdaptiveResumeReviewMembershipV1 {
        schema_version: 1,
        policy_id: "resume-policy-test".into(),
        receipt_digest: "a".repeat(64),
        ordinal: 1,
        review_id: Uuid::new_v4(),
        operation_id: Uuid::new_v4(),
        grant_digest: "b".repeat(64),
        context_digest: "c".repeat(64),
    };
    membership.validate().unwrap();
    let mut wrong = membership.clone();
    wrong.schema_version = 2;
    assert!(wrong.validate().is_err());
    let mut value = serde_json::to_value(&membership).unwrap();
    value.as_object_mut().unwrap().remove("schema_version");
    assert!(serde_json::from_value::<AdaptiveResumeReviewMembershipV1>(value).is_err());
    let mut value = serde_json::to_value(&membership).unwrap();
    value["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<AdaptiveResumeReviewMembershipV1>(value).is_err());
}

#[test]
fn reason_reference_uses_identifier_not_free_text_contract() {
    validate_identifier(&"x".repeat(128)).unwrap();
    assert!(validate_identifier(&"x".repeat(129)).is_err());
    assert!(validate_identifier("free text reason").is_err());
    validate_identifier("operator:issue-856.resume").unwrap();
}
