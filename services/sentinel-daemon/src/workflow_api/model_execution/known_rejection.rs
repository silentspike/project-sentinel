//! Receipt-only disposition of a known proposal rejected before tool admission.

use super::*;
use sentinel_limbo::{LlmCompletionEntry, LlmRetainedModelRejectionEvidenceV1};

const KNOWN_REJECTION_ERROR: &str = "adaptive model result admission failed";
const KNOWN_REJECTION_REASON: &str = "fresh_observation_required";

pub(super) struct VerifiedRejection {
    pub(super) evidence: LlmRetainedModelRejectionEvidenceV1,
    decision: AdaptiveModelDecisionV1,
}

fn workflow_conflict() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::AuthorityConflict,
        false,
        "retained rejected-model evidence changed",
    )
}

fn workflow_receipt(
    session: &sentinel_workflow::AdaptiveSessionV1,
    entry_digest: &str,
    verified: &VerifiedRejection,
    resolution: &DomainEvent,
) -> Result<sentinel_workflow::AdaptiveRejectedModelReceiptV1, &'static str> {
    let evidence = &verified.evidence;
    let AdaptiveModelDecisionV1::Tool { tool_digest, .. } = &verified.decision else {
        return Err("known rejection is not a tool proposal");
    };
    let receipt = sentinel_workflow::AdaptiveRejectedModelReceiptV1 {
        schema_version: 1,
        session_id: session.grant.session_id,
        source_session_version: session.version,
        source_entry_digest: entry_digest.to_owned(),
        effect: AdaptiveEffectV1 {
            id: evidence.effect_id,
            request_digest: evidence.request_digest.clone(),
        },
        resolution_event_id: Uuid::parse_str(&resolution.event_id)
            .map_err(|_| "known rejection receipt identity invalid")?,
        reason_code: KNOWN_REJECTION_REASON.to_owned(),
        reservation_digest: value_digest(
            &serde_json::to_value(&evidence.reservation)
                .map_err(|_| "known rejection reservation encoding failed")?,
        )?,
        authority_binding_digest: evidence.authority_binding_digest.clone(),
        completion_payload_digest: evidence.completion_payload_digest.clone(),
        model_response_digest: evidence.model_response_digest.clone(),
        context_digest: evidence.model_context_digest.clone(),
        tool_digest: tool_digest.clone(),
        usage_event_id: evidence.usage_event.event_id.clone(),
        usage_event_digest: evidence.usage_event_digest.clone(),
    };
    receipt
        .validate()
        .map_err(|_| "known rejection receipt invalid")?;
    Ok(receipt)
}

fn value_digest(value: &serde_json::Value) -> Result<String, &'static str> {
    sentinel_common::canonical_json(value)
        .map(|bytes| sentinel_common::sha256_hex(&bytes))
        .map_err(|_| "known rejection evidence encoding failed")
}

impl WorkflowApi {
    pub(in crate::workflow_api) fn recover_known_rejected_adaptive_model(
        &self,
        project: &sentinel_workflow::ProjectV1,
    ) -> Result<bool, &'static str> {
        let Some(allowance) = project.subscription_call.as_ref() else {
            return Ok(false);
        };
        let Some(authority) = self.authority.as_ref() else {
            return Ok(false);
        };
        let Ok(current) = authority.snapshot_for_admission(
            &project.tenant_id,
            &project.project_id,
            &allowance.grant.work_item_id,
            allowance.grant.agent_id,
            false,
        ) else {
            return Ok(false);
        };
        let session = match self.store.adaptive_session_for_authority(&current) {
            Ok(Some(session)) => session,
            Ok(None) => return Ok(false),
            Err(error) if error.code == WorkflowErrorCode::AuthorityConflict => return Ok(false),
            Err(_) => return Err("known rejection session unavailable"),
        };
        let AdaptiveCursorV1::ModelPending { effect } = &session.cursor else {
            return Ok(false);
        };
        if !session.requires_fresh_observation() || session.continuation.is_none() {
            return Ok(false);
        }
        let events = self
            .event_store
            .as_ref()
            .ok_or("known rejection EventStore unavailable")?;
        let request_id = format!(
            "company-adaptive-{}-{}",
            session.grant.session_id, effect.id
        );
        let Some(entry) = events
            .get_llm_completion(&request_id)
            .map_err(|_| "known rejection completion unavailable")?
        else {
            return Ok(false);
        };
        if entry.status != "failed" || entry.last_error.as_deref() != Some(KNOWN_REJECTION_ERROR) {
            return Ok(false);
        }
        let Some((source, entry_digest)) = self
            .store
            .adaptive_pending_model_head_evidence(
                session.grant.session_id,
                session.version,
                effect,
                &current,
            )
            .map_err(|_| "known rejection journal evidence unavailable")?
        else {
            return Ok(false);
        };
        let verified = self.verified_known_rejection(&source, &entry)?;
        if self
            .store
            .adaptive_model_result_is_adopted(
                &source.effective_grant(),
                effect,
                &verified.evidence.model_response_digest,
                &verified.decision,
            )
            .map_err(|_| "known rejection adoption evidence unavailable")?
        {
            return Err("known rejection result was already adopted");
        }
        // Receipt publication may precede a crash. Its replay is exact and leaves
        // the original outbox/usage untouched; the journal transaction comes next.
        let resolution = events
            .record_retained_model_decision_rejection(&verified.evidence)
            .map_err(|_| "known rejection retained receipt failed")?;
        let receipt = workflow_receipt(&source, &entry_digest, &verified, &resolution)?;
        let operation = stable_operation_id(
            "sentinel.workflow.dispose-rejected-model.v1",
            &resolution.event_id,
            source.version,
        );
        self.store
            .dispose_rejected_adaptive_model(
                operation,
                &receipt,
                &current,
                || now_unix_ms().max(source.updated_at_ms),
                |locked, digest, expected| {
                    let retained = events
                        .get_llm_completion(&request_id)
                        .map_err(|_| workflow_conflict())?
                        .ok_or_else(workflow_conflict)?;
                    let checked = self
                        .verified_known_rejection(locked, &retained)
                        .map_err(|_| workflow_conflict())?;
                    let recorded = events
                        .event_by_operation_id(&format!("llm_resolution_{request_id}"))
                        .map_err(|_| workflow_conflict())?
                        .ok_or_else(workflow_conflict)?;
                    if serde_json::to_value(&recorded).map_err(|_| workflow_conflict())?
                        != serde_json::to_value(&resolution).map_err(|_| workflow_conflict())?
                        || workflow_receipt(locked, digest, &checked, &recorded)
                            .map_err(|_| workflow_conflict())?
                            != *expected
                    {
                        return Err(workflow_conflict());
                    }
                    // Recheck every original row field and the before-send binding
                    // atomically, including receipt replay. New row metadata is not
                    // interchangeable with the evidence sealed before journal entry.
                    let replayed = events
                        .record_retained_model_decision_rejection(&verified.evidence)
                        .map_err(|_| workflow_conflict())?;
                    if serde_json::to_value(replayed).map_err(|_| workflow_conflict())?
                        != serde_json::to_value(&resolution).map_err(|_| workflow_conflict())?
                    {
                        return Err(workflow_conflict());
                    }
                    Ok(checked.decision)
                },
            )
            .map_err(|_| "known rejection journal disposition failed")?;
        Ok(true)
    }

    // This verification must not enter WorkflowStore: the final callback runs
    // while its exact source head is locked. EventStore is read independently.
    pub(super) fn verified_known_rejection(
        &self,
        session: &sentinel_workflow::AdaptiveSessionV1,
        entry: &LlmCompletionEntry,
    ) -> Result<VerifiedRejection, &'static str> {
        let AdaptiveCursorV1::ModelPending { effect } = &session.cursor else {
            return Err("known rejection source is not pending");
        };
        if !session.requires_fresh_observation()
            || session.continuation.is_none()
            || session.is_abandoned_model_effect(effect)
            || entry.status != "failed"
            || entry.last_error.as_deref() != Some(KNOWN_REJECTION_ERROR)
            || entry.payload.is_empty()
            || entry.payload.len() > 2 * 1024 * 1024
            || entry.request_digest != effect.request_digest
            || entry.owner_scope
                != sentinel_common::StateTransferScope::for_agent(
                    session.grant.authority.agent_id.to_string(),
                )
        {
            return Err("known rejection source evidence changed");
        }
        let payload: serde_json::Value =
            serde_json::from_str(&entry.payload).map_err(|_| "known rejection payload invalid")?;
        let model_work = payload
            .get("model_work")
            .ok_or("known rejection model completion missing")?;
        let completion: ModelExecutionCompletion = serde_json::from_value(model_work.clone())
            .map_err(|_| "known rejection model completion invalid")?;
        let ModelExecutionContext::Adaptive(context) = &completion.context else {
            return Err("known rejection model subject changed");
        };
        let binding = &context.binding;
        if !completion.admissible
            || completion.content.len() > 128 * 1024
            || binding.schema_version != 3
            || binding.session_version.checked_add(1) != Some(session.version)
            || binding.effect_id != effect.id
            || binding.grant != session.effective_grant()
            || binding.previous_observation != session.last_observation
            || binding.request_id() != entry.request_id
            || stable_operation_id(
                "sentinel.workflow.adaptive-model-effect.v1",
                &session.grant.session_id.to_string(),
                binding.session_version,
            ) != effect.id
            || payload.get("version").and_then(serde_json::Value::as_u64) != Some(2)
            || payload
                .get("request_id")
                .and_then(serde_json::Value::as_str)
                != Some(entry.request_id.as_str())
            || payload
                .get("request_digest")
                .and_then(serde_json::Value::as_str)
                != Some(entry.request_digest.as_str())
            || !payload
                .get("actions")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|actions| actions.is_empty())
            || serde_json::to_value(&completion)
                .map_err(|_| "known rejection completion encoding failed")?
                != *model_work
        {
            return Err("known rejection model authority changed");
        }
        // Validate the actual retained context historically; never rebuild its
        // prompt, catalogue or request bytes from the newer preparation code.
        context.validate_dispatch(entry.created_at)?;
        if entry.created_at < binding.grant.created_at_ms {
            return Err("known rejection predates its grant");
        }
        let decision = parse_adaptive_decision(&completion.content)?;
        if !matches!(
            &decision,
            AdaptiveModelDecisionV1::Tool {
                tool: WorkbenchTool::WriteFile { .. },
                ..
            }
        ) {
            return Err("known rejection is not the supported preinspection write");
        }
        validate_adaptive_decision_evidence(context.observation.as_ref(), &decision)?;
        let model_response_digest = hex_sha256(completion.content.as_bytes());
        // Adaptive completions historically retain the full raw content without
        // the leadership-only redundant digest. Seal the actual payload/content;
        // a supplied optional digest must still match, never be repaired.
        if payload
            .get("model_response_digest")
            .is_some_and(|value| value.as_str() != Some(model_response_digest.as_str()))
        {
            return Err("known rejection raw response changed");
        }
        let usage_event: DomainEvent = serde_json::from_value(
            payload
                .get("usage_event")
                .cloned()
                .ok_or("known rejection usage missing")?,
        )
        .map_err(|_| "known rejection usage invalid")?;
        completion.validate_usage(&usage_event)?;
        let persisted = self
            .event_store
            .as_ref()
            .ok_or("known rejection EventStore unavailable")?
            .event_by_operation_id(&format!("llm_usage_{}", entry.request_id))
            .map_err(|_| "known rejection persisted usage unavailable")?
            .ok_or("known rejection persisted usage missing")?;
        let usage_value = serde_json::to_value(&usage_event)
            .map_err(|_| "known rejection usage encoding failed")?;
        if serde_json::to_value(&persisted)
            .map_err(|_| "known rejection persisted usage encoding failed")?
            != usage_value
        {
            return Err("known rejection durable usage changed");
        }
        let authority_binding =
            serde_json::to_value(binding).map_err(|_| "known rejection binding encoding failed")?;
        let reservation = crate::llm_bridge::bridge::model_reservation(
            &completion.context,
            &entry.request_id,
            &entry.request_digest,
        )
        .map_err(|_| "known rejection original reservation invalid")?
        .ok_or("known rejection original reservation missing")?;
        Ok(VerifiedRejection {
            evidence: LlmRetainedModelRejectionEvidenceV1 {
                schema_version: 1,
                request_id: entry.request_id.clone(),
                request_digest: entry.request_digest.clone(),
                owner_scope: entry.owner_scope.clone(),
                session_id: session.grant.session_id,
                effect_id: effect.id,
                provider_session_version: binding.session_version,
                expected_session_version: session.version,
                authority_binding_digest: value_digest(&authority_binding)?,
                authority_binding,
                completion_payload_digest: hex_sha256(entry.payload.as_bytes()),
                model_response_digest,
                model_context_digest: value_digest(
                    model_work
                        .get("context")
                        .ok_or("known rejection context missing")?,
                )?,
                usage_event_digest: value_digest(&usage_value)?,
                usage_event,
                expected_error: KNOWN_REJECTION_ERROR.to_owned(),
                expected_attempt_count: entry.attempt_count,
                expected_created_at_ms: entry.created_at,
                expected_updated_at_ms: entry.updated_at,
                reason_code: KNOWN_REJECTION_REASON.to_owned(),
                reservation,
            },
            decision,
        })
    }
}
