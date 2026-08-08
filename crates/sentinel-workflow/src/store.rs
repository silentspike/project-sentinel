use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::sync::Mutex;

use rusqlite::{
    params, Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    CommandOutcome, PendingCompletionEvidence, PendingExecution, ProjectId, ProjectProjection,
    WorkflowError, WorkflowEvent, WorkflowResponse,
};

const WORKFLOW_STORE_SCHEMA_VERSION: u32 = 3;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS workflow_schema_meta (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    schema_version INTEGER NOT NULL
);
INSERT OR IGNORE INTO workflow_schema_meta (singleton, schema_version) VALUES (1, 3);
CREATE TABLE IF NOT EXISTS workflow_entities (
    entity_type TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    payload BLOB NOT NULL,
    PRIMARY KEY (entity_type, entity_id)
);
CREATE TABLE IF NOT EXISTS workflow_operations (
    operation_namespace TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    operation_digest TEXT NOT NULL,
    response BLOB NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (operation_namespace, operation_id)
);
CREATE TABLE IF NOT EXISTS workflow_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE,
    event_type TEXT NOT NULL,
    aggregate_type TEXT NOT NULL,
    aggregate_id TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    payload BLOB NOT NULL,
    created_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_workflow_events_aggregate
    ON workflow_events(aggregate_type, aggregate_id, sequence);
CREATE INDEX IF NOT EXISTS idx_workflow_events_operation
    ON workflow_events(operation_id, sequence);
CREATE TABLE IF NOT EXISTS workflow_event_outbox (
    event_id TEXT PRIMARY KEY REFERENCES workflow_events(event_id),
    event_sequence INTEGER NOT NULL UNIQUE,
    state TEXT NOT NULL CHECK (state IN ('pending', 'published')),
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    published_at_ms INTEGER
);
CREATE INDEX IF NOT EXISTS idx_workflow_event_outbox_pending
    ON workflow_event_outbox(state, event_sequence)
    WHERE state = 'pending';
CREATE TABLE IF NOT EXISTS workflow_entity_history (
    mutation_sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_type TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    payload BLOB NOT NULL,
    UNIQUE (entity_type, entity_id, version)
);
CREATE INDEX IF NOT EXISTS idx_workflow_entity_history_entity
    ON workflow_entity_history(entity_type, entity_id, version);
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
    state TEXT NOT NULL CHECK (state IN ('pending', 'dispatched', 'failed', 'authority_conflict')),
    receipt BLOB,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_workflow_execution_outbox_work
    ON workflow_execution_outbox(project_id, work_item_id, state);
CREATE TABLE IF NOT EXISTS workflow_completion_outbox (
    request_id TEXT PRIMARY KEY,
    invocation_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    work_item_id TEXT NOT NULL,
    assignment_version INTEGER NOT NULL,
    request_digest TEXT NOT NULL,
    request BLOB NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'completed', 'failed')),
    authority_receipt_id TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE (invocation_id, assignment_version)
);
CREATE INDEX IF NOT EXISTS idx_workflow_completion_outbox_pending
    ON workflow_completion_outbox(state, created_at_ms, request_id);
CREATE TABLE IF NOT EXISTS workflow_projection_checkpoint (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    source_event_high_watermark INTEGER NOT NULL,
    projected_event_high_watermark INTEGER NOT NULL,
    project_count INTEGER NOT NULL,
    rebuilt_at_ms INTEGER
);
INSERT OR IGNORE INTO workflow_projection_checkpoint
    (singleton, source_event_high_watermark, projected_event_high_watermark, project_count)
    VALUES (1, 0, 0, 0);
"#;

const MIGRATE_V1_TO_V2_STEPS: &[&str] = &[
    r#"CREATE TABLE IF NOT EXISTS workflow_entity_history (
        mutation_sequence INTEGER PRIMARY KEY AUTOINCREMENT,
        entity_type TEXT NOT NULL,
        entity_id TEXT NOT NULL,
        version INTEGER NOT NULL,
        payload BLOB NOT NULL,
        UNIQUE (entity_type, entity_id, version)
    );
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
        state TEXT NOT NULL CHECK (state IN ('pending', 'dispatched', 'failed', 'authority_conflict')),
        receipt BLOB,
        attempts INTEGER NOT NULL DEFAULT 0,
        last_error TEXT,
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS workflow_projection_checkpoint (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        source_event_high_watermark INTEGER NOT NULL,
        projected_event_high_watermark INTEGER NOT NULL,
        project_count INTEGER NOT NULL,
        rebuilt_at_ms INTEGER
    );
    INSERT OR IGNORE INTO workflow_projection_checkpoint
        (singleton, source_event_high_watermark, projected_event_high_watermark, project_count)
        VALUES (1, 0, 0, 0);"#,
    "DROP INDEX IF EXISTS idx_workflow_events_aggregate;",
    "DROP INDEX IF EXISTS idx_workflow_events_operation;",
    "ALTER TABLE workflow_operations RENAME TO workflow_operations_v1;",
    r#"CREATE TABLE workflow_operations (
    operation_namespace TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    operation_digest TEXT NOT NULL,
    response BLOB NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (operation_namespace, operation_id)
);"#,
    r#"INSERT INTO workflow_operations
    (operation_namespace, operation_id, operation_digest, response, created_at_ms)
    SELECT 'legacy', operation_id, operation_digest, response, created_at_ms
    FROM workflow_operations_v1;"#,
    "DROP TABLE workflow_operations_v1;",
    "ALTER TABLE workflow_events RENAME TO workflow_events_v1;",
    r#"CREATE TABLE workflow_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE,
    event_type TEXT NOT NULL,
    aggregate_type TEXT NOT NULL,
    aggregate_id TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    payload BLOB NOT NULL,
    created_at_ms INTEGER NOT NULL
);"#,
    r#"INSERT INTO workflow_events
    (sequence, event_id, event_type, aggregate_type, aggregate_id, tenant_id,
     operation_id, payload, created_at_ms)
    SELECT sequence, event_id, event_type, aggregate_type, aggregate_id, 'legacy',
           operation_id, payload, created_at_ms
    FROM workflow_events_v1;"#,
    "DROP TABLE workflow_events_v1;",
    r#"INSERT OR IGNORE INTO workflow_entity_history
    (entity_type, entity_id, version, payload)
    SELECT entity_type, entity_id, version, payload FROM workflow_entities;"#,
    r#"UPDATE workflow_projection_checkpoint
SET source_event_high_watermark = (SELECT COALESCE(MAX(sequence), 0) FROM workflow_events),
    projected_event_high_watermark = (SELECT COALESCE(MAX(sequence), 0) FROM workflow_events),
    project_count = (SELECT COUNT(*) FROM workflow_project_projections)
WHERE singleton = 1;"#,
    "UPDATE workflow_schema_meta SET schema_version = 2 WHERE singleton = 1;",
];

const MIGRATE_V2_TO_V3_STEPS: &[&str] = &[
    r#"CREATE TABLE IF NOT EXISTS workflow_event_outbox (
        event_id TEXT PRIMARY KEY REFERENCES workflow_events(event_id),
        event_sequence INTEGER NOT NULL UNIQUE,
        state TEXT NOT NULL CHECK (state IN ('pending', 'published')),
        attempts INTEGER NOT NULL DEFAULT 0,
        last_error TEXT,
        published_at_ms INTEGER
    );"#,
    r#"INSERT OR IGNORE INTO workflow_event_outbox (event_id, event_sequence, state)
       SELECT event_id, sequence, 'pending' FROM workflow_events;"#,
    r#"CREATE INDEX IF NOT EXISTS idx_workflow_event_outbox_pending
       ON workflow_event_outbox(state, event_sequence)
       WHERE state = 'pending';"#,
    "UPDATE workflow_schema_meta SET schema_version = 3 WHERE singleton = 1;",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionCheckpoint {
    pub source_event_high_watermark: i64,
    pub projected_event_high_watermark: i64,
    pub project_count: u64,
    pub rebuilt_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowBackupManifest {
    pub schema_version: u32,
    pub database_sha256: String,
    pub event_high_watermark: i64,
    pub entity_history_high_watermark: i64,
    pub entity_count: u64,
    pub operation_count: u64,
    pub execution_outbox_count: u64,
    pub completion_outbox_count: u64,
    pub event_publication_pending_count: u64,
    pub event_publication_published_count: u64,
    pub event_publication_high_watermark: i64,
    pub project_projection_count: u64,
    pub projection_checkpoint: ProjectionCheckpoint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowBackupImage {
    pub manifest: WorkflowBackupManifest,
    pub database: Vec<u8>,
}

impl WorkflowBackupManifest {
    fn same_logical_state(&self, other: &Self) -> bool {
        self.schema_version == other.schema_version
            && self.event_high_watermark == other.event_high_watermark
            && self.entity_history_high_watermark == other.entity_history_high_watermark
            && self.entity_count == other.entity_count
            && self.operation_count == other.operation_count
            && self.execution_outbox_count == other.execution_outbox_count
            && self.completion_outbox_count == other.completion_outbox_count
            && self.event_publication_pending_count == other.event_publication_pending_count
            && self.event_publication_published_count == other.event_publication_published_count
            && self.event_publication_high_watermark == other.event_publication_high_watermark
            && self.project_projection_count == other.project_projection_count
            && self.projection_checkpoint == other.projection_checkpoint
    }
}

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
        Self::open_with_migration_failpoint(path, None)
    }

    fn open_with_migration_failpoint(
        path: impl AsRef<Path>,
        fail_after_step: Option<usize>,
    ) -> Result<Self, WorkflowError> {
        Self::open_with_migration_failpoints(path, fail_after_step, None)
    }

    fn open_with_migration_failpoints(
        path: impl AsRef<Path>,
        fail_after_v1_step: Option<usize>,
        fail_after_v2_step: Option<usize>,
    ) -> Result<Self, WorkflowError> {
        let mut connection = Connection::open(path).map_err(|_| WorkflowError::persistence())?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| WorkflowError::persistence())?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(|_| WorkflowError::persistence())?;
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(|_| WorkflowError::persistence())?;
        let has_schema: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'workflow_schema_meta')",
                [],
                |row| row.get(0),
            )
            .map_err(|_| WorkflowError::persistence())?;
        if !has_schema {
            connection
                .execute_batch(SCHEMA)
                .map_err(|_| WorkflowError::persistence())?;
        }
        let mut schema_version: u32 = connection
            .query_row(
                "SELECT schema_version FROM workflow_schema_meta WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .map_err(|_| WorkflowError::persistence())?;
        if schema_version == 1 {
            migrate_v1_to_v2(&mut connection, fail_after_v1_step)?;
            schema_version = 2;
        }
        if schema_version == 2 {
            migrate_v2_to_v3(&mut connection, fail_after_v2_step)?;
            schema_version = WORKFLOW_STORE_SCHEMA_VERSION;
        }
        if schema_version != WORKFLOW_STORE_SCHEMA_VERSION {
            return Err(WorkflowError::new(
                crate::WorkflowErrorCode::PersistenceFailure,
                false,
                "workflow store schema version is unsupported",
            ));
        }
        connection
            .execute_batch(SCHEMA)
            .map_err(|_| WorkflowError::persistence())?;
        connection
            .execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_workflow_events_aggregate
                 ON workflow_events(aggregate_type, aggregate_id, sequence);
                 CREATE INDEX IF NOT EXISTS idx_workflow_events_operation
                 ON workflow_events(operation_id, sequence);
                 CREATE INDEX IF NOT EXISTS idx_workflow_events_tenant
                 ON workflow_events(tenant_id, sequence);",
            )
            .map_err(|_| WorkflowError::persistence())?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub(crate) fn execute<F>(
        &self,
        operation_namespace: &str,
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
                "SELECT operation_digest, response FROM workflow_operations
                 WHERE operation_namespace = ?1 AND operation_id = ?2",
                params![operation_namespace, operation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|_| WorkflowError::persistence())?;
        if existing.is_none() {
            // Version 1 used globally unique operation IDs. Those records have
            // no trustworthy principal or tenant owner, so they are tombstones:
            // never disclose their response through a newly scoped principal.
            let legacy_exists = transaction
                .query_row(
                    "SELECT 1 FROM workflow_operations
                     WHERE operation_namespace = 'legacy' AND operation_id = ?1",
                    [operation_id],
                    |_| Ok(()),
                )
                .optional()
                .map_err(|_| WorkflowError::persistence())?;
            if legacy_exists.is_some() {
                return Err(WorkflowError::new(
                    crate::WorkflowErrorCode::IdempotencyConflict,
                    false,
                    "legacy operation id has no authenticated principal namespace",
                ));
            }
        }
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
                "INSERT INTO workflow_operations
                 (operation_namespace, operation_id, operation_digest, response, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    operation_namespace,
                    operation_id,
                    operation_digest,
                    response_bytes,
                    now_ms
                ],
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
        tenant_id: &str,
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
                "SELECT sequence, payload FROM workflow_events
                 WHERE tenant_id = ?1 AND sequence > ?2 ORDER BY sequence ASC LIMIT ?3",
            )
            .map_err(|_| WorkflowError::persistence())?;
        let rows = statement
            .query_map(params![tenant_id, after_sequence, limit], |row| {
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

    pub fn projection_checkpoint(&self) -> Result<ProjectionCheckpoint, WorkflowError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| WorkflowError::persistence())?;
        read_projection_checkpoint(&connection)
    }

    /// Creates a transactionally consistent standalone SQLite image.
    /// The destination must not exist; callers persist the returned manifest
    /// next to the database and use it as the restore authorization input.
    pub fn backup_to(
        &self,
        destination: impl AsRef<Path>,
    ) -> Result<WorkflowBackupManifest, WorkflowError> {
        let destination = destination.as_ref();
        if destination.exists() {
            return Err(WorkflowError::new(
                crate::WorkflowErrorCode::BackupVerificationFailed,
                false,
                "workflow backup destination already exists",
            ));
        }
        let destination_text = destination
            .to_str()
            .ok_or_else(WorkflowError::persistence)?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| WorkflowError::persistence())?;
        let backup_result = connection
            .execute("VACUUM main INTO ?1", [destination_text])
            .map_err(|_| WorkflowError::persistence());
        drop(connection);
        if let Err(error) = backup_result {
            let _ = fs::remove_file(destination);
            return Err(error);
        }
        match inspect_backup(destination) {
            Ok(manifest) => Ok(manifest),
            Err(error) => {
                let _ = fs::remove_file(destination);
                Err(error)
            }
        }
    }

    /// Captures a verified portable image while holding the workflow store
    /// connection lock. The image is suitable for embedding in an
    /// application-level Time Machine snapshot.
    pub fn backup_image(&self) -> Result<WorkflowBackupImage, WorkflowError> {
        let path = std::env::temp_dir().join(format!(
            "sentinel-workflow-backup-{}.sqlite",
            uuid::Uuid::now_v7()
        ));
        let result = (|| {
            let manifest = self.backup_to(&path)?;
            let database = fs::read(&path).map_err(|_| WorkflowError::persistence())?;
            Ok(WorkflowBackupImage { manifest, database })
        })();
        let _ = fs::remove_file(path);
        result
    }

    /// Restores a verified image into the live SQLite connection. A verified
    /// pre-restore copy is retained until the restored image has been read
    /// back, so any failed restore is rolled back before returning.
    pub fn restore_image(&self, image: &WorkflowBackupImage) -> Result<(), WorkflowError> {
        let nonce = uuid::Uuid::now_v7();
        let source_path =
            std::env::temp_dir().join(format!("sentinel-workflow-restore-{nonce}.sqlite"));
        let rollback_path =
            std::env::temp_dir().join(format!("sentinel-workflow-rollback-{nonce}.sqlite"));
        let verify_path =
            std::env::temp_dir().join(format!("sentinel-workflow-verify-{nonce}.sqlite"));
        let result = (|| {
            fs::write(&source_path, &image.database).map_err(|_| WorkflowError::persistence())?;
            File::options()
                .write(true)
                .open(&source_path)
                .and_then(|file| file.sync_all())
                .map_err(|_| WorkflowError::persistence())?;
            if inspect_backup(&source_path)? != image.manifest {
                return Err(WorkflowError::new(
                    crate::WorkflowErrorCode::BackupVerificationFailed,
                    false,
                    "workflow restore image does not match its manifest",
                ));
            }

            let mut connection = self
                .connection
                .lock()
                .map_err(|_| WorkflowError::persistence())?;
            connection
                .backup(rusqlite::MAIN_DB, &rollback_path, None)
                .map_err(|_| WorkflowError::persistence())?;
            let restore_result = (|| {
                connection
                    .restore(
                        rusqlite::MAIN_DB,
                        &source_path,
                        None::<fn(rusqlite::backup::Progress)>,
                    )
                    .map_err(|_| WorkflowError::persistence())?;
                connection
                    .backup(rusqlite::MAIN_DB, &verify_path, None)
                    .map_err(|_| WorkflowError::persistence())?;
                // SQLite's online backup API may produce a different page
                // layout and change-counter while preserving the exact
                // logical database. The source image SHA above authorizes the
                // input bytes; the post-restore check must verify every
                // durable invariant rather than require page identity.
                if !inspect_backup(&verify_path)?.same_logical_state(&image.manifest) {
                    return Err(WorkflowError::new(
                        crate::WorkflowErrorCode::BackupVerificationFailed,
                        false,
                        "live workflow restore failed post-restore verification",
                    ));
                }
                Ok(())
            })();
            if let Err(error) = restore_result {
                connection
                    .restore(
                        rusqlite::MAIN_DB,
                        &rollback_path,
                        None::<fn(rusqlite::backup::Progress)>,
                    )
                    .map_err(|_| WorkflowError::persistence())?;
                return Err(error);
            }
            Ok(())
        })();
        let _ = fs::remove_file(source_path);
        let _ = fs::remove_file(rollback_path);
        let _ = fs::remove_file(verify_path);
        result
    }

    /// Restores a verified backup into a new, offline destination.
    /// Overwriting an existing or open database is deliberately unsupported.
    pub fn restore_from_backup(
        backup: impl AsRef<Path>,
        destination: impl AsRef<Path>,
        expected: &WorkflowBackupManifest,
    ) -> Result<(), WorkflowError> {
        let backup = backup.as_ref();
        let destination = destination.as_ref();
        if destination.exists() || inspect_backup(backup)? != *expected {
            return Err(WorkflowError::new(
                crate::WorkflowErrorCode::BackupVerificationFailed,
                false,
                "workflow backup manifest or restore destination is invalid",
            ));
        }
        let parent = destination
            .parent()
            .ok_or_else(WorkflowError::persistence)?;
        let file_name = destination
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(WorkflowError::persistence)?;
        let temporary = parent.join(format!(".{file_name}.restore-{}", uuid::Uuid::now_v7()));
        let restore_result = (|| -> Result<(), WorkflowError> {
            fs::copy(backup, &temporary).map_err(|_| WorkflowError::persistence())?;
            File::options()
                .write(true)
                .open(&temporary)
                .and_then(|file| file.sync_all())
                .map_err(|_| WorkflowError::persistence())?;
            if inspect_backup(&temporary)? != *expected {
                return Err(WorkflowError::new(
                    crate::WorkflowErrorCode::BackupVerificationFailed,
                    false,
                    "restored workflow image failed manifest verification",
                ));
            }
            fs::rename(&temporary, destination).map_err(|_| WorkflowError::persistence())?;
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| WorkflowError::persistence())?;
            Ok(())
        })();
        if restore_result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        restore_result
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

    pub fn pending_completion_evidence(
        &self,
        limit: usize,
    ) -> Result<Vec<PendingCompletionEvidence>, WorkflowError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| WorkflowError::persistence())?;
        let mut statement = connection
            .prepare(
                "SELECT request FROM workflow_completion_outbox
                 WHERE state = 'pending'
                 ORDER BY created_at_ms, request_id LIMIT ?1",
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

    pub fn completion_evidence_request(
        &self,
        request_id: &str,
    ) -> Result<Option<(PendingCompletionEvidence, String)>, WorkflowError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| WorkflowError::persistence())?;
        let row: Option<(Vec<u8>, String)> = connection
            .query_row(
                "SELECT request, state FROM workflow_completion_outbox WHERE request_id = ?1",
                [request_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|_| WorkflowError::persistence())?;
        row.map(|(payload, state)| Ok((serde_json::from_slice(&payload)?, state)))
            .transpose()
    }

    pub fn pending_event_publications(
        &self,
        limit: usize,
    ) -> Result<Vec<WorkflowEvent>, WorkflowError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| WorkflowError::persistence())?;
        let mut statement = connection
            .prepare(
                "SELECT events.sequence, events.payload
                 FROM workflow_event_outbox AS outbox
                 JOIN workflow_events AS events ON events.event_id = outbox.event_id
                 WHERE outbox.state = 'pending'
                 ORDER BY outbox.event_sequence
                 LIMIT ?1",
            )
            .map_err(|_| WorkflowError::persistence())?;
        let rows = statement
            .query_map([limit.clamp(1, 1_000) as i64], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
            })
            .map_err(|_| WorkflowError::persistence())?;
        let mut pending = Vec::new();
        for row in rows {
            let (sequence, payload) = row.map_err(|_| WorkflowError::persistence())?;
            let mut event: WorkflowEvent = serde_json::from_slice(&payload)?;
            event.sequence = sequence;
            pending.push(event);
        }
        Ok(pending)
    }

    pub fn mark_event_published(
        &self,
        event_id: &str,
        event_sequence: i64,
        now_ms: u64,
    ) -> Result<(), WorkflowError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| WorkflowError::persistence())?;
        let changed = connection
            .execute(
                "UPDATE workflow_event_outbox
                 SET state = 'published', attempts = attempts + 1,
                     last_error = NULL, published_at_ms = ?1
                 WHERE event_id = ?2 AND event_sequence = ?3
                   AND state IN ('pending', 'published')",
                params![sql_integer(now_ms)?, event_id, event_sequence],
            )
            .map_err(|_| WorkflowError::persistence())?;
        if changed != 1 {
            return Err(WorkflowError::new(
                crate::WorkflowErrorCode::PersistenceFailure,
                false,
                "workflow event publication identity is invalid",
            ));
        }
        Ok(())
    }

    pub fn mark_event_publish_failed(
        &self,
        event_id: &str,
        event_sequence: i64,
        error: &str,
    ) -> Result<(), WorkflowError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| WorkflowError::persistence())?;
        let changed = connection
            .execute(
                "UPDATE workflow_event_outbox
                 SET attempts = attempts + 1, last_error = ?1
                 WHERE event_id = ?2 AND event_sequence = ?3 AND state = 'pending'",
                params![bounded_error(error), event_id, event_sequence],
            )
            .map_err(|_| WorkflowError::persistence())?;
        if changed != 1 {
            return Err(WorkflowError::new(
                crate::WorkflowErrorCode::PersistenceFailure,
                false,
                "workflow event publication identity is invalid",
            ));
        }
        Ok(())
    }

    pub fn event_publication_state(&self) -> Result<(u64, i64), WorkflowError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| WorkflowError::persistence())?;
        let pending = connection
            .query_row(
                "SELECT COUNT(*) FROM workflow_event_outbox WHERE state = 'pending'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|_| WorkflowError::persistence())?
            .max(0) as u64;
        let high_watermark = connection
            .query_row(
                "SELECT COALESCE(MAX(event_sequence), 0)
                 FROM workflow_event_outbox WHERE state = 'published'",
                [],
                |row| row.get(0),
            )
            .map_err(|_| WorkflowError::persistence())?;
        Ok((pending, high_watermark))
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
                "INSERT INTO workflow_entity_history (entity_type, entity_id, version, payload)
                 VALUES (?1, ?2, ?3, ?4)",
                params![entity_type, entity_id, version, &payload],
            )
            .map_err(|_| WorkflowError::persistence())?;
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
                "INSERT INTO workflow_events
                 (event_id, event_type, aggregate_type, aggregate_id, tenant_id,
                  operation_id, payload, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    event.event_id,
                    format!("{:?}", event.event_type),
                    event.aggregate_type,
                    event.aggregate_id,
                    event.tenant_id,
                    event.operation_id,
                    payload,
                    timestamp_ms,
                ],
            )
            .map_err(|_| WorkflowError::persistence())?;
        let sequence = self.transaction.last_insert_rowid();
        event.sequence = sequence;
        self.transaction
            .execute(
                "INSERT INTO workflow_event_outbox
                 (event_id, event_sequence, state)
                 VALUES (?1, ?2, 'pending')",
                params![event.event_id, sequence],
            )
            .map_err(|_| WorkflowError::persistence())?;
        self.transaction
            .execute(
                "UPDATE workflow_projection_checkpoint
                 SET source_event_high_watermark = ?1,
                     projected_event_high_watermark = ?1
                 WHERE singleton = 1",
                [sequence],
            )
            .map_err(|_| WorkflowError::persistence())?;
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
        self.transaction
            .execute(
                "UPDATE workflow_projection_checkpoint
                 SET project_count = (SELECT COUNT(*) FROM workflow_project_projections)
                 WHERE singleton = 1",
                [],
            )
            .map_err(|_| WorkflowError::persistence())?;
        Ok(())
    }

    pub(crate) fn clear_projections(&self) -> Result<(), WorkflowError> {
        self.transaction
            .execute("DELETE FROM workflow_project_projections", [])
            .map_err(|_| WorkflowError::persistence())?;
        Ok(())
    }

    pub(crate) fn project_ids(&self) -> Result<Vec<ProjectId>, WorkflowError> {
        let mut statement = self
            .transaction
            .prepare(
                "SELECT entity_id FROM workflow_entities
                 WHERE entity_type = 'project' ORDER BY entity_id",
            )
            .map_err(|_| WorkflowError::persistence())?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|_| WorkflowError::persistence())?;
        let mut projects = Vec::new();
        for row in rows {
            projects.push(ProjectId(row.map_err(|_| WorkflowError::persistence())?));
        }
        Ok(projects)
    }

    pub(crate) fn event_high_watermark(&self) -> Result<i64, WorkflowError> {
        self.transaction
            .query_row(
                "SELECT COALESCE(MAX(sequence), 0) FROM workflow_events",
                [],
                |row| row.get(0),
            )
            .map_err(|_| WorkflowError::persistence())
    }

    pub(crate) fn mark_projection_rebuilt(&self, now_ms: u64) -> Result<(), WorkflowError> {
        let high_watermark = self.event_high_watermark()?;
        self.transaction
            .execute(
                "UPDATE workflow_projection_checkpoint
                 SET source_event_high_watermark = ?1,
                     projected_event_high_watermark = ?1,
                     project_count = (SELECT COUNT(*) FROM workflow_project_projections),
                     rebuilt_at_ms = ?2
                 WHERE singleton = 1",
                params![high_watermark, sql_integer(now_ms)?],
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

    pub(crate) fn execution_request(
        &self,
        invocation_id: &str,
    ) -> Result<(PendingExecution, String), WorkflowError> {
        let (payload, state): (Vec<u8>, String) = self
            .transaction
            .query_row(
                "SELECT request, state FROM workflow_execution_outbox WHERE invocation_id = ?1",
                [invocation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
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
        Ok((serde_json::from_slice(&payload)?, state))
    }

    pub(crate) fn mark_execution_authority_conflict(
        &self,
        invocation_id: &str,
        now_ms: u64,
    ) -> Result<(), WorkflowError> {
        let changed = self
            .transaction
            .execute(
                "UPDATE workflow_execution_outbox
             SET state = 'authority_conflict', attempts = attempts + 1,
                 last_error = 'organization_authority_conflict', updated_at_ms = ?1
             WHERE invocation_id = ?2 AND state = 'pending'",
                params![sql_integer(now_ms)?, invocation_id],
            )
            .map_err(|_| WorkflowError::persistence())?;
        if changed != 1 {
            return Err(WorkflowError::new(
                crate::WorkflowErrorCode::InvalidTransition,
                false,
                "execution outbox entry is no longer pending",
            ));
        }
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

    pub(crate) fn enqueue_completion_evidence(
        &self,
        request: &PendingCompletionEvidence,
    ) -> Result<bool, WorkflowError> {
        let payload = serde_json::to_vec(request)?;
        let now_ms = sql_integer(request.created_at_ms)?;
        let existing: Option<String> = self
            .transaction
            .query_row(
                "SELECT request_digest FROM workflow_completion_outbox
                 WHERE request_id = ?1 OR (invocation_id = ?2 AND assignment_version = ?3)",
                params![
                    request.query.request_id,
                    request.query.invocation_id,
                    sql_integer(request.query.assignment_version)?
                ],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| WorkflowError::persistence())?;
        if let Some(existing_digest) = existing {
            if existing_digest != request.query.request_digest {
                return Err(WorkflowError::new(
                    crate::WorkflowErrorCode::DigestConflict,
                    false,
                    "completion evidence request conflicts with a durable assignment request",
                ));
            }
            return Ok(false);
        }
        self.transaction
            .execute(
                "INSERT INTO workflow_completion_outbox
                 (request_id, invocation_id, project_id, work_item_id, assignment_version,
                  request_digest, request, state, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, ?8)",
                params![
                    request.query.request_id,
                    request.query.invocation_id,
                    request.query.project_id.0,
                    request.query.work_item_id.0,
                    sql_integer(request.query.assignment_version)?,
                    request.query.request_digest,
                    payload,
                    now_ms
                ],
            )
            .map_err(|_| WorkflowError::persistence())?;
        Ok(true)
    }

    pub(crate) fn completion_request_state(
        &self,
        request_id: &str,
    ) -> Result<String, WorkflowError> {
        self.transaction
            .query_row(
                "SELECT state FROM workflow_completion_outbox WHERE request_id = ?1",
                [request_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| WorkflowError::persistence())?
            .ok_or_else(|| {
                WorkflowError::new(
                    crate::WorkflowErrorCode::NotFound,
                    false,
                    "completion evidence request not found",
                )
            })
    }

    pub(crate) fn mark_completion_attempt_failed(
        &self,
        request_id: &str,
        error: &str,
        now_ms: u64,
        terminal: bool,
    ) -> Result<(), WorkflowError> {
        let attempts: i64 = self
            .transaction
            .query_row(
                "SELECT attempts FROM workflow_completion_outbox
                 WHERE request_id = ?1 AND state = 'pending'",
                [request_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| WorkflowError::persistence())?
            .ok_or_else(|| {
                WorkflowError::new(
                    crate::WorkflowErrorCode::InvalidTransition,
                    false,
                    "completion evidence request is not pending",
                )
            })?;
        let state = if terminal || attempts >= 2 {
            "failed"
        } else {
            "pending"
        };
        self.transaction
            .execute(
                "UPDATE workflow_completion_outbox
                 SET state = ?1, attempts = attempts + 1, last_error = ?2, updated_at_ms = ?3
                 WHERE request_id = ?4 AND state = 'pending'",
                params![state, error, sql_integer(now_ms)?, request_id],
            )
            .map_err(|_| WorkflowError::persistence())?;
        Ok(())
    }

    pub(crate) fn mark_completion_completed(
        &self,
        request_id: &str,
        authority_receipt_id: &str,
        now_ms: u64,
    ) -> Result<(), WorkflowError> {
        let changed = self
            .transaction
            .execute(
                "UPDATE workflow_completion_outbox
                 SET state = 'completed', authority_receipt_id = ?1, attempts = attempts + 1,
                     last_error = NULL, updated_at_ms = ?2
                 WHERE request_id = ?3 AND state = 'pending'",
                params![authority_receipt_id, sql_integer(now_ms)?, request_id],
            )
            .map_err(|_| WorkflowError::persistence())?;
        if changed != 1 {
            return Err(WorkflowError::new(
                crate::WorkflowErrorCode::InvalidTransition,
                false,
                "completion evidence request is no longer pending",
            ));
        }
        Ok(())
    }
}

fn migrate_v1_to_v2(
    connection: &mut Connection,
    fail_after_step: Option<usize>,
) -> Result<(), WorkflowError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| WorkflowError::persistence())?;
    for (index, step) in MIGRATE_V1_TO_V2_STEPS.iter().enumerate() {
        transaction
            .execute_batch(step)
            .map_err(|_| WorkflowError::persistence())?;
        if fail_after_step == Some(index) {
            return Err(WorkflowError::new(
                crate::WorkflowErrorCode::PersistenceFailure,
                true,
                "injected workflow migration interruption",
            ));
        }
    }
    transaction
        .commit()
        .map_err(|_| WorkflowError::persistence())
}

fn migrate_v2_to_v3(
    connection: &mut Connection,
    fail_after_step: Option<usize>,
) -> Result<(), WorkflowError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| WorkflowError::persistence())?;
    for (index, step) in MIGRATE_V2_TO_V3_STEPS.iter().enumerate() {
        transaction
            .execute_batch(step)
            .map_err(|_| WorkflowError::persistence())?;
        if fail_after_step == Some(index) {
            return Err(WorkflowError::new(
                crate::WorkflowErrorCode::PersistenceFailure,
                true,
                "injected workflow migration interruption",
            ));
        }
    }
    transaction
        .commit()
        .map_err(|_| WorkflowError::persistence())
}

fn sql_integer(value: u64) -> Result<i64, WorkflowError> {
    i64::try_from(value).map_err(|_| WorkflowError::persistence())
}

fn read_projection_checkpoint(
    connection: &Connection,
) -> Result<ProjectionCheckpoint, WorkflowError> {
    connection
        .query_row(
            "SELECT source_event_high_watermark, projected_event_high_watermark,
                    project_count, rebuilt_at_ms
             FROM workflow_projection_checkpoint WHERE singleton = 1",
            [],
            |row| {
                let rebuilt_at_ms: Option<i64> = row.get(3)?;
                Ok(ProjectionCheckpoint {
                    source_event_high_watermark: row.get(0)?,
                    projected_event_high_watermark: row.get(1)?,
                    project_count: row.get::<_, i64>(2)?.max(0) as u64,
                    rebuilt_at_ms: rebuilt_at_ms.and_then(|value| u64::try_from(value).ok()),
                })
            },
        )
        .map_err(|_| WorkflowError::persistence())
}

fn inspect_backup(path: &Path) -> Result<WorkflowBackupManifest, WorkflowError> {
    let database_sha256 = file_sha256(path)?;
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| WorkflowError::persistence())?;
    let integrity: String = connection
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|_| WorkflowError::persistence())?;
    if integrity != "ok" {
        return Err(WorkflowError::new(
            crate::WorkflowErrorCode::BackupVerificationFailed,
            false,
            "workflow backup failed SQLite integrity verification",
        ));
    }
    let schema_version: u32 = connection
        .query_row(
            "SELECT schema_version FROM workflow_schema_meta WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(|_| WorkflowError::persistence())?;
    if schema_version != WORKFLOW_STORE_SCHEMA_VERSION {
        return Err(WorkflowError::new(
            crate::WorkflowErrorCode::BackupVerificationFailed,
            false,
            "workflow backup schema version is unsupported",
        ));
    }
    let event_high_watermark = connection
        .query_row(
            "SELECT COALESCE(MAX(sequence), 0) FROM workflow_events",
            [],
            |row| row.get(0),
        )
        .map_err(|_| WorkflowError::persistence())?;
    let entity_history_high_watermark = connection
        .query_row(
            "SELECT COALESCE(MAX(mutation_sequence), 0) FROM workflow_entity_history",
            [],
            |row| row.get(0),
        )
        .map_err(|_| WorkflowError::persistence())?;
    let entity_count = table_count(&connection, "workflow_entities")?;
    let operation_count = connection
        .query_row("SELECT COUNT(*) FROM workflow_operations", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|_| WorkflowError::persistence())?
        .max(0) as u64;
    let execution_outbox_count = table_count(&connection, "workflow_execution_outbox")?;
    let completion_outbox_count = table_count(&connection, "workflow_completion_outbox")?;
    let event_publication_pending_count = connection
        .query_row(
            "SELECT COUNT(*) FROM workflow_event_outbox WHERE state = 'pending'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|_| WorkflowError::persistence())?
        .max(0) as u64;
    let event_publication_published_count = connection
        .query_row(
            "SELECT COUNT(*) FROM workflow_event_outbox WHERE state = 'published'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|_| WorkflowError::persistence())?
        .max(0) as u64;
    let event_publication_high_watermark = connection
        .query_row(
            "SELECT COALESCE(MAX(event_sequence), 0)
             FROM workflow_event_outbox WHERE state = 'published'",
            [],
            |row| row.get(0),
        )
        .map_err(|_| WorkflowError::persistence())?;
    let unjournaled_events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM workflow_events AS events
             LEFT JOIN workflow_event_outbox AS outbox
               ON outbox.event_id = events.event_id
              AND outbox.event_sequence = events.sequence
             WHERE outbox.event_id IS NULL",
            [],
            |row| row.get(0),
        )
        .map_err(|_| WorkflowError::persistence())?;
    if unjournaled_events != 0 {
        return Err(WorkflowError::new(
            crate::WorkflowErrorCode::BackupVerificationFailed,
            false,
            "workflow event history is not fully represented in the publication outbox",
        ));
    }
    let project_projection_count = table_count(&connection, "workflow_project_projections")?;
    let current_entities_without_history: i64 = connection
        .query_row(
            "SELECT COUNT(*)
             FROM workflow_entities AS current
             LEFT JOIN workflow_entity_history AS history
               ON history.entity_type = current.entity_type
              AND history.entity_id = current.entity_id
              AND history.version = current.version
              AND history.payload = current.payload
             WHERE history.mutation_sequence IS NULL",
            [],
            |row| row.get(0),
        )
        .map_err(|_| WorkflowError::persistence())?;
    if current_entities_without_history != 0 {
        return Err(WorkflowError::new(
            crate::WorkflowErrorCode::BackupVerificationFailed,
            false,
            "workflow current state is not backed by immutable entity history",
        ));
    }
    let projection_checkpoint = read_projection_checkpoint(&connection)?;
    if projection_checkpoint.source_event_high_watermark != event_high_watermark
        || projection_checkpoint.projected_event_high_watermark != event_high_watermark
        || projection_checkpoint.project_count != project_projection_count
    {
        return Err(WorkflowError::new(
            crate::WorkflowErrorCode::BackupVerificationFailed,
            false,
            "workflow projection checkpoint is not caught up to the event store",
        ));
    }
    Ok(WorkflowBackupManifest {
        schema_version,
        database_sha256,
        event_high_watermark,
        entity_history_high_watermark,
        entity_count,
        operation_count,
        execution_outbox_count,
        completion_outbox_count,
        event_publication_pending_count,
        event_publication_published_count,
        event_publication_high_watermark,
        project_projection_count,
        projection_checkpoint,
    })
}

fn table_count(connection: &Connection, table: &str) -> Result<u64, WorkflowError> {
    let sql = match table {
        "workflow_entities" => "SELECT COUNT(*) FROM workflow_entities",
        "workflow_execution_outbox" => "SELECT COUNT(*) FROM workflow_execution_outbox",
        "workflow_completion_outbox" => "SELECT COUNT(*) FROM workflow_completion_outbox",
        "workflow_project_projections" => "SELECT COUNT(*) FROM workflow_project_projections",
        _ => return Err(WorkflowError::persistence()),
    };
    connection
        .query_row(sql, [], |row| row.get::<_, i64>(0))
        .map_err(|_| WorkflowError::persistence())
        .map(|count| count.max(0) as u64)
}

fn bounded_error(error: &str) -> String {
    error.chars().take(512).collect()
}

fn file_sha256(path: &Path) -> Result<String, WorkflowError> {
    use std::fmt::Write as _;

    let mut file = File::open(path).map_err(|_| WorkflowError::persistence())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|_| WorkflowError::persistence())?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let mut digest = String::with_capacity(64);
    for byte in hasher.finalize() {
        write!(&mut digest, "{byte:02x}").map_err(|_| WorkflowError::persistence())?;
    }
    Ok(digest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{WorkflowErrorCode, WorkflowResponse};

    fn create_version_one_database(path: &Path) {
        let response = WorkflowResponse::WorkItems(Vec::new());
        let response_bytes = serde_json::to_vec(&response).expect("serialize response");
        let connection = Connection::open(path).expect("create version 1 database");
        connection
            .execute_batch(
                "CREATE TABLE workflow_schema_meta (
                     singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                     schema_version INTEGER NOT NULL
                 );
                 INSERT INTO workflow_schema_meta VALUES (1, 1);
                 CREATE TABLE workflow_entities (
                     entity_type TEXT NOT NULL,
                     entity_id TEXT NOT NULL,
                     version INTEGER NOT NULL,
                     payload BLOB NOT NULL,
                     PRIMARY KEY (entity_type, entity_id)
                 );
                 CREATE TABLE workflow_operations (
                     operation_id TEXT PRIMARY KEY,
                     operation_digest TEXT NOT NULL,
                     response BLOB NOT NULL,
                     created_at_ms INTEGER NOT NULL
                 );
                 CREATE TABLE workflow_events (
                     sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                     event_id TEXT NOT NULL UNIQUE,
                     event_type TEXT NOT NULL,
                     aggregate_type TEXT NOT NULL,
                     aggregate_id TEXT NOT NULL,
                     operation_id TEXT NOT NULL,
                     payload BLOB NOT NULL,
                     created_at_ms INTEGER NOT NULL
                 );",
            )
            .expect("create version 1 schema");
        connection
            .execute(
                "INSERT INTO workflow_entities VALUES ('request', 'request-v1', 1, ?1)",
                [b"{}".as_slice()],
            )
            .expect("insert legacy entity");
        connection
            .execute(
                "INSERT INTO workflow_operations VALUES ('legacy-operation', 'legacy-digest', ?1, 1)",
                [response_bytes],
            )
            .expect("insert legacy operation");
        connection
            .execute(
                "INSERT INTO workflow_events
                 (event_id, event_type, aggregate_type, aggregate_id, operation_id, payload, created_at_ms)
                 VALUES ('event-v1', 'legacy', 'request', 'request-v1', 'legacy-operation', ?1, 1)",
                [b"{}".as_slice()],
            )
            .expect("insert legacy event");
    }

    #[test]
    fn every_version_one_migration_failpoint_rolls_back_and_retries_cleanly() {
        let directory = tempfile::tempdir().expect("tempdir");
        for fail_after_step in 0..MIGRATE_V1_TO_V2_STEPS.len() {
            let database = directory
                .path()
                .join(format!("workflow-v1-failpoint-{fail_after_step}.db"));
            create_version_one_database(&database);
            let error =
                WorkflowStore::open_with_migration_failpoint(&database, Some(fail_after_step))
                    .expect_err("injected migration interruption must fail");
            assert_eq!(error.code, WorkflowErrorCode::PersistenceFailure);
            let connection = Connection::open(&database).expect("inspect rolled back database");
            let schema_version: u32 = connection
                .query_row(
                    "SELECT schema_version FROM workflow_schema_meta WHERE singleton = 1",
                    [],
                    |row| row.get(0),
                )
                .expect("version marker");
            assert_eq!(schema_version, 1, "failpoint {fail_after_step}");
            let legacy_operations: i64 = connection
                .query_row("SELECT COUNT(*) FROM workflow_operations", [], |row| {
                    row.get(0)
                })
                .expect("legacy operation table restored");
            let legacy_events: i64 = connection
                .query_row("SELECT COUNT(*) FROM workflow_events", [], |row| row.get(0))
                .expect("legacy event table restored");
            assert_eq!(legacy_operations, 1, "failpoint {fail_after_step}");
            assert_eq!(legacy_events, 1, "failpoint {fail_after_step}");
            drop(connection);

            let recovered = WorkflowStore::open(&database)
                .expect("next startup deterministically retries the whole migration");
            assert_eq!(
                recovered
                    .projection_checkpoint()
                    .expect("recovered checkpoint")
                    .source_event_high_watermark,
                1
            );
        }
    }

    #[test]
    fn every_version_two_migration_failpoint_rolls_back_and_retries_cleanly() {
        let directory = tempfile::tempdir().expect("tempdir");
        for fail_after_step in 0..MIGRATE_V2_TO_V3_STEPS.len() {
            let database = directory
                .path()
                .join(format!("workflow-v2-failpoint-{fail_after_step}.db"));
            create_version_one_database(&database);
            {
                let mut connection = Connection::open(&database).expect("open version 1 database");
                migrate_v1_to_v2(&mut connection, None).expect("create version 2 database");
            }

            let error = WorkflowStore::open_with_migration_failpoints(
                &database,
                None,
                Some(fail_after_step),
            )
            .expect_err("injected migration interruption must fail");
            assert_eq!(error.code, WorkflowErrorCode::PersistenceFailure);
            let connection = Connection::open(&database).expect("inspect rolled back database");
            let schema_version: u32 = connection
                .query_row(
                    "SELECT schema_version FROM workflow_schema_meta WHERE singleton = 1",
                    [],
                    |row| row.get(0),
                )
                .expect("version marker");
            assert_eq!(schema_version, 2, "failpoint {fail_after_step}");
            let outbox_exists: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master
                     WHERE type = 'table' AND name = 'workflow_event_outbox'",
                    [],
                    |row| row.get(0),
                )
                .expect("outbox existence");
            assert_eq!(outbox_exists, 0, "failpoint {fail_after_step}");
            drop(connection);

            let recovered =
                WorkflowStore::open(&database).expect("next startup retries version 3 migration");
            assert_eq!(
                recovered.event_publication_state().expect("outbox state"),
                (1, 0)
            );
        }
    }

    #[test]
    fn version_one_migration_preserves_replay_history_and_backup_watermarks() {
        let directory = tempfile::tempdir().expect("tempdir");
        let database = directory.path().join("workflow-v1.db");
        let backup = directory.path().join("workflow-v2.backup.db");
        let response = WorkflowResponse::WorkItems(Vec::new());
        let response_bytes = serde_json::to_vec(&response).expect("serialize response");
        {
            let connection = Connection::open(&database).expect("create version 1 database");
            connection
                .execute_batch(
                    "CREATE TABLE workflow_schema_meta (
                         singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                         schema_version INTEGER NOT NULL
                     );
                     INSERT INTO workflow_schema_meta VALUES (1, 1);
                     CREATE TABLE workflow_entities (
                         entity_type TEXT NOT NULL,
                         entity_id TEXT NOT NULL,
                         version INTEGER NOT NULL,
                         payload BLOB NOT NULL,
                         PRIMARY KEY (entity_type, entity_id)
                     );
                     CREATE TABLE workflow_operations (
                         operation_id TEXT PRIMARY KEY,
                         operation_digest TEXT NOT NULL,
                         response BLOB NOT NULL,
                         created_at_ms INTEGER NOT NULL
                     );
                     CREATE TABLE workflow_events (
                         sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                         event_id TEXT NOT NULL UNIQUE,
                         event_type TEXT NOT NULL,
                         aggregate_type TEXT NOT NULL,
                         aggregate_id TEXT NOT NULL,
                         operation_id TEXT NOT NULL,
                         payload BLOB NOT NULL,
                         created_at_ms INTEGER NOT NULL
                     );",
                )
                .expect("create version 1 schema");
            connection
                .execute(
                    "INSERT INTO workflow_entities VALUES ('request', 'request-v1', 1, ?1)",
                    [b"{}".as_slice()],
                )
                .expect("insert legacy entity");
            connection
                .execute(
                    "INSERT INTO workflow_operations VALUES ('legacy-operation', 'legacy-digest', ?1, 1)",
                    [response_bytes],
                )
                .expect("insert legacy operation");
            connection
                .execute(
                    "INSERT INTO workflow_events
                     (event_id, event_type, aggregate_type, aggregate_id, operation_id, payload, created_at_ms)
                     VALUES ('event-v1', 'legacy', 'request', 'request-v1', 'legacy-operation', ?1, 1)",
                    [b"{}".as_slice()],
                )
                .expect("insert legacy event");
        }

        let store = WorkflowStore::open(&database).expect("migrate version 1 database");
        let checkpoint = store
            .projection_checkpoint()
            .expect("read migrated checkpoint");
        assert_eq!(checkpoint.source_event_high_watermark, 1);
        assert_eq!(checkpoint.projected_event_high_watermark, 1);
        let replay = store
            .execute(
                "tenant-a:customer:principal-a",
                "legacy-operation",
                "legacy-digest",
                2,
                |_| panic!("legacy replay must not apply again"),
            )
            .expect_err("legacy response cannot cross an unprovable principal boundary");
        assert_eq!(replay.code, WorkflowErrorCode::IdempotencyConflict);
        let conflict = store
            .execute(
                "tenant-b:customer:principal-b",
                "legacy-operation",
                "different-digest",
                2,
                |_| panic!("legacy conflict must not apply"),
            )
            .expect_err("legacy operation ID remains globally fail closed");
        assert_eq!(conflict.code, WorkflowErrorCode::IdempotencyConflict);
        let manifest = store.backup_to(&backup).expect("backup migrated store");
        assert_eq!(manifest.schema_version, WORKFLOW_STORE_SCHEMA_VERSION);
        assert_eq!(manifest.event_high_watermark, 1);
        assert_eq!(manifest.entity_history_high_watermark, 1);
        assert_eq!(manifest.entity_count, 1);
        assert_eq!(manifest.operation_count, 1);
        assert_eq!(manifest.execution_outbox_count, 0);
        assert_eq!(manifest.completion_outbox_count, 0);
        assert_eq!(manifest.event_publication_pending_count, 1);
        assert_eq!(manifest.event_publication_published_count, 0);
        assert_eq!(manifest.event_publication_high_watermark, 0);
        assert_eq!(manifest.project_projection_count, 0);

        let rejected_backup = directory.path().join("workflow-stale-projection.db");
        store
            .connection
            .lock()
            .expect("store connection")
            .execute(
                "UPDATE workflow_projection_checkpoint
                 SET projected_event_high_watermark = 0 WHERE singleton = 1",
                [],
            )
            .expect("make checkpoint stale");
        let rejected = store
            .backup_to(&rejected_backup)
            .expect_err("backup requires a caught-up projection checkpoint");
        assert_eq!(rejected.code, WorkflowErrorCode::BackupVerificationFailed);
        assert!(!rejected_backup.exists());
    }

    #[test]
    fn verified_backup_image_restores_the_live_store_atomically() {
        let directory = tempfile::tempdir().expect("tempdir");
        let database = directory.path().join("workflow-v1.db");
        create_version_one_database(&database);
        let store = WorkflowStore::open(&database).expect("migrated store");
        let image = store.backup_image().expect("verified image");

        {
            let connection = store.connection.lock().expect("store connection");
            connection
                .execute("DELETE FROM workflow_event_outbox", [])
                .expect("delete outbox");
            connection
                .execute("DELETE FROM workflow_events", [])
                .expect("delete events");
            connection
                .execute("DELETE FROM workflow_entity_history", [])
                .expect("delete history");
            connection
                .execute("DELETE FROM workflow_entities", [])
                .expect("delete entities");
        }
        assert!(store
            .entity::<serde_json::Value>("request", "request-v1")
            .expect("read mutated store")
            .is_none());

        store.restore_image(&image).expect("atomic live restore");
        assert!(store
            .entity::<serde_json::Value>("request", "request-v1")
            .expect("read restored store")
            .is_some());
        assert!(store
            .backup_image()
            .expect("post-restore image")
            .manifest
            .same_logical_state(&image.manifest));
    }
}
