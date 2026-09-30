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
        self.unknown_model_proof_digest_with_import(project, session, effect, true)
    }

    pub(super) fn read_only_unknown_model_proof_digest(
        &self,
        project: &sentinel_workflow::ProjectV1,
        session: &AdaptiveSessionV1,
        effect: &AdaptiveEffectV1,
    ) -> Result<Option<String>, &'static str> {
        self.unknown_model_proof_digest_with_import(project, session, effect, false)
    }

    fn unknown_model_proof_digest_with_import(
        &self,
        project: &sentinel_workflow::ProjectV1,
        session: &AdaptiveSessionV1,
        effect: &AdaptiveEffectV1,
        allow_import: bool,
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
            let Some(journal) = self
                .store
                .first_unknown_model_journal_evidence(
                    session.grant.session_id,
                    &session.grant.authority,
                )
                .map_err(|_| "historical model journal invalid")?
            else {
                return Ok(None);
            };
            Some(journal)
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
        }) else {
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
        if allow_import {
            events
                .import_retrospective_unknown_llm_model_binding(&historical)
                .map_err(|_| "historical model import rejected")?;
        }
        if self
            .store
            .adaptive_session(session.grant.session_id, &session.grant.authority)
            .map_err(|_| "historical source head unavailable")?
            .as_ref()
            != Some(session)
        {
            return Err("historical source head changed during import");
        }
        let Some(evidence) = crate::llm_bridge::bridge::retrospective_unknown_model_evidence(
            events,
            &authority,
            &request_id,
            &effect.request_digest,
        )
        .map_err(|_| "historical model evidence invalid")?
        else {
            return if allow_import {
                Err("historical model evidence disappeared")
            } else {
                Ok(None)
            };
        };
        let bytes = sentinel_common::canonical_json(&evidence)
            .map_err(|_| "historical evidence encoding failed")?;
        Ok(Some(sentinel_common::sha256_hex(&bytes)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_limbo::rusqlite::{self, types::Value};
    use sentinel_workflow::{
        adaptive_tool_digest, AdaptiveModelDecisionV1, AdaptiveObservationRefV1,
        AdaptiveTransitionV1,
    };

    fn advance(
        api: &WorkflowApi,
        session: &AdaptiveSessionV1,
        command: AdaptiveTransitionV1,
    ) -> AdaptiveSessionV1 {
        api.store
            .advance_adaptive_session(
                session.grant.session_id,
                session.version,
                Uuid::new_v4(),
                &command,
                &session.grant.authority,
                session.updated_at_ms + 1,
            )
            .unwrap()
            .1
    }

    fn claim(
        api: &WorkflowApi,
        session: &AdaptiveSessionV1,
    ) -> (AdaptiveSessionV1, AdaptiveEffectV1) {
        let effect = AdaptiveEffectV1 {
            id: Uuid::new_v4(),
            request_digest: "a".repeat(64),
        };
        let pending = advance(
            api,
            session,
            AdaptiveTransitionV1::ClaimModel {
                effect: effect.clone(),
                previous_observation_digest: session
                    .last_observation
                    .as_ref()
                    .map(|observation| observation.observation_digest.clone()),
            },
        );
        (pending, effect)
    }

    fn project(api: &WorkflowApi, session: &AdaptiveSessionV1) -> sentinel_workflow::ProjectV1 {
        api.store
            .company_project(
                &session.grant.authority.tenant_id,
                &session.grant.authority.project_id,
            )
            .unwrap()
            .unwrap()
    }

    fn roll_allowance(
        api: &WorkflowApi,
        session: &AdaptiveSessionV1,
    ) -> sentinel_workflow::ProjectV1 {
        let before = project(api, session);
        let allowance = before.subscription_call.as_ref().unwrap();
        let now = allowance.grant.expires_at_unix_ms + 1;
        let mut grant = allowance.grant.clone();
        grant.expires_at_unix_ms = now + 300_000;
        api.store
            .apply_company_command(
                &api.principals.principal("pm").unwrap().principal,
                Uuid::new_v4(),
                &CompanyWorkflowCommandV1::GrantSubscriptionCall {
                    project_id: before.project_id.clone(),
                    expected_version: before.version,
                    grant,
                },
                now,
            )
            .unwrap();
        project(api, session)
    }

    fn unknown(
        api: &WorkflowApi,
        pending: &AdaptiveSessionV1,
        effect: &AdaptiveEffectV1,
    ) -> AdaptiveSessionV1 {
        let events = api.event_store.as_ref().unwrap();
        let request_id = format!(
            "company-adaptive-{}-{}",
            pending.grant.session_id, effect.id
        );
        events
            .reserve_llm_request(
                &request_id,
                &effect.request_digest,
                &pending.grant.authority.agent_id.to_string(),
            )
            .unwrap();
        events
            .mark_llm_provider_outcome_unknown(
                &request_id,
                &effect.request_digest,
                "UnknownOutcome: bridge_task_ended_without_durable_response",
            )
            .unwrap();
        advance(
            api,
            pending,
            AdaptiveTransitionV1::MarkUnknown {
                effect: effect.clone(),
            },
        )
    }

    fn second_model_unknown(
        company_path: &Path,
        event_path: &Path,
    ) -> (
        WorkflowApi,
        AdaptiveSessionV1,
        AdaptiveEffectV1,
        sentinel_workflow::ProjectV1,
    ) {
        let (api, ready) =
            super::super::adaptive_recovery::tests::fixture(company_path, event_path, false);
        let (pending, first) = claim(&api, &ready);
        let tool = WorkbenchTool::ListDirectory {
            path: ".".into(),
            after: None,
            max_entries: 1,
        };
        let tool_digest = adaptive_tool_digest(&tool).unwrap();
        let tool_ready = advance(
            &api,
            &pending,
            AdaptiveTransitionV1::ResolveModel {
                effect: first,
                result_digest: "b".repeat(64),
                decision: AdaptiveModelDecisionV1::Tool {
                    tool,
                    tool_digest: tool_digest.clone(),
                },
            },
        );
        let tool_effect = AdaptiveEffectV1 {
            id: Uuid::new_v4(),
            request_digest: "c".repeat(64),
        };
        let tool_pending = advance(
            &api,
            &tool_ready,
            AdaptiveTransitionV1::ClaimTool {
                effect: tool_effect.clone(),
                tool_digest,
            },
        );
        // Synthetic journal observation only: no Workbench or provider is invoked.
        let observed = advance(
            &api,
            &tool_pending,
            AdaptiveTransitionV1::ObserveTool {
                observation: AdaptiveObservationRefV1 {
                    effect: tool_effect,
                    observation_digest: "d".repeat(64),
                },
            },
        );
        let (pending, effect) = claim(&api, &observed);
        let sealed = unknown(&api, &pending, &effect);
        let rolled = roll_allowance(&api, &sealed);
        (api, sealed, effect, rolled)
    }

    fn durable_state(company_path: &Path, event_path: &Path) -> Vec<Vec<Vec<Value>>> {
        let company = rusqlite::Connection::open(company_path).unwrap();
        let events = rusqlite::Connection::open(event_path).unwrap();
        company.execute_batch("PRAGMA query_only=ON").unwrap();
        events.execute_batch("PRAGMA query_only=ON").unwrap();
        [
            (
                &company,
                "SELECT * FROM company_entities ORDER BY tenant_id,entity_kind,entity_id",
            ),
            (
                &company,
                "SELECT * FROM company_operations ORDER BY authority_namespace,operation_id",
            ),
            (&company, "SELECT * FROM company_events ORDER BY sequence"),
            (
                &company,
                "SELECT * FROM workflow_operations ORDER BY operation_namespace,operation_id",
            ),
            (
                &company,
                "SELECT * FROM workflow_adaptive_heads ORDER BY session_id",
            ),
            (
                &events,
                "SELECT * FROM llm_completion_outbox ORDER BY request_id",
            ),
            (&events, "SELECT * FROM events ORDER BY id"),
        ]
        .into_iter()
        .map(|(connection, sql)| {
            let mut statement = connection.prepare(sql).unwrap();
            let columns = statement.column_count();
            let rows = statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|index| row.get::<_, Value>(index))
                        .collect::<rusqlite::Result<Vec<Value>>>()
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            rows
        })
        .collect()
    }

    #[test]
    fn unsupported_second_model_with_rolled_allowance_is_non_serving_and_read_only() {
        let temp = tempfile::tempdir().unwrap();
        let company_path = temp.path().join("company.sqlite");
        let event_path = temp.path().join("events.sqlite");
        let (api, session, effect, rolled) = second_model_unknown(&company_path, &event_path);
        assert_eq!((session.model_calls, session.tool_calls), (2, 1));
        assert_ne!(
            rolled.subscription_call.as_ref().unwrap().allowance_id,
            session.grant.provider_allowance_id
        );
        assert!(select_provider_usage_binding(
            std::slice::from_ref(&rolled),
            session.grant.authority.agent_id,
            Some(session.active_provider_allowance_id()),
        )
        .unwrap()
        .is_none());
        let before = durable_state(&company_path, &event_path);
        assert!(api
            .store
            .first_unknown_model_journal_evidence(
                session.grant.session_id,
                &session.grant.authority
            )
            .unwrap()
            .is_none());
        for _ in 0..2 {
            assert_eq!(
                api.unknown_model_proof_digest(&rolled, &session, &effect),
                Ok(None)
            );
        }
        assert_eq!(
            api.store
                .adaptive_session(session.grant.session_id, &session.grant.authority)
                .unwrap(),
            Some(session)
        );
        assert_eq!(durable_state(&company_path, &event_path), before);
    }

    #[test]
    fn corrupt_journal_with_rolled_allowance_remains_an_error_without_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let company_path = temp.path().join("company.sqlite");
        let event_path = temp.path().join("events.sqlite");
        let (api, session, effect, rolled) = second_model_unknown(&company_path, &event_path);
        assert_eq!(
            rusqlite::Connection::open(&company_path)
                .unwrap()
                .execute(
                    "UPDATE workflow_operations SET request_digest=?1 WHERE operation_namespace=?2 AND operation_id=?3",
                    rusqlite::params![
                        "0".repeat(64),
                        format!("adaptive-session-v1:{}", session.grant.session_id),
                        format!("{:020}", session.version)
                    ],
                )
                .unwrap(),
            1
        );
        let before = durable_state(&company_path, &event_path);
        assert!(api
            .store
            .first_unknown_model_journal_evidence(
                session.grant.session_id,
                &session.grant.authority
            )
            .is_err());
        assert_eq!(
            api.unknown_model_proof_digest(&rolled, &session, &effect),
            Err("historical model journal invalid")
        );
        assert_eq!(durable_state(&company_path, &event_path), before);
    }

    #[test]
    fn inherited_recovery_root_requires_verified_original_allowance_without_replay() {
        for mismatched_allowance_digest in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let company_path = temp.path().join("company.sqlite");
            let event_path = temp.path().join("events.sqlite");
            let (api, ready) =
                super::super::adaptive_recovery::tests::fixture(&company_path, &event_path, false);
            let (pending, effect) = claim(&api, &ready);
            let rejected = advance(
                &api,
                &pending,
                AdaptiveTransitionV1::RejectModel {
                    effect,
                    reason_code: "schema_invalid".into(),
                    resolution_event_id: Uuid::new_v4().to_string(),
                },
            );
            let replacement = roll_allowance(&api, &rejected);
            let allowance = replacement.subscription_call.as_ref().unwrap();
            let mut grant = rejected.grant.clone();
            grant.session_id = Uuid::new_v4();
            grant.provider_allowance_id = allowance.allowance_id.clone();
            grant.provider_authority_digest =
                sentinel_workflow::adaptive_leadership_continuation_provider_authority_digest(
                    allowance,
                    &grant.authority,
                )
                .unwrap();
            if mismatched_allowance_digest {
                grant.provider_authority_digest = "0".repeat(64);
            }
            grant.created_at_ms = allowance.created_at_unix_ms;
            grant.deadline_ms = allowance.grant.expires_at_unix_ms;
            let (_, ready) = api
                .store
                .begin_adaptive_session(&grant, &grant.authority, grant.created_at_ms)
                .unwrap();
            let (pending, effect) = claim(&api, &ready);
            let sealed = unknown(&api, &pending, &effect);
            let current = roll_allowance(&api, &sealed);
            let feedback = api
                .store
                .adaptive_recovery_feedback(&grant.authority)
                .unwrap()
                .unwrap();
            assert_eq!(feedback.count, 1);
            assert_eq!(feedback.previous_session_id, rejected.grant.session_id);
            assert_eq!((sealed.model_calls, sealed.tool_calls), (1, 0));
            let before = durable_state(&company_path, &event_path);
            let journal = api
                .store
                .first_unknown_model_journal_evidence(grant.session_id, &grant.authority)
                .unwrap()
                .unwrap();
            assert_eq!(journal.root_grant, grant);
            assert_eq!(journal.effect, effect);
            assert_eq!(journal.observed_head_version, sealed.version);
            let historical = api
                .store
                .historical_adaptive_provider_project(
                    &journal.root_grant,
                    journal.claim.recorded_at_ms,
                )
                .unwrap();
            assert!(select_provider_usage_binding(
                std::slice::from_ref(&current),
                grant.authority.agent_id,
                Some(grant.provider_allowance_id.as_str()),
            )
            .unwrap()
            .is_none());
            if mismatched_allowance_digest {
                assert!(historical.is_none());
                assert_eq!(
                    api.unknown_model_proof_digest(&current, &sealed, &effect),
                    Err("unknown model original allowance unavailable")
                );
            } else {
                // Stop at verified attribution: no /etc proof or inferred context.
                let historical = historical.unwrap();
                let binding = select_provider_usage_binding(
                    std::slice::from_ref(&historical),
                    grant.authority.agent_id,
                    Some(grant.provider_allowance_id.as_str()),
                )
                .unwrap()
                .unwrap();
                assert_eq!(binding.reservation_id, grant.provider_allowance_id);
                assert_eq!(
                    binding.assignment_version,
                    grant.authority.assignment_version
                );
            }
            assert_eq!(
                api.store
                    .adaptive_recovery_feedback(&grant.authority)
                    .unwrap(),
                Some(feedback)
            );
            assert_eq!(
                api.store
                    .adaptive_session(grant.session_id, &grant.authority)
                    .unwrap(),
                Some(sealed)
            );
            assert_eq!(durable_state(&company_path, &event_path), before);
        }
    }
}
