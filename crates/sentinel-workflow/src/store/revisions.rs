//! Same-work-item revisions preserve prior plans and receipts in the existing journal.

use super::*;
use serde::Deserialize;

const MAX_EXECUTION_REVISIONS: i64 = 4;

/// A source-bound revision request. Feedback is evidence, not additional authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionRevisionV1 {
    pub previous_plan_id: Uuid,
    pub previous_plan_digest: String,
    pub previous_version: u64,
    pub previous_state_digest: String,
    pub feedback_digest: String,
}

impl ExecutionRevisionV1 {
    pub fn from_completed_work(
        previous: &WorkItemExecutionV1,
        feedback_digest: String,
    ) -> Result<Self, WorkflowError> {
        if !validate_sha256(&feedback_digest) {
            return Err(invalid_revision());
        }
        Ok(Self {
            previous_plan_id: previous.plan.plan_id,
            previous_plan_digest: previous.plan.request_digest.clone(),
            previous_version: previous.version,
            previous_state_digest: state_digest(previous)?,
            feedback_digest,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchivedExecutionV1 {
    previous: WorkItemExecutionV1,
    revision: ExecutionRevisionV1,
    next_plan_id: Uuid,
    next_plan_digest: String,
    archived_at_ms: u64,
}

impl WorkflowStore {
    /// Starts a bounded revision without deleting old plans, effects or operation IDs.
    pub fn admit_revision_plan(
        &self,
        plan: &ExecutionPlanV1,
        revision: &ExecutionRevisionV1,
        authority: &RuntimeAuthoritySnapshotV1,
        now_ms: u64,
    ) -> Result<(bool, WorkItemExecutionV1), WorkflowError> {
        plan.validate_canonical()?;
        authority.validate()?;
        if !plan.authority_matches(authority) || !authority.active {
            return Err(authority_conflict());
        }
        validate_revision(revision)?;
        let subject = subject_namespace(&plan.tenant_id, &plan.project_id, &plan.work_item_id)?;
        let admission_namespace = format!("revision-admission:{subject}");
        let history_namespace = format!("execution-history:{subject}");
        let digest =
            canonical_sha256("sentinel.workflow.revision-admission.v1", &(plan, revision))?;
        let mut connection = self.lock()?;
        let transaction = immediate(&mut connection)?;
        if let Some((stored_digest, response, created_at)) = read_operation(
            &transaction,
            &admission_namespace,
            &plan.plan_id.to_string(),
        )? {
            if !constant_time_eq(&digest, &stored_digest) {
                return Err(idempotency_conflict());
            }
            let admitted: WorkItemExecutionV1 = decode(&response)?;
            validate_stored_work_item(
                &admitted,
                &plan.tenant_id,
                &plan.project_id,
                &plan.work_item_id,
                admitted.version,
            )?;
            let archive =
                read_archive(&transaction, &history_namespace, revision.previous_plan_id)?
                    .ok_or_else(corrupt_store)?;
            let current = read_plan_version(
                &transaction,
                &plan.tenant_id,
                &plan.project_id,
                &plan.work_item_id,
                plan.plan_id,
            )?
            .ok_or_else(corrupt_store)?;
            if admitted.plan != *plan
                || admitted.state != WorkItemState::Claimed
                || Some(admitted.version) != revision.previous_version.checked_add(1)
                || admitted.updated_at_unix_ms != stored_u64(created_at)?
                || archive.revision != *revision
                || archive.next_plan_id != plan.plan_id
                || archive.next_plan_digest != plan.request_digest
                || archive.archived_at_ms != admitted.updated_at_unix_ms
                || current.plan != *plan
            {
                return Err(corrupt_store());
            }
            return Ok((true, admitted));
        }
        plan.validate_at(now_ms)?;
        let previous = read_work_item(
            &transaction,
            &plan.tenant_id,
            &plan.project_id,
            &plan.work_item_id,
        )?
        .ok_or_else(invalid_revision)?;
        if previous.plan.plan_id != revision.previous_plan_id
            || previous.plan.request_digest != revision.previous_plan_digest
            || previous.version != revision.previous_version
            || state_digest(&previous)? != revision.previous_state_digest
            || previous.plan.plan_id == plan.plan_id
            || !previous.plan.authority_matches(authority)
            || now_ms < previous.updated_at_unix_ms
            || plan.created_at_unix_ms < previous.updated_at_unix_ms
        {
            return Err(authority_conflict());
        }
        require_completed_source(&transaction, &previous)?;
        if read_plan_version(
            &transaction,
            &plan.tenant_id,
            &plan.project_id,
            &plan.work_item_id,
            plan.plan_id,
        )?
        .is_some()
        {
            return Err(idempotency_conflict());
        }
        let count: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM workflow_operations WHERE operation_namespace=?1",
                [&history_namespace],
                |row| row.get(0),
            )
            .map_err(map_sqlite_error)?;
        if count >= MAX_EXECUTION_REVISIONS {
            return Err(invalid_revision());
        }
        // All new invocation IDs must be unused before any new step is admitted.
        for step in &plan.steps {
            if read_execution_row(&transaction, step.invocation_id)?.is_some()
                || previous.plan.steps.iter().any(|old| {
                    old.step_id == step.step_id || old.invocation_id == step.invocation_id
                })
            {
                return Err(idempotency_conflict());
            }
        }
        let archive = ArchivedExecutionV1 {
            previous: previous.clone(),
            revision: revision.clone(),
            next_plan_id: plan.plan_id,
            next_plan_digest: plan.request_digest.clone(),
            archived_at_ms: now_ms,
        };
        insert_operation(
            &transaction,
            &history_namespace,
            &previous.plan.plan_id.to_string(),
            &canonical_sha256("sentinel.workflow.execution-history.v1", &archive)?,
            &archive,
            now_ms,
        )?;
        let authority_digest = authority.canonical_digest()?;
        let pending = PendingExecutionV1 {
            schema_version: WORKFLOW_SCHEMA_VERSION,
            plan_id: plan.plan_id,
            plan_digest: plan.request_digest.clone(),
            step: plan.steps[0].clone(),
            authority_snapshot_digest: authority_digest.clone(),
            state: ExecutionReconcileState::NotFound,
            attempts: 0,
            created_at_unix_ms: now_ms,
            updated_at_unix_ms: now_ms,
        };
        let admitted = WorkItemExecutionV1 {
            schema_version: WORKFLOW_SCHEMA_VERSION,
            tenant_id: plan.tenant_id.clone(),
            project_id: plan.project_id.clone(),
            work_item_id: plan.work_item_id.clone(),
            agent_id: plan.agent_id,
            state: WorkItemState::Claimed,
            version: previous
                .version
                .checked_add(1)
                .ok_or_else(invalid_revision)?,
            plan: plan.clone(),
            next_step_ordinal: 0,
            terminal_execution_evidence: None,
            gate_evidence: None,
            blocker_code: None,
            updated_at_unix_ms: now_ms,
        };
        write_current_work_item(&transaction, &admitted)?;
        insert_execution(&transaction, plan, &pending)?;
        append_audit(
            &transaction,
            &admitted,
            "execution_revision_admitted",
            Some(previous.state),
            WorkItemState::Claimed,
            &authority_digest,
            &digest,
            now_ms,
        )?;
        insert_operation(
            &transaction,
            &admission_namespace,
            &plan.plan_id.to_string(),
            &digest,
            &admitted,
            now_ms,
        )?;
        transaction.commit().map_err(map_sqlite_error)?;
        Ok((false, admitted))
    }

    pub fn work_item_for_plan(
        &self,
        tenant: &crate::TenantId,
        project: &crate::ProjectId,
        work: &crate::WorkItemId,
        plan_id: Uuid,
    ) -> Result<Option<WorkItemExecutionV1>, WorkflowError> {
        let connection = self.lock()?;
        read_plan_version(&connection, tenant, project, work, plan_id)
    }
}

fn validate_revision(revision: &ExecutionRevisionV1) -> Result<(), WorkflowError> {
    if revision.previous_plan_id.is_nil()
        || revision.previous_version == 0
        || !validate_sha256(&revision.previous_plan_digest)
        || !validate_sha256(&revision.previous_state_digest)
        || !validate_sha256(&revision.feedback_digest)
    {
        return Err(invalid_revision());
    }
    Ok(())
}

pub(crate) fn require_completed_source(
    connection: &Connection,
    previous: &WorkItemExecutionV1,
) -> Result<(), WorkflowError> {
    if previous.state != WorkItemState::Done
        && !(previous.state == WorkItemState::Blocked
            && previous.blocker_code.as_deref() == Some("execution_failed"))
    {
        return Err(invalid_revision());
    }
    let executed = usize::from(previous.next_step_ordinal) + 1;
    for (ordinal, step) in previous.plan.steps.iter().take(executed).enumerate() {
        let row = read_execution_row(connection, step.invocation_id)?.ok_or_else(corrupt_store)?;
        let request: PendingExecutionV1 = decode(&row.request)?;
        let failed = previous.state == WorkItemState::Blocked && ordinal + 1 == executed;
        let expected_state = if failed {
            ExecutionReconcileState::Failed
        } else {
            ExecutionReconcileState::Succeeded
        };
        if request.schema_version != WORKFLOW_SCHEMA_VERSION
            || row.invocation_id != step.invocation_id.to_string()
            || request.created_at_unix_ms > request.updated_at_unix_ms
            || !validate_sha256(&request.authority_snapshot_digest)
            || request.plan_id != previous.plan.plan_id
            || request.plan_digest != previous.plan.request_digest
            || request.step != *step
            || request.state != expected_state
            || row.state != execution_database_state(expected_state)
            || row.tenant_id != previous.tenant_id.0
            || row.project_id != previous.project_id.0
            || row.work_item_id != previous.work_item_id.0
            || row.plan_digest != previous.plan.request_digest
            || stored_u16(row.step_ordinal)? != step.ordinal
            || stored_u16(row.attempts)? != request.attempts
            || stored_u64(row.updated_at_ms)? != request.updated_at_unix_ms
        {
            return Err(corrupt_store());
        }
        if !failed {
            let completion_id = completion_request_identity(
                previous.plan.plan_id,
                step.step_id,
                step.invocation_id,
                &previous.plan.request_digest,
            )?;
            let completion =
                read_completion_row(connection, &completion_id)?.ok_or_else(corrupt_store)?;
            let completion: PendingCompletionEvidenceV1 = decode(&completion.request)?;
            if !validate_completion_request(connection, previous, &completion)? {
                return Err(corrupt_store());
            }
        }
    }
    if previous.state == WorkItemState::Done {
        let terminal = previous
            .terminal_execution_evidence
            .as_ref()
            .ok_or_else(corrupt_store)?;
        let gate_id = gate_request_identity(
            previous.plan.plan_id,
            &previous.plan.request_digest,
            &terminal.receipt_id,
        )?;
        let gate = read_gate_row(connection, &gate_id)?.ok_or_else(corrupt_store)?;
        let gate: PendingGateEvidenceV1 = decode(&gate.request)?;
        if !validate_gate_request(connection, previous, &gate)? {
            return Err(corrupt_store());
        }
    }
    for table in ["workflow_completion_outbox", "workflow_gate_outbox"] {
        let pending: i64 = connection.query_row(&format!(
            "SELECT COUNT(*) FROM {table} WHERE tenant_id=?1 AND project_id=?2 AND work_item_id=?3 AND state!='completed'"
        ), params![previous.tenant_id.0, previous.project_id.0, previous.work_item_id.0], |row| row.get(0))
            .map_err(map_sqlite_error)?;
        if pending != 0 {
            return Err(invalid_revision());
        }
    }
    Ok(())
}

fn state_digest(previous: &WorkItemExecutionV1) -> Result<String, WorkflowError> {
    canonical_sha256("sentinel.workflow.revision-source.v1", previous)
}

fn subject_namespace(
    tenant: &crate::TenantId,
    project: &crate::ProjectId,
    work: &crate::WorkItemId,
) -> Result<String, WorkflowError> {
    canonical_sha256(
        "sentinel.workflow.execution-subject.v1",
        &(tenant, project, work),
    )
}

fn read_archive(
    connection: &Connection,
    namespace: &str,
    plan_id: Uuid,
) -> Result<Option<ArchivedExecutionV1>, WorkflowError> {
    read_operation(connection, namespace, &plan_id.to_string())?
        .map(|(digest, bytes, created_at)| {
            let archive: ArchivedExecutionV1 = decode(&bytes)?;
            let previous = &archive.previous;
            validate_stored_work_item(
                previous,
                &previous.tenant_id,
                &previous.project_id,
                &previous.work_item_id,
                previous.version,
            )?;
            validate_revision(&archive.revision).map_err(|_| corrupt_store())?;
            if digest != canonical_sha256("sentinel.workflow.execution-history.v1", &archive)?
                || previous.plan.plan_id != plan_id
                || archive.next_plan_id.is_nil()
                || archive.next_plan_id == plan_id
                || !validate_sha256(&archive.next_plan_digest)
                || archive.archived_at_ms != stored_u64(created_at)?
                || archive.archived_at_ms < previous.updated_at_unix_ms
                || archive.revision
                    != ExecutionRevisionV1::from_completed_work(
                        previous,
                        archive.revision.feedback_digest.clone(),
                    )?
                || namespace
                    != format!(
                        "execution-history:{}",
                        subject_namespace(
                            &previous.tenant_id,
                            &previous.project_id,
                            &previous.work_item_id
                        )?
                    )
            {
                return Err(corrupt_store());
            }
            Ok(archive)
        })
        .transpose()
}

pub(super) fn read_plan_version(
    connection: &Connection,
    tenant: &crate::TenantId,
    project: &crate::ProjectId,
    work: &crate::WorkItemId,
    plan_id: Uuid,
) -> Result<Option<WorkItemExecutionV1>, WorkflowError> {
    let current = read_work_item(connection, tenant, project, work)?;
    if current
        .as_ref()
        .is_some_and(|value| value.plan.plan_id == plan_id)
    {
        return Ok(current);
    }
    let namespace = format!(
        "execution-history:{}",
        subject_namespace(tenant, project, work)?
    );
    read_archive(connection, &namespace, plan_id).map(|value| value.map(|archive| archive.previous))
}

fn invalid_revision() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::InvalidInput,
        false,
        "execution revision requires a completed source and bounded fresh authority",
    )
}
