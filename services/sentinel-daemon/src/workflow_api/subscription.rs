//! Final dispatch consumes workflow authority, never an in-memory call counter.
use super::*;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DispatchRequest {
    schema_version: u16,
    allowance_id: String,
    agent_id: u16,
    request_id: String,
    request_digest: String,
    context_digest: String,
    provider: String,
    model: String,
    catalog_digest: String,
}

impl WorkflowApi {
    // Only the operator-secret-authenticated route calls this method. Ordinary
    // company APIs cannot submit ClaimSubscriptionCall or choose its principal.
    pub(crate) fn subscription_dispatch(&self, body: &[u8]) -> WorkflowHttpResponse {
        let request: DispatchRequest = match decode_body(body) {
            Ok(request) => request,
            Err(response) => return response,
        };
        match self.claim_subscription_dispatch(&request) {
            Ok(deadline) => json(
                200,
                &serde_json::json!({
                    "schema_version": 1,
                    "allowance_id": request.allowance_id,
                    "request_id": request.request_id,
                    "request_digest": request.request_digest,
                    "deadline_unix_ms": deadline,
                }),
            ),
            Err(_) => json_error(
                403,
                "subscription_dispatch_denied",
                "subscription dispatch authority unavailable or consumed",
                false,
            ),
        }
    }

    fn claim_subscription_dispatch(&self, request: &DispatchRequest) -> Result<u64, &'static str> {
        let _fence = self
            .mutation_fence
            .write()
            .map_err(|_| "workflow recovery active")?;
        let now_ms = now_unix_ms();
        if !self.enabled
            || !self.model_work_enabled
            || request.schema_version != 1
            || self.subscription_allowance_id.as_deref() != Some(request.allowance_id.as_str())
        {
            return Err("subscription mode unavailable");
        }
        let binding = <Self as crate::llm_bridge::bridge::ProviderUsageAuthorityResolver>::resolve_provider_usage_authority(self, AgentId(request.agent_id))?
            .ok_or("subscription binding unavailable")?;
        let grant = binding
            .subscription_grant
            .as_ref()
            .ok_or("subscription grant unavailable")?;
        if binding.reservation_id != request.allowance_id
            || grant.provider != request.provider
            || grant.model != request.model
            || grant.catalog_digest != request.catalog_digest
            || request.request_id != format!("company-provider-{}", request.allowance_id)
        {
            return Err("subscription binding changed");
        }
        let context = self
            .prepare_model_work(&binding)?
            .ok_or("model context unavailable")?;
        context.validate_dispatch(now_ms)?;
        let context_bytes = serde_json::to_vec(&context).map_err(|_| "model context invalid")?;
        if format!("{:x}", Sha256::digest(context_bytes)) != request.context_digest {
            return Err("model context changed");
        }
        let event_store = self
            .event_store
            .as_ref()
            .ok_or("request store unavailable")?;
        let pending = event_store
            .get_llm_completion(&request.request_id)
            .map_err(|_| "request reservation unavailable")?
            .ok_or("request not reserved")?;
        if pending.request_digest != request.request_digest
            || pending.status != "provider_in_flight"
            || !pending.payload.is_empty()
            || pending.owner_scope
                != sentinel_common::StateTransferScope::for_aggregate(&binding.agent_id.to_string())
        {
            return Err("request reservation changed");
        }
        let principal = self
            .principals
            .principal(&context.authority.principal.principal_id)
            .ok_or("subscription principal unavailable")?;
        let tenant = TenantId::parse(&binding.tenant_id).map_err(|_| "invalid tenant")?;
        let project_id = ProjectId::parse(&binding.project_id).map_err(|_| "invalid project")?;
        let project = self
            .store
            .company_project(&tenant, &project_id)
            .map_err(|_| "project unavailable")?
            .ok_or("project missing")?;
        // A fresh operation is deliberate: replaying an HTTP response must not
        // mint another permission after the permanent dispatch tombstone exists.
        let now_ms = now_unix_ms();
        context.validate_dispatch(now_ms)?;
        self.core
            .apply_company_command(
                &principal.principal,
                Uuid::new_v4(),
                &CompanyWorkflowCommandV1::ClaimSubscriptionCall {
                    project_id,
                    expected_version: project.version,
                    allowance_id: request.allowance_id.clone(),
                    request_id: request.request_id.clone(),
                    request_digest: request.request_digest.clone(),
                },
                now_ms,
            )
            .map_err(|_| "subscription claim denied")?;
        Ok(grant
            .expires_at_unix_ms
            .min(now_ms.saturating_add(grant.max_duration_ms)))
    }
}
