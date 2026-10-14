use super::*;
use crate::{AdaptiveProgressReadFacts, ProjectV1};

/// SQL facts only. Deferred errors are consumed in the caller's original order.
#[derive(Debug, Clone, Serialize)]
pub struct AdaptiveModelPreparationReadSnapshot {
    session: AdaptiveSessionV1,
    project: Result<ProjectV1, DeferredHeadReadError>,
    recovery_feedback: Result<Option<AdaptiveRecoveryFeedbackV1>, DeferredHeadReadError>,
    working_memory_source: Result<AdaptiveWorkingMemorySourceV1, DeferredHeadReadError>,
}

impl DeferredHeadReadError {
    fn capture(error: WorkflowError) -> Self {
        Self {
            code: error.code,
            retryable: error.retryable,
            message: error.message,
        }
    }

    fn restore(&self) -> WorkflowError {
        WorkflowError::new(self.code, self.retryable, self.message)
    }
}

impl AdaptiveModelPreparationReadSnapshot {
    pub fn session(&self) -> &AdaptiveSessionV1 {
        &self.session
    }

    pub fn project(&self) -> Result<&ProjectV1, WorkflowError> {
        self.project
            .as_ref()
            .map_err(DeferredHeadReadError::restore)
    }

    pub fn recovery_feedback(&self) -> Result<Option<&AdaptiveRecoveryFeedbackV1>, WorkflowError> {
        self.recovery_feedback
            .as_ref()
            .map(Option::as_ref)
            .map_err(DeferredHeadReadError::restore)
    }

    pub fn working_memory_source(&self) -> Result<&AdaptiveWorkingMemorySourceV1, WorkflowError> {
        self.working_memory_source
            .as_ref()
            .map_err(DeferredHeadReadError::restore)
    }

    fn retainable(&self) -> bool {
        self.project.is_ok() && self.recovery_feedback.is_ok() && self.working_memory_source.is_ok()
    }
}

impl WorkflowStore {
    /// Each scope keeps its old replay budget; only compact projections are retained.
    pub fn adaptive_progress_read_batch(
        &self,
        authorities: &[RuntimeAuthoritySnapshotV1],
    ) -> Result<Vec<Result<Option<AdaptiveProgressReadFacts>, WorkflowError>>, WorkflowError> {
        if authorities.is_empty() {
            return Ok(Vec::new());
        }
        if authorities.len() > 128 * 128 {
            return Err(WorkflowError::new(
                WorkflowErrorCode::InvalidInput,
                false,
                "customer progress batch limit exceeded",
            ));
        }
        let key = encode(&authorities)?;
        let mut connection = self.lock()?;
        let facts = self.validated_read_snapshot_if(
            &mut connection,
            "adaptive-progress-batch",
            &key,
            |tx| {
                Ok(authorities
                    .iter()
                    .map(|current| {
                        (|| {
                            current.validate()?;
                            adaptive_session_for_authority_on_connection(tx, current)
                                .map(|session| session.map(|value| value.progress_read_facts()))
                        })()
                        .map_err(DeferredHeadReadError::capture)
                    })
                    .collect::<Vec<_>>())
            },
            |facts| facts.iter().all(Result::is_ok),
        )?;
        Ok(facts
            .into_iter()
            .map(|value| value.map_err(|error| error.restore()))
            .collect())
    }

    /// One exact read proof, with optional new-context failures deferred for legacy replay.
    pub fn adaptive_model_preparation_read_snapshot(
        &self,
        session_id: Uuid,
        provider_version: u64,
        effect_id: Uuid,
        current: &RuntimeAuthoritySnapshotV1,
    ) -> Result<Option<AdaptiveModelPreparationReadSnapshot>, WorkflowError> {
        current.validate()?;
        let key = encode(&(session_id, provider_version, effect_id, current))?;
        let mut connection = self.lock()?;
        self.validated_read_snapshot_if(
            &mut connection,
            "adaptive-model-preparation",
            &key,
            |tx| {
                let scope = crate::domain_store::validation_scope::enter(tx)?;
                let mut collected = None;
                let Some((session, _, feedback)) =
                    load_with_working_memory(tx, session_id, provider_version, &mut collected)?
                else {
                    scope.finish()?;
                    return Ok(None);
                };
                authorize(&session.grant, current)?;
                require_head(tx, &session)?;
                // The exact current head was authenticated above. Reuse its replayed
                // feedback, including ModelRejected's derived feedback semantics.
                let recovery_feedback = Ok(recovery_feedback_from_loaded(&session, feedback));
                let memory = working_memory_source_from_loaded(
                    tx,
                    &session,
                    provider_version,
                    effect_id,
                    collected,
                );
                // Optional assembly errors cannot hide a valid legacy retained context.
                // A failed arena is discarded before reading an independent project.
                let working_memory_source = match memory {
                    Ok(source) => {
                        scope.finish()?;
                        Ok(source)
                    }
                    Err(error) => {
                        drop(scope);
                        Err(DeferredHeadReadError::capture(error))
                    }
                };
                let project = crate::domain_store::validated_company_project_in_snapshot(
                    tx,
                    &current.tenant_id,
                    &current.project_id,
                )
                .and_then(|value| value.ok_or_else(not_found))
                .map_err(DeferredHeadReadError::capture);
                Ok(Some(AdaptiveModelPreparationReadSnapshot {
                    session,
                    project,
                    recovery_feedback,
                    working_memory_source,
                }))
            },
            |value| {
                value
                    .as_ref()
                    .is_none_or(AdaptiveModelPreparationReadSnapshot::retainable)
            },
        )
    }
}

pub(super) fn recovery_feedback_from_loaded(
    session: &AdaptiveSessionV1,
    feedback: Option<AdaptiveRecoveryFeedbackV1>,
) -> Option<AdaptiveRecoveryFeedbackV1> {
    if let crate::AdaptiveCursorV1::ModelRejected {
        reason_code,
        resolution_event_id,
    } = &session.cursor
    {
        Some(AdaptiveRecoveryFeedbackV1 {
            count: feedback.as_ref().map_or(0, |value| value.count),
            reason_code: reason_code.clone(),
            resolution_event_id: resolution_event_id.clone(),
            previous_session_id: session.grant.session_id,
        })
    } else {
        feedback
    }
}

pub(super) fn working_memory_source_from_loaded(
    tx: &Connection,
    current_session: &AdaptiveSessionV1,
    provider_version: u64,
    effect_id: Uuid,
    collected: Option<WorkingMemoryReplayCollector>,
) -> Result<AdaptiveWorkingMemorySourceV1, WorkflowError> {
    let session_id = current_session.grant.session_id;
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
    let (head_entry_digest, session, mut rows, completed_tool_count, latest_test) = match collected
    {
        Some(collected) => collected.finish(&current_session.grant)?,
        None => {
            // An existing normal journal proof has no projection; preserve its fallback.
            let (digest, prefix) = evidence_entry(tx, &namespace(session_id), provider_version)?;
            if prefix.session.grant != current_session.grant {
                return Err(corrupt_store());
            }
            let rows = working_memory_rows(tx, &prefix.session)?;
            let count = u16::try_from(rows.len()).map_err(|_| corrupt_store())?;
            let latest_test = rows
                .iter()
                .rfind(|row| row.tool_kind == AdaptiveWorkingMemoryToolKindV1::RunTests)
                .cloned();
            (digest, prefix.session, rows, count, latest_test)
        }
    };
    if session.grant != current_session.grant {
        return Err(corrupt_store());
    }
    if rows.len() > ADAPTIVE_WORKING_MEMORY_MAX_ROWS {
        rows = rows.split_off(rows.len() - ADAPTIVE_WORKING_MEMORY_MAX_ROWS);
    }
    if usize::from(completed_tool_count) > ADAPTIVE_WORKING_MEMORY_MAX_ROWS {
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
    Ok(source)
}

#[cfg(test)]
mod tests;
