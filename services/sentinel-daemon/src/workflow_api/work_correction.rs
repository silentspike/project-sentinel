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
        let tenant = &principal.tenant_id;
        let project = self
            .store
            .company_project(tenant, project_id)
            .map_err(|_| "correction project unavailable")?
            .ok_or("correction project missing")?;
        if governed_project_participant(&project, principal).is_none() {
            return Err("project leadership binding unavailable");
        }
        let delivery = self
            .delivery
            .as_ref()
            .ok_or("delivery exclusion authority unavailable")?;
        if delivery
            .contains_project(&tenant.0, &project_id.0)
            .map_err(|_| "delivery exclusion read failed")?
        {
            return Err("delivery already consumes this project");
        }
        let allowance = project
            .subscription_call
            .as_ref()
            .ok_or("prior provider allowance missing")?;
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
}

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
