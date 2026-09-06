use super::*;

const MAX_GRANT_WINDOW_MS: u64 = 300_000;

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
        || grant.max_calls != 1
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
            ) && matches!(
                work.spec.required_role,
                CompanyRoleV1::Developer | CompanyRoleV1::Designer
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
    let Some(allowance) = &project.subscription_call else {
        return Ok(());
    };
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
        || !matches!(
            work.spec.required_role,
            CompanyRoleV1::Developer | CompanyRoleV1::Designer
        )
        || !work.assignments.iter().any(|assignment| {
            assignment.assignment_id == allowance.grant.assignment_id
                && assignment.assignment_version == allowance.grant.assignment_version
                && assignment.agent_id == allowance.grant.agent_id
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
