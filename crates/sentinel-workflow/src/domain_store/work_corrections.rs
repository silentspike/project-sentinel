use super::*;

const MAX_CORRECTIONS_PER_WORK: usize = 4;

pub(super) fn request(
    connection: &Connection,
    project: &mut ProjectV1,
    principal: &AuthenticatedCompanyPrincipalV1,
    operation_id: Uuid,
    command: &CompanyWorkflowCommandV1,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    let CompanyWorkflowCommandV1::RequestWorkCorrection {
        work_item_id,
        expected_work_version,
        execution_revision,
        feedback_ref,
        ..
    } = command
    else {
        return Err(invalid("work correction command required"));
    };
    require_role(
        principal,
        &[CompanyRoleV1::ProjectManager, CompanyRoleV1::TechnicalLead],
    )?;
    validate_identifier(feedback_ref)?;
    if !matches!(
        project.lifecycle_state,
        ProjectLifecycleStateV1::Active
            | ProjectLifecycleStateV1::DeliveryCandidate
            | ProjectLifecycleStateV1::Blocked
    ) || project.work_corrections.len() >= MAX_AGGREGATE_ITEMS
        || project
            .work_corrections
            .iter()
            .filter(|record| &record.previous.spec.work_item_id == work_item_id)
            .count()
            >= MAX_CORRECTIONS_PER_WORK
        || project.work_items.values().any(|work| {
            work.spec.dependency_ids.contains(work_item_id)
                && work.state != CompanyWorkStateV1::DependencyPending
        })
        || project
            .handoffs
            .iter()
            .any(|handoff| &handoff.work_item_id == work_item_id)
        || project.blockers.iter().any(|blocker| {
            blocker.state != BlockerStateV1::Resolved
                && blocker
                    .work_item_id
                    .as_ref()
                    .is_none_or(|id| id == work_item_id)
        })
    {
        return Err(invalid(
            "work correction requires unconsumed output and no unresolved project blocker",
        ));
    }
    let previous = project
        .work_items
        .get(work_item_id)
        .ok_or_else(not_found)?
        .clone();
    require_version(previous.version, *expected_work_version)?;
    if !matches!(
        previous.state,
        CompanyWorkStateV1::Done | CompanyWorkStateV1::InReview | CompanyWorkStateV1::Blocked
    ) {
        return Err(transition());
    }
    let execution = read_work_item(
        connection,
        &project.tenant_id,
        &project.project_id,
        work_item_id,
    )?
    .ok_or_else(not_found)?;
    crate::store::require_completed_source(connection, &execution)?;
    let expected = crate::ExecutionRevisionV1::from_completed_work(
        &execution,
        execution_revision.feedback_digest.clone(),
    )?;
    let assignment = current_assignment(&previous).ok_or_else(transition)?;
    if expected != *execution_revision
        || execution.updated_at_unix_ms > now_ms
        || execution.plan.agent_id != assignment.agent_id
        || execution.plan.assignment_version != assignment.assignment_version
        || execution.plan.assignment_digest != assignment.canonical_digest()?
        || (previous.state == CompanyWorkStateV1::Blocked)
            != (execution.state == crate::WorkItemState::Blocked)
    {
        return Err(invalid("work correction source or assignment changed"));
    }
    if matches!(
        previous.state,
        CompanyWorkStateV1::Done | CompanyWorkStateV1::InReview
    ) {
        let terminal = execution
            .terminal_execution_evidence
            .as_ref()
            .ok_or_else(corrupt)?;
        if previous.output_receipts.len() != terminal.outputs.len()
            || previous
                .output_receipts
                .iter()
                .zip(&terminal.outputs)
                .any(|(receipt, output)| receipt.content_digest != output.digest)
            || previous.gate_receipt.as_ref().is_some_and(|receipt| {
                execution
                    .gate_evidence
                    .as_ref()
                    .is_none_or(|gate| receipt.subject_digest != gate.subject_digest)
            })
        {
            return Err(invalid(
                "work correction output does not match sealed execution",
            ));
        }
    }
    let correction = WorkCorrectionV1 {
        correction_id: stable_domain_id("correction", &principal.tenant_id, operation_id)?,
        previous: previous.clone(),
        execution_revision: execution_revision.clone(),
        feedback_ref: feedback_ref.clone(),
        requested_by: principal.principal_id.clone(),
        requested_at_unix_ms: now_ms,
    };
    let actor = authorize_project_actor(project, principal)?;
    let work = project
        .work_items
        .get_mut(work_item_id)
        .ok_or_else(not_found)?;
    work.state = CompanyWorkStateV1::Assigned;
    work.version = work.version.checked_add(1).ok_or_else(corrupt)?;
    work.output_receipts.clear();
    work.gate_receipt = None;
    append_work_transition(
        work,
        principal,
        actor,
        previous.state,
        CompanyWorkStateV1::Assigned,
        &correction.correction_id,
        now_ms,
    )?;
    project.work_corrections.push(correction);
    refresh_project_lifecycle(project);
    Ok(())
}

pub(super) fn matches_transition(
    project: &ProjectV1,
    work: &CompanyWorkItemV1,
    index: usize,
    audit: &StateTransitionAuditV1,
) -> bool {
    project.work_corrections.iter().any(|record| {
        record.previous.spec.work_item_id == work.spec.work_item_id
            && record.previous.transition_history.len() == index
            && record.correction_id == audit.reason_ref
            && record.requested_by == audit.actor_id
            && record.requested_at_unix_ms == audit.occurred_at_unix_ms
            && enum_name(record.previous.state) == audit.before
            && audit.after == "Assigned"
    })
}

pub(super) fn validate(project: &ProjectV1) -> Result<(), WorkflowError> {
    if project.work_corrections.len() > MAX_AGGREGATE_ITEMS {
        return Err(corrupt());
    }
    let mut counts = BTreeMap::<WorkItemId, usize>::new();
    let mut ids = BTreeSet::new();
    let mut plans = BTreeSet::new();
    for record in &project.work_corrections {
        validate_identifier(&record.correction_id).map_err(|_| corrupt())?;
        validate_identifier(&record.feedback_ref).map_err(|_| corrupt())?;
        let revision = &record.execution_revision;
        validate_digest(&revision.previous_plan_digest).map_err(|_| corrupt())?;
        validate_digest(&revision.previous_state_digest).map_err(|_| corrupt())?;
        validate_digest(&revision.feedback_digest).map_err(|_| corrupt())?;
        let previous = &record.previous;
        let current = project
            .work_items
            .get(&previous.spec.work_item_id)
            .ok_or_else(corrupt)?;
        let requester = project
            .governance
            .participants
            .iter()
            .find(|p| p.principal_id == record.requested_by)
            .ok_or_else(corrupt)?;
        let count = counts
            .entry(previous.spec.work_item_id.clone())
            .or_default();
        *count += 1;
        let index = previous.transition_history.len();
        let audit = current.transition_history.get(index).ok_or_else(corrupt)?;
        if *count > MAX_CORRECTIONS_PER_WORK
            || !ids.insert(&record.correction_id)
            || !plans.insert(revision.previous_plan_id)
            || revision.previous_plan_id.is_nil()
            || revision.previous_version == 0
            || previous.spec != current.spec
            || previous.version >= current.version
            || !matches!(
                previous.state,
                CompanyWorkStateV1::Done
                    | CompanyWorkStateV1::InReview
                    | CompanyWorkStateV1::Blocked
            )
            || !matches!(
                requester.role,
                CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
            )
            || requester.agent_id != audit.actor_agent_id
            || record.requested_at_unix_ms < project.created_at_unix_ms
            || record.requested_at_unix_ms > project.updated_at_unix_ms
            || previous.transition_history.as_slice() != &current.transition_history[..index]
            || !assignment_history_matches(previous, current, record.requested_at_unix_ms)
            || !matches_transition(project, current, index, audit)
        {
            return Err(corrupt());
        }
        validate_work_transition_history(previous, record.requested_at_unix_ms)?;
        if matches!(
            previous.state,
            CompanyWorkStateV1::Done | CompanyWorkStateV1::InReview
        ) {
            validate_output_receipts(&previous.spec, &previous.output_receipts)
                .map_err(|_| corrupt())?;
        } else if !previous.output_receipts.is_empty() {
            return Err(corrupt());
        }
        if previous.state == CompanyWorkStateV1::Done {
            let gate = previous.gate_receipt.as_ref().ok_or_else(corrupt)?;
            if !gate.passed
                || gate.gate_id != previous.spec.quality_gate.gate_id
                || gate.generation != previous.spec.quality_gate.generation
                || gate.gate_digest != previous.spec.quality_gate.digest
            {
                return Err(corrupt());
            }
            validate_digest(&gate.subject_digest).map_err(|_| corrupt())?;
        } else if previous.gate_receipt.is_some() {
            return Err(corrupt());
        }
    }
    Ok(())
}

fn assignment_history_matches(
    previous: &CompanyWorkItemV1,
    current: &CompanyWorkItemV1,
    recorded_at: u64,
) -> bool {
    previous.assignments.len() <= current.assignments.len()
        && previous
            .assignments
            .iter()
            .zip(&current.assignments)
            .all(|(old, current)| {
                if old == current {
                    return true;
                }
                if !old.active
                    || current.active
                    || old.ended_at_unix_ms.is_some()
                    || !current
                        .ended_at_unix_ms
                        .is_some_and(|ended| ended >= recorded_at)
                {
                    return false;
                }
                let mut normalized = current.clone();
                normalized.active = old.active;
                normalized.ended_at_unix_ms = old.ended_at_unix_ms;
                &normalized == old
            })
}
