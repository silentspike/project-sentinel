//! Evidence-gated, same-work corrections; no caller may reset a provider result.
use super::model_execution::{ModelExecutionCompletion, ModelExecutionContext};
use super::*;

#[derive(Deserialize)]
pub(super) struct StoredModelResult {
    pub(super) version: u32,
    pub(super) request_id: String,
    pub(super) request_digest: String,
    pub(super) usage_event: DomainEvent,
    pub(super) model_work: Option<ModelExecutionCompletion>,
}

impl WorkflowApi {
    pub(super) fn correct_model_work(
        &self,
        principal: &BoundPrincipal,
        body: &[u8],
    ) -> WorkflowHttpResponse {
        if principal.principal.kind != CompanyPrincipalKindV1::Agent
            || !matches!(
                principal.principal.role,
                CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
            )
        {
            return json_error(
                403,
                "authority_conflict",
                "project leadership authority required",
                false,
            );
        }
        let envelope: CompanyCommandEnvelope = match decode_body(body) {
            Ok(value) => value,
            Err(response) => return response,
        };
        let CompanyWorkflowCommandV1::RequestWorkCorrection {
            project_id,
            work_item_id,
            execution_revision,
            feedback,
            ..
        } = &envelope.command
        else {
            return json_error(
                400,
                "invalid_input",
                "work correction command required",
                false,
            );
        };
        let Ok(_guard) = self.mutation_fence.write() else {
            return json_error(503, "workflow_busy", "workflow recovery is active", true);
        };
        if feedback.is_none() {
            return json_error(
                400,
                "invalid_input",
                "bounded correction feedback required",
                false,
            );
        }
        let replay = match self
            .store
            .has_company_operation(&principal.principal, envelope.operation_id)
        {
            Ok(value) => value,
            Err(error) => return workflow_error(error),
        };
        if !replay {
            if let Err(error) = self.validate_model_work_correction(
                &principal.principal,
                project_id,
                work_item_id,
                execution_revision,
            ) {
                return json_error(409, "correction_evidence_unavailable", error, false);
            }
        }
        match self.core.apply_company_command(
            &principal.principal,
            envelope.operation_id,
            &envelope.command,
            now_unix_ms(),
        ) {
            Ok(outcome) => company_command_response(&outcome, &principal.principal),
            Err(error) => workflow_error(error),
        }
    }

    pub(super) fn validate_model_work_correction(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        project_id: &ProjectId,
        work_id: &WorkItemId,
        revision: &sentinel_workflow::ExecutionRevisionV1,
    ) -> Result<(), &'static str> {
        self.validate_model_work_result(principal, project_id, work_id, Some(revision))
    }

    pub(super) fn validate_model_work_result(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        project_id: &ProjectId,
        work_id: &WorkItemId,
        revision: Option<&sentinel_workflow::ExecutionRevisionV1>,
    ) -> Result<(), &'static str> {
        let project = self
            .store
            .company_project(&principal.tenant_id, project_id)
            .map_err(|_| "correction project unavailable")?
            .ok_or("correction project missing")?;
        let allowance = project
            .subscription_call
            .as_ref()
            .ok_or("prior provider allowance missing")?;
        self.validate_bound_model_work_result(
            principal, &project, work_id, revision, allowance, false,
        )
    }

    pub(super) fn validate_model_work_for_source_review(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        project: &sentinel_workflow::ProjectV1,
        work_id: &WorkItemId,
    ) -> Result<(), &'static str> {
        let allowance = project
            .subscription_call
            .as_ref()
            .ok_or("source-review provider allowance missing")?;
        self.validate_bound_model_work_result(principal, project, work_id, None, allowance, true)
    }

    pub(super) fn validate_source_review_predecessor(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        project: &sentinel_workflow::ProjectV1,
        work_id: &WorkItemId,
        revision: &sentinel_workflow::ExecutionRevisionV1,
    ) -> Result<(), &'static str> {
        let allowance = project
            .source_review_previous_call
            .as_ref()
            .ok_or("prior source provider allowance missing")?;
        self.validate_bound_model_work_result(
            principal,
            project,
            work_id,
            Some(revision),
            allowance,
            true,
        )
    }

    fn validate_bound_model_work_result(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        project: &sentinel_workflow::ProjectV1,
        work_id: &WorkItemId,
        revision: Option<&sentinel_workflow::ExecutionRevisionV1>,
        allowance: &sentinel_workflow::SubscriptionCallAllowanceV1,
        allow_superseded_candidate: bool,
    ) -> Result<(), &'static str> {
        let tenant = &principal.tenant_id;
        let project_id = &project.project_id;
        if governed_project_participant(project, principal).is_none() {
            return Err("project leadership binding unavailable");
        }
        let delivery = self
            .delivery
            .as_ref()
            .ok_or("delivery exclusion authority unavailable")?;
        let delivery_allowed = if allow_superseded_candidate {
            delivery.permits_new_source_review(&tenant.0, &project_id.0)
        } else {
            delivery
                .contains_project(&tenant.0, &project_id.0)
                .map(|exists| !exists)
        }
        .map_err(|_| "delivery exclusion read failed")?;
        if !delivery_allowed {
            return Err("delivery already consumes this project");
        }
        if allowance.dispatch.is_none() {
            let previous = self
                .store
                .work_item(tenant, project_id, work_id)
                .map_err(|_| "execution predecessor unavailable")?
                .ok_or("execution predecessor missing")?;
            return self.validate_adaptive_source_completion(
                principal, project, allowance, &previous, revision,
            );
        }
        let dispatch = allowance
            .dispatch
            .as_ref()
            .ok_or("prior provider call was not dispatched")?;
        if &allowance.grant.work_item_id != work_id {
            return Err("provider work binding changed");
        }
        let events = self
            .event_store
            .as_ref()
            .ok_or("provider result authority unavailable")?;
        if events
            .has_event_operation_id(&format!("llm_resolution_{}", dispatch.request_id))
            .map_err(|_| "provider resolution read failed")?
        {
            return Err("provider result was operator-resolved");
        }
        let operation = stable_operation_id(
            "sentinel.model-work.v1",
            &format!("{}:{}", dispatch.request_id, dispatch.request_digest),
            1,
        );
        let previous = self
            .store
            .work_item(tenant, project_id, work_id)
            .map_err(|_| "execution predecessor unavailable")?
            .ok_or("execution predecessor missing")?;
        let revision_matches = if let Some(revision) = revision {
            sentinel_workflow::ExecutionRevisionV1::from_completed_work(
                &previous,
                revision.feedback_digest.clone(),
            )
            .map_err(|_| "execution predecessor invalid")?
                == *revision
        } else {
            previous.state == sentinel_workflow::WorkItemState::Done
                && previous.terminal_execution_evidence.is_some()
                && previous
                    .gate_evidence
                    .as_ref()
                    .is_some_and(|gate| gate.passed)
        };
        if previous.plan.plan_id != operation || !revision_matches {
            return Err("provider result was not adopted as this execution");
        }
        let usage_id = format!("llm_usage_{}", dispatch.request_id);
        let committed_usage = events
            .event_by_operation_id(&usage_id)
            .map_err(|_| "provider usage read failed")?
            .ok_or("provider usage not committed")?;
        let expected = ProviderUsageBinding {
            tenant_id: tenant.0.clone(),
            project_id: project_id.0.clone(),
            work_item_id: work_id.0.clone(),
            reservation_id: allowance.allowance_id.clone(),
            assignment_id: allowance.grant.assignment_id.clone(),
            assignment_version: allowance.grant.assignment_version,
            agent_id: allowance.grant.agent_id,
            provider: allowance.grant.provider.clone(),
            subscription_grant: Some(allowance.grant.clone()),
        };
        let payload: DomainEventPayload =
            serde_json::from_str(&committed_usage.payload).map_err(|_| "provider usage invalid")?;
        let DomainEventPayload::AgentLlmUsage { cost_usd, .. } = &payload else {
            return Err("provider usage payload type is invalid");
        };
        // A subscription reserves calls, not zero-valued usage accounting.
        // The canonical event owns the recorded cost; callers cannot supply it.
        let recorded_cost = usd_to_micros(*cost_usd).ok_or("provider usage cost is invalid")?;
        validate_provider_usage_event(&committed_usage, &usage_id, &expected, recorded_cost)?;
        if !matches!(payload, DomainEventPayload::AgentLlmUsage { output_tokens, .. } if output_tokens > 0)
            || committed_usage.correlation_id != dispatch.request_id
        {
            return Err("provider output evidence missing");
        }
        let Some(row) = events
            .get_llm_completion(&dispatch.request_id)
            .map_err(|_| "provider result read failed")?
        else {
            // Normal completion removes the payload. The usage marker and
            // bound completed execution remain; absence alone proves nothing.
            return Ok(());
        };
        if row.status != "action_claimed"
            || row.request_digest != dispatch.request_digest
            || row.request_id != dispatch.request_id
            || row.payload.len() > 1024 * 1024
            || row.owner_scope
                != sentinel_common::StateTransferScope::for_agent(
                    allowance.grant.agent_id.to_string(),
                )
        {
            return Err("provider result is unresolved or changed");
        }
        let result: StoredModelResult =
            serde_json::from_str(&row.payload).map_err(|_| "provider result invalid")?;
        let completion = result.model_work.as_ref().ok_or("model result missing")?;
        let ModelExecutionContext::Project(context) = &completion.context else {
            return Err("provider result belongs to another subject");
        };
        if result.version != 2
            || !completion.admissible
            || result.request_id != dispatch.request_id
            || result.request_digest != dispatch.request_digest
            || context.authority.tenant_id != *tenant
            || context.authority.project_id != *project_id
            || context.authority.work_item_id != *work_id
            || context.binding.reservation_id != allowance.allowance_id
            || context.binding.subscription_grant.as_ref() != Some(&allowance.grant)
            || context.binding.agent_id != allowance.grant.agent_id
        {
            return Err("model result authority mismatch");
        }
        completion.validate_usage(&result.usage_event)?;
        if serde_json::to_value(&committed_usage).map_err(|_| "provider usage invalid")?
            != serde_json::to_value(&result.usage_event).map_err(|_| "provider usage invalid")?
        {
            return Err("provider usage changed");
        }
        let intent = ExecutionIntentV1 {
            project_id: project_id.clone(),
            work_item_id: work_id.clone(),
            tools: super::model_work::parse_proposal(&completion.content)?,
        };
        if previous.plan.plan_id != operation
            || !previous.plan.authority_matches(&context.authority)
            || !execution_intent_matches_plan(&intent, &previous.plan)
        {
            return Err("provider result was not adopted as this execution");
        }
        // The atomic company command also checks the entire terminal receipt chain.
        Ok(())
    }

    pub(super) fn validate_adaptive_source_completion(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        project: &sentinel_workflow::ProjectV1,
        allowance: &sentinel_workflow::SubscriptionCallAllowanceV1,
        execution: &sentinel_workflow::WorkItemExecutionV1,
        revision: Option<&sentinel_workflow::ExecutionRevisionV1>,
    ) -> Result<(), &'static str> {
        if principal.tenant_id != project.tenant_id
            || governed_project_participant(project, principal).is_none()
            || allowance.dispatch.is_some()
            || execution.tenant_id != project.tenant_id
            || execution.project_id != project.project_id
            || execution.work_item_id != allowance.grant.work_item_id
        {
            return Err("adaptive source authority changed");
        }
        let revision_matches = if let Some(revision) = revision {
            sentinel_workflow::ExecutionRevisionV1::from_completed_work(
                execution,
                revision.feedback_digest.clone(),
            )
            .map_err(|_| "execution predecessor invalid")?
                == *revision
        } else {
            execution.state == sentinel_workflow::WorkItemState::Done
                && execution.terminal_execution_evidence.is_some()
                && execution
                    .gate_evidence
                    .as_ref()
                    .is_some_and(|gate| gate.passed)
        };
        if !revision_matches {
            return Err("adaptive source execution is not complete");
        }
        let proof = self
            .store
            .adaptive_source_completion(
                &project.tenant_id,
                &project.project_id,
                allowance,
                execution.plan.plan_id,
            )
            .map_err(|_| "adaptive source completion proof unavailable")?
            .ok_or("adaptive source completion proof missing")?;
        let source = &proof.provider_source;
        if !execution.plan.authority_matches(&source.grant.authority) {
            return Err("adaptive source execution authority changed");
        }
        let binding = super::model_execution::AdaptiveProviderAuthority {
            schema_version: 3,
            grant: source.effective_grant(),
            session_version: source.version,
            effect_id: proof.effect.id,
            assignment_id: allowance.grant.assignment_id.clone(),
            previous_observation: source.last_observation.clone(),
        };
        let observation = self.adaptive_source_package(&proof, execution)?;
        self.validate_adaptive_source_provider(&proof, &binding, &observation)
    }

    fn adaptive_source_package(
        &self,
        proof: &sentinel_workflow::AdaptiveSourceCompletionV1,
        execution: &sentinel_workflow::WorkItemExecutionV1,
    ) -> Result<sentinel_common::WorkbenchPrivateObservation, &'static str> {
        let source = &proof.provider_source;
        let reference = source
            .last_observation
            .as_ref()
            .ok_or("adaptive source package observation missing")?;
        let authority = self
            .authority
            .as_ref()
            .ok_or("adaptive source authority unavailable")?;
        let (profile, digest) = authority
            .profile_for_binding(&source.grant.authority.profile_id)
            .map_err(|_| "adaptive source package profile unavailable")?;
        // Historical evidence reads only: no Poll/Submit, fresh model admission,
        // or renewed execution window is needed to inspect a completed package.
        let record = crate::workbench::read_workbench_invocation_status(
            &reference.effect.id.to_string(),
            authority.as_ref(),
            profile,
            digest,
        )
        .map_err(|_| "adaptive source package record unavailable")?
        .ok_or("adaptive source package record missing")?;
        let observation = crate::workbench::read_workbench_private_observation(
            &reference.effect.id.to_string(),
            authority.as_ref(),
            profile,
            digest,
        )
        .map_err(|_| "adaptive source package observation unavailable")?
        .ok_or("adaptive source package observation missing")?;
        observation
            .validate(
                &reference.effect.id.to_string(),
                &reference.effect.request_digest,
            )
            .map_err(|_| "adaptive source package observation changed")?;
        let [step] = execution.plan.steps.as_slice() else {
            return Err("adaptive source completion plan changed");
        };
        let ExecutionToolV1::PackageArtifact {
            artifact_kind,
            media_type,
            paths,
        } = &step.tool
        else {
            return Err("adaptive source completion tool changed");
        };
        let [artifact] = record.artifacts.as_slice() else {
            return Err("adaptive source package artifact changed");
        };
        let terminal = execution
            .terminal_execution_evidence
            .as_ref()
            .ok_or("adaptive source terminal evidence missing")?;
        let [sealed] = terminal.artifacts.as_slice() else {
            return Err("adaptive source sealed artifact changed");
        };
        if record.state != WorkbenchInvocationState::Succeeded
            || record.agent_id != source.grant.authority.agent_id
            || record.project_id != source.grant.authority.project_id.0
            || record.work_item_id != source.grant.authority.work_item_id.0
            || record.caller_id != source.grant.authority.principal.principal_id
            || record.assignment_version != source.grant.authority.assignment_version
            || record.credential_generation != source.grant.authority.principal.principal_generation
            || record.policy_digest != source.grant.authority.policy_digest
            || record.tool_profile != source.grant.authority.profile_id
            || record.tool_profile_digest != source.grant.authority.profile_digest
            || record.runtime_key != source.grant.authority.runtime_key
            || record.request_digest != reference.effect.request_digest
            || record.result_digest.is_none()
            || record.completed_at_ms.is_none()
            || record.observation_digest.as_deref() != Some(reference.observation_digest.as_str())
            || observation.digest() != reference.observation_digest
            || observation.outcome() != sentinel_common::WorkbenchOutcome::Succeeded
            || observation.artifacts() != record.artifacts.as_slice()
            || artifact.sha256 != proof.artifact_digest
            || artifact.artifact_kind != *artifact_kind
            || artifact.media_type != *media_type
            || record.package_artifact_kind.as_deref() != Some(artifact_kind.as_str())
            || record.package_media_type.as_deref() != Some(media_type.as_str())
            || record.package_paths != *paths
            || sealed.digest != proof.artifact_digest
            || sealed.artifact_kind != *artifact_kind
            || sealed.media_type != *media_type
            || sealed.paths != *paths
        {
            return Err("adaptive source package evidence changed");
        }
        Ok(observation)
    }

    fn validate_adaptive_source_provider(
        &self,
        proof: &sentinel_workflow::AdaptiveSourceCompletionV1,
        binding: &super::model_execution::AdaptiveProviderAuthority,
        observation: &sentinel_common::WorkbenchPrivateObservation,
    ) -> Result<(), &'static str> {
        let request_id = binding.request_id();
        let events = self
            .event_store
            .as_ref()
            .ok_or("adaptive source EventStore unavailable")?;
        if events
            .has_event_operation_id(&format!("llm_resolution_{request_id}"))
            .map_err(|_| "adaptive source resolution unavailable")?
        {
            return Err("adaptive source provider was operator-resolved");
        }
        let usage = events
            .event_by_operation_id(&format!("llm_usage_{request_id}"))
            .map_err(|_| "adaptive source usage unavailable")?
            .ok_or("adaptive source usage missing")?;
        super::model_execution::validate_adaptive_usage_binding(binding, true, &usage)?;
        let Some(row) = events
            .get_llm_completion(&request_id)
            .map_err(|_| "adaptive source provider result unavailable")?
        else {
            // Normal cleanup removes the reservation/payload together. The
            // authenticated journal, execution and canonical usage remain.
            return Ok(());
        };
        let owner = sentinel_common::StateTransferScope::for_agent(
            binding.grant.authority.agent_id.to_string(),
        );
        if row.status != "action_claimed"
            || row.request_id != request_id
            || row.request_digest != proof.effect.request_digest
            || row.owner_scope != owner
            || row.payload.len() > 1024 * 1024
        {
            return Err("adaptive source provider result unresolved or changed");
        }
        let reservation = events
            .llm_model_reservation(&request_id, &owner)
            .map_err(|_| "adaptive source reservation unavailable")?
            .ok_or("adaptive source reservation missing")?;
        let provider =
            super::model_execution::ProviderExecutionAuthority::Adaptive(Box::new(binding.clone()));
        let authority_digest = hex_sha256(
            &serde_json::to_vec(&provider).map_err(|_| "adaptive source authority invalid")?,
        );
        let expected_usage = sentinel_limbo::LlmModelUsageBindingV1 {
            agent_id: binding.grant.authority.agent_id,
            tenant_id: binding.grant.authority.tenant_id.0.clone(),
            project_id: binding.grant.authority.project_id.0.clone(),
            work_item_id: binding.grant.authority.work_item_id.0.clone(),
            reservation_id: binding.grant.provider_allowance_id.clone(),
            assignment_id: binding.assignment_id.clone(),
            assignment_version: binding.grant.authority.assignment_version,
            provider: binding.grant.provider.clone(),
            model: binding.grant.model.clone(),
        };
        if reservation.request_digest != proof.effect.request_digest
            || reservation.request_id != request_id
            || reservation.owner_scope != owner
            || reservation.allowance_id != binding.grant.provider_allowance_id
            || reservation.authority_digest != authority_digest
            || reservation.usage_binding != expected_usage
            || reservation.subject
                != (sentinel_limbo::LlmModelSubjectV1::Adaptive {
                    session_id: binding.grant.session_id,
                    effect_id: binding.effect_id,
                    session_version: binding.session_version,
                })
        {
            return Err("adaptive source reservation changed");
        }
        let stored: StoredModelResult = serde_json::from_str(&row.payload)
            .map_err(|_| "adaptive source provider payload invalid")?;
        let completion = stored
            .model_work
            .as_ref()
            .ok_or("adaptive source model result missing")?;
        let ModelExecutionContext::Adaptive(context) = &completion.context else {
            return Err("adaptive source provider subject changed");
        };
        if stored.version != 2
            || !completion.admissible
            || stored.request_id != request_id
            || stored.request_digest != proof.effect.request_digest
            || context.binding != *binding
            || context.observation.as_ref() != Some(observation)
            || hex_sha256(completion.content.as_bytes()) != proof.model_response_digest
            || hex_sha256(
                &serde_json::to_vec(&completion.context)
                    .map_err(|_| "adaptive source context invalid")?,
            ) != reservation.context_digest
            || super::model_execution::parse_adaptive_decision(&completion.content)?
                != (sentinel_workflow::AdaptiveModelDecisionV1::ProposeCompletion {
                    artifact_digest: proof.artifact_digest.clone(),
                })
            || serde_json::to_value(&stored.usage_event)
                .map_err(|_| "adaptive source usage invalid")?
                != serde_json::to_value(&usage).map_err(|_| "adaptive source usage invalid")?
        {
            return Err("adaptive source retained result changed");
        }
        completion.validate_usage(&usage)?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "work_correction/adaptive_source_tests.rs"]
mod adaptive_source_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correction_route_requires_leadership_and_project_evidence_before_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let api =
            super::super::model_work::configured_test_api(&temp.path().join("company.sqlite"));
        let envelope = serde_json::json!({
            "operation_id": Uuid::new_v4(),
            "command": {
                "command": "request_work_correction", "project_id": "project-m0", "expected_version": 1,
                "work_item_id": "work-m0", "expected_work_version": 1, "feedback_ref": "repair-css",
                "feedback": {"summary": "Review found invalid CSS dimensions.", "artifact_digest": null},
                "execution_revision": {"previous_plan_id": Uuid::new_v4(), "previous_plan_digest": "a".repeat(64),
                    "previous_version": 1, "previous_state_digest": "b".repeat(64), "feedback_digest": "c".repeat(64)}
            }
        });
        let body = serde_json::to_vec(&envelope).unwrap();
        for id in ["customer", "sales", "developer-6", "operator"] {
            let principal = api.principals.principal(id).unwrap();
            assert_eq!(
                api.correct_model_work(&principal, &body).status,
                403,
                "{id}"
            );
        }
        let pm = api.principals.principal("pm").unwrap();
        let response = api.correct_model_work(&pm, &body);
        assert_eq!(response.status, 409);
        assert!(String::from_utf8(response.body)
            .unwrap()
            .contains("correction project missing"));
        assert!(!api
            .store
            .has_company_operation(
                &pm.principal,
                serde_json::from_value(envelope["operation_id"].clone()).unwrap()
            )
            .unwrap());
        assert_eq!(api.correct_model_work(&pm, b"{}").status, 400);
        assert!(is_workflow_path(WORK_CORRECTION_PATH));
    }
}
