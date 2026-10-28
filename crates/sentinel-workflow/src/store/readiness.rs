use super::*;
use crate::{AdaptiveCursorV1, AgentId};

#[cfg(test)]
mod tests;

pub(super) const EXECUTION_STATES: &str = "SELECT COUNT(*) FROM workflow_execution_outbox WHERE state NOT IN ('not_found','reserved','executing','awaiting_evidence','failed','cancelled','timed_out','unknown_outcome')";
pub(super) const EXECUTION_PENDING: &str = "SELECT request FROM workflow_execution_outbox WHERE state IN ('not_found','reserved','executing') ORDER BY updated_at_ms, invocation_id LIMIT ?1";
pub(super) const COMPLETION_STATES: &str = "SELECT COUNT(*) FROM workflow_completion_outbox WHERE state NOT IN ('pending','completed')";
pub(super) const COMPLETION_PENDING: &str = "SELECT request FROM workflow_completion_outbox WHERE state='pending' ORDER BY updated_at_ms, request_id LIMIT ?1";
pub(super) const GATE_STATES: &str = "SELECT COUNT(*) FROM workflow_gate_outbox WHERE state NOT IN ('pending','completed','timed_out')";
pub(super) const GATE_PENDING: &str = "SELECT request FROM workflow_gate_outbox WHERE state='pending' ORDER BY updated_at_ms, request_id LIMIT ?1";

/// Exact historical effect identity for a live EventStore health read, never retry authority.
#[derive(Debug, Clone, Serialize)]
pub struct AdaptivePendingModelHealth {
    request_id: String,
    request_digest: String,
    agent_id: AgentId,
}

impl AdaptivePendingModelHealth {
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    pub fn request_digest(&self) -> &str {
        &self.request_digest
    }

    pub fn agent_id(&self) -> AgentId {
        self.agent_id
    }
}

/// Payload-free projection after complete project/journal validation, in original head order.
#[derive(Debug, Clone, Serialize)]
pub struct AdaptiveHealthSession {
    project_present: bool,
    unknown_outcome: bool,
    pending_model: Option<AdaptivePendingModelHealth>,
}

impl AdaptiveHealthSession {
    pub fn project_present(&self) -> bool {
        self.project_present
    }

    pub fn unknown_outcome(&self) -> bool {
        self.unknown_outcome
    }

    pub fn pending_model(&self) -> Option<&AdaptivePendingModelHealth> {
        self.pending_model.as_ref()
    }
}

/// One SQL snapshot's facts, not an authority, provider result, or Ready verdict.
#[derive(Debug)]
pub struct WorkflowReadinessFacts {
    pub pending_execution: Result<usize, WorkflowError>,
    pub pending_completion: Result<usize, WorkflowError>,
    pub pending_gate: Result<usize, WorkflowError>,
    pub adaptive_sessions: Result<Vec<AdaptiveHealthSession>, AdaptiveHealthReadError>,
    pub canonical_event_cursor: Result<u64, WorkflowError>,
}

impl WorkflowReadinessFacts {
    fn failed(error: WorkflowError) -> Self {
        let copy = || WorkflowError::new(error.code, error.retryable, error.message);
        Self {
            pending_execution: Err(copy()),
            pending_completion: Err(copy()),
            pending_gate: Err(copy()),
            adaptive_sessions: Err(AdaptiveHealthReadError::Projects(copy())),
            canonical_event_cursor: Err(copy()),
        }
    }
}

impl WorkflowStore {
    /// All SQL guards are released before returning; external health must be read afterwards.
    pub fn readiness_facts(&self, limit: usize, include_adaptive: bool) -> WorkflowReadinessFacts {
        let mut connection = match self.lock() {
            Ok(connection) => connection,
            Err(error) => return WorkflowReadinessFacts::failed(error),
        };
        let result = self.with_validated_read_snapshot(
            &mut connection,
            "workflow-readiness-facts",
            |cache, snapshot, eligible| {
                let pending_execution = self.readiness_field(
                    WorkflowDiagnosticStage::PendingExecutions,
                    || pending_count::<PendingExecutionV1>(snapshot, EXECUTION_STATES, EXECUTION_PENDING, limit),
                );
                let pending_completion = self.readiness_field(
                    WorkflowDiagnosticStage::PendingCompletions,
                    || pending_count::<PendingCompletionEvidenceV1>(snapshot, COMPLETION_STATES, COMPLETION_PENDING, limit),
                );
                let pending_gate = self.readiness_field(
                    WorkflowDiagnosticStage::PendingGates,
                    || pending_count::<PendingGateEvidenceV1>(snapshot, GATE_STATES, GATE_PENDING, limit),
                );
                let mut validating_sessions = false;
                let adaptive_sessions = if include_adaptive {
                    self.readiness_field(WorkflowDiagnosticStage::AdaptiveInventory, || {
                        let mut validate = || adaptive_facts(snapshot, &mut validating_sessions);
                        match cache {
                            Some(cache) => cache.read_keyed_if(
                                snapshot, eligible, "adaptive-health-readiness", &[], validate, |_| true,
                            ),
                            None => validate(),
                        }
                    })
                    .map_err(|error| {
                        if validating_sessions {
                            AdaptiveHealthReadError::Sessions(error)
                        } else {
                            AdaptiveHealthReadError::Projects(error)
                        }
                    })
                } else {
                    Ok(Vec::new())
                };
                let canonical_event_cursor = self.readiness_field(
                    WorkflowDiagnosticStage::CompanyCursor,
                    || {
                        let cursor: i64 = snapshot.query_row(
                            "SELECT COALESCE(MAX(sequence),0) FROM company_events", [], |row| row.get(0),
                        )?;
                        stored_u64(cursor)
                    },
                );
                Ok(WorkflowReadinessFacts {
                    pending_execution,
                    pending_completion,
                    pending_gate,
                    adaptive_sessions,
                    canonical_event_cursor,
                })
            },
            read_cache::diagnostics_enabled(),
            None,
        );
        result.unwrap_or_else(WorkflowReadinessFacts::failed)
    }

    fn readiness_field<T>(
        &self,
        stage: WorkflowDiagnosticStage,
        read: impl FnOnce() -> Result<T, WorkflowError>,
    ) -> Result<T, WorkflowError> {
        let span = self.diagnostic_span(stage);
        let result = read();
        span.finish(result.is_ok());
        result
    }
}

fn adaptive_facts(
    snapshot: &Connection,
    validating_sessions: &mut bool,
) -> Result<Vec<AdaptiveHealthSession>, WorkflowError> {
    let (projects, sessions) =
        adaptive::adaptive_health_inventory_in_snapshot(snapshot, validating_sessions)?;
    let project_ids: BTreeSet<_> = projects
        .into_iter()
        .map(|project| (project.tenant_id, project.project_id))
        .collect();
    Ok(sessions
        .into_iter()
        .map(|session| AdaptiveHealthSession {
            project_present: project_ids.contains(&(
                session.grant.authority.tenant_id.clone(),
                session.grant.authority.project_id.clone(),
            )),
            unknown_outcome: matches!(
                &session.cursor,
                AdaptiveCursorV1::ModelUnknown { .. } | AdaptiveCursorV1::ToolUnknown { .. }
            ),
            pending_model: match session.cursor {
                AdaptiveCursorV1::ModelPending { effect } => Some(AdaptivePendingModelHealth {
                    request_id: format!("company-adaptive-{}-{}", session.grant.session_id, effect.id),
                    request_digest: effect.request_digest,
                    agent_id: session.grant.authority.agent_id,
                }),
                _ => None,
            },
        })
        .collect())
}

fn pending_count<T: DeserializeOwned + StoredSchema>(
    connection: &Connection,
    state_sql: &str,
    pending_sql: &str,
    limit: usize,
) -> Result<usize, WorkflowError> {
    validate_state_domain(connection, state_sql)?;
    read_pending::<T>(connection, pending_sql, limit).map(|rows| rows.len())
}

pub(super) fn validate_state_domain(connection: &Connection, sql: &str) -> Result<(), WorkflowError> {
    let invalid: i64 = connection.query_row(sql, [], |row| row.get(0)).map_err(map_sqlite_error)?;
    if invalid == 0 { Ok(()) } else { Err(corrupt_store()) }
}

pub(super) fn read_pending<T: DeserializeOwned + StoredSchema>(
    connection: &Connection,
    sql: &str,
    limit: usize,
) -> Result<Vec<T>, WorkflowError> {
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
