use super::*;

// Snapshot of the pre-v2 request decoder, including the required string epoch.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyRequest {
    schema_version: u16,
    operation_id: Uuid,
    tenant_id: TenantId,
    project_id: ProjectId,
    work_item_id: WorkItemId,
    session_id: Uuid,
    review_id: Uuid,
    epoch_digest: String,
    original_call_digest: String,
    project_digest: String,
    session_digest: String,
    session_head_digest: String,
    request_id: String,
    request_digest: String,
    context_digest: String,
    payload_digest: String,
    model_response_digest: String,
    usage_event_digest: String,
    original_completion_error: String,
    completion_attempts: u32,
    release: AdaptiveRecoveryReleaseV1,
    repair_digest: String,
    decision: AdaptiveLeadershipReviewDecisionV1,
    expires_at_unix_ms: u64,
}

fn decode_legacy(bytes: &[u8]) -> Result<LegacyRequest, WorkflowError> {
    let request: LegacyRequest = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if request.schema_version != 1 {
        return Err(invalid());
    }
    Ok(request)
}

#[test]
fn legacy_request_wire_bytes_and_digest_are_unchanged() {
    let golden = concat!(
        "{\"schema_version\":1,\"operation_id\":\"00000000-0000-0000-0000-000000000001\",",
        "\"tenant_id\":\"adoption-tenant\",\"project_id\":\"adoption-project\",",
        "\"work_item_id\":\"adoption-work\",\"session_id\":\"00000000-0000-0000-0000-000000000002\",",
        "\"review_id\":\"00000000-0000-0000-0000-000000000003\",",
        "\"epoch_digest\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",",
        "\"original_call_digest\":\"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\",",
        "\"project_digest\":\"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc\",",
        "\"session_digest\":\"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd\",",
        "\"session_head_digest\":\"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee\",",
        "\"request_id\":\"company-leadership-00000000-0000-0000-0000-000000000003\",",
        "\"request_digest\":\"ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff\",",
        "\"context_digest\":\"1111111111111111111111111111111111111111111111111111111111111111\",",
        "\"payload_digest\":\"2222222222222222222222222222222222222222222222222222222222222222\",",
        "\"model_response_digest\":\"3333333333333333333333333333333333333333333333333333333333333333\",",
        "\"usage_event_digest\":\"4444444444444444444444444444444444444444444444444444444444444444\",",
        "\"original_completion_error\":\"continuation audit invalid\",\"completion_attempts\":5,",
        "\"release\":{\"schema_version\":1,\"source_git_sha\":\"5555555555555555555555555555555555555555\",",
        "\"release_manifest_digest\":\"6666666666666666666666666666666666666666666666666666666666666666\",",
        "\"gateway_binary_digest\":\"7777777777777777777777777777777777777777777777777777777777777777\"},",
        "\"repair_digest\":\"8888888888888888888888888888888888888888888888888888888888888888\",",
        "\"decision\":{\"schema_version\":2,\"decision\":{\"kind\":\"continue\",\"additional_model_calls\":1,",
        "\"window_ms\":120000,\"rationale\":\"Adopt the exact retained leadership response\",",
        "\"evidence_refs\":[\"source:retained\"]}},\"expires_at_unix_ms\":121000}"
    );
    let request = record().request;
    assert_eq!(
        serde_json::to_vec(&decode_legacy(golden.as_bytes()).unwrap()).unwrap(),
        golden.as_bytes(),
    );
    assert_eq!(serde_json::to_vec(&request).unwrap(), golden.as_bytes());
    assert_eq!(
        serde_json::from_str::<AdaptiveLeadershipLocalAdoptionRequestV1>(golden).unwrap(),
        request,
    );
    assert_eq!(
        request.canonical_digest().unwrap(),
        crate::digest::canonical_sha256(
            "sentinel.workflow.leadership-local-adoption-request.v1",
            &decode_legacy(golden.as_bytes()).unwrap(),
        )
        .unwrap(),
    );
}

#[test]
fn funded_version_requires_exact_error_attempts_and_exclusive_binding() {
    let legacy = record();
    let mut funded = legacy.clone();
    funded.request.schema_version = 2;
    funded.request.epoch_digest = None;
    funded.request.funded_binding = Some(AdaptiveLeadershipLocalAdoptionFundingBindingV2 {
        funding_id: "work-funding-shape-test".into(),
        receipt_digest: "a".repeat(64),
        binding_digest: "b".repeat(64),
    });
    funded.request.original_completion_error = "work funding current reviews unavailable".into();
    funded.validate().unwrap();
    assert!(funded.request.is_funded());
    let encoded = serde_json::to_value(&funded.request).unwrap();
    assert!(encoded.get("epoch_digest").is_none());
    assert!(encoded.get("funded_binding").is_some());
    for case in ["mixed", "missing", "legacy-error", "attempts", "version"] {
        let mut changed = funded.clone();
        match case {
            "mixed" => changed.request.epoch_digest = legacy.request.epoch_digest.clone(),
            "missing" => changed.request.funded_binding = None,
            "legacy-error" => {
                changed.request.original_completion_error = "continuation audit invalid".into();
            }
            "attempts" => changed.request.completion_attempts = 4,
            _ => changed.request.schema_version = 1,
        }
        assert!(changed.validate().is_err(), "{case}");
    }
    for attempts in 1..=5 {
        let mut value = legacy.clone();
        value.request.completion_attempts = attempts;
        value.validate().unwrap();
    }
}

#[test]
fn legacy_decoder_rejects_funded_v2_without_falling_back_to_a_forged_epoch() {
    let mut funded = serde_json::to_value(record().request).unwrap();
    funded["schema_version"] = serde_json::json!(2);
    funded.as_object_mut().unwrap().remove("epoch_digest");
    funded["funded_binding"] = serde_json::json!({
        "funding_id":"work-funding-legacy-decoder-test",
        "receipt_digest":"a".repeat(64),
        "binding_digest":"b".repeat(64),
    });
    assert!(decode_legacy(&serde_json::to_vec(&funded).unwrap()).is_err());
    funded["epoch_digest"] = serde_json::json!("a".repeat(64));
    assert!(decode_legacy(&serde_json::to_vec(&funded).unwrap()).is_err());
    funded.as_object_mut().unwrap().remove("funded_binding");
    // Even after stripping the unknown field, schema2 cannot fall back to v1.
    assert!(decode_legacy(&serde_json::to_vec(&funded).unwrap()).is_err());
}
