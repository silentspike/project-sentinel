use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::de::DeserializeOwned;
use serde::Serialize;
use uuid::Uuid;

use crate::digest::{canonical_sha256, constant_time_eq};
use crate::domain::*;
use crate::model::{validate_digest, validate_identifier};
use crate::{ProjectId, TenantId, WorkflowError, WorkflowErrorCode, WorkflowStore};

const COMPANY_STORE_SCHEMA_VERSION: u32 = 1;
const MAX_AGGREGATE_ITEMS: usize = 128;

const COMPANY_SCHEMA: &str = r#"
CREATE TABLE company_schema_meta (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    schema_version INTEGER NOT NULL
);
INSERT INTO company_schema_meta(singleton,schema_version) VALUES(1,1);
CREATE TABLE company_entities (
    tenant_id TEXT NOT NULL,
    entity_kind TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    payload BLOB NOT NULL,
    payload_digest TEXT NOT NULL,
    PRIMARY KEY (tenant_id, entity_kind, entity_id)
);
CREATE TABLE company_operations (
    authority_namespace TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    authority_binding_digest TEXT NOT NULL,
    target_predecessor_digest TEXT NOT NULL,
    response BLOB NOT NULL,
    response_digest TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (authority_namespace, operation_id)
);
CREATE TABLE company_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE,
    tenant_id TEXT NOT NULL,
    project_id TEXT,
    event_type TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    operation_digest TEXT NOT NULL,
    principal_id TEXT NOT NULL,
    principal_kind TEXT NOT NULL,
    principal_role TEXT NOT NULL,
    agent_id INTEGER,
    customer_id TEXT,
    authority_generation INTEGER NOT NULL,
    authority_digest TEXT NOT NULL,
    authority_binding_digest TEXT NOT NULL,
    payload BLOB NOT NULL,
    payload_digest TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);
CREATE INDEX idx_company_events_tenant_project
    ON company_events(tenant_id, project_id, sequence);
CREATE TABLE company_project_projections (
    tenant_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    source_sequence INTEGER NOT NULL,
    payload BLOB NOT NULL,
    payload_digest TEXT NOT NULL,
    PRIMARY KEY (tenant_id, project_id)
);
"#;

pub(crate) fn ensure_company_schema(connection: &Connection) -> Result<(), WorkflowError> {
    ensure_company_schema_inner(connection, false)
}

fn ensure_company_schema_inner(
    connection: &Connection,
    fail_after_create: bool,
) -> Result<(), WorkflowError> {
    let has_meta: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='company_schema_meta')",
            [],
            |row| row.get(0),
        )
        .map_err(WorkflowError::from)?;
    if !has_meta {
        let orphan_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type IN ('table','index','view','trigger') AND (name LIKE 'company_%' OR tbl_name LIKE 'company_%')",
                [],
                |row| row.get(0),
            )
            .map_err(WorkflowError::from)?;
        if orphan_count != 0 {
            return Err(corrupt());
        }
        connection
            .execute_batch(COMPANY_SCHEMA)
            .map_err(WorkflowError::from)?;
        if fail_after_create {
            return Err(corrupt());
        }
    }
    validate_company_schema(connection)
}

fn validate_company_schema(connection: &Connection) -> Result<(), WorkflowError> {
    let meta = connection
        .query_row(
            "SELECT singleton,schema_version FROM company_schema_meta",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(WorkflowError::from)?;
    if meta != Some((1, i64::from(COMPANY_STORE_SCHEMA_VERSION))) {
        return Err(corrupt());
    }
    let objects = connection
        .prepare("SELECT type,name FROM sqlite_master WHERE (name LIKE 'company_%' OR tbl_name LIKE 'company_%') AND name NOT LIKE 'sqlite_%' ORDER BY type,name")
        .map_err(WorkflowError::from)?
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(WorkflowError::from)?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(WorkflowError::from)?;
    let expected_objects = [
        ("index", "idx_company_events_tenant_project"),
        ("table", "company_entities"),
        ("table", "company_events"),
        ("table", "company_operations"),
        ("table", "company_project_projections"),
        ("table", "company_schema_meta"),
    ]
    .into_iter()
    .map(|(kind, name)| (kind.to_owned(), name.to_owned()))
    .collect::<BTreeSet<_>>();
    if objects != expected_objects {
        return Err(corrupt());
    }
    validate_columns(
        connection,
        "company_schema_meta",
        &[
            ("singleton", "INTEGER", 0, 1),
            ("schema_version", "INTEGER", 1, 0),
        ],
    )?;
    validate_columns(
        connection,
        "company_entities",
        &[
            ("tenant_id", "TEXT", 1, 1),
            ("entity_kind", "TEXT", 1, 2),
            ("entity_id", "TEXT", 1, 3),
            ("version", "INTEGER", 1, 0),
            ("payload", "BLOB", 1, 0),
            ("payload_digest", "TEXT", 1, 0),
        ],
    )?;
    validate_columns(
        connection,
        "company_operations",
        &[
            ("authority_namespace", "TEXT", 1, 1),
            ("operation_id", "TEXT", 1, 2),
            ("request_digest", "TEXT", 1, 0),
            ("authority_binding_digest", "TEXT", 1, 0),
            ("target_predecessor_digest", "TEXT", 1, 0),
            ("response", "BLOB", 1, 0),
            ("response_digest", "TEXT", 1, 0),
            ("created_at_ms", "INTEGER", 1, 0),
        ],
    )?;
    validate_columns(
        connection,
        "company_project_projections",
        &[
            ("tenant_id", "TEXT", 1, 1),
            ("project_id", "TEXT", 1, 2),
            ("source_sequence", "INTEGER", 1, 0),
            ("payload", "BLOB", 1, 0),
            ("payload_digest", "TEXT", 1, 0),
        ],
    )?;
    validate_columns(
        connection,
        "company_events",
        &[
            ("sequence", "INTEGER", 0, 1),
            ("event_id", "TEXT", 1, 0),
            ("tenant_id", "TEXT", 1, 0),
            ("project_id", "TEXT", 0, 0),
            ("event_type", "TEXT", 1, 0),
            ("operation_id", "TEXT", 1, 0),
            ("operation_digest", "TEXT", 1, 0),
            ("principal_id", "TEXT", 1, 0),
            ("principal_kind", "TEXT", 1, 0),
            ("principal_role", "TEXT", 1, 0),
            ("agent_id", "INTEGER", 0, 0),
            ("customer_id", "TEXT", 0, 0),
            ("authority_generation", "INTEGER", 1, 0),
            ("authority_digest", "TEXT", 1, 0),
            ("authority_binding_digest", "TEXT", 1, 0),
            ("payload", "BLOB", 1, 0),
            ("payload_digest", "TEXT", 1, 0),
            ("created_at_ms", "INTEGER", 1, 0),
        ],
    )?;
    let meta_sql: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='company_schema_meta'",
            [],
            |row| row.get(0),
        )
        .map_err(WorkflowError::from)?;
    let events_sql: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='company_events'",
            [],
            |row| row.get(0),
        )
        .map_err(WorkflowError::from)?;
    if !meta_sql.contains("CHECK (singleton = 1)")
        || !events_sql.contains("event_id TEXT NOT NULL UNIQUE")
    {
        return Err(corrupt());
    }
    let event_index = connection
        .prepare("PRAGMA index_info('idx_company_events_tenant_project')")
        .map_err(WorkflowError::from)?
        .query_map([], |row| row.get::<_, String>(2))
        .map_err(WorkflowError::from)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(WorkflowError::from)?;
    if event_index != ["tenant_id", "project_id", "sequence"] {
        return Err(corrupt());
    }
    Ok(())
}

fn validate_columns(
    connection: &Connection,
    table: &str,
    expected: &[(&str, &str, i64, i64)],
) -> Result<(), WorkflowError> {
    let actual = table_columns(connection, table)?;
    if actual.len() != expected.len()
        || actual.iter().zip(expected).any(
            |((name, kind, not_null, default_value, primary_key), expected)| {
                name != expected.0
                    || kind != expected.1
                    || *not_null != expected.2
                    || default_value.is_some()
                    || *primary_key != expected.3
            },
        )
    {
        return Err(corrupt());
    }
    Ok(())
}

type TableColumnRow = (String, String, i64, Option<String>, i64);

fn table_columns(
    connection: &Connection,
    table: &str,
) -> Result<Vec<TableColumnRow>, WorkflowError> {
    connection
        .prepare(&format!("PRAGMA table_info('{table}')"))
        .map_err(WorkflowError::from)?
        .query_map([], |row| {
            Ok((
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .map_err(WorkflowError::from)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(WorkflowError::from)
}

#[derive(Debug)]
struct CompanyEventRow {
    sequence: i64,
    event_id: String,
    tenant_id: String,
    project_id: Option<String>,
    event_type: String,
    operation_id: String,
    operation_digest: String,
    principal_id: String,
    principal_kind: String,
    principal_role: String,
    agent_id: Option<i64>,
    customer_id: Option<String>,
    authority_generation: i64,
    authority_digest: String,
    authority_binding_digest: String,
    payload: Vec<u8>,
    payload_digest: String,
    created_at_ms: i64,
}

fn validate_project_snapshot_event(
    connection: &Connection,
    row: &CompanyEventRow,
) -> Result<(u64, ProjectV1), WorkflowError> {
    let sequence = stored_u64(row.sequence)?;
    let created_at_ms = stored_u64(row.created_at_ms)?;
    let operation_id = Uuid::parse_str(&row.operation_id).map_err(|_| corrupt())?;
    let tenant_id = TenantId::parse(&row.tenant_id).map_err(|_| corrupt())?;
    let project_id =
        ProjectId::parse(row.project_id.as_deref().ok_or_else(corrupt)?).map_err(|_| corrupt())?;
    let agent_id = row
        .agent_id
        .map(stored_u64)
        .transpose()?
        .map(|value| {
            u16::try_from(value)
                .map(crate::AgentId)
                .map_err(|_| corrupt())
        })
        .transpose()?;
    let principal = AuthenticatedCompanyPrincipalV1 {
        schema_version: COMPANY_DOMAIN_SCHEMA_VERSION,
        tenant_id: tenant_id.clone(),
        principal_id: row.principal_id.clone(),
        kind: parse_principal_kind(&row.principal_kind)?,
        role: parse_company_role(&row.principal_role)?,
        customer_id: row.customer_id.clone(),
        agent_id,
        authority_generation: stored_u64(row.authority_generation)?,
        authority_digest: row.authority_digest.clone(),
    };
    principal.validate().map_err(|_| corrupt())?;
    validate_digest(&row.operation_digest).map_err(|_| corrupt())?;
    validate_digest(&row.authority_digest).map_err(|_| corrupt())?;
    let payload_digest = bytes_digest("sentinel.workflow.company-event-payload.v1", &row.payload)?;
    if !is_project_event_type(&row.event_type)
        || !constant_time_eq(&payload_digest, &row.payload_digest)
        || !constant_time_eq(&principal.binding_digest()?, &row.authority_binding_digest)
    {
        return Err(corrupt());
    }
    let expected_event_id = canonical_sha256(
        "sentinel.workflow.company-event-id.v1",
        &(
            &tenant_id,
            Some(&project_id),
            row.event_type.as_str(),
            operation_id,
            row.operation_digest.as_str(),
            row.authority_binding_digest.as_str(),
            row.payload_digest.as_str(),
            created_at_ms,
        ),
    )?;
    let project: ProjectV1 = decode(&row.payload)?;
    if !constant_time_eq(&expected_event_id, &row.event_id)
        || project.tenant_id != tenant_id
        || project.project_id != project_id
        || project.updated_at_unix_ms != created_at_ms
    {
        return Err(corrupt());
    }
    validate_project(&project)?;
    let operation = connection
        .query_row(
            "SELECT request_digest,authority_binding_digest,target_predecessor_digest,response,response_digest,created_at_ms FROM company_operations WHERE authority_namespace=?1 AND operation_id=?2",
            params![principal.namespace(), row.operation_id],
            |operation| Ok((operation.get::<_, String>(0)?, operation.get::<_, String>(1)?, operation.get::<_, String>(2)?, operation.get::<_, Vec<u8>>(3)?, operation.get::<_, String>(4)?, operation.get::<_, i64>(5)?)),
        )
        .optional()
        .map_err(WorkflowError::from)?
        .ok_or_else(corrupt)?;
    validate_digest(&operation.2).map_err(|_| corrupt())?;
    if !constant_time_eq(&operation.0, &row.operation_digest)
        || !constant_time_eq(&operation.1, &row.authority_binding_digest)
        || !constant_time_eq(
            &bytes_digest(
                "sentinel.workflow.company-operation-response.v1",
                &operation.3,
            )?,
            &operation.4,
        )
        || stored_u64(operation.5)? != created_at_ms
    {
        return Err(corrupt());
    }
    let response: CompanyWorkflowResponseV1 = decode(&operation.3)?;
    let response_project = match response {
        CompanyWorkflowResponseV1::AgreementProject { project, .. } => *project,
        CompanyWorkflowResponseV1::Project(project) => project,
        _ => return Err(corrupt()),
    };
    if response_project != project {
        return Err(corrupt());
    }
    Ok((sequence, project))
}

fn is_project_event_type(value: &str) -> bool {
    matches!(
        value,
        "project_created"
            | "project_work_graph_planned"
            | "project_activated"
            | "project_work_assigned"
            | "project_work_reassigned"
            | "project_work_delegated"
            | "project_work_transition_applied"
            | "project_decision_recorded"
            | "project_handoff_created"
            | "project_handoff_acknowledged"
            | "project_blocker_raised"
            | "project_blocker_escalated"
            | "project_blocker_resolved"
            | "project_approval_recorded"
            | "project_cost_reserved"
            | "project_cost_committed"
            | "project_cost_released"
            | "project_room_created"
            | "project_question_recorded"
            | "project_question_resolved"
            | "project_action_recorded"
            | "project_action_resolved"
            | "project_governed_rework_created"
    )
}

fn parse_principal_kind(value: &str) -> Result<CompanyPrincipalKindV1, WorkflowError> {
    match value {
        "Customer" => Ok(CompanyPrincipalKindV1::Customer),
        "Operator" => Ok(CompanyPrincipalKindV1::Operator),
        "Agent" => Ok(CompanyPrincipalKindV1::Agent),
        _ => Err(corrupt()),
    }
}

fn parse_company_role(value: &str) -> Result<CompanyRoleV1, WorkflowError> {
    match value {
        "Customer" => Ok(CompanyRoleV1::Customer),
        "Sales" => Ok(CompanyRoleV1::Sales),
        "ProjectManager" => Ok(CompanyRoleV1::ProjectManager),
        "TechnicalLead" => Ok(CompanyRoleV1::TechnicalLead),
        "Designer" => Ok(CompanyRoleV1::Designer),
        "Developer" => Ok(CompanyRoleV1::Developer),
        "Qa" => Ok(CompanyRoleV1::Qa),
        "ReleaseManager" => Ok(CompanyRoleV1::ReleaseManager),
        "Gaia" => Ok(CompanyRoleV1::Gaia),
        _ => Err(corrupt()),
    }
}

#[cfg(test)]
fn expected_company_tables() -> BTreeSet<String> {
    [
        "company_schema_meta",
        "company_entities",
        "company_events",
        "company_operations",
        "company_project_projections",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

impl WorkflowStore {
    /// Returns the durable company-domain event frontier used by readiness and
    /// restart evidence. Zero is a valid frontier before the first command.
    pub fn company_event_cursor(&self) -> Result<u64, WorkflowError> {
        let connection = self.connection.lock().map_err(|_| persistence())?;
        let cursor: i64 = connection
            .query_row(
                "SELECT COALESCE(MAX(sequence),0) FROM company_events",
                [],
                |row| row.get(0),
            )
            .map_err(WorkflowError::from)?;
        stored_u64(cursor)
    }

    pub fn apply_company_command(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        operation_id: Uuid,
        command: &CompanyWorkflowCommandV1,
        now_ms: u64,
    ) -> Result<CompanyCommandOutcomeV1, WorkflowError> {
        self.apply_company_command_inner(principal, operation_id, command, now_ms, false)
    }

    #[cfg(test)]
    pub(crate) fn apply_company_command_with_accept_failpoint(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        operation_id: Uuid,
        command: &CompanyWorkflowCommandV1,
        now_ms: u64,
    ) -> Result<CompanyCommandOutcomeV1, WorkflowError> {
        self.apply_company_command_inner(principal, operation_id, command, now_ms, true)
    }

    fn apply_company_command_inner(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        operation_id: Uuid,
        command: &CompanyWorkflowCommandV1,
        now_ms: u64,
        fail_after_agreement: bool,
    ) -> Result<CompanyCommandOutcomeV1, WorkflowError> {
        principal.validate()?;
        if now_ms == 0 {
            return Err(invalid("company command time must be positive"));
        }
        let request_digest = command.canonical_digest()?;
        let authority_binding_digest = principal.binding_digest()?;
        let target_predecessor_digest = command_predecessor_digest(command)?;
        let namespace = principal.namespace();
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(WorkflowError::from)?;
        if let Some((stored_digest, stored_authority, stored_predecessor, response, response_digest)) = transaction
            .query_row(
                "SELECT request_digest,authority_binding_digest,target_predecessor_digest,response,response_digest FROM company_operations WHERE authority_namespace=?1 AND operation_id=?2",
                params![namespace, operation_id.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, Vec<u8>>(3)?, row.get::<_, String>(4)?)),
            )
            .optional()
            .map_err(WorkflowError::from)?
        {
            if !constant_time_eq(&stored_digest, &request_digest)
                || !constant_time_eq(&stored_authority, &authority_binding_digest)
                || !constant_time_eq(&stored_predecessor, &target_predecessor_digest)
            {
                return Err(WorkflowError::new(
                    WorkflowErrorCode::IdempotencyConflict,
                    false,
                    "company operation id is bound to another request",
                ));
            }
            if !constant_time_eq(
                &bytes_digest("sentinel.workflow.company-operation-response.v1", &response)?,
                &response_digest,
            ) {
                return Err(corrupt());
            }
            let response: CompanyWorkflowResponseV1 = decode(&response)?;
            validate_replay_response(&transaction, principal, command, &response)?;
            return Ok(CompanyCommandOutcomeV1 { replayed: true, response });
        }
        let response = apply_company_command(
            &transaction,
            principal,
            operation_id,
            &request_digest,
            command,
            now_ms,
            fail_after_agreement,
        )?;
        let encoded_response = encode(&response)?;
        let response_digest = bytes_digest(
            "sentinel.workflow.company-operation-response.v1",
            &encoded_response,
        )?;
        transaction
            .execute(
                "INSERT INTO company_operations(authority_namespace,operation_id,request_digest,authority_binding_digest,target_predecessor_digest,response,response_digest,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![namespace, operation_id.to_string(), request_digest, authority_binding_digest, target_predecessor_digest, encoded_response, response_digest, sql_u64(now_ms)?],
            )
            .map_err(WorkflowError::from)?;
        transaction.commit().map_err(WorkflowError::from)?;
        Ok(CompanyCommandOutcomeV1 {
            replayed: false,
            response,
        })
    }

    pub fn company_customer_request(
        &self,
        tenant_id: &TenantId,
        request_id: &str,
    ) -> Result<Option<CustomerRequestV1>, WorkflowError> {
        tenant_id.validate()?;
        validate_identifier(request_id)?;
        let connection = self.connection.lock().map_err(|_| persistence())?;
        get_entity(&connection, tenant_id, "request", request_id)
    }

    pub fn company_project(
        &self,
        tenant_id: &TenantId,
        project_id: &ProjectId,
    ) -> Result<Option<ProjectV1>, WorkflowError> {
        tenant_id.validate()?;
        project_id.validate()?;
        let connection = self.connection.lock().map_err(|_| persistence())?;
        let value = get_entity(&connection, tenant_id, "project", &project_id.0)?;
        if let Some(project) = &value {
            validate_project(project)?;
        }
        Ok(value)
    }

    pub fn company_agreement(
        &self,
        tenant_id: &TenantId,
        agreement_id: &str,
    ) -> Result<Option<AgreementV1>, WorkflowError> {
        tenant_id.validate()?;
        validate_identifier(agreement_id)?;
        let connection = self.connection.lock().map_err(|_| persistence())?;
        let value = get_entity(&connection, tenant_id, "agreement", agreement_id)?;
        if let Some(agreement) = &value {
            validate_agreement(agreement)?;
        }
        Ok(value)
    }

    pub fn company_project_projection(
        &self,
        tenant_id: &TenantId,
        project_id: &ProjectId,
    ) -> Result<Option<ProjectProjectionV1>, WorkflowError> {
        tenant_id.validate()?;
        project_id.validate()?;
        let connection = self.connection.lock().map_err(|_| persistence())?;
        connection
            .query_row(
                "SELECT tenant_id,project_id,source_sequence,payload,payload_digest FROM company_project_projections WHERE tenant_id=?1 AND project_id=?2",
                params![tenant_id.0, project_id.0],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?, row.get::<_, Vec<u8>>(3)?, row.get::<_, String>(4)?)),
            )
            .optional()
            .map_err(WorkflowError::from)?
            .map(|(row_tenant,row_project,row_sequence,payload,payload_digest)| {
                if row_tenant != tenant_id.0
                    || row_project != project_id.0
                    || !constant_time_eq(&bytes_digest("sentinel.workflow.company-projection-row.v1", &payload)?, &payload_digest)
                {
                    return Err(corrupt());
                }
                let projection = decode::<ProjectProjectionV1>(&payload)?;
                if projection.source_sequence != stored_u64(row_sequence)? {
                    return Err(corrupt());
                }
                Ok(projection)
            })
            .transpose()?
            .map(|projection| {
                validate_projection(&projection, tenant_id, project_id)?;
                let row = read_company_event_row(&connection, projection.source_sequence)?
                    .ok_or_else(corrupt)?;
                let (sequence, event_project) =
                    validate_project_snapshot_event(&connection, &row)?;
                if sequence != projection.source_sequence || event_project != projection.project {
                    return Err(corrupt());
                }
                Ok(projection)
            })
            .transpose()
    }

    pub fn company_project_events_since(
        &self,
        tenant_id: &TenantId,
        after: u64,
        limit: usize,
    ) -> Result<Vec<CompanyProjectEventViewV1>, WorkflowError> {
        tenant_id.validate()?;
        if !(1..=1_000).contains(&limit) {
            return Err(invalid("company event read limit is invalid"));
        }
        let connection = self.connection.lock().map_err(|_| persistence())?;
        let mut statement = connection
            .prepare(
                "SELECT sequence,event_id,tenant_id,project_id,event_type,operation_id,operation_digest,principal_id,principal_kind,principal_role,agent_id,customer_id,authority_generation,authority_digest,authority_binding_digest,payload,payload_digest,created_at_ms FROM company_events WHERE tenant_id=?1 AND project_id IS NOT NULL AND sequence>?2 ORDER BY sequence LIMIT ?3",
            )
            .map_err(WorkflowError::from)?;
        let rows = statement
            .query_map(
                params![
                    tenant_id.0,
                    sql_u64(after)?,
                    i64::try_from(limit)
                        .map_err(|_| invalid("company event read limit is invalid"))?
                ],
                |row| {
                    Ok(CompanyEventRow {
                        sequence: row.get(0)?,
                        event_id: row.get(1)?,
                        tenant_id: row.get(2)?,
                        project_id: row.get(3)?,
                        event_type: row.get(4)?,
                        operation_id: row.get(5)?,
                        operation_digest: row.get(6)?,
                        principal_id: row.get(7)?,
                        principal_kind: row.get(8)?,
                        principal_role: row.get(9)?,
                        agent_id: row.get(10)?,
                        customer_id: row.get(11)?,
                        authority_generation: row.get(12)?,
                        authority_digest: row.get(13)?,
                        authority_binding_digest: row.get(14)?,
                        payload: row.get(15)?,
                        payload_digest: row.get(16)?,
                        created_at_ms: row.get(17)?,
                    })
                },
            )
            .map_err(WorkflowError::from)?;
        rows.map(|row| {
            let row = row.map_err(WorkflowError::from)?;
            let (sequence, project) = validate_project_snapshot_event(&connection, &row)?;
            Ok(CompanyProjectEventViewV1 {
                sequence,
                event_id: row.event_id,
                tenant_id: project.tenant_id.clone(),
                project_id: project.project_id.clone(),
                event_type: row.event_type,
                operation_id: Uuid::parse_str(&row.operation_id).map_err(|_| corrupt())?,
                principal_id: row.principal_id,
                principal_kind: parse_principal_kind(&row.principal_kind)?,
                principal_role: parse_company_role(&row.principal_role)?,
                created_at_unix_ms: stored_u64(row.created_at_ms)?,
                project,
            })
        })
        .collect()
    }

    pub fn rebuild_company_project_projections(&self) -> Result<usize, WorkflowError> {
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(WorkflowError::from)?;
        transaction
            .execute("DELETE FROM company_project_projections", [])
            .map_err(WorkflowError::from)?;
        let snapshots = {
            let mut statement = transaction
                .prepare(
                    "SELECT sequence,event_id,tenant_id,project_id,event_type,operation_id,operation_digest,principal_id,principal_kind,principal_role,agent_id,customer_id,authority_generation,authority_digest,authority_binding_digest,payload,payload_digest,created_at_ms FROM company_events WHERE event_type LIKE 'project_%' ORDER BY sequence",
                )
                .map_err(WorkflowError::from)?;
            let rows = statement
                .query_map([], |row| {
                    Ok(CompanyEventRow {
                        sequence: row.get(0)?,
                        event_id: row.get(1)?,
                        tenant_id: row.get(2)?,
                        project_id: row.get(3)?,
                        event_type: row.get(4)?,
                        operation_id: row.get(5)?,
                        operation_digest: row.get(6)?,
                        principal_id: row.get(7)?,
                        principal_kind: row.get(8)?,
                        principal_role: row.get(9)?,
                        agent_id: row.get(10)?,
                        customer_id: row.get(11)?,
                        authority_generation: row.get(12)?,
                        authority_digest: row.get(13)?,
                        authority_binding_digest: row.get(14)?,
                        payload: row.get(15)?,
                        payload_digest: row.get(16)?,
                        created_at_ms: row.get(17)?,
                    })
                })
                .map_err(WorkflowError::from)?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(WorkflowError::from)?
        };
        let mut latest = BTreeMap::<(TenantId, ProjectId), (u64, ProjectV1)>::new();
        for row in snapshots {
            let (sequence, project) = validate_project_snapshot_event(&transaction, &row)?;
            let key = (project.tenant_id.clone(), project.project_id.clone());
            match latest.get(&key) {
                Some((previous_sequence, previous))
                    if sequence > *previous_sequence
                        && project.version == previous.version.saturating_add(1)
                        && project.created_at_unix_ms == previous.created_at_unix_ms
                        && project.updated_at_unix_ms >= previous.updated_at_unix_ms
                        && row.event_type != "project_created" => {}
                None if project.version == 1 && row.event_type == "project_created" => {}
                _ => return Err(corrupt()),
            }
            latest.insert(key, (sequence, project));
        }
        for ((tenant_id, project_id), (sequence, project)) in &latest {
            let stored: ProjectV1 = get_entity(&transaction, tenant_id, "project", &project_id.0)?
                .ok_or_else(corrupt)?;
            if stored != *project {
                return Err(corrupt());
            }
            put_projection(&transaction, tenant_id, project_id, *sequence, project)?;
        }
        let project_count: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM company_entities WHERE entity_kind='project'",
                [],
                |row| row.get(0),
            )
            .map_err(WorkflowError::from)?;
        if stored_u64(project_count)? != latest.len() as u64 {
            return Err(corrupt());
        }
        transaction.commit().map_err(WorkflowError::from)?;
        Ok(latest.len())
    }
}

fn read_company_event_row(
    connection: &Connection,
    sequence: u64,
) -> Result<Option<CompanyEventRow>, WorkflowError> {
    connection
        .query_row(
            "SELECT sequence,event_id,tenant_id,project_id,event_type,operation_id,operation_digest,principal_id,principal_kind,principal_role,agent_id,customer_id,authority_generation,authority_digest,authority_binding_digest,payload,payload_digest,created_at_ms FROM company_events WHERE sequence=?1",
            [sql_u64(sequence)?],
            |row| {
                Ok(CompanyEventRow {
                    sequence: row.get(0)?, event_id: row.get(1)?, tenant_id: row.get(2)?, project_id: row.get(3)?, event_type: row.get(4)?, operation_id: row.get(5)?, operation_digest: row.get(6)?, principal_id: row.get(7)?, principal_kind: row.get(8)?, principal_role: row.get(9)?, agent_id: row.get(10)?, customer_id: row.get(11)?, authority_generation: row.get(12)?, authority_digest: row.get(13)?, authority_binding_digest: row.get(14)?, payload: row.get(15)?, payload_digest: row.get(16)?, created_at_ms: row.get(17)?,
                })
            },
        )
        .optional()
        .map_err(WorkflowError::from)
}

#[allow(clippy::too_many_arguments)]
fn apply_company_command(
    transaction: &Transaction<'_>,
    principal: &AuthenticatedCompanyPrincipalV1,
    operation_id: Uuid,
    operation_digest: &str,
    command: &CompanyWorkflowCommandV1,
    now_ms: u64,
    fail_after_agreement: bool,
) -> Result<CompanyWorkflowResponseV1, WorkflowError> {
    match command {
        CompanyWorkflowCommandV1::SubmitCustomerRequest {
            summary_ref,
            desired_outcome,
            constraints,
        } => {
            require_role(principal, &[CompanyRoleV1::Customer])?;
            validate_text(summary_ref)?;
            validate_text(desired_outcome)?;
            validate_text_collection(constraints, false)?;
            let customer_id = principal.customer_id.clone().ok_or_else(unauthorized)?;
            let request = CustomerRequestV1 {
                schema_version: COMPANY_DOMAIN_SCHEMA_VERSION,
                request_id: stable_domain_id("request", &principal.tenant_id, operation_id)?,
                tenant_id: principal.tenant_id.clone(),
                customer_id,
                summary_ref: summary_ref.clone(),
                desired_outcome: desired_outcome.clone(),
                constraints: constraints.clone(),
                clarifications: Vec::new(),
                feedback: Vec::new(),
                state: CustomerRequestStateV1::Submitted,
                version: 1,
                proposal_ids: Vec::new(),
                created_at_unix_ms: now_ms,
                updated_at_unix_ms: now_ms,
            };
            put_entity(
                transaction,
                &request.tenant_id,
                "request",
                &request.request_id,
                request.version,
                &request,
            )?;
            append_event(
                transaction,
                principal,
                operation_id,
                operation_digest,
                None,
                "customer_request_submitted",
                &request,
                now_ms,
            )?;
            Ok(CompanyWorkflowResponseV1::CustomerRequest(request))
        }
        CompanyWorkflowCommandV1::ClarifyCustomerRequest {
            request_id,
            expected_version,
            question_ref,
            answer_ref,
        } => {
            require_role(principal, &[CompanyRoleV1::Customer, CompanyRoleV1::Sales])?;
            validate_text(question_ref)?;
            validate_text(answer_ref)?;
            let mut request = required_request(transaction, principal, request_id, now_ms)?;
            require_version(request.version, *expected_version)?;
            if !matches!(
                request.state,
                CustomerRequestStateV1::Submitted | CustomerRequestStateV1::Clarifying
            ) {
                return Err(transition());
            }
            ensure_collection_capacity(request.clarifications.len())?;
            request.clarifications.push(ClarificationV1 {
                question_ref: question_ref.clone(),
                answer_ref: answer_ref.clone(),
                recorded_by: principal.principal_id.clone(),
                recorded_at_unix_ms: now_ms,
            });
            request.state = CustomerRequestStateV1::Clarifying;
            bump_request(
                transaction,
                principal,
                operation_id,
                operation_digest,
                &mut request,
                "customer_request_clarified",
                now_ms,
            )?;
            Ok(CompanyWorkflowResponseV1::CustomerRequest(request))
        }
        CompanyWorkflowCommandV1::QualifyCustomerRequest {
            request_id,
            expected_version,
            reason_ref,
        } => {
            require_role(principal, &[CompanyRoleV1::Sales])?;
            validate_text(reason_ref)?;
            let mut request = required_request(transaction, principal, request_id, now_ms)?;
            require_version(request.version, *expected_version)?;
            if !matches!(
                request.state,
                CustomerRequestStateV1::Submitted | CustomerRequestStateV1::Clarifying
            ) {
                return Err(transition());
            }
            request.state = CustomerRequestStateV1::Qualified;
            bump_request(
                transaction,
                principal,
                operation_id,
                operation_digest,
                &mut request,
                "customer_request_qualified",
                now_ms,
            )?;
            Ok(CompanyWorkflowResponseV1::CustomerRequest(request))
        }
        CompanyWorkflowCommandV1::CreateProposal {
            request_id,
            expected_version,
            binding,
        } => {
            require_role(principal, &[CompanyRoleV1::Sales])?;
            binding.validate(now_ms)?;
            let mut request = required_request(transaction, principal, request_id, now_ms)?;
            require_version(request.version, *expected_version)?;
            if request.state != CustomerRequestStateV1::Qualified {
                return Err(transition());
            }
            let proposal_id = stable_domain_id("proposal", &principal.tenant_id, operation_id)?;
            let proposal_digest =
                canonical_sha256("sentinel.workflow.proposal-binding.v1", binding)?;
            let proposal = ProposalV1 {
                schema_version: COMPANY_DOMAIN_SCHEMA_VERSION,
                proposal_id: proposal_id.clone(),
                tenant_id: principal.tenant_id.clone(),
                request_id: request.request_id.clone(),
                generation: u32::try_from(request.proposal_ids.len() + 1)
                    .map_err(|_| invalid("proposal generation overflow"))?,
                binding: binding.clone(),
                proposal_digest,
                created_by: principal.principal_id.clone(),
                created_at_unix_ms: now_ms,
            };
            ensure_collection_capacity(request.proposal_ids.len())?;
            request.proposal_ids.push(proposal_id.clone());
            request.state = CustomerRequestStateV1::Proposed;
            request.version = request
                .version
                .checked_add(1)
                .ok_or_else(|| invalid("request version overflow"))?;
            request.updated_at_unix_ms = now_ms;
            put_entity(
                transaction,
                &principal.tenant_id,
                "proposal",
                &proposal_id,
                u64::from(proposal.generation),
                &proposal,
            )?;
            put_entity(
                transaction,
                &principal.tenant_id,
                "request",
                &request.request_id,
                request.version,
                &request,
            )?;
            append_event(
                transaction,
                principal,
                operation_id,
                operation_digest,
                None,
                "proposal_created",
                &proposal,
                now_ms,
            )?;
            Ok(CompanyWorkflowResponseV1::Proposal(proposal))
        }
        CompanyWorkflowCommandV1::AcceptProposal {
            request_id,
            expected_version,
            proposal_id,
            proposal_digest,
        } => {
            require_role(principal, &[CompanyRoleV1::Customer])?;
            let mut request = required_request(transaction, principal, request_id, now_ms)?;
            require_version(request.version, *expected_version)?;
            if request.state != CustomerRequestStateV1::Proposed {
                return Err(transition());
            }
            let proposal: ProposalV1 =
                get_entity(transaction, &principal.tenant_id, "proposal", proposal_id)?
                    .ok_or_else(not_found)?;
            require_non_regressing_time(now_ms, proposal.created_at_unix_ms)?;
            if proposal.request_id != request.request_id
                || proposal.tenant_id != principal.tenant_id
                || !constant_time_eq(&proposal.proposal_digest, proposal_digest)
                || proposal.proposal_digest
                    != canonical_sha256("sentinel.workflow.proposal-binding.v1", &proposal.binding)?
                || proposal.binding.expires_at_unix_ms <= now_ms
            {
                return Err(WorkflowError::new(
                    WorkflowErrorCode::InvalidDigest,
                    false,
                    "accepted proposal binding is invalid",
                ));
            }
            let agreement = AgreementV1 {
                schema_version: COMPANY_DOMAIN_SCHEMA_VERSION,
                agreement_id: stable_domain_id("agreement", &principal.tenant_id, operation_id)?,
                tenant_id: principal.tenant_id.clone(),
                request_id: request.request_id.clone(),
                proposal_id: proposal.proposal_id.clone(),
                proposal_digest: proposal.proposal_digest.clone(),
                customer_id: request.customer_id.clone(),
                accepted_by: principal.principal_id.clone(),
                accepted_at_unix_ms: now_ms,
            };
            let project_id = ProjectId::parse(stable_domain_id(
                "project",
                &principal.tenant_id,
                operation_id,
            )?)?;
            let project = ProjectV1 {
                schema_version: COMPANY_DOMAIN_SCHEMA_VERSION,
                tenant_id: principal.tenant_id.clone(),
                project_id: project_id.clone(),
                agreement_id: agreement.agreement_id.clone(),
                agreement_digest: agreement.proposal_digest.clone(),
                governance: proposal.binding.governance.clone(),
                cost_ceiling_micros: proposal.binding.cost_ceiling_micros,
                provider_cost_ceilings_micros: proposal
                    .binding
                    .provider_cost_ceilings_micros
                    .clone(),
                lifecycle_state: ProjectLifecycleStateV1::Planning,
                reserved_cost_micros: 0,
                committed_cost_micros: 0,
                work_items: BTreeMap::new(),
                decisions: Vec::new(),
                handoffs: Vec::new(),
                blockers: Vec::new(),
                approvals: Vec::new(),
                reservations: Vec::new(),
                rooms: Vec::new(),
                questions: Vec::new(),
                actions: Vec::new(),
                version: 1,
                created_at_unix_ms: now_ms,
                updated_at_unix_ms: now_ms,
            };
            validate_project(&project)?;
            request.state = CustomerRequestStateV1::Accepted;
            request.version += 1;
            request.updated_at_unix_ms = now_ms;
            put_entity(
                transaction,
                &principal.tenant_id,
                "agreement",
                &agreement.agreement_id,
                1,
                &agreement,
            )?;
            if fail_after_agreement {
                return Err(persistence());
            }
            put_entity(
                transaction,
                &principal.tenant_id,
                "project",
                &project_id.0,
                project.version,
                &project,
            )?;
            put_entity(
                transaction,
                &principal.tenant_id,
                "request",
                &request.request_id,
                request.version,
                &request,
            )?;
            append_event(
                transaction,
                principal,
                operation_id,
                operation_digest,
                Some(&project_id),
                "agreement_accepted",
                &agreement,
                now_ms,
            )?;
            append_project_snapshot(
                transaction,
                principal,
                operation_id,
                operation_digest,
                "project_created",
                &project,
                now_ms,
            )?;
            Ok(CompanyWorkflowResponseV1::AgreementProject {
                agreement: Box::new(agreement),
                project: Box::new(project),
            })
        }
        CompanyWorkflowCommandV1::RejectProposal {
            request_id,
            expected_version,
            proposal_id,
            proposal_digest,
            reason_ref,
        } => {
            require_role(principal, &[CompanyRoleV1::Customer])?;
            validate_text(reason_ref)?;
            let mut request = required_request(transaction, principal, request_id, now_ms)?;
            require_version(request.version, *expected_version)?;
            let proposal: ProposalV1 =
                get_entity(transaction, &principal.tenant_id, "proposal", proposal_id)?
                    .ok_or_else(not_found)?;
            require_non_regressing_time(now_ms, proposal.created_at_unix_ms)?;
            if request.state != CustomerRequestStateV1::Proposed
                || proposal.request_id != request.request_id
                || !constant_time_eq(&proposal.proposal_digest, proposal_digest)
            {
                return Err(transition());
            }
            request.state = CustomerRequestStateV1::Rejected;
            bump_request(
                transaction,
                principal,
                operation_id,
                operation_digest,
                &mut request,
                "customer_request_rejected",
                now_ms,
            )?;
            Ok(CompanyWorkflowResponseV1::CustomerRequest(request))
        }
        CompanyWorkflowCommandV1::CancelCustomerRequest {
            request_id,
            expected_version,
            reason_ref,
        } => {
            require_role(principal, &[CompanyRoleV1::Customer])?;
            validate_text(reason_ref)?;
            let mut request = required_request(transaction, principal, request_id, now_ms)?;
            require_version(request.version, *expected_version)?;
            if matches!(
                request.state,
                CustomerRequestStateV1::Accepted
                    | CustomerRequestStateV1::Rejected
                    | CustomerRequestStateV1::Cancelled
            ) {
                return Err(transition());
            }
            request.state = CustomerRequestStateV1::Cancelled;
            bump_request(
                transaction,
                principal,
                operation_id,
                operation_digest,
                &mut request,
                "customer_request_cancelled",
                now_ms,
            )?;
            Ok(CompanyWorkflowResponseV1::CustomerRequest(request))
        }
        CompanyWorkflowCommandV1::RecordCustomerFeedback {
            request_id,
            expected_version,
            feedback_ref,
        } => {
            require_role(principal, &[CompanyRoleV1::Customer])?;
            validate_text(feedback_ref)?;
            let mut request = required_request(transaction, principal, request_id, now_ms)?;
            require_version(request.version, *expected_version)?;
            if !matches!(
                request.state,
                CustomerRequestStateV1::Accepted
                    | CustomerRequestStateV1::Rejected
                    | CustomerRequestStateV1::Cancelled
            ) {
                return Err(transition());
            }
            ensure_collection_capacity(request.feedback.len())?;
            request.feedback.push(CustomerFeedbackV1 {
                feedback_ref: feedback_ref.clone(),
                recorded_by: principal.principal_id.clone(),
                recorded_at_unix_ms: now_ms,
            });
            bump_request(
                transaction,
                principal,
                operation_id,
                operation_digest,
                &mut request,
                "customer_feedback_recorded",
                now_ms,
            )?;
            Ok(CompanyWorkflowResponseV1::CustomerRequest(request))
        }
        CompanyWorkflowCommandV1::CreateGovernedRework {
            project_id,
            expected_version,
            source_candidate_digest,
            feedback_digest,
            source_delivery_id,
        } => apply_governed_rework(
            transaction,
            principal,
            operation_id,
            operation_digest,
            project_id,
            *expected_version,
            source_candidate_digest,
            feedback_digest,
            source_delivery_id,
            now_ms,
        ),
        _ => mutate_project(
            transaction,
            principal,
            operation_id,
            operation_digest,
            command,
            now_ms,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_governed_rework(
    transaction: &Transaction<'_>,
    principal: &AuthenticatedCompanyPrincipalV1,
    operation_id: Uuid,
    operation_digest: &str,
    project_id: &ProjectId,
    expected_version: u64,
    source_candidate_digest: &str,
    feedback_digest: &str,
    source_delivery_id: &str,
    now_ms: u64,
) -> Result<CompanyWorkflowResponseV1, WorkflowError> {
    require_role(principal, &[CompanyRoleV1::Customer])?;
    validate_digest(source_candidate_digest)?;
    validate_digest(feedback_digest)?;
    validate_identifier(source_delivery_id)?;
    let mut project: ProjectV1 =
        get_entity(transaction, &principal.tenant_id, "project", &project_id.0)?
            .ok_or_else(not_found)?;
    require_non_regressing_time(now_ms, project.updated_at_unix_ms)?;
    require_version(project.version, expected_version)?;
    if project.lifecycle_state != ProjectLifecycleStateV1::DeliveryCandidate
        || project.work_items.is_empty()
        || project
            .work_items
            .values()
            .any(|work| work.state != CompanyWorkStateV1::Done)
    {
        return Err(transition());
    }
    let agreement: AgreementV1 = get_entity(
        transaction,
        &principal.tenant_id,
        "agreement",
        &project.agreement_id,
    )?
    .ok_or_else(corrupt)?;
    if agreement.accepted_by != principal.principal_id
        || principal.customer_id.as_deref() != Some(agreement.customer_id.as_str())
    {
        return Err(unauthorized());
    }

    let current_generation = project
        .work_items
        .values()
        .map(|work| {
            work.spec
                .rework
                .as_ref()
                .map_or(0, |binding| binding.generation)
        })
        .max()
        .unwrap_or(0);
    let sources = project
        .work_items
        .values()
        .filter(|work| {
            work.spec
                .rework
                .as_ref()
                .map_or(current_generation == 0, |binding| {
                    binding.generation == current_generation
                })
        })
        .cloned()
        .collect::<Vec<_>>();
    if sources.is_empty()
        || project.work_items.len().saturating_add(sources.len()) > MAX_AGGREGATE_ITEMS
    {
        return Err(invalid(
            "governed rework would exceed the project work limit",
        ));
    }
    let existing_budget = project.work_items.values().try_fold(0_u64, |total, work| {
        total.checked_add(work.spec.budget_micros)
    });
    let rework_budget = sources.iter().try_fold(0_u64, |total, work| {
        total.checked_add(work.spec.budget_micros)
    });
    if existing_budget
        .and_then(|total| rework_budget.and_then(|rework| total.checked_add(rework)))
        .is_none_or(|total| total > project.cost_ceiling_micros)
    {
        return Err(invalid("governed rework exceeds the project cost ceiling"));
    }
    let next_generation = current_generation
        .checked_add(1)
        .ok_or_else(|| invalid("governed rework generation overflow"))?;
    let mut mapped_ids = BTreeMap::new();
    for source in &sources {
        let digest = canonical_sha256(
            "sentinel.workflow.governed-rework-item.v1",
            &(
                &principal.tenant_id,
                project_id,
                operation_id,
                &source.spec.work_item_id,
                next_generation,
            ),
        )?;
        mapped_ids.insert(
            source.spec.work_item_id.clone(),
            crate::WorkItemId::parse(format!("rework-{}", &digest[..24]))?,
        );
    }
    let mut created = Vec::with_capacity(sources.len());
    for source in sources {
        let mut spec = source.spec.clone();
        let new_id = mapped_ids
            .get(&source.spec.work_item_id)
            .cloned()
            .ok_or_else(corrupt)?;
        spec.work_item_id = new_id.clone();
        spec.dependency_ids = source
            .spec
            .dependency_ids
            .iter()
            .map(|dependency| mapped_ids.get(dependency).cloned().ok_or_else(corrupt))
            .collect::<Result<BTreeSet<_>, _>>()?;
        for input in &mut spec.inputs {
            input.producer_work_item_id = mapped_ids
                .get(&input.producer_work_item_id)
                .cloned()
                .ok_or_else(corrupt)?;
        }
        spec.rework = Some(GovernedReworkBindingV1 {
            operation_id,
            source_work_item_id: source.spec.work_item_id,
            source_delivery_id: source_delivery_id.to_string(),
            source_candidate_digest: source_candidate_digest.to_string(),
            feedback_digest: feedback_digest.to_string(),
            generation: next_generation,
        });
        let state = if spec.dependency_ids.is_empty() {
            CompanyWorkStateV1::Ready
        } else {
            CompanyWorkStateV1::DependencyPending
        };
        created.push((
            new_id,
            CompanyWorkItemV1 {
                spec,
                state,
                version: 1,
                assignments: Vec::new(),
                output_receipts: Vec::new(),
                gate_receipt: None,
                transition_history: Vec::new(),
            },
        ));
    }
    for (id, work) in created {
        if project.work_items.insert(id, work).is_some() {
            return Err(corrupt());
        }
    }
    project.lifecycle_state = ProjectLifecycleStateV1::Active;
    project.version = project
        .version
        .checked_add(1)
        .ok_or_else(|| invalid("project version overflow"))?;
    project.updated_at_unix_ms = now_ms;
    validate_project(&project)?;
    put_entity(
        transaction,
        &principal.tenant_id,
        "project",
        &project.project_id.0,
        project.version,
        &project,
    )?;
    append_project_snapshot(
        transaction,
        principal,
        operation_id,
        operation_digest,
        "project_governed_rework_created",
        &project,
        now_ms,
    )?;
    Ok(CompanyWorkflowResponseV1::Project(project))
}

fn mutate_project(
    transaction: &Transaction<'_>,
    principal: &AuthenticatedCompanyPrincipalV1,
    operation_id: Uuid,
    operation_digest: &str,
    command: &CompanyWorkflowCommandV1,
    now_ms: u64,
) -> Result<CompanyWorkflowResponseV1, WorkflowError> {
    let (project_id, expected_version) =
        project_target(command).ok_or_else(|| invalid("command has no project target"))?;
    let mut project: ProjectV1 =
        get_entity(transaction, &principal.tenant_id, "project", &project_id.0)?
            .ok_or_else(not_found)?;
    require_non_regressing_time(now_ms, project.updated_at_unix_ms)?;
    require_version(project.version, expected_version)?;
    let actor_id = authorize_project_actor(&project, principal)?;
    match command {
        CompanyWorkflowCommandV1::PlanWorkGraph { items, .. } => {
            require_role(
                principal,
                &[CompanyRoleV1::ProjectManager, CompanyRoleV1::TechnicalLead],
            )?;
            validate_work_graph(items)?;
            let total = items
                .iter()
                .try_fold(0_u64, |sum, item| sum.checked_add(item.budget_micros))
                .ok_or_else(|| invalid("work graph budget overflow"))?;
            if total > project.cost_ceiling_micros {
                return Err(invalid("work graph exceeds project cost ceiling"));
            }
            if project.lifecycle_state != ProjectLifecycleStateV1::Planning
                || !project.work_items.is_empty()
            {
                return Err(transition());
            }
            for item in items {
                if !project
                    .governance
                    .participants
                    .iter()
                    .any(|participant| participant.agent_id == item.owner)
                {
                    return Err(unauthorized());
                }
            }
            project.work_items = items
                .iter()
                .map(|spec| {
                    (
                        spec.work_item_id.clone(),
                        CompanyWorkItemV1 {
                            spec: spec.clone(),
                            state: if spec.dependency_ids.is_empty() {
                                CompanyWorkStateV1::Ready
                            } else {
                                CompanyWorkStateV1::DependencyPending
                            },
                            version: 1,
                            assignments: Vec::new(),
                            output_receipts: Vec::new(),
                            gate_receipt: None,
                            transition_history: Vec::new(),
                        },
                    )
                })
                .collect();
        }
        CompanyWorkflowCommandV1::ActivateProject { reason_ref, .. } => {
            require_role(
                principal,
                &[CompanyRoleV1::ProjectManager, CompanyRoleV1::TechnicalLead],
            )?;
            validate_text(reason_ref)?;
            if project.lifecycle_state != ProjectLifecycleStateV1::Planning
                || project.work_items.is_empty()
            {
                return Err(transition());
            }
            project.lifecycle_state = ProjectLifecycleStateV1::Active;
        }
        CompanyWorkflowCommandV1::AssignWork {
            work_item_id,
            agent_id,
            organization_generation,
            organization_digest,
            reason_ref,
            ..
        } => {
            require_role(
                principal,
                &[CompanyRoleV1::ProjectManager, CompanyRoleV1::TechnicalLead],
            )?;
            validate_digest(organization_digest)?;
            validate_text(reason_ref)?;
            if *organization_generation == 0 {
                return Err(invalid("organization generation must be positive"));
            }
            let participant = project
                .governance
                .participants
                .iter()
                .find(|item| item.agent_id == *agent_id)
                .ok_or_else(unauthorized)?;
            let work = project
                .work_items
                .get_mut(work_item_id)
                .ok_or_else(not_found)?;
            if project.lifecycle_state != ProjectLifecycleStateV1::Active
                || !work.assignments.is_empty()
                || work.state != CompanyWorkStateV1::Ready
                || work.spec.owner != *agent_id
                || participant.role != work.spec.required_role
                || !work
                    .spec
                    .required_specialties
                    .is_subset(&participant.specialties)
            {
                return Err(unauthorized());
            }
            ensure_collection_capacity(work.assignments.len())?;
            work.assignments.push(AssignmentV1 {
                assignment_id: stable_domain_id("assignment", &principal.tenant_id, operation_id)?,
                agent_id: *agent_id,
                role: participant.role,
                specialties: participant.specialties.clone(),
                profile: participant.profile.clone(),
                organization_generation: *organization_generation,
                organization_digest: organization_digest.clone(),
                assignment_version: 1,
                delegated_by: None,
                reason_ref: reason_ref.clone(),
                active: true,
                assigned_by: principal.principal_id.clone(),
                created_at_unix_ms: now_ms,
                ended_at_unix_ms: None,
            });
            append_work_transition(
                work,
                principal,
                actor_id,
                CompanyWorkStateV1::Ready,
                CompanyWorkStateV1::Assigned,
                reason_ref,
                now_ms,
            )?;
            work.state = CompanyWorkStateV1::Assigned;
            work.version += 1;
        }
        CompanyWorkflowCommandV1::ReassignWork {
            work_item_id,
            expected_assignment_version,
            agent_id,
            organization_generation,
            organization_digest,
            reason_ref,
            ..
        } => {
            require_role(
                principal,
                &[CompanyRoleV1::ProjectManager, CompanyRoleV1::TechnicalLead],
            )?;
            validate_digest(organization_digest)?;
            validate_text(reason_ref)?;
            let participant = project
                .governance
                .participants
                .iter()
                .find(|item| item.agent_id == *agent_id)
                .ok_or_else(unauthorized)?;
            let work = project
                .work_items
                .get_mut(work_item_id)
                .ok_or_else(not_found)?;
            let current = work
                .assignments
                .iter_mut()
                .find(|assignment| assignment.active)
                .ok_or_else(transition)?;
            if current.assignment_version != *expected_assignment_version
                || !matches!(
                    work.state,
                    CompanyWorkStateV1::Assigned | CompanyWorkStateV1::Blocked
                )
                || participant.role != work.spec.required_role
                || !work
                    .spec
                    .required_specialties
                    .is_subset(&participant.specialties)
                || *organization_generation == 0
            {
                return Err(unauthorized());
            }
            current.active = false;
            current.ended_at_unix_ms = Some(now_ms);
            let next_version = current
                .assignment_version
                .checked_add(1)
                .ok_or_else(|| invalid("assignment version overflow"))?;
            ensure_collection_capacity(work.assignments.len())?;
            work.assignments.push(AssignmentV1 {
                assignment_id: stable_domain_id("assignment", &principal.tenant_id, operation_id)?,
                agent_id: *agent_id,
                role: participant.role,
                specialties: participant.specialties.clone(),
                profile: participant.profile.clone(),
                organization_generation: *organization_generation,
                organization_digest: organization_digest.clone(),
                assignment_version: next_version,
                delegated_by: None,
                reason_ref: reason_ref.clone(),
                active: true,
                assigned_by: principal.principal_id.clone(),
                created_at_unix_ms: now_ms,
                ended_at_unix_ms: None,
            });
            append_work_transition(
                work,
                principal,
                actor_id,
                work.state,
                CompanyWorkStateV1::Assigned,
                reason_ref,
                now_ms,
            )?;
            work.state = CompanyWorkStateV1::Assigned;
            work.version += 1;
            refresh_project_lifecycle(&mut project);
        }
        CompanyWorkflowCommandV1::DelegateWork {
            work_item_id,
            expected_assignment_version,
            delegate,
            reason_ref,
            ..
        } => {
            validate_text(reason_ref)?;
            let participant = project
                .governance
                .participants
                .iter()
                .find(|item| item.agent_id == *delegate)
                .ok_or_else(unauthorized)?;
            let work = project
                .work_items
                .get_mut(work_item_id)
                .ok_or_else(not_found)?;
            if participant.role != work.spec.required_role
                || !work
                    .spec
                    .required_specialties
                    .is_subset(&participant.specialties)
                || !is_direct_report(&project.governance, *delegate, actor_id)
                || !matches!(
                    work.state,
                    CompanyWorkStateV1::Assigned | CompanyWorkStateV1::Blocked
                )
            {
                return Err(unauthorized());
            }
            let (next_version, organization_generation, organization_digest) = {
                let current = work
                    .assignments
                    .iter_mut()
                    .find(|assignment| assignment.active)
                    .ok_or_else(transition)?;
                if current.agent_id != actor_id
                    || current.assignment_version != *expected_assignment_version
                {
                    return Err(unauthorized());
                }
                current.active = false;
                current.ended_at_unix_ms = Some(now_ms);
                (
                    current
                        .assignment_version
                        .checked_add(1)
                        .ok_or_else(|| invalid("assignment version overflow"))?,
                    current.organization_generation,
                    current.organization_digest.clone(),
                )
            };
            ensure_collection_capacity(work.assignments.len())?;
            work.assignments.push(AssignmentV1 {
                assignment_id: stable_domain_id("assignment", &principal.tenant_id, operation_id)?,
                agent_id: *delegate,
                role: participant.role,
                specialties: participant.specialties.clone(),
                profile: participant.profile.clone(),
                organization_generation,
                organization_digest,
                assignment_version: next_version,
                delegated_by: Some(actor_id),
                reason_ref: reason_ref.clone(),
                active: true,
                assigned_by: principal.principal_id.clone(),
                created_at_unix_ms: now_ms,
                ended_at_unix_ms: None,
            });
            append_work_transition(
                work,
                principal,
                actor_id,
                work.state,
                CompanyWorkStateV1::Assigned,
                reason_ref,
                now_ms,
            )?;
            work.state = CompanyWorkStateV1::Assigned;
            work.version += 1;
            refresh_project_lifecycle(&mut project);
        }
        CompanyWorkflowCommandV1::ApplyWorkTransition { receipt, .. } => {
            if receipt.schema_version != COMPANY_DOMAIN_SCHEMA_VERSION
                || receipt.project_id != project.project_id
                || receipt.expected_project_version != project.version
                || receipt.occurred_at_unix_ms != now_ms
            {
                return Err(transition());
            }
            validate_digest(&receipt.phase_a_evidence_digest)?;
            validate_text(&receipt.reason_ref)?;
            let work = project
                .work_items
                .get_mut(&receipt.work_item_id)
                .ok_or_else(not_found)?;
            let assignment = current_assignment(work).ok_or_else(transition)?.clone();
            if work.version != receipt.expected_work_version
                || work.state != receipt.from_state
                || assignment.assignment_version != receipt.expected_assignment_version
            {
                return Err(WorkflowError::new(
                    WorkflowErrorCode::VersionConflict,
                    false,
                    "work transition predecessor is stale",
                ));
            }
            let actor_is_assignee = assignment.agent_id == actor_id;
            let legal = matches!(
                (receipt.from_state, receipt.to_state),
                (CompanyWorkStateV1::Assigned, CompanyWorkStateV1::InProgress)
                    | (CompanyWorkStateV1::InProgress, CompanyWorkStateV1::InReview)
            ) && actor_is_assignee
                || receipt.from_state == CompanyWorkStateV1::InReview
                    && receipt.to_state == CompanyWorkStateV1::Done
                    && matches!(
                        principal.role,
                        CompanyRoleV1::Qa | CompanyRoleV1::ReleaseManager
                    )
                    && !actor_is_assignee
                || matches!(
                    (receipt.from_state, receipt.to_state),
                    (CompanyWorkStateV1::Assigned, CompanyWorkStateV1::Blocked)
                        | (CompanyWorkStateV1::InProgress, CompanyWorkStateV1::Blocked)
                ) && (actor_is_assignee
                    || matches!(
                        principal.role,
                        CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
                    ));
            if !legal {
                return Err(transition());
            }
            if matches!(
                receipt.to_state,
                CompanyWorkStateV1::InReview | CompanyWorkStateV1::Done
            ) {
                validate_output_receipts(&work.spec, &receipt.output_receipts)?;
            } else if !receipt.output_receipts.is_empty() {
                return Err(invalid("work outputs are not valid for this transition"));
            }
            if receipt.to_state == CompanyWorkStateV1::Done {
                let gate = receipt.gate_receipt.as_ref().ok_or_else(transition)?;
                if !gate.passed
                    || gate.gate_id != work.spec.quality_gate.gate_id
                    || gate.generation != work.spec.quality_gate.generation
                    || !constant_time_eq(&gate.gate_digest, &work.spec.quality_gate.digest)
                {
                    return Err(transition());
                }
                validate_digest(&gate.subject_digest)?;
                work.gate_receipt = Some(gate.clone());
            } else if receipt.gate_receipt.is_some() {
                return Err(invalid("gate evidence is only valid for Done"));
            }
            let before = work.state;
            work.state = receipt.to_state;
            work.output_receipts = receipt.output_receipts.clone();
            work.version = work
                .version
                .checked_add(1)
                .ok_or_else(|| invalid("work item version overflow"))?;
            append_work_transition(
                work,
                principal,
                actor_id,
                before,
                receipt.to_state,
                &receipt.reason_ref,
                now_ms,
            )?;
            refresh_dependency_states(&mut project, principal, actor_id, now_ms)?;
            refresh_project_lifecycle(&mut project);
        }
        CompanyWorkflowCommandV1::RecordDecision {
            work_item_id,
            choice_ref,
            rationale_ref,
            ..
        } => {
            require_role(
                principal,
                &[CompanyRoleV1::ProjectManager, CompanyRoleV1::TechnicalLead],
            )?;
            validate_text(choice_ref)?;
            validate_text(rationale_ref)?;
            validate_optional_work(&project, work_item_id.as_ref())?;
            ensure_collection_capacity(project.decisions.len())?;
            project.decisions.push(DecisionV1 {
                decision_id: stable_domain_id("decision", &principal.tenant_id, operation_id)?,
                work_item_id: work_item_id.clone(),
                choice_ref: choice_ref.clone(),
                rationale_ref: rationale_ref.clone(),
                decided_by: principal.principal_id.clone(),
                created_at_unix_ms: now_ms,
            });
        }
        CompanyWorkflowCommandV1::CreateHandoff {
            work_item_id,
            consumer,
            artifact_digests,
            reason_ref,
            ..
        } => {
            require_role(
                principal,
                &[
                    CompanyRoleV1::Developer,
                    CompanyRoleV1::Designer,
                    CompanyRoleV1::TechnicalLead,
                ],
            )?;
            let producer = principal.agent_id.ok_or_else(unauthorized)?;
            if producer == *consumer
                || artifact_digests.is_empty()
                || !project
                    .governance
                    .participants
                    .iter()
                    .any(|p| p.agent_id == *consumer)
            {
                return Err(unauthorized());
            }
            for digest in artifact_digests {
                validate_digest(digest)?;
            }
            validate_text(reason_ref)?;
            let work = project.work_items.get(work_item_id).ok_or_else(not_found)?;
            if current_assignment(work).map(|value| value.agent_id) != Some(producer) {
                return Err(unauthorized());
            }
            ensure_collection_capacity(project.handoffs.len())?;
            project.handoffs.push(HandoffV1 {
                handoff_id: stable_domain_id("handoff", &principal.tenant_id, operation_id)?,
                work_item_id: work_item_id.clone(),
                producer,
                consumer: *consumer,
                artifact_digests: artifact_digests.clone(),
                state: HandoffStateV1::Offered,
                reason_ref: reason_ref.clone(),
                created_at_unix_ms: now_ms,
                acknowledged_by: None,
                acknowledged_at_unix_ms: None,
                acknowledgement_reason_ref: None,
                transition_history: Vec::new(),
            });
        }
        CompanyWorkflowCommandV1::AcknowledgeHandoff {
            handoff_id,
            accepted,
            reason_ref,
            ..
        } => {
            validate_identifier(handoff_id)?;
            validate_text(reason_ref)?;
            let actor = principal.agent_id.ok_or_else(unauthorized)?;
            let handoff = project
                .handoffs
                .iter_mut()
                .find(|value| value.handoff_id == *handoff_id)
                .ok_or_else(not_found)?;
            if handoff.consumer != actor || handoff.state != HandoffStateV1::Offered {
                return Err(unauthorized());
            }
            handoff.state = if *accepted {
                HandoffStateV1::Accepted
            } else {
                HandoffStateV1::Rejected
            };
            handoff.acknowledged_by = Some(principal.principal_id.clone());
            handoff.acknowledged_at_unix_ms = Some(now_ms);
            handoff.acknowledgement_reason_ref = Some(reason_ref.clone());
            ensure_collection_capacity(handoff.transition_history.len())?;
            handoff.transition_history.push(StateTransitionAuditV1 {
                before: "Offered".to_owned(),
                after: enum_name(handoff.state),
                actor_id: principal.principal_id.clone(),
                actor_agent_id: actor,
                reason_ref: reason_ref.clone(),
                occurred_at_unix_ms: now_ms,
            });
        }
        CompanyWorkflowCommandV1::RaiseBlocker {
            work_item_id,
            cause_ref,
            owner,
            ..
        } => {
            if !matches!(
                principal.role,
                CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
            ) && *owner != actor_id
            {
                return Err(unauthorized());
            }
            validate_text(cause_ref)?;
            validate_optional_work(&project, work_item_id.as_ref())?;
            if !project
                .governance
                .participants
                .iter()
                .any(|p| p.agent_id == *owner)
            {
                return Err(unauthorized());
            }
            ensure_collection_capacity(project.blockers.len())?;
            project.blockers.push(BlockerV1 {
                blocker_id: stable_domain_id("blocker", &principal.tenant_id, operation_id)?,
                work_item_id: work_item_id.clone(),
                cause_ref: cause_ref.clone(),
                owner: *owner,
                escalation_target: None,
                state: BlockerStateV1::Open,
                blocker_kind: BlockerKindV1::Operational,
                blocked_from_state: Some(project.lifecycle_state),
                resolution_ref: None,
                last_actor_id: principal.principal_id.clone(),
                created_at_unix_ms: now_ms,
                updated_at_unix_ms: now_ms,
                transition_history: Vec::new(),
            });
            project.lifecycle_state = ProjectLifecycleStateV1::Blocked;
        }
        CompanyWorkflowCommandV1::EscalateBlocker {
            blocker_id,
            escalation_target,
            reason_ref,
            ..
        } => {
            require_role(
                principal,
                &[CompanyRoleV1::ProjectManager, CompanyRoleV1::TechnicalLead],
            )?;
            validate_text(reason_ref)?;
            let blocker = project
                .blockers
                .iter_mut()
                .find(|value| value.blocker_id == *blocker_id)
                .ok_or_else(not_found)?;
            if blocker.state != BlockerStateV1::Open
                || !project
                    .governance
                    .participants
                    .iter()
                    .any(|p| p.agent_id == *escalation_target)
            {
                return Err(transition());
            }
            blocker.state = BlockerStateV1::Escalated;
            blocker.escalation_target = Some(*escalation_target);
            blocker.last_actor_id = principal.principal_id.clone();
            blocker.updated_at_unix_ms = now_ms;
            ensure_collection_capacity(blocker.transition_history.len())?;
            blocker.transition_history.push(StateTransitionAuditV1 {
                before: "Open".to_owned(),
                after: "Escalated".to_owned(),
                actor_id: principal.principal_id.clone(),
                actor_agent_id: actor_id,
                reason_ref: reason_ref.clone(),
                occurred_at_unix_ms: now_ms,
            });
            refresh_project_lifecycle(&mut project);
        }
        CompanyWorkflowCommandV1::ResolveBlocker {
            blocker_id,
            resolution_ref,
            ..
        } => {
            validate_identifier(blocker_id)?;
            validate_text(resolution_ref)?;
            let actor = principal.agent_id.ok_or_else(unauthorized)?;
            let blocker = project
                .blockers
                .iter_mut()
                .find(|value| value.blocker_id == *blocker_id)
                .ok_or_else(not_found)?;
            if !matches!(
                blocker.state,
                BlockerStateV1::Open | BlockerStateV1::Escalated
            ) || (blocker.owner != actor && blocker.escalation_target != Some(actor))
            {
                return Err(unauthorized());
            }
            blocker.state = BlockerStateV1::Resolved;
            blocker.resolution_ref = Some(resolution_ref.clone());
            blocker.last_actor_id = principal.principal_id.clone();
            blocker.updated_at_unix_ms = now_ms;
            ensure_collection_capacity(blocker.transition_history.len())?;
            blocker.transition_history.push(StateTransitionAuditV1 {
                before: if blocker.escalation_target.is_some() {
                    "Escalated".to_owned()
                } else {
                    "Open".to_owned()
                },
                after: "Resolved".to_owned(),
                actor_id: principal.principal_id.clone(),
                actor_agent_id: actor,
                reason_ref: resolution_ref.clone(),
                occurred_at_unix_ms: now_ms,
            });
            restore_project_lifecycle_after_last_blocker(&mut project);
            refresh_project_lifecycle(&mut project);
        }
        CompanyWorkflowCommandV1::RecordApproval {
            work_item_id,
            subject_digest,
            approved,
            ..
        } => {
            require_role(
                principal,
                &[CompanyRoleV1::Qa, CompanyRoleV1::ReleaseManager],
            )?;
            validate_digest(subject_digest)?;
            let actor = principal.agent_id.ok_or_else(unauthorized)?;
            let work = project.work_items.get(work_item_id).ok_or_else(not_found)?;
            if current_assignment(work).map(|binding| binding.agent_id) == Some(actor) {
                return Err(unauthorized());
            }
            ensure_collection_capacity(project.approvals.len())?;
            project.approvals.push(ApprovalV1 {
                approval_id: stable_domain_id("approval", &principal.tenant_id, operation_id)?,
                work_item_id: work_item_id.clone(),
                subject_digest: subject_digest.clone(),
                approved: *approved,
                actor_id: principal.principal_id.clone(),
                actor_agent_id: actor,
                created_at_unix_ms: now_ms,
            });
        }
        CompanyWorkflowCommandV1::ReserveCost {
            work_item_id,
            provider,
            amount_micros,
            ..
        } => {
            require_role(
                principal,
                &[CompanyRoleV1::ProjectManager, CompanyRoleV1::TechnicalLead],
            )?;
            if project.lifecycle_state != ProjectLifecycleStateV1::Active {
                return Err(transition());
            }
            validate_identifier(provider)?;
            validate_optional_work(&project, work_item_id.as_ref())?;
            let provider_ceiling = project
                .provider_cost_ceilings_micros
                .get(provider)
                .copied()
                .ok_or_else(unauthorized)?;
            let provider_reserved = project
                .reservations
                .iter()
                .filter(|r| r.provider == *provider)
                .try_fold(0_u64, |sum, item| {
                    sum.checked_add(reservation_effective_amount(item))
                })
                .ok_or_else(|| invalid("provider reservation overflow"))?;
            let total = project
                .reserved_cost_micros
                .checked_add(*amount_micros)
                .ok_or_else(|| invalid("project reservation overflow"))?;
            let provider_total = provider_reserved
                .checked_add(*amount_micros)
                .ok_or_else(|| invalid("provider reservation overflow"))?;
            let work_total = work_item_id
                .as_ref()
                .map(|work_id| {
                    let budget = project
                        .work_items
                        .get(work_id)
                        .ok_or_else(not_found)?
                        .spec
                        .budget_micros;
                    let current = project
                        .reservations
                        .iter()
                        .filter(|reservation| reservation.work_item_id.as_ref() == Some(work_id))
                        .try_fold(0_u64, |sum, reservation| {
                            sum.checked_add(reservation_effective_amount(reservation))
                        })
                        .ok_or_else(|| invalid("work item reservation overflow"))?;
                    Ok::<_, WorkflowError>((current, budget))
                })
                .transpose()?;
            if (*amount_micros == 0 && provider != "local")
                || total > project.cost_ceiling_micros
                || provider_total > provider_ceiling
                || work_total.is_some_and(|(current, budget)| {
                    current
                        .checked_add(*amount_micros)
                        .is_none_or(|next| next > budget)
                })
            {
                return Err(invalid("cost reservation exceeds ceiling"));
            }
            project.reserved_cost_micros = total;
            ensure_collection_capacity(project.reservations.len())?;
            project.reservations.push(CostReservationV1 {
                reservation_id: stable_domain_id(
                    "reservation",
                    &principal.tenant_id,
                    operation_id,
                )?,
                work_item_id: work_item_id.clone(),
                provider: provider.clone(),
                reserved_micros: *amount_micros,
                committed_micros: None,
                state: CostReservationStateV1::Active,
                created_by: principal.principal_id.clone(),
                created_at_unix_ms: now_ms,
                updated_at_unix_ms: now_ms,
            });
            if total == project.cost_ceiling_micros
                && !project.blockers.iter().any(|blocker| {
                    blocker.blocker_kind == BlockerKindV1::BudgetExhausted
                        && blocker.state != BlockerStateV1::Resolved
                })
            {
                ensure_collection_capacity(project.blockers.len())?;
                project.blockers.push(BlockerV1 {
                    blocker_id: stable_domain_id(
                        "budget-blocker",
                        &principal.tenant_id,
                        operation_id,
                    )?,
                    work_item_id: work_item_id.clone(),
                    cause_ref: "budget-exhausted".to_owned(),
                    owner: project.governance.owner,
                    escalation_target: None,
                    state: BlockerStateV1::Open,
                    blocker_kind: BlockerKindV1::BudgetExhausted,
                    blocked_from_state: Some(project.lifecycle_state),
                    resolution_ref: None,
                    last_actor_id: principal.principal_id.clone(),
                    created_at_unix_ms: now_ms,
                    updated_at_unix_ms: now_ms,
                    transition_history: Vec::new(),
                });
                project.lifecycle_state = ProjectLifecycleStateV1::Blocked;
            }
        }
        CompanyWorkflowCommandV1::CommitCost {
            reservation_id,
            actual_micros,
            ..
        } => {
            require_role(
                principal,
                &[CompanyRoleV1::ProjectManager, CompanyRoleV1::TechnicalLead],
            )?;
            let reservation = project
                .reservations
                .iter_mut()
                .find(|r| r.reservation_id == *reservation_id)
                .ok_or_else(not_found)?;
            if reservation.state != CostReservationStateV1::Active
                || *actual_micros > reservation.reserved_micros
            {
                return Err(transition());
            }
            let total = project
                .committed_cost_micros
                .checked_add(*actual_micros)
                .ok_or_else(|| invalid("committed cost overflow"))?;
            if total > project.cost_ceiling_micros {
                return Err(invalid("committed cost exceeds ceiling"));
            }
            reservation.committed_micros = Some(*actual_micros);
            reservation.state = CostReservationStateV1::Committed;
            reservation.updated_at_unix_ms = now_ms;
            project.reserved_cost_micros = project
                .reserved_cost_micros
                .checked_sub(reservation.reserved_micros - *actual_micros)
                .ok_or_else(corrupt)?;
            project.committed_cost_micros = total;
            resolve_budget_blockers_if_headroom(
                &mut project,
                principal,
                actor_id,
                now_ms,
                "cost-reconciled",
            )?;
        }
        CompanyWorkflowCommandV1::ReleaseCost {
            reservation_id,
            reason_ref,
            ..
        } => {
            require_role(
                principal,
                &[CompanyRoleV1::ProjectManager, CompanyRoleV1::TechnicalLead],
            )?;
            validate_identifier(reservation_id)?;
            validate_text(reason_ref)?;
            let reservation = project
                .reservations
                .iter_mut()
                .find(|value| value.reservation_id == *reservation_id)
                .ok_or_else(not_found)?;
            if reservation.state != CostReservationStateV1::Active {
                return Err(transition());
            }
            project.reserved_cost_micros = project
                .reserved_cost_micros
                .checked_sub(reservation.reserved_micros)
                .ok_or_else(corrupt)?;
            reservation.state = CostReservationStateV1::Released;
            reservation.updated_at_unix_ms = now_ms;
            resolve_budget_blockers_if_headroom(
                &mut project,
                principal,
                actor_id,
                now_ms,
                reason_ref,
            )?;
        }
        CompanyWorkflowCommandV1::CreateProjectRoom { kind, members, .. } => {
            require_role(principal, &[CompanyRoleV1::ProjectManager])?;
            let unique = members
                .iter()
                .map(|member| member.0)
                .collect::<BTreeSet<_>>();
            if members.is_empty()
                || unique.len() != members.len()
                || !members.windows(2).all(|pair| pair[0].0 < pair[1].0)
                || members.iter().any(|member| {
                    !project
                        .governance
                        .participants
                        .iter()
                        .any(|p| p.agent_id == *member)
                })
            {
                return Err(unauthorized());
            }
            ensure_collection_capacity(project.rooms.len())?;
            project.rooms.push(ProjectRoomV1 {
                room_id: stable_domain_id("room", &principal.tenant_id, operation_id)?,
                kind: *kind,
                members: members.clone(),
                created_at_unix_ms: now_ms,
            });
        }
        CompanyWorkflowCommandV1::RecordQuestion {
            work_item_id,
            owner,
            question_ref,
            ..
        } => {
            validate_text(question_ref)?;
            validate_optional_work(&project, work_item_id.as_ref())?;
            if !project
                .governance
                .participants
                .iter()
                .any(|p| p.agent_id == *owner)
                || (*owner != actor_id
                    && !matches!(
                        principal.role,
                        CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
                    ))
            {
                return Err(unauthorized());
            }
            ensure_collection_capacity(project.questions.len())?;
            project.questions.push(ProjectQuestionV1 {
                question_id: stable_domain_id("question", &principal.tenant_id, operation_id)?,
                work_item_id: work_item_id.clone(),
                owner: *owner,
                question_ref: question_ref.clone(),
                resolution_ref: None,
                created_by: principal.principal_id.clone(),
                resolved_by: None,
                created_at_unix_ms: now_ms,
                updated_at_unix_ms: now_ms,
            });
        }
        CompanyWorkflowCommandV1::ResolveQuestion {
            question_id,
            resolution_ref,
            ..
        } => {
            validate_identifier(question_id)?;
            validate_text(resolution_ref)?;
            let actor = principal.agent_id.ok_or_else(unauthorized)?;
            let question = project
                .questions
                .iter_mut()
                .find(|value| value.question_id == *question_id)
                .ok_or_else(not_found)?;
            if question.owner != actor || question.resolution_ref.is_some() {
                return Err(unauthorized());
            }
            question.resolution_ref = Some(resolution_ref.clone());
            question.resolved_by = Some(principal.principal_id.clone());
            question.updated_at_unix_ms = now_ms;
        }
        CompanyWorkflowCommandV1::RecordAction {
            work_item_id,
            owner,
            action_ref,
            ..
        } => {
            validate_text(action_ref)?;
            validate_optional_work(&project, work_item_id.as_ref())?;
            if !project
                .governance
                .participants
                .iter()
                .any(|p| p.agent_id == *owner)
                || (*owner != actor_id
                    && !matches!(
                        principal.role,
                        CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
                    ))
            {
                return Err(unauthorized());
            }
            ensure_collection_capacity(project.actions.len())?;
            project.actions.push(ProjectActionV1 {
                action_id: stable_domain_id("action", &principal.tenant_id, operation_id)?,
                work_item_id: work_item_id.clone(),
                owner: *owner,
                action_ref: action_ref.clone(),
                completed: false,
                created_by: principal.principal_id.clone(),
                completed_by: None,
                resolution_ref: None,
                created_at_unix_ms: now_ms,
                updated_at_unix_ms: now_ms,
            });
        }
        CompanyWorkflowCommandV1::ResolveAction {
            action_id,
            resolution_ref,
            ..
        } => {
            validate_identifier(action_id)?;
            validate_text(resolution_ref)?;
            let actor = principal.agent_id.ok_or_else(unauthorized)?;
            let action = project
                .actions
                .iter_mut()
                .find(|value| value.action_id == *action_id)
                .ok_or_else(not_found)?;
            if action.owner != actor || action.completed {
                return Err(unauthorized());
            }
            action.completed = true;
            action.completed_by = Some(principal.principal_id.clone());
            action.resolution_ref = Some(resolution_ref.clone());
            action.updated_at_unix_ms = now_ms;
        }
        _ => return Err(invalid("command is not a project mutation")),
    }
    project.version = project
        .version
        .checked_add(1)
        .ok_or_else(|| invalid("project version overflow"))?;
    project.updated_at_unix_ms = now_ms;
    validate_project(&project)?;
    put_entity(
        transaction,
        &principal.tenant_id,
        "project",
        &project.project_id.0,
        project.version,
        &project,
    )?;
    append_project_snapshot(
        transaction,
        principal,
        operation_id,
        operation_digest,
        project_event_type(command)?,
        &project,
        now_ms,
    )?;
    Ok(CompanyWorkflowResponseV1::Project(project))
}

fn project_target(command: &CompanyWorkflowCommandV1) -> Option<(&ProjectId, u64)> {
    match command {
        CompanyWorkflowCommandV1::PlanWorkGraph {
            project_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::CreateGovernedRework {
            project_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::ActivateProject {
            project_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::AssignWork {
            project_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::ReassignWork {
            project_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::DelegateWork {
            project_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::ApplyWorkTransition {
            project_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::RecordDecision {
            project_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::CreateHandoff {
            project_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::AcknowledgeHandoff {
            project_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::RaiseBlocker {
            project_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::EscalateBlocker {
            project_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::ResolveBlocker {
            project_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::RecordApproval {
            project_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::ReserveCost {
            project_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::CommitCost {
            project_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::ReleaseCost {
            project_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::CreateProjectRoom {
            project_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::RecordQuestion {
            project_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::ResolveQuestion {
            project_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::RecordAction {
            project_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::ResolveAction {
            project_id,
            expected_version,
            ..
        } => Some((project_id, *expected_version)),
        _ => None,
    }
}

fn project_event_type(command: &CompanyWorkflowCommandV1) -> Result<&'static str, WorkflowError> {
    match command {
        CompanyWorkflowCommandV1::PlanWorkGraph { .. } => Ok("project_work_graph_planned"),
        CompanyWorkflowCommandV1::ActivateProject { .. } => Ok("project_activated"),
        CompanyWorkflowCommandV1::AssignWork { .. } => Ok("project_work_assigned"),
        CompanyWorkflowCommandV1::ReassignWork { .. } => Ok("project_work_reassigned"),
        CompanyWorkflowCommandV1::DelegateWork { .. } => Ok("project_work_delegated"),
        CompanyWorkflowCommandV1::ApplyWorkTransition { .. } => {
            Ok("project_work_transition_applied")
        }
        CompanyWorkflowCommandV1::RecordDecision { .. } => Ok("project_decision_recorded"),
        CompanyWorkflowCommandV1::CreateHandoff { .. } => Ok("project_handoff_created"),
        CompanyWorkflowCommandV1::AcknowledgeHandoff { .. } => Ok("project_handoff_acknowledged"),
        CompanyWorkflowCommandV1::RaiseBlocker { .. } => Ok("project_blocker_raised"),
        CompanyWorkflowCommandV1::EscalateBlocker { .. } => Ok("project_blocker_escalated"),
        CompanyWorkflowCommandV1::ResolveBlocker { .. } => Ok("project_blocker_resolved"),
        CompanyWorkflowCommandV1::RecordApproval { .. } => Ok("project_approval_recorded"),
        CompanyWorkflowCommandV1::ReserveCost { .. } => Ok("project_cost_reserved"),
        CompanyWorkflowCommandV1::CommitCost { .. } => Ok("project_cost_committed"),
        CompanyWorkflowCommandV1::ReleaseCost { .. } => Ok("project_cost_released"),
        CompanyWorkflowCommandV1::CreateProjectRoom { .. } => Ok("project_room_created"),
        CompanyWorkflowCommandV1::RecordQuestion { .. } => Ok("project_question_recorded"),
        CompanyWorkflowCommandV1::ResolveQuestion { .. } => Ok("project_question_resolved"),
        CompanyWorkflowCommandV1::RecordAction { .. } => Ok("project_action_recorded"),
        CompanyWorkflowCommandV1::ResolveAction { .. } => Ok("project_action_resolved"),
        _ => Err(invalid("command is not a project event")),
    }
}

fn command_predecessor_digest(command: &CompanyWorkflowCommandV1) -> Result<String, WorkflowError> {
    if let Some((project_id, expected_version)) = project_target(command) {
        return canonical_sha256(
            "sentinel.workflow.company-command-predecessor.v1",
            &("project", project_id, expected_version),
        );
    }
    let request = match command {
        CompanyWorkflowCommandV1::SubmitCustomerRequest { .. } => {
            return canonical_sha256(
                "sentinel.workflow.company-command-predecessor.v1",
                &("new-request", 0_u64),
            );
        }
        CompanyWorkflowCommandV1::ClarifyCustomerRequest {
            request_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::QualifyCustomerRequest {
            request_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::CreateProposal {
            request_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::AcceptProposal {
            request_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::RejectProposal {
            request_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::CancelCustomerRequest {
            request_id,
            expected_version,
            ..
        }
        | CompanyWorkflowCommandV1::RecordCustomerFeedback {
            request_id,
            expected_version,
            ..
        } => (request_id.as_str(), *expected_version),
        _ => return Err(invalid("company command predecessor is undefined")),
    };
    canonical_sha256(
        "sentinel.workflow.company-command-predecessor.v1",
        &("request", request.0, request.1),
    )
}

fn required_request(
    transaction: &Transaction<'_>,
    principal: &AuthenticatedCompanyPrincipalV1,
    request_id: &str,
    now_ms: u64,
) -> Result<CustomerRequestV1, WorkflowError> {
    validate_identifier(request_id)?;
    let request: CustomerRequestV1 =
        get_entity(transaction, &principal.tenant_id, "request", request_id)?
            .ok_or_else(not_found)?;
    if principal.kind == CompanyPrincipalKindV1::Customer
        && principal.customer_id.as_deref() != Some(&request.customer_id)
    {
        return Err(unauthorized());
    }
    require_non_regressing_time(now_ms, request.updated_at_unix_ms)?;
    Ok(request)
}

fn require_non_regressing_time(now_ms: u64, previous_ms: u64) -> Result<(), WorkflowError> {
    if now_ms < previous_ms {
        Err(invalid("company command time predates durable state"))
    } else {
        Ok(())
    }
}

fn bump_request(
    transaction: &Transaction<'_>,
    principal: &AuthenticatedCompanyPrincipalV1,
    operation_id: Uuid,
    operation_digest: &str,
    request: &mut CustomerRequestV1,
    event_type: &str,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    request.version = request
        .version
        .checked_add(1)
        .ok_or_else(|| invalid("request version overflow"))?;
    request.updated_at_unix_ms = now_ms;
    put_entity(
        transaction,
        &request.tenant_id,
        "request",
        &request.request_id,
        request.version,
        request,
    )?;
    append_event(
        transaction,
        principal,
        operation_id,
        operation_digest,
        None,
        event_type,
        request,
        now_ms,
    )?;
    Ok(())
}

fn validate_replay_response(
    transaction: &Transaction<'_>,
    principal: &AuthenticatedCompanyPrincipalV1,
    command: &CompanyWorkflowCommandV1,
    response: &CompanyWorkflowResponseV1,
) -> Result<(), WorkflowError> {
    match response {
        CompanyWorkflowResponseV1::CustomerRequest(value) => {
            validate_customer_request(value)?;
            let stored: CustomerRequestV1 = get_entity(
                transaction,
                &principal.tenant_id,
                "request",
                &value.request_id,
            )?
            .ok_or_else(corrupt)?;
            if stored.schema_version != value.schema_version
                || stored.tenant_id != value.tenant_id
                || stored.request_id != value.request_id
                || stored.customer_id != value.customer_id
                || stored.summary_ref != value.summary_ref
                || stored.desired_outcome != value.desired_outcome
                || stored.constraints != value.constraints
                || stored.created_at_unix_ms != value.created_at_unix_ms
                || stored.version < value.version
                || !stored.clarifications.starts_with(&value.clarifications)
                || !stored.feedback.starts_with(&value.feedback)
                || !stored.proposal_ids.starts_with(&value.proposal_ids)
            {
                return Err(corrupt());
            }
        }
        CompanyWorkflowResponseV1::Proposal(value) => {
            validate_proposal(value)?;
            let stored: ProposalV1 = get_entity(
                transaction,
                &principal.tenant_id,
                "proposal",
                &value.proposal_id,
            )?
            .ok_or_else(corrupt)?;
            if stored != *value {
                return Err(corrupt());
            }
        }
        CompanyWorkflowResponseV1::AgreementProject { agreement, project } => {
            validate_agreement(agreement)?;
            validate_project(project)?;
            let stored_agreement: AgreementV1 = get_entity(
                transaction,
                &principal.tenant_id,
                "agreement",
                &agreement.agreement_id,
            )?
            .ok_or_else(corrupt)?;
            let stored_project: ProjectV1 = get_entity(
                transaction,
                &principal.tenant_id,
                "project",
                &project.project_id.0,
            )?
            .ok_or_else(corrupt)?;
            if stored_agreement != **agreement
                || stored_project.agreement_id != project.agreement_id
                || stored_project.tenant_id != project.tenant_id
                || stored_project.project_id != project.project_id
                || stored_project.version < project.version
            {
                return Err(corrupt());
            }
        }
        CompanyWorkflowResponseV1::Project(value) => {
            validate_project(value)?;
            let stored: ProjectV1 = get_entity(
                transaction,
                &principal.tenant_id,
                "project",
                &value.project_id.0,
            )?
            .ok_or_else(corrupt)?;
            if stored.tenant_id != value.tenant_id
                || stored.project_id != value.project_id
                || stored.agreement_id != value.agreement_id
                || stored.agreement_digest != value.agreement_digest
                || stored.created_at_unix_ms != value.created_at_unix_ms
                || stored.version < value.version
            {
                return Err(corrupt());
            }
        }
    }
    if command.canonical_digest().is_err() {
        return Err(corrupt());
    }
    Ok(())
}

fn append_project_snapshot(
    transaction: &Transaction<'_>,
    principal: &AuthenticatedCompanyPrincipalV1,
    operation_id: Uuid,
    operation_digest: &str,
    event_type: &str,
    project: &ProjectV1,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    let sequence = append_event(
        transaction,
        principal,
        operation_id,
        operation_digest,
        Some(&project.project_id),
        event_type,
        project,
        now_ms,
    )?;
    put_projection(
        transaction,
        &project.tenant_id,
        &project.project_id,
        sequence,
        project,
    )
}

fn append_event<T: Serialize>(
    transaction: &Transaction<'_>,
    principal: &AuthenticatedCompanyPrincipalV1,
    operation_id: Uuid,
    operation_digest: &str,
    project_id: Option<&ProjectId>,
    event_type: &str,
    payload: &T,
    now_ms: u64,
) -> Result<u64, WorkflowError> {
    let payload = encode(payload)?;
    let payload_digest = bytes_digest("sentinel.workflow.company-event-payload.v1", &payload)?;
    let authority_binding_digest = principal.binding_digest()?;
    let event_id = canonical_sha256(
        "sentinel.workflow.company-event-id.v1",
        &(
            &principal.tenant_id,
            project_id,
            event_type,
            operation_id,
            operation_digest,
            &authority_binding_digest,
            &payload_digest,
            now_ms,
        ),
    )?;
    transaction.execute(
        "INSERT INTO company_events(event_id,tenant_id,project_id,event_type,operation_id,operation_digest,principal_id,principal_kind,principal_role,agent_id,customer_id,authority_generation,authority_digest,authority_binding_digest,payload,payload_digest,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
        params![event_id, principal.tenant_id.0, project_id.map(|value| value.0.as_str()), event_type, operation_id.to_string(), operation_digest, principal.principal_id, enum_name(principal.kind), enum_name(principal.role), principal.agent_id.map(|value| i64::from(value.0)), principal.customer_id, sql_u64(principal.authority_generation)?, principal.authority_digest, authority_binding_digest, payload, payload_digest, sql_u64(now_ms)?],
    ).map_err(WorkflowError::from)?;
    stored_u64(transaction.last_insert_rowid())
}

fn put_projection(
    transaction: &Transaction<'_>,
    tenant_id: &TenantId,
    project_id: &ProjectId,
    source_sequence: u64,
    project: &ProjectV1,
) -> Result<(), WorkflowError> {
    let mut projection = ProjectProjectionV1 {
        schema_version: COMPANY_DOMAIN_SCHEMA_VERSION,
        tenant_id: tenant_id.clone(),
        project_id: project_id.clone(),
        source_sequence,
        project: project.clone(),
        projection_digest: String::new(),
    };
    projection.projection_digest =
        canonical_sha256("sentinel.workflow.project-projection.v1", &projection)?;
    let payload = encode(&projection)?;
    let payload_digest = bytes_digest("sentinel.workflow.company-projection-row.v1", &payload)?;
    transaction.execute(
        "INSERT INTO company_project_projections(tenant_id,project_id,source_sequence,payload,payload_digest) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(tenant_id,project_id) DO UPDATE SET source_sequence=excluded.source_sequence,payload=excluded.payload,payload_digest=excluded.payload_digest",
        params![tenant_id.0, project_id.0, sql_u64(source_sequence)?, payload, payload_digest],
    ).map_err(WorkflowError::from)?;
    Ok(())
}

fn validate_customer_request(request: &CustomerRequestV1) -> Result<(), WorkflowError> {
    if request.schema_version != COMPANY_DOMAIN_SCHEMA_VERSION
        || request.version == 0
        || request.created_at_unix_ms == 0
        || request.created_at_unix_ms > request.updated_at_unix_ms
        || request.clarifications.len() > MAX_AGGREGATE_ITEMS
        || request.feedback.len() > MAX_AGGREGATE_ITEMS
        || request.proposal_ids.len() > MAX_AGGREGATE_ITEMS
    {
        return Err(corrupt());
    }
    request.tenant_id.validate().map_err(|_| corrupt())?;
    validate_identifier(&request.request_id).map_err(|_| corrupt())?;
    validate_identifier(&request.customer_id).map_err(|_| corrupt())?;
    validate_text(&request.summary_ref).map_err(|_| corrupt())?;
    validate_text(&request.desired_outcome).map_err(|_| corrupt())?;
    validate_text_collection(&request.constraints, false).map_err(|_| corrupt())?;
    let mut proposal_ids = BTreeSet::new();
    for id in &request.proposal_ids {
        validate_identifier(id).map_err(|_| corrupt())?;
        if !proposal_ids.insert(id) {
            return Err(corrupt());
        }
    }
    for clarification in &request.clarifications {
        validate_text(&clarification.question_ref).map_err(|_| corrupt())?;
        validate_text(&clarification.answer_ref).map_err(|_| corrupt())?;
        validate_identifier(&clarification.recorded_by).map_err(|_| corrupt())?;
        if clarification.recorded_at_unix_ms < request.created_at_unix_ms
            || clarification.recorded_at_unix_ms > request.updated_at_unix_ms
        {
            return Err(corrupt());
        }
    }
    for feedback in &request.feedback {
        validate_text(&feedback.feedback_ref).map_err(|_| corrupt())?;
        validate_identifier(&feedback.recorded_by).map_err(|_| corrupt())?;
        if feedback.recorded_at_unix_ms < request.created_at_unix_ms
            || feedback.recorded_at_unix_ms > request.updated_at_unix_ms
        {
            return Err(corrupt());
        }
    }
    if matches!(
        request.state,
        CustomerRequestStateV1::Proposed | CustomerRequestStateV1::Accepted
    ) && request.proposal_ids.is_empty()
    {
        return Err(corrupt());
    }
    Ok(())
}

fn validate_proposal(proposal: &ProposalV1) -> Result<(), WorkflowError> {
    if proposal.schema_version != COMPANY_DOMAIN_SCHEMA_VERSION
        || proposal.generation == 0
        || proposal.created_at_unix_ms == 0
    {
        return Err(corrupt());
    }
    proposal.tenant_id.validate().map_err(|_| corrupt())?;
    validate_identifier(&proposal.proposal_id).map_err(|_| corrupt())?;
    validate_identifier(&proposal.request_id).map_err(|_| corrupt())?;
    validate_identifier(&proposal.created_by).map_err(|_| corrupt())?;
    proposal
        .binding
        .validate(proposal.created_at_unix_ms)
        .map_err(|_| corrupt())?;
    let digest = canonical_sha256("sentinel.workflow.proposal-binding.v1", &proposal.binding)?;
    if !constant_time_eq(&digest, &proposal.proposal_digest) {
        return Err(corrupt());
    }
    Ok(())
}

fn validate_agreement(agreement: &AgreementV1) -> Result<(), WorkflowError> {
    if agreement.schema_version != COMPANY_DOMAIN_SCHEMA_VERSION
        || agreement.accepted_at_unix_ms == 0
    {
        return Err(corrupt());
    }
    agreement.tenant_id.validate().map_err(|_| corrupt())?;
    for value in [
        &agreement.agreement_id,
        &agreement.request_id,
        &agreement.proposal_id,
        &agreement.customer_id,
        &agreement.accepted_by,
    ] {
        validate_identifier(value).map_err(|_| corrupt())?;
    }
    validate_digest(&agreement.proposal_digest).map_err(|_| corrupt())
}

fn validate_project(project: &ProjectV1) -> Result<(), WorkflowError> {
    if project.schema_version != COMPANY_DOMAIN_SCHEMA_VERSION
        || project.version == 0
        || project.created_at_unix_ms == 0
        || project.created_at_unix_ms > project.updated_at_unix_ms
        || project.cost_ceiling_micros == 0
        || project.provider_cost_ceilings_micros.is_empty()
        || project.reserved_cost_micros > project.cost_ceiling_micros
        || project.committed_cost_micros > project.cost_ceiling_micros
        || project.work_items.len() > MAX_AGGREGATE_ITEMS
        || project.decisions.len() > MAX_AGGREGATE_ITEMS
        || project.handoffs.len() > MAX_AGGREGATE_ITEMS
        || project.blockers.len() > MAX_AGGREGATE_ITEMS
        || project.approvals.len() > MAX_AGGREGATE_ITEMS
        || project.reservations.len() > MAX_AGGREGATE_ITEMS
        || project.rooms.len() > MAX_AGGREGATE_ITEMS
        || project.questions.len() > MAX_AGGREGATE_ITEMS
        || project.actions.len() > MAX_AGGREGATE_ITEMS
    {
        return Err(corrupt());
    }
    project.tenant_id.validate().map_err(|_| corrupt())?;
    project.project_id.validate().map_err(|_| corrupt())?;
    validate_identifier(&project.agreement_id).map_err(|_| corrupt())?;
    validate_digest(&project.agreement_digest).map_err(|_| corrupt())?;
    project.governance.validate().map_err(|_| corrupt())?;
    if project
        .provider_cost_ceilings_micros
        .iter()
        .any(|(provider, ceiling)| {
            validate_identifier(provider).is_err()
                || *ceiling == 0
                || *ceiling > project.cost_ceiling_micros
        })
    {
        return Err(corrupt());
    }
    validate_work_graph_if_present(&project.work_items, project.updated_at_unix_ms)?;
    validate_project_collections(project)?;
    Ok(())
}

fn validate_projection(
    projection: &ProjectProjectionV1,
    tenant_id: &TenantId,
    project_id: &ProjectId,
) -> Result<(), WorkflowError> {
    let mut canonical = projection.clone();
    let supplied = canonical.projection_digest.clone();
    canonical.projection_digest.clear();
    let digest = canonical_sha256("sentinel.workflow.project-projection.v1", &canonical)?;
    if projection.schema_version != COMPANY_DOMAIN_SCHEMA_VERSION
        || projection.tenant_id != *tenant_id
        || projection.project_id != *project_id
        || projection.project.tenant_id != *tenant_id
        || projection.project.project_id != *project_id
        || projection.source_sequence == 0
        || !constant_time_eq(&digest, &supplied)
    {
        return Err(corrupt());
    }
    validate_project(&projection.project)
}

fn validate_work_graph_if_present(
    items: &BTreeMap<crate::WorkItemId, CompanyWorkItemV1>,
    project_updated_at_unix_ms: u64,
) -> Result<(), WorkflowError> {
    if items.is_empty() {
        return Ok(());
    }
    let specs = items
        .values()
        .map(|value| value.spec.clone())
        .collect::<Vec<_>>();
    validate_work_graph(&specs)?;
    for item in items.values() {
        if let Some(binding) = &item.spec.rework {
            let source = items
                .get(&binding.source_work_item_id)
                .ok_or_else(corrupt)?;
            let expected_generation = source.spec.rework.as_ref().map_or(1, |source_binding| {
                source_binding.generation.saturating_add(1)
            });
            if source.state != CompanyWorkStateV1::Done || binding.generation != expected_generation
            {
                return Err(corrupt());
            }
        }
    }
    for (id, item) in items {
        if id != &item.spec.work_item_id
            || item.version == 0
            || item.assignments.len() > MAX_AGGREGATE_ITEMS
            || item.output_receipts.len() > MAX_AGGREGATE_ITEMS
            || item.transition_history.len() > MAX_AGGREGATE_ITEMS
        {
            return Err(corrupt());
        }
        let participant = |agent_id| {
            // Participant binding is checked by validate_project_collections.
            agent_id
        };
        let mut assignment_ids = BTreeSet::new();
        let mut expected_assignment_version = 1_u64;
        let mut active = 0_usize;
        for assignment in &item.assignments {
            validate_identifier(&assignment.assignment_id).map_err(|_| corrupt())?;
            validate_identifier(&assignment.assigned_by).map_err(|_| corrupt())?;
            validate_text(&assignment.reason_ref).map_err(|_| corrupt())?;
            validate_digest(&assignment.organization_digest).map_err(|_| corrupt())?;
            assignment.profile.validate().map_err(|_| corrupt())?;
            if assignment.agent_id.0 == 0
                || assignment.organization_generation == 0
                || assignment.assignment_version != expected_assignment_version
                || assignment.created_at_unix_ms == 0
                || !assignment_ids.insert(&assignment.assignment_id)
                || (assignment.active && assignment.ended_at_unix_ms.is_some())
                || (!assignment.active
                    && assignment
                        .ended_at_unix_ms
                        .is_none_or(|ended| ended < assignment.created_at_unix_ms))
                || assignment.created_at_unix_ms > project_updated_at_unix_ms
                || assignment
                    .ended_at_unix_ms
                    .is_some_and(|ended| ended > project_updated_at_unix_ms)
            {
                return Err(corrupt());
            }
            let _ = participant(assignment.agent_id);
            active += usize::from(assignment.active);
            expected_assignment_version = expected_assignment_version
                .checked_add(1)
                .ok_or_else(corrupt)?;
        }
        let needs_assignment = matches!(
            item.state,
            CompanyWorkStateV1::Assigned
                | CompanyWorkStateV1::InProgress
                | CompanyWorkStateV1::InReview
                | CompanyWorkStateV1::Done
                | CompanyWorkStateV1::Blocked
        );
        if active > 1 || (needs_assignment && active != 1) || (!needs_assignment && active != 0) {
            return Err(corrupt());
        }
        if matches!(
            item.state,
            CompanyWorkStateV1::InReview | CompanyWorkStateV1::Done
        ) {
            validate_output_receipts(&item.spec, &item.output_receipts).map_err(|_| corrupt())?;
        } else if !item.output_receipts.is_empty() {
            return Err(corrupt());
        }
        if item.state == CompanyWorkStateV1::Done {
            let gate = item.gate_receipt.as_ref().ok_or_else(corrupt)?;
            validate_digest(&gate.gate_digest).map_err(|_| corrupt())?;
            validate_digest(&gate.subject_digest).map_err(|_| corrupt())?;
            if !gate.passed
                || gate.gate_id != item.spec.quality_gate.gate_id
                || gate.generation != item.spec.quality_gate.generation
                || !constant_time_eq(&gate.gate_digest, &item.spec.quality_gate.digest)
            {
                return Err(corrupt());
            }
        } else if item.gate_receipt.is_some() {
            return Err(corrupt());
        }
        validate_work_transition_history(item, project_updated_at_unix_ms)?;
    }
    Ok(())
}

fn validate_project_collections(project: &ProjectV1) -> Result<(), WorkflowError> {
    let participant = |agent_id: crate::AgentId| {
        project
            .governance
            .participants
            .iter()
            .find(|value| value.agent_id == agent_id)
            .ok_or_else(corrupt)
    };
    let principal_participant = |principal_id: &str| {
        project
            .governance
            .participants
            .iter()
            .find(|value| value.principal_id == principal_id)
            .ok_or_else(corrupt)
    };
    for work in project.work_items.values() {
        participant(work.spec.owner)?;
        for (audit_index, audit) in work.transition_history.iter().enumerate() {
            let actor = participant(audit.actor_agent_id)?;
            if actor.principal_id != audit.actor_id
                || !work_transition_actor_is_authorized(project, work, audit_index, audit, actor)
            {
                return Err(corrupt());
            }
        }
        let assignment_edges = work
            .transition_history
            .iter()
            .filter(|audit| {
                matches!(
                    (audit.before.as_str(), audit.after.as_str()),
                    ("Ready", "Assigned") | ("Assigned", "Assigned") | ("Blocked", "Assigned")
                )
            })
            .count();
        if assignment_edges != work.assignments.len() {
            return Err(corrupt());
        }
        for (assignment_index, assignment) in work.assignments.iter().enumerate() {
            let bound = participant(assignment.agent_id)?;
            let assigner = project
                .governance
                .participants
                .iter()
                .find(|value| value.principal_id == assignment.assigned_by)
                .ok_or_else(corrupt)?;
            if bound.role != assignment.role
                || bound.specialties != assignment.specialties
                || bound.profile != assignment.profile
                || assignment.role != work.spec.required_role
                || !work
                    .spec
                    .required_specialties
                    .is_subset(&assignment.specialties)
            {
                return Err(corrupt());
            }
            if assignment_index > 0 {
                let previous = &work.assignments[assignment_index - 1];
                if previous.ended_at_unix_ms != Some(assignment.created_at_unix_ms) {
                    return Err(corrupt());
                }
            }
            if let Some(delegator) = assignment.delegated_by {
                let delegator = participant(delegator)?;
                if assigner.agent_id != delegator.agent_id
                    || !is_direct_report(
                        &project.governance,
                        assignment.agent_id,
                        delegator.agent_id,
                    )
                {
                    return Err(corrupt());
                }
            } else if !matches!(
                assigner.role,
                CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
            ) {
                return Err(corrupt());
            }
        }
        let dependencies_ready = dependencies_satisfied(project, work);
        if (work.state == CompanyWorkStateV1::DependencyPending && dependencies_ready)
            || (work.state != CompanyWorkStateV1::DependencyPending && !dependencies_ready)
        {
            return Err(corrupt());
        }
    }
    let mut ids = BTreeSet::new();
    for decision in &project.decisions {
        validate_identifier(&decision.decision_id).map_err(|_| corrupt())?;
        validate_optional_work(project, decision.work_item_id.as_ref()).map_err(|_| corrupt())?;
        validate_text(&decision.choice_ref).map_err(|_| corrupt())?;
        validate_text(&decision.rationale_ref).map_err(|_| corrupt())?;
        validate_identifier(&decision.decided_by).map_err(|_| corrupt())?;
        let decider = principal_participant(&decision.decided_by)?;
        if !matches!(
            decider.role,
            CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
        ) || decision.created_at_unix_ms < project.created_at_unix_ms
            || decision.created_at_unix_ms > project.updated_at_unix_ms
            || !ids.insert(("decision", decision.decision_id.as_str()))
        {
            return Err(corrupt());
        }
    }
    for handoff in &project.handoffs {
        validate_identifier(&handoff.handoff_id).map_err(|_| corrupt())?;
        participant(handoff.producer)?;
        participant(handoff.consumer)?;
        validate_text(&handoff.reason_ref).map_err(|_| corrupt())?;
        if handoff.producer == handoff.consumer
            || handoff.artifact_digests.is_empty()
            || !project.work_items.contains_key(&handoff.work_item_id)
            || !ids.insert(("handoff", handoff.handoff_id.as_str()))
            || handoff
                .artifact_digests
                .iter()
                .any(|digest| validate_digest(digest).is_err())
            || (handoff.state == HandoffStateV1::Offered
                && (handoff.acknowledged_by.is_some()
                    || handoff.acknowledged_at_unix_ms.is_some()
                    || handoff.acknowledgement_reason_ref.is_some()))
            || (handoff.state != HandoffStateV1::Offered
                && (handoff.acknowledged_by.is_none()
                    || handoff.acknowledged_at_unix_ms.is_none()
                    || handoff.acknowledgement_reason_ref.is_none()))
            || handoff.transition_history.len() > MAX_AGGREGATE_ITEMS
            || handoff.created_at_unix_ms < project.created_at_unix_ms
            || handoff.created_at_unix_ms > project.updated_at_unix_ms
            || handoff.acknowledged_at_unix_ms.is_some_and(|value| {
                value < handoff.created_at_unix_ms || value > project.updated_at_unix_ms
            })
        {
            return Err(corrupt());
        }
        validate_handoff_transition_history(handoff, project.updated_at_unix_ms)?;
        for audit in &handoff.transition_history {
            let actor = participant(audit.actor_agent_id)?;
            if actor.principal_id != audit.actor_id
                || audit.actor_agent_id != handoff.consumer
                || handoff.acknowledged_by.as_deref() != Some(audit.actor_id.as_str())
            {
                return Err(corrupt());
            }
        }
    }
    for blocker in &project.blockers {
        validate_identifier(&blocker.blocker_id).map_err(|_| corrupt())?;
        validate_optional_work(project, blocker.work_item_id.as_ref()).map_err(|_| corrupt())?;
        validate_text(&blocker.cause_ref).map_err(|_| corrupt())?;
        validate_identifier(&blocker.last_actor_id).map_err(|_| corrupt())?;
        participant(blocker.owner)?;
        let last_actor = principal_participant(&blocker.last_actor_id)?;
        if let Some(target) = blocker.escalation_target {
            participant(target)?;
            if blocker.state == BlockerStateV1::Open {
                return Err(corrupt());
            }
        }
        if blocker.state == BlockerStateV1::Resolved {
            if blocker
                .resolution_ref
                .as_deref()
                .is_none_or(|value| validate_text(value).is_err())
            {
                return Err(corrupt());
            }
        } else if blocker.resolution_ref.is_some() {
            return Err(corrupt());
        }
        if blocker.created_at_unix_ms == 0
            || blocker.updated_at_unix_ms < blocker.created_at_unix_ms
            || blocker.updated_at_unix_ms > project.updated_at_unix_ms
            || blocker.transition_history.len() > MAX_AGGREGATE_ITEMS
            || !ids.insert(("blocker", blocker.blocker_id.as_str()))
        {
            return Err(corrupt());
        }
        validate_blocker_transition_history(blocker, project.updated_at_unix_ms)?;
        if blocker.transition_history.is_empty()
            && last_actor.agent_id != blocker.owner
            && !matches!(
                last_actor.role,
                CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
            )
        {
            return Err(corrupt());
        }
        for audit in &blocker.transition_history {
            let actor = participant(audit.actor_agent_id)?;
            if actor.principal_id != audit.actor_id {
                return Err(corrupt());
            }
            let authorized = match (audit.before.as_str(), audit.after.as_str()) {
                ("Open", "Escalated") => matches!(
                    actor.role,
                    CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
                ),
                ("Open", "Resolved") | ("Escalated", "Resolved") => {
                    audit.actor_agent_id == blocker.owner
                        || blocker.escalation_target == Some(audit.actor_agent_id)
                        || blocker.blocker_kind == BlockerKindV1::BudgetExhausted
                            && matches!(
                                actor.role,
                                CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
                            )
                }
                _ => false,
            };
            if !authorized {
                return Err(corrupt());
            }
        }
    }
    for approval in &project.approvals {
        validate_identifier(&approval.approval_id).map_err(|_| corrupt())?;
        validate_identifier(&approval.actor_id).map_err(|_| corrupt())?;
        validate_digest(&approval.subject_digest).map_err(|_| corrupt())?;
        let work = project
            .work_items
            .get(&approval.work_item_id)
            .ok_or_else(corrupt)?;
        let actor = participant(approval.actor_agent_id)?;
        if !matches!(
            actor.role,
            CompanyRoleV1::Qa | CompanyRoleV1::ReleaseManager
        ) || actor.principal_id != approval.actor_id
            || assignment_active_at(work, approval.actor_agent_id, approval.created_at_unix_ms)
            || !ids.insert(("approval", approval.approval_id.as_str()))
            || approval.created_at_unix_ms < project.created_at_unix_ms
            || approval.created_at_unix_ms > project.updated_at_unix_ms
        {
            return Err(corrupt());
        }
    }
    let mut reserved = 0_u64;
    let mut committed = 0_u64;
    let mut provider_totals = BTreeMap::<&str, u64>::new();
    let mut work_totals = BTreeMap::<&crate::WorkItemId, u64>::new();
    for reservation in &project.reservations {
        validate_identifier(&reservation.reservation_id).map_err(|_| corrupt())?;
        validate_identifier(&reservation.provider).map_err(|_| corrupt())?;
        validate_identifier(&reservation.created_by).map_err(|_| corrupt())?;
        let creator = principal_participant(&reservation.created_by)?;
        validate_optional_work(project, reservation.work_item_id.as_ref())
            .map_err(|_| corrupt())?;
        if !matches!(
            creator.role,
            CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
        ) || reservation.created_at_unix_ms == 0
            || reservation.updated_at_unix_ms < reservation.created_at_unix_ms
            || reservation.updated_at_unix_ms > project.updated_at_unix_ms
            || !ids.insert(("reservation", reservation.reservation_id.as_str()))
            || (reservation.reserved_micros == 0 && reservation.provider != "local")
            || (reservation.state == CostReservationStateV1::Active
                && reservation.committed_micros.is_some())
            || (reservation.state == CostReservationStateV1::Committed
                && reservation
                    .committed_micros
                    .is_none_or(|value| value > reservation.reserved_micros))
            || (reservation.state == CostReservationStateV1::Released
                && reservation.committed_micros.is_some())
        {
            return Err(corrupt());
        }
        let effective = reservation_effective_amount(reservation);
        reserved = reserved.checked_add(effective).ok_or_else(corrupt)?;
        committed = committed
            .checked_add(reservation.committed_micros.unwrap_or(0))
            .ok_or_else(corrupt)?;
        let provider_total = provider_totals.entry(&reservation.provider).or_default();
        *provider_total = provider_total.checked_add(effective).ok_or_else(corrupt)?;
        if let Some(work_id) = reservation.work_item_id.as_ref() {
            let work_total = work_totals.entry(work_id).or_default();
            *work_total = work_total.checked_add(effective).ok_or_else(corrupt)?;
        }
    }
    if reserved != project.reserved_cost_micros || committed != project.committed_cost_micros {
        return Err(corrupt());
    }
    for (provider, amount) in provider_totals {
        if amount
            > project
                .provider_cost_ceilings_micros
                .get(provider)
                .copied()
                .ok_or_else(corrupt)?
        {
            return Err(corrupt());
        }
    }
    for (work_id, amount) in work_totals {
        if amount
            > project
                .work_items
                .get(work_id)
                .ok_or_else(corrupt)?
                .spec
                .budget_micros
        {
            return Err(corrupt());
        }
    }
    validate_support_collections(project, &participant, &principal_participant, &mut ids)?;
    let has_blocker = project
        .blockers
        .iter()
        .any(|blocker| blocker.state != BlockerStateV1::Resolved)
        || project
            .work_items
            .values()
            .any(|work| work.state == CompanyWorkStateV1::Blocked);
    if (project.lifecycle_state != ProjectLifecycleStateV1::Planning
        && project.lifecycle_state != ProjectLifecycleStateV1::Cancelled
        && project.work_items.is_empty())
        || (project.lifecycle_state == ProjectLifecycleStateV1::Planning
            && project.work_items.values().any(|work| {
                !work.assignments.is_empty()
                    || !matches!(
                        work.state,
                        CompanyWorkStateV1::Ready | CompanyWorkStateV1::DependencyPending
                    )
            }))
        || (project.lifecycle_state == ProjectLifecycleStateV1::DeliveryCandidate
            && (project.work_items.is_empty()
                || project
                    .work_items
                    .values()
                    .any(|work| work.state != CompanyWorkStateV1::Done)))
        || (project.lifecycle_state == ProjectLifecycleStateV1::Blocked && !has_blocker)
        || (project.lifecycle_state == ProjectLifecycleStateV1::Active && has_blocker)
    {
        return Err(corrupt());
    }
    Ok(())
}

fn validate_support_collections<'a>(
    project: &'a ProjectV1,
    participant: &impl Fn(crate::AgentId) -> Result<&'a ParticipantBindingV1, WorkflowError>,
    principal_participant: &impl Fn(&str) -> Result<&'a ParticipantBindingV1, WorkflowError>,
    ids: &mut BTreeSet<(&'static str, &'a str)>,
) -> Result<(), WorkflowError> {
    for room in &project.rooms {
        validate_identifier(&room.room_id).map_err(|_| corrupt())?;
        if room.members.is_empty()
            || !room.members.windows(2).all(|pair| pair[0].0 < pair[1].0)
            || !ids.insert(("room", room.room_id.as_str()))
            || room.created_at_unix_ms < project.created_at_unix_ms
            || room.created_at_unix_ms > project.updated_at_unix_ms
        {
            return Err(corrupt());
        }
        for member in &room.members {
            participant(*member)?;
        }
    }
    for question in &project.questions {
        validate_identifier(&question.question_id).map_err(|_| corrupt())?;
        validate_text(&question.question_ref).map_err(|_| corrupt())?;
        validate_identifier(&question.created_by).map_err(|_| corrupt())?;
        validate_optional_work(project, question.work_item_id.as_ref()).map_err(|_| corrupt())?;
        let owner = participant(question.owner)?;
        let creator = principal_participant(&question.created_by)?;
        let creator_authorized = creator.agent_id == question.owner
            || matches!(
                creator.role,
                CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
            );
        if !creator_authorized
            || question.created_at_unix_ms == 0
            || question.updated_at_unix_ms < question.created_at_unix_ms
            || question.created_at_unix_ms < project.created_at_unix_ms
            || question.updated_at_unix_ms > project.updated_at_unix_ms
            || !ids.insert(("question", question.question_id.as_str()))
            || (question.resolution_ref.is_some() != question.resolved_by.is_some())
        {
            return Err(corrupt());
        }
        if let Some(value) = &question.resolution_ref {
            validate_text(value).map_err(|_| corrupt())?;
            let resolved_by = question.resolved_by.as_deref().ok_or_else(corrupt)?;
            validate_identifier(resolved_by).map_err(|_| corrupt())?;
            if resolved_by != owner.principal_id {
                return Err(corrupt());
            }
        }
    }
    for action in &project.actions {
        validate_identifier(&action.action_id).map_err(|_| corrupt())?;
        validate_text(&action.action_ref).map_err(|_| corrupt())?;
        validate_identifier(&action.created_by).map_err(|_| corrupt())?;
        validate_optional_work(project, action.work_item_id.as_ref()).map_err(|_| corrupt())?;
        let owner = participant(action.owner)?;
        let creator = principal_participant(&action.created_by)?;
        let creator_authorized = creator.agent_id == action.owner
            || matches!(
                creator.role,
                CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
            );
        if !creator_authorized
            || action.created_at_unix_ms == 0
            || action.updated_at_unix_ms < action.created_at_unix_ms
            || action.created_at_unix_ms < project.created_at_unix_ms
            || action.updated_at_unix_ms > project.updated_at_unix_ms
            || !ids.insert(("action", action.action_id.as_str()))
            || (action.completed
                != (action.completed_by.is_some() && action.resolution_ref.is_some()))
        {
            return Err(corrupt());
        }
        if let Some(value) = &action.resolution_ref {
            validate_text(value).map_err(|_| corrupt())?;
            let completed_by = action.completed_by.as_deref().ok_or_else(corrupt)?;
            validate_identifier(completed_by).map_err(|_| corrupt())?;
            if completed_by != owner.principal_id {
                return Err(corrupt());
            }
        }
    }
    Ok(())
}

fn validate_transition_history(history: &[StateTransitionAuditV1]) -> Result<(), WorkflowError> {
    let mut previous_time = 0_u64;
    let mut previous_after: Option<&str> = None;
    for audit in history {
        validate_text(&audit.before).map_err(|_| corrupt())?;
        validate_text(&audit.after).map_err(|_| corrupt())?;
        validate_identifier(&audit.actor_id).map_err(|_| corrupt())?;
        validate_text(&audit.reason_ref).map_err(|_| corrupt())?;
        if audit.actor_agent_id.0 == 0
            || audit.occurred_at_unix_ms == 0
            || audit.occurred_at_unix_ms < previous_time
            || previous_after.is_some_and(|after| after != audit.before)
        {
            return Err(corrupt());
        }
        previous_time = audit.occurred_at_unix_ms;
        previous_after = Some(&audit.after);
    }
    Ok(())
}

fn validate_work_transition_history(
    work: &CompanyWorkItemV1,
    project_updated_at_unix_ms: u64,
) -> Result<(), WorkflowError> {
    validate_transition_history(&work.transition_history)?;
    let initial = if work.spec.dependency_ids.is_empty() {
        CompanyWorkStateV1::Ready
    } else {
        CompanyWorkStateV1::DependencyPending
    };
    if work.transition_history.is_empty() {
        return if work.state == initial {
            Ok(())
        } else {
            Err(corrupt())
        };
    }
    if work
        .transition_history
        .first()
        .map(|audit| audit.before.as_str())
        != Some(enum_name(initial).as_str())
        || work
            .transition_history
            .last()
            .map(|audit| audit.after.as_str())
            != Some(enum_name(work.state).as_str())
        || work
            .transition_history
            .last()
            .is_some_and(|audit| audit.occurred_at_unix_ms > project_updated_at_unix_ms)
    {
        return Err(corrupt());
    }
    for audit in &work.transition_history {
        let legal = matches!(
            (audit.before.as_str(), audit.after.as_str()),
            ("DependencyPending", "Ready")
                | ("Ready", "Assigned")
                | ("Assigned", "InProgress")
                | ("Assigned", "Blocked")
                | ("InProgress", "InReview")
                | ("InProgress", "Blocked")
                | ("InReview", "Done")
                | ("Assigned", "Assigned")
                | ("Blocked", "Assigned")
        );
        if !legal {
            return Err(corrupt());
        }
    }
    Ok(())
}

fn work_transition_actor_is_authorized(
    project: &ProjectV1,
    work: &CompanyWorkItemV1,
    audit_index: usize,
    audit: &StateTransitionAuditV1,
    actor: &ParticipantBindingV1,
) -> bool {
    let is_manager = matches!(
        actor.role,
        CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
    );
    let active_assignment = assignment_at_transition(work, audit_index);
    let is_independent_qa = matches!(
        actor.role,
        CompanyRoleV1::Qa | CompanyRoleV1::ReleaseManager
    ) && active_assignment.map(|assignment| assignment.agent_id)
        != Some(actor.agent_id);
    let is_assignee =
        active_assignment.map(|assignment| assignment.agent_id) == Some(actor.agent_id);
    match (audit.before.as_str(), audit.after.as_str()) {
        ("Ready", "Assigned") => {
            is_manager
                && active_assignment.is_some_and(|assignment| {
                    assignment.created_at_unix_ms == audit.occurred_at_unix_ms
                        && assignment.assigned_by == audit.actor_id
                        && assignment.delegated_by.is_none()
                })
        }
        ("Assigned", "InProgress") | ("InProgress", "InReview") => is_assignee,
        ("InReview", "Done") => is_independent_qa,
        ("Assigned", "Blocked") | ("InProgress", "Blocked") => is_assignee || is_manager,
        ("Assigned", "Assigned") | ("Blocked", "Assigned") => {
            active_assignment.is_some_and(|assignment| {
                assignment.created_at_unix_ms == audit.occurred_at_unix_ms
                    && assignment.assigned_by == audit.actor_id
                    && match assignment.delegated_by {
                        None => is_manager,
                        Some(delegator) => {
                            delegator == actor.agent_id
                                && assignment_index_at_transition(work, audit_index)
                                    .and_then(|index| index.checked_sub(1))
                                    .and_then(|index| work.assignments.get(index))
                                    .map(|previous| previous.agent_id)
                                    == Some(delegator)
                                && is_direct_report(
                                    &project.governance,
                                    assignment.agent_id,
                                    actor.agent_id,
                                )
                        }
                    }
            })
        }
        ("DependencyPending", "Ready") => {
            audit.reason_ref == "dependency-contract-satisfied"
                && is_independent_qa
                && work.spec.dependency_ids.iter().any(|dependency_id| {
                    project
                        .work_items
                        .get(dependency_id)
                        .is_some_and(|dependency| {
                            dependency.transition_history.iter().any(|transition| {
                                transition.before == "InReview"
                                    && transition.after == "Done"
                                    && transition.actor_agent_id == audit.actor_agent_id
                                    && transition.actor_id == audit.actor_id
                                    && transition.occurred_at_unix_ms == audit.occurred_at_unix_ms
                            })
                        })
                })
        }
        _ => false,
    }
}

fn assignment_at_transition(work: &CompanyWorkItemV1, audit_index: usize) -> Option<&AssignmentV1> {
    work.assignments
        .get(assignment_index_at_transition(work, audit_index)?)
}

fn assignment_index_at_transition(work: &CompanyWorkItemV1, audit_index: usize) -> Option<usize> {
    work.transition_history[..=audit_index]
        .iter()
        .filter(|audit| {
            matches!(
                (audit.before.as_str(), audit.after.as_str()),
                ("Ready", "Assigned") | ("Assigned", "Assigned") | ("Blocked", "Assigned")
            )
        })
        .count()
        .checked_sub(1)
}

fn assignment_active_at(
    work: &CompanyWorkItemV1,
    agent_id: crate::AgentId,
    occurred_at_unix_ms: u64,
) -> bool {
    work.assignments.iter().any(|assignment| {
        assignment.agent_id == agent_id
            && assignment.created_at_unix_ms <= occurred_at_unix_ms
            && assignment
                .ended_at_unix_ms
                .is_none_or(|ended| occurred_at_unix_ms < ended)
    })
}

fn validate_handoff_transition_history(
    handoff: &HandoffV1,
    project_updated_at_unix_ms: u64,
) -> Result<(), WorkflowError> {
    validate_transition_history(&handoff.transition_history)?;
    match handoff.state {
        HandoffStateV1::Offered if handoff.transition_history.is_empty() => Ok(()),
        HandoffStateV1::Accepted | HandoffStateV1::Rejected
            if handoff.transition_history.len() == 1
                && handoff.transition_history[0].before == "Offered"
                && handoff.transition_history[0].after == enum_name(handoff.state)
                && handoff.transition_history[0].occurred_at_unix_ms
                    <= project_updated_at_unix_ms =>
        {
            Ok(())
        }
        _ => Err(corrupt()),
    }
}

fn validate_blocker_transition_history(
    blocker: &BlockerV1,
    project_updated_at_unix_ms: u64,
) -> Result<(), WorkflowError> {
    validate_transition_history(&blocker.transition_history)?;
    let legal_shape = match blocker.state {
        BlockerStateV1::Open => blocker.transition_history.is_empty(),
        BlockerStateV1::Escalated => {
            blocker.transition_history.len() == 1
                && blocker.transition_history[0].before == "Open"
                && blocker.transition_history[0].after == "Escalated"
        }
        BlockerStateV1::Resolved => match blocker.transition_history.as_slice() {
            [resolved] => resolved.before == "Open" && resolved.after == "Resolved",
            [escalated, resolved] => {
                escalated.before == "Open"
                    && escalated.after == "Escalated"
                    && resolved.before == "Escalated"
                    && resolved.after == "Resolved"
            }
            _ => false,
        },
    };
    if !legal_shape
        || blocker.transition_history.last().is_some_and(|audit| {
            audit.after != enum_name(blocker.state)
                || audit.actor_id != blocker.last_actor_id
                || audit.occurred_at_unix_ms > project_updated_at_unix_ms
        })
    {
        return Err(corrupt());
    }
    Ok(())
}

fn validate_optional_work(
    project: &ProjectV1,
    work_item_id: Option<&crate::WorkItemId>,
) -> Result<(), WorkflowError> {
    if work_item_id.is_some_and(|id| !project.work_items.contains_key(id)) {
        Err(not_found())
    } else {
        Ok(())
    }
}

fn ensure_collection_capacity(current_len: usize) -> Result<(), WorkflowError> {
    if current_len >= MAX_AGGREGATE_ITEMS {
        Err(invalid("company aggregate collection limit reached"))
    } else {
        Ok(())
    }
}

fn authorize_project_actor(
    project: &ProjectV1,
    principal: &AuthenticatedCompanyPrincipalV1,
) -> Result<crate::AgentId, WorkflowError> {
    if principal.kind != CompanyPrincipalKindV1::Agent {
        return Err(unauthorized());
    }
    let agent_id = principal.agent_id.ok_or_else(unauthorized)?;
    let participant = project
        .governance
        .participants
        .iter()
        .find(|participant| participant.agent_id == agent_id)
        .ok_or_else(unauthorized)?;
    if participant.role != principal.role || participant.principal_id != principal.principal_id {
        return Err(unauthorized());
    }
    participant.validate().map_err(|_| unauthorized())?;
    Ok(agent_id)
}

fn is_direct_report(
    governance: &ProposalGovernanceV1,
    candidate: crate::AgentId,
    manager: crate::AgentId,
) -> bool {
    governance.participants.iter().any(|participant| {
        participant.agent_id == candidate && participant.reports_to == Some(manager)
    })
}

fn current_assignment(work: &CompanyWorkItemV1) -> Option<&AssignmentV1> {
    let mut current = work
        .assignments
        .iter()
        .filter(|assignment| assignment.active);
    let value = current.next()?;
    if current.next().is_some() {
        None
    } else {
        Some(value)
    }
}

fn append_work_transition(
    work: &mut CompanyWorkItemV1,
    principal: &AuthenticatedCompanyPrincipalV1,
    actor_id: crate::AgentId,
    before: CompanyWorkStateV1,
    after: CompanyWorkStateV1,
    reason_ref: &str,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    ensure_collection_capacity(work.transition_history.len())?;
    work.transition_history.push(StateTransitionAuditV1 {
        before: enum_name(before),
        after: enum_name(after),
        actor_id: principal.principal_id.clone(),
        actor_agent_id: actor_id,
        reason_ref: reason_ref.to_owned(),
        occurred_at_unix_ms: now_ms,
    });
    Ok(())
}

fn reservation_effective_amount(reservation: &CostReservationV1) -> u64 {
    match reservation.state {
        CostReservationStateV1::Active => reservation.reserved_micros,
        CostReservationStateV1::Committed => reservation.committed_micros.unwrap_or(0),
        CostReservationStateV1::Released => 0,
    }
}

fn resolve_budget_blockers_if_headroom(
    project: &mut ProjectV1,
    principal: &AuthenticatedCompanyPrincipalV1,
    actor_id: crate::AgentId,
    now_ms: u64,
    reason_ref: &str,
) -> Result<(), WorkflowError> {
    if project.reserved_cost_micros >= project.cost_ceiling_micros {
        return Ok(());
    }
    for blocker in project.blockers.iter_mut().filter(|blocker| {
        blocker.blocker_kind == BlockerKindV1::BudgetExhausted
            && blocker.state != BlockerStateV1::Resolved
    }) {
        ensure_collection_capacity(blocker.transition_history.len())?;
        blocker.transition_history.push(StateTransitionAuditV1 {
            before: enum_name(blocker.state),
            after: "Resolved".to_owned(),
            actor_id: principal.principal_id.clone(),
            actor_agent_id: actor_id,
            reason_ref: reason_ref.to_owned(),
            occurred_at_unix_ms: now_ms,
        });
        blocker.state = BlockerStateV1::Resolved;
        blocker.resolution_ref = Some(reason_ref.to_owned());
        blocker.last_actor_id = principal.principal_id.clone();
        blocker.updated_at_unix_ms = now_ms;
    }
    restore_project_lifecycle_after_last_blocker(project);
    refresh_project_lifecycle(project);
    Ok(())
}

fn validate_output_receipts(
    spec: &CompanyWorkItemSpecV1,
    receipts: &[WorkOutputReceiptV1],
) -> Result<(), WorkflowError> {
    if receipts.len() != spec.outputs.len() {
        return Err(transition());
    }
    let mut names = BTreeSet::new();
    for receipt in receipts {
        validate_identifier(&receipt.name)?;
        validate_digest(&receipt.contract_digest)?;
        validate_digest(&receipt.content_digest)?;
        if !names.insert(receipt.name.as_str()) {
            return Err(invalid("work output receipt is duplicated"));
        }
        let expected = spec
            .outputs
            .iter()
            .find(|output| output.name == receipt.name)
            .ok_or_else(transition)?;
        if receipt.contract_generation != expected.contract_generation
            || !constant_time_eq(&receipt.contract_digest, &expected.contract_digest)
        {
            return Err(transition());
        }
    }
    Ok(())
}

fn refresh_dependency_states(
    project: &mut ProjectV1,
    principal: &AuthenticatedCompanyPrincipalV1,
    actor_id: crate::AgentId,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    let ready = project
        .work_items
        .iter()
        .filter(|(_, work)| work.state == CompanyWorkStateV1::DependencyPending)
        .filter_map(|(id, work)| {
            let dependencies_ready = dependencies_satisfied(project, work);
            dependencies_ready.then(|| id.clone())
        })
        .collect::<Vec<_>>();
    for id in ready {
        let work = project.work_items.get_mut(&id).ok_or_else(corrupt)?;
        append_work_transition(
            work,
            principal,
            actor_id,
            CompanyWorkStateV1::DependencyPending,
            CompanyWorkStateV1::Ready,
            "dependency-contract-satisfied",
            now_ms,
        )?;
        work.state = CompanyWorkStateV1::Ready;
        work.version = work.version.checked_add(1).ok_or_else(corrupt)?;
    }
    Ok(())
}

fn dependencies_satisfied(project: &ProjectV1, work: &CompanyWorkItemV1) -> bool {
    work.spec.dependency_ids.iter().all(|dependency_id| {
        project
            .work_items
            .get(dependency_id)
            .is_some_and(|dependency| {
                dependency.state == CompanyWorkStateV1::Done
                    && work
                        .spec
                        .inputs
                        .iter()
                        .filter(|input| input.producer_work_item_id == *dependency_id)
                        .all(|input| {
                            dependency.output_receipts.iter().any(|output| {
                                output.name == input.producer_output_name
                                    && output.contract_generation
                                        == input.expected_contract_generation
                                    && constant_time_eq(
                                        &output.contract_digest,
                                        &input.expected_contract_digest,
                                    )
                            })
                        })
            })
    })
}

fn refresh_project_lifecycle(project: &mut ProjectV1) {
    if project.lifecycle_state == ProjectLifecycleStateV1::Cancelled {
        return;
    }
    if !project.work_items.is_empty()
        && project
            .work_items
            .values()
            .all(|work| work.state == CompanyWorkStateV1::Done)
    {
        project.lifecycle_state = ProjectLifecycleStateV1::DeliveryCandidate;
    } else if project
        .blockers
        .iter()
        .any(|blocker| blocker.state != BlockerStateV1::Resolved)
        || project
            .work_items
            .values()
            .any(|work| work.state == CompanyWorkStateV1::Blocked)
    {
        project.lifecycle_state = ProjectLifecycleStateV1::Blocked;
    } else if project.lifecycle_state != ProjectLifecycleStateV1::Planning {
        project.lifecycle_state = ProjectLifecycleStateV1::Active;
    }
}

fn restore_project_lifecycle_after_last_blocker(project: &mut ProjectV1) {
    if project
        .blockers
        .iter()
        .any(|blocker| blocker.state != BlockerStateV1::Resolved)
    {
        return;
    }
    if let Some(state) = project
        .blockers
        .iter()
        .rev()
        .filter_map(|blocker| blocker.blocked_from_state)
        .find(|state| *state != ProjectLifecycleStateV1::Blocked)
    {
        project.lifecycle_state = state;
    }
}

fn put_entity<T: Serialize>(
    transaction: &Transaction<'_>,
    tenant_id: &TenantId,
    kind: &str,
    id: &str,
    version: u64,
    value: &T,
) -> Result<(), WorkflowError> {
    let payload = encode(value)?;
    let payload_digest = bytes_digest("sentinel.workflow.company-entity-row.v1", &payload)?;
    transaction.execute(
        "INSERT INTO company_entities(tenant_id,entity_kind,entity_id,version,payload,payload_digest) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(tenant_id,entity_kind,entity_id) DO UPDATE SET version=excluded.version,payload=excluded.payload,payload_digest=excluded.payload_digest",
        params![tenant_id.0, kind, id, sql_u64(version)?, payload, payload_digest],
    ).map_err(WorkflowError::from)?;
    Ok(())
}

trait CompanyEntity {
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64);
    fn validate_entity(&self) -> Result<(), WorkflowError>;
}

impl CompanyEntity for CustomerRequestV1 {
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        (&self.tenant_id, "request", &self.request_id, self.version)
    }

    fn validate_entity(&self) -> Result<(), WorkflowError> {
        validate_customer_request(self)
    }
}

impl CompanyEntity for ProposalV1 {
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        (
            &self.tenant_id,
            "proposal",
            &self.proposal_id,
            u64::from(self.generation),
        )
    }

    fn validate_entity(&self) -> Result<(), WorkflowError> {
        validate_proposal(self)
    }
}

impl CompanyEntity for AgreementV1 {
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        (&self.tenant_id, "agreement", &self.agreement_id, 1)
    }

    fn validate_entity(&self) -> Result<(), WorkflowError> {
        validate_agreement(self)
    }
}

impl CompanyEntity for ProjectV1 {
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        (&self.tenant_id, "project", &self.project_id.0, self.version)
    }

    fn validate_entity(&self) -> Result<(), WorkflowError> {
        validate_project(self)
    }
}

fn get_entity<T: DeserializeOwned + CompanyEntity>(
    connection: &Connection,
    tenant_id: &TenantId,
    kind: &str,
    id: &str,
) -> Result<Option<T>, WorkflowError> {
    connection.query_row("SELECT tenant_id,entity_kind,entity_id,version,payload,payload_digest FROM company_entities WHERE tenant_id=?1 AND entity_kind=?2 AND entity_id=?3", params![tenant_id.0, kind, id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, i64>(3)?, row.get::<_, Vec<u8>>(4)?, row.get::<_, String>(5)?))).optional().map_err(WorkflowError::from)?.map(|(row_tenant,row_kind,row_id,row_version,payload,payload_digest)| {
        if row_tenant != tenant_id.0 || row_kind != kind || row_id != id || row_version <= 0 || !constant_time_eq(&bytes_digest("sentinel.workflow.company-entity-row.v1", &payload)?, &payload_digest) {
            return Err(corrupt());
        }
        let value: T = decode(&payload)?;
        let (value_tenant, value_kind, value_id, value_version) = value.row_binding();
        if value_tenant != tenant_id
            || value_kind != kind
            || value_id != id
            || value_version != stored_u64(row_version)?
        {
            return Err(corrupt());
        }
        value.validate_entity().map_err(|_| corrupt())?;
        Ok(value)
    }).transpose()
}

fn bytes_digest(domain: &'static str, value: &[u8]) -> Result<String, WorkflowError> {
    canonical_sha256(domain, &value)
}

fn enum_name<T: std::fmt::Debug>(value: T) -> String {
    format!("{value:?}")
}

fn require_role(
    principal: &AuthenticatedCompanyPrincipalV1,
    roles: &[CompanyRoleV1],
) -> Result<(), WorkflowError> {
    if roles.contains(&principal.role) {
        Ok(())
    } else {
        Err(unauthorized())
    }
}
fn require_version(actual: u64, expected: u64) -> Result<(), WorkflowError> {
    if actual == expected {
        Ok(())
    } else {
        Err(WorkflowError::new(
            WorkflowErrorCode::VersionConflict,
            false,
            "company aggregate version is stale",
        ))
    }
}
fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, WorkflowError> {
    serde_json::to_vec(value).map_err(|_| persistence())
}
fn decode<T: DeserializeOwned>(value: &[u8]) -> Result<T, WorkflowError> {
    serde_json::from_slice(value).map_err(WorkflowError::from)
}
fn sql_u64(value: u64) -> Result<i64, WorkflowError> {
    i64::try_from(value).map_err(|_| persistence())
}
fn stored_u64(value: i64) -> Result<u64, WorkflowError> {
    u64::try_from(value).map_err(|_| corrupt())
}
fn invalid(message: &'static str) -> WorkflowError {
    WorkflowError::new(WorkflowErrorCode::InvalidInput, false, message)
}
fn unauthorized() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::AuthorityConflict,
        false,
        "company principal is not authorized",
    )
}
fn transition() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::InvalidTransition,
        false,
        "company aggregate transition is invalid",
    )
}
fn not_found() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::NotFound,
        false,
        "company aggregate was not found",
    )
}
fn corrupt() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::CorruptStore,
        false,
        "company workflow store integrity validation failed",
    )
}
fn persistence() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::PersistenceFailure,
        false,
        "company workflow persistence failed",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentId, CompanyPrincipalKindV1, CompanyRoleV1, ParticipantBindingV1, ProposalGovernanceV1,
        WorkProfileBindingV1, COMPANY_DOMAIN_SCHEMA_VERSION,
    };
    use tempfile::TempDir;

    const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn profile(id: &str) -> WorkProfileBindingV1 {
        WorkProfileBindingV1 {
            profile_id: id.to_owned(),
            generation: 1,
            digest: DIGEST.to_owned(),
        }
    }

    fn accepted_project_fixture() -> (
        TempDir,
        std::path::PathBuf,
        WorkflowStore,
        AuthenticatedCompanyPrincipalV1,
        ProjectV1,
    ) {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("workflow.sqlite");
        let store = WorkflowStore::open(&path).unwrap();
        let tenant_id = TenantId::parse("tenant-a").unwrap();
        let principal = AuthenticatedCompanyPrincipalV1 {
            schema_version: COMPANY_DOMAIN_SCHEMA_VERSION,
            tenant_id: tenant_id.clone(),
            principal_id: "customer-a".to_owned(),
            kind: CompanyPrincipalKindV1::Customer,
            role: CompanyRoleV1::Customer,
            customer_id: Some("customer-a".to_owned()),
            agent_id: None,
            authority_generation: 1,
            authority_digest: DIGEST.to_owned(),
        };
        let binding = ProposalBindingV1 {
            scope: "bounded-scope".to_owned(),
            deliverables: vec!["artifact".to_owned()],
            exclusions: Vec::new(),
            acceptance_criteria: vec!["qa".to_owned()],
            assumptions: Vec::new(),
            cost_ceiling_micros: 100,
            provider_cost_ceilings_micros: BTreeMap::from([("local".to_owned(), 100)]),
            governance: ProposalGovernanceV1 {
                owner: AgentId(1),
                participants: vec![
                    ParticipantBindingV1 {
                        agent_id: AgentId(1),
                        principal_id: "pm-a".to_owned(),
                        role: CompanyRoleV1::ProjectManager,
                        specialties: BTreeSet::from(["coordination".to_owned()]),
                        reports_to: None,
                        profile: profile("pm-v1"),
                    },
                    ParticipantBindingV1 {
                        agent_id: AgentId(2),
                        principal_id: "developer-a".to_owned(),
                        role: CompanyRoleV1::Developer,
                        specialties: BTreeSet::from(["rust".to_owned()]),
                        reports_to: Some(AgentId(1)),
                        profile: profile("developer-v1"),
                    },
                    ParticipantBindingV1 {
                        agent_id: AgentId(3),
                        principal_id: "qa-a".to_owned(),
                        role: CompanyRoleV1::Qa,
                        specialties: BTreeSet::from(["qa".to_owned()]),
                        reports_to: Some(AgentId(1)),
                        profile: profile("qa-v1"),
                    },
                    ParticipantBindingV1 {
                        agent_id: AgentId(5),
                        principal_id: "pm-b".to_owned(),
                        role: CompanyRoleV1::ProjectManager,
                        specialties: BTreeSet::from(["coordination".to_owned()]),
                        reports_to: None,
                        profile: profile("pm-v1"),
                    },
                ],
                project_profile: profile("project-v1"),
            },
            expires_at_unix_ms: 100,
        };
        let proposal_digest =
            canonical_sha256("sentinel.workflow.proposal-binding.v1", &binding).unwrap();
        let request = CustomerRequestV1 {
            schema_version: COMPANY_DOMAIN_SCHEMA_VERSION,
            request_id: "request-a".to_owned(),
            tenant_id: tenant_id.clone(),
            customer_id: "customer-a".to_owned(),
            summary_ref: "summary".to_owned(),
            desired_outcome: "outcome".to_owned(),
            constraints: Vec::new(),
            clarifications: Vec::new(),
            feedback: Vec::new(),
            state: CustomerRequestStateV1::Proposed,
            version: 1,
            proposal_ids: vec!["proposal-a".to_owned()],
            created_at_unix_ms: 1,
            updated_at_unix_ms: 1,
        };
        let proposal = ProposalV1 {
            schema_version: COMPANY_DOMAIN_SCHEMA_VERSION,
            proposal_id: "proposal-a".to_owned(),
            tenant_id: tenant_id.clone(),
            request_id: request.request_id.clone(),
            generation: 1,
            binding,
            proposal_digest: proposal_digest.clone(),
            created_by: "sales-a".to_owned(),
            created_at_unix_ms: 1,
        };
        {
            let mut connection = store.connection.lock().unwrap();
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            put_entity(
                &transaction,
                &tenant_id,
                "request",
                &request.request_id,
                request.version,
                &request,
            )
            .unwrap();
            put_entity(
                &transaction,
                &tenant_id,
                "proposal",
                &proposal.proposal_id,
                1,
                &proposal,
            )
            .unwrap();
            transaction.commit().unwrap();
        }
        let accepted = store
            .apply_company_command(
                &principal,
                Uuid::from_u128(10),
                &CompanyWorkflowCommandV1::AcceptProposal {
                    request_id: request.request_id,
                    expected_version: 1,
                    proposal_id: proposal.proposal_id,
                    proposal_digest,
                },
                2,
            )
            .unwrap();
        let CompanyWorkflowResponseV1::AgreementProject { project, .. } = accepted.response else {
            panic!()
        };
        (temp, path, store, principal, *project)
    }

    fn audit(
        before: &str,
        after: &str,
        actor_agent_id: AgentId,
        occurred_at_unix_ms: u64,
    ) -> StateTransitionAuditV1 {
        StateTransitionAuditV1 {
            before: before.to_owned(),
            after: after.to_owned(),
            actor_id: format!("agent-{}", actor_agent_id.0),
            actor_agent_id,
            reason_ref: "bounded-transition".to_owned(),
            occurred_at_unix_ms,
        }
    }

    fn company_row_counts(store: &WorkflowStore) -> (i64, i64, i64, i64) {
        let connection = store.connection.lock().unwrap();
        (
            connection
                .query_row("SELECT COUNT(*) FROM company_entities", [], |row| {
                    row.get(0)
                })
                .unwrap(),
            connection
                .query_row("SELECT COUNT(*) FROM company_events", [], |row| row.get(0))
                .unwrap(),
            connection
                .query_row("SELECT COUNT(*) FROM company_operations", [], |row| {
                    row.get(0)
                })
                .unwrap(),
            connection
                .query_row(
                    "SELECT COUNT(*) FROM company_project_projections",
                    [],
                    |row| row.get(0),
                )
                .unwrap(),
        )
    }

    #[test]
    fn backdated_request_and_project_commands_write_no_rows() {
        let (_temp, _path, store, customer, project) = accepted_project_fixture();
        let before = company_row_counts(&store);
        let request: CustomerRequestV1 = get_entity(
            &store.connection.lock().unwrap(),
            &project.tenant_id,
            "request",
            "request-a",
        )
        .unwrap()
        .unwrap();
        let error = store
            .apply_company_command(
                &customer,
                Uuid::from_u128(20),
                &CompanyWorkflowCommandV1::RecordCustomerFeedback {
                    request_id: request.request_id,
                    expected_version: request.version,
                    feedback_ref: "backdated-feedback".to_owned(),
                },
                request.updated_at_unix_ms - 1,
            )
            .unwrap_err();
        assert_eq!(error.code, WorkflowErrorCode::InvalidInput);
        assert_eq!(company_row_counts(&store), before);

        let pm = AuthenticatedCompanyPrincipalV1 {
            schema_version: COMPANY_DOMAIN_SCHEMA_VERSION,
            tenant_id: project.tenant_id.clone(),
            principal_id: "pm-a".to_owned(),
            kind: CompanyPrincipalKindV1::Agent,
            role: CompanyRoleV1::ProjectManager,
            customer_id: None,
            agent_id: Some(AgentId(1)),
            authority_generation: 1,
            authority_digest: DIGEST.to_owned(),
        };
        let error = store
            .apply_company_command(
                &pm,
                Uuid::from_u128(21),
                &CompanyWorkflowCommandV1::RecordDecision {
                    project_id: project.project_id,
                    expected_version: project.version,
                    work_item_id: None,
                    choice_ref: "backdated-choice".to_owned(),
                    rationale_ref: "must-not-persist".to_owned(),
                },
                project.updated_at_unix_ms - 1,
            )
            .unwrap_err();
        assert_eq!(error.code, WorkflowErrorCode::InvalidInput);
        assert_eq!(company_row_counts(&store), before);
    }

    #[test]
    fn projection_rebuild_rejects_regressing_project_event_time_without_replacing_projection() {
        let (_temp, _path, store, _customer, project) = accepted_project_fixture();
        let pm = AuthenticatedCompanyPrincipalV1 {
            schema_version: COMPANY_DOMAIN_SCHEMA_VERSION,
            tenant_id: project.tenant_id.clone(),
            principal_id: "pm-a".to_owned(),
            kind: CompanyPrincipalKindV1::Agent,
            role: CompanyRoleV1::ProjectManager,
            customer_id: None,
            agent_id: Some(AgentId(1)),
            authority_generation: 1,
            authority_digest: DIGEST.to_owned(),
        };
        let first = store
            .apply_company_command(
                &pm,
                Uuid::from_u128(30),
                &CompanyWorkflowCommandV1::RecordDecision {
                    project_id: project.project_id.clone(),
                    expected_version: project.version,
                    work_item_id: None,
                    choice_ref: "first-choice".to_owned(),
                    rationale_ref: "first-rationale".to_owned(),
                },
                4,
            )
            .unwrap();
        let CompanyWorkflowResponseV1::Project(first) = first.response else {
            panic!()
        };
        store
            .apply_company_command(
                &pm,
                Uuid::from_u128(31),
                &CompanyWorkflowCommandV1::RecordDecision {
                    project_id: project.project_id.clone(),
                    expected_version: first.version,
                    work_item_id: None,
                    choice_ref: "second-choice".to_owned(),
                    rationale_ref: "second-rationale".to_owned(),
                },
                5,
            )
            .unwrap();

        let connection = store.connection.lock().unwrap();
        let projection_before = connection
            .query_row(
                "SELECT source_sequence,payload,payload_digest FROM company_project_projections WHERE tenant_id=?1 AND project_id=?2",
                params![project.tenant_id.0, project.project_id.0],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?, row.get::<_, String>(2)?)),
            )
            .unwrap();
        let event = connection
            .query_row(
                "SELECT tenant_id,project_id,event_type,operation_id,operation_digest,authority_binding_digest,payload FROM company_events WHERE operation_id=?1 AND event_type='project_decision_recorded'",
                [Uuid::from_u128(30).to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?, row.get::<_, Vec<u8>>(6)?)),
            )
            .unwrap();
        let mut tampered_project: ProjectV1 = decode(&event.6).unwrap();
        tampered_project.updated_at_unix_ms = 6;
        let payload = encode(&tampered_project).unwrap();
        let payload_digest =
            bytes_digest("sentinel.workflow.company-event-payload.v1", &payload).unwrap();
        let tenant_id = TenantId::parse(&event.0).unwrap();
        let project_id = ProjectId::parse(&event.1).unwrap();
        let operation_id = Uuid::parse_str(&event.3).unwrap();
        let event_id = canonical_sha256(
            "sentinel.workflow.company-event-id.v1",
            &(
                &tenant_id,
                Some(&project_id),
                event.2.as_str(),
                operation_id,
                event.4.as_str(),
                event.5.as_str(),
                payload_digest.as_str(),
                6_u64,
            ),
        )
        .unwrap();
        let response = encode(&CompanyWorkflowResponseV1::Project(tampered_project)).unwrap();
        let response_digest =
            bytes_digest("sentinel.workflow.company-operation-response.v1", &response).unwrap();
        connection
            .execute(
                "UPDATE company_events SET event_id=?1,payload=?2,payload_digest=?3,created_at_ms=6 WHERE operation_id=?4 AND event_type='project_decision_recorded'",
                params![event_id, payload, payload_digest, event.3],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE company_operations SET response=?1,response_digest=?2,created_at_ms=6 WHERE operation_id=?3",
                params![response, response_digest, Uuid::from_u128(30).to_string()],
            )
            .unwrap();
        drop(connection);

        assert_eq!(
            store
                .rebuild_company_project_projections()
                .unwrap_err()
                .code,
            WorkflowErrorCode::CorruptStore
        );
        let connection = store.connection.lock().unwrap();
        let projection_after = connection
            .query_row(
                "SELECT source_sequence,payload,payload_digest FROM company_project_projections WHERE tenant_id=?1 AND project_id=?2",
                params![project.tenant_id.0, project.project_id.0],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?, row.get::<_, String>(2)?)),
            )
            .unwrap();
        assert_eq!(projection_after, projection_before);
    }

    #[test]
    fn lifecycle_histories_reject_discontinuity_reordering_and_impossible_shapes() {
        assert_eq!(
            validate_transition_history(&[
                audit("Ready", "Assigned", AgentId(1), 2),
                audit("Blocked", "Assigned", AgentId(1), 3),
            ])
            .unwrap_err()
            .code,
            WorkflowErrorCode::CorruptStore
        );
        assert_eq!(
            validate_transition_history(&[
                audit("Ready", "Assigned", AgentId(1), 3),
                audit("Assigned", "Blocked", AgentId(1), 2),
            ])
            .unwrap_err()
            .code,
            WorkflowErrorCode::CorruptStore
        );

        let handoff = HandoffV1 {
            handoff_id: "handoff-a".to_owned(),
            work_item_id: crate::WorkItemId::parse("work-a").unwrap(),
            producer: AgentId(1),
            consumer: AgentId(2),
            artifact_digests: BTreeSet::from([DIGEST.to_owned()]),
            state: HandoffStateV1::Accepted,
            reason_ref: "bounded-handoff".to_owned(),
            created_at_unix_ms: 1,
            acknowledged_by: Some("agent-2".to_owned()),
            acknowledged_at_unix_ms: Some(3),
            acknowledgement_reason_ref: Some("accepted".to_owned()),
            transition_history: vec![
                audit("Offered", "Rejected", AgentId(2), 2),
                audit("Rejected", "Accepted", AgentId(2), 3),
            ],
        };
        assert_eq!(
            validate_handoff_transition_history(&handoff, 3)
                .unwrap_err()
                .code,
            WorkflowErrorCode::CorruptStore
        );

        let blocker = BlockerV1 {
            blocker_id: "blocker-a".to_owned(),
            work_item_id: None,
            cause_ref: "dependency-unavailable".to_owned(),
            owner: AgentId(1),
            escalation_target: None,
            state: BlockerStateV1::Open,
            blocker_kind: BlockerKindV1::Operational,
            blocked_from_state: None,
            resolution_ref: None,
            last_actor_id: "agent-1".to_owned(),
            created_at_unix_ms: 1,
            updated_at_unix_ms: 2,
            transition_history: vec![audit("Open", "Resolved", AgentId(1), 2)],
        };
        assert_eq!(
            validate_blocker_transition_history(&blocker, 2)
                .unwrap_err()
                .code,
            WorkflowErrorCode::CorruptStore
        );
    }

    #[test]
    fn lifecycle_history_actor_must_be_a_project_participant() {
        let (_temp, _path, _store, _principal, mut project) = accepted_project_fixture();
        project.updated_at_unix_ms = 3;
        project.blockers.push(BlockerV1 {
            blocker_id: "blocker-a".to_owned(),
            work_item_id: None,
            cause_ref: "dependency-unavailable".to_owned(),
            owner: AgentId(1),
            escalation_target: None,
            state: BlockerStateV1::Resolved,
            blocker_kind: BlockerKindV1::Operational,
            blocked_from_state: None,
            resolution_ref: Some("restored".to_owned()),
            last_actor_id: "agent-2".to_owned(),
            created_at_unix_ms: 2,
            updated_at_unix_ms: 3,
            transition_history: vec![audit("Open", "Resolved", AgentId(2), 3)],
        });
        assert_eq!(
            validate_project(&project).unwrap_err().code,
            WorkflowErrorCode::CorruptStore
        );
    }

    #[test]
    fn work_history_rejects_separately_rebound_agent_principal_and_assigner() {
        let (_temp, _path, _store, _principal, mut project) = accepted_project_fixture();
        let work_item_id = crate::WorkItemId::parse("work-a").unwrap();
        project.lifecycle_state = ProjectLifecycleStateV1::Active;
        project.updated_at_unix_ms = 3;
        project.work_items.insert(
            work_item_id.clone(),
            CompanyWorkItemV1 {
                spec: CompanyWorkItemSpecV1 {
                    work_item_id,
                    title: "bounded-work".to_owned(),
                    objective: "produce-artifact".to_owned(),
                    required_role: CompanyRoleV1::Developer,
                    required_specialties: BTreeSet::from(["rust".to_owned()]),
                    dependency_ids: BTreeSet::new(),
                    owner: AgentId(2),
                    inputs: Vec::new(),
                    outputs: vec![WorkOutputContractV1 {
                        name: "result".to_owned(),
                        media_type: "application/octet-stream".to_owned(),
                        digest_algorithm: "sha256".to_owned(),
                        contract_generation: 1,
                        contract_digest: DIGEST.to_owned(),
                    }],
                    quality_gate: QualityGateBindingV1 {
                        gate_id: "qa-v1".to_owned(),
                        generation: 1,
                        digest: DIGEST.to_owned(),
                    },
                    budget_micros: 100,
                    rework: None,
                },
                state: CompanyWorkStateV1::Assigned,
                version: 2,
                assignments: vec![AssignmentV1 {
                    assignment_id: "assignment-a".to_owned(),
                    agent_id: AgentId(2),
                    role: CompanyRoleV1::Developer,
                    specialties: BTreeSet::from(["rust".to_owned()]),
                    profile: profile("developer-v1"),
                    organization_generation: 1,
                    organization_digest: DIGEST.to_owned(),
                    assignment_version: 1,
                    delegated_by: None,
                    reason_ref: "manager-assignment".to_owned(),
                    active: true,
                    assigned_by: "pm-a".to_owned(),
                    created_at_unix_ms: 3,
                    ended_at_unix_ms: None,
                }],
                output_receipts: Vec::new(),
                gate_receipt: None,
                transition_history: vec![StateTransitionAuditV1 {
                    before: "Ready".to_owned(),
                    after: "Assigned".to_owned(),
                    actor_id: "pm-a".to_owned(),
                    actor_agent_id: AgentId(1),
                    reason_ref: "manager-assignment".to_owned(),
                    occurred_at_unix_ms: 3,
                }],
            },
        );
        validate_project(&project).unwrap();

        let mut active_reassignment = project.clone();
        active_reassignment.updated_at_unix_ms = 4;
        let work = active_reassignment.work_items.values_mut().next().unwrap();
        work.assignments[0].active = false;
        work.assignments[0].ended_at_unix_ms = Some(4);
        work.assignments.push(AssignmentV1 {
            assignment_id: "assignment-b".to_owned(),
            agent_id: AgentId(2),
            role: CompanyRoleV1::Developer,
            specialties: BTreeSet::from(["rust".to_owned()]),
            profile: profile("developer-v1"),
            organization_generation: 2,
            organization_digest: DIGEST.to_owned(),
            assignment_version: 2,
            delegated_by: None,
            reason_ref: "manager-reassignment".to_owned(),
            active: true,
            assigned_by: "pm-a".to_owned(),
            created_at_unix_ms: 4,
            ended_at_unix_ms: None,
        });
        work.transition_history.push(StateTransitionAuditV1 {
            before: "Assigned".to_owned(),
            after: "Assigned".to_owned(),
            actor_id: "pm-a".to_owned(),
            actor_agent_id: AgentId(1),
            reason_ref: "manager-reassignment".to_owned(),
            occurred_at_unix_ms: 4,
        });
        work.version = 3;
        validate_project(&active_reassignment).unwrap();

        let mut orphan_assignment = active_reassignment.clone();
        orphan_assignment
            .work_items
            .values_mut()
            .next()
            .unwrap()
            .transition_history
            .pop();
        assert_eq!(
            validate_project(&orphan_assignment).unwrap_err().code,
            WorkflowErrorCode::CorruptStore
        );
        let mut missing_assignment = active_reassignment.clone();
        let work = missing_assignment.work_items.values_mut().next().unwrap();
        work.assignments.pop();
        work.assignments[0].active = true;
        work.assignments[0].ended_at_unix_ms = None;
        assert_eq!(
            validate_project(&missing_assignment).unwrap_err().code,
            WorkflowErrorCode::CorruptStore
        );
        let mut extra_assignment = active_reassignment.clone();
        let work = extra_assignment.work_items.values_mut().next().unwrap();
        let mut extra = work.assignments[1].clone();
        extra.assignment_id = "assignment-c".to_owned();
        extra.assignment_version = 3;
        extra.created_at_unix_ms = 4;
        work.assignments[1].active = false;
        work.assignments[1].ended_at_unix_ms = Some(4);
        work.assignments.push(extra);
        assert_eq!(
            validate_project(&extra_assignment).unwrap_err().code,
            WorkflowErrorCode::CorruptStore
        );
        let mut wrong_active_actor = active_reassignment;
        let audit = &mut wrong_active_actor
            .work_items
            .values_mut()
            .next()
            .unwrap()
            .transition_history[1];
        audit.actor_agent_id = AgentId(5);
        audit.actor_id = "pm-b".to_owned();
        assert_eq!(
            validate_project(&wrong_active_actor).unwrap_err().code,
            WorkflowErrorCode::CorruptStore
        );

        let mut approved = project.clone();
        approved.approvals.push(ApprovalV1 {
            approval_id: "approval-a".to_owned(),
            work_item_id: crate::WorkItemId::parse("work-a").unwrap(),
            subject_digest: DIGEST.to_owned(),
            approved: true,
            actor_id: "qa-a".to_owned(),
            actor_agent_id: AgentId(3),
            created_at_unix_ms: 3,
        });
        validate_project(&approved).unwrap();
        let mut approval_principal_swap = approved.clone();
        approval_principal_swap.approvals[0].actor_id = "pm-a".to_owned();
        assert_eq!(
            validate_project(&approval_principal_swap).unwrap_err().code,
            WorkflowErrorCode::CorruptStore
        );
        let mut historical_self_approval = approved;
        let work = historical_self_approval
            .work_items
            .values_mut()
            .next()
            .unwrap();
        work.spec.owner = AgentId(3);
        work.spec.required_role = CompanyRoleV1::Qa;
        work.spec.required_specialties = BTreeSet::from(["qa".to_owned()]);
        work.assignments[0].agent_id = AgentId(3);
        work.assignments[0].role = CompanyRoleV1::Qa;
        work.assignments[0].specialties = BTreeSet::from(["qa".to_owned()]);
        work.assignments[0].profile = profile("qa-v1");
        assert_eq!(
            validate_project(&historical_self_approval)
                .unwrap_err()
                .code,
            WorkflowErrorCode::CorruptStore
        );

        let mut rebound_agent = project.clone();
        rebound_agent
            .work_items
            .values_mut()
            .next()
            .unwrap()
            .transition_history[0]
            .actor_agent_id = AgentId(3);
        assert_eq!(
            validate_project(&rebound_agent).unwrap_err().code,
            WorkflowErrorCode::CorruptStore
        );

        let mut rebound_principal = project.clone();
        rebound_principal
            .work_items
            .values_mut()
            .next()
            .unwrap()
            .transition_history[0]
            .actor_id = "qa-a".to_owned();
        assert_eq!(
            validate_project(&rebound_principal).unwrap_err().code,
            WorkflowErrorCode::CorruptStore
        );

        let mut manager_swap = project.clone();
        let audit = &mut manager_swap
            .work_items
            .values_mut()
            .next()
            .unwrap()
            .transition_history[0];
        audit.actor_agent_id = AgentId(5);
        audit.actor_id = "pm-b".to_owned();
        assert_eq!(
            validate_project(&manager_swap).unwrap_err().code,
            WorkflowErrorCode::CorruptStore
        );

        let mut rebound_assigner = project.clone();
        rebound_assigner
            .work_items
            .values_mut()
            .next()
            .unwrap()
            .assignments[0]
            .assigned_by = "qa-a".to_owned();
        assert_eq!(
            validate_project(&rebound_assigner).unwrap_err().code,
            WorkflowErrorCode::CorruptStore
        );

        let mut exact_boundary = project.clone();
        exact_boundary.updated_at_unix_ms = 4;
        let work = exact_boundary.work_items.values_mut().next().unwrap();
        work.assignments[0].active = false;
        work.assignments[0].ended_at_unix_ms = Some(4);
        work.assignments.push(AssignmentV1 {
            assignment_id: "assignment-b".to_owned(),
            agent_id: AgentId(2),
            role: CompanyRoleV1::Developer,
            specialties: BTreeSet::from(["rust".to_owned()]),
            profile: profile("developer-v1"),
            organization_generation: 2,
            organization_digest: DIGEST.to_owned(),
            assignment_version: 2,
            delegated_by: None,
            reason_ref: "manager-reassignment".to_owned(),
            active: true,
            assigned_by: "pm-a".to_owned(),
            created_at_unix_ms: 4,
            ended_at_unix_ms: None,
        });
        work.transition_history.push(StateTransitionAuditV1 {
            before: "Assigned".to_owned(),
            after: "Blocked".to_owned(),
            actor_id: "developer-a".to_owned(),
            actor_agent_id: AgentId(2),
            reason_ref: "dependency-blocked".to_owned(),
            occurred_at_unix_ms: 4,
        });
        work.transition_history.push(StateTransitionAuditV1 {
            before: "Blocked".to_owned(),
            after: "Assigned".to_owned(),
            actor_id: "pm-a".to_owned(),
            actor_agent_id: AgentId(1),
            reason_ref: "manager-reassignment".to_owned(),
            occurred_at_unix_ms: 4,
        });
        work.version = 4;
        validate_project(&exact_boundary).unwrap();
        let mut wrong_delegator = exact_boundary.clone();
        let work = wrong_delegator.work_items.values_mut().next().unwrap();
        work.assignments[1].delegated_by = Some(AgentId(1));
        assert_eq!(
            validate_project(&wrong_delegator).unwrap_err().code,
            WorkflowErrorCode::CorruptStore
        );
        exact_boundary
            .work_items
            .values_mut()
            .next()
            .unwrap()
            .assignments[0]
            .ended_at_unix_ms = Some(5);
        assert_eq!(
            validate_project(&exact_boundary).unwrap_err().code,
            WorkflowErrorCode::CorruptStore
        );

        let mut skipped_edge = project;
        let work = skipped_edge.work_items.values_mut().next().unwrap();
        work.state = CompanyWorkStateV1::InProgress;
        work.transition_history[0].after = "InProgress".to_owned();
        assert_eq!(
            validate_project(&skipped_edge).unwrap_err().code,
            WorkflowErrorCode::CorruptStore
        );
    }

    #[test]
    fn string_only_actor_fields_resolve_to_authorized_project_participants() {
        let (_temp, _path, _store, _principal, mut project) = accepted_project_fixture();
        project.updated_at_unix_ms = 5;
        project.decisions.push(DecisionV1 {
            decision_id: "decision-a".to_owned(),
            work_item_id: None,
            choice_ref: "bounded-choice".to_owned(),
            rationale_ref: "bounded-rationale".to_owned(),
            decided_by: "pm-a".to_owned(),
            created_at_unix_ms: 3,
        });
        project.reservations.push(CostReservationV1 {
            reservation_id: "reservation-a".to_owned(),
            work_item_id: None,
            provider: "local".to_owned(),
            reserved_micros: 10,
            committed_micros: None,
            state: CostReservationStateV1::Active,
            created_by: "pm-a".to_owned(),
            created_at_unix_ms: 3,
            updated_at_unix_ms: 3,
        });
        project.reserved_cost_micros = 10;
        project.questions.push(ProjectQuestionV1 {
            question_id: "question-a".to_owned(),
            work_item_id: None,
            owner: AgentId(2),
            question_ref: "bounded-question".to_owned(),
            resolution_ref: Some("bounded-answer".to_owned()),
            created_by: "pm-a".to_owned(),
            resolved_by: Some("developer-a".to_owned()),
            created_at_unix_ms: 3,
            updated_at_unix_ms: 4,
        });
        project.actions.push(ProjectActionV1 {
            action_id: "action-a".to_owned(),
            work_item_id: None,
            owner: AgentId(2),
            action_ref: "bounded-action".to_owned(),
            completed: true,
            created_by: "pm-a".to_owned(),
            completed_by: Some("developer-a".to_owned()),
            resolution_ref: Some("bounded-completion".to_owned()),
            created_at_unix_ms: 3,
            updated_at_unix_ms: 4,
        });
        project.blockers.push(BlockerV1 {
            blocker_id: "blocker-a".to_owned(),
            work_item_id: None,
            cause_ref: "bounded-blocker".to_owned(),
            owner: AgentId(2),
            escalation_target: None,
            state: BlockerStateV1::Open,
            blocker_kind: BlockerKindV1::Operational,
            blocked_from_state: None,
            resolution_ref: None,
            last_actor_id: "developer-a".to_owned(),
            created_at_unix_ms: 3,
            updated_at_unix_ms: 3,
            transition_history: Vec::new(),
        });
        validate_project(&project).unwrap();

        let mut variants = Vec::new();
        let mut decision = project.clone();
        decision.decisions[0].decided_by = "developer-a".to_owned();
        variants.push(decision);
        let mut reservation = project.clone();
        reservation.reservations[0].created_by = "developer-a".to_owned();
        variants.push(reservation);
        let mut question = project.clone();
        question.questions[0].created_by = "qa-a".to_owned();
        variants.push(question);
        let mut action = project.clone();
        action.actions[0].created_by = "qa-a".to_owned();
        variants.push(action);
        let mut resolver = project.clone();
        resolver.questions[0].resolved_by = Some("qa-a".to_owned());
        variants.push(resolver);
        let mut completer = project.clone();
        completer.actions[0].completed_by = Some("qa-a".to_owned());
        variants.push(completer);
        let mut blocker = project;
        blocker.blockers[0].last_actor_id = "qa-a".to_owned();
        variants.push(blocker);
        for variant in variants {
            assert_eq!(
                validate_project(&variant).unwrap_err().code,
                WorkflowErrorCode::CorruptStore
            );
        }
    }

    #[test]
    fn company_schema_bootstrap_is_atomic_and_rejects_orphan_objects() {
        let mut connection = Connection::open_in_memory().unwrap();
        {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            assert_eq!(
                ensure_company_schema_inner(&transaction, true)
                    .unwrap_err()
                    .code,
                WorkflowErrorCode::CorruptStore
            );
        }
        let object_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE 'company_%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(object_count, 0);
        {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            ensure_company_schema(&transaction).unwrap();
            transaction.commit().unwrap();
        }
        let tables = connection
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'company_%'")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<BTreeSet<_>, _>>()
            .unwrap();
        assert_eq!(tables, expected_company_tables());

        let orphan = Connection::open_in_memory().unwrap();
        orphan
            .execute("CREATE TABLE company_entities(tenant_id TEXT NOT NULL)", [])
            .unwrap();
        assert_eq!(
            ensure_company_schema(&orphan).unwrap_err().code,
            WorkflowErrorCode::CorruptStore
        );
        assert_eq!(
            orphan
                .query_row("SELECT COUNT(*) FROM company_entities", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn company_schema_shape_meta_index_and_extra_object_drift_fail_closed() {
        for mutation in [
            "UPDATE company_schema_meta SET schema_version=2",
            "DROP INDEX idx_company_events_tenant_project",
            "CREATE TRIGGER company_unexpected AFTER INSERT ON company_entities BEGIN SELECT 1; END",
        ] {
            let connection = Connection::open_in_memory().unwrap();
            ensure_company_schema(&connection).unwrap();
            connection.execute_batch(mutation).unwrap();
            let error = validate_company_schema(&connection).unwrap_err();
            assert_eq!(error.code, WorkflowErrorCode::CorruptStore);
            assert!(!error.retryable);
        }
        for schema in [
            COMPANY_SCHEMA.replacen("payload_digest TEXT NOT NULL", "payload_digest TEXT", 1),
            COMPANY_SCHEMA.replacen(
                "version INTEGER NOT NULL",
                "version INTEGER NOT NULL DEFAULT 1",
                1,
            ),
        ] {
            let connection = Connection::open_in_memory().unwrap();
            connection.execute_batch(&schema).unwrap();
            let error = validate_company_schema(&connection).unwrap_err();
            assert_eq!(error.code, WorkflowErrorCode::CorruptStore);
            assert!(!error.retryable);
        }
    }

    #[test]
    fn entity_projection_event_and_operation_tamper_fail_after_reopen() {
        for target in ["entity", "projection", "event", "operation"] {
            let (_temp, path, store, _customer, project) = accepted_project_fixture();
            {
                let connection = store.connection.lock().unwrap();
                match target {
                    "entity" => connection
                        .execute(
                            "UPDATE company_entities SET payload_digest=?1 WHERE entity_kind='project'",
                            ["bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"],
                        )
                        .unwrap(),
                    "projection" => connection
                        .execute(
                            "UPDATE company_project_projections SET source_sequence=source_sequence+1",
                            [],
                        )
                        .unwrap(),
                    "event" => connection
                        .execute(
                            "UPDATE company_events SET principal_role='Developer' WHERE event_type='project_created'",
                            [],
                        )
                        .unwrap(),
                    "operation" => connection
                        .execute(
                            "UPDATE company_operations SET response_digest=?1 WHERE operation_id=?2",
                            params!["bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", Uuid::from_u128(10).to_string()],
                        )
                        .unwrap(),
                    _ => unreachable!(),
                };
            }
            drop(store);
            let reopened = WorkflowStore::open(path).unwrap();
            let error = match target {
                "entity" => reopened
                    .company_project(&project.tenant_id, &project.project_id)
                    .unwrap_err(),
                "projection" => reopened
                    .company_project_projection(&project.tenant_id, &project.project_id)
                    .unwrap_err(),
                "event" | "operation" => {
                    reopened.rebuild_company_project_projections().unwrap_err()
                }
                _ => unreachable!(),
            };
            assert_eq!(error.code, WorkflowErrorCode::CorruptStore);
            assert!(!error.retryable);
        }
    }

    #[test]
    fn agreement_and_project_acceptance_rolls_back_atomically_at_failpoint() {
        let temp = TempDir::new().unwrap();
        let store = WorkflowStore::open(temp.path().join("workflow.sqlite")).unwrap();
        let tenant_id = TenantId::parse("tenant-a").unwrap();
        let principal = AuthenticatedCompanyPrincipalV1 {
            schema_version: COMPANY_DOMAIN_SCHEMA_VERSION,
            tenant_id: tenant_id.clone(),
            principal_id: "customer-a".to_owned(),
            kind: CompanyPrincipalKindV1::Customer,
            role: CompanyRoleV1::Customer,
            customer_id: Some("customer-a".to_owned()),
            agent_id: None,
            authority_generation: 1,
            authority_digest: DIGEST.to_owned(),
        };
        let binding = ProposalBindingV1 {
            scope: "bounded-scope".to_owned(),
            deliverables: vec!["artifact".to_owned()],
            exclusions: Vec::new(),
            acceptance_criteria: vec!["qa".to_owned()],
            assumptions: Vec::new(),
            cost_ceiling_micros: 100,
            provider_cost_ceilings_micros: BTreeMap::from([("local".to_owned(), 100)]),
            governance: ProposalGovernanceV1 {
                owner: AgentId(1),
                participants: vec![ParticipantBindingV1 {
                    agent_id: AgentId(1),
                    principal_id: "pm-a".to_owned(),
                    role: CompanyRoleV1::ProjectManager,
                    specialties: BTreeSet::from(["coordination".to_owned()]),
                    reports_to: None,
                    profile: profile("pm-v1"),
                }],
                project_profile: profile("project-v1"),
            },
            expires_at_unix_ms: 100,
        };
        let proposal_digest =
            canonical_sha256("sentinel.workflow.proposal-binding.v1", &binding).unwrap();
        let request = CustomerRequestV1 {
            schema_version: COMPANY_DOMAIN_SCHEMA_VERSION,
            request_id: "request-a".to_owned(),
            tenant_id: tenant_id.clone(),
            customer_id: "customer-a".to_owned(),
            summary_ref: "summary".to_owned(),
            desired_outcome: "outcome".to_owned(),
            constraints: Vec::new(),
            clarifications: Vec::new(),
            feedback: Vec::new(),
            state: CustomerRequestStateV1::Proposed,
            version: 1,
            proposal_ids: vec!["proposal-a".to_owned()],
            created_at_unix_ms: 1,
            updated_at_unix_ms: 1,
        };
        let proposal = ProposalV1 {
            schema_version: COMPANY_DOMAIN_SCHEMA_VERSION,
            proposal_id: "proposal-a".to_owned(),
            tenant_id: tenant_id.clone(),
            request_id: request.request_id.clone(),
            generation: 1,
            binding,
            proposal_digest: proposal_digest.clone(),
            created_by: "sales-a".to_owned(),
            created_at_unix_ms: 1,
        };
        {
            let mut connection = store.connection.lock().unwrap();
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            put_entity(
                &transaction,
                &tenant_id,
                "request",
                &request.request_id,
                request.version,
                &request,
            )
            .unwrap();
            put_entity(
                &transaction,
                &tenant_id,
                "proposal",
                &proposal.proposal_id,
                1,
                &proposal,
            )
            .unwrap();
            transaction.commit().unwrap();
        }
        let error = store
            .apply_company_command_with_accept_failpoint(
                &principal,
                Uuid::from_u128(1),
                &CompanyWorkflowCommandV1::AcceptProposal {
                    request_id: request.request_id.clone(),
                    expected_version: 1,
                    proposal_id: proposal.proposal_id,
                    proposal_digest,
                },
                2,
            )
            .unwrap_err();
        assert_eq!(error.code, WorkflowErrorCode::PersistenceFailure);
        assert_eq!(
            store
                .company_customer_request(&tenant_id, &request.request_id)
                .unwrap()
                .unwrap(),
            request
        );
        let connection = store.connection.lock().unwrap();
        let agreements: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM company_entities WHERE entity_kind IN ('agreement','project')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(agreements, 0);
    }
}
