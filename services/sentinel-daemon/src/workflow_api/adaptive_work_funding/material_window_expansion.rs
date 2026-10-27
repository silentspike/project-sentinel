//! Registered operator material offers; no inference, decision or productive adoption.
use super::*;
use sentinel_workflow::{
    AdaptiveMaterialWindowUsageProofV1, AdaptiveMaterialWindowVerificationSourceV1,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct MaterialSubmission {
    schema_version: u16,
    operation_id: Uuid,
    project_id: ProjectId,
    session_id: Uuid,
    declined_funding_receipt_digest: String,
    refused_review_id: Uuid,
    reason_ref: String,
    limits: AdaptiveWorkFundingLimitsV1,
    source_digest: String,
}

fn view(request: &AdaptiveWorkFundingRequestV1) -> Result<MaterialSubmission, WorkflowError> {
    let source = &request.source;
    let material = source
        .material_window_expansion
        .as_ref()
        .ok_or_else(source_conflict)?;
    Ok(MaterialSubmission {
        schema_version: 1,
        operation_id: request.operation_id,
        project_id: source.resume_source.project_id.clone(),
        session_id: source.resume_source.session_id,
        declined_funding_receipt_digest: material.declined_receipt_digest.clone(),
        refused_review_id: material.refused_review_id,
        reason_ref: request.reason_ref.clone(),
        limits: request.limits.clone(),
        source_digest: request.canonical_digest()?,
    })
}

fn draft_query(path: &str) -> Option<MaterialSubmission> {
    Some(MaterialSubmission {
        schema_version: 1,
        operation_id: Uuid::parse_str(query_parameter(path, "operation_id")?).ok()?,
        project_id: ProjectId::parse(query_parameter(path, "project_id")?).ok()?,
        session_id: Uuid::parse_str(query_parameter(path, "session_id")?).ok()?,
        declined_funding_receipt_digest: query_parameter(path, "declined_funding_receipt_digest")?
            .into(),
        refused_review_id: Uuid::parse_str(query_parameter(path, "refused_review_id")?).ok()?,
        reason_ref: query_parameter(path, "reason_ref")?.into(),
        limits: AdaptiveWorkFundingLimitsV1 {
            additional_model_calls: query_parameter(path, "additional_model_calls")?
                .parse()
                .ok()?,
            additional_tool_calls: query_parameter(path, "additional_tool_calls")?
                .parse()
                .ok()?,
            additional_reviews: query_parameter(path, "additional_reviews")?.parse().ok()?,
            additional_windows: query_parameter(path, "additional_windows")?.parse().ok()?,
            max_window_ms: query_parameter(path, "max_window_ms")?.parse().ok()?,
            max_call_duration_ms: query_parameter(path, "max_call_duration_ms")?
                .parse()
                .ok()?,
            dispatch_margin_ms: query_parameter(path, "dispatch_margin_ms")?.parse().ok()?,
            expires_at_unix_ms: query_parameter(path, "expires_at_unix_ms")?.parse().ok()?,
        },
        source_digest: String::new(),
    })
}

type OutboxStamp = Option<(String, String, String)>;

struct NativeWitness {
    proof: AdaptiveMaterialWindowUsageProofV1,
    outbox: OutboxStamp,
}

impl WorkflowApi {
    fn material_native_stamp(
        &self,
        source: &AdaptiveMaterialWindowVerificationSourceV1,
        now: u64,
    ) -> Result<NativeWitness, WorkflowError> {
        let call = &source.refused;
        let events = self.event_store.as_ref().ok_or_else(source_conflict)?;
        let dispatch = call.dispatch.as_ref().ok_or_else(source_conflict)?;
        let usage = events
            .event_by_operation_id(&format!("llm_usage_{}", call.request_id()))
            .map_err(|_| source_conflict())?
            .ok_or_else(source_conflict)?;
        if events
            .event_by_operation_id(&format!("llm_resolution_{}", call.request_id()))
            .map_err(|_| source_conflict())?
            .is_some()
        {
            return Err(source_conflict());
        }
        let outbox = events
            .get_llm_completion(&call.request_id())
            .map_err(|_| source_conflict())?
            .map(|row| {
                (
                    row.status,
                    row.request_digest,
                    sentinel_common::sha256_hex(row.payload.as_bytes()),
                )
            });
        if outbox.as_ref().is_some_and(|(status, digest, _)| {
            !matches!(status.as_str(), "ready_for_action" | "action_claimed")
                || digest != &dispatch.request_digest
        }) {
            return Err(source_conflict());
        }
        Ok(NativeWitness {
            proof: AdaptiveMaterialWindowUsageProofV1 {
                request_id: call.request_id(),
                request_digest: dispatch.request_digest.clone(),
                usage_event_digest: sentinel_common::sha256_hex(
                    &sentinel_common::canonical_json(&usage).map_err(|_| source_conflict())?,
                ),
                verified_at_unix_ms: now,
            },
            outbox,
        })
    }

    pub(super) fn material_window_expansion_http(
        &self,
        operator: &BoundPrincipal,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> WorkflowHttpResponse {
        self.material_window_expansion_http_with_clock(operator, method, path, body, now_unix_ms)
    }

    fn material_window_expansion_http_with_clock(
        &self,
        operator: &BoundPrincipal,
        method: &str,
        path: &str,
        body: &[u8],
        clock: impl Fn() -> u64,
    ) -> WorkflowHttpResponse {
        if self.validate_work_funding_operator(operator).is_err() {
            return workflow_error(source_conflict());
        }
        if !self.enabled || !self.model_work_enabled {
            return workflow_error(workflow_unavailable());
        }
        if !matches!(method, "GET" | "POST") {
            return json_error(405, "method_not_allowed", "GET or POST required", false);
        }
        let mut supplied = if method == "GET" {
            match draft_query(path) {
                Some(value) => value,
                None => {
                    return json_error(
                        400,
                        "invalid_input",
                        "explicit material offer required",
                        false,
                    )
                }
            }
        } else {
            match decode_body::<MaterialSubmission>(body) {
                Ok(value) => value,
                Err(error) => return error,
            }
        };
        if supplied.schema_version != 1
            || supplied.operation_id.is_nil()
            || supplied.session_id.is_nil()
            || supplied.refused_review_id.is_nil()
        {
            return workflow_error(source_conflict());
        }
        let Ok(_fence) = self.mutation_fence.write() else {
            return workflow_error(workflow_unavailable());
        };
        let prepared = (|| -> Result<_, WorkflowError> {
            // Exact historical operation first, without giving it fresh admission or clocks.
            if let Some(stored) = self.store.adaptive_work_funding(
                &operator.principal.tenant_id,
                supplied.session_id,
                supplied.operation_id,
            )? {
                let expected = view(&stored.request)?;
                if method == "GET" {
                    supplied.source_digest = expected.source_digest.clone();
                }
                if supplied != expected || stored.issuer_principal != operator.principal {
                    return Err(source_conflict());
                }
                return Ok(Err(stored));
            }
            let now = clock();
            self.work_funding_current_source(
                operator,
                &supplied.project_id,
                supplied.session_id,
                now,
            )?;
            let source = self.store.adaptive_material_window_verification_source(
                &operator.principal,
                &supplied.project_id,
                supplied.session_id,
                &supplied.declined_funding_receipt_digest,
                supplied.refused_review_id,
                now,
            )?;
            // Full canonical retained-success verification BEFORE acquiring the store SQL mutex.
            let before = self.material_native_stamp(&source, clock())?;
            self.verify_accounting_refusal(&source.refused)
                .map_err(|_| source_conflict())?;
            let witness = self.material_native_stamp(&source, clock())?;
            if witness.outbox != before.outbox
                || witness.proof.usage_event_digest != before.proof.usage_event_digest
            {
                return Err(source_conflict());
            }
            let request = self.store.adaptive_material_window_draft(
                &operator.principal,
                &source,
                supplied.operation_id,
                &supplied.reason_ref,
                supplied.limits.clone(),
                &witness.proof,
                clock(),
            )?;
            let expected = view(&request)?;
            if method == "POST" && supplied != expected {
                return Err(source_conflict());
            }
            Ok(Ok((source, witness, request, expected)))
        })();
        let (source, witness, request, expected) = match prepared {
            Err(error) => return workflow_error(error),
            Ok(Err(receipt)) => return material_receipt(&receipt, true),
            Ok(Ok(value)) => value,
        };
        if method == "GET" {
            return json(
                200,
                &serde_json::json!({"schema_version":1,"requires_explicit_submission":true,
                "request":expected,"authoritative_floor":source.floor,
                "declined_bounds":source.refused.grant.work_funding.as_ref().map(|epoch| &epoch.binding.limits),
                "model_decision_recorded":false,"developer_window_created":false}),
            );
        }
        let outcome = self.store.authorize_adaptive_material_window(
            &operator.principal,
            &request,
            clock(),
            || {
                // Only pinned-source external reads and EventStore here; never reenter WorkflowStore.
                self.work_funding_current_source_in_snapshot(
                    &source.project,
                    &source.session,
                    clock(),
                )?;
                let fresh = self.material_native_stamp(&source, clock())?;
                if fresh.outbox != witness.outbox
                    || fresh.proof.request_id != witness.proof.request_id
                    || fresh.proof.request_digest != witness.proof.request_digest
                    || fresh.proof.usage_event_digest != witness.proof.usage_event_digest
                {
                    return Err(source_conflict());
                }
                Ok(fresh.proof)
            },
        );
        match outcome {
            Ok((replayed, receipt)) => material_receipt(&receipt, replayed),
            Err(error) => workflow_error(error),
        }
    }
}

fn material_receipt(
    receipt: &AdaptiveWorkFundingReceiptV1,
    replayed: bool,
) -> WorkflowHttpResponse {
    let mut response = receipt_http(receipt, replayed);
    if response.status == 200 {
        if let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&response.body) {
            if let Some(binding) = &receipt.request.source.material_window_expansion {
                value["material_slot_key"] = serde_json::json!(binding.slot_key);
                value["declined_receipt_digest"] =
                    serde_json::json!(binding.declined_receipt_digest);
                response.body = serde_json::to_vec(&value).unwrap_or_default();
            }
        }
    }
    response
}
