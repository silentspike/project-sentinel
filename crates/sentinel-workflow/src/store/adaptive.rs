//! Append-only rounds reuse the existing FULL/WAL operation journal and backup boundary.

use super::*;
use crate::domain_store::adaptive_resume_policy::{
    read_resume_policy_leaf, require_resume_authorization_membership,
    require_resume_review_membership,
};
use crate::domain_store::adaptive_work_funding::{
    insert_funding_adoption_membership, require_funding_authorization_membership,
};
use crate::{
    adaptive_collaboration_digest, adaptive_continuation_provider_digest,
    AdaptiveContinuationAuthorizationV1, AdaptiveContinuationSourceV1, AdaptiveEffectV1,
    AdaptiveFirstUnknownModelJournalEvidenceV1, AdaptiveLeadershipReviewCallV1,
    AdaptiveModelDecisionV1, AdaptiveModelJournalRecordEvidenceV1, AdaptiveRecoveryFeedbackV1,
    AdaptiveRejectedModelReceiptV1, AdaptiveSessionGrantV1, AdaptiveSessionV1,
    AdaptiveTransitionV1, AdaptiveWorkingMemoryCompletedRowV1, AdaptiveWorkingMemorySourceV1,
    AdaptiveWorkingMemoryToolKindV1, ADAPTIVE_SCHEMA_MAX_CORRECTIONS,
    ADAPTIVE_WORKING_MEMORY_MAX_BYTES, ADAPTIVE_WORKING_MEMORY_MAX_LABEL_BYTES,
    ADAPTIVE_WORKING_MEMORY_MAX_ROWS,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[cfg(test)]
mod company_claim_tests;
#[cfg(test)]
mod health_inventory_tests;
mod recovery_lineage;
#[cfg(test)]
mod rejected_model_tests;
#[cfg(test)]
mod work_funding_tests;
#[cfg(test)]
mod working_memory_tests;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RejectedModelDisposition {
    receipt: AdaptiveRejectedModelReceiptV1,
    response: AdaptiveSessionV1,
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

    /// Historical health inventory only, never admission or authority to retry effects.
    pub fn adaptive_sessions_for_health(&self) -> Result<Vec<AdaptiveSessionV1>, WorkflowError> {
        let mut connection = self.lock()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(map_sqlite_error)?;
        // The validation scope pins the snapshot and reuses replay proofs only until
        // this read ends, including journals named by both heads and namespaces.
        crate::domain_store::validation_scope::with_scope(&tx, || adaptive_health_inventory(&tx))
    }

    /// Private historical pointers from one authorized, fully replayed read snapshot.
    pub fn adaptive_working_memory_source(
        &self,
        session_id: Uuid,
        provider_version: u64,
        effect_id: Uuid,
        current: &RuntimeAuthoritySnapshotV1,
    ) -> Result<Option<AdaptiveWorkingMemorySourceV1>, WorkflowError> {
        current.validate()?;
        if session_id.is_nil() || provider_version == 0 || effect_id.is_nil() {
            return Err(authority_conflict());
        }
        let mut connection = self.lock()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(map_sqlite_error)?;
        let Some((current_session, _)) = load(&tx, session_id)? else {
            return Ok(None);
        };
        authorize(&current_session.grant, current)?;
        require_head(&tx, &current_session)?;
        let effect_matches = match &current_session.cursor {
            crate::AdaptiveCursorV1::ReadyForModel => {
                current_session.version == provider_version
                    && working_memory_model_effect_id(session_id, provider_version) == effect_id
            }
            crate::AdaptiveCursorV1::ModelPending { effect }
            | crate::AdaptiveCursorV1::ModelUnknown { effect } => {
                provider_version.checked_add(1) == Some(current_session.version)
                    && effect.id == effect_id
            }
            _ => false,
        };
        if !effect_matches {
            return Err(authority_conflict());
        }
        let (head_entry_digest, prefix) =
            evidence_entry(&tx, &namespace(session_id), provider_version)?;
        let session = prefix.session;
        if session.grant != current_session.grant {
            return Err(corrupt_store());
        }
        let mut rows = working_memory_rows(&tx, &session)?;
        let completed_tool_count = u16::try_from(rows.len()).map_err(|_| corrupt_store())?;
        let latest_test = rows
            .iter()
            .rfind(|row| row.tool_kind == AdaptiveWorkingMemoryToolKindV1::RunTests)
            .cloned();
        if rows.len() > ADAPTIVE_WORKING_MEMORY_MAX_ROWS {
            rows = rows.split_off(rows.len() - ADAPTIVE_WORKING_MEMORY_MAX_ROWS);
            if let Some(test) = &latest_test {
                if !rows
                    .iter()
                    .any(|row| row.session_version == test.session_version)
                {
                    rows.remove(0);
                    rows.insert(0, test.clone());
                }
            }
        }
        let mut source = AdaptiveWorkingMemorySourceV1 {
            schema_version: 1,
            session_id,
            authority: session.grant.authority.clone(),
            provider_version,
            effect_id,
            head_version: session.version,
            head_entry_digest,
            last_observation: session.last_observation.clone(),
            model_calls: session.model_calls,
            tool_calls: session.tool_calls,
            root_model_ceiling: session.grant.max_model_calls,
            root_tool_ceiling: session.grant.max_tool_calls,
            active_model_ceiling: session.active_model_ceiling(),
            continuation_windows: u16::try_from(
                session
                    .continuation
                    .as_ref()
                    .map_or(0, |state| state.authorizations.len()),
            )
            .map_err(|_| corrupt_store())?,
            completed_tool_count,
            omitted_count: completed_tool_count - rows.len() as u16,
            rows,
            work_funding: session.active_work_funding().cloned().map(Box::new),
        };
        while serde_json::to_vec(&source)
            .map_err(|_| corrupt_store())?
            .len()
            > ADAPTIVE_WORKING_MEMORY_MAX_BYTES
        {
            let index = source
                .rows
                .iter()
                .position(|row| {
                    source
                        .rows
                        .last()
                        .is_some_and(|last| last.session_version != row.session_version)
                        && latest_test
                            .as_ref()
                            .is_none_or(|test| test.session_version != row.session_version)
                })
                .ok_or_else(corrupt_store)?;
            source.rows.remove(index);
            source.omitted_count += 1;
        }
        source.validate().map_err(|_| corrupt_store())?;
        Ok(Some(source))
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

    /// One authorized read snapshot of the exact pending session and journal digest.
    /// This evidence does not authorize disposition; the write path rechecks it.
    pub fn adaptive_pending_model_head_evidence(
        &self,
        session_id: Uuid,
        expected_version: u64,
        effect: &AdaptiveEffectV1,
        current: &RuntimeAuthoritySnapshotV1,
    ) -> Result<Option<(AdaptiveSessionV1, String)>, WorkflowError> {
        current.validate()?;
        let mut connection = self.lock()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(map_sqlite_error)?;
        let Some((session, digest)) = load(&tx, session_id)? else {
            return Ok(None);
        };
        authorize(&session.grant, current)?;
        require_head(&tx, &session)?;
        if session.version != expected_version {
            return Err(WorkflowError::new(
                WorkflowErrorCode::VersionConflict,
                false,
                "adaptive session version changed",
            ));
        }
        if !matches!(&session.cursor, crate::AdaptiveCursorV1::ModelPending { effect: pending }
            if pending == effect)
        {
            return Err(authority_conflict());
        }
        Ok(Some((session, digest)))
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
        self.advance_adaptive_session_with_clock(
            session_id,
            expected_version,
            operation_id,
            command,
            current,
            || now_ms,
        )
    }

    pub fn advance_adaptive_session_with_clock<Clock: FnMut() -> u64>(
        &self,
        session_id: Uuid,
        expected_version: u64,
        operation_id: Uuid,
        command: &AdaptiveTransitionV1,
        current: &RuntimeAuthoritySnapshotV1,
        clock: Clock,
    ) -> Result<(bool, AdaptiveSessionV1), WorkflowError> {
        self.advance_adaptive_session_in_transaction(
            session_id,
            expected_version,
            operation_id,
            command,
            current,
            clock,
            false,
        )
    }

    /// Checks persisted company authority only for a new final model claim.
    pub fn advance_company_adaptive_model_with_clock<Clock: FnMut() -> u64>(
        &self,
        session_id: Uuid,
        expected_version: u64,
        operation_id: Uuid,
        command: &AdaptiveTransitionV1,
        current: &RuntimeAuthoritySnapshotV1,
        clock: Clock,
    ) -> Result<(bool, AdaptiveSessionV1), WorkflowError> {
        if !matches!(command, AdaptiveTransitionV1::ClaimModel { .. }) {
            return Err(authority_conflict());
        }
        self.advance_adaptive_session_in_transaction(
            session_id,
            expected_version,
            operation_id,
            command,
            current,
            clock,
            true,
        )
    }

    fn advance_adaptive_session_in_transaction<Clock: FnMut() -> u64>(
        &self,
        session_id: Uuid,
        expected_version: u64,
        operation_id: Uuid,
        command: &AdaptiveTransitionV1,
        current: &RuntimeAuthoritySnapshotV1,
        mut clock: Clock,
        company_claim: bool,
    ) -> Result<(bool, AdaptiveSessionV1), WorkflowError> {
        current.validate()?;
        if operation_id.is_nil()
            || matches!(
                command,
                AdaptiveTransitionV1::ContinueGoverned { .. }
                    | AdaptiveTransitionV1::ResumeRejectedModel { .. }
            )
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
        // The organization snapshot precedes this transaction. Only a NEW
        // company claim must recheck persisted assignment/allowance authority.
        let company_window = if company_claim {
            Some(require_current_company_model_binding(
                &tx, &session, current,
            )?)
        } else {
            None
        };
        // Full project/receipt validation may be expensive. Sample the claim
        // clock afterward so transition admission checks fresh dispatch slack.
        let now_ms = clock();
        if company_window.is_some_and(|(earliest, expires)| now_ms < earliest || now_ms >= expires)
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

    /// The verifier reads retained EventStore evidence under the locked pending
    /// head. It must not re-enter WorkflowStore or perform provider/tool I/O.
    /// Committed replay returns the historical response without moving the head.
    pub fn dispose_rejected_adaptive_model<F, C>(
        &self,
        operation_id: Uuid,
        receipt: &AdaptiveRejectedModelReceiptV1,
        current: &RuntimeAuthoritySnapshotV1,
        mut clock: C,
        verify: F,
    ) -> Result<(bool, AdaptiveSessionV1), WorkflowError>
    where
        C: FnMut() -> u64,
        F: FnOnce(
            &AdaptiveSessionV1,
            &str,
            &AdaptiveRejectedModelReceiptV1,
        ) -> Result<AdaptiveModelDecisionV1, WorkflowError>,
    {
        current.validate()?;
        receipt.validate()?;
        if operation_id.is_nil() {
            return Err(authority_conflict());
        }
        let mut connection = self.lock()?;
        let tx = immediate(&mut connection)?;
        let (session, prior_digest) = load(&tx, receipt.session_id)?.ok_or_else(not_found)?;
        authorize(&session.grant, current)?;
        require_head(&tx, &session)?;
        if let Some(recorded) =
            read_rejected_model_disposition(&tx, receipt.session_id, operation_id)?
        {
            if recorded.receipt != *receipt {
                return Err(idempotency_conflict());
            }
            return Ok((true, recorded.response));
        }
        require_rejected_model_source(&session, &prior_digest, receipt)?;
        if validated_journal_operations(&tx, &session)?
            .iter()
            .any(|(_, command)| {
                matches!(command, AdaptiveTransitionV1::ResolveModel { effect, .. }
                    if effect.id == receipt.effect.id)
            })
        {
            return Err(authority_conflict());
        }
        let decision = verify(&session, &prior_digest, receipt)?;
        let AdaptiveModelDecisionV1::Tool { tool, tool_digest } = decision else {
            return Err(authority_conflict());
        };
        if !matches!(&tool, sentinel_common::WorkbenchTool::WriteFile { .. })
            || tool_digest != receipt.tool_digest
            || crate::adaptive_tool_digest(&tool)? != tool_digest
        {
            return Err(authority_conflict());
        }
        let now_ms = clock();
        let (reject, resume) = rejected_model_commands(operation_id, receipt)?;
        let rejected = session.transition(&reject, now_ms)?;
        let resumed = rejected.transition(&resume, now_ms)?;
        let ns = namespace(receipt.session_id);
        let rejected_entry = Entry {
            previous_digest: Some(prior_digest),
            command: Some(reject.clone()),
            session: rejected.clone(),
            recovery_feedback: None,
        };
        let rejected_digest =
            canonical_sha256("sentinel.workflow.adaptive-entry.v1", &rejected_entry)?;
        append(&tx, &ns, &rejected_entry)?;
        append(
            &tx,
            &ns,
            &Entry {
                previous_digest: Some(rejected_digest),
                command: Some(resume.clone()),
                session: resumed.clone(),
                recovery_feedback: None,
            },
        )?;
        let operations = format!("{ns}:operations");
        for (id, source_version, command, response) in [
            (operation_id, session.version, &reject, &rejected),
            (
                rejected_model_resume_operation_id(operation_id),
                rejected.version,
                &resume,
                &resumed,
            ),
        ] {
            let digest = canonical_sha256(
                "sentinel.workflow.adaptive-command.v1",
                &(receipt.session_id, source_version, command),
            )?;
            insert_operation(&tx, &operations, &id.to_string(), &digest, response, now_ms)?;
        }
        let disposition = RejectedModelDisposition {
            receipt: receipt.clone(),
            response: resumed.clone(),
        };
        insert_operation(
            &tx,
            &rejected_model_disposition_namespace(receipt.session_id),
            &operation_id.to_string(),
            &rejected_model_disposition_digest(operation_id, receipt)?,
            &disposition,
            now_ms,
        )?;
        update_head(&tx, &session, &resumed)?;
        tx.commit().map_err(map_sqlite_error)?;
        Ok((false, resumed))
    }
}

fn rejected_model_disposition_namespace(session_id: Uuid) -> String {
    format!("{}:rejected-model-dispositions", namespace(session_id))
}

fn rejected_model_disposition_digest(
    operation_id: Uuid,
    receipt: &AdaptiveRejectedModelReceiptV1,
) -> Result<String, WorkflowError> {
    canonical_sha256(
        "sentinel.workflow.adaptive-rejected-model-disposition.v1",
        &(operation_id, receipt),
    )
}

fn rejected_model_resume_operation_id(operation_id: Uuid) -> Uuid {
    let mut hash = Sha256::new();
    hash.update(b"sentinel.workflow.resume-rejected-model-operation.v1\0");
    hash.update(operation_id.as_bytes());
    let digest = hash.finalize();
    let mut bytes = [0_u8; 16];
    bytes[6..].copy_from_slice(&digest[..10]);
    bytes[6] = (bytes[6] & 0x0f) | 0x70;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

fn rejected_model_commands(
    operation_id: Uuid,
    receipt: &AdaptiveRejectedModelReceiptV1,
) -> Result<(AdaptiveTransitionV1, AdaptiveTransitionV1), WorkflowError> {
    if operation_id.is_nil() || rejected_model_resume_operation_id(operation_id) == operation_id {
        return Err(authority_conflict());
    }
    Ok((
        AdaptiveTransitionV1::RejectModel {
            effect: receipt.effect.clone(),
            resolution_event_id: receipt.resolution_event_id.to_string(),
            reason_code: receipt.reason_code.clone(),
        },
        AdaptiveTransitionV1::ResumeRejectedModel {
            disposition_operation_id: operation_id,
            expected_reason_code: receipt.reason_code.clone(),
            resolution_event_id: receipt.resolution_event_id.to_string(),
            receipt_digest: receipt.canonical_digest()?,
        },
    ))
}

fn require_rejected_model_source(
    session: &AdaptiveSessionV1,
    entry_digest: &str,
    receipt: &AdaptiveRejectedModelReceiptV1,
) -> Result<(), WorkflowError> {
    receipt.validate()?;
    if session.grant.session_id != receipt.session_id
        || session.version != receipt.source_session_version
        || entry_digest != receipt.source_entry_digest
        || !session.requires_fresh_observation()
        || session.is_abandoned_model_effect(&receipt.effect)
        || !matches!(&session.cursor, crate::AdaptiveCursorV1::ModelPending { effect }
            if effect == &receipt.effect)
    {
        return Err(authority_conflict());
    }
    Ok(())
}

fn read_rejected_model_disposition(
    connection: &Connection,
    session_id: Uuid,
    operation_id: Uuid,
) -> Result<Option<RejectedModelDisposition>, WorkflowError> {
    let Some((digest, bytes, created)) = read_operation(
        connection,
        &rejected_model_disposition_namespace(session_id),
        &operation_id.to_string(),
    )?
    else {
        return Ok(None);
    };
    let record: RejectedModelDisposition = decode(&bytes)?;
    let receipt = &record.receipt;
    receipt.validate().map_err(|_| corrupt_store())?;
    if receipt.session_id != session_id
        || !constant_time_eq(
            &digest,
            &rejected_model_disposition_digest(operation_id, receipt)?,
        )
        || stored_u64(created)? != record.response.updated_at_ms
    {
        return Err(corrupt_store());
    }
    let ns = namespace(session_id);
    let (source_digest, source) = evidence_entry(connection, &ns, receipt.source_session_version)?;
    require_rejected_model_source(&source.session, &source_digest, receipt)
        .map_err(|_| corrupt_store())?;
    let (reject, resume) = rejected_model_commands(operation_id, receipt)?;
    let now_ms = record.response.updated_at_ms;
    let rejected = source
        .session
        .transition(&reject, now_ms)
        .map_err(|_| corrupt_store())?;
    let resumed = rejected
        .transition(&resume, now_ms)
        .map_err(|_| corrupt_store())?;
    let (rejected_digest, rejected_entry) = evidence_entry(connection, &ns, rejected.version)?;
    let (_, resumed_entry) = evidence_entry(connection, &ns, resumed.version)?;
    let expected_rejected = Entry {
        previous_digest: Some(source_digest),
        command: Some(reject.clone()),
        session: rejected.clone(),
        recovery_feedback: None,
    };
    let expected_resumed = Entry {
        previous_digest: Some(rejected_digest),
        command: Some(resume.clone()),
        session: resumed.clone(),
        recovery_feedback: None,
    };
    if rejected_entry != expected_rejected
        || resumed_entry != expected_resumed
        || record.response != resumed
    {
        return Err(corrupt_store());
    }
    for (id, source_version, command, response) in [
        (operation_id, source.session.version, &reject, &rejected),
        (
            rejected_model_resume_operation_id(operation_id),
            rejected.version,
            &resume,
            &resumed,
        ),
    ] {
        let (phase_digest, phase_bytes, phase_created) =
            read_operation(connection, &format!("{ns}:operations"), &id.to_string())?
                .ok_or_else(corrupt_store)?;
        let expected_digest = canonical_sha256(
            "sentinel.workflow.adaptive-command.v1",
            &(session_id, source_version, command),
        )?;
        if !constant_time_eq(&phase_digest, &expected_digest)
            || decode::<AdaptiveSessionV1>(&phase_bytes)? != *response
            || stored_u64(phase_created)? != now_ms
        {
            return Err(corrupt_store());
        }
    }
    Ok(Some(record))
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

fn working_memory_model_effect_id(session_id: Uuid, version: u64) -> Uuid {
    // Byte-exact daemon adaptive-model-effect identity, not a new effect reservation.
    let digest = Sha256::digest(
        format!("sentinel.workflow.adaptive-model-effect.v1:{session_id}:{version}").as_bytes(),
    );
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

fn working_memory_label(value: &str, omitted: &mut bool) -> Option<String> {
    if value.trim().is_empty()
        || value.len() > ADAPTIVE_WORKING_MEMORY_MAX_LABEL_BYTES
        || value.chars().any(char::is_control)
    {
        *omitted = true;
        None
    } else {
        Some(value.to_owned())
    }
}

fn working_memory_row(
    entry: &Entry,
    entry_digest: String,
    tool: &sentinel_common::WorkbenchTool,
    tool_digest: &str,
    observation: &crate::AdaptiveObservationRefV1,
) -> AdaptiveWorkingMemoryCompletedRowV1 {
    use sentinel_common::WorkbenchTool;
    use AdaptiveWorkingMemoryToolKindV1 as Kind;
    let mut row = AdaptiveWorkingMemoryCompletedRowV1 {
        session_version: entry.session.version,
        entry_digest,
        recorded_at_ms: entry.session.updated_at_ms,
        tool_kind: Kind::PackageArtifact,
        tool_digest: tool_digest.to_owned(),
        target: None,
        program: None,
        suite_id: None,
        labels_omitted: false,
        observation: observation.clone(),
    };
    row.tool_kind = match tool {
        WorkbenchTool::ListDirectory { path, .. }
        | WorkbenchTool::InspectFile { path, .. }
        | WorkbenchTool::WriteFile { path, .. }
        | WorkbenchTool::ApplyPatch { path, .. } => {
            row.target = working_memory_label(path, &mut row.labels_omitted);
            match tool {
                WorkbenchTool::ListDirectory { .. } => Kind::ListDirectory,
                WorkbenchTool::InspectFile { .. } => Kind::InspectFile,
                WorkbenchTool::WriteFile { .. } => Kind::WriteFile,
                _ => Kind::ApplyPatch,
            }
        }
        WorkbenchTool::RunCommand { program, .. } => {
            row.program = working_memory_label(program, &mut row.labels_omitted);
            Kind::RunCommand
        }
        WorkbenchTool::RunTests {
            program, suite_id, ..
        } => {
            row.program = working_memory_label(program, &mut row.labels_omitted);
            row.suite_id = working_memory_label(suite_id, &mut row.labels_omitted);
            Kind::RunTests
        }
        WorkbenchTool::PackageArtifact { .. } => {
            row.labels_omitted = true;
            Kind::PackageArtifact
        }
    };
    row
}

fn working_memory_rows(
    connection: &Connection,
    session: &AdaptiveSessionV1,
) -> Result<Vec<AdaptiveWorkingMemoryCompletedRowV1>, WorkflowError> {
    // load() already replayed these immutable rows in this same read transaction.
    let mut statement = connection.prepare(
        "SELECT request_digest,response FROM workflow_operations WHERE operation_namespace=?1 AND operation_id<=?2 ORDER BY operation_id LIMIT ?3",
    ).map_err(map_sqlite_error)?;
    let mut entries = statement
        .query(params![
            namespace(session.grant.session_id),
            format!("{:020}", session.version),
            (MAX_JOURNAL_ENTRIES + 1) as i64,
        ])
        .map_err(map_sqlite_error)?;
    let mut previous: Option<Entry> = None;
    let mut rows = Vec::new();
    let mut count = 0;
    while let Some(record) = entries.next().map_err(map_sqlite_error)? {
        count += 1;
        if count > MAX_JOURNAL_ENTRIES {
            return Err(corrupt_store());
        }
        let entry_digest: String = record.get(0).map_err(map_sqlite_error)?;
        let bytes: Vec<u8> = record.get(1).map_err(map_sqlite_error)?;
        let entry: Entry = decode(&bytes)?;
        if let Some(AdaptiveTransitionV1::ObserveTool { observation }) = &entry.command {
            let prior = previous.as_ref().ok_or_else(corrupt_store)?;
            let (effect, tool, tool_digest) = match &prior.session.cursor {
                crate::AdaptiveCursorV1::ToolPending {
                    effect,
                    tool,
                    tool_digest,
                }
                | crate::AdaptiveCursorV1::ToolUnknown {
                    effect,
                    tool,
                    tool_digest,
                } => (effect, tool, tool_digest),
                _ => return Err(corrupt_store()),
            };
            if effect != &observation.effect {
                return Err(corrupt_store());
            }
            rows.push(working_memory_row(
                &entry,
                entry_digest,
                tool,
                tool_digest,
                observation,
            ));
        }
        previous = Some(entry);
    }
    if previous.as_ref().map(|entry| &entry.session) != Some(session) {
        return Err(corrupt_store());
    }
    Ok(rows)
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
    if authorization.resume_policy != review.grant.resume_policy
        || authorization.work_funding != review.grant.work_funding
        || (authorization.work_funding.is_some() && review.grant.recovery_epoch.is_some())
    {
        return Err(authority_conflict());
    }
    if review.grant.resume_policy.is_some() {
        require_resume_review_membership(
            tx,
            &review.grant,
            &review.context_digest()?,
            review.operation_id,
        )?;
    }
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
    if let Some(binding) = &review.grant.resume_policy {
        let receipt = read_resume_policy_leaf(tx, &current.tenant_id, session.grant.session_id)?
            .ok_or_else(authority_conflict)?;
        receipt.validate_binding(binding)?;
        require_resume_policy_anchor(tx, &receipt, &session)?;
        if now_ms >= binding.limits.expires_at_unix_ms {
            return Err(authority_conflict());
        }
    } else if authorization.work_funding.is_none()
        && read_resume_policy_leaf(tx, &current.tenant_id, authorization.session_id)?.is_some()
    {
        return Err(authority_conflict());
    }
    if let Some(epoch) = &authorization.work_funding {
        require_work_funding_anchor(tx, epoch, &session)?;
        if now_ms >= epoch.binding.limits.expires_at_unix_ms {
            return Err(authority_conflict());
        }
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
            if matches!(review.grant.schema_version, 3..=5) =>
        {
            &budget.root_allowance
        }
        _ => captured_allowance,
    };
    let model_ceiling = authorization
        .work_funding
        .as_ref()
        .map_or(session.funded_model_call_ceiling(), |epoch| {
            epoch.binding.limits.total_model_call_ceiling
        });
    let duration = authorization
        .work_funding
        .as_ref()
        .map_or_else(
            || {
                session.grant.max_call_duration_ms.min(
                    review
                        .grant
                        .resume_policy
                        .as_ref()
                        .map_or(policy_allowance.grant.max_duration_ms, |binding| {
                            binding.limits.max_call_duration_ms
                        }),
                )
            },
            |epoch| epoch.binding.limits.max_call_duration_ms,
        )
        .min(authorization.deadline_ms - authorization.issued_at_ms);
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
        || (review.grant.resume_policy.is_none()
            && authorization.work_funding.is_none()
            && fresh.max_calls > policy_allowance.grant.max_calls)
        || (matches!(review.grant.schema_version, 3..=5)
            && session
                .model_calls
                .checked_add(fresh.max_calls)
                .is_none_or(|calls| calls > model_ceiling))
        || fresh.max_concurrent != 1
        || fresh.max_duration_ms != duration
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
    if authorization.work_funding.is_some() {
        insert_funding_adoption_membership(tx, authorization, current)?;
    }
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

/// Validated journal head and immutable root, without loading leadership reviews.
pub(crate) fn adaptive_resume_journal_source(
    connection: &Connection,
    session_id: Uuid,
) -> Result<Option<(AdaptiveSessionV1, String, String)>, WorkflowError> {
    let Some((session, head_digest)) = load(connection, session_id)? else {
        return Ok(None);
    };
    require_head(connection, &session)?;
    let (root_digest, _) = evidence_entry(connection, &namespace(session_id), 1)?;
    Ok(Some((session, root_digest, head_digest)))
}

pub(crate) fn require_resume_policy_anchor(
    connection: &Connection,
    receipt: &crate::AdaptiveResumePolicyReceiptV1,
    session: &AdaptiveSessionV1,
) -> Result<(), WorkflowError> {
    let source = &receipt.request.source;
    if session.grant.session_id != source.session_id
        || session.grant.authority != source.assignee_authority
        || session.version < source.expected_session_version
        || session.model_calls < source.base_model_calls
        || session.tool_calls < source.base_tool_calls
        || session
            .continuation
            .as_ref()
            .map_or(0, |state| state.authorizations.len())
            < usize::from(source.base_window_count)
        || receipt.request.limits.max_call_duration_ms != session.grant.max_call_duration_ms
    {
        return Err(authority_conflict());
    }
    let ns = namespace(source.session_id);
    let root =
        read_operation(connection, &ns, &format!("{:020}", 1))?.ok_or_else(authority_conflict)?;
    let anchor = read_operation(
        connection,
        &ns,
        &format!("{:020}", source.expected_session_version),
    )?
    .ok_or_else(authority_conflict)?;
    let root_entry: Entry = decode(&root.1)?;
    let anchor_entry: Entry = decode(&anchor.1)?;
    if root.0 != source.root_entry_digest
        || anchor.0 != source.head_entry_digest
        || root.0 != canonical_sha256("sentinel.workflow.adaptive-entry.v1", &root_entry)?
        || anchor.0 != canonical_sha256("sentinel.workflow.adaptive-entry.v1", &anchor_entry)?
        || root_entry.session.grant != session.grant
        || anchor_entry.session.grant != session.grant
        || anchor_entry.session.version != source.expected_session_version
        || anchor_entry.session.model_calls != source.base_model_calls
        || anchor_entry.session.tool_calls != source.base_tool_calls
        || crate::adaptive_budget_history_digest(&anchor_entry.session.continuation)?
            != source.continuation_history_digest
        || anchor_entry
            .session
            .continuation
            .as_ref()
            .map_or(0, |state| state.authorizations.len())
            != usize::from(source.base_window_count)
    {
        return Err(authority_conflict());
    }
    Ok(())
}

fn require_work_funding_anchor(
    connection: &Connection,
    epoch: &crate::AdaptiveWorkFundingEpochV1,
    session: &AdaptiveSessionV1,
) -> Result<(), WorkflowError> {
    epoch.validate()?;
    let source = &epoch.receipt.request.source;
    let anchor = &source.resume_source;
    let ns = namespace(session.grant.session_id);
    let (root_digest, root) = evidence_entry(connection, &ns, 1)?;
    let (head_digest, head) = evidence_entry(connection, &ns, anchor.expected_session_version)?;
    if root_digest != anchor.root_entry_digest
        || head_digest != anchor.head_entry_digest
        || root.session.grant != session.grant
        || head.session.grant != session.grant
        || anchor.session_id != session.grant.session_id
        || anchor.assignee_authority != session.grant.authority
        || source.original_model_call_ceiling != session.grant.max_model_calls
        || source.original_tool_call_ceiling != session.grant.max_tool_calls
        || source.current_model_call_ceiling != head.session.funded_model_call_ceiling()
        || source.current_tool_call_ceiling != head.session.funded_tool_call_ceiling()
        || source.predecessor_receipt_digest.as_deref()
            != head
                .session
                .active_work_funding()
                .map(|prior| prior.binding.receipt_digest.as_str())
        || head.session.model_calls != anchor.base_model_calls
        || head.session.tool_calls != anchor.base_tool_calls
        || head
            .session
            .continuation
            .as_ref()
            .map_or(0, |state| state.authorizations.len())
            != usize::from(anchor.base_window_count)
        || crate::adaptive_budget_history_digest(&head.session.continuation)?
            != anchor.continuation_history_digest
    {
        return Err(authority_conflict());
    }
    Ok(())
}

fn require_current_company_model_binding(
    connection: &Connection,
    session: &AdaptiveSessionV1,
    current: &RuntimeAuthoritySnapshotV1,
) -> Result<(u64, u64), WorkflowError> {
    let project = crate::domain_store::validated_company_project_in_snapshot(
        connection,
        &current.tenant_id,
        &current.project_id,
    )?
    .ok_or_else(authority_conflict)?;
    let effective = session.effective_grant();
    if effective.authority != *current
        || project.lifecycle_state != crate::ProjectLifecycleStateV1::Active
        || project.governance.project_profile.generation != current.policy_generation
        || !constant_time_eq(
            &project.governance.project_profile.digest,
            &current.policy_digest,
        )
    {
        return Err(authority_conflict());
    }
    let work = project
        .work_items
        .get(&current.work_item_id)
        .ok_or_else(authority_conflict)?;
    let mut assignments = work
        .assignments
        .iter()
        .filter(|assignment| assignment.active);
    let assignment = assignments.next().ok_or_else(authority_conflict)?;
    if assignments.next().is_some()
        || !matches!(
            work.state,
            crate::CompanyWorkStateV1::Assigned
                | crate::CompanyWorkStateV1::InProgress
                | crate::CompanyWorkStateV1::InReview
        )
        || work.spec.work_item_id != current.work_item_id
        || assignment.agent_id != current.agent_id
        || assignment.role != work.spec.required_role
        || assignment.assignment_version != current.assignment_version
        || !constant_time_eq(&assignment.canonical_digest()?, &current.assignment_digest)
        || assignment.organization_generation != current.organization_generation
        || !constant_time_eq(
            &assignment.organization_digest,
            &current.organization_digest,
        )
        || assignment.profile.profile_id != current.profile_id
        || assignment.profile.generation != current.profile_generation
        || !constant_time_eq(&assignment.profile.digest, &current.profile_digest)
        || !project.governance.participants.iter().any(|participant| {
            participant.agent_id == current.agent_id
                && participant.principal_id == current.principal.principal_id
                && participant.role == assignment.role
                && participant.profile == assignment.profile
        })
        || project
            .reservations
            .iter()
            .any(|reservation| reservation.work_item_id.as_ref() == Some(&current.work_item_id))
    {
        return Err(authority_conflict());
    }
    let allowance = project
        .subscription_call
        .as_ref()
        .ok_or_else(authority_conflict)?;
    let grant = &allowance.grant;
    // A continuation allowance authorizes one window, not the lifetime ceiling.
    // Journal replay already verifies the continuation receipt and its root limits.
    let window_calls = session
        .continuation
        .as_ref()
        .and_then(|state| state.authorizations.last())
        .map_or(session.grant.max_model_calls, |authorization| {
            authorization.additional_model_calls
        });
    if allowance.allowance_id != effective.provider_allowance_id
        || allowance.dispatch.is_some()
        || grant.work_item_id != current.work_item_id
        || grant.assignment_id != assignment.assignment_id
        || grant.assignment_version != current.assignment_version
        || grant.agent_id != current.agent_id
        || grant.provider != effective.provider
        || grant.model != effective.model
        || !constant_time_eq(&grant.catalog_digest, &effective.catalog_digest)
        || grant.max_calls != window_calls
        || grant.max_concurrent != 1
        || grant.max_duration_ms != effective.max_call_duration_ms
        || grant.token_policy != crate::SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap
        || grant.expires_at_unix_ms != effective.deadline_ms
        || allowance.created_at_unix_ms != effective.created_at_ms
        || !constant_time_eq(
            &adaptive_continuation_provider_digest(allowance, current)?,
            &effective.provider_authority_digest,
        )
    {
        return Err(authority_conflict());
    }
    Ok((
        project.updated_at_unix_ms.max(allowance.created_at_unix_ms),
        grant.expires_at_unix_ms,
    ))
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

type HealthScope = (String, String, String, i64, String);

struct HealthJournal {
    session: AdaptiveSessionV1,
    root: Entry,
    scope: HealthScope,
}

fn health_scope(session: &AdaptiveSessionV1) -> Result<HealthScope, WorkflowError> {
    let authority = &session.grant.authority;
    Ok((
        authority.tenant_id.to_string(),
        authority.project_id.to_string(),
        authority.work_item_id.to_string(),
        i64::from(authority.agent_id.0),
        authority.canonical_digest()?,
    ))
}

fn adaptive_health_inventory(
    connection: &Connection,
) -> Result<Vec<AdaptiveSessionV1>, WorkflowError> {
    use std::collections::{BTreeMap, BTreeSet};

    // Inspect namespace keys independently of heads and JSON payload locators.
    // A deleted head, missing root, companion-only journal or alias must not vanish.
    let mut ids = BTreeSet::new();
    let mut statement = connection
        .prepare("SELECT DISTINCT operation_namespace FROM workflow_operations WHERE operation_namespace GLOB 'adaptive-session-v1:*' ORDER BY operation_namespace")
        .map_err(map_sqlite_error)?;
    let mut rows = statement.query([]).map_err(map_sqlite_error)?;
    while let Some(row) = rows.next().map_err(map_sqlite_error)? {
        let ns: String = row.get(0).map_err(map_sqlite_error)?;
        let key = ns
            .strip_prefix("adaptive-session-v1:")
            .ok_or_else(corrupt_store)?;
        let (session_key, suffix) = key
            .split_once(':')
            .map_or((key, None), |(session, suffix)| (session, Some(suffix)));
        let id = Uuid::parse_str(session_key).map_err(|_| corrupt_store())?;
        if id.is_nil()
            || session_key != id.to_string()
            || !matches!(
                suffix,
                None | Some("operations") | Some("rejected-model-dispositions")
            )
        {
            return Err(corrupt_store());
        }
        ids.insert(id);
    }

    let mut heads = BTreeMap::new();
    let mut head_ids = BTreeSet::new();
    let mut lineage_counts = BTreeMap::new();
    let mut statement = connection
        .prepare("SELECT tenant_id,project_id,work_item_id,agent_id,authority_digest,session_id,version,updated_at_ms FROM workflow_adaptive_heads ORDER BY tenant_id,project_id,work_item_id,agent_id,authority_digest")
        .map_err(map_sqlite_error)?;
    let mut rows = statement.query([]).map_err(map_sqlite_error)?;
    while let Some(row) = rows.next().map_err(map_sqlite_error)? {
        let scope: HealthScope = (
            row.get(0).map_err(map_sqlite_error)?,
            row.get(1).map_err(map_sqlite_error)?,
            row.get(2).map_err(map_sqlite_error)?,
            row.get(3).map_err(map_sqlite_error)?,
            row.get(4).map_err(map_sqlite_error)?,
        );
        let lineage = (scope.0.clone(), scope.1.clone(), scope.2.clone(), scope.3);
        let count = lineage_counts.entry(lineage).or_insert(0);
        *count += 1;
        if *count > MAX_SCOPED_ADAPTIVE_HEADS {
            return Err(corrupt_store());
        }
        let key: String = row.get(5).map_err(map_sqlite_error)?;
        let id = Uuid::parse_str(&key).map_err(|_| corrupt_store())?;
        let head = AdaptiveHead {
            session_id: id,
            version: stored_u64(row.get(6).map_err(map_sqlite_error)?)?,
            updated_at_ms: stored_u64(row.get(7).map_err(map_sqlite_error)?)?,
        };
        let (session, _) = load(connection, id)?.ok_or_else(corrupt_store)?;
        validate_head(&head, &session)?;
        if id.is_nil()
            || key != id.to_string()
            || scope != health_scope(&session)?
            || !head_ids.insert(id)
            || heads.insert(scope, id).is_some()
        {
            return Err(corrupt_store());
        }
        ids.insert(id);
    }

    let mut journals = BTreeMap::new();
    let mut roots = BTreeMap::new();
    for id in ids {
        let (session, _, _) = load_with_feedback(connection, id)?.ok_or_else(corrupt_store)?;
        let (_, root) = evidence_entry(connection, &namespace(id), 1)?;
        let operation_versions = validated_journal_operations(connection, &session)?
            .into_iter()
            .map(|(record, _)| record.session_version)
            .collect::<BTreeSet<_>>();
        validate_health_dispositions(connection, &session)?;
        // Only a proven rollover cancellation may omit its command companion.
        let rollover = !head_ids.contains(&id)
            && matches!(&session.cursor, crate::AdaptiveCursorV1::Cancelled);
        if (2..=session.version).any(|version| {
            !operation_versions.contains(&version) && !(rollover && version == session.version)
        }) || (!head_ids.contains(&id) && !rollover)
        {
            return Err(corrupt_store());
        }
        let scope = health_scope(&session)?;
        if roots
            .insert((scope.clone(), root.session.updated_at_ms), id)
            .is_some()
        {
            return Err(corrupt_store());
        }
        journals.insert(
            id,
            HealthJournal {
                session,
                root,
                scope,
            },
        );
    }

    let mut successors = BTreeMap::new();
    let mut predecessors = BTreeSet::new();
    for (id, journal) in &journals {
        if head_ids.contains(id) {
            continue;
        }
        // The scope head has already been independently replayed and index-bound.
        if !heads.contains_key(&journal.scope) {
            return Err(corrupt_store());
        }
        let next = roots
            .get(&(journal.scope.clone(), journal.session.updated_at_ms))
            .filter(|next| *next != id)
            .ok_or_else(corrupt_store)?;
        validate_health_rollover(connection, journal, &journals[next])?;
        if !predecessors.insert(*next) {
            return Err(corrupt_store());
        }
        successors.insert(*id, *next);
    }
    // Feedback is introduced only by a validated rejected-first-model rollover;
    // each later rollover preserves or increments that exact inherited record.
    // This proves all origins once, without repeatedly replaying ancestor chains.
    if journals
        .iter()
        .any(|(id, journal)| journal.root.recovery_feedback.is_some() && !predecessors.contains(id))
    {
        return Err(corrupt_store());
    }

    // Bound each proof chain, not the global inventory. Every unrelated head
    // remains visible, including inventories larger than 64 sessions.
    let mut depths = head_ids
        .iter()
        .map(|id| (*id, 1))
        .collect::<BTreeMap<_, _>>();
    for id in successors.keys() {
        let mut cursor = *id;
        let mut path = Vec::new();
        let mut visited = BTreeSet::new();
        while !depths.contains_key(&cursor) {
            if path.len() >= MAX_SCOPED_ADAPTIVE_HEADS || !visited.insert(cursor) {
                return Err(corrupt_store());
            }
            path.push(cursor);
            cursor = *successors.get(&cursor).ok_or_else(corrupt_store)?;
        }
        let mut depth = depths[&cursor];
        for prior in path.into_iter().rev() {
            depth += 1;
            if depth > MAX_SCOPED_ADAPTIVE_HEADS {
                return Err(corrupt_store());
            }
            depths.insert(prior, depth);
        }
    }
    Ok(heads
        .values()
        .map(|id| journals[id].session.clone())
        .collect())
}

fn validate_health_dispositions(
    connection: &Connection,
    session: &AdaptiveSessionV1,
) -> Result<(), WorkflowError> {
    let mut statement = connection
        .prepare("SELECT operation_id FROM workflow_operations WHERE operation_namespace=?1 ORDER BY operation_id LIMIT ?2")
        .map_err(map_sqlite_error)?;
    let mut rows = statement
        .query(params![
            rejected_model_disposition_namespace(session.grant.session_id),
            (MAX_JOURNAL_ENTRIES + 1) as i64,
        ])
        .map_err(map_sqlite_error)?;
    let mut count = 0;
    while let Some(row) = rows.next().map_err(map_sqlite_error)? {
        count += 1;
        let key: String = row.get(0).map_err(map_sqlite_error)?;
        let id = Uuid::parse_str(&key).map_err(|_| corrupt_store())?;
        if count > MAX_JOURNAL_ENTRIES || id.is_nil() || key != id.to_string() {
            return Err(corrupt_store());
        }
        read_rejected_model_disposition(connection, session.grant.session_id, id)?
            .ok_or_else(corrupt_store)?;
    }
    Ok(())
}

fn validate_health_rollover(
    connection: &Connection,
    prior: &HealthJournal,
    next: &HealthJournal,
) -> Result<(), WorkflowError> {
    let session = &prior.session;
    let ns = namespace(session.grant.session_id);
    let (_, cancellation) = evidence_entry(connection, &ns, session.version)?;
    let (_, source) = evidence_entry(
        connection,
        &ns,
        session.version.checked_sub(1).ok_or_else(corrupt_store)?,
    )?;
    let source = source.session;
    let never_claimed = source.version == 1
        && source.model_calls == 0
        && source.tool_calls == 0
        && matches!(&source.cursor, crate::AdaptiveCursorV1::ReadyForModel)
        && source.last_observation.is_none()
        && source.last_model_result_digest.is_none()
        && source.effect_ids.is_empty();
    let rejected_first_model = matches!(source.version, 3 | 4)
        && source.model_calls == 1
        && source.tool_calls == 0
        && matches!(
            &source.cursor,
            crate::AdaptiveCursorV1::ModelRejected { .. }
        )
        && source.last_observation.is_none()
        && source.last_model_result_digest.is_none()
        && source.effect_ids.len() == 1;
    if source.continuation.is_some()
        || !(never_claimed
            || rejected_first_model
            || matches!(
                &source.cursor,
                crate::AdaptiveCursorV1::BlockedResolved { .. }
            ))
        || !matches!(&cancellation.command, Some(AdaptiveTransitionV1::Cancel))
        || session.grant.authority != next.session.grant.authority
        || session.grant.provider_allowance_id == next.session.grant.provider_allowance_id
        || session.updated_at_ms < session.grant.deadline_ms
        || session.updated_at_ms != next.root.session.updated_at_ms
        || prior.root.session.updated_at_ms >= next.root.session.updated_at_ms
    {
        return Err(corrupt_store());
    }
    let feedback = if rejected_first_model {
        let count = prior
            .root
            .recovery_feedback
            .as_ref()
            .map_or(0, |feedback| feedback.count);
        if count >= ADAPTIVE_SCHEMA_MAX_CORRECTIONS {
            return Err(corrupt_store());
        }
        let crate::AdaptiveCursorV1::ModelRejected {
            reason_code,
            resolution_event_id,
        } = &source.cursor
        else {
            return Err(corrupt_store());
        };
        Some(AdaptiveRecoveryFeedbackV1 {
            count: count + 1,
            reason_code: reason_code.clone(),
            resolution_event_id: resolution_event_id.clone(),
            previous_session_id: session.grant.session_id,
        })
    } else {
        prior.root.recovery_feedback.clone()
    };
    if next.root.recovery_feedback != feedback {
        return Err(corrupt_store());
    }
    Ok(())
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
                if let Some(AdaptiveTransitionV1::ContinueGoverned { authorization }) =
                    &entry.command
                {
                    if authorization.resume_policy.is_some() {
                        require_resume_authorization_membership(
                            connection,
                            authorization,
                            &prior.grant.authority,
                        )?;
                    }
                    if let Some(epoch) = &authorization.work_funding {
                        require_funding_authorization_membership(
                            connection,
                            authorization,
                            &prior.grant.authority,
                        )?;
                        require_work_funding_anchor(connection, epoch, prior)?;
                    }
                }
                if let Some(AdaptiveTransitionV1::ResumeRejectedModel {
                    disposition_operation_id,
                    ..
                }) = &entry.command
                {
                    let record =
                        read_rejected_model_disposition(connection, id, *disposition_operation_id)?
                            .ok_or_else(corrupt_store)?;
                    if record.response != entry.session {
                        return Err(corrupt_store());
                    }
                }
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
            resume_policy: None,
            work_funding: None,
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
