use super::*;

const REWORK_REASON: &str = "qa-source-rework";

pub(super) fn restart(
    connection: &Connection,
    project: &mut ProjectV1,
    principal: &AuthenticatedCompanyPrincipalV1,
    operation_id: Uuid,
    command: &CompanyWorkflowCommandV1,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    let CompanyWorkflowCommandV1::RestartSourceAfterQa {
        review_work_item_id,
        source_work_item_id,
        blocker_id,
        report_digest,
        expected_source_work_version,
        source_execution_revision,
        feedback,
        next_subscription_grant,
        ..
    } = command
    else {
        return Err(invalid("source review rework command required"));
    };
    require_role(
        principal,
        &[CompanyRoleV1::ProjectManager, CompanyRoleV1::TechnicalLead],
    )?;
    validate_digest(report_digest)?;
    validate_identifier(blocker_id)?;
    ensure_collection_capacity(project.archived_source_reviews.len())?;
    if project.lifecycle_state != ProjectLifecycleStateV1::Blocked
        || project.archived_source_reviews.iter().any(|entry| {
            entry.report_digest == *report_digest
                || entry.review_work.spec.work_item_id == *review_work_item_id
        })
    {
        return Err(transition());
    }
    let source = project
        .work_items
        .get(source_work_item_id)
        .filter(|work| {
            work.state == CompanyWorkStateV1::Done
                && work.output_receipts.len() == 1
                && matches!(
                    work.spec.required_role,
                    CompanyRoleV1::Developer | CompanyRoleV1::Designer
                )
                && work.version == *expected_source_work_version
        })
        .ok_or_else(transition)?
        .clone();
    let review = project
        .work_items
        .get(review_work_item_id)
        .filter(|work| {
            work.state == CompanyWorkStateV1::Done
                && work.spec.required_role == CompanyRoleV1::Qa
                && work.spec.rework.is_none()
                && work.spec.dependency_ids.contains(source_work_item_id)
                && work.output_receipts.len() == 1
                && work.output_receipts[0].content_digest == *report_digest
                && work.gate_receipt.as_ref().is_some_and(|gate| gate.passed)
        })
        .ok_or_else(transition)?
        .clone();
    if project.work_items.values().any(|work| {
        work.spec.dependency_ids.contains(review_work_item_id)
            || work.spec.dependency_ids.contains(source_work_item_id)
                && work.spec.work_item_id != *review_work_item_id
                && work.state != CompanyWorkStateV1::DependencyPending
    }) {
        return Err(invalid("QA source output has another active consumer"));
    }
    let prior_source = project
        .source_review_previous_call
        .as_ref()
        .filter(|allowance| {
            allowance.grant.work_item_id == *source_work_item_id && allowance.dispatch.is_some()
        })
        .ok_or_else(transition)?
        .clone();
    let prior_review = project
        .subscription_call
        .as_ref()
        .filter(|allowance| {
            allowance.grant.work_item_id == *review_work_item_id
                && allowance.dispatch.is_some()
                && allowance.allowance_id != prior_source.allowance_id
        })
        .ok_or_else(transition)?
        .clone();
    let open = project
        .blockers
        .iter()
        .filter(|blocker| blocker.state != BlockerStateV1::Resolved)
        .collect::<Vec<_>>();
    if open.len() != 1
        || open[0].blocker_id != *blocker_id
        || open[0].state != BlockerStateV1::Open
        || open[0].work_item_id.as_ref() != Some(source_work_item_id)
        || open[0].cause_ref != format!("qa-source-review:{report_digest}")
        || source
            .assignments
            .iter()
            .find(|assignment| assignment.active)
            .is_none_or(|assignment| assignment.agent_id != open[0].owner)
    {
        return Err(invalid("QA blocker authority changed"));
    }
    if feedback.artifact_digest.as_ref()
        != source
            .output_receipts
            .first()
            .map(|receipt| &receipt.content_digest)
        || feedback.canonical_digest()? != source_execution_revision.feedback_digest
        || next_subscription_grant.work_item_id != *source_work_item_id
        || next_subscription_grant.agent_id != prior_source.grant.agent_id
        || next_subscription_grant.assignment_id != prior_source.grant.assignment_id
        || next_subscription_grant.assignment_version != prior_source.grant.assignment_version
        || next_subscription_grant.provider != prior_source.grant.provider
        || next_subscription_grant.model != prior_source.grant.model
        || next_subscription_grant.catalog_digest != prior_source.grant.catalog_digest
    {
        return Err(invalid("QA rework grant or feedback changed"));
    }

    let review_corrections = project
        .work_corrections
        .iter()
        .filter(|record| record.previous.spec.work_item_id == *review_work_item_id)
        .cloned()
        .collect::<Vec<_>>();
    project
        .work_corrections
        .retain(|record| record.previous.spec.work_item_id != *review_work_item_id);
    let review_abandoned_calls = project
        .abandoned_subscription_calls
        .iter()
        .filter(|entry| entry.allowance.grant.work_item_id == *review_work_item_id)
        .cloned()
        .collect::<Vec<_>>();
    project
        .abandoned_subscription_calls
        .retain(|entry| entry.allowance.grant.work_item_id != *review_work_item_id);
    project.work_items.remove(review_work_item_id);
    project.source_review_previous_call = None;
    project.subscription_call = Some(prior_source.clone());
    project
        .archived_source_reviews
        .push(ArchivedSourceReviewV1 {
            review_work: review,
            review_corrections,
            review_abandoned_calls,
            source_work: source,
            review_allowance: prior_review,
            source_allowance: prior_source,
            report_digest: report_digest.clone(),
            blocker_id: blocker_id.clone(),
            archived_at_unix_ms: now_ms,
        });
    let actor = principal.agent_id.ok_or_else(unauthorized)?;
    let blocker = project
        .blockers
        .iter_mut()
        .find(|blocker| blocker.blocker_id == *blocker_id)
        .ok_or_else(corrupt)?;
    let reason = format!("{REWORK_REASON}:{report_digest}");
    blocker.state = BlockerStateV1::Resolved;
    blocker.resolution_ref = Some(reason.clone());
    blocker.last_actor_id = principal.principal_id.clone();
    blocker.updated_at_unix_ms = now_ms;
    blocker.transition_history.push(StateTransitionAuditV1 {
        before: "Open".to_owned(),
        after: "Resolved".to_owned(),
        actor_id: principal.principal_id.clone(),
        actor_agent_id: actor,
        reason_ref: reason,
        occurred_at_unix_ms: now_ms,
    });
    restore_project_lifecycle_after_last_blocker(project);

    let correction = CompanyWorkflowCommandV1::RequestWorkCorrection {
        project_id: project.project_id.clone(),
        expected_version: project.version,
        work_item_id: source_work_item_id.clone(),
        expected_work_version: *expected_source_work_version,
        execution_revision: source_execution_revision.clone(),
        feedback_ref: report_digest.clone(),
        feedback: Some(feedback.clone()),
        next_subscription_grant: Some(next_subscription_grant.clone()),
    };
    work_corrections::request(
        connection,
        project,
        principal,
        operation_id,
        &correction,
        now_ms,
    )
}

pub(super) fn validate(project: &ProjectV1) -> Result<(), WorkflowError> {
    let mut reports = BTreeSet::new();
    let mut reviews = BTreeSet::new();
    let mut correction_ids = BTreeSet::new();
    let mut abandoned_allowance_ids = project
        .abandoned_subscription_calls
        .iter()
        .map(|entry| entry.allowance.allowance_id.as_str())
        .collect::<BTreeSet<_>>();
    let mut resolution_event_ids = project
        .abandoned_subscription_calls
        .iter()
        .map(|entry| entry.resolution_event_id.as_str())
        .collect::<BTreeSet<_>>();
    for entry in &project.archived_source_reviews {
        validate_digest(&entry.report_digest).map_err(|_| corrupt())?;
        validate_identifier(&entry.blocker_id).map_err(|_| corrupt())?;
        let review_id = &entry.review_work.spec.work_item_id;
        let source_id = &entry.source_work.spec.work_item_id;
        if !reports.insert(&entry.report_digest)
            || !reviews.insert(review_id)
            || project.work_items.contains_key(review_id)
            || entry.review_work.state != CompanyWorkStateV1::Done
            || entry.review_work.spec.required_role != CompanyRoleV1::Qa
            || entry.review_corrections.iter().any(|record| {
                record.previous.spec.work_item_id != *review_id
                    || !correction_ids.insert(record.correction_id.clone())
            })
            || entry.review_abandoned_calls.iter().any(|call| {
                call.allowance.grant.work_item_id != *review_id
                    || call.allowance.allowance_id == entry.review_allowance.allowance_id
                    || !abandoned_allowance_ids.insert(&call.allowance.allowance_id)
                    || !resolution_event_ids.insert(&call.resolution_event_id)
            })
            || project
                .abandoned_subscription_calls
                .iter()
                .any(|call| call.allowance.grant.work_item_id == *review_id)
            || project.work_corrections.iter().any(|record| {
                record.previous.spec.work_item_id == *review_id
                    || correction_ids.contains(&record.correction_id)
            })
            || !entry.review_work.spec.dependency_ids.contains(source_id)
            || entry.review_work.output_receipts.len() != 1
            || entry.review_work.output_receipts[0].content_digest != entry.report_digest
            || entry
                .review_work
                .gate_receipt
                .as_ref()
                .is_none_or(|gate| !gate.passed)
            || entry.source_work.state != CompanyWorkStateV1::Done
            || entry.source_work.output_receipts.len() != 1
            || entry.source_allowance.grant.work_item_id != *source_id
            || entry.review_allowance.grant.work_item_id != *review_id
            || entry.source_allowance.dispatch.is_none()
            || entry.review_allowance.dispatch.is_none()
            || entry.source_allowance.allowance_id == entry.review_allowance.allowance_id
            || entry.archived_at_unix_ms < project.created_at_unix_ms
            || entry.archived_at_unix_ms > project.updated_at_unix_ms
            || !project.work_corrections.iter().any(|correction| {
                correction.previous == entry.source_work
                    && correction.previous_subscription_call.as_ref()
                        == Some(&entry.source_allowance)
                    && correction.feedback_ref == entry.report_digest
                    && correction.requested_at_unix_ms == entry.archived_at_unix_ms
            })
        {
            return Err(corrupt());
        }
        let blocker = project
            .blockers
            .iter()
            .find(|blocker| blocker.blocker_id == entry.blocker_id)
            .ok_or_else(corrupt)?;
        if blocker.state != BlockerStateV1::Resolved
            || blocker.work_item_id.as_ref() != Some(source_id)
            || blocker.cause_ref != format!("qa-source-review:{}", entry.report_digest)
            || blocker.resolution_ref.as_deref()
                != Some(format!("{REWORK_REASON}:{}", entry.report_digest).as_str())
        {
            return Err(corrupt());
        }
        validate_work_transition_history(&entry.review_work, entry.archived_at_unix_ms)?;
        validate_output_receipts(&entry.review_work.spec, &entry.review_work.output_receipts)?;
        let mut historical = project.clone();
        historical
            .work_items
            .insert(review_id.clone(), entry.review_work.clone());
        historical
            .work_corrections
            .extend(entry.review_corrections.iter().cloned());
        work_corrections::validate(&historical)?;
        // Validate this archived handoff against its own consumed authority,
        // never a later QA call or the live archive/regrant gap.
        historical
            .work_items
            .insert(source_id.clone(), entry.source_work.clone());
        historical.subscription_call = Some(entry.review_allowance.clone());
        historical.source_review_previous_call = Some(entry.source_allowance.clone());
        historical.abandoned_subscription_calls = entry.review_abandoned_calls.clone();
        subscription::validate(&historical)?;
        subscription::validate_allowance(project, &entry.source_allowance)?;
    }
    Ok(())
}

pub(super) fn authorizes_blocker_resolution(
    project: &ProjectV1,
    blocker: &BlockerV1,
    audit: &StateTransitionAuditV1,
    actor: &ParticipantBindingV1,
) -> bool {
    matches!(
        actor.role,
        CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
    ) && project.archived_source_reviews.iter().any(|entry| {
        entry.blocker_id == blocker.blocker_id
            && audit.reason_ref == format!("{REWORK_REASON}:{}", entry.report_digest)
            && entry.archived_at_unix_ms == audit.occurred_at_unix_ms
    })
}
