use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::{
    CommandOutcome, PendingExecution, ProjectId, ProjectProjection, WorkflowError, WorkflowEvent,
    WorkflowResponse,
};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS workflow_schema_meta (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    schema_version INTEGER NOT NULL
);
INSERT OR IGNORE INTO workflow_schema_meta (singleton, schema_version) VALUES (1, 1);
CREATE TABLE IF NOT EXISTS workflow_entities (
    entity_type TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    payload BLOB NOT NULL,
    PRIMARY KEY (entity_type, entity_id)
);
CREATE TABLE IF NOT EXISTS workflow_operations (
    operation_id TEXT PRIMARY KEY,
    operation_digest TEXT NOT NULL,
    response BLOB NOT NULL,
    created_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS workflow_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE,
    event_type TEXT NOT NULL,
    aggregate_type TEXT NOT NULL,
    aggregate_id TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    payload BLOB NOT NULL,
    created_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_workflow_events_aggregate
    ON workflow_events(aggregate_type, aggregate_id, sequence);
CREATE INDEX IF NOT EXISTS idx_workflow_events_operation
    ON workflow_events(operation_id, sequence);
CREATE TABLE IF NOT EXISTS workflow_project_projections (
    project_id TEXT PRIMARY KEY,
    last_event_sequence INTEGER NOT NULL,
    payload BLOB NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS workflow_execution_outbox (
    invocation_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    work_item_id TEXT NOT NULL,
    assignment_version INTEGER NOT NULL,
    request_digest TEXT NOT NULL,
    request BLOB NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'dispatched', 'failed')),
    receipt BLOB,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_workflow_execution_outbox_work
    ON workflow_execution_outbox(project_id, work_item_id, state);
"#;

pub struct WorkflowStore {
    connection: Mutex<Connection>,
}

impl std::fmt::Debug for WorkflowStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkflowStore")
            .finish_non_exhaustive()
    }
}

impl WorkflowStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, WorkflowError> {
        let connection = Connection::open(path).map_err(|_| WorkflowError::persistence())?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| WorkflowError::persistence())?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(|_| WorkflowError::persistence())?;
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(|_| WorkflowError::persistence())?;
        connection
            .execute_batch(SCHEMA)
            .map_err(|_| WorkflowError::persistence())?;
        let schema_version: u32 = connection
            .query_row(
                "SELECT schema_version FROM workflow_schema_meta WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .map_err(|_| WorkflowError::persistence())?;
        if schema_version != crate::WORKFLOW_SCHEMA_VERSION {
            return Err(WorkflowError::new(
                crate::WorkflowErrorCode::PersistenceFailure,
                false,
                "workflow store schema version is unsupported",
            ));
        }
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub(crate) fn execute<F>(
        &self,
        operation_id: &str,
        operation_digest: &str,
        now_ms: u64,
        apply: F,
    ) -> Result<CommandOutcome, WorkflowError>
    where
        F: FnOnce(&WorkflowTransaction<'_>) -> Result<WorkflowResponse, WorkflowError>,
    {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| WorkflowError::persistence())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| WorkflowError::persistence())?;

        let existing: Option<(String, Vec<u8>)> = transaction
            .query_row(
                "SELECT operation_digest, response FROM workflow_operations WHERE operation_id = ?1",
                [operation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|_| WorkflowError::persistence())?;
        if let Some((digest, response)) = existing {
            if digest != operation_digest {
                return Err(WorkflowError::new(
                    crate::WorkflowErrorCode::IdempotencyConflict,
                    false,
                    "operation id was already used with a different request digest",
                ));
            }
            return Ok(CommandOutcome {
                replayed: true,
                response: serde_json::from_slice(&response)?,
            });
        }

        let workflow_tx = WorkflowTransaction {
            transaction: &transaction,
        };
        let response = apply(&workflow_tx)?;
        let response_bytes = serde_json::to_vec(&response)?;
        let now_ms = sql_integer(now_ms)?;
        transaction
            .execute(
                "INSERT INTO workflow_operations (operation_id, operation_digest, response, created_at_ms) VALUES (?1, ?2, ?3, ?4)",
                params![operation_id, operation_digest, response_bytes, now_ms],
            )
            .map_err(|_| WorkflowError::persistence())?;
        transaction
            .commit()
            .map_err(|_| WorkflowError::persistence())?;
        Ok(CommandOutcome {
            replayed: false,
            response,
        })
    }

    pub(crate) fn write<F, T>(&self, apply: F) -> Result<T, WorkflowError>
    where
        F: FnOnce(&WorkflowTransaction<'_>) -> Result<T, WorkflowError>,
    {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| WorkflowError::persistence())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| WorkflowError::persistence())?;
        let workflow_tx = WorkflowTransaction {
            transaction: &transaction,
        };
        let result = apply(&workflow_tx)?;
        transaction
            .commit()
            .map_err(|_| WorkflowError::persistence())?;
        Ok(result)
    }

    pub fn entity<T: DeserializeOwned>(
        &self,
        entity_type: &str,
        entity_id: &str,
    ) -> Result<Option<T>, WorkflowError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| WorkflowError::persistence())?;
        let payload: Option<Vec<u8>> = connection
            .query_row(
                "SELECT payload FROM workflow_entities WHERE entity_type = ?1 AND entity_id = ?2",
                params![entity_type, entity_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| WorkflowError::persistence())?;
        payload
            .map(|bytes| serde_json::from_slice(&bytes).map_err(WorkflowError::from))
            .transpose()
    }

    pub fn project_projection(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<ProjectProjection>, WorkflowError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| WorkflowError::persistence())?;
        let payload: Option<Vec<u8>> = connection
            .query_row(
                "SELECT payload FROM workflow_project_projections WHERE project_id = ?1",
                [&project_id.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| WorkflowError::persistence())?;
        payload
            .map(|bytes| serde_json::from_slice(&bytes).map_err(WorkflowError::from))
            .transpose()
    }

    pub fn events_since(
        &self,
        after_sequence: i64,
        limit: usize,
    ) -> Result<Vec<WorkflowEvent>, WorkflowError> {
        let limit = limit.clamp(1, 1_000) as i64;
        let connection = self
            .connection
            .lock()
            .map_err(|_| WorkflowError::persistence())?;
        let mut statement = connection
            .prepare(
                "SELECT sequence, payload FROM workflow_events WHERE sequence > ?1 ORDER BY sequence ASC LIMIT ?2",
            )
            .map_err(|_| WorkflowError::persistence())?;
        let rows = statement
            .query_map(params![after_sequence, limit], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
            })
            .map_err(|_| WorkflowError::persistence())?;
        let mut events = Vec::new();
        for row in rows {
            let (sequence, payload) = row.map_err(|_| WorkflowError::persistence())?;
            let mut event: WorkflowEvent = serde_json::from_slice(&payload)?;
            event.sequence = sequence;
            events.push(event);
        }
        Ok(events)
    }

    pub fn pending_executions(&self, limit: usize) -> Result<Vec<PendingExecution>, WorkflowError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| WorkflowError::persistence())?;
        let mut statement = connection
            .prepare(
                "SELECT request FROM workflow_execution_outbox WHERE state = 'pending' ORDER BY created_at_ms, invocation_id LIMIT ?1",
            )
            .map_err(|_| WorkflowError::persistence())?;
        let rows = statement
            .query_map([limit.clamp(1, 100) as i64], |row| row.get::<_, Vec<u8>>(0))
            .map_err(|_| WorkflowError::persistence())?;
        let mut pending = Vec::new();
        for row in rows {
            pending.push(serde_json::from_slice(
                &row.map_err(|_| WorkflowError::persistence())?,
            )?);
        }
        Ok(pending)
    }
}

pub(crate) struct WorkflowTransaction<'a> {
    transaction: &'a Transaction<'a>,
}

impl WorkflowTransaction<'_> {
    pub(crate) fn entity<T: DeserializeOwned>(
        &self,
        entity_type: &str,
        entity_id: &str,
    ) -> Result<Option<T>, WorkflowError> {
        let payload: Option<Vec<u8>> = self
            .transaction
            .query_row(
                "SELECT payload FROM workflow_entities WHERE entity_type = ?1 AND entity_id = ?2",
                params![entity_type, entity_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| WorkflowError::persistence())?;
        payload
            .map(|bytes| serde_json::from_slice(&bytes).map_err(WorkflowError::from))
            .transpose()
    }

    pub(crate) fn entities<T: DeserializeOwned>(
        &self,
        entity_type: &str,
    ) -> Result<Vec<T>, WorkflowError> {
        let mut statement = self
            .transaction
            .prepare(
                "SELECT payload FROM workflow_entities WHERE entity_type = ?1 ORDER BY entity_id",
            )
            .map_err(|_| WorkflowError::persistence())?;
        let rows = statement
            .query_map([entity_type], |row| row.get::<_, Vec<u8>>(0))
            .map_err(|_| WorkflowError::persistence())?;
        let mut entities = Vec::new();
        for row in rows {
            entities.push(serde_json::from_slice(
                &row.map_err(|_| WorkflowError::persistence())?,
            )?);
        }
        Ok(entities)
    }

    pub(crate) fn put_entity<T: Serialize>(
        &self,
        entity_type: &str,
        entity_id: &str,
        version: u64,
        entity: &T,
    ) -> Result<(), WorkflowError> {
        let payload = serde_json::to_vec(entity)?;
        let version = sql_integer(version)?;
        self.transaction
            .execute(
                "INSERT INTO workflow_entities (entity_type, entity_id, version, payload) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(entity_type, entity_id) DO UPDATE SET version = excluded.version, payload = excluded.payload",
                params![entity_type, entity_id, version, payload],
            )
            .map_err(|_| WorkflowError::persistence())?;
        Ok(())
    }

    pub(crate) fn append_event(&self, event: &mut WorkflowEvent) -> Result<i64, WorkflowError> {
        let payload = serde_json::to_vec(event)?;
        let timestamp_ms = sql_integer(event.timestamp_ms)?;
        self.transaction
            .execute(
                "INSERT INTO workflow_events (event_id, event_type, aggregate_type, aggregate_id, operation_id, payload, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    event.event_id,
                    format!("{:?}", event.event_type),
                    event.aggregate_type,
                    event.aggregate_id,
                    event.operation_id,
                    payload,
                    timestamp_ms,
                ],
            )
            .map_err(|_| WorkflowError::persistence())?;
        let sequence = self.transaction.last_insert_rowid();
        event.sequence = sequence;
        Ok(sequence)
    }

    pub(crate) fn put_projection(
        &self,
        projection: &ProjectProjection,
    ) -> Result<(), WorkflowError> {
        let payload = serde_json::to_vec(projection)?;
        let updated_at_ms = sql_integer(projection.updated_at_ms)?;
        self.transaction
            .execute(
                "INSERT INTO workflow_project_projections (project_id, last_event_sequence, payload, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(project_id) DO UPDATE SET last_event_sequence = excluded.last_event_sequence,
                 payload = excluded.payload, updated_at_ms = excluded.updated_at_ms",
                params![
                    projection.project_id.0,
                    projection.last_event_sequence,
                    payload,
                    updated_at_ms,
                ],
            )
            .map_err(|_| WorkflowError::persistence())?;
        Ok(())
    }

    pub(crate) fn enqueue_execution(
        &self,
        request: &PendingExecution,
        request_digest: &str,
        now_ms: u64,
    ) -> Result<(), WorkflowError> {
        let payload = serde_json::to_vec(request)?;
        let now_ms = sql_integer(now_ms)?;
        let existing: Option<String> = self
            .transaction
            .query_row(
                "SELECT request_digest FROM workflow_execution_outbox WHERE invocation_id = ?1",
                [&request.invocation_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| WorkflowError::persistence())?;
        if let Some(existing_digest) = existing {
            if existing_digest != request_digest {
                return Err(WorkflowError::new(
                    crate::WorkflowErrorCode::DigestConflict,
                    false,
                    "execution invocation was already reserved with another digest",
                ));
            }
            return Ok(());
        }
        self.transaction
            .execute(
                "INSERT INTO workflow_execution_outbox
                 (invocation_id, project_id, work_item_id, assignment_version, request_digest,
                  request, state, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?7)",
                params![
                    request.invocation_id,
                    request.project_id.0,
                    request.work_item_id.0,
                    sql_integer(request.assignment_version)?,
                    request_digest,
                    payload,
                    now_ms
                ],
            )
            .map_err(|_| WorkflowError::persistence())?;
        Ok(())
    }

    pub(crate) fn mark_execution_attempt(
        &self,
        invocation_id: &str,
        receipt: Option<&crate::ExecutionReceipt>,
        error: Option<&str>,
        now_ms: u64,
    ) -> Result<bool, WorkflowError> {
        let attempts: i64 = self
            .transaction
            .query_row(
                "SELECT attempts FROM workflow_execution_outbox WHERE invocation_id = ?1",
                [invocation_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| WorkflowError::persistence())?
            .ok_or_else(|| {
                WorkflowError::new(
                    crate::WorkflowErrorCode::NotFound,
                    false,
                    "execution outbox entry not found",
                )
            })?;
        let (state, receipt) = match receipt {
            Some(receipt) => ("dispatched", Some(serde_json::to_vec(receipt)?)),
            None if attempts >= 2 => ("failed", None),
            None => ("pending", None),
        };
        let now_ms = sql_integer(now_ms)?;
        let changed = self
            .transaction
            .execute(
                "UPDATE workflow_execution_outbox SET state = ?1, receipt = ?2,
                 attempts = attempts + 1, last_error = ?3, updated_at_ms = ?4
                 WHERE invocation_id = ?5",
                params![state, receipt, error, now_ms, invocation_id],
            )
            .map_err(|_| WorkflowError::persistence())?;
        if changed != 1 {
            return Err(WorkflowError::new(
                crate::WorkflowErrorCode::NotFound,
                false,
                "execution outbox entry not found",
            ));
        }
        Ok(state == "failed")
    }

    pub(crate) fn reset_failed_execution(
        &self,
        project_id: &ProjectId,
        work_item_id: &crate::WorkItemId,
        now_ms: u64,
    ) -> Result<(), WorkflowError> {
        let changed = self
            .transaction
            .execute(
                "UPDATE workflow_execution_outbox SET state = 'pending', attempts = 0,
                 last_error = NULL, updated_at_ms = ?1
                 WHERE project_id = ?2 AND work_item_id = ?3 AND state = 'failed'",
                params![sql_integer(now_ms)?, project_id.0, work_item_id.0],
            )
            .map_err(|_| WorkflowError::persistence())?;
        if changed != 1 {
            return Err(WorkflowError::new(
                crate::WorkflowErrorCode::InvalidTransition,
                false,
                "failed execution outbox entry is not uniquely resolvable",
            ));
        }
        Ok(())
    }
}

fn sql_integer(value: u64) -> Result<i64, WorkflowError> {
    i64::try_from(value).map_err(|_| WorkflowError::persistence())
}
