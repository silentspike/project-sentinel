//! Authenticated, bounded HTTP adapter for the durable company workflow.
//!
//! This module deliberately exposes commands and read models only. Execution
//! remains behind `WorkExecutionPort`; #694 supplies the production adapter.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use sentinel_workflow::{
    ActorRole, AuthenticatedActor, CustomerRequestId, ProjectId, UnavailableExecutionPort,
    WorkItemId, WorkflowCommand, WorkflowEngine, WorkflowError, WorkflowErrorCode, WorkflowStore,
};
use serde::{Deserialize, Serialize};

pub const CUSTOMER_COMMAND_PATH: &str = "/customer/workflow/commands";
pub const CUSTOMER_REQUEST_PATH: &str = "/customer/workflow/requests";
pub const OPERATOR_COMMAND_PATH: &str = "/operator/workflow/commands";
pub const OPERATOR_PROJECT_PATH: &str = "/operator/workflow/projects";
pub const OPERATOR_WORK_ITEM_PATH: &str = "/operator/workflow/work-items";
pub const OPERATOR_PROJECTION_PATH: &str = "/operator/workflow/projections";
pub const OPERATOR_EVENTS_PATH: &str = "/operator/workflow/events";
pub const MAX_WORKFLOW_BODY_BYTES: usize = 256 * 1024;
const CUSTOMER_KEY_HEADER: &str = "x-sentinel-customer-key";
const CUSTOMER_ID_HEADER: &str = "x-sentinel-customer-id";

#[derive(Debug, Clone, Deserialize)]
struct CommandEnvelope {
    operation_id: String,
    actor: AuthenticatedActor,
    command: WorkflowCommand,
}

#[derive(Debug, Serialize)]
struct PublicError<'a> {
    code: &'a str,
    error: &'a str,
    retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowHttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

pub struct WorkflowApi {
    engine: Arc<WorkflowEngine>,
    customer_secret: Option<String>,
}

impl std::fmt::Debug for WorkflowApi {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkflowApi")
            .finish_non_exhaustive()
    }
}

impl WorkflowApi {
    pub fn open(
        path: impl AsRef<Path>,
        customer_secret: Option<String>,
    ) -> Result<Self, WorkflowError> {
        let store = Arc::new(WorkflowStore::open(path)?);
        Ok(Self {
            engine: Arc::new(WorkflowEngine::new(
                store,
                Arc::new(UnavailableExecutionPort),
            )),
            customer_secret,
        })
    }

    pub fn handle(
        &self,
        method: &str,
        path: &str,
        headers: &HashMap<String, String>,
        body: &[u8],
        operator_authorized: bool,
    ) -> Option<WorkflowHttpResponse> {
        let path_only = path.split('?').next().unwrap_or(path);
        match (method, path_only) {
            ("POST", CUSTOMER_COMMAND_PATH) => Some(self.handle_customer_command(headers, body)),
            ("GET", CUSTOMER_REQUEST_PATH) => Some(self.handle_customer_request(headers, path)),
            ("POST", OPERATOR_COMMAND_PATH) => {
                Some(self.handle_operator_command(body, operator_authorized))
            }
            ("GET", OPERATOR_PROJECT_PATH) => Some(if operator_authorized {
                self.handle_project(path)
            } else {
                unauthorized()
            }),
            ("GET", OPERATOR_WORK_ITEM_PATH) => Some(if operator_authorized {
                self.handle_work_item(path)
            } else {
                unauthorized()
            }),
            ("GET", OPERATOR_PROJECTION_PATH) => Some(if operator_authorized {
                self.handle_projection(path)
            } else {
                unauthorized()
            }),
            ("GET", OPERATOR_EVENTS_PATH) => Some(if operator_authorized {
                self.handle_events(path)
            } else {
                unauthorized()
            }),
            _ if is_workflow_path(path_only) => Some(json_error(
                405,
                "method_not_allowed",
                "workflow endpoint does not accept this method",
                false,
            )),
            _ => None,
        }
    }

    fn handle_customer_command(
        &self,
        headers: &HashMap<String, String>,
        body: &[u8],
    ) -> WorkflowHttpResponse {
        let Some(customer_id) = self.authorized_customer(headers) else {
            return if self.customer_secret.is_some() {
                unauthorized()
            } else {
                json_error(
                    503,
                    "customer_auth_unavailable",
                    "customer API authentication is not configured",
                    true,
                )
            };
        };
        let envelope: CommandEnvelope = match serde_json::from_slice(body) {
            Ok(value) => value,
            Err(_) => {
                return json_error(
                    400,
                    "invalid_input",
                    "invalid workflow command envelope",
                    false,
                )
            }
        };
        if envelope.actor.role != ActorRole::Customer
            || envelope.actor.customer_id.as_deref() != Some(customer_id)
            || envelope.actor.agent_id.is_some()
        {
            return unauthorized();
        }
        match self
            .engine
            .execute(envelope.actor, &envelope.operation_id, envelope.command)
        {
            Ok(outcome) => json(200, &outcome),
            Err(error) => workflow_error(error),
        }
    }

    fn handle_operator_command(
        &self,
        body: &[u8],
        operator_authorized: bool,
    ) -> WorkflowHttpResponse {
        if !operator_authorized {
            return unauthorized();
        }
        let envelope: CommandEnvelope = match serde_json::from_slice(body) {
            Ok(value) => value,
            Err(_) => {
                return json_error(
                    400,
                    "invalid_input",
                    "invalid workflow command envelope",
                    false,
                )
            }
        };
        if !envelope.actor.role.is_internal()
            || envelope.actor.customer_id.is_some()
            || envelope.actor.agent_id.is_none()
        {
            return unauthorized();
        }
        match self
            .engine
            .execute(envelope.actor, &envelope.operation_id, envelope.command)
        {
            Ok(outcome) => json(200, &outcome),
            Err(error) => workflow_error(error),
        }
    }

    fn handle_customer_request(
        &self,
        headers: &HashMap<String, String>,
        path: &str,
    ) -> WorkflowHttpResponse {
        let Some(customer_id) = self.authorized_customer(headers) else {
            return if self.customer_secret.is_some() {
                unauthorized()
            } else {
                json_error(
                    503,
                    "customer_auth_unavailable",
                    "customer API authentication is not configured",
                    true,
                )
            };
        };
        let Some(request_id) = query(path).get("request_id").cloned() else {
            return json_error(400, "invalid_input", "request_id is required", false);
        };
        match self.engine.customer_request(&CustomerRequestId(request_id)) {
            Ok(Some(request)) if request.customer_id == customer_id => json(200, &request),
            Ok(Some(_)) => unauthorized(),
            Ok(None) => json_error(404, "not_found", "customer request was not found", false),
            Err(error) => workflow_error(error),
        }
    }

    fn handle_project(&self, path: &str) -> WorkflowHttpResponse {
        let Some(project_id) = query(path).get("project_id").cloned() else {
            return json_error(400, "invalid_input", "project_id is required", false);
        };
        match self.engine.project(&ProjectId(project_id)) {
            Ok(Some(project)) => json(200, &project),
            Ok(None) => json_error(404, "not_found", "project was not found", false),
            Err(error) => workflow_error(error),
        }
    }

    fn handle_work_item(&self, path: &str) -> WorkflowHttpResponse {
        let Some(work_item_id) = query(path).get("work_item_id").cloned() else {
            return json_error(400, "invalid_input", "work_item_id is required", false);
        };
        match self.engine.work_item(&WorkItemId(work_item_id)) {
            Ok(Some(work_item)) => json(200, &work_item),
            Ok(None) => json_error(404, "not_found", "work item was not found", false),
            Err(error) => workflow_error(error),
        }
    }

    fn handle_projection(&self, path: &str) -> WorkflowHttpResponse {
        let Some(project_id) = query(path).get("project_id").cloned() else {
            return json_error(400, "invalid_input", "project_id is required", false);
        };
        match self.engine.project_projection(&ProjectId(project_id)) {
            Ok(Some(projection)) => json(200, &projection),
            Ok(None) => json_error(404, "not_found", "project projection was not found", false),
            Err(error) => workflow_error(error),
        }
    }

    fn handle_events(&self, path: &str) -> WorkflowHttpResponse {
        let params = query(path);
        let after = match params.get("after").map(|value| value.parse::<i64>()) {
            Some(Ok(value)) if value >= 0 => value,
            None => 0,
            _ => return json_error(400, "invalid_input", "after must be non-negative", false),
        };
        let limit = match params.get("limit").map(|value| value.parse::<usize>()) {
            Some(Ok(value)) if (1..=1_000).contains(&value) => value,
            None => 100,
            _ => {
                return json_error(
                    400,
                    "invalid_input",
                    "limit must be between 1 and 1000",
                    false,
                )
            }
        };
        match self.engine.events_since(after, limit) {
            Ok(events) => json(200, &events),
            Err(error) => workflow_error(error),
        }
    }

    fn authorized_customer<'a>(&self, headers: &'a HashMap<String, String>) -> Option<&'a str> {
        let expected = self.customer_secret.as_deref()?;
        let supplied = headers
            .get(CUSTOMER_KEY_HEADER)
            .map(String::as_str)
            .or_else(|| {
                headers
                    .get("authorization")
                    .and_then(|value| value.strip_prefix("Bearer "))
            });
        if supplied != Some(expected) {
            return None;
        }
        headers.get(CUSTOMER_ID_HEADER).map(String::as_str)
    }
}

pub fn is_workflow_path(path: &str) -> bool {
    path.starts_with("/customer/workflow/") || path.starts_with("/operator/workflow/")
}

fn query(path: &str) -> HashMap<String, String> {
    path.split_once('?')
        .map(|(_, query)| {
            query
                .split('&')
                .filter_map(|pair| pair.split_once('='))
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect()
        })
        .unwrap_or_default()
}

fn workflow_error(error: WorkflowError) -> WorkflowHttpResponse {
    let (status, code) = match error.code {
        WorkflowErrorCode::InvalidInput | WorkflowErrorCode::DagInvalid => (400, "invalid_input"),
        WorkflowErrorCode::NotFound => (404, "not_found"),
        WorkflowErrorCode::Unauthorized => (401, "unauthorized"),
        WorkflowErrorCode::PersistenceFailure | WorkflowErrorCode::ExecutionUnavailable => {
            (503, "service_unavailable")
        }
        WorkflowErrorCode::InvalidTransition
        | WorkflowErrorCode::VersionConflict
        | WorkflowErrorCode::IdempotencyConflict
        | WorkflowErrorCode::DigestConflict
        | WorkflowErrorCode::CapabilityDenied
        | WorkflowErrorCode::BudgetExceeded => (409, "conflict"),
    };
    json_error(status, code, &error.message, error.retryable)
}

fn unauthorized() -> WorkflowHttpResponse {
    json_error(401, "unauthorized", "workflow authentication failed", false)
}

fn json_error(status: u16, code: &str, error: &str, retryable: bool) -> WorkflowHttpResponse {
    json(
        status,
        &PublicError {
            code,
            error,
            retryable,
        },
    )
}

fn json<T: Serialize>(status: u16, payload: &T) -> WorkflowHttpResponse {
    WorkflowHttpResponse {
        status,
        body: serde_json::to_vec(payload).unwrap_or_else(|_| {
            br#"{"code":"serialization_failure","error":"response serialization failed","retryable":true}"#
                .to_vec()
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn headers(customer_id: &str) -> HashMap<String, String> {
        HashMap::from([
            (CUSTOMER_KEY_HEADER.into(), "customer-secret".into()),
            (CUSTOMER_ID_HEADER.into(), customer_id.into()),
        ])
    }

    #[test]
    fn customer_api_is_authenticated_bounded_and_tenant_isolated() {
        let api = WorkflowApi::open(":memory:", Some("customer-secret".into())).expect("API");
        let envelope = json!({
            "operation_id": "customer-submit-1",
            "actor": {
                "actor_id": "customer-user-a",
                "role": "customer",
                "customer_id": "customer-a"
            },
            "command": {
                "command": "submit_customer_request",
                "customer_id": "customer-a",
                "summary_ref": "sha256:public-safe-summary",
                "desired_outcome": "A bounded website",
                "constraints": ["No unrestricted uploads"]
            }
        });
        let body = serde_json::to_vec(&envelope).expect("JSON");
        let unauthorized = api
            .handle("POST", CUSTOMER_COMMAND_PATH, &HashMap::new(), &body, false)
            .expect("workflow response");
        assert_eq!(unauthorized.status, 401);
        let created = api
            .handle(
                "POST",
                CUSTOMER_COMMAND_PATH,
                &headers("customer-a"),
                &body,
                false,
            )
            .expect("workflow response");
        assert_eq!(created.status, 200);
        let value: serde_json::Value =
            serde_json::from_slice(&created.body).expect("response JSON");
        let request_id = value["response"]["value"]["id"]
            .as_str()
            .expect("request ID");
        let denied = api
            .handle(
                "GET",
                &format!("{CUSTOMER_REQUEST_PATH}?request_id={request_id}"),
                &headers("customer-b"),
                &[],
                false,
            )
            .expect("workflow response");
        assert_eq!(denied.status, 401);
    }

    #[test]
    fn operator_api_rejects_customer_actors_and_chat_has_no_workflow_route() {
        let api = WorkflowApi::open(":memory:", Some("customer-secret".into())).expect("API");
        let body = serde_json::to_vec(&json!({
            "operation_id": "operator-spoof",
            "actor": {
                "actor_id": "customer-user-a",
                "role": "customer",
                "customer_id": "customer-a"
            },
            "command": {
                "command": "submit_customer_request",
                "customer_id": "customer-a",
                "summary_ref": "ref",
                "desired_outcome": "result",
                "constraints": []
            }
        }))
        .expect("JSON");
        assert_eq!(
            api.handle("POST", OPERATOR_COMMAND_PATH, &HashMap::new(), &body, true,)
                .expect("workflow response")
                .status,
            401
        );
        assert!(api
            .handle("POST", "/operator/chat", &HashMap::new(), &[], true,)
            .is_none());
    }
}
