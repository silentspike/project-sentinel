//! Authenticated, bounded HTTP adapter for the durable company workflow.
//!
//! This module deliberately exposes commands and read models only. Execution
//! remains behind `WorkExecutionPort`; #694 supplies the production adapter.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sentinel_workflow::{
    AuthenticatedPrincipal, CanonicalWorkProfile, CustomerRequestId, DependencyReadiness,
    PrincipalKind, ProjectId, UnavailableCompletionEvidencePort, UnavailableExecutionPort,
    UnavailableOrganizationRuntimePort, WorkItemId, WorkflowCommand, WorkflowEngine, WorkflowError,
    WorkflowErrorCode, WorkflowStore,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const CUSTOMER_COMMAND_PATH: &str = "/customer/workflow/commands";
pub const CUSTOMER_REQUEST_PATH: &str = "/customer/workflow/requests";
pub const OPERATOR_COMMAND_PATH: &str = "/operator/workflow/commands";
pub const AGENT_COMMAND_PATH: &str = "/agent/workflow/commands";
pub const OPERATOR_PROJECT_PATH: &str = "/operator/workflow/projects";
pub const OPERATOR_WORK_ITEM_PATH: &str = "/operator/workflow/work-items";
pub const OPERATOR_PROJECTION_PATH: &str = "/operator/workflow/projections";
pub const OPERATOR_EVENTS_PATH: &str = "/operator/workflow/events";
pub const MAX_WORKFLOW_BODY_BYTES: usize = 256 * 1024;

fn work_profile_path() -> PathBuf {
    if let Ok(path) = std::env::var("SENTINEL_WORK_PROFILE_FILE") {
        return path.into();
    }
    #[cfg(test)]
    {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/work-profiles/web-project-v1.toml")
    }
    #[cfg(not(test))]
    {
        "config/work-profiles/web-project-v1.toml".into()
    }
}

fn workflow_enabled() -> Result<bool, WorkflowError> {
    match std::env::var("SENTINEL_COMPANY_WORKFLOW_ENABLED") {
        Err(std::env::VarError::NotPresent) => parse_workflow_enabled(None),
        Ok(value) => parse_workflow_enabled(Some(&value)),
        _ => Err(WorkflowError::new(
            WorkflowErrorCode::PersistenceFailure,
            false,
            "company workflow enablement flag is invalid",
        )),
    }
}

fn parse_workflow_enabled(value: Option<&str>) -> Result<bool, WorkflowError> {
    match value.map(str::trim) {
        None | Some("0" | "false" | "FALSE") => Ok(false),
        Some("1" | "true" | "TRUE") => Ok(true),
        Some(_) => Err(WorkflowError::new(
            WorkflowErrorCode::PersistenceFailure,
            false,
            "company workflow enablement flag is invalid",
        )),
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandEnvelope {
    operation_id: String,
    command: WorkflowCommand,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrincipalBindingFile {
    bindings: Vec<PrincipalBinding>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrincipalBinding {
    credential_env: String,
    principal: AuthenticatedPrincipal,
}

#[derive(Debug, Default)]
struct PrincipalAuthenticator {
    by_credential_digest: HashMap<String, AuthenticatedPrincipal>,
}

impl PrincipalAuthenticator {
    fn from_file(path: Option<PathBuf>) -> Result<Self, WorkflowError> {
        let Some(path) = path else {
            return Ok(Self::default());
        };
        let bytes = std::fs::read(path).map_err(|_| auth_configuration_error())?;
        let file: PrincipalBindingFile =
            serde_json::from_slice(&bytes).map_err(|_| auth_configuration_error())?;
        let mut credentials = Vec::with_capacity(file.bindings.len());
        for binding in file.bindings {
            let credential =
                std::env::var(&binding.credential_env).map_err(|_| auth_configuration_error())?;
            credentials.push((credential, binding.principal));
        }
        Self::new(credentials)
    }

    fn new(credentials: Vec<(String, AuthenticatedPrincipal)>) -> Result<Self, WorkflowError> {
        let mut by_credential_digest = HashMap::new();
        for (credential, principal) in credentials {
            validate_principal_binding(&principal)?;
            if credential.len() < 32 {
                return Err(auth_configuration_error());
            }
            if by_credential_digest
                .insert(credential_digest(&credential), principal)
                .is_some()
            {
                return Err(auth_configuration_error());
            }
        }
        Ok(Self {
            by_credential_digest,
        })
    }

    fn authenticate(&self, headers: &HashMap<String, String>) -> Option<AuthenticatedPrincipal> {
        let credential = headers.get("authorization")?.strip_prefix("Bearer ")?;
        self.by_credential_digest
            .get(&credential_digest(credential))
            .cloned()
    }

    fn configured(&self) -> bool {
        !self.by_credential_digest.is_empty()
    }
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
    principals: Arc<PrincipalAuthenticator>,
    enabled: bool,
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
        principal_bindings_file: Option<PathBuf>,
    ) -> Result<Self, WorkflowError> {
        Self::open_configured(
            path,
            workflow_enabled()?,
            Some(work_profile_path()),
            principal_bindings_file,
        )
    }

    fn open_configured(
        path: impl AsRef<Path>,
        enabled: bool,
        profile_file: Option<PathBuf>,
        principal_bindings_file: Option<PathBuf>,
    ) -> Result<Self, WorkflowError> {
        if !enabled {
            let store = Arc::new(WorkflowStore::open(":memory:")?);
            return Ok(Self {
                engine: Arc::new(WorkflowEngine::with_ports(
                    store,
                    Arc::new(UnavailableExecutionPort),
                    Arc::new(UnavailableOrganizationRuntimePort),
                    Arc::new(UnavailableCompletionEvidencePort),
                    Arc::new(CanonicalWorkProfile::embedded()?),
                )),
                principals: Arc::new(PrincipalAuthenticator::default()),
                enabled: false,
            });
        }
        let profile_file = profile_file.ok_or_else(auth_configuration_error)?;
        let principal_bindings_file =
            principal_bindings_file.ok_or_else(auth_configuration_error)?;
        let profile = Arc::new(CanonicalWorkProfile::load_verified(profile_file)?);
        let store = Arc::new(WorkflowStore::open(path)?);
        Ok(Self {
            engine: Arc::new(WorkflowEngine::with_ports(
                store,
                Arc::new(UnavailableExecutionPort),
                Arc::new(UnavailableOrganizationRuntimePort),
                Arc::new(UnavailableCompletionEvidencePort),
                profile,
            )),
            principals: Arc::new(PrincipalAuthenticator::from_file(Some(
                principal_bindings_file,
            ))?),
            enabled: true,
        })
    }

    #[cfg(test)]
    fn with_dependencies(
        path: impl AsRef<Path>,
        principals: PrincipalAuthenticator,
        execution_port: Arc<dyn sentinel_workflow::WorkExecutionPort>,
        organization_port: Arc<dyn sentinel_workflow::OrganizationRuntimePort>,
        completion_port: Arc<dyn sentinel_workflow::CompletionEvidencePort>,
    ) -> Result<Self, WorkflowError> {
        let store = Arc::new(WorkflowStore::open(path)?);
        Ok(Self {
            engine: Arc::new(WorkflowEngine::with_ports(
                store,
                execution_port,
                organization_port,
                completion_port,
                Arc::new(CanonicalWorkProfile::embedded()?),
            )),
            principals: Arc::new(principals),
            enabled: true,
        })
    }

    pub fn handle(
        &self,
        method: &str,
        path: &str,
        headers: &HashMap<String, String>,
        body: &[u8],
    ) -> Option<WorkflowHttpResponse> {
        let path_only = path.split('?').next().unwrap_or(path);
        if !self.enabled && is_workflow_path(path_only) {
            return Some(json_error(
                503,
                "workflow_unavailable",
                "company workflow is disabled or not provisioned",
                true,
            ));
        }
        match (method, path_only) {
            ("POST", CUSTOMER_COMMAND_PATH) => Some(self.handle_customer_command(headers, body)),
            ("GET", CUSTOMER_REQUEST_PATH) => Some(self.handle_customer_request(headers, path)),
            ("POST", OPERATOR_COMMAND_PATH) => {
                Some(self.handle_internal_command(headers, body, PrincipalKind::Operator))
            }
            ("POST", AGENT_COMMAND_PATH) => {
                Some(self.handle_internal_command(headers, body, PrincipalKind::Agent))
            }
            ("GET", OPERATOR_PROJECT_PATH) => Some(self.handle_project(headers, path)),
            ("GET", OPERATOR_WORK_ITEM_PATH) => Some(self.handle_work_item(headers, path)),
            ("GET", OPERATOR_PROJECTION_PATH) => Some(self.handle_projection(headers, path)),
            ("GET", OPERATOR_EVENTS_PATH) => Some(self.handle_events(headers, path)),
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
        let Some(principal) = self.authenticate_kind(headers, PrincipalKind::Customer) else {
            return self.authentication_failure();
        };
        if let Some(response) = self.mutation_gate() {
            return response;
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
        match self
            .engine
            .execute(principal, &envelope.operation_id, envelope.command)
        {
            Ok(outcome) => json(200, &outcome),
            Err(error) => workflow_error(error),
        }
    }

    fn handle_internal_command(
        &self,
        headers: &HashMap<String, String>,
        body: &[u8],
        kind: PrincipalKind,
    ) -> WorkflowHttpResponse {
        let Some(principal) = self.authenticate_kind(headers, kind) else {
            return self.authentication_failure();
        };
        if let Some(response) = self.mutation_gate() {
            return response;
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
        match self
            .engine
            .execute(principal, &envelope.operation_id, envelope.command)
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
        let Some(principal) = self.authenticate_kind(headers, PrincipalKind::Customer) else {
            return self.authentication_failure();
        };
        let Some(request_id) = query(path).get("request_id").cloned() else {
            return json_error(400, "invalid_input", "request_id is required", false);
        };
        match self
            .engine
            .customer_request_for(&principal, &CustomerRequestId(request_id))
        {
            Ok(Some(request)) => json(200, &request),
            Ok(None) => json_error(404, "not_found", "customer request was not found", false),
            Err(error) => workflow_error(error),
        }
    }

    fn handle_project(
        &self,
        headers: &HashMap<String, String>,
        path: &str,
    ) -> WorkflowHttpResponse {
        let Some(principal) = self.authenticate_kind(headers, PrincipalKind::Operator) else {
            return self.authentication_failure();
        };
        let Some(project_id) = query(path).get("project_id").cloned() else {
            return json_error(400, "invalid_input", "project_id is required", false);
        };
        match self.engine.project_for(&principal, &ProjectId(project_id)) {
            Ok(Some(project)) => json(200, &project),
            Ok(None) => json_error(404, "not_found", "project was not found", false),
            Err(error) => workflow_error(error),
        }
    }

    fn handle_work_item(
        &self,
        headers: &HashMap<String, String>,
        path: &str,
    ) -> WorkflowHttpResponse {
        let Some(principal) = self.authenticate_kind(headers, PrincipalKind::Operator) else {
            return self.authentication_failure();
        };
        let Some(work_item_id) = query(path).get("work_item_id").cloned() else {
            return json_error(400, "invalid_input", "work_item_id is required", false);
        };
        match self
            .engine
            .work_item_for(&principal, &WorkItemId(work_item_id))
        {
            Ok(Some(work_item)) => json(200, &work_item),
            Ok(None) => json_error(404, "not_found", "work item was not found", false),
            Err(error) => workflow_error(error),
        }
    }

    fn handle_projection(
        &self,
        headers: &HashMap<String, String>,
        path: &str,
    ) -> WorkflowHttpResponse {
        let Some(principal) = self.authenticate_kind(headers, PrincipalKind::Operator) else {
            return self.authentication_failure();
        };
        let Some(project_id) = query(path).get("project_id").cloned() else {
            return json_error(400, "invalid_input", "project_id is required", false);
        };
        match self
            .engine
            .project_projection_for(&principal, &ProjectId(project_id))
        {
            Ok(Some(projection)) => json(200, &projection),
            Ok(None) => json_error(404, "not_found", "project projection was not found", false),
            Err(error) => workflow_error(error),
        }
    }

    fn handle_events(&self, headers: &HashMap<String, String>, path: &str) -> WorkflowHttpResponse {
        let Some(principal) = self.authenticate_kind(headers, PrincipalKind::Operator) else {
            return self.authentication_failure();
        };
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
        match self.engine.events_since(&principal, after, limit) {
            Ok(events) => json(200, &events),
            Err(error) => workflow_error(error),
        }
    }

    fn authenticate_kind(
        &self,
        headers: &HashMap<String, String>,
        expected: PrincipalKind,
    ) -> Option<AuthenticatedPrincipal> {
        self.principals
            .authenticate(headers)
            .filter(|principal| principal.kind == expected)
    }

    fn authentication_failure(&self) -> WorkflowHttpResponse {
        if self.principals.configured() {
            unauthorized()
        } else {
            json_error(
                503,
                "principal_auth_unavailable",
                "workflow principal authentication is not configured",
                true,
            )
        }
    }

    fn mutation_gate(&self) -> Option<WorkflowHttpResponse> {
        (self.engine.mutation_readiness() != DependencyReadiness::Ready).then(|| {
            json_error(
                503,
                "dispatcher_not_ready",
                "workflow mutations are disabled until the production dispatcher is ready",
                true,
            )
        })
    }
}

pub fn is_workflow_path(path: &str) -> bool {
    path.starts_with("/customer/workflow/")
        || path.starts_with("/operator/workflow/")
        || path.starts_with("/agent/workflow/")
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

fn credential_digest(credential: &str) -> String {
    use std::fmt::Write as _;

    let mut digest = String::with_capacity(64);
    for byte in Sha256::digest(credential.as_bytes()) {
        let _ = write!(&mut digest, "{byte:02x}");
    }
    digest
}

fn validate_principal_binding(principal: &AuthenticatedPrincipal) -> Result<(), WorkflowError> {
    let text_valid = |value: &str| !value.trim().is_empty() && value.len() <= 4_096;
    if !text_valid(&principal.principal_id) || !text_valid(&principal.tenant_id) {
        return Err(auth_configuration_error());
    }
    match principal.kind {
        PrincipalKind::Customer
            if principal.role == sentinel_workflow::ActorRole::Customer
                && principal.customer_id.as_deref().is_some_and(text_valid)
                && principal.agent_id.is_none() =>
        {
            Ok(())
        }
        PrincipalKind::Operator | PrincipalKind::Agent
            if principal.role.is_internal()
                && principal.customer_id.is_none()
                && principal.agent_id.is_some() =>
        {
            Ok(())
        }
        _ => Err(auth_configuration_error()),
    }
}

fn auth_configuration_error() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::PersistenceFailure,
        false,
        "workflow principal authentication configuration is invalid",
    )
}

fn workflow_error(error: WorkflowError) -> WorkflowHttpResponse {
    let (status, code) = match error.code {
        WorkflowErrorCode::InvalidInput | WorkflowErrorCode::DagInvalid => (400, "invalid_input"),
        WorkflowErrorCode::NotFound => (404, "not_found"),
        WorkflowErrorCode::Unauthorized => (401, "unauthorized"),
        WorkflowErrorCode::PersistenceFailure | WorkflowErrorCode::ExecutionUnavailable => {
            (503, "service_unavailable")
        }
        WorkflowErrorCode::DispatcherNotReady => (503, "dispatcher_not_ready"),
        WorkflowErrorCode::BackupVerificationFailed => (409, "backup_verification_failed"),
        WorkflowErrorCode::InvalidTransition
        | WorkflowErrorCode::VersionConflict
        | WorkflowErrorCode::IdempotencyConflict
        | WorkflowErrorCode::DigestConflict
        | WorkflowErrorCode::CapabilityDenied
        | WorkflowErrorCode::OrganizationAuthorityConflict
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
    use sentinel_workflow::{
        AgentId, CompletionAuthorityReceipt, CompletionEvidencePort, CompletionReceiptQuery,
        ExecutionReceipt, OrganizationAgentSnapshot, OrganizationRuntimePort, WorkExecutionError,
        WorkExecutionPort,
    };
    use serde_json::json;

    const CUSTOMER_A_TOKEN: &str = "customer-a-token-that-is-long-enough";
    const CUSTOMER_B_TOKEN: &str = "customer-b-token-that-is-long-enough";
    const OPERATOR_TOKEN: &str = "operator-token-that-is-long-enough-1";
    const AGENT_TOKEN: &str = "agent-token-that-is-long-enough-0001";

    fn canonical_profile_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/work-profiles/web-project-v1.toml")
    }

    fn principal_file(directory: &Path, env_name: &str) -> PathBuf {
        let path = directory.join("principals.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({
                "bindings": [{
                    "credential_env": env_name,
                    "principal": {
                        "principal_id": "startup-customer",
                        "tenant_id": "startup-tenant",
                        "kind": "customer",
                        "role": "customer",
                        "customer_id": "startup-customer-id"
                    }
                }]
            }))
            .expect("principal JSON"),
        )
        .expect("write principals");
        path
    }

    #[derive(Debug)]
    struct ReadyExecution;

    impl WorkExecutionPort for ReadyExecution {
        fn readiness(&self) -> DependencyReadiness {
            DependencyReadiness::Ready
        }

        fn reserve(
            &self,
            request: &sentinel_workflow::PendingExecution,
        ) -> Result<ExecutionReceipt, WorkExecutionError> {
            Ok(ExecutionReceipt {
                invocation_id: request.invocation_id.clone(),
                accepted: true,
            })
        }
    }

    #[derive(Debug)]
    struct ReadyOrganization;

    impl OrganizationRuntimePort for ReadyOrganization {
        fn readiness(&self) -> DependencyReadiness {
            DependencyReadiness::Ready
        }

        fn agent_snapshot(
            &self,
            _agent_id: AgentId,
        ) -> Result<OrganizationAgentSnapshot, WorkExecutionError> {
            Err(WorkExecutionError::Unavailable)
        }
    }

    #[derive(Debug)]
    struct ReadyCompletion;

    impl CompletionEvidencePort for ReadyCompletion {
        fn readiness(&self) -> DependencyReadiness {
            DependencyReadiness::Ready
        }

        fn completion_receipt(
            &self,
            _query: &CompletionReceiptQuery,
        ) -> Result<Box<dyn CompletionAuthorityReceipt>, WorkExecutionError> {
            Err(WorkExecutionError::Unavailable)
        }
    }

    fn principal(principal_id: &str, tenant_id: &str, customer_id: &str) -> AuthenticatedPrincipal {
        AuthenticatedPrincipal {
            principal_id: principal_id.into(),
            tenant_id: tenant_id.into(),
            kind: PrincipalKind::Customer,
            role: sentinel_workflow::ActorRole::Customer,
            customer_id: Some(customer_id.into()),
            agent_id: None,
        }
    }

    fn operator() -> AuthenticatedPrincipal {
        AuthenticatedPrincipal {
            principal_id: "operator-a".into(),
            tenant_id: "tenant-a".into(),
            kind: PrincipalKind::Operator,
            role: sentinel_workflow::ActorRole::ProjectManager,
            customer_id: None,
            agent_id: Some(AgentId(9)),
        }
    }

    fn agent_principal() -> AuthenticatedPrincipal {
        AuthenticatedPrincipal {
            principal_id: "agent-a".into(),
            tenant_id: "tenant-a".into(),
            kind: PrincipalKind::Agent,
            role: sentinel_workflow::ActorRole::Developer,
            customer_id: None,
            agent_id: Some(AgentId(2)),
        }
    }

    fn api(execution_ready: bool) -> WorkflowApi {
        let principals = PrincipalAuthenticator::new(vec![
            (
                CUSTOMER_A_TOKEN.into(),
                principal("customer-principal-a", "tenant-a", "customer-a"),
            ),
            (
                CUSTOMER_B_TOKEN.into(),
                principal("customer-principal-b", "tenant-b", "customer-b"),
            ),
            (OPERATOR_TOKEN.into(), operator()),
            (AGENT_TOKEN.into(), agent_principal()),
        ])
        .expect("principal registry");
        let execution: Arc<dyn WorkExecutionPort> = if execution_ready {
            Arc::new(ReadyExecution)
        } else {
            Arc::new(UnavailableExecutionPort)
        };
        WorkflowApi::with_dependencies(
            ":memory:",
            principals,
            execution,
            Arc::new(ReadyOrganization),
            Arc::new(ReadyCompletion),
        )
        .expect("API")
    }

    #[test]
    fn workflow_is_default_off_and_unprovisioned_start_is_non_fatal() {
        assert!(!parse_workflow_enabled(None).expect("absent flag is disabled"));
        assert!(!parse_workflow_enabled(Some("false")).expect("explicit false is disabled"));
        assert!(parse_workflow_enabled(Some("true")).expect("explicit true is enabled"));
        assert!(parse_workflow_enabled(Some("enabled")).is_err());

        let api = WorkflowApi::open_configured(
            "/definitely/not/created/workflow.sqlite",
            false,
            Some("/missing/profile.toml".into()),
            Some("/missing/principals.json".into()),
        )
        .expect("disabled workflow must not touch provisioning");
        let response = api
            .handle("GET", OPERATOR_PROJECT_PATH, &HashMap::new(), &[])
            .expect("typed workflow response");
        assert_eq!(response.status, 503);
        let value: serde_json::Value = serde_json::from_slice(&response.body).expect("JSON");
        assert_eq!(value["code"], "workflow_unavailable");
    }

    #[test]
    fn enabled_start_fails_closed_for_missing_or_modified_profile_and_principals() {
        let directory = tempfile::tempdir().expect("tempdir");
        let missing_profile = WorkflowApi::open_configured(
            directory.path().join("missing-profile.db"),
            true,
            Some(directory.path().join("missing-profile.toml")),
            Some(directory.path().join("principals.json")),
        )
        .expect_err("enabled workflow requires deployed canonical profile");
        assert!(matches!(
            missing_profile.code,
            WorkflowErrorCode::PersistenceFailure | WorkflowErrorCode::DigestConflict
        ));

        let mut modified = std::fs::read(canonical_profile_path()).expect("canonical profile");
        modified.extend_from_slice(b"\n# modified\n");
        let modified_path = directory.path().join("modified-profile.toml");
        std::fs::write(&modified_path, modified).expect("modified profile");
        let digest_conflict = WorkflowApi::open_configured(
            directory.path().join("modified-profile.db"),
            true,
            Some(modified_path),
            Some(directory.path().join("principals.json")),
        )
        .expect_err("modified canonical profile must fail closed");
        assert_eq!(digest_conflict.code, WorkflowErrorCode::DigestConflict);

        let missing_principals = WorkflowApi::open_configured(
            directory.path().join("missing-principals.db"),
            true,
            Some(canonical_profile_path()),
            None,
        )
        .expect_err("enabled workflow requires principal binding configuration");
        assert_eq!(
            missing_principals.code,
            WorkflowErrorCode::PersistenceFailure
        );
    }

    #[test]
    fn fully_provisioned_enabled_start_loads_external_ssot() {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV_LOCK.lock().expect("environment lock");
        let directory = tempfile::tempdir().expect("tempdir");
        let env_name = "SENTINEL_TEST_WORKFLOW_STARTUP_TOKEN";
        std::env::set_var(env_name, CUSTOMER_A_TOKEN);
        let principals = principal_file(directory.path(), env_name);
        let result = WorkflowApi::open_configured(
            directory.path().join("workflow.db"),
            true,
            Some(canonical_profile_path()),
            Some(principals),
        );
        std::env::remove_var(env_name);
        let api = result.expect("fully provisioned workflow starts");
        assert!(api.enabled);
        assert!(api.principals.configured());
    }

    fn headers(token: &str) -> HashMap<String, String> {
        HashMap::from([("authorization".into(), format!("Bearer {token}"))])
    }

    #[test]
    fn customer_api_is_authenticated_bounded_and_tenant_isolated() {
        let api = api(true);
        let envelope = json!({
            "operation_id": "customer-submit-1",
            "command": {
                "command": "submit_customer_request",
                "summary_ref": "sha256:public-safe-summary",
                "desired_outcome": "A bounded website",
                "constraints": ["No unrestricted uploads"]
            }
        });
        let body = serde_json::to_vec(&envelope).expect("JSON");
        let unauthorized = api
            .handle("POST", CUSTOMER_COMMAND_PATH, &HashMap::new(), &body)
            .expect("workflow response");
        assert_eq!(unauthorized.status, 401);
        let created = api
            .handle(
                "POST",
                CUSTOMER_COMMAND_PATH,
                &headers(CUSTOMER_A_TOKEN),
                &body,
            )
            .expect("workflow response");
        assert_eq!(created.status, 200);
        assert!(!String::from_utf8_lossy(&created.body).contains(CUSTOMER_A_TOKEN));
        let value: serde_json::Value =
            serde_json::from_slice(&created.body).expect("response JSON");
        let request_id = value["response"]["value"]["id"]
            .as_str()
            .expect("request ID");
        let denied = api
            .handle(
                "GET",
                &format!("{CUSTOMER_REQUEST_PATH}?request_id={request_id}"),
                &headers(CUSTOMER_B_TOKEN),
                &[],
            )
            .expect("workflow response");
        assert_eq!(denied.status, 401);
    }

    #[test]
    fn operator_api_rejects_customer_actors_and_chat_has_no_workflow_route() {
        let api = api(true);
        let body = serde_json::to_vec(&json!({
            "operation_id": "operator-spoof",
            "actor": {
                "actor_id": "customer-user-a",
                "role": "customer",
                "customer_id": "customer-a"
            },
            "command": {
                "command": "submit_customer_request",
                "summary_ref": "ref",
                "desired_outcome": "result",
                "constraints": []
            }
        }))
        .expect("JSON");
        assert_eq!(
            api.handle(
                "POST",
                OPERATOR_COMMAND_PATH,
                &headers(OPERATOR_TOKEN),
                &body
            )
            .expect("workflow response")
            .status,
            400
        );
        assert!(api
            .handle("POST", "/operator/chat", &HashMap::new(), &[])
            .is_none());
    }

    #[test]
    fn principal_kinds_cannot_cross_operator_and_agent_routes() {
        let api = api(true);
        let body = serde_json::to_vec(&json!({
            "operation_id": "route-kind-check",
            "command": {
                "command": "claim_work",
                "work_item_id": "work-missing",
                "expected_version": 1,
                "agent_id": 2,
                "input_digest": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "deadline_ms": 4000000000000_u64
            }
        }))
        .expect("JSON");
        assert_eq!(
            api.handle("POST", AGENT_COMMAND_PATH, &headers(OPERATOR_TOKEN), &body)
                .expect("workflow response")
                .status,
            401
        );
        assert_eq!(
            api.handle("POST", OPERATOR_COMMAND_PATH, &headers(AGENT_TOKEN), &body)
                .expect("workflow response")
                .status,
            401
        );
        assert_eq!(
            api.handle("POST", AGENT_COMMAND_PATH, &headers(AGENT_TOKEN), &body)
                .expect("workflow response")
                .status,
            404
        );
    }

    #[test]
    fn agent_api_rejects_forged_outputs_and_self_attested_gates() {
        let api = api(true);
        let body = serde_json::to_vec(&json!({
            "operation_id": "forged-completion",
            "command": {
                "command": "request_work_completion",
                "work_item_id": "work-forged",
                "expected_version": 4,
                "assignment_version": 1,
                "output_refs": {"source_tree": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
                "gate_passed": true,
                "gate_id": "browser_smoke"
            }
        }))
        .expect("JSON");
        let response = api
            .handle("POST", AGENT_COMMAND_PATH, &headers(AGENT_TOKEN), &body)
            .expect("workflow response");
        assert_eq!(response.status, 400);
        let value: serde_json::Value = serde_json::from_slice(&response.body).expect("JSON");
        assert_eq!(value["code"], "invalid_input");
    }

    #[test]
    fn agent_api_rejects_a_caller_computed_completion_seal() {
        let api = api(true);
        let body = serde_json::to_vec(&json!({
            "operation_id": "forged-completion-seal",
            "command": {
                "command": "request_work_completion",
                "work_item_id": "work-forged",
                "expected_version": 4,
                "assignment_version": 1,
                "receipt_seal": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            }
        }))
        .expect("JSON");
        let response = api
            .handle("POST", AGENT_COMMAND_PATH, &headers(AGENT_TOKEN), &body)
            .expect("workflow response");
        assert_eq!(response.status, 400);
        let value: serde_json::Value = serde_json::from_slice(&response.body).expect("JSON");
        assert_eq!(value["code"], "invalid_input");
    }

    #[test]
    fn mutations_fail_closed_until_dispatcher_is_ready() {
        let api = api(false);
        let body = serde_json::to_vec(&json!({
            "operation_id": "gated-submit",
            "command": {
                "command": "submit_customer_request",
                "summary_ref": "ref",
                "desired_outcome": "result",
                "constraints": []
            }
        }))
        .expect("JSON");
        let response = api
            .handle(
                "POST",
                CUSTOMER_COMMAND_PATH,
                &headers(CUSTOMER_A_TOKEN),
                &body,
            )
            .expect("workflow response");
        assert_eq!(response.status, 503);
        let value: serde_json::Value = serde_json::from_slice(&response.body).expect("JSON");
        assert_eq!(value["code"], "dispatcher_not_ready");
    }
}
