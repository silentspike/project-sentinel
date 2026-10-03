//! A durable audit precedes the atomic domain continuation; retries retain its time.
use super::*;
use sentinel_common::{
    AppendProposalV2, AuthorityKindV1, AuthorityRefV1, CausalContextV1, CausationPolicyV1,
    EventContractError, EventDurability, EventPayloadCodec, EventSchemaDefinition,
    EventSchemaRegistry, ExpectedStreamRevision,
};
use sentinel_workflow::{AdaptiveLeadershipReviewCallV1, CompleteAdaptiveLeadershipReviewCallV1};

const EVENT_TYPE: &str = "adaptive_leadership_continuation_authorized";
const PRODUCER: &str = "sentinel-daemon-adaptive-continuation";

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContinuationAudit {
    schema_version: u16,
    call: AdaptiveLeadershipReviewCallV1,
    result: CompleteAdaptiveLeadershipReviewCallV1,
}

fn validate_audit(payload: &[u8]) -> Result<(), EventContractError> {
    let invalid = || EventContractError::InvalidField {
        field: "adaptive_continuation.payload",
        reason: "invalid governed continuation audit",
    };
    let audit: ContinuationAudit = serde_json::from_slice(payload).map_err(|_| invalid())?;
    let continuation = audit.result.continuation.as_ref().ok_or_else(invalid)?;
    let call = &audit.call;
    if audit.schema_version
        != (if continuation.local_adoption.is_some() {
            2
        } else {
            1
        })
        || call.decision.is_some()
        || call.retired_at_unix_ms.is_some()
        || audit.result.review_id != call.grant.review_id
        || audit.result.allowance_id != call.allowance_id
        || !call
            .dispatch
            .as_ref()
            .is_some_and(|dispatch| dispatch.request_digest == audit.result.request_digest)
        || continuation.review_id != call.grant.review_id
        || continuation.session_id != call.grant.session_id
        || continuation.source_session_version != call.grant.expected_session_version
        || continuation.resume_policy != call.grant.resume_policy
        || audit.result.resolution_event_id != Some(continuation.resolution_event_id)
    {
        return Err(invalid());
    }
    call.grant
        .validate(call.grant_issued_at_unix_ms)
        .map_err(|_| invalid())?;
    call.context.validate(&call.grant).map_err(|_| invalid())?;
    call.validate_completion_proposal(&audit.result)
        .map_err(|_| invalid())?;
    audit
        .result
        .decision
        .validate_subject(&call.grant)
        .map_err(|_| invalid())?;
    audit
        .result
        .decision
        .validate(&call.context.evidence_refs)
        .map_err(|_| invalid())?;
    continuation.validate().map_err(|_| invalid())?;
    let expected = sentinel_workflow::adaptive_leadership_continuation_audit_id(
        call.grant.review_id,
        &audit.result.request_digest,
        &audit.result.model_response_digest,
        &audit.result.decision,
    )
    .map_err(|_| invalid())?;
    if expected != continuation.resolution_event_id {
        return Err(invalid());
    }
    Ok(())
}

fn continuation_proposal(
    call: &AdaptiveLeadershipReviewCallV1,
    proposed: &CompleteAdaptiveLeadershipReviewCallV1,
) -> Result<AppendProposalV2, &'static str> {
    let authorization = proposed
        .continuation
        .as_ref()
        .ok_or("continuation missing")?;
    if authorization.local_adoption.is_some() {
        return sentinel_workflow::WorkflowStore::local_adoption_continuation_audit_proposal(
            call, proposed,
        )
        .map_err(|_| "continuation audit invalid");
    }
    let id = authorization.resolution_event_id.to_string();
    let payload = sentinel_common::canonical_json(&ContinuationAudit {
        schema_version: if authorization.local_adoption.is_some() {
            2
        } else {
            1
        },
        call: call.clone(),
        result: proposed.clone(),
    })
    .map_err(|_| "continuation audit encoding failed")?;
    validate_audit(&payload).map_err(|_| "continuation audit invalid")?;
    let digest = sentinel_common::sha256_hex(&payload);
    let authority = &call.grant.assignee_authority;
    let reference = |kind, id, generation, digest| AuthorityRefV1 {
        kind,
        id,
        authority_generation: generation,
        authority_digest: digest,
    };
    Ok(AppendProposalV2 {
        proposal_version: sentinel_common::EVENT_PROPOSAL_VERSION_V2,
        requested_event_id: Some(id),
        event_type: EVENT_TYPE.to_owned(),
        schema_version: 1,
        payload_codec: EventPayloadCodec::Json,
        payload_digest: digest.clone(),
        payload,
        causal_context: CausalContextV1 {
            schema_version: sentinel_common::CAUSAL_CONTEXT_VERSION_V1,
            tenant: reference(
                AuthorityKindV1::Tenant,
                authority.tenant_id.0.clone(),
                1,
                domain_digest(
                    "sentinel.adaptive-recovery.tenant.v1",
                    &[authority.tenant_id.0.as_bytes()],
                ),
            ),
            company: reference(
                AuthorityKindV1::Company,
                "virtual-company".to_owned(),
                authority.organization_generation,
                authority.organization_digest.clone(),
            ),
            project: reference(
                AuthorityKindV1::Project,
                authority.project_id.0.clone(),
                authority.policy_generation,
                authority.policy_digest.clone(),
            ),
            workflow: Some(reference(
                AuthorityKindV1::Workflow,
                format!("adaptive-continuation:{}", call.grant.review_id),
                1,
                authority
                    .canonical_digest()
                    .map_err(|_| "continuation authority invalid")?,
            )),
            work_item: Some(reference(
                AuthorityKindV1::WorkItem,
                authority.work_item_id.0.clone(),
                authority.assignment_version,
                authority.assignment_digest.clone(),
            )),
            request_id: call.request_id(),
            request_digest: proposed.request_digest.clone(),
            correlation_id: call.grant.session_id.to_string(),
            causation_event_id: None,
            operation_id: authorization.resolution_event_id.to_string(),
            attempt: 1,
            source_generation: call.grant.leadership_principal.authority_generation,
            source_digest: digest,
            invocation_id: None,
            agent_id: Some(authority.agent_id.to_string()),
            tick: None,
            artifact_id: None,
            artifact_digest: None,
            qa_run_id: None,
            release_id: None,
            delivery_id: None,
            diagnostic_trace_id: None,
            diagnostic_span_id: None,
        },
        producer: PRODUCER.to_owned(),
        owner_term: None,
        tick: None,
        requested_durability: EventDurability::Authoritative,
        expected_stream_revision: ExpectedStreamRevision::NoStream,
        delivery_intents: Vec::new(),
        effect_reservations: Vec::new(),
    })
}

fn audit_envelope_matches(
    event: &sentinel_common::EventEnvelopeV2,
    proposal: &AppendProposalV2,
) -> Result<bool, &'static str> {
    Ok(
        proposal.requested_event_id.as_deref() == Some(event.event_id.as_str())
            && event.event_type == proposal.event_type
            && event.producer == proposal.producer
            && event.schema_version == proposal.schema_version
            && event.payload_codec == proposal.payload_codec
            && event.payload == proposal.payload
            && event.payload_digest == proposal.payload_digest
            && event.causal_context == proposal.causal_context
            && event.owner_term == proposal.owner_term
            && event.tick == proposal.tick
            && event.durability == proposal.requested_durability
            && event.canonical_request_digest
                == proposal
                    .canonical_request_digest()
                    .map_err(|_| "continuation audit request invalid")?,
    )
}

impl WorkflowApi {
    pub(super) fn append_continuation_audit(
        &self,
        call: &AdaptiveLeadershipReviewCallV1,
        proposed: &CompleteAdaptiveLeadershipReviewCallV1,
    ) -> Result<CompleteAdaptiveLeadershipReviewCallV1, &'static str> {
        self.validate_resume_policy_review(call)?;
        self.store
            .validate_adaptive_leadership_recovery_decision(call, &proposed.decision)
            .map_err(|_| "recovery continuation exceeds immutable authority")?;
        let events = self
            .event_store
            .as_ref()
            .ok_or("continuation EventStore missing")?;
        let authorization = proposed
            .continuation
            .as_ref()
            .ok_or("continuation missing")?;
        let id = authorization.resolution_event_id.to_string();
        // A crash after append reuses the recorded grant clock, not a new window.
        if let Some(event) = events
            .event_v2_by_id(&id)
            .map_err(|_| "continuation audit read failed")?
        {
            validate_audit(&event.payload).map_err(|_| "continuation historical audit invalid")?;
            let recorded: ContinuationAudit = serde_json::from_slice(&event.payload)
                .map_err(|_| "continuation historical audit invalid")?;
            let mut comparable = proposed.clone();
            comparable.continuation = recorded.result.continuation.clone();
            let expected = continuation_proposal(&recorded.call, &recorded.result)?;
            if recorded.call != *call
                || recorded.result != comparable
                || !audit_envelope_matches(&event, &expected)?
            {
                return Err("continuation historical audit binding changed");
            }
            return Ok(recorded.result);
        }
        let proposal = continuation_proposal(call, proposed)?;
        let registry = EventSchemaRegistry::new([EventSchemaDefinition {
            event_type: EVENT_TYPE.to_owned(),
            schema_version: 1,
            durability: EventDurability::Authoritative,
            payload_codec: EventPayloadCodec::Json,
            causation_policy: CausationPolicyV1::RootRequired,
            allowed_producers: BTreeSet::from([PRODUCER.to_owned()]),
            deterministic_event_id_producers: BTreeSet::from([PRODUCER.to_owned()]),
            validator_id: "governed-adaptive-continuation-v1".to_owned(),
            validate_payload: validate_audit,
            upcast: None,
        }])
        .map_err(|_| "continuation registry invalid")?;
        let caller = sentinel_limbo::AuthenticatedEventCallerV1 {
            service_id: "sentinel-daemon-workflow".to_owned(),
            producer: PRODUCER.to_owned(),
            authority_scope_digest: proposal
                .causal_context
                .authority_scope_digest()
                .map_err(|_| "continuation scope invalid")?,
        };
        events
            .append_gateway(&registry)
            .append(&caller, &proposal)
            .map_err(|_| "continuation audit append failed")?;
        Ok(proposed.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuation_replay_rejects_envelope_authority_and_request_changes() {
        let reference = |kind, id: &str| AuthorityRefV1 {
            kind,
            id: id.to_owned(),
            authority_generation: 1,
            authority_digest: "a".repeat(64),
        };
        let proposal = AppendProposalV2 {
            proposal_version: sentinel_common::EVENT_PROPOSAL_VERSION_V2,
            requested_event_id: Some(Uuid::now_v7().to_string()),
            event_type: EVENT_TYPE.to_owned(),
            schema_version: 1,
            payload_codec: EventPayloadCodec::Json,
            payload_digest: sentinel_common::sha256_hex(b"{}"),
            payload: b"{}".to_vec(),
            causal_context: CausalContextV1 {
                schema_version: sentinel_common::CAUSAL_CONTEXT_VERSION_V1,
                tenant: reference(AuthorityKindV1::Tenant, "tenant"),
                company: reference(AuthorityKindV1::Company, "company"),
                project: reference(AuthorityKindV1::Project, "project"),
                workflow: None,
                work_item: None,
                request_id: "request".to_owned(),
                request_digest: "b".repeat(64),
                correlation_id: "correlation".to_owned(),
                causation_event_id: None,
                operation_id: Uuid::new_v4().to_string(),
                attempt: 1,
                source_generation: 1,
                source_digest: "c".repeat(64),
                invocation_id: None,
                agent_id: None,
                tick: None,
                artifact_id: None,
                artifact_digest: None,
                qa_run_id: None,
                release_id: None,
                delivery_id: None,
                diagnostic_trace_id: None,
                diagnostic_span_id: None,
            },
            producer: PRODUCER.to_owned(),
            owner_term: None,
            tick: None,
            requested_durability: EventDurability::Authoritative,
            expected_stream_revision: ExpectedStreamRevision::NoStream,
            delivery_intents: Vec::new(),
            effect_reservations: Vec::new(),
        };
        let event = sentinel_common::EventEnvelopeV2 {
            event_id: proposal.requested_event_id.clone().unwrap(),
            event_truth_generation: 1,
            stream_namespace: "test".to_owned(),
            stream_revision: 1,
            global_position: 1,
            event_type: proposal.event_type.clone(),
            schema_version: proposal.schema_version,
            payload_codec: proposal.payload_codec,
            payload_digest: proposal.payload_digest.clone(),
            payload: proposal.payload.clone(),
            causal_context: proposal.causal_context.clone(),
            producer: proposal.producer.clone(),
            owner_term: None,
            tick: None,
            appended_at_ms: 1,
            durability: proposal.requested_durability,
            canonical_request_digest: proposal.canonical_request_digest().unwrap(),
            append_receipt_digest: "d".repeat(64),
            sealed_envelope_digest: "e".repeat(64),
        };
        assert!(audit_envelope_matches(&event, &proposal).unwrap());
        for field in 0..13 {
            let mut changed = event.clone();
            match field {
                0 => changed.event_id = Uuid::new_v4().to_string(),
                1 => changed.event_type.push_str("_other"),
                2 => changed.producer.push_str("_other"),
                3 => changed.schema_version += 1,
                4 => changed.payload.push(b' '),
                5 => changed.payload_digest = "f".repeat(64),
                6 => changed.causal_context.project.authority_generation += 1,
                7 => changed.causal_context.request_digest = "f".repeat(64),
                8 => changed.canonical_request_digest = "f".repeat(64),
                9 => changed.payload_codec = EventPayloadCodec::DeterministicCbor,
                10 => changed.durability = EventDurability::RebuildableTelemetry,
                11 => changed.tick = Some(1),
                _ => {
                    changed.owner_term = Some(sentinel_common::OwnerTerm {
                        scope: sentinel_common::StateTransferScope::for_agent("AGENT-01"),
                        owner_node: sentinel_common::NodeId(Uuid::new_v4()),
                        epoch: 1,
                        coordinator_generation: 1,
                    })
                }
            }
            assert!(
                !audit_envelope_matches(&changed, &proposal).unwrap(),
                "field {field}"
            );
        }
    }
}
