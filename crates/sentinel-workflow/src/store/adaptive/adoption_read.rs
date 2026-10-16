use super::*;

/// Exact persisted adoption facts, never permission to execute a new effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum AdaptiveModelAdoptionRead {
    Missing,
    GrantChanged,
    Current {
        session: Box<AdaptiveSessionV1>,
        already_adopted: bool,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum AdaptiveModelAdoptionReadError {
    Session(WorkflowError),
    Replay(WorkflowError),
}

const REPLAY_FAILURE: WorkflowError = WorkflowError::new(
    WorkflowErrorCode::PersistenceFailure,
    false,
    "adaptive model adoption replay boundary failed",
);

impl WorkflowStore {
    /// One historical read boundary; the caller must obtain current runtime authority.
    pub fn adaptive_model_adoption_read(
        &self,
        grant: &AdaptiveSessionGrantV1,
        current: &RuntimeAuthoritySnapshotV1,
        effect: &AdaptiveEffectV1,
        result_digest: &str,
        decision: &AdaptiveModelDecisionV1,
    ) -> Result<AdaptiveModelAdoptionRead, AdaptiveModelAdoptionReadError> {
        use AdaptiveModelAdoptionReadError::{Replay, Session};

        current.validate().map_err(Session)?;
        let query_key = canonical_sha256(
            "sentinel.workflow.adaptive-model-adoption-read-key.v1",
            &(grant, current, effect, result_digest, decision),
        )
        .map_err(Session)?;
        let mut connection = self.lock().map_err(Session)?;
        let mut replay_error = None;
        let read = self.validated_read_snapshot(
            &mut connection,
            "adaptive-model-adoption",
            query_key.as_bytes(),
            |snapshot| {
                crate::domain_store::validation_scope::with_scope(snapshot, || {
                    let Some((session, _)) = load(snapshot, grant.session_id)? else {
                        return Ok(AdaptiveModelAdoptionRead::Missing);
                    };
                    authorize(&session.grant, current)?;
                    require_head(snapshot, &session)?;
                    if session.effective_grant() != *grant {
                        return Ok(AdaptiveModelAdoptionRead::GrantChanged);
                    }
                    let replay = (|| {
                        authorize(grant, &grant.authority)?;
                        model_result_is_adopted_from_loaded(
                            snapshot,
                            &session,
                            effect,
                            result_digest,
                            decision,
                        )
                    })();
                    let already_adopted = match replay {
                        Ok(adopted) => adopted,
                        Err(error) => {
                            replay_error = Some(error);
                            return Err(REPLAY_FAILURE);
                        }
                    };
                    Ok(AdaptiveModelAdoptionRead::Current {
                        session: Box::new(session),
                        already_adopted,
                    })
                })
            },
        );
        // A warm typed-miss integrity error can replace the validator's error.
        // Restore replay provenance only when its private marker survives intact.
        match (read, replay_error) {
            (Err(error), Some(replay)) if error == REPLAY_FAILURE => Err(Replay(replay)),
            (Err(error), _) => Err(Session(error)),
            (Ok(value), _) => Ok(value),
        }
    }
}

/// The caller has fully replayed this head in the same snapshot and validation scope.
pub(super) fn model_result_is_adopted_from_loaded(
    connection: &Connection,
    session: &AdaptiveSessionV1,
    effect: &AdaptiveEffectV1,
    result_digest: &str,
    decision: &AdaptiveModelDecisionV1,
) -> Result<bool, WorkflowError> {
    if session.is_abandoned_model_effect(effect) {
        return Err(authority_conflict());
    }
    let entries = usize::try_from(session.version).map_err(|_| corrupt_store())?;
    if entries == 0 || entries > MAX_JOURNAL_ENTRIES {
        return Err(corrupt_store());
    }
    let mut collaboration_digest = None;
    // Full replay already checked every ordered key, digest and transition. These
    // lookups reuse its Arc owners rather than decoding a second journal copy.
    for version in 1..=session.version {
        let stored = journal_entry(connection, session.grant.session_id, version)?
            .ok_or_else(corrupt_store)?;
        match &stored.0.entry.command {
            Some(AdaptiveTransitionV1::ResolveModel {
                effect: recorded_effect,
                result_digest: recorded_digest,
                decision: recorded_decision,
            }) if recorded_effect == effect
                && recorded_digest == result_digest
                && recorded_decision == decision =>
            {
                if let AdaptiveModelDecisionV1::Collaborate { action } = recorded_decision {
                    collaboration_digest = Some(adaptive_collaboration_digest(action)?);
                } else {
                    return Ok(true);
                }
            }
            Some(AdaptiveTransitionV1::CommitCollaboration {
                effect: recorded_effect,
                action_digest,
            }) if recorded_effect == effect
                && collaboration_digest.as_ref() == Some(action_digest) =>
            {
                return Ok(true);
            }
            _ => {}
        }
    }
    Ok(false)
}
