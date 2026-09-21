use super::*;

const MAX_GRANT_WINDOW_MS: u64 = 300_000;

fn subscription_role_supported(role: CompanyRoleV1, profile_id: &str, has_inputs: bool) -> bool {
    matches!(role, CompanyRoleV1::Developer | CompanyRoleV1::Designer)
        || (role == CompanyRoleV1::Qa && profile_id == "web-review-v1" && has_inputs)
}

fn validate_grant(
    grant: &SubscriptionCallGrantV1,
    created_at_ms: u64,
) -> Result<(), WorkflowError> {
    grant.work_item_id.validate()?;
    validate_identifier(&grant.assignment_id)?;
    validate_identifier(&grant.model)?;
    validate_digest(&grant.catalog_digest)?;
    if grant.schema_version != 1
        || grant.agent_id.0 == 0
        || grant.assignment_version == 0
        || grant.provider != "codex-cli"
        || !(1..=crate::ADAPTIVE_SESSION_MAX_CALLS).contains(&grant.max_calls)
        || grant.max_concurrent != 1
        || grant.max_duration_ms != 120_000
        || grant.expires_at_unix_ms <= created_at_ms
        || grant.expires_at_unix_ms - created_at_ms > MAX_GRANT_WINDOW_MS
        || created_at_ms == 0
    {
        return Err(invalid("invalid subscription call grant"));
    }
    Ok(())
}

fn active_assignment_matches(project: &ProjectV1, grant: &SubscriptionCallGrantV1) -> bool {
    project
        .work_items
        .get(&grant.work_item_id)
        .is_some_and(|work| {
            matches!(
                work.state,
                CompanyWorkStateV1::Assigned | CompanyWorkStateV1::InProgress
            ) && work
                .assignments
                .iter()
                .filter(|assignment| assignment.active)
                .count()
                == 1
                && work.assignments.iter().any(|assignment| {
                    assignment.active
                        && assignment.assignment_id == grant.assignment_id
                        && assignment.assignment_version == grant.assignment_version
                        && assignment.agent_id == grant.agent_id
                        && subscription_role_supported(
                            work.spec.required_role,
                            &assignment.profile.profile_id,
                            !work.spec.inputs.is_empty(),
                        )
                })
        })
}

pub(super) fn grant(
    project: &mut ProjectV1,
    principal: &AuthenticatedCompanyPrincipalV1,
    operation_id: Uuid,
    grant: &SubscriptionCallGrantV1,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    require_role(
        principal,
        &[CompanyRoleV1::ProjectManager, CompanyRoleV1::TechnicalLead],
    )?;
    validate_grant(grant, now_ms)?;
    if project.lifecycle_state != ProjectLifecycleStateV1::Active
        || project.subscription_call.is_some()
        || !active_assignment_matches(project, grant)
        || project
            .reservations
            .iter()
            .any(|reservation| reservation.work_item_id.as_ref() == Some(&grant.work_item_id))
    {
        return Err(invalid("subscription call authority unavailable"));
    }
    project.subscription_call = Some(SubscriptionCallAllowanceV1 {
        allowance_id: stable_domain_id("subscription", &principal.tenant_id, operation_id)?,
        grant: grant.clone(),
        created_by: principal.principal_id.clone(),
        created_at_unix_ms: now_ms,
        dispatch: None,
    });
    Ok(())
}

pub(super) fn claim(
    project: &mut ProjectV1,
    principal: &AuthenticatedCompanyPrincipalV1,
    allowance_id: &str,
    request_id: &str,
    request_digest: &str,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    validate_identifier(allowance_id)?;
    validate_identifier(request_id)?;
    validate_digest(request_digest)?;
    let allowance = project.subscription_call.as_ref().ok_or_else(not_found)?;
    if project.lifecycle_state != ProjectLifecycleStateV1::Active
        || principal.kind != CompanyPrincipalKindV1::Agent
        || principal.agent_id != Some(allowance.grant.agent_id)
        || allowance.allowance_id != allowance_id
        || request_id != format!("company-provider-{allowance_id}")
        || allowance.dispatch.is_some()
        || project
            .source_review_previous_call
            .as_ref()
            .is_some_and(|previous| previous.allowance_id == allowance.allowance_id)
        || project.work_corrections.iter().any(|record| {
            record
                .previous_subscription_call
                .as_ref()
                .is_some_and(|previous| previous.allowance_id == allowance.allowance_id)
        })
        || now_ms < allowance.created_at_unix_ms
        || now_ms >= allowance.grant.expires_at_unix_ms
        || !active_assignment_matches(project, &allowance.grant)
    {
        return Err(unauthorized());
    }
    project
        .subscription_call
        .as_mut()
        .ok_or_else(not_found)?
        .dispatch = Some(SubscriptionCallDispatchV1 {
        request_id: request_id.to_owned(),
        request_digest: request_digest.to_owned(),
        dispatched_at_unix_ms: now_ms,
    });
    Ok(())
}

pub(super) fn validate(project: &ProjectV1) -> Result<(), WorkflowError> {
    if let Some(previous) = &project.source_review_previous_call {
        validate_allowance(project, previous)?;
        let current = project.subscription_call.as_ref().ok_or_else(corrupt)?;
        if !review_handoff_matches(project, previous, &current.grant)
            || previous.allowance_id == current.allowance_id
            || previous.created_at_unix_ms > current.created_at_unix_ms
        {
            return Err(corrupt());
        }
    }
    let Some(allowance) = &project.subscription_call else {
        return Ok(());
    };
    validate_allowance(project, allowance)
}

fn review_handoff_matches(
    project: &ProjectV1,
    previous: &SubscriptionCallAllowanceV1,
    next: &SubscriptionCallGrantV1,
) -> bool {
    let Some(source) = project.work_items.get(&previous.grant.work_item_id) else {
        return false;
    };
    let Some(review) = project.work_items.get(&next.work_item_id) else {
        return false;
    };
    previous.dispatch.is_some()
        && source.state == CompanyWorkStateV1::Done
        && matches!(
            source.spec.required_role,
            CompanyRoleV1::Developer | CompanyRoleV1::Designer
        )
        && previous.grant.agent_id != next.agent_id
        && review
            .spec
            .inputs
            .iter()
            .any(|input| input.producer_work_item_id == source.spec.work_item_id)
        && review.assignments.iter().any(|assignment| {
            assignment.assignment_id == next.assignment_id
                && assignment.assignment_version == next.assignment_version
                && assignment.agent_id == next.agent_id
                && source_review_profile_allowed(
                    review,
                    &assignment.profile,
                    &assignment.reason_ref,
                )
        })
}

pub(super) fn handoff_to_review(
    project: &mut ProjectV1,
    principal: &AuthenticatedCompanyPrincipalV1,
    operation_id: Uuid,
    previous_allowance_id: &str,
    next: &SubscriptionCallGrantV1,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    let previous = project.subscription_call.as_ref().ok_or_else(transition)?;
    if project.source_review_previous_call.is_some()
        || previous.allowance_id != previous_allowance_id
        || !review_handoff_matches(project, previous, next)
        || stable_domain_id("subscription", &principal.tenant_id, operation_id)?
            == previous.allowance_id
    {
        return Err(invalid("source-review subscription handoff unavailable"));
    }
    // Work on a candidate so even a direct failed call cannot clear authority.
    let mut candidate = project.clone();
    candidate.source_review_previous_call = candidate.subscription_call.take();
    grant(&mut candidate, principal, operation_id, next, now_ms)?;
    *project = candidate;
    Ok(())
}

pub(super) fn validate_allowance(
    project: &ProjectV1,
    allowance: &SubscriptionCallAllowanceV1,
) -> Result<(), WorkflowError> {
    validate_identifier(&allowance.allowance_id).map_err(|_| corrupt())?;
    validate_grant(&allowance.grant, allowance.created_at_unix_ms).map_err(|_| corrupt())?;
    let creator = project
        .governance
        .participants
        .iter()
        .find(|participant| participant.principal_id == allowance.created_by)
        .ok_or_else(corrupt)?;
    let work = project
        .work_items
        .get(&allowance.grant.work_item_id)
        .ok_or_else(corrupt)?;
    if !matches!(
        creator.role,
        CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
    ) || allowance.created_at_unix_ms < project.created_at_unix_ms
        || allowance.created_at_unix_ms > project.updated_at_unix_ms
        || !work.assignments.iter().any(|assignment| {
            assignment.assignment_id == allowance.grant.assignment_id
                && assignment.assignment_version == allowance.grant.assignment_version
                && assignment.agent_id == allowance.grant.agent_id
                && subscription_role_supported(
                    work.spec.required_role,
                    &assignment.profile.profile_id,
                    !work.spec.inputs.is_empty(),
                )
        })
        || project.reservations.iter().any(|reservation| {
            reservation.work_item_id.as_ref() == Some(&allowance.grant.work_item_id)
        })
    {
        return Err(corrupt());
    }
    if let Some(dispatch) = &allowance.dispatch {
        validate_digest(&dispatch.request_digest).map_err(|_| corrupt())?;
        if dispatch.request_id != format!("company-provider-{}", allowance.allowance_id)
            || dispatch.dispatched_at_unix_ms < allowance.created_at_unix_ms
            || dispatch.dispatched_at_unix_ms >= allowance.grant.expires_at_unix_ms
            || dispatch.dispatched_at_unix_ms > project.updated_at_unix_ms
        {
            return Err(corrupt());
        }
    }
    Ok(())
}

pub(super) fn renew_for_correction(
    project: &mut ProjectV1,
    principal: &AuthenticatedCompanyPrincipalV1,
    operation_id: Uuid,
    next: &SubscriptionCallGrantV1,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    let record = project.work_corrections.last().ok_or_else(transition)?;
    let prior = record
        .previous_subscription_call
        .as_ref()
        .ok_or_else(transition)?;
    if project.subscription_call.as_ref() != Some(prior)
        || prior.dispatch.is_none()
        || record.requested_at_unix_ms != now_ms
        || record.requested_by != principal.principal_id
        || record.previous.spec.work_item_id != next.work_item_id
        || prior.grant.work_item_id != next.work_item_id
        || prior.grant.agent_id != next.agent_id
        || prior.grant.assignment_id != next.assignment_id
        || prior.grant.assignment_version != next.assignment_version
        || stable_domain_id("subscription", &principal.tenant_id, operation_id)?
            == prior.allowance_id
    {
        return Err(invalid(
            "correction subscription source changed or was not consumed",
        ));
    }
    project.subscription_call = None;
    grant(project, principal, operation_id, next, now_ms)
}

#[cfg(test)]
mod role_tests {
    use super::*;
    #[test]
    fn subscription_qa_requires_the_review_profile_and_bound_inputs() {
        assert!(subscription_role_supported(
            CompanyRoleV1::Qa,
            "web-review-v1",
            true
        ));
        assert!(!subscription_role_supported(
            CompanyRoleV1::Qa,
            "web-authoring-v1",
            true
        ));
        assert!(!subscription_role_supported(
            CompanyRoleV1::Qa,
            "web-review-v1",
            false
        ));
        assert!(!subscription_role_supported(
            CompanyRoleV1::Customer,
            "web-review-v1",
            true
        ));
        assert!(subscription_role_supported(
            CompanyRoleV1::Developer,
            "web-authoring-v1",
            false
        ));
        assert!(subscription_role_supported(
            CompanyRoleV1::Designer,
            "web-authoring-v1",
            false
        ));
    }
}
