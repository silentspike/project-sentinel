//! Append-only rounds reuse the existing FULL/WAL operation journal and backup boundary.

use super::*;
use crate::{AdaptiveSessionGrantV1, AdaptiveSessionV1, AdaptiveTransitionV1};
use serde::Deserialize;

const MAX_JOURNAL_ENTRIES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    previous_digest: Option<String>,
    command: Option<AdaptiveTransitionV1>,
    session: AdaptiveSessionV1,
}

impl WorkflowStore {
    /// Internal bookkeeping only: no provider grant or work-item completion is created.
    pub fn begin_adaptive_session(
        &self,
        grant: &AdaptiveSessionGrantV1,
        current: &RuntimeAuthoritySnapshotV1,
        now_ms: u64,
    ) -> Result<(bool, AdaptiveSessionV1), WorkflowError> {
        authorize(grant, current)?;
        let namespace = namespace(grant.session_id);
        let mut connection = self.lock()?;
        let tx = immediate(&mut connection)?;
        if let Some((existing, _)) = load(&tx, grant.session_id)? {
            if existing.grant != *grant {
                return Err(idempotency_conflict());
            }
            require_head(&tx, &existing)?;
            return Ok((true, existing));
        }
        if let Some(existing) = read_head(&tx, current)? {
            if existing.session_id != grant.session_id {
                return Err(idempotency_conflict());
            }
            return Err(corrupt_store());
        }
        if now_ms != grant.created_at_ms || now_ms >= grant.deadline_ms {
            return Err(authority_conflict());
        }
        let session = AdaptiveSessionV1::initial(grant.clone())?;
        append(
            &tx,
            &namespace,
            &Entry {
                previous_digest: None,
                command: None,
                session: session.clone(),
            },
        )?;
        insert_head(&tx, &session)?;
        tx.commit().map_err(map_sqlite_error)?;
        Ok((false, session))
    }

    pub fn adaptive_session(
        &self,
        session_id: Uuid,
        current: &RuntimeAuthoritySnapshotV1,
    ) -> Result<Option<AdaptiveSessionV1>, WorkflowError> {
        current.validate()?;
        let connection = self.lock()?;
        let Some((session, _)) = load(&connection, session_id)? else {
            return Ok(None);
        };
        authorize(&session.grant, current)?;
        require_head(&connection, &session)?;
        Ok(Some(session))
    }

    /// Resolves the only session owned by the exact current runtime authority.
    pub fn adaptive_session_for_authority(
        &self,
        current: &RuntimeAuthoritySnapshotV1,
    ) -> Result<Option<AdaptiveSessionV1>, WorkflowError> {
        current.validate()?;
        let connection = self.lock()?;
        let Some(head) = read_head(&connection, current)? else {
            return Ok(None);
        };
        let (session, _) = load(&connection, head.session_id)?.ok_or_else(corrupt_store)?;
        authorize(&session.grant, current)?;
        validate_head(&head, &session)?;
        Ok(Some(session))
    }

    /// State and idempotent response commit together, before any caller dispatches an effect.
    pub fn advance_adaptive_session(
        &self,
        session_id: Uuid,
        expected_version: u64,
        operation_id: Uuid,
        command: &AdaptiveTransitionV1,
        current: &RuntimeAuthoritySnapshotV1,
        now_ms: u64,
    ) -> Result<(bool, AdaptiveSessionV1), WorkflowError> {
        current.validate()?;
        if operation_id.is_nil() {
            return Err(authority_conflict());
        }
        let ns = namespace(session_id);
        let operations = format!("{ns}:operations");
        let digest = canonical_sha256(
            "sentinel.workflow.adaptive-command.v1",
            &(session_id, expected_version, command),
        )?;
        let mut connection = self.lock()?;
        let tx = immediate(&mut connection)?;
        let (session, prior_digest) = load(&tx, session_id)?.ok_or_else(not_found)?;
        authorize(&session.grant, current)?;
        require_head(&tx, &session)?;
        // Replay is checked before expiry/version, but never before current authorization.
        if let Some((stored_digest, bytes, created)) =
            read_operation(&tx, &operations, &operation_id.to_string())?
        {
            if !constant_time_eq(&stored_digest, &digest) {
                return Err(idempotency_conflict());
            }
            let response: AdaptiveSessionV1 = decode(&bytes)?;
            let (_, entry_bytes, _) =
                read_operation(&tx, &ns, &format!("{:020}", response.version))?
                    .ok_or_else(corrupt_store)?;
            let entry: Entry = decode(&entry_bytes)?;
            if response != entry.session
                || entry.command.as_ref() != Some(command)
                || expected_version.checked_add(1) != Some(response.version)
                || stored_u64(created)? != response.updated_at_ms
            {
                return Err(corrupt_store());
            }
            return Ok((true, response));
        }
        if session.version != expected_version {
            return Err(WorkflowError::new(
                WorkflowErrorCode::VersionConflict,
                false,
                "adaptive session version changed",
            ));
        }
        let next = session.transition(command, now_ms)?;
        append(
            &tx,
            &ns,
            &Entry {
                previous_digest: Some(prior_digest),
                command: Some(command.clone()),
                session: next.clone(),
            },
        )?;
        insert_operation(
            &tx,
            &operations,
            &operation_id.to_string(),
            &digest,
            &next,
            now_ms,
        )?;
        update_head(&tx, &session, &next)?;
        tx.commit().map_err(map_sqlite_error)?;
        Ok((false, next))
    }
}

struct AdaptiveHead {
    session_id: Uuid,
    version: u64,
    updated_at_ms: u64,
}

fn read_head(
    connection: &Connection,
    authority: &RuntimeAuthoritySnapshotV1,
) -> Result<Option<AdaptiveHead>, WorkflowError> {
    let authority_digest = authority.canonical_digest()?;
    connection
        .query_row(
            "SELECT session_id,version,updated_at_ms FROM workflow_adaptive_heads WHERE tenant_id=?1 AND project_id=?2 AND work_item_id=?3 AND agent_id=?4 AND authority_digest=?5",
            params![
                authority.tenant_id.to_string(),
                authority.project_id.to_string(),
                authority.work_item_id.to_string(),
                i64::from(authority.agent_id.0),
                authority_digest,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()
        .map_err(map_sqlite_error)?
        .map(|(session_id, version, updated_at_ms)| {
            Ok(AdaptiveHead {
                session_id: Uuid::parse_str(&session_id).map_err(|_| corrupt_store())?,
                version: stored_u64(version)?,
                updated_at_ms: stored_u64(updated_at_ms)?,
            })
        })
        .transpose()
}

fn insert_head(tx: &Transaction<'_>, session: &AdaptiveSessionV1) -> Result<(), WorkflowError> {
    let authority = &session.grant.authority;
    tx.execute(
        "INSERT INTO workflow_adaptive_heads (tenant_id,project_id,work_item_id,agent_id,authority_digest,session_id,version,updated_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            authority.tenant_id.to_string(),
            authority.project_id.to_string(),
            authority.work_item_id.to_string(),
            i64::from(authority.agent_id.0),
            authority.canonical_digest()?,
            session.grant.session_id.to_string(),
            sql_u64(session.version)?,
            sql_u64(session.updated_at_ms)?,
        ],
    )
    .map_err(map_sqlite_error)?;
    Ok(())
}

fn update_head(
    tx: &Transaction<'_>,
    previous: &AdaptiveSessionV1,
    next: &AdaptiveSessionV1,
) -> Result<(), WorkflowError> {
    let changed = tx
        .execute(
            "UPDATE workflow_adaptive_heads SET version=?1,updated_at_ms=?2 WHERE session_id=?3 AND version=?4 AND updated_at_ms=?5",
            params![
                sql_u64(next.version)?,
                sql_u64(next.updated_at_ms)?,
                next.grant.session_id.to_string(),
                sql_u64(previous.version)?,
                sql_u64(previous.updated_at_ms)?,
            ],
        )
        .map_err(map_sqlite_error)?;
    if changed != 1 {
        return Err(corrupt_store());
    }
    Ok(())
}

fn require_head(connection: &Connection, session: &AdaptiveSessionV1) -> Result<(), WorkflowError> {
    let head = read_head(connection, &session.grant.authority)?.ok_or_else(corrupt_store)?;
    validate_head(&head, session)
}

fn validate_head(head: &AdaptiveHead, session: &AdaptiveSessionV1) -> Result<(), WorkflowError> {
    if head.session_id != session.grant.session_id
        || head.version != session.version
        || head.updated_at_ms != session.updated_at_ms
    {
        return Err(corrupt_store());
    }
    Ok(())
}

fn namespace(id: Uuid) -> String {
    format!("adaptive-session-v1:{id}")
}

fn authorize(
    grant: &AdaptiveSessionGrantV1,
    current: &RuntimeAuthoritySnapshotV1,
) -> Result<(), WorkflowError> {
    grant.validate()?;
    current.validate()?;
    if grant.authority != *current {
        return Err(authority_conflict());
    }
    Ok(())
}

fn not_found() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::NotFound,
        false,
        "adaptive session was not found",
    )
}

fn append(tx: &Transaction<'_>, ns: &str, entry: &Entry) -> Result<(), WorkflowError> {
    if entry.session.version > MAX_JOURNAL_ENTRIES as u64 {
        return Err(corrupt_store());
    }
    let digest = canonical_sha256("sentinel.workflow.adaptive-entry.v1", entry)?;
    insert_operation(
        tx,
        ns,
        &format!("{:020}", entry.session.version),
        &digest,
        entry,
        entry.session.updated_at_ms,
    )
}

fn load(
    connection: &Connection,
    id: Uuid,
) -> Result<Option<(AdaptiveSessionV1, String)>, WorkflowError> {
    let mut statement = connection.prepare(
        "SELECT operation_id, request_digest, response, created_at_ms FROM workflow_operations WHERE operation_namespace=?1 ORDER BY operation_id LIMIT ?2"
    ).map_err(map_sqlite_error)?;
    let mut rows = statement
        .query(params![namespace(id), (MAX_JOURNAL_ENTRIES + 1) as i64])
        .map_err(map_sqlite_error)?;
    let mut previous: Option<(AdaptiveSessionV1, String)> = None;
    let mut count = 0;
    while let Some(row) = rows.next().map_err(map_sqlite_error)? {
        count += 1;
        let key: String = row.get(0).map_err(map_sqlite_error)?;
        let digest: String = row.get(1).map_err(map_sqlite_error)?;
        let bytes: Vec<u8> = row.get(2).map_err(map_sqlite_error)?;
        let created: i64 = row.get(3).map_err(map_sqlite_error)?;
        let entry: Entry = decode(&bytes)?;
        let recomputed = canonical_sha256("sentinel.workflow.adaptive-entry.v1", &entry)?;
        if count > MAX_JOURNAL_ENTRIES
            || entry.session.grant.session_id != id
            || key != format!("{:020}", entry.session.version)
            || !constant_time_eq(&digest, &recomputed)
            || stored_u64(created)? != entry.session.updated_at_ms
        {
            return Err(corrupt_store());
        }
        let expected = match &previous {
            None if entry.previous_digest.is_none() && entry.command.is_none() => {
                AdaptiveSessionV1::initial(entry.session.grant.clone())
            }
            Some((prior, prior_digest)) if entry.previous_digest.as_ref() == Some(prior_digest) => {
                prior.transition(
                    entry.command.as_ref().ok_or_else(corrupt_store)?,
                    entry.session.updated_at_ms,
                )
            }
            _ => return Err(corrupt_store()),
        }
        .map_err(|_| corrupt_store())?;
        if expected != entry.session {
            return Err(corrupt_store());
        }
        previous = Some((entry.session, digest));
    }
    Ok(previous)
}
