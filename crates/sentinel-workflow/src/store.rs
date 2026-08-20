use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::de::DeserializeOwned;
use serde::Serialize;
use uuid::Uuid;

use crate::digest::{canonical_sha256, constant_time_eq, validate_sha256};
use crate::model::{execution_subject_digest, step_digest};
use crate::{
    ExecutionEvidenceReadbackV1, ExecutionPlanV1, ExecutionReconcileState, GateEvidenceReadbackV1,
    PendingCompletionEvidenceV1, PendingExecutionV1, PendingGateEvidenceV1,
    RuntimeAuthoritySnapshotV1, WorkItemExecutionV1, WorkItemState, WorkflowError,
    WorkflowErrorCode, WORKFLOW_SCHEMA_VERSION,
};

pub const WORKFLOW_STORE_SCHEMA_VERSION: u32 = 2;
const MAX_NOT_FOUND_RECONCILES: u16 = 3;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS workflow_schema_meta (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    schema_version INTEGER NOT NULL
);
INSERT OR IGNORE INTO workflow_schema_meta (singleton, schema_version) VALUES (1, 2);
CREATE TABLE IF NOT EXISTS workflow_work_items (
    tenant_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    work_item_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    payload BLOB NOT NULL,
    PRIMARY KEY (tenant_id, project_id, work_item_id)
);
CREATE TABLE IF NOT EXISTS workflow_operations (
    operation_namespace TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    response BLOB NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (operation_namespace, operation_id)
);
CREATE TABLE IF NOT EXISTS workflow_execution_outbox (
    invocation_id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    work_item_id TEXT NOT NULL,
    plan_digest TEXT NOT NULL,
    step_ordinal INTEGER NOT NULL,
    state TEXT NOT NULL,
    request BLOB NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    updated_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_workflow_execution_pending
    ON workflow_execution_outbox(state, updated_at_ms, invocation_id);
CREATE TABLE IF NOT EXISTS workflow_completion_outbox (
    request_id TEXT PRIMARY KEY,
    invocation_id TEXT NOT NULL UNIQUE,
    tenant_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    work_item_id TEXT NOT NULL,
    state TEXT NOT NULL,
    request BLOB NOT NULL,
    evidence BLOB,
    attempts INTEGER NOT NULL DEFAULT 0,
    updated_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_workflow_completion_pending
    ON workflow_completion_outbox(state, updated_at_ms, request_id);
CREATE TABLE IF NOT EXISTS workflow_gate_outbox (
    request_id TEXT PRIMARY KEY,
    plan_id TEXT NOT NULL UNIQUE,
    tenant_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    work_item_id TEXT NOT NULL,
    state TEXT NOT NULL,
    request BLOB NOT NULL,
    evidence BLOB,
    attempts INTEGER NOT NULL DEFAULT 0,
    updated_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_workflow_gate_pending
    ON workflow_gate_outbox(state, updated_at_ms, request_id);
CREATE TABLE IF NOT EXISTS workflow_audit_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE,
    tenant_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    work_item_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    before_state TEXT,
    after_state TEXT NOT NULL,
    authority_digest TEXT NOT NULL,
    payload_digest TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);
"#;

const MIGRATE_V1_TO_V2: &str = r#"
CREATE TABLE workflow_gate_outbox (
    request_id TEXT PRIMARY KEY,
    plan_id TEXT NOT NULL UNIQUE,
    tenant_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    work_item_id TEXT NOT NULL,
    state TEXT NOT NULL,
    request BLOB NOT NULL,
    evidence BLOB,
    attempts INTEGER NOT NULL DEFAULT 0,
    updated_at_ms INTEGER NOT NULL
);
CREATE INDEX idx_workflow_gate_pending
    ON workflow_gate_outbox(state, updated_at_ms, request_id);
UPDATE workflow_schema_meta SET schema_version = 2 WHERE singleton = 1;
"#;

#[derive(Debug)]
pub struct WorkflowStore {
    pub(crate) connection: Mutex<Connection>,
}

struct ExecutionRow {
    invocation_id: String,
    tenant_id: String,
    project_id: String,
    work_item_id: String,
    plan_digest: String,
    step_ordinal: i64,
    state: String,
    request: Vec<u8>,
    attempts: i64,
    updated_at_ms: i64,
}

struct CompletionRow {
    request_id: String,
    invocation_id: String,
    tenant_id: String,
    project_id: String,
    work_item_id: String,
    state: String,
    request: Vec<u8>,
    evidence: Option<Vec<u8>>,
    attempts: i64,
    updated_at_ms: i64,
}

struct GateRow {
    request_id: String,
    plan_id: String,
    tenant_id: String,
    project_id: String,
    work_item_id: String,
    state: String,
    request: Vec<u8>,
    evidence: Option<Vec<u8>>,
    attempts: i64,
    updated_at_ms: i64,
}

impl WorkflowStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, WorkflowError> {
        Self::open_with_failure_injection(path, false, false)
    }

    fn open_with_failure_injection(
        path: impl AsRef<Path>,
        fail_after_bootstrap: bool,
        fail_after_migration: bool,
    ) -> Result<Self, WorkflowError> {
        let mut connection = Connection::open(path).map_err(map_sqlite_error)?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(map_sqlite_error)?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(map_sqlite_error)?;
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(map_sqlite_error)?;

        let transaction = immediate(&mut connection)?;
        let has_schema: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='workflow_schema_meta')",
                [],
                |row| row.get(0),
            )
            .map_err(map_sqlite_error)?;
        if !has_schema {
            let orphan_count = workflow_object_count(&transaction)?;
            if orphan_count != 0 {
                return Err(corrupt_store());
            }
            transaction
                .execute_batch(SCHEMA)
                .map_err(map_sqlite_error)?;
            if fail_after_bootstrap {
                return Err(corrupt_store());
            }
            validate_store_schema(&transaction, WORKFLOW_STORE_SCHEMA_VERSION)?;
        } else {
            let version = read_schema_version(&transaction)?;
            match version {
                1 => {
                    validate_store_schema(&transaction, 1)?;
                    transaction
                        .execute_batch(MIGRATE_V1_TO_V2)
                        .map_err(map_sqlite_error)?;
                    if fail_after_migration {
                        return Err(corrupt_store());
                    }
                    validate_store_schema(&transaction, WORKFLOW_STORE_SCHEMA_VERSION)?;
                }
                WORKFLOW_STORE_SCHEMA_VERSION => {
                    validate_store_schema(&transaction, WORKFLOW_STORE_SCHEMA_VERSION)?;
                }
                _ => {
                    return Err(corrupt_store());
                }
            }
        }
        crate::domain_store::ensure_company_schema(&transaction)?;
        transaction.commit().map_err(map_sqlite_error)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn admit_plan(
        &self,
        plan: &ExecutionPlanV1,
        authority: &RuntimeAuthoritySnapshotV1,
        now_ms: u64,
    ) -> Result<(bool, WorkItemExecutionV1), WorkflowError> {
        plan.validate_canonical()?;
        authority.validate()?;
        if !plan.authority_matches(authority) {
            return Err(authority_conflict());
        }
        let authority_digest = authority.canonical_digest()?;
        let namespace = operation_namespace(plan);
        let operation_id = plan.plan_id.to_string();
        let mut connection = self.lock()?;
        let transaction = immediate(&mut connection)?;
        if let Some((digest, response, created_at_ms)) =
            read_operation(&transaction, &namespace, &operation_id)?
        {
            if !constant_time_eq(&digest, &plan.request_digest) {
                return Err(idempotency_conflict());
            }
            let response: WorkItemExecutionV1 = decode(&response)?;
            validate_operation_response(&response, plan, stored_u64(created_at_ms)?)?;
            let current = read_work_item(
                &transaction,
                &plan.tenant_id,
                &plan.project_id,
                &plan.work_item_id,
            )?
            .ok_or_else(corrupt_store)?;
            if current.plan != *plan {
                return Err(corrupt_store());
            }
            return Ok((true, response));
        }
        if let Some(existing) = read_work_item(
            &transaction,
            &plan.tenant_id,
            &plan.project_id,
            &plan.work_item_id,
        )? {
            if existing.plan == *plan {
                return Err(corrupt_store());
            }
            return Err(WorkflowError::new(
                WorkflowErrorCode::VersionConflict,
                false,
                "work item already has a different execution plan",
            ));
        }
        plan.validate_at(now_ms)?;
        let first_step = plan.steps.first().cloned().ok_or_else(|| {
            WorkflowError::new(WorkflowErrorCode::InvalidInput, false, "plan has no step")
        })?;
        let pending = PendingExecutionV1 {
            schema_version: WORKFLOW_SCHEMA_VERSION,
            plan_id: plan.plan_id,
            plan_digest: plan.request_digest.clone(),
            step: first_step,
            authority_snapshot_digest: authority_digest.clone(),
            state: ExecutionReconcileState::NotFound,
            attempts: 0,
            created_at_unix_ms: now_ms,
            updated_at_unix_ms: now_ms,
        };
        let work_item = WorkItemExecutionV1 {
            schema_version: WORKFLOW_SCHEMA_VERSION,
            tenant_id: plan.tenant_id.clone(),
            project_id: plan.project_id.clone(),
            work_item_id: plan.work_item_id.clone(),
            agent_id: plan.agent_id,
            state: WorkItemState::Claimed,
            version: 1,
            plan: plan.clone(),
            next_step_ordinal: 0,
            terminal_execution_evidence: None,
            gate_evidence: None,
            blocker_code: None,
            updated_at_unix_ms: now_ms,
        };
        put_work_item(&transaction, &work_item)?;
        insert_execution(&transaction, plan, &pending)?;
        append_audit(
            &transaction,
            &work_item,
            "execution_plan_admitted",
            None,
            WorkItemState::Claimed,
            &authority_digest,
            &plan.request_digest,
            now_ms,
        )?;
        insert_operation(
            &transaction,
            &namespace,
            &operation_id,
            &plan.request_digest,
            &work_item,
            now_ms,
        )?;
        transaction.commit().map_err(map_sqlite_error)?;
        Ok((false, work_item))
    }

    pub fn reserve_operation_timestamp(
        &self,
        namespace: &str,
        operation_id: Uuid,
        request_digest: &str,
        now_ms: u64,
    ) -> Result<(bool, u64), WorkflowError> {
        if namespace.is_empty()
            || namespace.len() > 256
            || !namespace.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.')
            })
            || operation_id.is_nil()
            || !validate_sha256(request_digest)
        {
            return Err(WorkflowError::new(
                WorkflowErrorCode::InvalidInput,
                false,
                "operation timestamp reservation is invalid",
            ));
        }
        let mut connection = self.lock()?;
        let transaction = immediate(&mut connection)?;
        if let Some((stored_digest, response, stored_at_ms)) =
            read_operation(&transaction, namespace, &operation_id.to_string())?
        {
            if !constant_time_eq(&stored_digest, request_digest) {
                return Err(idempotency_conflict());
            }
            let stored_at_ms = stored_u64(stored_at_ms)?;
            let response_at_ms: u64 = decode(&response)?;
            if response_at_ms != stored_at_ms || stored_at_ms > now_ms {
                return Err(corrupt_store());
            }
            transaction.commit().map_err(map_sqlite_error)?;
            return Ok((true, stored_at_ms));
        }
        insert_operation(
            &transaction,
            namespace,
            &operation_id.to_string(),
            request_digest,
            &now_ms,
            now_ms,
        )?;
        transaction.commit().map_err(map_sqlite_error)?;
        Ok((false, now_ms))
    }

    pub fn work_item(
        &self,
        tenant_id: &crate::TenantId,
        project_id: &crate::ProjectId,
        work_item_id: &crate::WorkItemId,
    ) -> Result<Option<WorkItemExecutionV1>, WorkflowError> {
        tenant_id.validate()?;
        project_id.validate()?;
        work_item_id.validate()?;
        let connection = self.lock()?;
        read_work_item(&connection, tenant_id, project_id, work_item_id)
    }

    /// Returns a stable keyset page for rebuilding the company-domain view
    /// from durable execution state after a crash between the two commits.
    pub fn workflow_items_after(
        &self,
        after: Option<(&crate::TenantId, &crate::ProjectId, &crate::WorkItemId)>,
        limit: usize,
    ) -> Result<Vec<WorkItemExecutionV1>, WorkflowError> {
        if let Some((tenant_id, project_id, work_item_id)) = after {
            tenant_id.validate()?;
            project_id.validate()?;
            work_item_id.validate()?;
        }
        let connection = self.lock()?;
        let mut statement = if after.is_some() {
            connection
                .prepare(
                    "SELECT tenant_id,project_id,work_item_id FROM workflow_work_items \
                     WHERE (tenant_id,project_id,work_item_id) > (?1,?2,?3) \
                     ORDER BY tenant_id,project_id,work_item_id LIMIT ?4",
                )
                .map_err(map_sqlite_error)?
        } else {
            connection
                .prepare(
                    "SELECT tenant_id,project_id,work_item_id FROM workflow_work_items \
                     ORDER BY tenant_id,project_id,work_item_id LIMIT ?1",
                )
                .map_err(map_sqlite_error)?
        };
        let bounded_limit = limit.clamp(1, 100) as i64;
        let keys = if let Some((tenant_id, project_id, work_item_id)) = after {
            statement
                .query_map(
                    params![tenant_id.0, project_id.0, work_item_id.0, bounded_limit],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .map_err(map_sqlite_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(map_sqlite_error)?
        } else {
            statement
                .query_map([bounded_limit], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map_err(map_sqlite_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(map_sqlite_error)?
        };
        drop(statement);
        keys.into_iter()
            .map(|(tenant, project, work_item)| {
                read_work_item(
                    &connection,
                    &crate::TenantId(tenant),
                    &crate::ProjectId(project),
                    &crate::WorkItemId(work_item),
                )?
                .ok_or_else(corrupt_store)
            })
            .collect()
    }

    pub fn pending_executions(
        &self,
        limit: usize,
    ) -> Result<Vec<PendingExecutionV1>, WorkflowError> {
        self.validate_state_domain(
            "SELECT COUNT(*) FROM workflow_execution_outbox WHERE state NOT IN ('not_found','reserved','executing','awaiting_evidence','failed','cancelled','timed_out','unknown_outcome')",
        )?;
        self.read_pending(
            "SELECT request FROM workflow_execution_outbox WHERE state IN ('not_found','reserved','executing') ORDER BY updated_at_ms, invocation_id LIMIT ?1",
            limit,
        )
    }

    pub fn pending_completion_evidence(
        &self,
        limit: usize,
    ) -> Result<Vec<PendingCompletionEvidenceV1>, WorkflowError> {
        self.validate_state_domain(
            "SELECT COUNT(*) FROM workflow_completion_outbox WHERE state NOT IN ('pending','completed')",
        )?;
        self.read_pending(
            "SELECT request FROM workflow_completion_outbox WHERE state='pending' ORDER BY updated_at_ms, request_id LIMIT ?1",
            limit,
        )
    }

    pub fn pending_gate_evidence(
        &self,
        limit: usize,
    ) -> Result<Vec<PendingGateEvidenceV1>, WorkflowError> {
        self.validate_state_domain(
            "SELECT COUNT(*) FROM workflow_gate_outbox WHERE state NOT IN ('pending','completed')",
        )?;
        self.read_pending(
            "SELECT request FROM workflow_gate_outbox WHERE state='pending' ORDER BY updated_at_ms, request_id LIMIT ?1",
            limit,
        )
    }

    pub(crate) fn execution_work_item(
        &self,
        request: &PendingExecutionV1,
    ) -> Result<WorkItemExecutionV1, WorkflowError> {
        let connection = self.lock()?;
        let work_item = read_work_item_by_invocation(&connection, request.step.invocation_id)?
            .ok_or_else(corrupt_store)?;
        validate_execution_request(&connection, &work_item, request)?;
        Ok(work_item)
    }

    /// Returns the exact durable execution context after validating that the
    /// supplied outbox request is still the current stored request.
    pub fn execution_context(
        &self,
        request: &PendingExecutionV1,
    ) -> Result<WorkItemExecutionV1, WorkflowError> {
        self.execution_work_item(request)
    }

    pub(crate) fn record_execution_observation(
        &self,
        request: &PendingExecutionV1,
        state: ExecutionReconcileState,
        authority: &RuntimeAuthoritySnapshotV1,
        now_ms: u64,
    ) -> Result<WorkItemExecutionV1, WorkflowError> {
        authority.validate()?;
        let mut connection = self.lock()?;
        let transaction = immediate(&mut connection)?;
        let mut work_item = read_work_item_by_invocation(&transaction, request.step.invocation_id)?
            .ok_or_else(corrupt_store)?;
        validate_execution_request(&transaction, &work_item, request)?;
        validate_monotonic_time(
            now_ms,
            request.updated_at_unix_ms,
            work_item.updated_at_unix_ms,
        )?;
        if !work_item.plan.authority_matches(authority)
            || request.authority_snapshot_digest != authority.canonical_digest()?
        {
            return Err(authority_conflict());
        }
        let state = if state == ExecutionReconcileState::NotFound
            && request.attempts.saturating_add(1) >= MAX_NOT_FOUND_RECONCILES
        {
            ExecutionReconcileState::UnknownOutcome
        } else {
            state
        };
        let previous = work_item.state;
        let mut persisted = request.clone();
        persisted.state = state;
        persisted.attempts = persisted.attempts.saturating_add(1);
        persisted.updated_at_unix_ms = now_ms;
        let (database_state, blocker) = match state {
            ExecutionReconcileState::NotFound => ("not_found", None),
            ExecutionReconcileState::Reserved => {
                work_item.state = WorkItemState::InProgress;
                ("reserved", None)
            }
            ExecutionReconcileState::Executing => {
                work_item.state = WorkItemState::InProgress;
                ("executing", None)
            }
            ExecutionReconcileState::Succeeded => ("awaiting_evidence", None),
            ExecutionReconcileState::Failed => {
                work_item.state = WorkItemState::Blocked;
                ("failed", Some("execution_failed"))
            }
            ExecutionReconcileState::Cancelled => {
                work_item.state = WorkItemState::Cancelled;
                ("cancelled", Some("execution_cancelled"))
            }
            ExecutionReconcileState::TimedOut => {
                work_item.state = WorkItemState::Blocked;
                ("timed_out", Some("execution_timed_out"))
            }
            ExecutionReconcileState::UnknownOutcome => {
                work_item.state = WorkItemState::Blocked;
                ("unknown_outcome", Some("execution_unknown_outcome"))
            }
        };
        work_item.blocker_code = blocker.map(str::to_owned);
        work_item.version = work_item.version.saturating_add(1);
        work_item.updated_at_unix_ms = now_ms;
        let expected_database_state = execution_database_state(request.state);
        let updated = transaction
            .execute(
                "UPDATE workflow_execution_outbox SET state=?1, request=?2, attempts=?3, updated_at_ms=?4 WHERE invocation_id=?5 AND state=?6",
                params![database_state, encode(&persisted)?, i64::from(persisted.attempts), sql_u64(now_ms)?, request.step.invocation_id.to_string(), expected_database_state],
            )
            .map_err(map_sqlite_error)?;
        require_one_row(updated)?;
        if state == ExecutionReconcileState::Succeeded {
            insert_completion_request(&transaction, &work_item, request, authority, now_ms)?;
        }
        put_work_item(&transaction, &work_item)?;
        append_audit(
            &transaction,
            &work_item,
            "execution_reconciled",
            Some(previous),
            work_item.state,
            &authority.canonical_digest()?,
            &step_digest(&request.step)?,
            now_ms,
        )?;
        transaction.commit().map_err(map_sqlite_error)?;
        Ok(work_item)
    }

    pub(crate) fn completion_work_item(
        &self,
        request: &PendingCompletionEvidenceV1,
    ) -> Result<(WorkItemExecutionV1, bool), WorkflowError> {
        let connection = self.lock()?;
        let work_item = read_work_item_by_invocation(&connection, request.invocation_id)?
            .ok_or_else(corrupt_store)?;
        let completed = validate_completion_request(&connection, &work_item, request)?;
        Ok((work_item, completed))
    }

    /// Returns the exact durable completion context and whether its evidence
    /// has already been adopted.
    pub fn completion_context(
        &self,
        request: &PendingCompletionEvidenceV1,
    ) -> Result<(WorkItemExecutionV1, bool), WorkflowError> {
        self.completion_work_item(request)
    }

    pub(crate) fn record_terminal_evidence(
        &self,
        request: &PendingCompletionEvidenceV1,
        evidence: ExecutionEvidenceReadbackV1,
        authority: &RuntimeAuthoritySnapshotV1,
        now_ms: u64,
    ) -> Result<WorkItemExecutionV1, WorkflowError> {
        authority.validate()?;
        let mut connection = self.lock()?;
        let transaction = immediate(&mut connection)?;
        let mut work_item = read_work_item_by_invocation(&transaction, request.invocation_id)?
            .ok_or_else(corrupt_store)?;
        let _ = validate_completion_request(&transaction, &work_item, request)?;
        validate_monotonic_time(
            now_ms,
            request.created_at_unix_ms,
            work_item.updated_at_unix_ms,
        )?;
        if !work_item.plan.authority_matches(authority)
            || request.authority_snapshot_digest != authority.canonical_digest()?
        {
            return Err(authority_conflict());
        }
        let step_deadline = work_item
            .plan
            .steps
            .iter()
            .find(|step| step.step_id == request.step_id)
            .ok_or_else(corrupt_store)?
            .deadline_unix_ms;
        validate_execution_evidence(
            request,
            &evidence,
            now_ms,
            step_deadline,
            work_item.plan.deadline_unix_ms,
        )?;
        let existing: (String, Option<Vec<u8>>) = transaction
            .query_row(
                "SELECT state,evidence FROM workflow_completion_outbox WHERE request_id=?1",
                [&request.request_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(map_sqlite_error)?;
        if existing.0 == "completed" {
            let bytes = existing.1.ok_or_else(corrupt_store)?;
            let existing: ExecutionEvidenceReadbackV1 = decode(&bytes)?;
            if existing != evidence {
                return Err(idempotency_conflict());
            }
            return Ok(work_item);
        }
        if existing.0 != "pending" || existing.1.is_some() {
            return Err(corrupt_store());
        }
        let updated = transaction
            .execute(
                "UPDATE workflow_completion_outbox SET state='completed', evidence=?1, updated_at_ms=?2 WHERE request_id=?3 AND state='pending'",
                params![encode(&evidence)?, sql_u64(now_ms)?, request.request_id],
            )
            .map_err(map_sqlite_error)?;
        require_one_row(updated)?;
        let previous = work_item.state;
        let ordinal = usize::from(work_item.next_step_ordinal);
        if ordinal + 1 < work_item.plan.steps.len() {
            work_item.next_step_ordinal = work_item.next_step_ordinal.saturating_add(1);
            work_item.state = WorkItemState::InProgress;
            let next = work_item.plan.steps[ordinal + 1].clone();
            let pending = PendingExecutionV1 {
                schema_version: WORKFLOW_SCHEMA_VERSION,
                plan_id: work_item.plan.plan_id,
                plan_digest: work_item.plan.request_digest.clone(),
                step: next,
                authority_snapshot_digest: authority.canonical_digest()?,
                state: ExecutionReconcileState::NotFound,
                attempts: 0,
                created_at_unix_ms: now_ms,
                updated_at_unix_ms: now_ms,
            };
            insert_execution(&transaction, &work_item.plan, &pending)?;
        } else {
            work_item.terminal_execution_evidence = Some(evidence.clone());
            work_item.state = WorkItemState::InReview;
            let gate_request = build_gate_request(&work_item, authority, &evidence, now_ms)?;
            insert_gate_request(&transaction, &work_item, &gate_request, now_ms)?;
        }
        work_item.version = work_item.version.saturating_add(1);
        work_item.updated_at_unix_ms = now_ms;
        put_work_item(&transaction, &work_item)?;
        append_audit(
            &transaction,
            &work_item,
            "terminal_execution_evidence_recorded",
            Some(previous),
            work_item.state,
            &authority.canonical_digest()?,
            &evidence.output_bundle_digest,
            now_ms,
        )?;
        transaction.commit().map_err(map_sqlite_error)?;
        Ok(work_item)
    }

    pub(crate) fn gate_work_item(
        &self,
        request: &PendingGateEvidenceV1,
    ) -> Result<(WorkItemExecutionV1, bool), WorkflowError> {
        let connection = self.lock()?;
        let work_item =
            read_work_item_by_plan(&connection, request.plan_id)?.ok_or_else(corrupt_store)?;
        let completed = validate_gate_request(&connection, &work_item, request)?;
        Ok((work_item, completed))
    }

    /// Returns the exact durable work-item gate context and whether its
    /// independent evidence has already been adopted.
    pub fn gate_context(
        &self,
        request: &PendingGateEvidenceV1,
    ) -> Result<(WorkItemExecutionV1, bool), WorkflowError> {
        self.gate_work_item(request)
    }

    pub(crate) fn record_gate_evidence(
        &self,
        request: &PendingGateEvidenceV1,
        evidence: GateEvidenceReadbackV1,
        authority: &RuntimeAuthoritySnapshotV1,
        now_ms: u64,
    ) -> Result<WorkItemExecutionV1, WorkflowError> {
        authority.validate()?;
        let mut connection = self.lock()?;
        let transaction = immediate(&mut connection)?;
        let mut work_item =
            read_work_item_by_plan(&transaction, request.plan_id)?.ok_or_else(corrupt_store)?;
        let _ = validate_gate_request(&transaction, &work_item, request)?;
        validate_monotonic_time(
            now_ms,
            request.created_at_unix_ms,
            work_item.updated_at_unix_ms,
        )?;
        if !work_item.plan.authority_matches(authority)
            || request.authority_snapshot_digest != authority.canonical_digest()?
        {
            return Err(authority_conflict());
        }
        validate_gate_evidence(request, &evidence, now_ms, work_item.plan.deadline_unix_ms)?;
        let existing: (String, Option<Vec<u8>>) = transaction
            .query_row(
                "SELECT state,evidence FROM workflow_gate_outbox WHERE request_id=?1",
                [&request.request_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(map_sqlite_error)?;
        if existing.0 == "completed" {
            let bytes = existing.1.ok_or_else(corrupt_store)?;
            let existing: GateEvidenceReadbackV1 = decode(&bytes)?;
            if existing != evidence {
                return Err(idempotency_conflict());
            }
            return Ok(work_item);
        }
        if existing.0 != "pending" || existing.1.is_some() {
            return Err(corrupt_store());
        }
        let updated = transaction
            .execute(
                "UPDATE workflow_gate_outbox SET state='completed', evidence=?1, updated_at_ms=?2 WHERE request_id=?3 AND state='pending'",
                params![encode(&evidence)?, sql_u64(now_ms)?, request.request_id],
            )
            .map_err(map_sqlite_error)?;
        require_one_row(updated)?;
        let previous = work_item.state;
        if work_item.state != WorkItemState::InReview
            || work_item.terminal_execution_evidence.is_none()
            || !evidence.passed
        {
            return Err(WorkflowError::new(
                WorkflowErrorCode::InvalidTransition,
                false,
                "work item cannot become done without terminal execution and independent gate evidence",
            ));
        }
        work_item.gate_evidence = Some(evidence.clone());
        work_item.state = WorkItemState::Done;
        work_item.version = work_item.version.saturating_add(1);
        work_item.updated_at_unix_ms = now_ms;
        put_work_item(&transaction, &work_item)?;
        append_audit(
            &transaction,
            &work_item,
            "independent_work_item_gate_recorded",
            Some(previous),
            WorkItemState::Done,
            &authority.canonical_digest()?,
            &evidence.subject_digest,
            now_ms,
        )?;
        transaction.commit().map_err(map_sqlite_error)?;
        Ok(work_item)
    }

    fn read_pending<T: DeserializeOwned + StoredSchema>(
        &self,
        sql: &str,
        limit: usize,
    ) -> Result<Vec<T>, WorkflowError> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(sql).map_err(map_sqlite_error)?;
        let rows = statement
            .query_map([limit.clamp(1, 100) as i64], |row| row.get::<_, Vec<u8>>(0))
            .map_err(map_sqlite_error)?;
        let mut values = Vec::new();
        for row in rows {
            let value: T = decode(&row.map_err(map_sqlite_error)?)?;
            if value.schema_version() != WORKFLOW_SCHEMA_VERSION {
                return Err(corrupt_store());
            }
            values.push(value);
        }
        Ok(values)
    }

    fn validate_state_domain(&self, sql: &str) -> Result<(), WorkflowError> {
        let connection = self.lock()?;
        let invalid: i64 = connection
            .query_row(sql, [], |row| row.get(0))
            .map_err(map_sqlite_error)?;
        if invalid == 0 {
            Ok(())
        } else {
            Err(corrupt_store())
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, WorkflowError> {
        self.connection
            .lock()
            .map_err(|_| WorkflowError::persistence())
    }
}

fn build_gate_request(
    work_item: &WorkItemExecutionV1,
    authority: &RuntimeAuthoritySnapshotV1,
    evidence: &ExecutionEvidenceReadbackV1,
    now_ms: u64,
) -> Result<PendingGateEvidenceV1, WorkflowError> {
    let expectation = work_item
        .plan
        .steps
        .last()
        .ok_or_else(|| {
            WorkflowError::new(WorkflowErrorCode::InvalidInput, false, "plan has no gate")
        })?
        .gate_expectation
        .clone();
    let subject_digest = execution_subject_digest(&work_item.plan, evidence)?;
    let required_checks_digest = canonical_sha256(
        "sentinel.workflow.work-item-gate-checks.v1",
        &expectation.required_checks,
    )?;
    let request_id = gate_request_identity(
        work_item.plan.plan_id,
        &work_item.plan.request_digest,
        &evidence.receipt_id,
    )?;
    let mut request = PendingGateEvidenceV1 {
        schema_version: WORKFLOW_SCHEMA_VERSION,
        request_id,
        plan_id: work_item.plan.plan_id,
        plan_digest: work_item.plan.request_digest.clone(),
        execution_receipt_id: evidence.receipt_id.clone(),
        subject_digest,
        required_checks_digest,
        expectation,
        authority_snapshot_digest: authority.canonical_digest()?,
        request_digest: String::new(),
        created_at_unix_ms: now_ms,
    };
    request.request_digest = canonical_sha256("sentinel.workflow.gate-request.v1", &request)?;
    Ok(request)
}

fn insert_completion_request(
    tx: &Transaction<'_>,
    work_item: &WorkItemExecutionV1,
    execution: &PendingExecutionV1,
    authority: &RuntimeAuthoritySnapshotV1,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    let request_id = completion_request_identity(
        work_item.plan.plan_id,
        execution.step.step_id,
        execution.step.invocation_id,
        &work_item.plan.request_digest,
    )?;
    let mut request = PendingCompletionEvidenceV1 {
        schema_version: WORKFLOW_SCHEMA_VERSION,
        request_id,
        plan_id: work_item.plan.plan_id,
        plan_digest: work_item.plan.request_digest.clone(),
        step_id: execution.step.step_id,
        invocation_id: execution.step.invocation_id,
        step_digest: step_digest(&execution.step)?,
        authority_snapshot_digest: authority.canonical_digest()?,
        request_digest: String::new(),
        created_at_unix_ms: now_ms,
    };
    request.request_digest = canonical_sha256("sentinel.workflow.completion-request.v1", &request)?;
    let existing: Option<Vec<u8>> = tx
        .query_row(
            "SELECT request FROM workflow_completion_outbox WHERE invocation_id=?1",
            [request.invocation_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(map_sqlite_error)?;
    if let Some(existing) = existing {
        let existing: PendingCompletionEvidenceV1 = decode(&existing)?;
        if existing == request {
            return Ok(());
        }
        return Err(corrupt_store());
    }
    let inserted = tx
        .execute(
            "INSERT INTO workflow_completion_outbox (request_id, invocation_id, tenant_id, project_id, work_item_id, state, request, updated_at_ms) VALUES (?1,?2,?3,?4,?5,'pending',?6,?7)",
            params![request.request_id, request.invocation_id.to_string(), work_item.tenant_id.0, work_item.project_id.0, work_item.work_item_id.0, encode(&request)?, sql_u64(now_ms)?],
        )
        .map_err(map_sqlite_error)?;
    require_one_row(inserted)?;
    Ok(())
}

fn insert_gate_request(
    tx: &Transaction<'_>,
    work_item: &WorkItemExecutionV1,
    request: &PendingGateEvidenceV1,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    let existing: Option<Vec<u8>> = tx
        .query_row(
            "SELECT request FROM workflow_gate_outbox WHERE plan_id=?1",
            [request.plan_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(map_sqlite_error)?;
    if let Some(existing) = existing {
        let existing: PendingGateEvidenceV1 = decode(&existing)?;
        if existing == *request {
            return Ok(());
        }
        return Err(corrupt_store());
    }
    let inserted = tx
        .execute(
            "INSERT INTO workflow_gate_outbox (request_id, plan_id, tenant_id, project_id, work_item_id, state, request, updated_at_ms) VALUES (?1,?2,?3,?4,?5,'pending',?6,?7)",
            params![request.request_id, request.plan_id.to_string(), work_item.tenant_id.0, work_item.project_id.0, work_item.work_item_id.0, encode(request)?, sql_u64(now_ms)?],
        )
        .map_err(map_sqlite_error)?;
    require_one_row(inserted)?;
    Ok(())
}

fn insert_execution(
    tx: &Transaction<'_>,
    plan: &ExecutionPlanV1,
    request: &PendingExecutionV1,
) -> Result<(), WorkflowError> {
    let inserted = tx.execute(
        "INSERT INTO workflow_execution_outbox (invocation_id, tenant_id, project_id, work_item_id, plan_digest, step_ordinal, state, request, updated_at_ms) VALUES (?1,?2,?3,?4,?5,?6,'not_found',?7,?8)",
        params![request.step.invocation_id.to_string(), plan.tenant_id.0, plan.project_id.0, plan.work_item_id.0, plan.request_digest, i64::from(request.step.ordinal), encode(request)?, sql_u64(request.updated_at_unix_ms)?],
    )
    .map_err(map_sqlite_error)?;
    require_one_row(inserted)?;
    Ok(())
}

fn read_execution_row(
    connection: &Connection,
    invocation_id: Uuid,
) -> Result<Option<ExecutionRow>, WorkflowError> {
    connection
        .query_row(
            "SELECT invocation_id,tenant_id,project_id,work_item_id,plan_digest,step_ordinal,state,request,attempts,updated_at_ms FROM workflow_execution_outbox WHERE invocation_id=?1",
            [invocation_id.to_string()],
            |row| {
                Ok(ExecutionRow {
                    invocation_id: row.get(0)?,
                    tenant_id: row.get(1)?,
                    project_id: row.get(2)?,
                    work_item_id: row.get(3)?,
                    plan_digest: row.get(4)?,
                    step_ordinal: row.get(5)?,
                    state: row.get(6)?,
                    request: row.get(7)?,
                    attempts: row.get(8)?,
                    updated_at_ms: row.get(9)?,
                })
            },
        )
        .optional()
        .map_err(map_sqlite_error)
}

fn read_completion_row(
    connection: &Connection,
    request_id: &str,
) -> Result<Option<CompletionRow>, WorkflowError> {
    connection
        .query_row(
            "SELECT request_id,invocation_id,tenant_id,project_id,work_item_id,state,request,evidence,attempts,updated_at_ms FROM workflow_completion_outbox WHERE request_id=?1",
            [request_id],
            |row| {
                Ok(CompletionRow {
                    request_id: row.get(0)?,
                    invocation_id: row.get(1)?,
                    tenant_id: row.get(2)?,
                    project_id: row.get(3)?,
                    work_item_id: row.get(4)?,
                    state: row.get(5)?,
                    request: row.get(6)?,
                    evidence: row.get(7)?,
                    attempts: row.get(8)?,
                    updated_at_ms: row.get(9)?,
                })
            },
        )
        .optional()
        .map_err(map_sqlite_error)
}

fn read_gate_row(
    connection: &Connection,
    request_id: &str,
) -> Result<Option<GateRow>, WorkflowError> {
    connection
        .query_row(
            "SELECT request_id,plan_id,tenant_id,project_id,work_item_id,state,request,evidence,attempts,updated_at_ms FROM workflow_gate_outbox WHERE request_id=?1",
            [request_id],
            |row| {
                Ok(GateRow {
                    request_id: row.get(0)?,
                    plan_id: row.get(1)?,
                    tenant_id: row.get(2)?,
                    project_id: row.get(3)?,
                    work_item_id: row.get(4)?,
                    state: row.get(5)?,
                    request: row.get(6)?,
                    evidence: row.get(7)?,
                    attempts: row.get(8)?,
                    updated_at_ms: row.get(9)?,
                })
            },
        )
        .optional()
        .map_err(map_sqlite_error)
}

fn validate_execution_request(
    connection: &Connection,
    work_item: &WorkItemExecutionV1,
    request: &PendingExecutionV1,
) -> Result<(), WorkflowError> {
    let row =
        read_execution_row(connection, request.step.invocation_id)?.ok_or_else(corrupt_store)?;
    let stored: PendingExecutionV1 = decode(&row.request)?;
    if stored.schema_version != WORKFLOW_SCHEMA_VERSION {
        return Err(corrupt_store());
    }
    if stored != *request {
        return Err(idempotency_conflict());
    }
    if row.invocation_id != request.step.invocation_id.to_string()
        || row.tenant_id != work_item.tenant_id.0
        || row.project_id != work_item.project_id.0
        || row.work_item_id != work_item.work_item_id.0
        || row.plan_digest != request.plan_digest
        || stored_u16(row.step_ordinal)? != request.step.ordinal
        || row.state != execution_database_state(request.state)
        || stored_u16(row.attempts)? != request.attempts
        || stored_u64(row.updated_at_ms)? != request.updated_at_unix_ms
        || request.plan_id != work_item.plan.plan_id
        || request.plan_digest != work_item.plan.request_digest
        || usize::from(request.step.ordinal) >= work_item.plan.steps.len()
        || work_item.plan.steps[usize::from(request.step.ordinal)] != request.step
        || work_item.next_step_ordinal != request.step.ordinal
        || request.created_at_unix_ms > request.updated_at_unix_ms
    {
        return Err(corrupt_store());
    }
    match request.state {
        ExecutionReconcileState::NotFound
            if matches!(
                work_item.state,
                WorkItemState::Claimed | WorkItemState::InProgress
            ) => {}
        ExecutionReconcileState::Reserved | ExecutionReconcileState::Executing
            if work_item.state == WorkItemState::InProgress => {}
        _ => return Err(corrupt_store()),
    }
    if work_item.terminal_execution_evidence.is_some()
        || work_item.gate_evidence.is_some()
        || work_item.blocker_code.is_some()
    {
        return Err(corrupt_store());
    }
    Ok(())
}

fn validate_completion_request(
    connection: &Connection,
    work_item: &WorkItemExecutionV1,
    request: &PendingCompletionEvidenceV1,
) -> Result<bool, WorkflowError> {
    let row = read_completion_row(connection, &request.request_id)?.ok_or_else(corrupt_store)?;
    let stored: PendingCompletionEvidenceV1 = decode(&row.request)?;
    if stored.schema_version != WORKFLOW_SCHEMA_VERSION {
        return Err(corrupt_store());
    }
    let mut canonical = request.clone();
    let supplied = canonical.request_digest.clone();
    canonical.request_digest.clear();
    let expected = canonical_sha256("sentinel.workflow.completion-request.v1", &canonical)?;
    if stored != *request {
        return Err(idempotency_conflict());
    }
    if row.request_id != request.request_id
        || row.invocation_id != request.invocation_id.to_string()
        || row.tenant_id != work_item.tenant_id.0
        || row.project_id != work_item.project_id.0
        || row.work_item_id != work_item.work_item_id.0
        || row.attempts != 0
        || stored_u64(row.updated_at_ms)? < request.created_at_unix_ms
        || request.plan_id != work_item.plan.plan_id
        || request.plan_digest != work_item.plan.request_digest
        || !constant_time_eq(&expected, &supplied)
    {
        return Err(corrupt_store());
    }
    let step = work_item
        .plan
        .steps
        .iter()
        .find(|step| step.step_id == request.step_id)
        .ok_or_else(corrupt_store)?;
    if step.invocation_id != request.invocation_id
        || request.step_digest != step_digest(step)?
        || request.request_id
            != completion_request_identity(
                request.plan_id,
                request.step_id,
                request.invocation_id,
                &request.plan_digest,
            )?
    {
        return Err(corrupt_store());
    }
    let execution_row =
        read_execution_row(connection, request.invocation_id)?.ok_or_else(corrupt_store)?;
    let execution: PendingExecutionV1 = decode(&execution_row.request)?;
    if execution_row.invocation_id != request.invocation_id.to_string()
        || execution_row.tenant_id != work_item.tenant_id.0
        || execution_row.project_id != work_item.project_id.0
        || execution_row.work_item_id != work_item.work_item_id.0
        || execution_row.plan_digest != request.plan_digest
        || stored_u16(execution_row.step_ordinal)? != step.ordinal
        || execution_row.state != "awaiting_evidence"
        || stored_u16(execution_row.attempts)? != execution.attempts
        || stored_u64(execution_row.updated_at_ms)? != execution.updated_at_unix_ms
        || execution.state != ExecutionReconcileState::Succeeded
        || execution.step != *step
        || execution.plan_id != request.plan_id
        || execution.plan_digest != request.plan_digest
        || execution.authority_snapshot_digest != request.authority_snapshot_digest
    {
        return Err(corrupt_store());
    }
    match (row.state.as_str(), row.evidence) {
        ("pending", None) => {
            if !matches!(
                work_item.state,
                WorkItemState::Claimed | WorkItemState::InProgress
            ) || work_item.next_step_ordinal != step.ordinal
                || work_item.terminal_execution_evidence.is_some()
                || work_item.gate_evidence.is_some()
                || work_item.blocker_code.is_some()
                || stored_u64(row.updated_at_ms)? != request.created_at_unix_ms
            {
                return Err(corrupt_store());
            }
            Ok(false)
        }
        ("completed", Some(bytes)) => {
            let evidence: ExecutionEvidenceReadbackV1 = decode(&bytes)?;
            validate_execution_evidence(
                request,
                &evidence,
                evidence.completed_at_unix_ms,
                step.deadline_unix_ms,
                work_item.plan.deadline_unix_ms,
            )
            .map_err(|_| corrupt_store())?;
            validate_sealed_descriptors(step, &evidence).map_err(|_| corrupt_store())?;
            let ordinal = usize::from(step.ordinal);
            if ordinal + 1 == work_item.plan.steps.len() {
                if work_item.terminal_execution_evidence.as_ref() != Some(&evidence)
                    || !matches!(
                        work_item.state,
                        WorkItemState::InReview | WorkItemState::Done
                    )
                {
                    return Err(corrupt_store());
                }
            } else if usize::from(work_item.next_step_ordinal) <= ordinal {
                return Err(corrupt_store());
            }
            Ok(true)
        }
        _ => Err(corrupt_store()),
    }
}

fn validate_gate_request(
    connection: &Connection,
    work_item: &WorkItemExecutionV1,
    request: &PendingGateEvidenceV1,
) -> Result<bool, WorkflowError> {
    let row = read_gate_row(connection, &request.request_id)?.ok_or_else(corrupt_store)?;
    let stored: PendingGateEvidenceV1 = decode(&row.request)?;
    if stored.schema_version != WORKFLOW_SCHEMA_VERSION {
        return Err(corrupt_store());
    }
    let mut canonical = request.clone();
    let supplied = canonical.request_digest.clone();
    canonical.request_digest.clear();
    let expected = canonical_sha256("sentinel.workflow.gate-request.v1", &canonical)?;
    if stored != *request {
        return Err(idempotency_conflict());
    }
    if row.request_id != request.request_id
        || row.plan_id != request.plan_id.to_string()
        || row.tenant_id != work_item.tenant_id.0
        || row.project_id != work_item.project_id.0
        || row.work_item_id != work_item.work_item_id.0
        || row.attempts != 0
        || stored_u64(row.updated_at_ms)? < request.created_at_unix_ms
        || request.plan_id != work_item.plan.plan_id
        || request.plan_digest != work_item.plan.request_digest
        || !constant_time_eq(&expected, &supplied)
    {
        return Err(corrupt_store());
    }
    let terminal = work_item
        .terminal_execution_evidence
        .as_ref()
        .ok_or_else(corrupt_store)?;
    let expected_expectation = work_item
        .plan
        .steps
        .last()
        .ok_or_else(corrupt_store)?
        .gate_expectation
        .clone();
    let expected_subject = execution_subject_digest(&work_item.plan, terminal)?;
    let expected_checks = canonical_sha256(
        "sentinel.workflow.work-item-gate-checks.v1",
        &expected_expectation.required_checks,
    )?;
    if request.execution_receipt_id != terminal.receipt_id
        || request.expectation != expected_expectation
        || request.subject_digest != expected_subject
        || request.required_checks_digest != expected_checks
        || request.request_id
            != gate_request_identity(
                request.plan_id,
                &request.plan_digest,
                &request.execution_receipt_id,
            )?
    {
        return Err(corrupt_store());
    }
    match (row.state.as_str(), row.evidence) {
        ("pending", None) => {
            if work_item.state != WorkItemState::InReview
                || usize::from(work_item.next_step_ordinal) + 1 != work_item.plan.steps.len()
                || work_item.gate_evidence.is_some()
                || work_item.blocker_code.is_some()
                || stored_u64(row.updated_at_ms)? != request.created_at_unix_ms
            {
                return Err(corrupt_store());
            }
            Ok(false)
        }
        ("completed", Some(bytes)) => {
            let evidence: GateEvidenceReadbackV1 = decode(&bytes)?;
            if evidence.schema_version != WORKFLOW_SCHEMA_VERSION
                || evidence.profile_id != request.expectation.profile_id
                || evidence.profile_generation != request.expectation.profile_generation
                || evidence.profile_digest != request.expectation.profile_digest
                || evidence.subject_digest != request.subject_digest
                || evidence.required_checks_digest != request.required_checks_digest
                || !evidence.passed
                || evidence.receipt_id.is_empty()
                || evidence.completed_at_unix_ms < request.created_at_unix_ms
                || evidence.completed_at_unix_ms > work_item.plan.deadline_unix_ms
                || work_item.state != WorkItemState::Done
                || work_item.gate_evidence.as_ref() != Some(&evidence)
            {
                return Err(corrupt_store());
            }
            Ok(true)
        }
        _ => Err(corrupt_store()),
    }
}

fn validate_execution_evidence(
    request: &PendingCompletionEvidenceV1,
    evidence: &ExecutionEvidenceReadbackV1,
    now_ms: u64,
    step_deadline_ms: u64,
    plan_deadline_ms: u64,
) -> Result<(), WorkflowError> {
    if evidence.schema_version != WORKFLOW_SCHEMA_VERSION
        || evidence.invocation_id != request.invocation_id
        || evidence.plan_digest != request.plan_digest
        || evidence.step_digest != request.step_digest
        || evidence.receipt_id.is_empty()
        || evidence.completed_at_unix_ms < request.created_at_unix_ms
        || evidence.completed_at_unix_ms > now_ms
        || evidence.completed_at_unix_ms > step_deadline_ms
        || evidence.completed_at_unix_ms > plan_deadline_ms
    {
        return Err(authority_conflict());
    }
    Ok(())
}

fn validate_sealed_descriptors(
    step: &crate::ExecutionStepV1,
    evidence: &ExecutionEvidenceReadbackV1,
) -> Result<(), WorkflowError> {
    if evidence.outputs.len() != step.outputs.len()
        || evidence.artifacts.len() != step.artifacts.len()
    {
        return Err(authority_conflict());
    }
    for (expected, observed) in step.outputs.iter().zip(&evidence.outputs) {
        if observed.name != expected.name
            || observed.kind != expected.kind
            || observed.digest_algorithm != expected.digest_algorithm
        {
            return Err(authority_conflict());
        }
        crate::model::validate_digest(&observed.digest)?;
    }
    for (expected, observed) in step.artifacts.iter().zip(&evidence.artifacts) {
        if observed.artifact_kind != expected.artifact_kind
            || observed.media_type != expected.media_type
            || observed.paths != expected.required_paths
        {
            return Err(authority_conflict());
        }
        crate::model::validate_digest(&observed.digest)?;
    }
    let expected_bundle =
        crate::model::sealed_output_bundle_digest(&evidence.outputs, &evidence.artifacts)?;
    if !constant_time_eq(&expected_bundle, &evidence.output_bundle_digest) {
        return Err(authority_conflict());
    }
    Ok(())
}

fn validate_gate_evidence(
    request: &PendingGateEvidenceV1,
    evidence: &GateEvidenceReadbackV1,
    now_ms: u64,
    plan_deadline_ms: u64,
) -> Result<(), WorkflowError> {
    if evidence.schema_version != WORKFLOW_SCHEMA_VERSION
        || evidence.profile_id != request.expectation.profile_id
        || evidence.profile_generation != request.expectation.profile_generation
        || evidence.profile_digest != request.expectation.profile_digest
        || evidence.subject_digest != request.subject_digest
        || evidence.required_checks_digest != request.required_checks_digest
        || evidence.receipt_id.is_empty()
        || !evidence.passed
        || evidence.completed_at_unix_ms < request.created_at_unix_ms
        || evidence.completed_at_unix_ms > now_ms
        || evidence.completed_at_unix_ms > plan_deadline_ms
    {
        return Err(authority_conflict());
    }
    Ok(())
}

fn put_work_item(tx: &Transaction<'_>, value: &WorkItemExecutionV1) -> Result<(), WorkflowError> {
    tx.execute(
        "INSERT INTO workflow_work_items (tenant_id,project_id,work_item_id,version,payload) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(tenant_id,project_id,work_item_id) DO UPDATE SET version=excluded.version,payload=excluded.payload",
        params![value.tenant_id.0, value.project_id.0, value.work_item_id.0, sql_u64(value.version)?, encode(value)?],
    )
    .map_err(map_sqlite_error)?;
    Ok(())
}

fn validate_stored_work_item(
    value: &WorkItemExecutionV1,
    tenant_id: &crate::TenantId,
    project_id: &crate::ProjectId,
    work_item_id: &crate::WorkItemId,
    row_version: u64,
) -> Result<(), WorkflowError> {
    value
        .plan
        .validate_at(value.plan.created_at_unix_ms)
        .map_err(|_| corrupt_store())?;
    if value.schema_version != WORKFLOW_SCHEMA_VERSION
        || value.plan.schema_version != crate::EXECUTION_PLAN_SCHEMA_VERSION
        || value.tenant_id != *tenant_id
        || value.project_id != *project_id
        || value.work_item_id != *work_item_id
        || value.tenant_id != value.plan.tenant_id
        || value.project_id != value.plan.project_id
        || value.work_item_id != value.plan.work_item_id
        || value.agent_id != value.plan.agent_id
        || value.version == 0
        || value.version != row_version
        || value.updated_at_unix_ms < value.plan.created_at_unix_ms
        || usize::from(value.next_step_ordinal) >= value.plan.steps.len()
    {
        return Err(corrupt_store());
    }
    let final_ordinal = value.plan.steps.len() - 1;
    match value.state {
        WorkItemState::Claimed => {
            if value.next_step_ordinal != 0
                || value.terminal_execution_evidence.is_some()
                || value.gate_evidence.is_some()
                || value.blocker_code.is_some()
            {
                return Err(corrupt_store());
            }
        }
        WorkItemState::InProgress => {
            if value.terminal_execution_evidence.is_some()
                || value.gate_evidence.is_some()
                || value.blocker_code.is_some()
            {
                return Err(corrupt_store());
            }
        }
        WorkItemState::InReview | WorkItemState::Done => {
            if usize::from(value.next_step_ordinal) != final_ordinal || value.blocker_code.is_some()
            {
                return Err(corrupt_store());
            }
            let evidence = value
                .terminal_execution_evidence
                .as_ref()
                .ok_or_else(corrupt_store)?;
            let step = &value.plan.steps[final_ordinal];
            if evidence.schema_version != WORKFLOW_SCHEMA_VERSION
                || evidence.invocation_id != step.invocation_id
                || evidence.plan_digest != value.plan.request_digest
                || evidence.step_digest != step_digest(step)?
                || evidence.receipt_id.is_empty()
                || evidence.completed_at_unix_ms < value.plan.created_at_unix_ms
                || evidence.completed_at_unix_ms > step.deadline_unix_ms
                || evidence.completed_at_unix_ms > value.plan.deadline_unix_ms
            {
                return Err(corrupt_store());
            }
            validate_sealed_descriptors(step, evidence).map_err(|_| corrupt_store())?;
            if value.state == WorkItemState::InReview {
                if value.gate_evidence.is_some() {
                    return Err(corrupt_store());
                }
            } else {
                let gate = value.gate_evidence.as_ref().ok_or_else(corrupt_store)?;
                let expectation = &step.gate_expectation;
                let expected_subject = execution_subject_digest(&value.plan, evidence)?;
                let expected_checks = canonical_sha256(
                    "sentinel.workflow.work-item-gate-checks.v1",
                    &expectation.required_checks,
                )?;
                if gate.schema_version != WORKFLOW_SCHEMA_VERSION
                    || gate.receipt_id.is_empty()
                    || gate.profile_id != expectation.profile_id
                    || gate.profile_generation != expectation.profile_generation
                    || gate.profile_digest != expectation.profile_digest
                    || gate.subject_digest != expected_subject
                    || gate.required_checks_digest != expected_checks
                    || !gate.passed
                    || gate.completed_at_unix_ms < evidence.completed_at_unix_ms
                    || gate.completed_at_unix_ms > value.plan.deadline_unix_ms
                {
                    return Err(corrupt_store());
                }
            }
        }
        WorkItemState::Blocked => {
            if value.terminal_execution_evidence.is_some()
                || value.gate_evidence.is_some()
                || !matches!(
                    value.blocker_code.as_deref(),
                    Some("execution_failed")
                        | Some("execution_timed_out")
                        | Some("execution_unknown_outcome")
                )
            {
                return Err(corrupt_store());
            }
        }
        WorkItemState::Cancelled => {
            if value.terminal_execution_evidence.is_some()
                || value.gate_evidence.is_some()
                || value.blocker_code.as_deref() != Some("execution_cancelled")
            {
                return Err(corrupt_store());
            }
        }
        WorkItemState::Assigned => return Err(corrupt_store()),
    }
    Ok(())
}

fn read_work_item(
    connection: &Connection,
    tenant_id: &crate::TenantId,
    project_id: &crate::ProjectId,
    work_item_id: &crate::WorkItemId,
) -> Result<Option<WorkItemExecutionV1>, WorkflowError> {
    connection
        .query_row(
            "SELECT version,payload FROM workflow_work_items WHERE tenant_id=?1 AND project_id=?2 AND work_item_id=?3",
            params![tenant_id.0, project_id.0, work_item_id.0],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()
        .map_err(map_sqlite_error)?
        .map(|(version, payload)| {
            let value: WorkItemExecutionV1 = decode(&payload)?;
            validate_stored_work_item(
                &value,
                tenant_id,
                project_id,
                work_item_id,
                stored_u64(version)?,
            )?;
            Ok(value)
        })
        .transpose()
}

fn read_work_item_by_invocation(
    connection: &Connection,
    invocation_id: Uuid,
) -> Result<Option<WorkItemExecutionV1>, WorkflowError> {
    let key: Option<(String, String, String)> = connection
        .query_row(
            "SELECT tenant_id,project_id,work_item_id FROM workflow_execution_outbox WHERE invocation_id=?1",
            [invocation_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(map_sqlite_error)?;
    key.map(|(tenant, project, work)| {
        read_work_item(
            connection,
            &crate::TenantId(tenant),
            &crate::ProjectId(project),
            &crate::WorkItemId(work),
        )?
        .ok_or_else(corrupt_store)
    })
    .transpose()
}

fn read_work_item_by_plan(
    connection: &Connection,
    plan_id: Uuid,
) -> Result<Option<WorkItemExecutionV1>, WorkflowError> {
    let key: Option<(String, String, String)> = connection
        .query_row(
            "SELECT tenant_id,project_id,work_item_id FROM workflow_gate_outbox WHERE plan_id=?1",
            [plan_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(map_sqlite_error)?;
    key.map(|(tenant, project, work)| {
        read_work_item(
            connection,
            &crate::TenantId(tenant),
            &crate::ProjectId(project),
            &crate::WorkItemId(work),
        )?
        .ok_or_else(corrupt_store)
    })
    .transpose()
}

fn append_audit(
    tx: &Transaction<'_>,
    work_item: &WorkItemExecutionV1,
    event_type: &str,
    before: Option<WorkItemState>,
    after: WorkItemState,
    authority_digest: &str,
    payload_digest: &str,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    tx.execute(
        "INSERT INTO workflow_audit_events (event_id,tenant_id,project_id,work_item_id,event_type,before_state,after_state,authority_digest,payload_digest,created_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![Uuid::now_v7().to_string(), work_item.tenant_id.0, work_item.project_id.0, work_item.work_item_id.0, event_type, before.map(state_name), state_name(after), authority_digest, payload_digest, sql_u64(now_ms)?],
    )
    .map_err(map_sqlite_error)?;
    Ok(())
}

fn state_name(state: WorkItemState) -> &'static str {
    match state {
        WorkItemState::Assigned => "assigned",
        WorkItemState::Claimed => "claimed",
        WorkItemState::InProgress => "in_progress",
        WorkItemState::InReview => "in_review",
        WorkItemState::Done => "done",
        WorkItemState::Blocked => "blocked",
        WorkItemState::Cancelled => "cancelled",
    }
}

fn execution_database_state(state: ExecutionReconcileState) -> &'static str {
    match state {
        ExecutionReconcileState::NotFound => "not_found",
        ExecutionReconcileState::Reserved => "reserved",
        ExecutionReconcileState::Executing => "executing",
        ExecutionReconcileState::Succeeded => "awaiting_evidence",
        ExecutionReconcileState::Failed => "failed",
        ExecutionReconcileState::Cancelled => "cancelled",
        ExecutionReconcileState::TimedOut => "timed_out",
        ExecutionReconcileState::UnknownOutcome => "unknown_outcome",
    }
}

fn completion_request_identity(
    plan_id: Uuid,
    step_id: Uuid,
    invocation_id: Uuid,
    plan_digest: &str,
) -> Result<String, WorkflowError> {
    canonical_sha256(
        "sentinel.workflow.completion-request-identity.v1",
        &(plan_id, step_id, invocation_id, plan_digest),
    )
}

fn gate_request_identity(
    plan_id: Uuid,
    plan_digest: &str,
    execution_receipt_id: &str,
) -> Result<String, WorkflowError> {
    canonical_sha256(
        "sentinel.workflow.gate-request-identity.v1",
        &(plan_id, plan_digest, execution_receipt_id),
    )
}

fn operation_namespace(plan: &ExecutionPlanV1) -> String {
    format!(
        "v1:{}:{}:{}",
        plan.tenant_id.0, plan.principal.principal_id, plan.principal.principal_generation
    )
}

fn read_operation(
    tx: &Transaction<'_>,
    namespace: &str,
    operation_id: &str,
) -> Result<Option<(String, Vec<u8>, i64)>, WorkflowError> {
    tx.query_row(
        "SELECT request_digest,response,created_at_ms FROM workflow_operations WHERE operation_namespace=?1 AND operation_id=?2",
        params![namespace, operation_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .optional()
    .map_err(map_sqlite_error)
}

fn validate_operation_response(
    response: &WorkItemExecutionV1,
    plan: &ExecutionPlanV1,
    created_at_ms: u64,
) -> Result<(), WorkflowError> {
    validate_stored_work_item(
        response,
        &plan.tenant_id,
        &plan.project_id,
        &plan.work_item_id,
        response.version,
    )?;
    if response.plan != *plan
        || response.state != WorkItemState::Claimed
        || response.version != 1
        || response.next_step_ordinal != 0
        || response.terminal_execution_evidence.is_some()
        || response.gate_evidence.is_some()
        || response.blocker_code.is_some()
        || response.updated_at_unix_ms != created_at_ms
    {
        return Err(corrupt_store());
    }
    Ok(())
}

fn insert_operation<T: Serialize>(
    tx: &Transaction<'_>,
    namespace: &str,
    operation_id: &str,
    digest: &str,
    response: &T,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    tx.execute(
        "INSERT INTO workflow_operations (operation_namespace,operation_id,request_digest,response,created_at_ms) VALUES (?1,?2,?3,?4,?5)",
        params![namespace, operation_id, digest, encode(response)?, sql_u64(now_ms)?],
    )
    .map_err(map_sqlite_error)?;
    Ok(())
}

fn immediate(connection: &mut Connection) -> Result<Transaction<'_>, WorkflowError> {
    connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(map_sqlite_error)
}

fn read_schema_version(connection: &Connection) -> Result<u32, WorkflowError> {
    let rows = connection
        .prepare("SELECT singleton,schema_version FROM workflow_schema_meta ORDER BY singleton")
        .map_err(map_sqlite_error)?
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
        .map_err(map_sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite_error)?;
    match rows.as_slice() {
        [(1, version)] => u32::try_from(*version).map_err(|_| corrupt_store()),
        _ => Err(corrupt_store()),
    }
}

fn workflow_object_count(connection: &Connection) -> Result<i64, WorkflowError> {
    connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type IN ('table','index','view','trigger') AND (name LIKE 'workflow_%' OR tbl_name LIKE 'workflow_%' OR name LIKE 'company_%' OR tbl_name LIKE 'company_%')",
            [],
            |row| row.get(0),
        )
        .map_err(map_sqlite_error)
}

#[derive(Clone, Copy)]
struct ColumnContract {
    name: &'static str,
    kind: &'static str,
    not_null: bool,
    default_value: Option<&'static str>,
    primary_key: i64,
}

const fn column(
    name: &'static str,
    kind: &'static str,
    not_null: bool,
    default_value: Option<&'static str>,
    primary_key: i64,
) -> ColumnContract {
    ColumnContract {
        name,
        kind,
        not_null,
        default_value,
        primary_key,
    }
}

fn validate_store_schema(
    connection: &Connection,
    expected_version: u32,
) -> Result<(), WorkflowError> {
    const TABLES: &[(&str, &[ColumnContract])] = &[
        (
            "workflow_schema_meta",
            &[
                column("singleton", "INTEGER", false, None, 1),
                column("schema_version", "INTEGER", true, None, 0),
            ],
        ),
        (
            "workflow_work_items",
            &[
                column("tenant_id", "TEXT", true, None, 1),
                column("project_id", "TEXT", true, None, 2),
                column("work_item_id", "TEXT", true, None, 3),
                column("version", "INTEGER", true, None, 0),
                column("payload", "BLOB", true, None, 0),
            ],
        ),
        (
            "workflow_operations",
            &[
                column("operation_namespace", "TEXT", true, None, 1),
                column("operation_id", "TEXT", true, None, 2),
                column("request_digest", "TEXT", true, None, 0),
                column("response", "BLOB", true, None, 0),
                column("created_at_ms", "INTEGER", true, None, 0),
            ],
        ),
        (
            "workflow_execution_outbox",
            &[
                column("invocation_id", "TEXT", false, None, 1),
                column("tenant_id", "TEXT", true, None, 0),
                column("project_id", "TEXT", true, None, 0),
                column("work_item_id", "TEXT", true, None, 0),
                column("plan_digest", "TEXT", true, None, 0),
                column("step_ordinal", "INTEGER", true, None, 0),
                column("state", "TEXT", true, None, 0),
                column("request", "BLOB", true, None, 0),
                column("attempts", "INTEGER", true, Some("0"), 0),
                column("updated_at_ms", "INTEGER", true, None, 0),
            ],
        ),
        (
            "workflow_completion_outbox",
            &[
                column("request_id", "TEXT", false, None, 1),
                column("invocation_id", "TEXT", true, None, 0),
                column("tenant_id", "TEXT", true, None, 0),
                column("project_id", "TEXT", true, None, 0),
                column("work_item_id", "TEXT", true, None, 0),
                column("state", "TEXT", true, None, 0),
                column("request", "BLOB", true, None, 0),
                column("evidence", "BLOB", false, None, 0),
                column("attempts", "INTEGER", true, Some("0"), 0),
                column("updated_at_ms", "INTEGER", true, None, 0),
            ],
        ),
        (
            "workflow_gate_outbox",
            &[
                column("request_id", "TEXT", false, None, 1),
                column("plan_id", "TEXT", true, None, 0),
                column("tenant_id", "TEXT", true, None, 0),
                column("project_id", "TEXT", true, None, 0),
                column("work_item_id", "TEXT", true, None, 0),
                column("state", "TEXT", true, None, 0),
                column("request", "BLOB", true, None, 0),
                column("evidence", "BLOB", false, None, 0),
                column("attempts", "INTEGER", true, Some("0"), 0),
                column("updated_at_ms", "INTEGER", true, None, 0),
            ],
        ),
        (
            "workflow_audit_events",
            &[
                column("sequence", "INTEGER", false, None, 1),
                column("event_id", "TEXT", true, None, 0),
                column("tenant_id", "TEXT", true, None, 0),
                column("project_id", "TEXT", true, None, 0),
                column("work_item_id", "TEXT", true, None, 0),
                column("event_type", "TEXT", true, None, 0),
                column("before_state", "TEXT", false, None, 0),
                column("after_state", "TEXT", true, None, 0),
                column("authority_digest", "TEXT", true, None, 0),
                column("payload_digest", "TEXT", true, None, 0),
                column("created_at_ms", "INTEGER", true, None, 0),
            ],
        ),
    ];
    let expected_tables = TABLES
        .iter()
        .copied()
        .filter(|(table, _)| expected_version == 2 || *table != "workflow_gate_outbox")
        .collect::<Vec<_>>();
    let actual_tables = connection
        .prepare(
            "SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'workflow_%' ORDER BY name",
        )
        .map_err(map_sqlite_error)?
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(map_sqlite_error)?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(map_sqlite_error)?;
    let required_tables = expected_tables
        .iter()
        .map(|(table, _)| (*table).to_owned())
        .collect::<BTreeSet<_>>();
    if actual_tables != required_tables {
        return Err(corrupt_store());
    }
    let unexpected_object_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type IN ('view','trigger') AND (name LIKE 'workflow_%' OR tbl_name LIKE 'workflow_%')",
            [],
            |row| row.get(0),
        )
        .map_err(map_sqlite_error)?;
    if unexpected_object_count != 0 {
        return Err(corrupt_store());
    }
    for (table, expected) in expected_tables {
        let sql = format!("PRAGMA table_info({table})");
        let mut statement = connection.prepare(&sql).map_err(map_sqlite_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)? != 0,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(map_sqlite_error)?;
        let actual = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_sqlite_error)?;
        if actual.len() != expected.len()
            || actual.iter().zip(expected.iter()).any(
                |((name, kind, not_null, default_value, primary_key), expected)| {
                    name != expected.name
                        || kind != expected.kind
                        || *not_null != expected.not_null
                        || default_value.as_deref() != expected.default_value
                        || *primary_key != expected.primary_key
                },
            )
        {
            return Err(corrupt_store());
        }
    }
    let meta_rows: Vec<(i64, i64)> = connection
        .prepare("SELECT singleton,schema_version FROM workflow_schema_meta ORDER BY singleton")
        .map_err(map_sqlite_error)?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(map_sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(map_sqlite_error)?;
    if meta_rows != [(1, i64::from(expected_version))] {
        return Err(corrupt_store());
    }
    let schema_meta_sql: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='workflow_schema_meta'",
            [],
            |row| row.get(0),
        )
        .map_err(map_sqlite_error)?;
    let compact_meta_sql = schema_meta_sql
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    if !compact_meta_sql.contains("check(singleton=1)") {
        return Err(corrupt_store());
    }
    for (table, columns, origin) in [
        (
            "workflow_work_items",
            &["tenant_id", "project_id", "work_item_id"][..],
            "pk",
        ),
        (
            "workflow_operations",
            &["operation_namespace", "operation_id"][..],
            "pk",
        ),
        ("workflow_execution_outbox", &["invocation_id"][..], "pk"),
        ("workflow_completion_outbox", &["request_id"][..], "pk"),
        ("workflow_completion_outbox", &["invocation_id"][..], "u"),
        ("workflow_audit_events", &["event_id"][..], "u"),
    ] {
        validate_index(connection, table, None, columns, true, origin)?;
    }
    for (table, name, columns) in [
        (
            "workflow_execution_outbox",
            "idx_workflow_execution_pending",
            &["state", "updated_at_ms", "invocation_id"][..],
        ),
        (
            "workflow_completion_outbox",
            "idx_workflow_completion_pending",
            &["state", "updated_at_ms", "request_id"][..],
        ),
    ] {
        validate_index(connection, table, Some(name), columns, false, "c")?;
    }
    if expected_version == 2 {
        validate_index(
            connection,
            "workflow_gate_outbox",
            None,
            &["request_id"],
            true,
            "pk",
        )?;
        validate_index(
            connection,
            "workflow_gate_outbox",
            None,
            &["plan_id"],
            true,
            "u",
        )?;
        validate_index(
            connection,
            "workflow_gate_outbox",
            Some("idx_workflow_gate_pending"),
            &["state", "updated_at_ms", "request_id"],
            false,
            "c",
        )?;
    }
    for (table, expected_count) in [
        ("workflow_schema_meta", 0),
        ("workflow_work_items", 1),
        ("workflow_operations", 1),
        ("workflow_execution_outbox", 2),
        ("workflow_completion_outbox", 3),
        ("workflow_audit_events", 1),
    ] {
        validate_index_count(connection, table, expected_count)?;
    }
    if expected_version == 2 {
        validate_index_count(connection, "workflow_gate_outbox", 3)?;
    }
    Ok(())
}

fn validate_index_count(
    connection: &Connection,
    table: &str,
    expected_count: i64,
) -> Result<(), WorkflowError> {
    let actual: i64 = connection
        .query_row(
            &format!("SELECT COUNT(*) FROM pragma_index_list('{table}')"),
            [],
            |row| row.get(0),
        )
        .map_err(map_sqlite_error)?;
    if actual == expected_count {
        Ok(())
    } else {
        Err(corrupt_store())
    }
}

fn validate_index(
    connection: &Connection,
    table: &str,
    required_name: Option<&str>,
    expected_columns: &[&str],
    require_unique: bool,
    required_origin: &str,
) -> Result<(), WorkflowError> {
    let mut statement = connection
        .prepare(&format!("PRAGMA index_list({table})"))
        .map_err(map_sqlite_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? != 0,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)? != 0,
            ))
        })
        .map_err(map_sqlite_error)?;
    for row in rows {
        let (name, unique, origin, partial) = row.map_err(map_sqlite_error)?;
        if required_name.is_some_and(|required| required != name)
            || unique != require_unique
            || origin != required_origin
            || partial
        {
            continue;
        }
        let mut columns = connection
            .prepare("SELECT name FROM pragma_index_info(?1) ORDER BY seqno")
            .map_err(map_sqlite_error)?;
        let actual = columns
            .query_map([name], |row| row.get::<_, String>(0))
            .map_err(map_sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_sqlite_error)?;
        if actual
            .iter()
            .map(String::as_str)
            .eq(expected_columns.iter().copied())
        {
            return Ok(());
        }
    }
    Err(corrupt_store())
}

fn map_sqlite_error(error: rusqlite::Error) -> WorkflowError {
    WorkflowError::from(error)
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, WorkflowError> {
    serde_json::to_vec(value).map_err(|_| WorkflowError::persistence())
}

fn decode<T: DeserializeOwned>(value: &[u8]) -> Result<T, WorkflowError> {
    serde_json::from_slice(value).map_err(WorkflowError::from)
}

fn sql_u64(value: u64) -> Result<i64, WorkflowError> {
    i64::try_from(value).map_err(|_| WorkflowError::persistence())
}

fn stored_u64(value: i64) -> Result<u64, WorkflowError> {
    u64::try_from(value).map_err(|_| corrupt_store())
}

fn stored_u16(value: i64) -> Result<u16, WorkflowError> {
    u16::try_from(value).map_err(|_| corrupt_store())
}

fn validate_monotonic_time(
    now_ms: u64,
    request_updated_at_ms: u64,
    work_item_updated_at_ms: u64,
) -> Result<(), WorkflowError> {
    if now_ms < request_updated_at_ms || now_ms < work_item_updated_at_ms {
        return Err(WorkflowError::new(
            WorkflowErrorCode::InvalidTransition,
            false,
            "workflow clock regressed behind durable state",
        ));
    }
    Ok(())
}

fn require_one_row(rows: usize) -> Result<(), WorkflowError> {
    if rows == 1 {
        Ok(())
    } else {
        Err(corrupt_store())
    }
}

trait StoredSchema {
    fn schema_version(&self) -> u16;
}

impl StoredSchema for PendingExecutionV1 {
    fn schema_version(&self) -> u16 {
        self.schema_version
    }
}

impl StoredSchema for PendingCompletionEvidenceV1 {
    fn schema_version(&self) -> u16 {
        self.schema_version
    }
}

impl StoredSchema for PendingGateEvidenceV1 {
    fn schema_version(&self) -> u16 {
        self.schema_version
    }
}

fn authority_conflict() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::AuthorityConflict,
        false,
        "workflow authority changed or does not match the durable request",
    )
}

fn idempotency_conflict() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::IdempotencyConflict,
        false,
        "stable operation identity was rebound to different canonical content",
    )
}

fn corrupt_store() -> WorkflowError {
    WorkflowError::corrupt_store()
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use serde::ser::Error as _;
    use serde::{Serialize, Serializer};
    use uuid::Uuid;

    use super::{encode, sql_u64, WorkflowStore, SCHEMA};
    use crate::WorkflowErrorCode;

    struct SerializationFailure;

    impl Serialize for SerializationFailure {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            Err(S::Error::custom("injected serialization failure"))
        }
    }

    #[test]
    fn deterministic_local_store_failures_are_never_retryable() {
        let serialization = encode(&SerializationFailure).unwrap_err();
        assert_eq!(serialization.code, WorkflowErrorCode::PersistenceFailure);
        assert!(!serialization.retryable);

        let overflow = sql_u64(u64::MAX).unwrap_err();
        assert_eq!(overflow.code, WorkflowErrorCode::PersistenceFailure);
        assert!(!overflow.retryable);

        let store = Arc::new(WorkflowStore::open(":memory:").unwrap());
        let poison = Arc::clone(&store);
        let result = std::thread::spawn(move || {
            let _guard = poison.connection.lock().unwrap();
            panic!("injected mutex poison");
        })
        .join();
        assert!(result.is_err());
        let poisoned = store.pending_executions(1).unwrap_err();
        assert_eq!(poisoned.code, WorkflowErrorCode::PersistenceFailure);
        assert!(!poisoned.retryable);
    }

    #[test]
    fn operation_timestamp_reservation_is_durable_and_content_bound() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("workflow.sqlite");
        let operation_id = Uuid::parse_str("018f3f32-4f01-7f2c-a6c1-f6f4a81b2809").unwrap();
        let namespace = "delivery-intent-v1:tenant-m0:release-8:1";
        let digest = "a".repeat(64);

        let store = WorkflowStore::open(&path).unwrap();
        assert_eq!(
            store
                .reserve_operation_timestamp(namespace, operation_id, &digest, 100)
                .unwrap(),
            (false, 100)
        );
        drop(store);

        let reopened = WorkflowStore::open(&path).unwrap();
        assert_eq!(
            reopened
                .reserve_operation_timestamp(namespace, operation_id, &digest, 200)
                .unwrap(),
            (true, 100)
        );
        assert_eq!(
            reopened
                .reserve_operation_timestamp(namespace, operation_id, &"b".repeat(64), 200)
                .unwrap_err()
                .code,
            WorkflowErrorCode::IdempotencyConflict
        );
    }

    fn create_v1_store(path: &Path) {
        let connection = rusqlite::Connection::open(path).unwrap();
        connection.execute_batch(SCHEMA).unwrap();
        connection
            .execute_batch(
                "DROP INDEX idx_workflow_gate_pending;
                 DROP TABLE workflow_gate_outbox;
                 UPDATE workflow_schema_meta SET schema_version=1;",
            )
            .unwrap();
    }

    fn schema_snapshot(path: &Path) -> Vec<(String, String, String, Option<String>)> {
        let connection = rusqlite::Connection::open(path).unwrap();
        let mut statement = connection
            .prepare(
                "SELECT type,name,tbl_name,sql FROM sqlite_master ORDER BY type,name,tbl_name,sql",
            )
            .unwrap();
        let rows = statement
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        rows
    }

    #[test]
    fn bootstrap_and_migration_failure_points_roll_back_before_commit() {
        let directory = tempfile::tempdir().unwrap();
        let bootstrap = directory.path().join("bootstrap.sqlite");
        let error =
            WorkflowStore::open_with_failure_injection(&bootstrap, true, false).unwrap_err();
        assert_eq!(error.code, WorkflowErrorCode::CorruptStore);
        assert!(!error.retryable);
        assert!(schema_snapshot(&bootstrap).is_empty());
        drop(WorkflowStore::open(&bootstrap).unwrap());

        let migration = directory.path().join("migration.sqlite");
        create_v1_store(&migration);
        let before = schema_snapshot(&migration);
        let error =
            WorkflowStore::open_with_failure_injection(&migration, false, true).unwrap_err();
        assert_eq!(error.code, WorkflowErrorCode::CorruptStore);
        assert!(!error.retryable);
        assert_eq!(schema_snapshot(&migration), before);
        let connection = rusqlite::Connection::open(&migration).unwrap();
        let version: u32 = connection
            .query_row(
                "SELECT schema_version FROM workflow_schema_meta",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, 1);
        let gate_exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='workflow_gate_outbox')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!gate_exists);
        drop(connection);
        WorkflowStore::open(&migration).unwrap();
    }
}
