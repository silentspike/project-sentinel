//! Append-only rounds reuse the existing FULL/WAL operation journal and backup boundary.

use super::*;
use crate::{
    adaptive_collaboration_digest, adaptive_continuation_provider_digest,
    AdaptiveContinuationAuthorizationV1, AdaptiveContinuationSourceV1, AdaptiveEffectV1,
    AdaptiveFirstUnknownModelJournalEvidenceV1, AdaptiveLeadershipReviewCallV1,
    AdaptiveModelDecisionV1, AdaptiveModelJournalRecordEvidenceV1, AdaptiveRecoveryFeedbackV1,
    AdaptiveSessionGrantV1, AdaptiveSessionV1, AdaptiveTransitionV1,
    ADAPTIVE_SCHEMA_MAX_CORRECTIONS,
};
use serde::Deserialize;

mod recovery_lineage;

const MAX_JOURNAL_ENTRIES: usize = 512;
const MAX_SCOPED_ADAPTIVE_HEADS: usize = 64;

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
            if previous.continuation.is_some()
                || !(never_claimed || rejected_first_model || blocked_resolved)
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

    /// Exact journal identity for a separately authorized recovery intervention.
    pub fn adaptive_session_head_digest(
        &self,
        session_id: Uuid,
        current: &RuntimeAuthoritySnapshotV1,
    ) -> Result<Option<String>, WorkflowError> {
        current.validate()?;
        let connection = self.lock()?;
        let Some((session, digest)) = load(&connection, session_id)? else {
            return Ok(None);
        };
        authorize(&session.grant, current)?;
        require_head(&connection, &session)?;
        Ok(Some(digest))
    }

    /// Resolves exact authority without hiding same-assignment campaigns after drift.
    pub fn adaptive_session_for_authority(
        &self,
        current: &RuntimeAuthoritySnapshotV1,
    ) -> Result<Option<AdaptiveSessionV1>, WorkflowError> {
        current.validate()?;
        let mut connection = self.lock()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(map_sqlite_error)?;
        let mut statement = tx.prepare(
            "SELECT authority_digest,session_id,version,updated_at_ms FROM workflow_adaptive_heads WHERE tenant_id=?1 AND project_id=?2 AND work_item_id=?3 AND agent_id=?4 ORDER BY authority_digest LIMIT ?5"
        ).map_err(map_sqlite_error)?;
        let mut rows = statement
            .query(params![
                current.tenant_id.0,
                current.project_id.0,
                current.work_item_id.0,
                i64::from(current.agent_id.0),
                (MAX_SCOPED_ADAPTIVE_HEADS + 1) as i64
            ])
            .map_err(map_sqlite_error)?;
        let mut exact = None;
        let mut drifted_assignment = false;
        let mut count = 0;
        while let Some(row) = rows.next().map_err(map_sqlite_error)? {
            count += 1;
            if count > MAX_SCOPED_ADAPTIVE_HEADS {
                return Err(authority_conflict());
            }
            let stored_digest: String = row.get(0).map_err(map_sqlite_error)?;
            let session_id: String = row.get(1).map_err(map_sqlite_error)?;
            let head = AdaptiveHead {
                session_id: Uuid::parse_str(&session_id).map_err(|_| corrupt_store())?,
                version: stored_u64(row.get(2).map_err(map_sqlite_error)?)?,
                updated_at_ms: stored_u64(row.get(3).map_err(map_sqlite_error)?)?,
            };
            let (session, _) = load(&tx, head.session_id)?.ok_or_else(corrupt_store)?;
            validate_head(&head, &session)?;
            let source = &session.grant.authority;
            if source.tenant_id != current.tenant_id
                || source.project_id != current.project_id
                || source.work_item_id != current.work_item_id
                || source.agent_id != current.agent_id
                || !constant_time_eq(&stored_digest, &source.canonical_digest()?)
            {
                return Err(corrupt_store());
            }
            // Prior assignments remain history, never current provider authority.
            // Do not stop at an exact match: another head may hide spent authority.
            if source.assignment_version == current.assignment_version {
                if source != current {
                    drifted_assignment = true;
                } else {
                    authorize(&session.grant, current)?;
                    if exact.replace(session).is_some() {
                        return Err(corrupt_store());
                    }
                }
            }
        }
        if drifted_assignment {
            return Err(authority_conflict());
        }
        Ok(exact)
    }

    /// Read-only journal provenance for the original first model's sealed unknown.
    /// Historical facts do not authorize importing, adopting or retrying a result.
    pub fn first_unknown_model_journal_evidence(
        &self,
        session_id: Uuid,
        current: &RuntimeAuthoritySnapshotV1,
    ) -> Result<Option<AdaptiveFirstUnknownModelJournalEvidenceV1>, WorkflowError> {
        current.validate()?;
        let mut connection = self.lock()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(map_sqlite_error)?;
        let Some((head, head_digest)) = load(&tx, session_id)? else {
            return Ok(None);
        };
        authorize(&head.grant, current)?;
        require_head(&tx, &head)?;
        if head.version < 3 {
            return Ok(None);
        }
        let ns = namespace(head.grant.session_id);
        let (root_digest, root) = evidence_entry(&tx, &ns, 1)?;
        let (claim_digest, claim) = evidence_entry(&tx, &ns, 2)?;
        let (seal_digest, seal) = evidence_entry(&tx, &ns, 3)?;
        let effect = match (&claim.command, &seal.command) {
            (
                Some(AdaptiveTransitionV1::ClaimModel {
                    effect,
                    previous_observation_digest: None,
                }),
                Some(AdaptiveTransitionV1::MarkUnknown { effect: sealed }),
            ) if effect == sealed => effect.clone(),
            _ => return Ok(None),
        };
        recovery_lineage::validate_recovery_lineage(&tx, &root)?;
        if root.session.model_calls != 0
            || root.session.tool_calls != 0
            || root.session.last_observation.is_some()
            || root.session.last_model_result_digest.is_some()
            || root.session.continuation.is_some()
            || claim.session.model_calls != 1
            || seal.session.model_calls != 1
            || claim.session.tool_calls != 0
            || seal.session.tool_calls != 0
            || claim.session.last_observation.is_some()
            || seal.session.last_observation.is_some()
            || claim.session.last_model_result_digest.is_some()
            || seal.session.last_model_result_digest.is_some()
            || claim.session.continuation.is_some()
            || seal.session.continuation.is_some()
            || claim.session.effect_ids != std::collections::BTreeSet::from([effect.id])
            || seal.session.effect_ids != claim.session.effect_ids
            || !matches!(&claim.session.cursor, crate::AdaptiveCursorV1::ModelPending { effect: pending } if pending == &effect)
            || !matches!(&seal.session.cursor, crate::AdaptiveCursorV1::ModelUnknown { effect: sealed } if sealed == &effect)
        {
            return Ok(None);
        }
        // Full replay above validates every journal entry. Also bind every command
        // operation to that journal; neither a caller UUID nor an ambiguous alias
        // can be substituted for the original claim/seal operation identity.
        let mut claim_record = None;
        let mut seal_record = None;
        let mut adopted = false;
        for (record, command) in validated_journal_operations(&tx, &head)? {
            if record.session_version == 2 {
                claim_record = Some(record);
            } else if record.session_version == 3 {
                seal_record = Some(record);
            }
            if matches!(&command, AdaptiveTransitionV1::ResolveModel { effect: resolved, .. }
                | AdaptiveTransitionV1::RejectModel { effect: resolved, .. }
                | AdaptiveTransitionV1::CommitCollaboration { effect: resolved, .. } if resolved.id == effect.id)
            {
                adopted = true;
            }
        }
        // Scan the journal too: historical cancellation can legitimately lack an
        // operation row, but missing adoption rows must never hide an adoption.
        for version in 4..=head.version {
            let (_, entry) = evidence_entry(&tx, &ns, version)?;
            if matches!(entry.command.as_ref(), Some(AdaptiveTransitionV1::ResolveModel { effect: resolved, .. }
                | AdaptiveTransitionV1::RejectModel { effect: resolved, .. }
                | AdaptiveTransitionV1::CommitCollaboration { effect: resolved, .. }) if resolved.id == effect.id)
            {
                adopted = true;
            }
        }
        let claim_record = claim_record.ok_or_else(corrupt_store)?;
        let seal_record = seal_record.ok_or_else(corrupt_store)?;
        if claim_record.entry_digest != claim_digest || seal_record.entry_digest != seal_digest {
            return Err(corrupt_store());
        }
        if adopted {
            return Ok(None);
        }
        Ok(Some(AdaptiveFirstUnknownModelJournalEvidenceV1 {
            schema_version: 1,
            root_grant: root.session.grant,
            root_entry_digest: root_digest,
            root_recorded_at_ms: root.session.updated_at_ms,
            effect,
            claim: claim_record,
            seal: seal_record,
            sealed_model_calls: seal.session.model_calls,
            sealed_tool_calls: seal.session.tool_calls,
            observed_head_version: head.version,
            observed_head_entry_digest: head_digest,
        }))
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
        if session.effective_grant() != *grant {
            return Err(idempotency_conflict());
        }
        require_head(&tx, &session)?;
        if session.is_abandoned_model_effect(effect) {
            return Err(authority_conflict());
        }

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
        if operation_id.is_nil() || matches!(command, AdaptiveTransitionV1::ContinueGoverned { .. })
        {
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
        if command_effect(command).is_some_and(|effect| session.is_abandoned_model_effect(effect)) {
            return Err(authority_conflict());
        }
        if let (
            crate::AdaptiveCursorV1::ModelUnknown { effect: sealed },
            AdaptiveTransitionV1::ClaimModel {
                effect: claimed, ..
            },
        ) = (&session.cursor, command)
        {
            if sealed.id == claimed.id {
                return Err(authority_conflict());
            }
        }
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
        // Historical journal replay still accepts old transitions. New writes
        // cannot reinterpret a sealed unknown model result as a late response.
        if matches!(session.cursor, crate::AdaptiveCursorV1::ModelUnknown { .. })
            && matches!(
                command,
                AdaptiveTransitionV1::ResolveModel { .. }
                    | AdaptiveTransitionV1::RejectModel { .. }
            )
        {
            return Err(authority_conflict());
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

fn command_effect(command: &AdaptiveTransitionV1) -> Option<&AdaptiveEffectV1> {
    match command {
        AdaptiveTransitionV1::ClaimModel { effect, .. }
        | AdaptiveTransitionV1::ResolveModel { effect, .. }
        | AdaptiveTransitionV1::ClaimTool { effect, .. }
        | AdaptiveTransitionV1::CommitCollaboration { effect, .. }
        | AdaptiveTransitionV1::MarkUnknown { effect }
        | AdaptiveTransitionV1::RejectModel { effect, .. } => Some(effect),
        AdaptiveTransitionV1::ObserveTool { observation } => Some(&observation.effect),
        _ => None,
    }
}

fn validated_journal_operations(
    connection: &Connection,
    session: &AdaptiveSessionV1,
) -> Result<Vec<(AdaptiveModelJournalRecordEvidenceV1, AdaptiveTransitionV1)>, WorkflowError> {
    let ns = namespace(session.grant.session_id);
    let mut statement = connection.prepare(
        "SELECT operation_id,request_digest,response,created_at_ms FROM workflow_operations WHERE operation_namespace=?1 ORDER BY operation_id LIMIT ?2",
    ).map_err(map_sqlite_error)?;
    let mut rows = statement
        .query(params![
            format!("{ns}:operations"),
            (MAX_JOURNAL_ENTRIES + 1) as i64
        ])
        .map_err(map_sqlite_error)?;
    let mut records = Vec::new();
    let mut versions = std::collections::BTreeSet::new();
    while let Some(row) = rows.next().map_err(map_sqlite_error)? {
        let operation_key: String = row.get(0).map_err(map_sqlite_error)?;
        let operation_id = Uuid::parse_str(&operation_key).map_err(|_| corrupt_store())?;
        let digest: String = row.get(1).map_err(map_sqlite_error)?;
        let bytes: Vec<u8> = row.get(2).map_err(map_sqlite_error)?;
        let created: i64 = row.get(3).map_err(map_sqlite_error)?;
        let response: AdaptiveSessionV1 = decode(&bytes)?;
        if records.len() >= MAX_JOURNAL_ENTRIES
            || operation_id.is_nil()
            || operation_key != operation_id.to_string()
            || response.version <= 1
            || response.version > session.version
            || !versions.insert(response.version)
        {
            return Err(corrupt_store());
        }
        let (entry_digest, entry) = evidence_entry(connection, &ns, response.version)?;
        let command = entry.command.ok_or_else(corrupt_store)?;
        let expected_digest = canonical_sha256(
            "sentinel.workflow.adaptive-command.v1",
            &(session.grant.session_id, response.version - 1, &command),
        )?;
        if response != entry.session
            || !constant_time_eq(&digest, &expected_digest)
            || stored_u64(created)? != entry.session.updated_at_ms
        {
            return Err(corrupt_store());
        }
        records.push((
            AdaptiveModelJournalRecordEvidenceV1 {
                session_version: entry.session.version,
                entry_digest,
                operation_id,
                command_digest: digest,
                recorded_at_ms: entry.session.updated_at_ms,
            },
            command,
        ));
    }
    Ok(records)
}

fn evidence_entry(
    connection: &Connection,
    ns: &str,
    version: u64,
) -> Result<(String, Entry), WorkflowError> {
    let (digest, bytes, created) =
        read_operation(connection, ns, &format!("{version:020}"))?.ok_or_else(corrupt_store)?;
    let entry: Entry = decode(&bytes)?;
    if entry.session.version != version
        || stored_u64(created)? != entry.session.updated_at_ms
        || !constant_time_eq(
            &digest,
            &canonical_sha256("sentinel.workflow.adaptive-entry.v1", &entry)?,
        )
    {
        return Err(corrupt_store());
    }
    Ok((digest, entry))
}

/// Lane C validates the real result/audit, calls this before changing the project,
/// and issues the allowance/finalizes the receipt in this same transaction.
pub(crate) fn continue_adaptive_session_in_transaction(
    tx: &Transaction<'_>,
    authorization: &AdaptiveContinuationAuthorizationV1,
    review: &AdaptiveLeadershipReviewCallV1,
    fresh_allowance: &crate::SubscriptionCallAllowanceV1,
    current: &RuntimeAuthoritySnapshotV1,
    now_ms: u64,
) -> Result<(bool, AdaptiveSessionV1), WorkflowError> {
    authorization.validate()?;
    current.validate()?;
    review.grant.validate(review.grant_issued_at_unix_ms)?;
    review.context.validate(&review.grant)?;
    WorkflowStore::require_recovery_epoch_review(tx, review)?;
    WorkflowStore::require_adaptive_budget_review_source(tx, review)?;
    if matches!(review.grant.schema_version, 3 | 4) && authorization.local_adoption.is_some() {
        return Err(authority_conflict());
    }
    let stored_review: AdaptiveLeadershipReviewCallV1 = read_company_entity(
        tx,
        &current.tenant_id.0,
        "adaptive_leadership_review_call",
        &review.grant.review_id.to_string(),
        review.version,
    )?;
    if stored_review != *review
        || review.retired_at_unix_ms.is_some()
        || review.dispatch.is_none()
        || authorization.review_id != review.grant.review_id
        || authorization.session_id != review.grant.session_id
        || authorization.source_session_version != review.grant.expected_session_version
        || review.grant.assignee_authority != *current
        || review.context.source_session.grant.authority != *current
    {
        return Err(authority_conflict());
    }
    let dispatch = review.dispatch.as_ref().ok_or_else(authority_conflict)?;
    if dispatch.request_id != review.request_id()
        || dispatch.context_digest != review.context_digest()?
        || !crate::digest::validate_sha256(&dispatch.request_digest)
        || dispatch.dispatched_at_unix_ms < review.grant_issued_at_unix_ms
        || dispatch.dispatched_at_unix_ms >= review.grant.expires_at_unix_ms
        || authorization.issued_at_ms < dispatch.dispatched_at_unix_ms
    {
        return Err(authority_conflict());
    }
    let (session, prior_digest) = load(tx, authorization.session_id)?.ok_or_else(not_found)?;
    authorize(&session.grant, current)?;
    require_head(tx, &session)?;
    let ns = namespace(authorization.session_id);
    let operations = format!("{ns}:operations");
    let command = AdaptiveTransitionV1::ContinueGoverned {
        authorization: authorization.clone(),
    };
    let digest = canonical_sha256(
        "sentinel.workflow.adaptive-command.v1",
        &(
            authorization.session_id,
            authorization.source_session_version,
            &command,
        ),
    )?;
    if let Some((stored_digest, bytes, created)) =
        read_operation(tx, &operations, &authorization.operation_id.to_string())?
    {
        let response: AdaptiveSessionV1 = decode(&bytes)?;
        let (_, entry_bytes, _) = read_operation(tx, &ns, &format!("{:020}", response.version))?
            .ok_or_else(corrupt_store)?;
        let entry: Entry = decode(&entry_bytes)?;
        if !constant_time_eq(&stored_digest, &digest)
            || response != entry.session
            || entry.command.as_ref() != Some(&command)
            || authorization.source_session_version.checked_add(1) != Some(response.version)
            || stored_u64(created)? != response.updated_at_ms
        {
            return Err(idempotency_conflict());
        }
        if review.resolution_event_id != Some(authorization.resolution_event_id) {
            return Err(authority_conflict());
        }
        return Ok((true, response));
    }
    if review.decision.is_some()
        || session != review.context.source_session
        || session.version != authorization.source_session_version
    {
        return Err(authority_conflict());
    }
    let fresh = &fresh_allowance.grant;
    let source_work = review
        .context
        .source_project
        .work_items
        .get(&current.work_item_id)
        .ok_or_else(not_found)?;
    let assignment = source_work
        .assignments
        .iter()
        .find(|assignment| assignment.active && assignment.agent_id == current.agent_id)
        .ok_or_else(authority_conflict)?;
    let captured_allowance = review
        .context
        .source_project
        .subscription_call
        .as_ref()
        .ok_or_else(authority_conflict)?;
    let policy_allowance = match &review.grant.subject {
        Some(crate::AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget })
            if matches!(review.grant.schema_version, 3 | 4) =>
        {
            &budget.root_allowance
        }
        _ => captured_allowance,
    };
    if fresh_allowance.allowance_id != authorization.provider_allowance_id
        || fresh_allowance.allowance_id == review.allowance_id
        || fresh_allowance.created_at_unix_ms != authorization.issued_at_ms
        || fresh_allowance.created_by != review.grant.leadership_principal.principal_id
        || fresh_allowance.dispatch.is_some()
        || fresh.schema_version != 1
        || fresh.work_item_id != current.work_item_id
        || fresh.agent_id != current.agent_id
        || fresh.assignment_id != assignment.assignment_id
        || fresh.assignment_version != current.assignment_version
        || fresh.provider != session.grant.provider
        || fresh.model != session.grant.model
        || fresh.catalog_digest != session.grant.catalog_digest
        || fresh.token_policy != review.grant.token_policy
        || fresh.max_calls != authorization.additional_model_calls
        || fresh.max_calls > policy_allowance.grant.max_calls
        || (matches!(review.grant.schema_version, 3 | 4)
            && session
                .model_calls
                .checked_add(fresh.max_calls)
                .is_none_or(|calls| calls > session.grant.max_model_calls))
        || fresh.max_concurrent != 1
        || fresh.max_duration_ms
            != session
                .grant
                .max_call_duration_ms
                .min(policy_allowance.grant.max_duration_ms)
                .min(authorization.deadline_ms - authorization.issued_at_ms)
        || fresh.expires_at_unix_ms != authorization.deadline_ms
        || adaptive_continuation_provider_digest(fresh_allowance, current)?
            != authorization.provider_authority_digest
    {
        return Err(authority_conflict());
    }
    // The project row is digest-bound; full equality retains governance, work,
    // assignment and policy checks without interpreting an unverified snapshot.
    let project: crate::ProjectV1 = read_company_entity(
        tx,
        &current.tenant_id.0,
        "project",
        &current.project_id.0,
        review.context.source_project.version,
    )?;
    if project != review.context.source_project
        || project.tenant_id != current.tenant_id
        || project.project_id != current.project_id
        || project
            .subscription_call
            .as_ref()
            .is_some_and(|allowance| allowance.allowance_id == fresh_allowance.allowance_id)
        || !project.governance.participants.iter().any(|participant| {
            participant.principal_id == review.grant.leadership_principal.principal_id
                && Some(participant.agent_id) == review.grant.leadership_principal.agent_id
                && participant.role == review.grant.leadership_principal.role
        })
        || !matches!(
            review.grant.leadership_principal.role,
            crate::CompanyRoleV1::ProjectManager | crate::CompanyRoleV1::TechnicalLead
        )
    {
        return Err(authority_conflict());
    }
    if matches!(
        authorization.source,
        AdaptiveContinuationSourceV1::ModelUnknown
    ) && authorization.abandoned_model_effect.is_none()
    {
        return Err(authority_conflict());
    }
    let next = session.transition(&command, now_ms)?;
    append(
        tx,
        &ns,
        &Entry {
            previous_digest: Some(prior_digest),
            command: Some(command),
            session: next.clone(),
            recovery_feedback: None,
        },
    )?;
    insert_operation(
        tx,
        &operations,
        &authorization.operation_id.to_string(),
        &digest,
        &next,
        now_ms,
    )?;
    update_head(tx, &session, &next)?;
    Ok((false, next))
}

fn read_company_entity<T: DeserializeOwned>(
    connection: &Connection,
    tenant: &str,
    kind: &str,
    id: &str,
    version: u64,
) -> Result<T, WorkflowError> {
    let (stored_version, payload, digest): (i64, Vec<u8>, String) = connection.query_row(
        "SELECT version,payload,payload_digest FROM company_entities WHERE tenant_id=?1 AND entity_kind=?2 AND entity_id=?3",
        params![tenant, kind, id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional().map_err(map_sqlite_error)?.ok_or_else(not_found)?;
    if !constant_time_eq(
        &canonical_sha256("sentinel.workflow.company-entity-row.v1", &payload)?,
        &digest,
    ) {
        return Err(corrupt_store());
    }
    if stored_u64(stored_version)? != version {
        return Err(WorkflowError::new(
            WorkflowErrorCode::VersionConflict,
            false,
            "adaptive continuation source row changed",
        ));
    }
    decode(&payload)
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

pub(crate) fn require_head(
    connection: &Connection,
    session: &AdaptiveSessionV1,
) -> Result<(), WorkflowError> {
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

pub(crate) fn load(
    connection: &Connection,
    id: Uuid,
) -> Result<Option<(AdaptiveSessionV1, String)>, WorkflowError> {
    Ok(load_with_feedback(connection, id)?.map(|(session, digest, _)| (session, digest)))
}

pub(crate) fn require_journal_source(
    connection: &Connection,
    source: &AdaptiveSessionV1,
) -> Result<(), WorkflowError> {
    let (head, _) = load(connection, source.grant.session_id)?.ok_or_else(not_found)?;
    let (_, bytes, _) = read_operation(
        connection,
        &namespace(source.grant.session_id),
        &format!("{:020}", source.version),
    )?
    .ok_or_else(not_found)?;
    let entry: Entry = decode(&bytes)?;
    if entry.session != *source || head.grant != source.grant {
        return Err(authority_conflict());
    }
    Ok(())
}

pub(crate) fn allowance_is_governed_in_journal(
    connection: &Connection,
    tenant: &crate::TenantId,
    project: &crate::ProjectId,
    allowance: &crate::SubscriptionCallAllowanceV1,
) -> Result<bool, WorkflowError> {
    // Bound only journals naming this allowance; unrelated expired rollovers are not evidence.
    let mut statement = connection.prepare(
        "SELECT root.operation_namespace FROM workflow_operations AS root
         WHERE root.operation_namespace GLOB 'adaptive-session-v1:*'
           AND root.operation_id='00000000000000000001'
           AND CASE WHEN json_valid(root.response) THEN json_extract(root.response,'$.session.grant.authority.tenant_id') END=?1
           AND CASE WHEN json_valid(root.response) THEN json_extract(root.response,'$.session.grant.authority.project_id') END=?2
           AND CASE WHEN json_valid(root.response) THEN json_extract(root.response,'$.session.grant.authority.work_item_id') END=?3
           AND CASE WHEN json_valid(root.response) THEN json_extract(root.response,'$.session.grant.authority.agent_id') END=?4
           AND EXISTS (
               SELECT 1 FROM workflow_operations AS journal,
                    json_each(CASE WHEN json_valid(journal.response) THEN journal.response ELSE '{}' END,
                              '$.session.continuation.authorizations') AS authorization
               WHERE journal.operation_namespace=root.operation_namespace
                 AND json_extract(authorization.value,'$.provider_allowance_id')=?5
           )
         ORDER BY root.operation_namespace LIMIT ?6",
    ).map_err(map_sqlite_error)?;
    let namespaces = statement
        .query_map(
            params![
                tenant.0,
                project.0,
                allowance.grant.work_item_id.0,
                i64::from(allowance.grant.agent_id.0),
                allowance.allowance_id,
                (MAX_SCOPED_ADAPTIVE_HEADS + 1) as i64
            ],
            |row| row.get::<_, String>(0),
        )
        .map_err(map_sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite_error)?;
    if namespaces.len() > MAX_SCOPED_ADAPTIVE_HEADS {
        return Err(corrupt_store());
    }
    let mut governed = false;
    for ns in namespaces {
        let id = ns
            .strip_prefix("adaptive-session-v1:")
            .ok_or_else(corrupt_store)?;
        let id = Uuid::parse_str(id).map_err(|_| corrupt_store())?;
        let (session, _) = load(connection, id)?.ok_or_else(corrupt_store)?;
        if session.grant.authority.tenant_id != *tenant
            || session.grant.authority.project_id != *project
            || session.grant.authority.work_item_id != allowance.grant.work_item_id
            || session.grant.authority.agent_id != allowance.grant.agent_id
        {
            return Err(corrupt_store());
        }
        governed |= session.continuation.as_ref().is_some_and(|state| {
            state
                .authorizations
                .iter()
                .any(|authorization| authorization.provider_allowance_id == allowance.allowance_id)
        });
    }
    Ok(governed)
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
    crate::domain_store::validation_scope::memoize(connection, "adaptive-journal", &id, || {
        load_with_feedback_uncached(connection, id)
    })
}

fn load_with_feedback_uncached(
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
        crate::domain_store::validation_scope::charge_bytes(connection, bytes.len())?;
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

#[cfg(test)]
mod continuation_tests {
    use super::*;
    use crate::adaptive::continuation_tests::{authorization, effect, grant, NOW};
    use crate::{
        AdaptiveCursorV1, AdaptiveLeadershipReviewContextV1, AdaptiveLeadershipReviewGrantV1,
    };

    fn persisted_unknown(store: &WorkflowStore) -> AdaptiveSessionV1 {
        let root = grant();
        store
            .begin_adaptive_session(&root, &root.authority, NOW)
            .unwrap();
        store
            .advance_adaptive_session(
                root.session_id,
                1,
                Uuid::from_u128(10),
                &AdaptiveTransitionV1::ClaimModel {
                    effect: effect(102),
                    previous_observation_digest: None,
                },
                &root.authority,
                NOW + 1,
            )
            .unwrap();
        store
            .advance_adaptive_session(
                root.session_id,
                2,
                Uuid::from_u128(11),
                &AdaptiveTransitionV1::MarkUnknown {
                    effect: effect(102),
                },
                &root.authority,
                NOW + 2,
            )
            .unwrap()
            .1
    }

    fn operation_rows(store: &WorkflowStore) -> Vec<(String, String, String, Vec<u8>, i64)> {
        let connection = store.lock().unwrap();
        let mut statement = connection.prepare(
            "SELECT operation_namespace,operation_id,request_digest,response,created_at_ms FROM workflow_operations ORDER BY operation_namespace,operation_id"
        ).unwrap();
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    }

    type HeadRow = (String, String, String, i64, String, String, i64, i64);

    fn head_rows(store: &WorkflowStore) -> Vec<HeadRow> {
        let connection = store.lock().unwrap();
        let mut statement = connection.prepare(
            "SELECT tenant_id,project_id,work_item_id,agent_id,authority_digest,session_id,version,updated_at_ms FROM workflow_adaptive_heads ORDER BY tenant_id,project_id,work_item_id,agent_id,authority_digest"
        ).unwrap();
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    }

    fn continued_unknown_for_head_test(store: &WorkflowStore) -> AdaptiveSessionV1 {
        let source = persisted_unknown(store);
        let auth = authorization(&source);
        let next = source
            .transition(
                &AdaptiveTransitionV1::ContinueGoverned {
                    authorization: auth.clone(),
                },
                auth.issued_at_ms,
            )
            .unwrap();
        let mut connection = store.lock().unwrap();
        let tx = immediate(&mut connection).unwrap();
        let (_, prior_digest) = load(&tx, auth.session_id).unwrap().unwrap();
        // Seed journal facts only; these tests exercise read-only head discovery,
        // not lane C's real-leader authorization and receipt verification.
        append(
            &tx,
            &namespace(auth.session_id),
            &Entry {
                previous_digest: Some(prior_digest),
                command: Some(AdaptiveTransitionV1::ContinueGoverned {
                    authorization: auth,
                }),
                session: next.clone(),
                recovery_feedback: None,
            },
        )
        .unwrap();
        update_head(&tx, &source, &next).unwrap();
        tx.commit().unwrap();
        next
    }

    #[test]
    fn scoped_head_exact_match_and_authority_drift_are_read_only_across_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("workflow.sqlite");
        let store = WorkflowStore::open(&path).unwrap();
        let source = continued_unknown_for_head_test(&store);
        let before = (operation_rows(&store), head_rows(&store));
        let current = source.grant.authority.clone();
        let mut drifts = Vec::new();
        let mut changed = current.clone();
        changed.profile_generation += 1;
        drifts.push(changed);
        let mut changed = current.clone();
        changed.runtime_generation += 1;
        drifts.push(changed);
        let mut changed = current.clone();
        changed.policy_generation += 1;
        drifts.push(changed);
        let mut changed = current.clone();
        changed.organization_generation += 1;
        drifts.push(changed);
        let mut changed = current.clone();
        changed.principal =
            crate::PrincipalAuthorityV1::derive("agent-07", 5, &[0x5a; 32]).unwrap();
        drifts.push(changed);
        assert_eq!(
            store.adaptive_session_for_authority(&current).unwrap(),
            Some(source.clone())
        );
        for drift in &drifts {
            assert_eq!(
                store
                    .adaptive_session_for_authority(drift)
                    .unwrap_err()
                    .code,
                WorkflowErrorCode::AuthorityConflict
            );
        }
        assert_eq!((operation_rows(&store), head_rows(&store)), before);
        drop(store);
        let reopened = WorkflowStore::open(&path).unwrap();
        assert_eq!(
            reopened.adaptive_session_for_authority(&current).unwrap(),
            Some(source)
        );
        for drift in &drifts {
            assert_eq!(
                reopened
                    .adaptive_session_for_authority(drift)
                    .unwrap_err()
                    .code,
                WorkflowErrorCode::AuthorityConflict
            );
        }
        assert_eq!((operation_rows(&reopened), head_rows(&reopened)), before);
    }

    #[test]
    fn scoped_head_rejects_hidden_older_campaign_even_with_exact_fresh_head() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("workflow.sqlite");
        let store = WorkflowStore::open(&path).unwrap();
        let source = continued_unknown_for_head_test(&store);
        let mut fresh = source.grant.clone();
        fresh.session_id = Uuid::from_u128(999);
        fresh.provider_allowance_id = "foreign-fresh-root".into();
        fresh.authority.profile_generation += 1;
        store
            .begin_adaptive_session(&fresh, &fresh.authority, NOW)
            .unwrap();
        let before = (operation_rows(&store), head_rows(&store));
        assert_eq!(
            store
                .adaptive_session_for_authority(&fresh.authority)
                .unwrap_err()
                .code,
            WorkflowErrorCode::AuthorityConflict
        );
        assert_eq!(
            store
                .adaptive_session_for_authority(&source.grant.authority)
                .unwrap_err()
                .code,
            WorkflowErrorCode::AuthorityConflict
        );
        drop(store);
        let reopened = WorkflowStore::open(&path).unwrap();
        assert_eq!(
            reopened
                .adaptive_session_for_authority(&fresh.authority)
                .unwrap_err()
                .code,
            WorkflowErrorCode::AuthorityConflict
        );
        assert_eq!((operation_rows(&reopened), head_rows(&reopened)), before);
    }

    #[test]
    fn scoped_head_does_not_inherit_unrelated_scope_or_new_assignment() {
        let temp = tempfile::tempdir().unwrap();
        let store = WorkflowStore::open(temp.path().join("workflow.sqlite")).unwrap();
        let source = continued_unknown_for_head_test(&store);
        let mut scopes = Vec::new();
        let mut current = source.grant.authority.clone();
        current.tenant_id = crate::TenantId::parse("other-tenant").unwrap();
        scopes.push(current);
        let mut current = source.grant.authority.clone();
        current.project_id = crate::ProjectId::parse("other-project").unwrap();
        scopes.push(current);
        let mut current = source.grant.authority.clone();
        current.work_item_id = crate::WorkItemId::parse("other-work").unwrap();
        scopes.push(current);
        let mut current = source.grant.authority.clone();
        current.agent_id = crate::AgentId(8);
        scopes.push(current);
        let mut current = source.grant.authority.clone();
        current.assignment_version += 1;
        current.assignment_digest = "a".repeat(64);
        scopes.push(current);
        for (index, current) in scopes.into_iter().enumerate() {
            assert!(store
                .adaptive_session_for_authority(&current)
                .unwrap()
                .is_none());
            let mut fresh = source.grant.clone();
            fresh.session_id = Uuid::from_u128(500 + index as u128);
            fresh.provider_allowance_id = format!("new-scope-{index}");
            fresh.authority = current.clone();
            let initial = store
                .begin_adaptive_session(&fresh, &current, NOW)
                .unwrap()
                .1;
            assert_eq!(
                store.adaptive_session_for_authority(&current).unwrap(),
                Some(initial.clone())
            );
            assert_eq!((initial.model_calls, initial.tool_calls), (0, 0));
            assert!(initial.continuation.is_none());
        }
        assert_eq!(
            store
                .adaptive_session_for_authority(&source.grant.authority)
                .unwrap(),
            Some(source)
        );
    }

    #[test]
    fn scoped_head_rejects_corrupt_head_digest_and_complete_journal() {
        for sql in [
            "UPDATE workflow_adaptive_heads SET authority_digest='invalid'",
            "UPDATE workflow_adaptive_heads SET session_id='invalid'",
            "UPDATE workflow_adaptive_heads SET version=version+1",
            "UPDATE workflow_adaptive_heads SET updated_at_ms=updated_at_ms+1",
            "UPDATE workflow_operations SET request_digest='invalid' WHERE operation_namespace NOT LIKE '%:operations' AND operation_id='00000000000000000004'",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let store = WorkflowStore::open(temp.path().join("workflow.sqlite")).unwrap();
            let source = continued_unknown_for_head_test(&store);
            assert!(store.lock().unwrap().execute(sql, []).unwrap() > 0);
            let before = (operation_rows(&store), head_rows(&store));
            let mut drifted = source.grant.authority.clone(); drifted.policy_generation += 1;
            assert!(store.adaptive_session_for_authority(&drifted).is_err(), "{sql}");
            assert!(store.adaptive_session_for_authority(&source.grant.authority).is_err(), "{sql}");
            assert_eq!((operation_rows(&store), head_rows(&store)), before);
        }
    }

    #[test]
    fn scoped_head_rejects_cross_scope_session_alias() {
        let temp = tempfile::tempdir().unwrap();
        let store = WorkflowStore::open(temp.path().join("workflow.sqlite")).unwrap();
        let source = continued_unknown_for_head_test(&store);
        let mut foreign = grant();
        foreign.session_id = Uuid::from_u128(999);
        foreign.authority.work_item_id = crate::WorkItemId::parse("foreign-work").unwrap();
        store
            .begin_adaptive_session(&foreign, &foreign.authority, NOW)
            .unwrap();
        store
            .lock()
            .unwrap()
            .execute(
                "DELETE FROM workflow_adaptive_heads WHERE session_id=?1",
                params![foreign.session_id.to_string()],
            )
            .unwrap();
        store.lock().unwrap().execute(
            "UPDATE workflow_adaptive_heads SET session_id=?1,authority_digest=?2,version=1,updated_at_ms=?3 WHERE work_item_id=?4",
            params![foreign.session_id.to_string(), foreign.authority.canonical_digest().unwrap(), NOW as i64,
                source.grant.authority.work_item_id.0]).unwrap();
        assert_eq!(
            store
                .adaptive_session_for_authority(&source.grant.authority)
                .unwrap_err()
                .code,
            WorkflowErrorCode::CorruptStore
        );
    }

    #[test]
    fn scoped_head_scan_overflow_fails_closed_without_truncation_or_writes() {
        let temp = tempfile::tempdir().unwrap();
        let store = WorkflowStore::open(temp.path().join("workflow.sqlite")).unwrap();
        let current = grant().authority;
        for index in 0..=MAX_SCOPED_ADAPTIVE_HEADS {
            let mut root = grant();
            root.session_id = Uuid::from_u128(1_000 + index as u128);
            root.authority.assignment_version = 10 + index as u64;
            store
                .begin_adaptive_session(&root, &root.authority, NOW)
                .unwrap();
        }
        let before = (operation_rows(&store), head_rows(&store));
        assert_eq!(
            store
                .adaptive_session_for_authority(&current)
                .unwrap_err()
                .code,
            WorkflowErrorCode::AuthorityConflict
        );
        assert_eq!((operation_rows(&store), head_rows(&store)), before);
    }

    #[test]
    fn first_unknown_evidence_is_exact_read_only_and_survives_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("workflow.sqlite");
        let store = WorkflowStore::open(&path).unwrap();
        let source = persisted_unknown(&store);
        let before = operation_rows(&store);
        let evidence = store
            .first_unknown_model_journal_evidence(source.grant.session_id, &source.grant.authority)
            .unwrap()
            .unwrap();
        assert_eq!(evidence.root_grant, source.grant);
        assert_eq!(evidence.effect, effect(102));
        assert_eq!(
            (evidence.sealed_model_calls, evidence.sealed_tool_calls),
            (1, 0)
        );
        assert_eq!(
            (
                evidence.claim.session_version,
                evidence.seal.session_version
            ),
            (2, 3)
        );
        assert_eq!(
            (evidence.claim.operation_id, evidence.seal.operation_id),
            (Uuid::from_u128(10), Uuid::from_u128(11))
        );
        assert_eq!(
            (evidence.claim.recorded_at_ms, evidence.seal.recorded_at_ms),
            (NOW + 1, NOW + 2)
        );
        assert_eq!(evidence.root_recorded_at_ms, NOW);
        assert_eq!(evidence.observed_head_version, 3);
        assert_eq!(
            evidence.observed_head_entry_digest,
            evidence.seal.entry_digest
        );
        {
            let connection = store.lock().unwrap();
            let ns = namespace(source.grant.session_id);
            assert_eq!(
                evidence.root_entry_digest,
                evidence_entry(&connection, &ns, 1).unwrap().0
            );
            assert_eq!(
                evidence.claim.entry_digest,
                evidence_entry(&connection, &ns, 2).unwrap().0
            );
            assert_eq!(
                evidence.seal.entry_digest,
                evidence_entry(&connection, &ns, 3).unwrap().0
            );
            assert_eq!(
                evidence.claim.command_digest,
                read_operation(
                    &connection,
                    &format!("{ns}:operations"),
                    &Uuid::from_u128(10).to_string()
                )
                .unwrap()
                .unwrap()
                .0
            );
        }
        assert_eq!(operation_rows(&store), before);
        assert_eq!(
            store
                .adaptive_session(source.grant.session_id, &source.grant.authority)
                .unwrap(),
            Some(source.clone())
        );
        drop(store);
        let reopened = WorkflowStore::open(&path).unwrap();
        assert_eq!(
            reopened
                .first_unknown_model_journal_evidence(
                    source.grant.session_id,
                    &source.grant.authority
                )
                .unwrap(),
            Some(evidence)
        );
        assert_eq!(operation_rows(&reopened), before);
    }

    #[test]
    fn first_unknown_evidence_rejects_operation_tampering_and_aliases() {
        for sql in [
            "DELETE FROM workflow_operations WHERE operation_namespace=?1 AND operation_id=?2",
            "UPDATE workflow_operations SET request_digest='invalid' WHERE operation_namespace=?1 AND operation_id=?2",
            "UPDATE workflow_operations SET created_at_ms=created_at_ms+1 WHERE operation_namespace=?1 AND operation_id=?2",
            "UPDATE workflow_operations SET response=x'7b7d' WHERE operation_namespace=?1 AND operation_id=?2",
            "UPDATE workflow_operations SET operation_id='00000000-0000-0000-0000-000000000000' WHERE operation_namespace=?1 AND operation_id=?2",
            "UPDATE workflow_operations SET operation_namespace=operation_namespace||':foreign' WHERE operation_namespace=?1 AND operation_id=?2",
            "INSERT INTO workflow_operations SELECT operation_namespace,'00000000-0000-0000-0000-00000000ffff',request_digest,response,created_at_ms FROM workflow_operations WHERE operation_namespace=?1 AND operation_id=?2",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let store = WorkflowStore::open(temp.path().join("workflow.sqlite")).unwrap();
            let source = persisted_unknown(&store);
            let ns = format!("{}:operations", namespace(source.grant.session_id));
            store.lock().unwrap().execute(sql, params![ns, Uuid::from_u128(11).to_string()]).unwrap();
            let before = operation_rows(&store);
            assert!(store.first_unknown_model_journal_evidence(source.grant.session_id, &source.grant.authority).is_err(), "{sql}");
            assert_eq!(operation_rows(&store), before);
        }
    }

    #[test]
    fn first_unknown_evidence_requires_current_authority_and_entire_chain_head() {
        for corrupt_head in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let store = WorkflowStore::open(temp.path().join("workflow.sqlite")).unwrap();
            let source = persisted_unknown(&store);
            let mut stale = source.grant.authority.clone();
            stale.policy_generation += 1;
            assert!(store
                .first_unknown_model_journal_evidence(source.grant.session_id, &stale)
                .is_err());
            assert!(store
                .first_unknown_model_journal_evidence(Uuid::from_u128(999), &source.grant.authority)
                .unwrap()
                .is_none());
            if corrupt_head {
                store
                    .lock()
                    .unwrap()
                    .execute("UPDATE workflow_adaptive_heads SET version=version+1", [])
                    .unwrap();
            } else {
                store.lock().unwrap().execute("UPDATE workflow_operations SET request_digest='invalid' WHERE operation_namespace=?1 AND operation_id=?2",
                    params![namespace(source.grant.session_id), format!("{:020}", 1)]).unwrap();
            }
            assert!(store
                .first_unknown_model_journal_evidence(
                    source.grant.session_id,
                    &source.grant.authority
                )
                .is_err());
        }
    }

    #[test]
    fn first_unknown_evidence_does_not_infer_unknown_from_pending_or_blocked() {
        let temp = tempfile::tempdir().unwrap();
        let store = WorkflowStore::open(temp.path().join("workflow.sqlite")).unwrap();
        let root = grant();
        store
            .begin_adaptive_session(&root, &root.authority, NOW)
            .unwrap();
        assert!(store
            .first_unknown_model_journal_evidence(root.session_id, &root.authority)
            .unwrap()
            .is_none());
        store
            .advance_adaptive_session(
                root.session_id,
                1,
                Uuid::from_u128(10),
                &AdaptiveTransitionV1::ClaimModel {
                    effect: effect(102),
                    previous_observation_digest: None,
                },
                &root.authority,
                NOW + 1,
            )
            .unwrap();
        assert!(store
            .first_unknown_model_journal_evidence(root.session_id, &root.authority)
            .unwrap()
            .is_none());
        store
            .advance_adaptive_session(
                root.session_id,
                2,
                Uuid::from_u128(11),
                &AdaptiveTransitionV1::ResolveModel {
                    effect: effect(102),
                    result_digest: "a".repeat(64),
                    decision: AdaptiveModelDecisionV1::Blocked {
                        reason_code: "needs_review".into(),
                    },
                },
                &root.authority,
                NOW + 2,
            )
            .unwrap();
        assert!(store
            .first_unknown_model_journal_evidence(root.session_id, &root.authority)
            .unwrap()
            .is_none());
    }

    #[test]
    fn generic_advance_rejects_privileged_command_and_late_unknown_result() {
        let temp = tempfile::tempdir().unwrap();
        let store = WorkflowStore::open(temp.path().join("workflow.sqlite")).unwrap();
        let source = persisted_unknown(&store);
        let auth = authorization(&source);
        assert!(store
            .advance_adaptive_session(
                source.grant.session_id,
                source.version,
                auth.operation_id,
                &AdaptiveTransitionV1::ContinueGoverned {
                    authorization: auth.clone()
                },
                &source.grant.authority,
                auth.issued_at_ms
            )
            .is_err());
        assert!(store
            .advance_adaptive_session(
                source.grant.session_id,
                1,
                Uuid::from_u128(10),
                &AdaptiveTransitionV1::ClaimModel {
                    effect: effect(102),
                    previous_observation_digest: None
                },
                &source.grant.authority,
                NOW + 3
            )
            .is_err());
        assert!(store
            .advance_adaptive_session(
                source.grant.session_id,
                source.version,
                Uuid::from_u128(12),
                &AdaptiveTransitionV1::ResolveModel {
                    effect: effect(102),
                    result_digest: "a".repeat(64),
                    decision: AdaptiveModelDecisionV1::Blocked {
                        reason_code: "late_result".into()
                    }
                },
                &source.grant.authority,
                NOW + 3
            )
            .is_err());
        assert_eq!(
            store
                .adaptive_session(source.grant.session_id, &source.grant.authority)
                .unwrap(),
            Some(source)
        );
    }

    fn put_test_entity<T: Serialize>(
        tx: &Transaction<'_>,
        tenant: &str,
        kind: &str,
        id: &str,
        version: u64,
        value: &T,
    ) {
        let payload = encode(value).unwrap();
        let digest = canonical_sha256("sentinel.workflow.company-entity-row.v1", &payload).unwrap();
        tx.execute("INSERT OR REPLACE INTO company_entities (tenant_id,entity_kind,entity_id,version,payload,payload_digest) VALUES (?1,?2,?3,?4,?5,?6)",
            params![tenant, kind, id, version as i64, payload, digest]).unwrap();
    }

    // This fixture supplies the already dispatched review. Lane C owns proof of
    // actual model output and the cross-store audit, not this journal helper.
    fn blocked_review(
        store: &WorkflowStore,
    ) -> (
        AdaptiveLeadershipReviewCallV1,
        AdaptiveContinuationAuthorizationV1,
        crate::SubscriptionCallAllowanceV1,
    ) {
        let mut root = grant();
        let profile = crate::WorkProfileBindingV1 {
            profile_id: root.authority.profile_id.clone(),
            generation: root.authority.profile_generation,
            digest: root.authority.profile_digest.clone(),
        };
        let assignment = crate::AssignmentV1 {
            assignment_id: "assignment-01".into(),
            agent_id: root.authority.agent_id,
            role: crate::CompanyRoleV1::Developer,
            specialties: std::collections::BTreeSet::from(["rust".into()]),
            profile: profile.clone(),
            organization_generation: root.authority.organization_generation,
            organization_digest: root.authority.organization_digest.clone(),
            assignment_version: root.authority.assignment_version,
            delegated_by: None,
            reason_ref: "assigned".into(),
            active: true,
            assigned_by: "pm-01".into(),
            created_at_unix_ms: NOW,
            ended_at_unix_ms: None,
        };
        root.authority.assignment_digest = assignment.canonical_digest().unwrap();
        store
            .begin_adaptive_session(&root, &root.authority, NOW)
            .unwrap();
        store
            .advance_adaptive_session(
                root.session_id,
                1,
                Uuid::from_u128(10),
                &AdaptiveTransitionV1::ClaimModel {
                    effect: effect(102),
                    previous_observation_digest: None,
                },
                &root.authority,
                NOW + 1,
            )
            .unwrap();
        let source = store
            .advance_adaptive_session(
                root.session_id,
                2,
                Uuid::from_u128(11),
                &AdaptiveTransitionV1::ResolveModel {
                    effect: effect(102),
                    result_digest: "a".repeat(64),
                    decision: AdaptiveModelDecisionV1::Blocked {
                        reason_code: "needs_review".into(),
                    },
                },
                &root.authority,
                NOW + 2,
            )
            .unwrap()
            .1;
        let leader_authority = crate::PrincipalAuthorityV1::derive("pm-01", 1, &[1; 32]).unwrap();
        let leader = crate::AuthenticatedCompanyPrincipalV1 {
            schema_version: 1,
            tenant_id: root.authority.tenant_id.clone(),
            principal_id: "pm-01".into(),
            kind: crate::CompanyPrincipalKindV1::Agent,
            role: crate::CompanyRoleV1::ProjectManager,
            customer_id: None,
            agent_id: Some(crate::AgentId(1)),
            authority_generation: 1,
            authority_digest: leader_authority.authority_digest.clone(),
        };
        let work = serde_json::json!({
            "spec": {"work_item_id": root.authority.work_item_id, "title": "Source", "objective": "Build source",
                "required_role": "developer", "required_specialties": ["rust"], "dependency_ids": [],
                "owner": root.authority.agent_id, "inputs": [], "outputs": [],
                "quality_gate": {"gate_id": "qa-v1", "generation": 1, "digest": "a".repeat(64)}, "budget_micros": 100},
            "state": "assigned", "version": 1, "assignments": [assignment], "output_receipts": [],
            "gate_receipt": null, "transition_history": []
        });
        let project: crate::ProjectV1 = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "tenant_id": root.authority.tenant_id, "project_id": root.authority.project_id,
            "agreement_id": "agreement-01", "agreement_digest": "a".repeat(64),
            "governance": {"owner": 1, "project_profile": profile,
                "participants": [{"agent_id": 1, "principal_id": "pm-01", "role": "project_manager",
                    "specialties": ["coordination"], "reports_to": null, "profile": profile}]},
            "cost_ceiling_micros": 100, "provider_cost_ceilings_micros": {}, "lifecycle_state": "active",
            "reserved_cost_micros": 0, "committed_cost_micros": 0,
            "work_items": {"work-01": work}, "decisions": [], "handoffs": [], "blockers": [],
            "approvals": [], "reservations": [], "rooms": [], "questions": [], "actions": [],
            "version": 1, "created_at_unix_ms": NOW, "updated_at_unix_ms": NOW
        })).unwrap();
        let context = AdaptiveLeadershipReviewContextV1 {
            source_project: project,
            source_session: source,
            tool_catalog: serde_json::json!({"tools": ["file.inspect"]}),
            evidence_refs: vec!["retained-source".into()],
        };
        let fingerprint = crate::adaptive_leadership_evidence_fingerprint(
            &context.tool_catalog,
            &context.evidence_refs,
        )
        .unwrap();
        let review_grant = AdaptiveLeadershipReviewGrantV1 {
            schema_version: 1,
            recovery_epoch: None,
            subject: None,
            review_id: crate::adaptive_leadership_review_id(root.session_id, 3, &fingerprint)
                .unwrap(),
            project_id: root.authority.project_id.clone(),
            expected_project_version: 1,
            work_item_id: root.authority.work_item_id.clone(),
            session_id: root.session_id,
            expected_session_version: 3,
            expected_reason_code: "needs_review".into(),
            evidence_fingerprint: fingerprint,
            leadership_principal: leader,
            leadership_authority: leader_authority,
            assignment_id: "assignment-01".into(),
            assignee_authority: root.authority.clone(),
            provider: root.provider.clone(),
            model: root.model.clone(),
            catalog_digest: root.catalog_digest.clone(),
            max_duration_ms: 120_000,
            token_policy: crate::SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,
            expires_at_unix_ms: NOW + 100_000,
        };
        let mut review = AdaptiveLeadershipReviewCallV1 {
            schema_version: 1,
            review_key: review_grant.review_id.to_string(),
            allowance_id: "review-allowance".into(),
            operation_id: Uuid::from_u128(20),
            grant: review_grant,
            context,
            version: 2,
            created_at_unix_ms: NOW + 3,
            grant_issued_at_unix_ms: NOW + 3,
            updated_at_unix_ms: NOW + 4,
            dispatch: None,
            decision: None,
            model_response_digest: None,
            resolution_event_id: None,
            retired_at_unix_ms: None,
            continuation: None,
        };
        review.dispatch = Some(crate::RequestProviderDispatchV1 {
            request_id: review.request_id(),
            request_digest: "b".repeat(64),
            context_digest: review.context_digest().unwrap(),
            dispatched_at_unix_ms: NOW + 4,
        });
        let mut auth = authorization(&review.context.source_session);
        auth.review_id = review.grant.review_id;
        auth.source = crate::AdaptiveContinuationSourceV1::Blocked {
            reason_code: "needs_review".into(),
        };
        auth.abandoned_model_effect = None;
        let allowance = crate::SubscriptionCallAllowanceV1 {
            allowance_id: auth.provider_allowance_id.clone(),
            created_by: "pm-01".into(),
            created_at_unix_ms: auth.issued_at_ms,
            dispatch: None,
            grant: crate::SubscriptionCallGrantV1 {
                schema_version: 1,
                work_item_id: root.authority.work_item_id.clone(),
                assignment_id: "assignment-01".into(),
                assignment_version: root.authority.assignment_version,
                agent_id: root.authority.agent_id,
                provider: root.provider,
                model: root.model,
                catalog_digest: root.catalog_digest,
                max_calls: auth.additional_model_calls,
                max_concurrent: 1,
                max_duration_ms: root
                    .max_call_duration_ms
                    .min(auth.deadline_ms - auth.issued_at_ms),
                token_policy: review.grant.token_policy,
                expires_at_unix_ms: auth.deadline_ms,
            },
        };
        let mut source_policy = allowance.clone();
        source_policy.allowance_id = review
            .context
            .source_session
            .grant
            .provider_allowance_id
            .clone();
        source_policy.created_at_unix_ms = review.context.source_session.grant.created_at_ms;
        source_policy.grant.max_calls = review.context.source_session.grant.max_model_calls;
        source_policy.grant.max_duration_ms =
            review.context.source_session.grant.max_call_duration_ms;
        source_policy.grant.expires_at_unix_ms = review.context.source_session.grant.deadline_ms;
        review.context.source_project.subscription_call = Some(source_policy);
        let context_digest = review.context_digest().unwrap();
        review.dispatch.as_mut().unwrap().context_digest = context_digest;
        auth.provider_authority_digest =
            adaptive_continuation_provider_digest(&allowance, &root.authority).unwrap();
        let mut connection = store.lock().unwrap();
        let tx = immediate(&mut connection).unwrap();
        put_test_entity(
            &tx,
            &root.authority.tenant_id.0,
            "project",
            &root.authority.project_id.0,
            1,
            &review.context.source_project,
        );
        put_test_entity(
            &tx,
            &root.authority.tenant_id.0,
            "adaptive_leadership_review_call",
            &review.grant.review_id.to_string(),
            2,
            &review,
        );
        tx.commit().unwrap();
        (review, auth, allowance)
    }

    #[test]
    fn continuation_transaction_rolls_back_then_recovers_exact_receipt() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("workflow.sqlite");
        let store = WorkflowStore::open(&path).unwrap();
        let (mut review, auth, allowance) = blocked_review(&store);
        let current = review.grant.assignee_authority.clone();
        {
            let mut connection = store.lock().unwrap();
            let tx = immediate(&mut connection).unwrap();
            assert!(
                !continue_adaptive_session_in_transaction(
                    &tx,
                    &auth,
                    &review,
                    &allowance,
                    &current,
                    auth.issued_at_ms
                )
                .unwrap()
                .0
            );
            // Simulate a crash before lane C attaches allowance and final receipt.
        }
        assert_eq!(
            store.adaptive_session(auth.session_id, &current).unwrap(),
            Some(review.context.source_session.clone())
        );
        let continued;
        {
            let mut connection = store.lock().unwrap();
            let tx = immediate(&mut connection).unwrap();
            continued = continue_adaptive_session_in_transaction(
                &tx,
                &auth,
                &review,
                &allowance,
                &current,
                auth.issued_at_ms,
            )
            .unwrap()
            .1;
            review.version = 3;
            review.decision = Some(crate::AdaptiveLeadershipReviewDecisionV1 {
                schema_version: 1,
                decision: crate::AdaptiveLeadershipReviewDecisionKindV1::ResolveBlocked {
                    rationale: "Fixture-only lane C receipt".into(),
                    evidence_refs: review.context.evidence_refs.clone(),
                },
            });
            review.model_response_digest = Some("c".repeat(64));
            review.updated_at_unix_ms = auth.issued_at_ms;
            review.resolution_event_id = Some(auth.resolution_event_id);
            put_test_entity(
                &tx,
                &current.tenant_id.0,
                "adaptive_leadership_review_call",
                &auth.review_id.to_string(),
                3,
                &review,
            );
            tx.commit().unwrap();
        }
        drop(store);
        let reopened = WorkflowStore::open(&path).unwrap();
        let mut connection = reopened.lock().unwrap();
        let tx = immediate(&mut connection).unwrap();
        assert_eq!(
            continue_adaptive_session_in_transaction(
                &tx,
                &auth,
                &review,
                &allowance,
                &current,
                auth.deadline_ms + 1
            )
            .unwrap(),
            (true, continued)
        );
        let mut different = auth.clone();
        different.additional_model_calls += 1;
        assert!(continue_adaptive_session_in_transaction(
            &tx,
            &different,
            &review,
            &allowance,
            &current,
            auth.issued_at_ms
        )
        .is_err());
    }

    #[test]
    fn continuation_transaction_rejects_stale_head_authority_allowance_and_retirement() {
        let temp = tempfile::tempdir().unwrap();
        let store = WorkflowStore::open(temp.path().join("workflow.sqlite")).unwrap();
        let (review, auth, allowance) = blocked_review(&store);
        let current = review.grant.assignee_authority.clone();
        let mut connection = store.lock().unwrap();
        let tx = immediate(&mut connection).unwrap();
        let mut changed = current.clone();
        changed.policy_generation += 1;
        assert!(continue_adaptive_session_in_transaction(
            &tx,
            &auth,
            &review,
            &allowance,
            &changed,
            auth.issued_at_ms
        )
        .is_err());
        let mut stale = auth.clone();
        stale.source_session_version += 1;
        assert!(continue_adaptive_session_in_transaction(
            &tx,
            &stale,
            &review,
            &allowance,
            &current,
            auth.issued_at_ms
        )
        .is_err());
        let mut too_many = allowance.clone();
        too_many.grant.max_calls = 16;
        assert!(continue_adaptive_session_in_transaction(
            &tx,
            &auth,
            &review,
            &too_many,
            &current,
            auth.issued_at_ms
        )
        .is_err());
        let mut project = review.context.source_project.clone();
        project.version += 1;
        put_test_entity(
            &tx,
            &current.tenant_id.0,
            "project",
            &current.project_id.0,
            project.version,
            &project,
        );
        assert!(continue_adaptive_session_in_transaction(
            &tx,
            &auth,
            &review,
            &allowance,
            &current,
            auth.issued_at_ms
        )
        .is_err());
        let mut retired = review.clone();
        retired.retired_at_unix_ms = Some(auth.issued_at_ms);
        put_test_entity(
            &tx,
            &current.tenant_id.0,
            "adaptive_leadership_review_call",
            &auth.review_id.to_string(),
            2,
            &retired,
        );
        assert!(continue_adaptive_session_in_transaction(
            &tx,
            &auth,
            &retired,
            &allowance,
            &current,
            auth.issued_at_ms
        )
        .is_err());
        assert_eq!(
            load(&tx, auth.session_id).unwrap().unwrap().0.cursor,
            AdaptiveCursorV1::Blocked {
                reason_code: "needs_review".into()
            }
        );
    }

    #[test]
    fn abandoned_effect_tombstone_blocks_old_claim_replay_and_adoption_after_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("workflow.sqlite");
        let store = WorkflowStore::open(&path).unwrap();
        let source = persisted_unknown(&store);
        let auth = authorization(&source);
        // Seed only the validated journal transition: V2 ModelUnknown review
        // eligibility is lane C's integration test, not this tombstone unit test.
        let next = source
            .transition(
                &AdaptiveTransitionV1::ContinueGoverned {
                    authorization: auth.clone(),
                },
                auth.issued_at_ms,
            )
            .unwrap();
        {
            let mut connection = store.lock().unwrap();
            let tx = immediate(&mut connection).unwrap();
            let (_, previous_digest) = load(&tx, auth.session_id).unwrap().unwrap();
            append(
                &tx,
                &namespace(auth.session_id),
                &Entry {
                    previous_digest: Some(previous_digest),
                    command: Some(AdaptiveTransitionV1::ContinueGoverned {
                        authorization: auth.clone(),
                    }),
                    session: next.clone(),
                    recovery_feedback: None,
                },
            )
            .unwrap();
            update_head(&tx, &source, &next).unwrap();
            tx.commit().unwrap();
        }
        drop(store);
        let reopened = WorkflowStore::open(&path).unwrap();
        assert_eq!(
            reopened
                .adaptive_session(auth.session_id, &source.grant.authority)
                .unwrap(),
            Some(next.clone())
        );
        let evidence = reopened
            .first_unknown_model_journal_evidence(auth.session_id, &source.grant.authority)
            .unwrap()
            .unwrap();
        assert_eq!(evidence.root_grant, source.grant);
        assert_eq!(evidence.observed_head_version, next.version);
        assert_eq!(evidence.seal.session_version, 3);
        assert!(reopened
            .advance_adaptive_session(
                auth.session_id,
                1,
                Uuid::from_u128(10),
                &AdaptiveTransitionV1::ClaimModel {
                    effect: effect(102),
                    previous_observation_digest: None
                },
                &source.grant.authority,
                auth.issued_at_ms
            )
            .is_err());
        assert!(reopened
            .adaptive_model_result_is_adopted(
                &next.effective_grant(),
                &effect(102),
                &"a".repeat(64),
                &AdaptiveModelDecisionV1::Blocked {
                    reason_code: "late_result".into()
                }
            )
            .is_err());
        let mut rollover = source.grant.clone();
        rollover.session_id = Uuid::from_u128(999);
        rollover.provider_allowance_id = "new-root".into();
        rollover.created_at_ms = auth.deadline_ms;
        rollover.deadline_ms = auth.deadline_ms + 1_000;
        assert!(reopened
            .begin_adaptive_session(&rollover, &rollover.authority, auth.deadline_ms)
            .is_err());
    }

    #[test]
    fn historical_unknown_resolution_journal_still_loads_without_new_adoption() {
        let temp = tempfile::tempdir().unwrap();
        let store = WorkflowStore::open(temp.path().join("workflow.sqlite")).unwrap();
        let source = persisted_unknown(&store);
        let command = AdaptiveTransitionV1::ResolveModel {
            effect: effect(102),
            result_digest: "a".repeat(64),
            decision: AdaptiveModelDecisionV1::Blocked {
                reason_code: "historical_result".into(),
            },
        };
        let historical = source.transition(&command, NOW + 3).unwrap();
        {
            let mut connection = store.lock().unwrap();
            let tx = immediate(&mut connection).unwrap();
            let (_, previous_digest) = load(&tx, source.grant.session_id).unwrap().unwrap();
            append(
                &tx,
                &namespace(source.grant.session_id),
                &Entry {
                    previous_digest: Some(previous_digest),
                    command: Some(command),
                    session: historical.clone(),
                    recovery_feedback: None,
                },
            )
            .unwrap();
            update_head(&tx, &source, &historical).unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(
            store
                .adaptive_session(source.grant.session_id, &source.grant.authority)
                .unwrap(),
            Some(historical)
        );
        assert!(store
            .first_unknown_model_journal_evidence(source.grant.session_id, &source.grant.authority)
            .unwrap()
            .is_none());
    }
}
