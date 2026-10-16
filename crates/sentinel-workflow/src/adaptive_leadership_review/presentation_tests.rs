use super::*;

#[derive(Serialize)]
struct LegacyContext<'a> {
    source_project: &'a ProjectV1,
    source_session: &'a AdaptiveSessionV1,
    tool_catalog: &'a Value,
    evidence_refs: &'a [String],
}

fn legacy_context(context: &AdaptiveLeadershipReviewContextV1) -> LegacyContext<'_> {
    LegacyContext {
        source_project: &context.source_project,
        source_session: &context.source_session,
        tool_catalog: &context.tool_catalog,
        evidence_refs: &context.evidence_refs,
    }
}

// This exercises schema-6 shape validation, not store membership or NoIO authority.
fn funded_recovery() -> AdaptiveLeadershipReviewCallV1 {
    let mut call = funded();
    let epoch = call.grant.work_funding.as_ref().unwrap();
    let recovery = crate::AdaptiveFundedReviewRecoveryBindingV1 {
        schema_version: 1,
        receipt_key: "funded-presentation-recovery-fixture".into(),
        receipt_digest: "b".repeat(64),
        source_digest: "c".repeat(64),
        funding_receipt_digest: epoch.binding.receipt_digest.clone(),
        review_id: call.grant.review_id,
        retired_review_id: Uuid::from_u128(857),
        ordinal: epoch.binding.ordinal,
    };
    call.schema_version = 6;
    call.grant.schema_version = 6;
    call.grant.funded_review_recovery = Some(Box::new(recovery));
    call
}

#[test]
fn absent_presentation_preserves_legacy_context_bytes_and_digest() {
    for call in [fixture(), funded(), funded_recovery()] {
        assert_eq!(call.context.presentation_version, None);
        call.context.validate(&call.grant).unwrap();
        let original = legacy_context(&call.context);
        let bytes = serde_json::to_vec(&original).unwrap();
        assert_eq!(serde_json::to_vec(&call.context).unwrap(), bytes);
        let restored: AdaptiveLeadershipReviewContextV1 = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored.presentation_version, None);
        assert_eq!(restored, call.context);
        assert_eq!(serde_json::to_vec(&restored).unwrap(), bytes);
        restored.validate(&call.grant).unwrap();
        assert_eq!(
            call.context_digest().unwrap(),
            canonical_sha256(
                "sentinel.workflow.adaptive-leadership-context.v1",
                &(&call.grant, &original),
            )
            .unwrap()
        );
        assert!(serde_json::to_value(&restored).unwrap().get("presentation_version").is_none());
    }
}

#[test]
fn funded_presentation_two_roundtrips_without_changing_authority_or_allowance() {
    for original in [funded(), funded_recovery()] {
        let mut call = original.clone();
        call.context.presentation_version = Some(2);
        call.grant.validate(call.grant_issued_at_unix_ms).unwrap();
        call.context.validate(&call.grant).unwrap();
        let bytes = serde_json::to_vec(&call.context).unwrap();
        let restored: AdaptiveLeadershipReviewContextV1 = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored, call.context);
        assert_eq!(restored.presentation_version, Some(2));
        assert_eq!(serde_json::to_vec(&restored).unwrap(), bytes);
        assert_eq!(serde_json::to_value(&restored).unwrap()["presentation_version"], 2);
        assert_eq!(call.schema_version, original.schema_version);
        assert_eq!(call.grant, original.grant);
        assert_eq!(call.context.source_project, original.context.source_project);
        assert_eq!(call.context.source_session, original.context.source_session);
        assert_eq!(call.context.tool_catalog, original.context.tool_catalog);
        assert_eq!(call.context.evidence_refs, original.context.evidence_refs);
        let issued = call.grant_issued_at_unix_ms + 1;
        assert_eq!(
            call.continuation_allowance(issued, issued + 180_000, 1).unwrap(),
            original.continuation_allowance(issued, issued + 180_000, 1).unwrap()
        );
        let decision = AdaptiveLeadershipReviewDecisionV1 {
            schema_version: call.grant.schema_version,
            decision: AdaptiveLeadershipReviewDecisionKindV1::DeferBudget {
                rationale: "Independently retain this deferral".into(),
                evidence_refs: vec![call.context.evidence_refs[0].clone()],
            },
        };
        decision.validate(&call.context.evidence_refs).unwrap();
        decision.validate_subject(&call.grant).unwrap();
    }
}

#[test]
fn presentation_changes_context_and_call_digests_not_the_evidence_fingerprint() {
    for original in [funded(), funded_recovery()] {
        let original_digest = original.context_digest().unwrap();
        let original_call_digest = canonical_sha256(
            "sentinel.workflow.adaptive-leadership-call.v1", &original,
        ).unwrap();
        let mut marked = original.clone();
        marked.context.presentation_version = Some(2);
        marked.context.validate(&marked.grant).unwrap();
        let marked_digest = marked.context_digest().unwrap();
        assert_ne!(marked_digest, original_digest);
        assert_ne!(
            canonical_sha256("sentinel.workflow.adaptive-leadership-call.v1", &marked).unwrap(),
            original_call_digest
        );
        assert_eq!(
            adaptive_leadership_evidence_fingerprint(
                &marked.context.tool_catalog, &marked.context.evidence_refs,
            ).unwrap(),
            original.grant.evidence_fingerprint
        );
        let encoded = serde_json::to_vec(&marked.context).unwrap();
        let mut wire: Value = serde_json::from_slice(&encoded).unwrap();
        wire.as_object_mut().unwrap().remove("presentation_version");
        let downgraded: AdaptiveLeadershipReviewContextV1 = serde_json::from_value(wire).unwrap();
        // Legacy absence remains valid; sealed context-digest comparison detects removal.
        downgraded.validate(&marked.grant).unwrap();
        marked.context = downgraded;
        assert_ne!(marked.context_digest().unwrap(), marked_digest);
        assert_eq!(marked.context_digest().unwrap(), original_digest);
    }
}

#[test]
fn unknown_presentation_revisions_and_wrong_wire_types_are_rejected() {
    for version in [0, 1, 3, u16::MAX] {
        let mut call = funded();
        call.context.presentation_version = Some(version);
        let restored: AdaptiveLeadershipReviewContextV1 =
            serde_json::from_slice(&serde_json::to_vec(&call.context).unwrap()).unwrap();
        assert_eq!(restored.validate(&call.grant).unwrap_err().code, WorkflowErrorCode::InvalidInput);
    }
    for version in [
        serde_json::json!(-1),
        serde_json::json!(65_536),
        serde_json::json!(2.5),
        serde_json::json!("2"),
        serde_json::json!(true),
    ] {
        let mut wire = serde_json::to_value(funded().context).unwrap();
        wire["presentation_version"] = version;
        assert!(serde_json::from_value::<AdaptiveLeadershipReviewContextV1>(wire).is_err());
    }
    let mut wire = serde_json::to_value(funded().context).unwrap();
    wire["schema_version"] = serde_json::json!(2);
    assert!(serde_json::from_value::<AdaptiveLeadershipReviewContextV1>(wire).is_err());
}

#[test]
fn presentation_two_requires_a_funded_budget_subject_and_valid_funding_shape() {
    let mut legacy = fixture();
    legacy.context.validate(&legacy.grant).unwrap();
    legacy.context.presentation_version = Some(2);
    assert!(legacy.context.validate(&legacy.grant).is_err());
    for schema in 1..=4 {
        let mut call = funded();
        call.grant.schema_version = schema;
        call.context.presentation_version = Some(2);
        assert!(call.context.validate(&call.grant).is_err());
    }
    for subject in [
        None,
        Some(AdaptiveLeadershipReviewSubjectV2::UnknownModel {
            effect: crate::adaptive::continuation_tests::effect(856),
            sealed_unknown_proof_digest: "a".repeat(64),
        }),
        Some(AdaptiveLeadershipReviewSubjectV2::BlockedContinuation {
            reason_code: "needs_review".into(), resolution_event_id: None,
        }),
    ] {
        let mut call = funded();
        call.context.presentation_version = Some(2);
        call.grant.subject = subject;
        assert!(call.context.validate(&call.grant).is_err());
    }
    let mut call = funded();
    call.context.presentation_version = Some(2);
    call.grant.work_funding = None;
    assert!(call.context.validate(&call.grant).is_err());
    let mut call = funded_recovery();
    call.context.presentation_version = Some(2);
    call.grant.funded_review_recovery = None;
    assert!(call.context.validate(&call.grant).is_err());
}

#[test]
fn presentation_marker_is_charged_to_the_existing_funded_head_envelope() {
    let mut call = funded();
    call.context.tool_catalog = serde_json::json!({"padding": ""});
    let bytes = crate::digest::serialized_json_size(&call.context).unwrap();
    let history_bytes = crate::digest::serialized_json_size(
        &call.context.source_session.continuation.as_ref().unwrap().authorizations,
    ).unwrap();
    let head_bytes = bytes - history_bytes + 2;
    let padding = ADAPTIVE_LEADERSHIP_MAX_CONTEXT_BYTES - head_bytes;
    call.context.tool_catalog["padding"] = serde_json::json!("x".repeat(padding));
    // Size-policy fixture only: the padded catalogue is not a dispatchable context.
    assert!(call.context.size_is_bounded(&call.grant).unwrap());
    let legacy_bytes = serde_json::to_vec(&call.context).unwrap().len();
    call.context.presentation_version = Some(2);
    let marker_bytes = b",\"presentation_version\":2".len();
    assert_eq!(serde_json::to_vec(&call.context).unwrap().len(), legacy_bytes + marker_bytes);
    assert!(!call.context.size_is_bounded(&call.grant).unwrap());
    call.context.tool_catalog["padding"] = serde_json::json!("x".repeat(padding - marker_bytes));
    assert!(call.context.size_is_bounded(&call.grant).unwrap());
}
