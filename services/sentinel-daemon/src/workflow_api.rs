//! Authenticated, bounded HTTP adapter for the durable company workflow.
//!
//! This module deliberately exposes commands and read models only. Execution
//! remains behind `WorkExecutionPort`; #694 supplies the production adapter.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use sentinel_common::{CompanyWorkflowSnapshot, DomainEvent, DomainEventPayload};
use sentinel_limbo::EventStore;
use sentinel_workflow::{
    AuthenticatedPrincipal, CanonicalWorkProfile, CustomerRequestId, PrincipalKind, ProjectId,
    UnavailableCompletionEvidencePort, UnavailableExecutionPort,
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
    store: Arc<WorkflowStore>,
    event_sink: Arc<dyn WorkflowEventSink>,
    principals: Arc<PrincipalAuthenticator>,
    mutation_fence: RwLock<()>,
    enabled: bool,
}

trait WorkflowEventSink: Send + Sync {
    fn publish(&self, event: &sentinel_workflow::WorkflowEvent) -> Result<(), String>;
    fn cursor(&self) -> Result<i64, String>;
}

struct LimboWorkflowEventSink {
    event_store: Arc<EventStore>,
}

impl WorkflowEventSink for LimboWorkflowEventSink {
    fn publish(&self, workflow_event: &sentinel_workflow::WorkflowEvent) -> Result<(), String> {
        let payload = DomainEventPayload::CompanyWorkflowEvent {
            workflow_event: serde_json::to_value(workflow_event)
                .map_err(|_| "workflow event serialization failed".to_owned())?,
        };
        let correlation_id = format!(
            "workflow:{}:{}",
            workflow_event.tenant_id, workflow_event.operation_id
        );
        let operation_id = format!("workflow-event:{}", workflow_event.event_id);
        let mut event = DomainEvent::new(
            payload.event_type_str(),
            &format!(
                "workflow:{}:{}",
                workflow_event.aggregate_type, workflow_event.aggregate_id
            ),
            &payload.to_json(),
            &correlation_id,
            0,
        )
        .with_causation(&workflow_event.operation_id)
        .with_operation_id(&operation_id)
        .with_schema_version(workflow_event.schema_version);
        event.event_id = format!("workflow-{}", workflow_event.event_id);
        event.timestamp_ms = workflow_event.timestamp_ms;
        self.event_store
            .append_with_outbox(&event, "sentinel/events/company_workflow_event")
            .map(|_| ())
            .map_err(|_| "canonical event store publication failed".to_owned())
    }

    fn cursor(&self) -> Result<i64, String> {
        self.event_store
            .get_latest_event_id()
            .map_err(|_| "canonical event store cursor is unavailable".to_owned())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowHealthSnapshot {
    pub enabled: bool,
    pub status: String,
    pub publication_pending: u64,
    pub publication_high_watermark: i64,
    pub canonical_event_cursor: Option<i64>,
}

impl std::fmt::Debug for WorkflowApi {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkflowApi")
            .finish_non_exhaustive()
    }
}

impl WorkflowApi {
    #[cfg(test)]
    pub(crate) fn disabled(event_store: Arc<EventStore>) -> Result<Self, WorkflowError> {
        Self::open_configured(
            ":memory:",
            false,
            None,
            None,
            Arc::new(LimboWorkflowEventSink { event_store }),
        )
    }

    pub fn open(
        path: impl AsRef<Path>,
        principal_bindings_file: Option<PathBuf>,
        event_store: Arc<EventStore>,
    ) -> Result<Self, WorkflowError> {
        Self::open_configured(
            path,
            workflow_enabled()?,
            Some(work_profile_path()),
            principal_bindings_file,
            Arc::new(LimboWorkflowEventSink { event_store }),
        )
    }

    fn open_configured(
        path: impl AsRef<Path>,
        enabled: bool,
        profile_file: Option<PathBuf>,
        principal_bindings_file: Option<PathBuf>,
        event_sink: Arc<dyn WorkflowEventSink>,
    ) -> Result<Self, WorkflowError> {
        if !enabled {
            let store = Arc::new(WorkflowStore::open(":memory:")?);
            return Ok(Self {
                engine: Arc::new(WorkflowEngine::with_ports(
                    Arc::clone(&store),
                    Arc::new(UnavailableExecutionPort),
                    Arc::new(UnavailableOrganizationRuntimePort),
                    Arc::new(UnavailableCompletionEvidencePort),
                    Arc::new(CanonicalWorkProfile::embedded()?),
                )),
                store,
                event_sink,
                principals: Arc::new(PrincipalAuthenticator::default()),
                mutation_fence: RwLock::new(()),
                enabled: false,
            });
        }
        let profile_file = profile_file.ok_or_else(auth_configuration_error)?;
        let principal_bindings_file =
            principal_bindings_file.ok_or_else(auth_configuration_error)?;
        let profile = Arc::new(CanonicalWorkProfile::load_verified(profile_file)?);
        let store = Arc::new(WorkflowStore::open(path)?);
        let api = Self {
            engine: Arc::new(WorkflowEngine::with_ports(
                Arc::clone(&store),
                Arc::new(UnavailableExecutionPort),
                Arc::new(UnavailableOrganizationRuntimePort),
                Arc::new(UnavailableCompletionEvidencePort),
                profile,
            )),
            store,
            event_sink,
            principals: Arc::new(PrincipalAuthenticator::from_file(Some(
                principal_bindings_file,
            ))?),
            mutation_fence: RwLock::new(()),
            enabled: true,
        };
        let _ = api.recover_event_publications(1_000);
        Ok(api)
    }

    #[cfg(test)]
    fn with_dependencies(
        path: impl AsRef<Path>,
        principals: PrincipalAuthenticator,
        execution_port: Arc<dyn sentinel_workflow::WorkExecutionPort>,
        organization_port: Arc<dyn sentinel_workflow::OrganizationRuntimePort>,
        completion_port: Arc<dyn sentinel_workflow::CompletionEvidencePort>,
        event_sink: Arc<dyn WorkflowEventSink>,
    ) -> Result<Self, WorkflowError> {
        let store = Arc::new(WorkflowStore::open(path)?);
        Ok(Self {
            engine: Arc::new(WorkflowEngine::with_ports(
                Arc::clone(&store),
                execution_port,
                organization_port,
                completion_port,
                Arc::new(CanonicalWorkProfile::embedded()?),
            )),
            store,
            event_sink,
            principals: Arc::new(principals),
            mutation_fence: RwLock::new(()),
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
        if self.enabled {
            // Publication participates in the same read/write fence as
            // mutations. A Time Machine snapshot takes the write side, so no
            // canonical append can race the captured workflow image/cursor.
            if let Ok(_publication_guard) = self.mutation_fence.read() {
                let _ = self.recover_event_publications(1_000);
            }
        }
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
        let Ok(_mutation_guard) = self.mutation_fence.read() else {
            return json_error(
                503,
                "workflow_recovery_in_progress",
                "workflow mutation fence is unavailable",
                true,
            );
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
        match self
            .engine
            .execute(principal, &envelope.operation_id, envelope.command)
        {
            Ok(outcome) => {
                let _ = self.recover_event_publications(1_000);
                json(200, &outcome)
            }
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
        let Ok(_mutation_guard) = self.mutation_fence.read() else {
            return json_error(
                503,
                "workflow_recovery_in_progress",
                "workflow mutation fence is unavailable",
                true,
            );
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
        match self
            .engine
            .execute(principal, &envelope.operation_id, envelope.command)
        {
            Ok(outcome) => {
                let _ = self.recover_event_publications(1_000);
                json(200, &outcome)
            }
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

    fn recover_event_publications(&self, limit: usize) -> Result<usize, WorkflowError> {
        let pending = self.store.pending_event_publications(limit)?;
        let mut published = 0;
        for event in pending {
            match self.event_sink.publish(&event) {
                Ok(()) => {
                    self.store.mark_event_published(
                        &event.event_id,
                        event.sequence,
                        current_time_ms(),
                    )?;
                    published += 1;
                }
                Err(error) => {
                    self.store.mark_event_publish_failed(
                        &event.event_id,
                        event.sequence,
                        &error,
                    )?;
                    return Err(WorkflowError::new(
                        WorkflowErrorCode::PersistenceFailure,
                        true,
                        "canonical workflow event publication is unavailable",
                    ));
                }
            }
        }
        Ok(published)
    }

    pub fn health(&self) -> WorkflowHealthSnapshot {
        if !self.enabled {
            return WorkflowHealthSnapshot {
                enabled: false,
                status: "disabled".to_owned(),
                publication_pending: 0,
                publication_high_watermark: 0,
                canonical_event_cursor: self.event_sink.cursor().ok(),
            };
        }
        let state = self.store.event_publication_state();
        let cursor = self.event_sink.cursor().ok();
        match state {
            Ok((pending, high_watermark)) => WorkflowHealthSnapshot {
                enabled: true,
                status: if pending == 0 && cursor.is_some() {
                    "ready".to_owned()
                } else {
                    "degraded".to_owned()
                },
                publication_pending: pending,
                publication_high_watermark: high_watermark,
                canonical_event_cursor: cursor,
            },
            Err(_) => WorkflowHealthSnapshot {
                enabled: true,
                status: "unavailable".to_owned(),
                publication_pending: 0,
                publication_high_watermark: 0,
                canonical_event_cursor: cursor,
            },
        }
    }

    pub fn time_machine_snapshot(&self) -> Result<Option<CompanyWorkflowSnapshot>, WorkflowError> {
        if !self.enabled {
            return Ok(None);
        }
        let _fence = self
            .mutation_fence
            .write()
            .map_err(|_| workflow_persistence_error("workflow snapshot fence is unavailable"))?;
        for _ in 0..10 {
            self.recover_event_publications(1_000)?;
            if self.store.event_publication_state()?.0 == 0 {
                let image = self.store.backup_image()?;
                let limbo_event_cursor = self.event_sink.cursor().map_err(|_| {
                    WorkflowError::new(
                        WorkflowErrorCode::PersistenceFailure,
                        true,
                        "canonical event store cursor is unavailable",
                    )
                })?;
                return Ok(Some(CompanyWorkflowSnapshot {
                    database: image.database,
                    manifest_json: serde_json::to_vec(&image.manifest)?,
                    limbo_event_cursor,
                }));
            }
        }
        Err(WorkflowError::new(
            WorkflowErrorCode::PersistenceFailure,
            true,
            "workflow event publication backlog exceeds the bounded snapshot drain",
        ))
    }

    pub fn restore_time_machine_snapshot(
        &self,
        snapshot: Option<&CompanyWorkflowSnapshot>,
    ) -> Result<(), WorkflowError> {
        match (self.enabled, snapshot) {
            (false, None) => return Ok(()),
            (false, Some(_)) => {
                return Err(WorkflowError::new(
                    WorkflowErrorCode::BackupVerificationFailed,
                    false,
                    "workflow snapshot cannot be restored while the workflow is disabled",
                ))
            }
            (true, None) => {
                return Err(WorkflowError::new(
                    WorkflowErrorCode::BackupVerificationFailed,
                    false,
                    "enabled workflow requires a workflow image in the world snapshot",
                ))
            }
            (true, Some(_)) => {}
        }
        let snapshot = snapshot.expect("enabled snapshot checked above");
        let manifest: sentinel_workflow::WorkflowBackupManifest =
            serde_json::from_slice(&snapshot.manifest_json).map_err(|_| {
                WorkflowError::new(
                    WorkflowErrorCode::BackupVerificationFailed,
                    false,
                    "workflow snapshot manifest is invalid",
                )
            })?;
        let _fence = self
            .mutation_fence
            .write()
            .map_err(|_| workflow_persistence_error("workflow restore fence is unavailable"))?;
        self.store
            .restore_image(&sentinel_workflow::WorkflowBackupImage {
                manifest,
                database: snapshot.database.clone(),
            })
    }
}

fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn workflow_persistence_error(message: &str) -> WorkflowError {
    WorkflowError::new(WorkflowErrorCode::PersistenceFailure, true, message)
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
        WorkflowErrorCode::OrganizationUnavailable => (503, "organization_unavailable"),
        WorkflowErrorCode::ExecutionUnavailable => (503, "execution_unavailable"),
        WorkflowErrorCode::CompletionUnavailable => (503, "completion_unavailable"),
        WorkflowErrorCode::PersistenceFailure => (503, "service_unavailable"),
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
        DependencyReadiness, ExecutionReceipt, OrganizationAgentSnapshot, OrganizationRuntimePort,
        WorkExecutionError, WorkExecutionPort,
    };
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;

    const CUSTOMER_A_TOKEN: &str = "customer-a-token-that-is-long-enough";
    const CUSTOMER_B_TOKEN: &str = "customer-b-token-that-is-long-enough";
    const OPERATOR_TOKEN: &str = "operator-token-that-is-long-enough-1";
    const AGENT_TOKEN: &str = "agent-token-that-is-long-enough-0001";

    #[derive(Debug)]
    struct RecordingEventSink {
        available: AtomicBool,
        attempts: AtomicUsize,
        events: Mutex<BTreeMap<String, sentinel_workflow::WorkflowEvent>>,
    }

    impl RecordingEventSink {
        fn available() -> Arc<Self> {
            Arc::new(Self {
                available: AtomicBool::new(true),
                attempts: AtomicUsize::new(0),
                events: Mutex::new(BTreeMap::new()),
            })
        }
    }

    impl WorkflowEventSink for RecordingEventSink {
        fn publish(&self, event: &sentinel_workflow::WorkflowEvent) -> Result<(), String> {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            if !self.available.load(Ordering::SeqCst) {
                return Err("test event sink unavailable".to_owned());
            }
            self.events
                .lock()
                .expect("event sink lock")
                .entry(event.event_id.clone())
                .or_insert_with(|| event.clone());
            Ok(())
        }

        fn cursor(&self) -> Result<i64, String> {
            Ok(self.events.lock().expect("event sink lock").len() as i64)
        }
    }

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

    fn api_with_sink(
        execution_ready: bool,
        completion_ready: bool,
        event_sink: Arc<dyn WorkflowEventSink>,
    ) -> WorkflowApi {
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
        let completion: Arc<dyn CompletionEvidencePort> = if completion_ready {
            Arc::new(ReadyCompletion)
        } else {
            Arc::new(UnavailableCompletionEvidencePort)
        };
        WorkflowApi::with_dependencies(
            ":memory:",
            principals,
            execution,
            Arc::new(ReadyOrganization),
            completion,
            event_sink,
        )
        .expect("API")
    }

    fn api(execution_ready: bool) -> WorkflowApi {
        api_with_sink(execution_ready, true, RecordingEventSink::available())
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
            RecordingEventSink::available(),
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
            RecordingEventSink::available(),
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
            RecordingEventSink::available(),
        )
        .expect_err("modified canonical profile must fail closed");
        assert_eq!(digest_conflict.code, WorkflowErrorCode::DigestConflict);

        let missing_principals = WorkflowApi::open_configured(
            directory.path().join("missing-principals.db"),
            true,
            Some(canonical_profile_path()),
            None,
            RecordingEventSink::available(),
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
            RecordingEventSink::available(),
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
    fn workbench_degradation_preserves_local_commands_and_gates_claim_and_completion() {
        let api = api(false);
        let submit = serde_json::to_vec(&json!({
            "operation_id": "degraded-submit",
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
                &submit,
            )
            .expect("workflow response");
        assert_eq!(response.status, 200);
        let value: serde_json::Value = serde_json::from_slice(&response.body).expect("JSON");
        let request_id = value["response"]["value"]["id"]
            .as_str()
            .expect("request id");
        let clarify = serde_json::to_vec(&json!({
            "operation_id": "degraded-clarification",
            "command": {
                "command": "clarify_customer_request",
                "request_id": request_id,
                "expected_version": 1,
                "question_ref": "scope-question",
                "answer_ref": "scope-answer"
            }
        }))
        .expect("JSON");
        assert_eq!(
            api.handle(
                "POST",
                CUSTOMER_COMMAND_PATH,
                &headers(CUSTOMER_A_TOKEN),
                &clarify,
            )
            .expect("workflow response")
            .status,
            200
        );

        let claim = serde_json::to_vec(&json!({
            "operation_id": "degraded-claim",
            "command": {
                "command": "claim_work",
                "work_item_id": "work-not-resolved-before-readiness",
                "expected_version": 1,
                "agent_id": 2,
                "input_digest": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "deadline_ms": 4000000000000_u64
            }
        }))
        .expect("JSON");
        let response = api
            .handle("POST", AGENT_COMMAND_PATH, &headers(AGENT_TOKEN), &claim)
            .expect("workflow response");
        assert_eq!(response.status, 503);
        let value: serde_json::Value = serde_json::from_slice(&response.body).expect("JSON");
        assert_eq!(value["code"], "execution_unavailable");

        let completion_api = api_with_sink(true, false, RecordingEventSink::available());
        let completion = serde_json::to_vec(&json!({
            "operation_id": "degraded-completion",
            "command": {
                "command": "request_work_completion",
                "work_item_id": "work-not-resolved-before-readiness",
                "expected_version": 1,
                "assignment_version": 1
            }
        }))
        .expect("JSON");
        let response = completion_api
            .handle(
                "POST",
                AGENT_COMMAND_PATH,
                &headers(AGENT_TOKEN),
                &completion,
            )
            .expect("workflow response");
        assert_eq!(response.status, 503);
        let value: serde_json::Value = serde_json::from_slice(&response.body).expect("JSON");
        assert_eq!(value["code"], "completion_unavailable");
    }

    #[test]
    fn workflow_event_publication_recovers_without_duplicates() {
        let sink = RecordingEventSink::available();
        sink.available.store(false, Ordering::SeqCst);
        let api = api_with_sink(false, false, sink.clone());
        let submit = serde_json::to_vec(&json!({
            "operation_id": "publication-recovery",
            "command": {
                "command": "submit_customer_request",
                "summary_ref": "ref",
                "desired_outcome": "result",
                "constraints": []
            }
        }))
        .expect("JSON");
        let created = api
            .handle(
                "POST",
                CUSTOMER_COMMAND_PATH,
                &headers(CUSTOMER_A_TOKEN),
                &submit,
            )
            .expect("workflow response");
        assert_eq!(
            created.status, 200,
            "local command must survive sink outage"
        );
        assert_eq!(api.store.event_publication_state().unwrap().0, 1);
        assert!(sink.events.lock().unwrap().is_empty());

        sink.available.store(true, Ordering::SeqCst);
        let value: serde_json::Value = serde_json::from_slice(&created.body).expect("JSON");
        let request_id = value["response"]["value"]["id"]
            .as_str()
            .expect("request id");
        let read_path = format!("{CUSTOMER_REQUEST_PATH}?request_id={request_id}");
        assert_eq!(
            api.handle("GET", &read_path, &headers(CUSTOMER_A_TOKEN), &[],)
                .expect("workflow response")
                .status,
            200
        );
        assert_eq!(api.store.event_publication_state().unwrap().0, 0);
        assert_eq!(sink.events.lock().unwrap().len(), 1);

        let replayed = api
            .handle(
                "POST",
                CUSTOMER_COMMAND_PATH,
                &headers(CUSTOMER_A_TOKEN),
                &submit,
            )
            .expect("workflow response");
        assert_eq!(replayed.status, 200);
        assert_eq!(sink.events.lock().unwrap().len(), 1);
    }

    #[test]
    fn workflow_events_enter_limbo_and_its_transport_outbox_idempotently() {
        let limbo = Arc::new(EventStore::open(":memory:").expect("limbo"));
        let api = api_with_sink(
            false,
            false,
            Arc::new(LimboWorkflowEventSink {
                event_store: Arc::clone(&limbo),
            }),
        );
        let submit = serde_json::to_vec(&json!({
            "operation_id": "limbo-publication",
            "command": {
                "command": "submit_customer_request",
                "summary_ref": "ref",
                "desired_outcome": "result",
                "constraints": []
            }
        }))
        .expect("JSON");
        for _ in 0..2 {
            assert_eq!(
                api.handle(
                    "POST",
                    CUSTOMER_COMMAND_PATH,
                    &headers(CUSTOMER_A_TOKEN),
                    &submit,
                )
                .expect("workflow response")
                .status,
                200
            );
        }
        let events = limbo.get_all_events().expect("events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "company_workflow_event");
        assert!(matches!(
            serde_json::from_str::<DomainEventPayload>(&events[0].payload).expect("payload"),
            DomainEventPayload::CompanyWorkflowEvent { .. }
        ));
        assert_eq!(limbo.poll_outbox(10).expect("transport outbox").len(), 1);
    }

    #[test]
    fn time_machine_snapshot_restores_workflow_state_and_publication_checkpoint() {
        let sink = RecordingEventSink::available();
        let api = api_with_sink(false, false, sink.clone());
        let submit = serde_json::to_vec(&json!({
            "operation_id": "snapshot-submit",
            "command": {
                "command": "submit_customer_request",
                "summary_ref": "ref",
                "desired_outcome": "result",
                "constraints": []
            }
        }))
        .expect("JSON");
        let created = api
            .handle(
                "POST",
                CUSTOMER_COMMAND_PATH,
                &headers(CUSTOMER_A_TOKEN),
                &submit,
            )
            .expect("workflow response");
        assert_eq!(created.status, 200);
        let created_json: serde_json::Value =
            serde_json::from_slice(&created.body).expect("created JSON");
        let request_id = created_json["response"]["value"]["id"]
            .as_str()
            .expect("request id");
        let snapshot = api
            .time_machine_snapshot()
            .expect("workflow snapshot")
            .expect("enabled image");
        assert!(snapshot.database.starts_with(b"SQLite format 3"));
        assert_eq!(snapshot.limbo_event_cursor, 1);

        let clarify = serde_json::to_vec(&json!({
            "operation_id": "snapshot-clarification",
            "command": {
                "command": "clarify_customer_request",
                "request_id": request_id,
                "expected_version": 1,
                "question_ref": "scope-question",
                "answer_ref": "scope-answer"
            }
        }))
        .expect("JSON");
        assert_eq!(
            api.handle(
                "POST",
                CUSTOMER_COMMAND_PATH,
                &headers(CUSTOMER_A_TOKEN),
                &clarify,
            )
            .expect("clarification")
            .status,
            200
        );
        api.restore_time_machine_snapshot(Some(&snapshot))
            .expect("workflow restore");

        let read = api
            .handle(
                "GET",
                &format!("{CUSTOMER_REQUEST_PATH}?request_id={request_id}"),
                &headers(CUSTOMER_A_TOKEN),
                &[],
            )
            .expect("workflow read");
        let restored: serde_json::Value =
            serde_json::from_slice(&read.body).expect("restored JSON");
        assert_eq!(restored["version"], 1);
        assert_eq!(api.store.event_publication_state().unwrap(), (0, 1));
        assert_eq!(sink.events.lock().unwrap().len(), 2);
    }
}
