//! Append-only rounds reuse the existing FULL/WAL operation journal and backup boundary.

use super::*;
use crate::{
    adaptive_collaboration_digest, AdaptiveEffectV1, AdaptiveModelDecisionV1,
    AdaptiveRecoveryFeedbackV1, AdaptiveSessionGrantV1, AdaptiveSessionV1, AdaptiveTransitionV1,
    ADAPTIVE_SCHEMA_MAX_CORRECTIONS,
};
use serde::Deserialize;

const MAX_JOURNAL_ENTRIES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    previous_digest: Option<String>,
    command: Option<AdaptiveTransitionV1>,
    session: AdaptiveSessionV1,
    // Only the initial entry carries lineage. Omitting None preserves old entry digests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recovery_feedback: Option<AdaptiveRecoveryFeedbackV1>,
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
            if read_head(&tx, current)?.is_some_and(|head| head.session_id != grant.session_id) {
                return Err(idempotency_conflict());
            }
            require_head(&tx, &existing)?;
            return Ok((true, existing));
        }
        if now_ms < grant.created_at_ms || now_ms >= grant.deadline_ms {
            return Err(authority_conflict());
        }
        let previous = if let Some(head) = read_head(&tx, current)? {
            if head.session_id == grant.session_id {
                return Err(corrupt_store());
            }
            let (previous, previous_digest, feedback) =
                load_with_feedback(&tx, head.session_id)?.ok_or_else(corrupt_store)?;
            authorize(&previous.grant, current)?;
            validate_head(&head, &previous)?;
            // Never replace unresolved effects or implicitly clear a blocked result.
            let never_claimed = previous.version == 1
                && previous.model_calls == 0
                && previous.tool_calls == 0
                && matches!(previous.cursor, crate::AdaptiveCursorV1::ReadyForModel)
                && previous.last_observation.is_none()
                && previous.last_model_result_digest.is_none()
                && previous.effect_ids.is_empty();
            let rejected_first_model = matches!(previous.version, 3 | 4)
                && previous.model_calls == 1
                && previous.tool_calls == 0
                && matches!(
                    previous.cursor,
                    crate::AdaptiveCursorV1::ModelRejected { .. }
                )
                && previous.last_observation.is_none()
                && previous.last_model_result_digest.is_none()
                && previous.effect_ids.len() == 1;
            let blocked_resolved = matches!(
                previous.cursor,
                crate::AdaptiveCursorV1::BlockedResolved { .. }
            );
            if !(never_claimed || rejected_first_model || blocked_resolved)
                || previous.grant.provider_allowance_id == grant.provider_allowance_id
                || previous.grant.deadline_ms > now_ms
            {
                return Err(idempotency_conflict());
            }
            let feedback = if rejected_first_model {
                let count = feedback.as_ref().map_or(0, |feedback| feedback.count);
                if count >= ADAPTIVE_SCHEMA_MAX_CORRECTIONS {
                    return Err(idempotency_conflict());
                }
                let crate::AdaptiveCursorV1::ModelRejected {
                    reason_code,
                    resolution_event_id,
                } = &previous.cursor
                else {
                    return Err(corrupt_store());
                };
                Some(AdaptiveRecoveryFeedbackV1 {
                    count: count + 1,
                    reason_code: reason_code.clone(),
                    resolution_event_id: resolution_event_id.clone(),
                    previous_session_id: previous.grant.session_id,
                })
            } else {
                feedback
            };
            Some((previous, previous_digest, feedback))
        } else {
            None
        };
        let mut session = AdaptiveSessionV1::initial(grant.clone())?;
        // A grant can predate the resolution that made rollover safe. Use a recorded
        // journal time as the floor, without rewriting the grant's creation time.
        session.updated_at_ms = previous
            .as_ref()
            .map_or(now_ms, |(prior, _, _)| now_ms.max(prior.updated_at_ms));
        if session.updated_at_ms >= grant.deadline_ms {
            return Err(authority_conflict());
        }
        let recovery_feedback = if let Some((previous, previous_digest, feedback)) = previous {
            let cancelled =
                previous.transition(&AdaptiveTransitionV1::Cancel, session.updated_at_ms)?;
            append(
                &tx,
                &self::namespace(previous.grant.session_id),
                &Entry {
                    previous_digest: Some(previous_digest),
                    command: Some(AdaptiveTransitionV1::Cancel),
                    session: cancelled,
                    recovery_feedback: None,
                },
            )?;
            let changed = tx
                .execute(
                    "UPDATE workflow_adaptive_heads SET session_id=?1,version=?2,updated_at_ms=?3 WHERE session_id=?4 AND version=?5 AND updated_at_ms=?6",
                    params![
                        session.grant.session_id.to_string(),
                        sql_u64(session.version)?,
                        sql_u64(session.updated_at_ms)?,
                        previous.grant.session_id.to_string(),
                        sql_u64(previous.version)?,
                        sql_u64(previous.updated_at_ms)?,
                    ],
                )
                .map_err(map_sqlite_error)?;
            if changed != 1 {
                return Err(corrupt_store());
            }
            feedback
        } else {
            insert_head(&tx, &session)?;
            None
        };
        append(
            &tx,
            &namespace,
            &Entry {
                previous_digest: None,
                command: None,
                session: session.clone(),
                recovery_feedback,
            },
        )?;
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

    /// Latest bounded rejection context on one exact-authority read snapshot.
    /// Before rollover, count still denotes corrections already consumed.
    pub fn adaptive_recovery_feedback(
        &self,
        current: &RuntimeAuthoritySnapshotV1,
    ) -> Result<Option<AdaptiveRecoveryFeedbackV1>, WorkflowError> {
        current.validate()?;
        let mut connection = self.lock()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(map_sqlite_error)?;
        let Some(head) = read_head(&tx, current)? else {
            return Ok(None);
        };
        let (session, _, feedback) =
            load_with_feedback(&tx, head.session_id)?.ok_or_else(corrupt_store)?;
        authorize(&session.grant, current)?;
        validate_head(&head, &session)?;
        if let crate::AdaptiveCursorV1::ModelRejected {
            reason_code,
            resolution_event_id,
        } = session.cursor
        {
            return Ok(Some(AdaptiveRecoveryFeedbackV1 {
                count: feedback.as_ref().map_or(0, |feedback| feedback.count),
                reason_code,
                resolution_event_id,
                previous_session_id: session.grant.session_id,
            }));
        }
        Ok(feedback)
    }

    /// Reads exact durable adoption evidence; does not authorize or dispatch an effect.
    /// Collaboration proposals are adopted only after their matching durable commit.
    pub fn adaptive_model_result_is_adopted(
        &self,
        grant: &AdaptiveSessionGrantV1,
        effect: &AdaptiveEffectV1,
        result_digest: &str,
        decision: &AdaptiveModelDecisionV1,
    ) -> Result<bool, WorkflowError> {
        authorize(grant, &grant.authority)?;
        let mut connection = self.lock()?;
        // Keep validation and lookup on one read snapshot, including external writers.
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(map_sqlite_error)?;
        let (session, _) = load(&tx, grant.session_id)?.ok_or_else(not_found)?;
        authorize(&session.grant, &grant.authority)?;
        if session.grant != *grant {
            return Err(idempotency_conflict());
        }
        require_head(&tx, &session)?;

        let mut statement = tx
            .prepare(
                "SELECT response FROM workflow_operations WHERE operation_namespace=?1 ORDER BY operation_id LIMIT ?2",
            )
            .map_err(map_sqlite_error)?;
        let mut rows = statement
            .query(params![
                namespace(grant.session_id),
                MAX_JOURNAL_ENTRIES as i64
            ])
            .map_err(map_sqlite_error)?;
        let mut collaboration_digest = None;
        while let Some(row) = rows.next().map_err(map_sqlite_error)? {
            let bytes: Vec<u8> = row.get(0).map_err(map_sqlite_error)?;
            let entry: Entry = decode(&bytes)?;
            match entry.command {
                Some(AdaptiveTransitionV1::ResolveModel {
                    effect: recorded_effect,
                    result_digest: recorded_digest,
                    decision: recorded_decision,
                }) if recorded_effect == *effect
                    && recorded_digest == result_digest
                    && recorded_decision == *decision =>
                {
                    if let AdaptiveModelDecisionV1::Collaborate { action } = &recorded_decision {
                        collaboration_digest = Some(adaptive_collaboration_digest(action)?);
                    } else {
                        return Ok(true);
                    }
                }
                Some(AdaptiveTransitionV1::CommitCollaboration {
                    effect: recorded_effect,
                    action_digest,
                }) if recorded_effect == *effect
                    && collaboration_digest.as_ref() == Some(&action_digest) =>
                {
                    return Ok(true);
                }
                _ => {}
            }
        }
        Ok(false)
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
                recovery_feedback: None,
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
    Ok(load_with_feedback(connection, id)?.map(|(session, digest, _)| (session, digest)))
}

type LoadedSession = (
    AdaptiveSessionV1,
    String,
    Option<AdaptiveRecoveryFeedbackV1>,
);

fn load_with_feedback(
    connection: &Connection,
    id: Uuid,
) -> Result<Option<LoadedSession>, WorkflowError> {
    let mut statement = connection.prepare(
        "SELECT operation_id, request_digest, response, created_at_ms FROM workflow_operations WHERE operation_namespace=?1 ORDER BY operation_id LIMIT ?2"
    ).map_err(map_sqlite_error)?;
    let mut rows = statement
        .query(params![namespace(id), (MAX_JOURNAL_ENTRIES + 1) as i64])
        .map_err(map_sqlite_error)?;
    let mut previous: Option<(AdaptiveSessionV1, String)> = None;
    let mut feedback = None;
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
                if let Some(inherited) = &entry.recovery_feedback {
                    inherited.validate().map_err(|_| corrupt_store())?;
                    if inherited.count == 0 || inherited.previous_session_id == id {
                        return Err(corrupt_store());
                    }
                }
                feedback = entry.recovery_feedback.clone();
                let mut initial = AdaptiveSessionV1::initial(entry.session.grant.clone())
                    .map_err(|_| corrupt_store())?;
                if entry.session.updated_at_ms < initial.grant.created_at_ms
                    || entry.session.updated_at_ms >= initial.grant.deadline_ms
                {
                    return Err(corrupt_store());
                }
                initial.updated_at_ms = entry.session.updated_at_ms;
                Ok(initial)
            }
            Some((prior, prior_digest))
                if entry.previous_digest.as_ref() == Some(prior_digest)
                    && entry.recovery_feedback.is_none() =>
            {
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
    Ok(previous.map(|(session, digest)| (session, digest, feedback)))
}
