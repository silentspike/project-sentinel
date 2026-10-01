//! Audited review capacity is not a leadership decision or a developer allowance.
use super::*;
use sentinel_workflow::{
    AdaptiveBudgetReviewExtensionReceiptV1, AdaptiveBudgetReviewExtensionRequestV1,
};

fn issuance_receipt(
    receipt: &AdaptiveBudgetReviewExtensionReceiptV1,
    replayed: bool,
) -> WorkflowHttpResponse {
    json(
        200,
        &serde_json::json!({
            "schema_version": 1,
            "operation_id": receipt.request.operation_id,
            "project_id": receipt.request.project_id,
            "session_id": receipt.request.session_id,
            "expected_session_version": receipt.request.expected_session_version,
            "additional_reviews": receipt.request.additional_reviews,
            "expires_at_unix_ms": receipt.request.expires_at_unix_ms,
            "replayed": replayed,
            "receipt_kind": "immutable_issuance",
            "authority": "bounded_normal_leadership_reviews",
            "model_decision_recorded": false,
            "decision_state": "not_asserted_by_issuance_receipt"
        }),
    )
}

impl WorkflowApi {
    pub(super) fn budget_review_extension_http(
        &self,
        principal: &BoundPrincipal,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> WorkflowHttpResponse {
        if principal.principal.kind != CompanyPrincipalKindV1::Operator
            || !matches!(
                principal.principal.role,
                CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
            )
        {
            return json_error(
                403,
                "authority_conflict",
                "operator budget authority required",
                false,
            );
        }
        let Ok(_fence) = self.mutation_fence.write() else {
            return workflow_error(workflow_unavailable());
        };
        if !self.model_work_enabled {
            return workflow_error(workflow_unavailable());
        }
        let now = now_unix_ms();
        if method == "POST" {
            let request: AdaptiveBudgetReviewExtensionRequestV1 = match decode_body(body) {
                Ok(value) => value,
                Err(response) => return response,
            };
            if request.tenant_id != principal.principal.tenant_id {
                return json_error(403, "authority_conflict", "budget tenant mismatch", false);
            }
            // Store replay precedes freshness checks and never replenishes capacity.
            return match self.store.authorize_budget_review_extension(
                &principal.principal,
                &request,
                now,
            ) {
                Ok((replayed, receipt)) => issuance_receipt(&receipt, replayed),
                Err(error) => workflow_error(error),
            };
        }
        let project_id =
            match query_parameter(path, "project_id").and_then(|id| ProjectId::parse(id).ok()) {
                Some(id) => id,
                None => return json_error(400, "invalid_input", "project_id required", false),
            };
        let session_id =
            match query_parameter(path, "session_id").and_then(|id| Uuid::parse_str(id).ok()) {
                Some(id) if !id.is_nil() => id,
                _ => return json_error(400, "invalid_input", "session_id required", false),
            };
        let project = match self
            .store
            .company_project(&principal.principal.tenant_id, &project_id)
        {
            Ok(Some(project)) => project,
            Ok(None) => return json_error(404, "not_found", "budget project missing", false),
            Err(error) => return workflow_error(error),
        };
        let current_version = match self.review_sessions(&project) {
            Ok(sessions) => match sessions.iter().find(|s| s.grant.session_id == session_id) {
                Some(session) => session.version,
                None => return json_error(404, "not_found", "budget session missing", false),
            },
            Err(reason) => return json_error(409, "adaptive_budget_conflict", reason, false),
        };
        let requested_version = match query_parameter(path, "expected_session_version") {
            Some(value) => match value.parse::<u64>() {
                Ok(version) if version > 0 => version,
                _ => return json_error(400, "invalid_input", "invalid session version", false),
            },
            None => current_version,
        };
        match self.store.budget_review_extension(
            &principal.principal.tenant_id,
            session_id,
            requested_version,
        ) {
            Ok(Some(receipt)) if receipt.request.project_id == project_id => {
                return issuance_receipt(&receipt, true);
            }
            Ok(Some(_)) => {
                return json_error(403, "authority_conflict", "budget project mismatch", false)
            }
            Err(error) => return workflow_error(error),
            Ok(None) => {}
        }
        if requested_version != current_version {
            return json_error(
                409,
                "adaptive_budget_conflict",
                "budget session changed",
                false,
            );
        }
        let Some(expires) = now.checked_add(3_600_000) else {
            return workflow_error(workflow_unavailable());
        };
        match self.store.budget_review_extension_draft(
            &principal.principal,
            &project_id,
            session_id,
            Uuid::new_v4(),
            3,
            "maintainer-model-budget-extension",
            expires,
            now,
        ) {
            Ok(request) => json(
                200,
                &serde_json::json!({
                    "schema_version": 1,
                    "requires_explicit_submission": true,
                    "request": request,
                    "model_decision_recorded": false
                }),
            ),
            Err(error) => workflow_error(error),
        }
    }
}
