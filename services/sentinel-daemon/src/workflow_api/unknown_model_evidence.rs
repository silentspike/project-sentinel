//! Exact model-only provenance, without inventing a historical context or outcome.
use super::model_execution::{AdaptiveProviderAuthority, ProviderExecutionAuthority};
use super::*;

impl WorkflowApi {
    pub(super) fn unknown_model_proof_digest(
        &self,
        project: &sentinel_workflow::ProjectV1,
        session: &AdaptiveSessionV1,
        effect: &AdaptiveEffectV1,
    ) -> Result<Option<String>, &'static str> {
        let events = self
            .event_store
            .as_ref()
            .ok_or("unknown model EventStore missing")?;
        let session_version = session
            .version
            .checked_sub(2)
            .ok_or("unknown model version invalid")?;
        let current_binding = select_provider_usage_binding(
            std::slice::from_ref(project),
            session.grant.authority.agent_id,
            Some(session.active_provider_allowance_id()),
        )?;
        // A later grant does not replace the original request's usage identity.
        // Resolve only a verified historical project; never reset the live grant.
        let retained_journal = if current_binding.is_none() {
            self.store
                .first_unknown_model_journal_evidence(
                    session.grant.session_id,
                    &session.grant.authority,
                )
                .map_err(|_| "historical model journal invalid")?
        } else {
            None
        };
        let mut historical_project = match retained_journal.as_ref() {
            Some(journal) => self
                .store
                .historical_adaptive_provider_project(
                    &journal.root_grant,
                    journal.claim.recorded_at_ms,
                )
                .map_err(|_| "historical allowance provenance invalid")?,
            None => None,
        };
        let binding = match current_binding {
            Some(binding) => binding,
            None => select_provider_usage_binding(
                std::slice::from_ref(
                    historical_project
                        .as_ref()
                        .ok_or("unknown model original allowance unavailable")?,
                ),
                session.grant.authority.agent_id,
                Some(session.grant.provider_allowance_id.as_str()),
            )?
            .ok_or("unknown model original allowance unavailable")?,
        };
        let authority = ProviderExecutionAuthority::Adaptive(Box::new(AdaptiveProviderAuthority {
            schema_version: 3,
            grant: session.effective_grant(),
            session_version,
            effect_id: effect.id,
            assignment_id: binding.assignment_id.clone(),
            previous_observation: session.last_observation.clone(),
        }));
        let request_id = authority.request_id();
        if let Some(evidence) = crate::llm_bridge::bridge::sealed_unknown_model_evidence(
            events,
            &authority,
            &request_id,
            &effect.request_digest,
        )
        .map_err(|_| "unknown model reservation evidence invalid")?
        {
            // The new before-send model-only registration binds the trusted
            // inference-only bridge. It never registers tool authority.
            let bytes = sentinel_common::canonical_json(&evidence)
                .map_err(|_| "unknown model evidence encoding failed")?;
            return Ok(Some(sentinel_common::sha256_hex(&bytes)));
        }
        let Some(journal) = (match retained_journal {
            Some(journal) => Some(journal),
            None => self
                .store
                .first_unknown_model_journal_evidence(
                    session.grant.session_id,
                    &session.grant.authority,
                )
                .map_err(|_| "historical model journal invalid")?,
        })
        else {
            return Ok(None);
        };
        if journal.observed_head_version != session.version
            || journal.effect != *effect
            || journal.root_grant != session.grant
            || journal.sealed_model_calls != 1
            || journal.sealed_tool_calls != 0
        {
            return Err("historical model journal head changed");
        }
        let Some(boundary) = super::historical_model_boundary::verified_historical_boundary(
            &request_id,
            &effect.request_digest,
            journal.claim.recorded_at_ms,
        )?
        else {
            return Ok(None);
        };
        if historical_project.is_none() {
            historical_project = self
                .store
                .historical_adaptive_provider_project(
                    &journal.root_grant,
                    journal.claim.recorded_at_ms,
                )
                .map_err(|_| "historical allowance provenance invalid")?;
        }
        let allowance = historical_project
            .as_ref()
            .ok_or("historical allowance missing")?
            .subscription_call
            .as_ref()
            .ok_or("historical allowance missing")?;
        if allowance.allowance_id != journal.root_grant.provider_allowance_id
            || sentinel_workflow::adaptive_leadership_continuation_provider_authority_digest(
                allowance,
                &journal.root_grant.authority,
            )
            .map_err(|_| "historical allowance digest invalid")?
                != journal.root_grant.provider_authority_digest
        {
            return Err("historical allowance binding changed");
        }
        let binding = select_provider_usage_binding(
            std::slice::from_ref(
                historical_project
                    .as_ref()
                    .ok_or("historical allowance missing")?,
            ),
            journal.root_grant.authority.agent_id,
            Some(journal.root_grant.provider_allowance_id.as_str()),
        )?
        .ok_or("historical usage binding missing")?;
        let authority = ProviderExecutionAuthority::Adaptive(Box::new(AdaptiveProviderAuthority {
            schema_version: 3,
            grant: session.effective_grant(),
            session_version,
            effect_id: effect.id,
            assignment_id: binding.assignment_id.clone(),
            previous_observation: session.last_observation.clone(),
        }));
        let record = |entry: &sentinel_workflow::AdaptiveModelJournalRecordEvidenceV1| {
            sentinel_limbo::LlmHistoricalModelJournalReceiptV1 {
                session_id: session.grant.session_id,
                effect_id: effect.id,
                request_digest: effect.request_digest.clone(),
                session_version: entry.session_version,
                operation_id: entry.operation_id,
                entry_digest: entry.entry_digest.clone(),
                timestamp_ms: entry.recorded_at_ms,
            }
        };
        let historical = sentinel_limbo::LlmRetrospectiveModelBindingV1 {
            schema_version: 1, request_id: request_id.clone(), request_digest: effect.request_digest.clone(),
            owner_scope: sentinel_common::StateTransferScope::for_agent(binding.agent_id.to_string()),
            subject: sentinel_limbo::LlmModelSubjectV1::Adaptive {
                session_id: session.grant.session_id, effect_id: effect.id, session_version,
            },
            allowance_id: journal.root_grant.provider_allowance_id.clone(),
            historical_context_digest: None,
            authority_digest: format!("{:x}", Sha256::digest(serde_json::to_vec(&authority)
                .map_err(|_| "historical authority encoding failed")?)),
            usage_binding: sentinel_limbo::LlmModelUsageBindingV1 {
                agent_id: binding.agent_id, tenant_id: binding.tenant_id,
                project_id: binding.project_id, work_item_id: binding.work_item_id,
                reservation_id: binding.reservation_id, assignment_id: binding.assignment_id,
                assignment_version: binding.assignment_version, provider: binding.provider,
                model: journal.root_grant.model.clone(),
            },
            provenance: sentinel_limbo::LlmRetrospectiveModelProvenanceV1::JournalAndPinnedInferenceBoundary {
                claim_model: record(&journal.claim), mark_unknown: record(&journal.seal),
                journal_head_digest: journal.seal.entry_digest.clone(), model_claims: journal.sealed_model_calls,
                tool_claims: journal.sealed_tool_calls, collaboration_claims: 0,
                original_allowance_digest: journal.root_grant.provider_authority_digest.clone(), boundary,
            },
        };
        if self
            .store
            .adaptive_session(session.grant.session_id, &session.grant.authority)
            .map_err(|_| "historical source head unavailable")?
            .as_ref()
            != Some(session)
        {
            return Err("historical source head changed before import");
        }
        events
            .import_retrospective_unknown_llm_model_binding(&historical)
            .map_err(|_| "historical model import rejected")?;
        if self
            .store
            .adaptive_session(session.grant.session_id, &session.grant.authority)
            .map_err(|_| "historical source head unavailable")?
            .as_ref()
            != Some(session)
        {
            return Err("historical source head changed during import");
        }
        let evidence = crate::llm_bridge::bridge::retrospective_unknown_model_evidence(
            events,
            &authority,
            &request_id,
            &effect.request_digest,
        )
        .map_err(|_| "historical model evidence invalid")?
        .ok_or("historical model evidence disappeared")?;
        let bytes = sentinel_common::canonical_json(&evidence)
            .map_err(|_| "historical evidence encoding failed")?;
        Ok(Some(sentinel_common::sha256_hex(&bytes)))
    }
}
