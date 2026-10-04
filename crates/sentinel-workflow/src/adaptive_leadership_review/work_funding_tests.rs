use super::*;

fn rebind(call: &mut AdaptiveLeadershipReviewCallV1) {
    call.grant.evidence_fingerprint = adaptive_leadership_evidence_fingerprint(
        &call.context.tool_catalog, &call.context.evidence_refs,
    ).unwrap();
    call.grant.review_id = adaptive_leadership_review_id(
        call.grant.session_id, call.grant.expected_session_version, &call.grant.evidence_fingerprint,
    ).unwrap();
    call.review_key = call.grant.review_id.to_string();
}

fn funded() -> AdaptiveLeadershipReviewCallV1 {
    let mut call = fixture();
    let session = &call.context.source_session;
    let mut operator = call.grant.leadership_principal.clone();
    operator.kind = CompanyPrincipalKindV1::Operator;
    operator.agent_id = None;
    let operation_id = Uuid::from_u128(856);
    let receipt = crate::AdaptiveWorkFundingReceiptV1 {
        schema_version: 1,
        funding_id: crate::adaptive_work_funding_id(
            &operator.tenant_id, session.grant.session_id, operation_id,
        ).unwrap(),
        request: crate::AdaptiveWorkFundingRequestV1 {
            schema_version: 1, operation_id,
            source: crate::AdaptiveWorkFundingSourceV1 {
                resume_source: crate::AdaptiveResumeSourceV1 {
                    tenant_id: operator.tenant_id.clone(),
                    project_id: call.grant.project_id.clone(),
                    work_item_id: call.grant.work_item_id.clone(),
                    session_id: session.grant.session_id,
                    expected_project_version: call.grant.expected_project_version,
                    expected_session_version: session.version,
                    project_payload_digest: "a".repeat(64),
                    root_entry_digest: "b".repeat(64), head_entry_digest: "c".repeat(64),
                    continuation_history_digest: adaptive_budget_history_digest(&session.continuation).unwrap(),
                    review_history_digest: "d".repeat(64),
                    assignee_authority: session.grant.authority.clone(),
                    base_model_calls: session.model_calls, base_tool_calls: session.tool_calls,
                    base_review_count: 1,
                    base_window_count: session.continuation.as_ref().unwrap().authorizations.len() as u16,
                    subject: crate::AdaptiveResumeSubjectV1::ReadyForModel {
                        active_allowance_digest: budget(&call).active_allowance_digest.clone(),
                    },
                },
                original_model_call_ceiling: session.grant.max_model_calls,
                original_tool_call_ceiling: session.grant.max_tool_calls,
                current_model_call_ceiling: session.grant.max_model_calls,
                current_tool_call_ceiling: session.grant.max_tool_calls,
                predecessor_receipt_digest: None,
                supersedes_unused_receipt_digest: None,
            },
            limits: crate::AdaptiveWorkFundingLimitsV1 {
                additional_model_calls: 8, additional_tool_calls: 8,
                additional_reviews: 3, additional_windows: 3,
                max_window_ms: 180_000, max_call_duration_ms: session.grant.max_call_duration_ms,
                dispatch_margin_ms: crate::ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
                expires_at_unix_ms: NOW + 3_600_000,
            },
            reason_ref: "explicit-finite-funding".into(),
        },
        issuer_principal: operator, issued_at_unix_ms: call.grant_issued_at_unix_ms,
    };
    let epoch = crate::AdaptiveWorkFundingEpochV1 { binding: receipt.binding(2).unwrap(), receipt };
    call.context.evidence_refs.push(epoch.evidence_ref().unwrap());
    call.schema_version = 5;
    call.grant.schema_version = 5;
    call.grant.work_funding = Some(Box::new(epoch));
    rebind(&mut call);
    call
}

#[test]
fn funding_receipt_changes_review_identity_but_never_the_source_session() {
    let original = fixture();
    let call = funded();
    call.grant.validate(call.grant_issued_at_unix_ms).unwrap();
    call.context.validate(&call.grant).unwrap();
    assert_ne!(call.grant.review_id, original.grant.review_id);
    assert_eq!(call.context.source_session, original.context.source_session);
    assert!(call.context.source_session.active_work_funding().is_none());
    let remaining = call.grant.work_funding.as_ref().unwrap().binding.limits.total_model_call_ceiling
        - call.context.source_session.model_calls;
    assert!(remaining > original.context.source_session.grant.max_model_calls
        - original.context.source_session.model_calls);
    let issued = call.grant_issued_at_unix_ms + 1;
    assert_eq!(call.continuation_allowance(issued, issued + 180_000, remaining).unwrap().grant.max_calls, remaining);
    assert!(call.continuation_allowance(issued, issued + 180_000, remaining + 1).is_err());
    assert!(call.continuation_allowance(issued, issued + 120_000, 1).is_err());
}

#[test]
fn funding_reference_must_be_exact_and_bound_before_fingerprint_and_identity() {
    for mutation in 0..4 {
        let mut call = funded();
        let reference = call.grant.work_funding.as_ref().unwrap().evidence_ref().unwrap();
        call.context.evidence_refs.retain(|value| value != &reference);
        match mutation {
            0 => {}
            1 => call.context.evidence_refs.push(format!("{reference}-wrong")),
            2 => {
                call.context.evidence_refs.push(reference.clone());
                call.context.evidence_refs.push(format!("{reference}-extra"));
            }
            _ => call.context.evidence_refs.push("adaptive-resume-policy:other-epoch".into()),
        }
        rebind(&mut call);
        assert!(call.context.validate(&call.grant).is_err());
    }
    let mut call = fixture();
    let epoch = funded().grant.work_funding.unwrap();
    call.schema_version = 5;
    call.grant.schema_version = 5;
    call.context.evidence_refs.push(epoch.evidence_ref().unwrap());
    call.grant.work_funding = Some(epoch);
    assert!(call.context.validate(&call.grant).is_err());
}

#[test]
fn funding_rejects_old_schemas_unknown_subjects_and_mixed_extensions() {
    let baseline = funded();
    for schema in 1..=4 {
        let mut call = baseline.clone();
        call.grant.schema_version = schema;
        assert!(call.grant.validate(call.grant_issued_at_unix_ms).is_err());
        assert!(call.context.validate(&call.grant).is_err());
    }
    let mut call = baseline.clone();
    call.grant.work_funding = None;
    assert!(call.grant.validate(call.grant_issued_at_unix_ms).is_err());
    let mut call = baseline.clone();
    call.grant.resume_policy = Some(Box::new(crate::AdaptiveResumePolicyBindingV1 {
        schema_version: 1, policy_id: "other-policy".into(), receipt_digest: "a".repeat(64),
        ordinal: 2, limits: crate::AdaptiveResumePolicyLimitsV1 {
            total_review_ceiling: 4, total_window_ceiling: 4, max_window_ms: 180_000,
            max_call_duration_ms: 120_000, dispatch_margin_ms: 1_000,
            expires_at_unix_ms: NOW + 3_600_000,
        },
    }));
    assert!(call.grant.validate(call.grant_issued_at_unix_ms).is_err());
    let mut call = baseline.clone();
    call.grant.recovery_epoch = Some(crate::AdaptiveLeadershipRecoveryBindingV1 {
        schema_version: 2,
        epoch_key: crate::adaptive_leadership_recovery_epoch_key(
            &call.grant.leadership_principal.tenant_id, call.grant.session_id,
        ).unwrap(),
        epoch_digest: "b".repeat(64), review_id: call.grant.review_id,
        max_window_ms: 180_000, max_additional_model_calls: 1,
    });
    assert!(call.grant.validate(call.grant_issued_at_unix_ms).is_err());
    assert!(call.context.validate(&call.grant).is_err());
    for subject in [None, Some(AdaptiveLeadershipReviewSubjectV2::UnknownModel {
        effect: crate::adaptive::continuation_tests::effect(856),
        sealed_unknown_proof_digest: "a".repeat(64),
    }), Some(AdaptiveLeadershipReviewSubjectV2::BlockedContinuation {
        reason_code: "needs_review".into(), resolution_event_id: None,
    })] {
        let mut call = baseline.clone();
        call.grant.subject = subject;
        assert!(call.context.validate(&call.grant).is_err());
    }
}

#[test]
fn schema5_decisions_use_exact_epoch_bounds_and_exclude_old_decision_kinds() {
    let call = funded();
    let reference = call.grant.work_funding.as_ref().unwrap().evidence_ref().unwrap();
    let mut decision = AdaptiveLeadershipReviewDecisionV1 {
        schema_version: 5,
        decision: AdaptiveLeadershipReviewDecisionKindV1::Continue {
            additional_model_calls: 22, window_ms: 180_000,
            rationale: "Use the explicit funded ceiling".into(), evidence_refs: vec![reference.clone()],
        },
    };
    decision.validate(&call.context.evidence_refs).unwrap();
    decision.validate_subject(&call.grant).unwrap();
    if let AdaptiveLeadershipReviewDecisionKindV1::Continue { additional_model_calls, .. } = &mut decision.decision {
        *additional_model_calls = 23;
    }
    assert!(decision.validate_subject(&call.grant).is_err());
    decision.decision = AdaptiveLeadershipReviewDecisionKindV1::DeferBudget {
        rationale: "Preserve this refusal".into(), evidence_refs: vec![reference.clone()],
    };
    decision.validate(&call.context.evidence_refs).unwrap();
    decision.validate_subject(&call.grant).unwrap();
    decision.decision = AdaptiveLeadershipReviewDecisionKindV1::KeepUnknown {
        rationale: "Unsupported funded subject".into(), evidence_refs: vec![reference],
    };
    assert!(decision.validate(&call.context.evidence_refs).is_err());
    assert!(decision.validate_subject(&call.grant).is_err());
}

#[test]
fn legacy_grant_bytes_omit_the_new_field_and_roundtrip_unchanged() {
    let original = fixture();
    let bytes = serde_json::to_vec(&original.grant).unwrap();
    assert!(!String::from_utf8(bytes.clone()).unwrap().contains("work_funding"));
    let restored: AdaptiveLeadershipReviewGrantV1 = serde_json::from_slice(&bytes).unwrap();
    assert!(restored.work_funding.is_none());
    assert_eq!(serde_json::to_vec(&restored).unwrap(), bytes);
    restored.validate(original.grant_issued_at_unix_ms).unwrap();
    original.context.validate(&restored).unwrap();
}
