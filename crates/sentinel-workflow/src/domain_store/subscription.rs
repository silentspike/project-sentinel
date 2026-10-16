use super::*;
use std::collections::BTreeSet;

const MAX_GRANT_WINDOW_MS: u64 = 300_000;

fn subscription_role_supported(role: CompanyRoleV1, profile_id: &str, has_inputs: bool) -> bool {
    matches!(role, CompanyRoleV1::Developer | CompanyRoleV1::Designer)
        || (role == CompanyRoleV1::Qa && profile_id == "web-review-v1" && has_inputs)
}

fn validate_grant(
    grant: &SubscriptionCallGrantV1,
    created_at_ms: u64,
) -> Result<(), WorkflowError> {
    validate_grant_shape(grant, created_at_ms)?;
    if grant.max_duration_ms != 120_000 {
        return Err(invalid("invalid ordinary subscription call duration"));
    }
    Ok(())
}

fn validate_grant_shape(
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
        || !(1_000..=120_000).contains(&grant.max_duration_ms)
        || grant.expires_at_unix_ms <= created_at_ms
        || grant.expires_at_unix_ms - created_at_ms > MAX_GRANT_WINDOW_MS
        || created_at_ms == 0
        || (grant.max_duration_ms != 120_000
            && grant.max_duration_ms > grant.expires_at_unix_ms - created_at_ms)
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

fn expired_undispatched_grant_matches(
    project: &ProjectV1,
    principal: &AuthenticatedCompanyPrincipalV1,
    current: &SubscriptionCallAllowanceV1,
    next: &SubscriptionCallGrantV1,
    now_ms: u64,
) -> bool {
    let mut expected = current.grant.clone();
    expected.expires_at_unix_ms = next.expires_at_unix_ms;
    current.dispatch.is_none()
        && now_ms >= current.grant.expires_at_unix_ms
        && current.created_by == principal.principal_id
        && expected == *next
        && active_assignment_matches(project, &current.grant)
}

fn completed_dispatched_grant_can_advance(
    project: &ProjectV1,
    current: &SubscriptionCallAllowanceV1,
    next: &SubscriptionCallGrantV1,
) -> bool {
    current.dispatch.is_some()
        && current.grant.work_item_id != next.work_item_id
        && project.source_review_previous_call.is_none()
        && project
            .work_items
            .get(&current.grant.work_item_id)
            .is_some_and(|work| work.state == CompanyWorkStateV1::Done)
        && active_assignment_matches(project, next)
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
    grant_validated(project, principal, operation_id, grant, now_ms)
}

/// Adaptive completion has no legacy dispatch. Authenticate its durable producer
/// before releasing the old allowance; a failed successor grant changes nothing.
pub(super) fn grant_on_connection(
    connection: &Connection,
    project: &mut ProjectV1,
    principal: &AuthenticatedCompanyPrincipalV1,
    operation_id: Uuid,
    next: &SubscriptionCallGrantV1,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    let adaptive_previous = project.subscription_call.as_ref().filter(|previous| {
        previous.dispatch.is_none()
            && previous.grant.work_item_id != next.work_item_id
            && project.source_review_previous_call.is_none()
            && project.work_items.get(&previous.grant.work_item_id)
                .is_some_and(|work| work.state == CompanyWorkStateV1::Done)
    });
    let Some(previous) = adaptive_previous else {
        return grant(project, principal, operation_id, next, now_ms);
    };
    let source = project.work_items.get(&previous.grant.work_item_id).ok_or_else(transition)?;
    crate::store::adaptive::completion_proof::require_source_review_producer(
        connection, project, source, previous, now_ms,
    )?;
    let mut candidate = project.clone();
    candidate.subscription_call = None;
    grant(&mut candidate, principal, operation_id, next, now_ms)?;
    *project = candidate;
    Ok(())
}

pub(super) fn grant_governed_continuation(
    connection: &Connection,
    project: &mut ProjectV1,
    receipt: &crate::AdaptiveLeadershipReviewCallV1,
    allowance: &SubscriptionCallAllowanceV1,
) -> Result<(), WorkflowError> {
    adaptive_leadership_review::validate_governed_allowance_receipt(receipt, allowance)?;
    WorkflowStore::require_adaptive_budget_review_source(connection, receipt)?;
    let mut expected = receipt.context.source_project.clone();
    expected.subscription_call = None;
    if *project != expected || project.subscription_call.is_some() {
        return Err(unauthorized());
    }
    let principal = &receipt.grant.leadership_principal;
    require_role(
        principal,
        &[CompanyRoleV1::ProjectManager, CompanyRoleV1::TechnicalLead],
    )?;
    validate_grant_shape(&allowance.grant, allowance.created_at_unix_ms)?;
    grant_validated(
        project,
        principal,
        receipt.operation_id,
        &allowance.grant,
        allowance.created_at_unix_ms,
    )
}

fn grant_validated(
    project: &mut ProjectV1,
    principal: &AuthenticatedCompanyPrincipalV1,
    operation_id: Uuid,
    grant: &SubscriptionCallGrantV1,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    if project.lifecycle_state != ProjectLifecycleStateV1::Active
        || !active_assignment_matches(project, grant)
        || project
            .reservations
            .iter()
            .any(|reservation| reservation.work_item_id.as_ref() == Some(&grant.work_item_id))
    {
        return Err(invalid("subscription call authority unavailable"));
    }
    if let Some(current) = project.subscription_call.as_ref() {
        if !expired_undispatched_grant_matches(project, principal, current, grant, now_ms)
            && !completed_dispatched_grant_can_advance(project, current, grant)
        {
            return Err(invalid("subscription call authority unavailable"));
        }
        project.subscription_call = None;
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

pub(super) fn abandon(
    project: &mut ProjectV1,
    principal: &AuthenticatedCompanyPrincipalV1,
    allowance_id: &str,
    request_digest: &str,
    resolution_event_id: &str,
    abandoned_by: &str,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    require_role(
        principal,
        &[CompanyRoleV1::ProjectManager, CompanyRoleV1::TechnicalLead],
    )?;
    validate_identifier(allowance_id)?;
    validate_digest(request_digest)?;
    validate_identifier(abandoned_by)?;
    Uuid::parse_str(resolution_event_id)
        .map_err(|_| invalid("invalid provider resolution event"))?;
    if project.lifecycle_state != ProjectLifecycleStateV1::Active
        || project.abandoned_subscription_calls.len() >= MAX_AGGREGATE_ITEMS
    {
        return Err(unauthorized());
    }
    let current = project.subscription_call.as_ref().ok_or_else(not_found)?;
    let dispatch = current.dispatch.as_ref().ok_or_else(transition)?;
    if current.allowance_id != allowance_id
        || dispatch.request_digest != request_digest
        || now_ms < dispatch.dispatched_at_unix_ms
        || project
            .abandoned_subscription_calls
            .iter()
            .any(|entry| entry.resolution_event_id == resolution_event_id)
    {
        return Err(invalid("subscription call resolution changed"));
    }
    let abandoned = project.subscription_call.take().ok_or_else(not_found)?;
    project
        .abandoned_subscription_calls
        .push(AbandonedSubscriptionCallV1 {
            allowance: abandoned,
            resolution_event_id: resolution_event_id.to_owned(),
            abandoned_by: abandoned_by.to_owned(),
            abandoned_at_unix_ms: now_ms,
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
    validate_grant(&allowance.grant, allowance.created_at_unix_ms)?;
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
    let mut abandoned_allowance_ids = BTreeSet::new();
    let mut resolution_event_ids = BTreeSet::new();
    for entry in &project.abandoned_subscription_calls {
        validate_allowance(project, &entry.allowance)?;
        validate_identifier(&entry.abandoned_by).map_err(|_| corrupt())?;
        Uuid::parse_str(&entry.resolution_event_id).map_err(|_| corrupt())?;
        let dispatch = entry.allowance.dispatch.as_ref().ok_or_else(corrupt)?;
        if !abandoned_allowance_ids.insert(entry.allowance.allowance_id.as_str())
            || !resolution_event_ids.insert(entry.resolution_event_id.as_str())
            || entry.abandoned_at_unix_ms < dispatch.dispatched_at_unix_ms
            || entry.abandoned_at_unix_ms > project.updated_at_unix_ms
        {
            return Err(corrupt());
        }
    }
    if project
        .subscription_call
        .as_ref()
        .is_some_and(|current| abandoned_allowance_ids.contains(current.allowance_id.as_str()))
    {
        return Err(corrupt());
    }
    if let Some(previous) = &project.source_review_previous_call {
        validate_allowance(project, previous)?;
        let review = project
            .subscription_call
            .as_ref()
            .or_else(|| {
                project
                    .abandoned_subscription_calls
                    .last()
                    .map(|entry| &entry.allowance)
            })
            .ok_or_else(corrupt)?;
        if !review_handoff_matches(project, previous, &review.grant)
            || previous.allowance_id == review.allowance_id
            || previous.created_at_unix_ms > review.created_at_unix_ms
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
    source.state == CompanyWorkStateV1::Done
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
    connection: &Connection,
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
    if previous.dispatch.is_none() {
        let source = project
            .work_items
            .get(&previous.grant.work_item_id)
            .ok_or_else(transition)?;
        crate::store::adaptive::completion_proof::require_source_review_producer(
            connection, project, source, previous, now_ms,
        )?;
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
    validate_grant_shape(&allowance.grant, allowance.created_at_unix_ms).map_err(|_| corrupt())?;
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

fn project_subscription_memo_key(project: &ProjectV1) -> Result<String, WorkflowError> {
    canonical_sha256(
        "sentinel.workflow.project-subscription-memo-key.v1",
        project,
    )
}

pub(super) fn validate_persisted(
    connection: &Connection,
    project: &ProjectV1,
) -> Result<(), WorkflowError> {
    validation_scope::with_scope(connection, || {
        let key = project_subscription_memo_key(project)?;
        validation_scope::memoize(connection, "project-subscription", &key, || {
            validate_persisted_uncached(connection, project)
        })
    })
}

fn validate_persisted_uncached(
    connection: &Connection,
    project: &ProjectV1,
) -> Result<(), WorkflowError> {
    if let Some(previous) = project.source_review_previous_call.as_ref().filter(|call| call.dispatch.is_none()) {
        let source = project.work_items.get(&previous.grant.work_item_id).ok_or_else(corrupt)?;
        let review = project.subscription_call.as_ref().or_else(|| project.abandoned_subscription_calls.last().map(|entry| &entry.allowance)).ok_or_else(corrupt)?;
        crate::store::adaptive::completion_proof::require_source_review_producer(
            connection, project, source, previous, review.created_at_unix_ms.min(project.updated_at_unix_ms),
        )?;
    }
    let mut statement = connection.prepare(
        "SELECT operation_id FROM company_events WHERE tenant_id=?1 AND project_id=?2
         AND event_type='adaptive_leadership_continuation_authorized' AND created_at_ms<=?3
         ORDER BY sequence LIMIT 4097",
    )?;
    let operations = statement
        .query_map(
            params![
                project.tenant_id.0,
                project.project_id.0,
                sql_u64(project.updated_at_unix_ms)?
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    if operations.len() > 4096 {
        return Err(corrupt());
    }
    let issued = operations
        .iter()
        .map(|operation| {
            let operation = Uuid::parse_str(operation).map_err(|_| corrupt())?;
            stable_domain_id("subscription", &project.tenant_id, operation)
        })
        .collect::<Result<BTreeSet<_>, WorkflowError>>()?;
    let allowances = project
        .subscription_call
        .iter()
        .chain(project.source_review_previous_call.iter())
        .chain(
            project
                .abandoned_subscription_calls
                .iter()
                .map(|entry| &entry.allowance),
        )
        .chain(
            project
                .work_corrections
                .iter()
                .filter_map(|record| record.previous_subscription_call.as_ref()),
        )
        .filter_map(|allowance| {
            let governed = (|| {
                Ok::<_, WorkflowError>(
                    issued.contains(&allowance.allowance_id)
                        || crate::store::adaptive::allowance_is_governed_in_journal(
                            connection,
                            &project.tenant_id,
                            &project.project_id,
                            allowance,
                        )?
                        || allowance.grant.max_duration_ms != 120_000,
                )
            })();
            match governed {
                Ok(true) => Some(Ok(allowance)),
                Ok(false) => None,
                Err(error) => Some(Err(error)),
            }
        });
    adaptive_leadership_review::validate_governed_allowances(connection, project, allowances)
}

pub(super) fn renew_for_correction(
    connection: &Connection,
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
    if prior.dispatch.is_none() {
        crate::store::adaptive::completion_proof::require_source_review_producer(
            connection, project, &record.previous, prior, record.requested_at_unix_ms,
        )?;
        let mut candidate = project.clone();
        candidate.subscription_call = None;
        grant(&mut candidate, principal, operation_id, next, now_ms)?;
        *project = candidate;
        return Ok(());
    }
    project.subscription_call = None;
    grant(project, principal, operation_id, next, now_ms)
}

#[cfg(test)]
mod role_tests {
    use super::*;

    #[test]
    fn subscription_memo_key_binds_full_project_and_reuses_only_equal_values() {
        let (_temp, _path, store, _customer, project) =
            crate::domain_store::tests::accepted_project_fixture();
        let key = project_subscription_memo_key(&project).unwrap();
        assert_eq!(key.len(), 64);
        assert_eq!(
            key,
            canonical_sha256(
                "sentinel.workflow.project-subscription-memo-key.v1",
                &project
            )
            .unwrap()
        );
        assert_ne!(
            key,
            canonical_sha256("sentinel.workflow.project-snapshot-memo-key.v1", &project).unwrap()
        );
        assert!(
            serde_json::to_vec(&key).unwrap().len() < serde_json::to_vec(&project).unwrap().len()
        );
        let connection = store.connection.lock().unwrap();
        validation_scope::with_scope(&connection, || {
            validate_persisted(&connection, &project)?;
            validate_persisted(&connection, &project.clone())?;
            assert_eq!(validation_scope::validations("project-subscription"), 1);
            for (index, field) in [
                "governance",
                "cost",
                "clock",
                "tenant",
                "project",
                "version",
            ]
            .into_iter()
            .enumerate()
            {
                let mut changed = project.clone();
                match field {
                    "governance" => changed.governance.project_profile.digest = "b".repeat(64),
                    "cost" => changed.cost_ceiling_micros += 1,
                    "clock" => changed.updated_at_unix_ms += 1,
                    "tenant" => changed.tenant_id = TenantId::parse("tenant-b").unwrap(),
                    "project" => changed.project_id = ProjectId::parse("project-b").unwrap(),
                    "version" => changed.version += 1,
                    _ => unreachable!(),
                }
                assert_ne!(project_subscription_memo_key(&changed)?, key, "{field}");
                if matches!(field, "governance" | "cost") {
                    assert_eq!(changed.tenant_id, project.tenant_id);
                    assert_eq!(changed.project_id, project.project_id);
                    assert_eq!(changed.version, project.version);
                }
                validate_persisted(&connection, &changed)?;
                assert_eq!(
                    validation_scope::validations("project-subscription"),
                    index + 2,
                    "{field}"
                );
                validate_persisted(&connection, &changed)?;
                assert_eq!(
                    validation_scope::validations("project-subscription"),
                    index + 2,
                    "{field}"
                );
            }
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn subscription_changed_allowance_cannot_reuse_warm_unit_proof() {
        let (_temp, _path, store, _customer, project) =
            crate::domain_store::tests::accepted_project_fixture();
        assert!(project.subscription_call.is_none());
        let mut changed = project.clone();
        changed.subscription_call = Some(SubscriptionCallAllowanceV1 {
            allowance_id: "missing-governed-allowance".into(),
            grant: SubscriptionCallGrantV1 {
                schema_version: 1,
                work_item_id: WorkItemId::parse("missing-work").unwrap(),
                assignment_id: "missing-assignment".into(),
                assignment_version: 1,
                agent_id: crate::AgentId(2),
                provider: "codex-cli".into(),
                model: "model".into(),
                catalog_digest: "a".repeat(64),
                max_calls: 1,
                max_concurrent: 1,
                max_duration_ms: 1_000,
                token_policy: SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,
                expires_at_unix_ms: project.updated_at_unix_ms + 10_000,
            },
            created_by: "pm-a".into(),
            created_at_unix_ms: project.updated_at_unix_ms,
            dispatch: None,
        });
        assert_eq!(changed.tenant_id, project.tenant_id);
        assert_eq!(changed.project_id, project.project_id);
        assert_eq!(changed.version, project.version);
        assert_ne!(
            project_subscription_memo_key(&changed).unwrap(),
            project_subscription_memo_key(&project).unwrap()
        );
        let connection = store.connection.lock().unwrap();
        let error = validation_scope::with_scope(&connection, || {
            validate_persisted(&connection, &project)?;
            let result = validate_persisted(&connection, &changed);
            assert_eq!(validation_scope::validations("project-subscription"), 2);
            assert_eq!(validation_scope::validations("governed-allowance"), 1);
            result
        })
        .unwrap_err();
        assert_eq!(error.code, WorkflowErrorCode::CorruptStore);
        assert!(connection.is_autocommit());
    }

    #[test]
    fn subscription_empty_allowance_batch_does_not_discover_unrelated_receipts() {
        let (_temp, _path, store, _customer, project) =
            crate::domain_store::tests::accepted_project_fixture();
        assert!(project.subscription_call.is_none());
        let connection = store.connection.lock().unwrap();
        connection.execute(
            "INSERT INTO company_entities(tenant_id,entity_kind,entity_id,version,payload,payload_digest)
             VALUES(?1,'adaptive_leadership_review_call','unrelated-review',1,X'00','invalid')",
            [&project.tenant_id.0],
        ).unwrap();
        validation_scope::with_scope(&connection, || {
            validate_persisted(&connection, &project)?;
            assert_eq!(
                validation_scope::validations("governed-allowance-locators"),
                0
            );
            assert_eq!(validation_scope::validations("governed-allowance"), 0);
            Ok(())
        })
        .unwrap();
    }

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

#[cfg(test)]
mod adaptive_successor_tests {
    use super::*;
    use crate::{AgentId, WorkItemExecutionV1, SubscriptionTokenPolicyV1};
    use crate::adaptive::continuation_tests::NOW;
    use crate::store::adaptive::completion_proof::tests::Fixture;
    use crate::store::adaptive::completion_proof::tests::{complete_for_project, complete_negative_review, journal_rows, root_for_project};

    fn successor(f: &mut Fixture, role: CompanyRoleV1) -> (AuthenticatedCompanyPrincipalV1, SubscriptionCallGrantV1) {
        let mut next = f.work.clone();
        next.spec.work_item_id = WorkItemId::parse("next-work").unwrap();
        next.spec.required_role = role;
        next.state = CompanyWorkStateV1::Assigned;
        next.output_receipts.clear();
        next.gate_receipt = None;
        next.assignments[0].assignment_id = "next-assignment".into();
        next.assignments[0].role = role;
        let mut grant = f.allowance.grant.clone();
        grant.work_item_id = next.spec.work_item_id.clone();
        grant.assignment_id = next.assignments[0].assignment_id.clone();
        grant.expires_at_unix_ms = NOW + 200_000;
        f.project.work_items.insert(next.spec.work_item_id.clone(),next);
        let principal = AuthenticatedCompanyPrincipalV1 {
            schema_version:1,tenant_id:f.project.tenant_id.clone(),principal_id:"pm".into(),
            kind:CompanyPrincipalKindV1::Agent,role:CompanyRoleV1::ProjectManager,
            customer_id:None,agent_id:Some(AgentId(1)),authority_generation:1,authority_digest:"a".repeat(64),
        };
        principal.validate().unwrap();
        (principal,grant)
    }

    #[test]
    fn authenticated_adaptive_done_advances_to_developer_or_designer_without_sql_mutation() {
        for role in [CompanyRoleV1::Developer,CompanyRoleV1::Designer] {
            let mut f = Fixture::new();
            let (principal,next) = successor(&mut f,role);
            let before = f.project.clone();
            let connection = f.store.connection.lock().unwrap();
            let changes: i64 = connection.query_row("SELECT total_changes()",[],|row|row.get(0)).unwrap();
            grant_on_connection(&connection,&mut f.project,&principal,Uuid::new_v4(),&next,NOW+11).unwrap();
            assert_eq!(f.project.subscription_call.as_ref().unwrap().grant,next);
            assert_ne!(f.project.subscription_call.as_ref().unwrap().allowance_id,f.allowance.allowance_id);
            assert_eq!(f.project.work_items,before.work_items);
            assert_eq!(f.project.source_review_previous_call,before.source_review_previous_call);
            assert_eq!(f.project.cost_ceiling_micros,before.cost_ceiling_micros);
            assert_eq!(f.project.reserved_cost_micros,before.reserved_cost_micros);
            assert_eq!(f.project.committed_cost_micros,before.committed_cost_micros);
            assert_eq!(connection.query_row("SELECT total_changes()",[],|row|row.get::<_,i64>(0)).unwrap(),changes);
        }
    }

    #[test]
    fn missing_gate_corruption_bad_successor_and_premature_cutoff_preserve_old_allowance() {
        for negative in 0..5 {
            let mut f = Fixture::with_recorded_gate(negative != 0);
            let (principal,mut next) = successor(&mut f,CompanyRoleV1::Developer);
            if negative == 1 {
                let connection = f.store.connection.lock().unwrap();
                assert_eq!(connection.execute("UPDATE workflow_gate_outbox SET evidence=X'00'",[]).unwrap(),1);
            }
            if negative == 2 { next.assignment_id = "missing".into(); }
            if negative == 4 { next.max_calls = 0; }
            let before = f.project.clone();
            let connection = f.store.connection.lock().unwrap();
            let changes: i64 = connection.query_row("SELECT total_changes()",[],|row|row.get(0)).unwrap();
            let at = if negative == 3 {NOW+9} else {NOW+11};
            assert!(grant_on_connection(&connection,&mut f.project,&principal,Uuid::new_v4(),&next,at).is_err(),"negative={negative}");
            assert_eq!(f.project,before,"negative={negative}");
            assert_eq!(connection.query_row("SELECT total_changes()",[],|row|row.get::<_,i64>(0)).unwrap(),changes);
        }
    }

    struct CommandFixture {
        _temp: tempfile::TempDir,
        path: std::path::PathBuf,
        store: WorkflowStore,
        project: ProjectV1,
        pm: AuthenticatedCompanyPrincipalV1,
        producer: AuthenticatedCompanyPrincipalV1,
        qa: AuthenticatedCompanyPrincipalV1,
        source_id: WorkItemId,
    }

    fn actor(project: &ProjectV1, id: u16, role: CompanyRoleV1, principal: &str) -> AuthenticatedCompanyPrincipalV1 {
        AuthenticatedCompanyPrincipalV1 {
            schema_version:1,tenant_id:project.tenant_id.clone(),principal_id:principal.into(),
            kind:CompanyPrincipalKindV1::Agent,role,customer_id:None,agent_id:Some(AgentId(id)),
            authority_generation:1,authority_digest:"a".repeat(64),
        }
    }

    fn item(id: &str, role: CompanyRoleV1, owner: u16, specialty: &str, budget: u64) -> CompanyWorkItemSpecV1 {
        CompanyWorkItemSpecV1 {
            work_item_id:WorkItemId::parse(id).unwrap(),title:id.into(),objective:"Produce contracted artifact".into(),
            required_role:role,required_specialties:BTreeSet::from([specialty.into()]),dependency_ids:BTreeSet::new(),
            owner:AgentId(owner),inputs:vec![],outputs:vec![WorkOutputContractV1 {
                name:"source".into(),media_type:"application/json".into(),digest_algorithm:"sha256".into(),
                contract_generation:1,contract_digest:"a".repeat(64),
            }],quality_gate:QualityGateBindingV1 {gate_id:"web-work-item-qa-v1".into(),generation:1,digest:"a".repeat(64)},
            budget_micros:budget,rework:None,
        }
    }

    fn source_input(source: &CompanyWorkItemSpecV1) -> WorkInputContractV1 {
        WorkInputContractV1 {
            name:"source-input".into(),producer_work_item_id:source.work_item_id.clone(),
            producer_output_name:source.outputs[0].name.clone(),expected_contract_generation:source.outputs[0].contract_generation,
            expected_contract_digest:source.outputs[0].contract_digest.clone(),
        }
    }

    impl CommandFixture {
        fn new(role: CompanyRoleV1) -> Self {
            Self::with_release_reviewer(role,false)
        }

        fn with_release_reviewer(role: CompanyRoleV1, release_reviewer: bool) -> Self {
            let (temp,path,store,customer,mut project) = crate::domain_store::tests::accepted_project_fixture();
            if role == CompanyRoleV1::Designer || release_reviewer {
                // Reuse the existing accepted-proposal fixture boundary. Do not
                // rewrite an accepted project's governance or operation history.
                let mut request: CustomerRequestV1 = store.company_customer_request(&customer.tenant_id,"request-a").unwrap().unwrap();
                let mut proposal: ProposalV1 = {
                    let connection = store.connection.lock().unwrap();
                    get_entity(&connection,&customer.tenant_id,"proposal","proposal-a").unwrap().unwrap()
                };
                request.request_id = "adaptive-design-request".into();
                request.state = CustomerRequestStateV1::Proposed;
                request.version = 1;
                request.proposal_ids = vec!["adaptive-design-proposal".into()];
                request.created_at_unix_ms = 1;
                request.updated_at_unix_ms = 1;
                proposal.proposal_id = request.proposal_ids[0].clone();
                proposal.request_id = request.request_id.clone();
                if role == CompanyRoleV1::Designer {
                    proposal.binding.governance.participants.push(ParticipantBindingV1 {
                        agent_id:AgentId(4),principal_id:"designer-a".into(),role:CompanyRoleV1::Designer,
                        specialties:BTreeSet::from(["design".into()]),reports_to:Some(AgentId(1)),
                        profile:WorkProfileBindingV1 {profile_id:"designer-v1".into(),generation:1,digest:"a".repeat(64)},
                    });
                }
                if release_reviewer {
                    proposal.binding.governance.participants.push(ParticipantBindingV1 {
                        agent_id:AgentId(6),principal_id:"release-a".into(),role:CompanyRoleV1::ReleaseManager,
                        specialties:BTreeSet::from(["release".into()]),reports_to:Some(AgentId(1)),
                        profile:WorkProfileBindingV1 {profile_id:"release-v1".into(),generation:1,digest:"a".repeat(64)},
                    });
                }
                proposal.proposal_digest = canonical_sha256("sentinel.workflow.proposal-binding.v1",&proposal.binding).unwrap();
                {
                    let mut connection = store.connection.lock().unwrap();
                    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate).unwrap();
                    put_entity(&transaction,&customer.tenant_id,"request",&request.request_id,1,&request).unwrap();
                    put_entity(&transaction,&customer.tenant_id,"proposal",&proposal.proposal_id,1,&proposal).unwrap();
                    transaction.commit().unwrap();
                }
                let result = store.apply_company_command(&customer,Uuid::new_v4(),&CompanyWorkflowCommandV1::AcceptProposal {
                    request_id:request.request_id,expected_version:1,proposal_id:proposal.proposal_id,proposal_digest:proposal.proposal_digest,
                },2).unwrap();
                let CompanyWorkflowResponseV1::AgreementProject {project:accepted,..} = result.response else {panic!("expected accepted project")};
                project = *accepted;
            }
            let pm = actor(&project,1,CompanyRoleV1::ProjectManager,"pm-a");
            let qa = actor(&project,3,CompanyRoleV1::Qa,"qa-a");
            let producer = if role == CompanyRoleV1::Designer {actor(&project,4,role,"designer-a")} else {actor(&project,2,role,"developer-a")};
            let source = item("adaptive-source",role,producer.agent_id.unwrap().0,if role == CompanyRoleV1::Designer {"design"} else {"rust"},if role == CompanyRoleV1::Designer {40} else {100});
            let mut items = vec![source.clone()];
            if role == CompanyRoleV1::Designer {
                let mut next = item("next-developer",CompanyRoleV1::Developer,2,"rust",60);
                next.dependency_ids.insert(source.work_item_id.clone());
                next.inputs.push(source_input(&source));
                items.push(next);
            }
            let source_id = source.work_item_id;
            let mut fixture = Self {_temp:temp,path,store,project,pm:pm.clone(),producer,qa,source_id:source_id.clone()};
            fixture.apply(&pm,CompanyWorkflowCommandV1::PlanWorkGraph {project_id:fixture.project.project_id.clone(),expected_version:fixture.project.version,items},3);
            fixture.apply(&pm,CompanyWorkflowCommandV1::ActivateProject {project_id:fixture.project.project_id.clone(),expected_version:fixture.project.version,reason_ref:"accepted scope".into()},4);
            fixture.apply(&pm,CompanyWorkflowCommandV1::AssignWork {
                project_id:fixture.project.project_id.clone(),expected_version:fixture.project.version,work_item_id:source_id,
                agent_id:fixture.producer.agent_id.unwrap(),organization_generation:1,organization_digest:"a".repeat(64),reason_ref:"assigned".into(),
            },5);
            let grant = fixture.grant_for(&fixture.source_id);
            fixture.apply(&pm,CompanyWorkflowCommandV1::GrantSubscriptionCall {project_id:fixture.project.project_id.clone(),expected_version:fixture.project.version,grant},6);
            fixture
        }

        fn apply(&mut self, principal: &AuthenticatedCompanyPrincipalV1, command: CompanyWorkflowCommandV1, now: u64) {
            let result = self.store.apply_company_command(principal,Uuid::new_v4(),&command,now).unwrap();
            assert!(!result.replayed);
            let CompanyWorkflowResponseV1::Project(project) = result.response else {panic!("expected project")};
            self.project = *project;
        }

        fn grant_for(&self, id: &WorkItemId) -> SubscriptionCallGrantV1 {
            let assignment = self.project.work_items[id].assignments.iter().find(|assignment|assignment.active).unwrap();
            SubscriptionCallGrantV1 {
                schema_version:1,work_item_id:id.clone(),assignment_id:assignment.assignment_id.clone(),assignment_version:assignment.assignment_version,
                agent_id:assignment.agent_id,provider:"codex-cli".into(),model:"gpt-5.6-luna".into(),catalog_digest:"a".repeat(64),
                max_calls:4,max_concurrent:1,max_duration_ms:120_000,token_policy:SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,expires_at_unix_ms:300_000,
            }
        }

        fn finish_source(&mut self, execution: Option<&WorkItemExecutionV1>) {
            let id = self.source_id.clone();
            let producer = self.producer.clone();
            let reviewer = self.qa.clone();
            self.finish_work(&id,execution,&producer,&reviewer,31);
        }

        fn finish_work(&mut self, id: &WorkItemId, execution: Option<&WorkItemExecutionV1>, producer: &AuthenticatedCompanyPrincipalV1, reviewer: &AuthenticatedCompanyPrincipalV1, started_at: u64) {
            for (index,to) in [CompanyWorkStateV1::InProgress,CompanyWorkStateV1::InReview,CompanyWorkStateV1::Done].into_iter().enumerate() {
                let now = started_at + index as u64;
                let work = &self.project.work_items[id];
                let output = &work.spec.outputs[0];
                let receipt = WorkTransitionReceiptV1 {
                    schema_version:1,project_id:self.project.project_id.clone(),work_item_id:id.clone(),
                    expected_project_version:self.project.version,expected_work_version:work.version,expected_assignment_version:work.assignments[0].assignment_version,
                    from_state:work.state,to_state:to,output_receipts:if index == 0 {vec![]} else {vec![WorkOutputReceiptV1 {
                        name:output.name.clone(),contract_generation:output.contract_generation,contract_digest:output.contract_digest.clone(),
                        content_digest:execution.map_or_else(||"a".repeat(64),|execution|execution.terminal_execution_evidence.as_ref().unwrap().outputs[0].digest.clone()),
                    }]},gate_receipt:(index == 2).then(||QualityGateReceiptBindingV1 {
                        gate_id:work.spec.quality_gate.gate_id.clone(),generation:work.spec.quality_gate.generation,gate_digest:work.spec.quality_gate.digest.clone(),
                        subject_digest:execution.map_or_else(||"f".repeat(64),|execution|execution.gate_evidence.as_ref().unwrap().subject_digest.clone()),passed:true,
                    }),phase_a_evidence_digest:"a".repeat(64),reason_ref:"verified-transition".into(),occurred_at_unix_ms:now,
                };
                // None deliberately supplies only company metadata for the
                // bare-Done negative, never authenticated execution evidence.
                let principal = if index == 2 {reviewer} else {producer};
                self.apply(principal,CompanyWorkflowCommandV1::ApplyWorkTransition {project_id:self.project.project_id.clone(),expected_version:self.project.version,receipt},now);
            }
        }

        fn review_command(&mut self) -> CompanyWorkflowCommandV1 {
            let pm = self.pm.clone();
            let source = &self.project.work_items[&self.source_id].spec;
            let mut review = item("source-review",CompanyRoleV1::Qa,3,"qa",0);
            review.outputs[0].name = "qa-report".into();
            review.outputs[0].media_type = "application/vnd.sentinel.qa-report+json".into();
            review.dependency_ids.insert(self.source_id.clone());
            review.inputs.push(source_input(source));
            let review_id = review.work_item_id.clone();
            self.apply(&pm,CompanyWorkflowCommandV1::AppendSourceReview {project_id:self.project.project_id.clone(),expected_version:self.project.version,item:review},34);
            self.apply(&pm,CompanyWorkflowCommandV1::AssignSourceReview {
                project_id:self.project.project_id.clone(),expected_version:self.project.version,work_item_id:review_id.clone(),agent_id:AgentId(3),
                organization_generation:1,organization_digest:"a".repeat(64),reason_ref:"source-review-profile".into(),
                profile:WorkProfileBindingV1 {profile_id:"web-review-v1".into(),generation:1,digest:"a".repeat(64)},
            },35);
            let mut grant = self.grant_for(&review_id);
            grant.max_calls = 1;
            CompanyWorkflowCommandV1::GrantSourceReviewCall {
                project_id:self.project.project_id.clone(),expected_version:self.project.version,
                previous_allowance_id:self.project.subscription_call.as_ref().unwrap().allowance_id.clone(),grant,
            }
        }

        fn successor_command(&mut self) -> CompanyWorkflowCommandV1 {
            let pm = self.pm.clone();
            let next = WorkItemId::parse("next-developer").unwrap();
            assert_eq!(self.project.work_items[&next].state,CompanyWorkStateV1::Ready);
            self.apply(&pm,CompanyWorkflowCommandV1::AssignWork {
                project_id:self.project.project_id.clone(),expected_version:self.project.version,work_item_id:next.clone(),agent_id:AgentId(2),
                organization_generation:1,organization_digest:"a".repeat(64),reason_ref:"design ready".into(),
            },34);
            CompanyWorkflowCommandV1::GrantSubscriptionCall {project_id:self.project.project_id.clone(),expected_version:self.project.version,grant:self.grant_for(&next)}
        }
    }

    fn company_rows(store: &WorkflowStore) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
        let connection = store.connection.lock().unwrap();
        ["SELECT * FROM company_entities ORDER BY tenant_id,entity_kind,entity_id",
         "SELECT * FROM company_operations ORDER BY authority_namespace,operation_id",
         "SELECT * FROM company_events ORDER BY sequence",
         "SELECT * FROM company_project_projections ORDER BY tenant_id,project_id"].iter().map(|sql| {
            let mut statement = connection.prepare(sql).unwrap();
            let columns = statement.column_count();
            let rows = statement.query_map([],|row|(0..columns).map(|column|row.get::<_,rusqlite::types::Value>(column)).collect::<rusqlite::Result<Vec<_>>>()).unwrap();
            rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
        }).collect()
    }

    fn apply_replay_and_reopen(f: &mut CommandFixture, command: &CompanyWorkflowCommandV1, now: u64) {
        let operation = Uuid::new_v4();
        let result = f.store.apply_company_command(&f.pm,operation,command,now).unwrap();
        assert!(!result.replayed);
        let CompanyWorkflowResponseV1::Project(project) = result.response else {panic!("expected project")};
        f.project = *project;
        assert_eq!(f.store.company_project(&f.pm.tenant_id,&f.project.project_id).unwrap().as_ref(),Some(&f.project));
        let rows = company_rows(&f.store);
        f.store = WorkflowStore::open(&f.path).unwrap();
        assert_eq!(f.store.company_project(&f.pm.tenant_id,&f.project.project_id).unwrap().as_ref(),Some(&f.project));
        let replay = f.store.apply_company_command(&f.pm,operation,command,now+1).unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.response,CompanyWorkflowResponseV1::Project(Box::new(f.project.clone())));
        assert_eq!(company_rows(&f.store),rows);
    }

    #[test]
    fn domain_command_adaptive_completion_hands_off_to_qa_and_reopens_exact_producer() {
        let mut f = CommandFixture::new(CompanyRoleV1::Developer);
        let old = f.project.subscription_call.clone().unwrap();
        let (source,plan,done) = complete_for_project(&f.store,&f.project,&f.source_id,20);
        f.finish_source(Some(&done));
        let command = f.review_command();
        let before = f.project.clone();
        apply_replay_and_reopen(&mut f,&command,36);
        assert_eq!(f.project.source_review_previous_call.as_ref(),Some(&old));
        assert_eq!(f.project.work_items,before.work_items);
        assert_eq!(f.project.cost_ceiling_micros,before.cost_ceiling_micros);
        assert_ne!(f.project.subscription_call.as_ref().unwrap().allowance_id,old.allowance_id);
        assert!(f.project.subscription_call.as_ref().unwrap().dispatch.is_none());
        assert_eq!(f.store.adaptive_source_completion(&f.pm.tenant_id,&f.project.project_id,&old,plan.plan_id).unwrap(),Some(source));
    }

    #[test]
    fn domain_command_adaptive_designer_done_funds_next_developer_without_changing_source() {
        let mut f = CommandFixture::new(CompanyRoleV1::Designer);
        let old = f.project.subscription_call.clone().unwrap();
        let (source,plan,done) = complete_for_project(&f.store,&f.project,&f.source_id,20);
        f.finish_source(Some(&done));
        let command = f.successor_command();
        let before = f.project.clone();
        apply_replay_and_reopen(&mut f,&command,35);
        assert_eq!(f.project.work_items,before.work_items);
        assert!(f.project.source_review_previous_call.is_none());
        assert_eq!(f.project.subscription_call.as_ref().unwrap().grant.work_item_id,WorkItemId::parse("next-developer").unwrap());
        assert_ne!(f.project.subscription_call.as_ref().unwrap().allowance_id,old.allowance_id);
        assert_eq!(f.project.cost_ceiling_micros,before.cost_ceiling_micros);
        assert_eq!(f.store.adaptive_source_completion(&f.pm.tenant_id,&f.project.project_id,&old,plan.plan_id).unwrap(),Some(source));
    }

    #[test]
    fn domain_commands_reject_bare_done_and_missing_journal_without_company_mutation() {
        for role in [CompanyRoleV1::Developer,CompanyRoleV1::Designer] {
            for missing_journal in [false,true] {
                let mut f = CommandFixture::new(role);
                let execution = missing_journal.then(||complete_for_project(&f.store,&f.project,&f.source_id,20));
                f.finish_source(execution.as_ref().map(|(_,_,done)|done));
                let command = if role == CompanyRoleV1::Developer {f.review_command()} else {f.successor_command()};
                if let Some((source,plan,_)) = execution {
                    let old = f.project.subscription_call.as_ref().unwrap();
                    assert_eq!(f.store.adaptive_source_completion(&f.pm.tenant_id,&f.project.project_id,old,plan.plan_id).unwrap(),Some(source));
                    let connection = f.store.connection.lock().unwrap();
                    assert!(connection.execute("DELETE FROM workflow_operations WHERE operation_namespace GLOB 'adaptive-session-v1:*:operations'",[]).unwrap() > 0);
                }
                let before = f.project.clone();
                let rows = company_rows(&f.store);
                assert!(f.store.apply_company_command(&f.pm,Uuid::new_v4(),&command,36).is_err(),"role={role:?} missing_journal={missing_journal}");
                assert_eq!(company_rows(&f.store),rows);
                assert_eq!(f.store.company_project(&f.pm.tenant_id,&before.project_id).unwrap().as_ref(),Some(&before));
            }
        }
    }

    #[test]
    fn domain_negative_qa_archives_adaptive_source_and_reopens_exact_revised_producers() {
        let mut f = CommandFixture::with_release_reviewer(CompanyRoleV1::Developer,true);
        let pm = f.pm.clone();
        let qa = f.qa.clone();
        let producer = f.producer.clone();
        let release = actor(&f.project,6,CompanyRoleV1::ReleaseManager,"release-a");
        let old_allowance = f.project.subscription_call.clone().unwrap();
        let (old_source,old_plan,old_execution) = complete_for_project(&f.store,&f.project,&f.source_id,20);
        let old_journal = journal_rows(&f.store,old_source.provider_source.grant.session_id);
        f.finish_source(Some(&old_execution));
        let source_work = f.project.work_items[&f.source_id].clone();
        let handoff = f.review_command();
        apply_replay_and_reopen(&mut f,&handoff,36);
        let review_id = WorkItemId::parse("source-review").unwrap();
        let review_allowance = f.project.subscription_call.clone().unwrap();
        assert_eq!(review_allowance.grant.max_calls,1);
        f.apply(&qa,CompanyWorkflowCommandV1::ClaimSubscriptionCall {
            project_id:f.project.project_id.clone(),expected_version:f.project.version,
            allowance_id:review_allowance.allowance_id.clone(),request_id:format!("company-provider-{}",review_allowance.allowance_id),request_digest:"b".repeat(64),
        },37);
        let claimed_review = f.project.subscription_call.clone().unwrap();
        let review_execution = complete_negative_review(&f.store,&f.project,&review_id,38);
        f.finish_work(&review_id,Some(&review_execution),&qa,&release,42);
        let report_digest = f.project.work_items[&review_id].output_receipts[0].content_digest.clone();
        f.apply(&pm,CompanyWorkflowCommandV1::RaiseBlocker {
            project_id:f.project.project_id.clone(),expected_version:f.project.version,work_item_id:Some(f.source_id.clone()),
            cause_ref:format!("qa-source-review:{report_digest}"),owner:producer.agent_id.unwrap(),
        },45);
        let feedback = crate::WorkCorrectionFeedbackV1 {summary:"Contracted behavior missing in the sealed QA report".into(),artifact_digest:Some(old_source.artifact_digest.clone())};
        let revision = crate::ExecutionRevisionV1::from_completed_work(&old_execution,feedback.canonical_digest().unwrap()).unwrap();
        let mut next = old_allowance.grant.clone();
        next.max_calls = 16;
        let restart = CompanyWorkflowCommandV1::RestartSourceAfterQa {
            project_id:f.project.project_id.clone(),expected_version:f.project.version,review_work_item_id:review_id,
            source_work_item_id:f.source_id.clone(),blocker_id:f.project.blockers[0].blocker_id.clone(),report_digest,
            expected_source_work_version:source_work.version,source_execution_revision:revision,feedback,next_subscription_grant:next,
        };
        let before = f.project.clone();
        let rows = company_rows(&f.store);
        let mut invalid = restart.clone();
        let CompanyWorkflowCommandV1::RestartSourceAfterQa {next_subscription_grant,..} = &mut invalid else {unreachable!()};
        next_subscription_grant.max_calls = 0;
        let error = f.store.apply_company_command(&pm,Uuid::new_v4(),&invalid,46).unwrap_err();
        assert_eq!(error.message,"invalid subscription call grant");
        assert_eq!(company_rows(&f.store),rows);
        assert_eq!(f.store.company_project(&pm.tenant_id,&before.project_id).unwrap(),Some(before.clone()));
        apply_replay_and_reopen(&mut f,&restart,46);
        assert!(f.project.source_review_previous_call.is_none());
        assert_eq!(f.project.archived_source_reviews.len(),1);
        let archive = &f.project.archived_source_reviews[0];
        assert_eq!(archive.source_work,source_work);
        assert_eq!(archive.source_allowance,old_allowance);
        assert_eq!(archive.review_allowance,claimed_review);
        assert_eq!(archive.archived_at_unix_ms,46);
        assert_eq!(f.project.work_corrections[0].requested_at_unix_ms,46);
        assert_eq!(f.project.work_corrections[0].previous_subscription_call.as_ref(),Some(&old_allowance));
        assert_eq!(f.project.subscription_call.as_ref().unwrap().grant.max_calls,16);
        assert_ne!(f.project.subscription_call.as_ref().unwrap().allowance_id,old_allowance.allowance_id);
        assert_eq!(f.project.cost_ceiling_micros,before.cost_ceiling_micros);
        assert_eq!(f.project.reserved_cost_micros,before.reserved_cost_micros);
        assert_eq!(f.project.committed_cost_micros,before.committed_cost_micros);
        assert_eq!(journal_rows(&f.store,old_source.provider_source.grant.session_id),old_journal);
        let renewed = f.project.subscription_call.clone().unwrap();
        // Real root/revision admission is mandatory: never delete a head or
        // rewrite the prior CompletionProposed journal to make this pass.
        let (revised_source,revised_plan,revised_execution) = complete_for_project(&f.store,&f.project,&f.source_id,50);
        assert_eq!(revised_source.provider_source.grant.max_model_calls,16);
        assert_ne!(revised_source.provider_source.grant.session_id,old_source.provider_source.grant.session_id);
        assert_eq!(revised_source.artifact_digest,old_source.artifact_digest);
        let source_id = f.source_id.clone();
        f.finish_work(&source_id,Some(&revised_execution),&producer,&qa,61);
        let expected = f.project.clone();
        f.store = WorkflowStore::open(&f.path).unwrap();
        assert_eq!(f.store.company_project(&pm.tenant_id,&expected.project_id).unwrap(),Some(expected));
        assert_eq!(f.store.adaptive_source_completion(&pm.tenant_id,&f.project.project_id,&old_allowance,old_plan.plan_id).unwrap(),Some(old_source.clone()));
        assert_eq!(f.store.adaptive_source_completion(&pm.tenant_id,&f.project.project_id,&renewed,revised_plan.plan_id).unwrap(),Some(revised_source.clone()));
        assert_eq!(journal_rows(&f.store,old_source.provider_source.grant.session_id),old_journal);
        let events = f.store.company_project_events_since(&pm.tenant_id,0,100).unwrap();
        assert!(events.iter().any(|event|event.event_type == "project_source_review_rework_started" && event.project.archived_source_reviews.len() == 1));
        let revised_journal = journal_rows(&f.store,revised_source.provider_source.grant.session_id);
        let revised_session_id = revised_source.provider_source.grant.session_id;
        let feedback = crate::WorkCorrectionFeedbackV1 {summary:"Refine the revised contracted behavior".into(),artifact_digest:Some(revised_source.artifact_digest.clone())};
        let correction = CompanyWorkflowCommandV1::RequestWorkCorrection {
            project_id:f.project.project_id.clone(),expected_version:f.project.version,work_item_id:f.source_id.clone(),
            expected_work_version:f.project.work_items[&f.source_id].version,
            execution_revision:crate::ExecutionRevisionV1::from_completed_work(&revised_execution,feedback.canonical_digest().unwrap()).unwrap(),
            feedback_ref:"second-pm-correction".into(),feedback:Some(feedback),next_subscription_grant:Some(renewed.grant.clone()),
        };
        apply_replay_and_reopen(&mut f,&correction,70);
        let latest_allowance = f.project.subscription_call.clone().unwrap();
        let (latest_source,latest_plan,latest_execution) = complete_for_project(&f.store,&f.project,&f.source_id,80);
        f.finish_work(&source_id,Some(&latest_execution),&producer,&qa,91);
        f.store = WorkflowStore::open(&f.path).unwrap();
        assert_eq!(f.store.company_project(&pm.tenant_id,&f.project.project_id).unwrap(),Some(f.project.clone()));
        assert_eq!(f.store.adaptive_health_inventory().unwrap().1.len(),1);
        assert_eq!(f.store.adaptive_source_completion(&pm.tenant_id,&f.project.project_id,&old_allowance,old_plan.plan_id).unwrap(),Some(old_source.clone()));
        assert_eq!(f.store.adaptive_source_completion(&pm.tenant_id,&f.project.project_id,&renewed,revised_plan.plan_id).unwrap(),Some(revised_source));
        assert_eq!(f.store.adaptive_source_completion(&pm.tenant_id,&f.project.project_id,&latest_allowance,latest_plan.plan_id).unwrap(),Some(latest_source));
        assert_eq!(journal_rows(&f.store,old_source.provider_source.grant.session_id),old_journal);
        assert_eq!(journal_rows(&f.store,revised_session_id),revised_journal);
        assert!(!f.store.company_project_events_since(&pm.tenant_id,0,100).unwrap().is_empty());
        let connection = f.store.connection.lock().unwrap();
        assert_eq!(connection.execute("UPDATE workflow_operations SET request_digest='invalid' WHERE operation_namespace GLOB 'execution-history:*'",[]).unwrap(),2);
        drop(connection);
        // A healthy current revision cannot substitute for the archived producer.
        assert!(f.store.company_project(&pm.tenant_id,&f.project.project_id).is_err());
    }

    #[test]
    fn domain_adaptive_correction_missing_journal_cannot_release_allowance_or_append_history() {
        let mut f = CommandFixture::new(CompanyRoleV1::Developer);
        let (source,_,execution) = complete_for_project(&f.store,&f.project,&f.source_id,20);
        f.finish_source(Some(&execution));
        let feedback = crate::WorkCorrectionFeedbackV1 {summary:"Correct the contracted behavior".into(),artifact_digest:Some(source.artifact_digest)};
        let mut next = f.project.subscription_call.as_ref().unwrap().grant.clone();
        next.max_calls = 16;
        let command = CompanyWorkflowCommandV1::RequestWorkCorrection {
            project_id:f.project.project_id.clone(),expected_version:f.project.version,work_item_id:f.source_id.clone(),
            expected_work_version:f.project.work_items[&f.source_id].version,
            execution_revision:crate::ExecutionRevisionV1::from_completed_work(&execution,feedback.canonical_digest().unwrap()).unwrap(),
            feedback_ref:"pm-correction".into(),feedback:Some(feedback),next_subscription_grant:Some(next),
        };
        let connection = f.store.connection.lock().unwrap();
        assert!(connection.execute("DELETE FROM workflow_operations WHERE operation_namespace GLOB 'adaptive-session-v1:*:operations'",[]).unwrap() > 0);
        drop(connection);
        let before = f.project.clone();
        let rows = company_rows(&f.store);
        assert!(f.store.apply_company_command(&f.pm,Uuid::new_v4(),&command,40).is_err());
        assert_eq!(company_rows(&f.store),rows);
        assert_eq!(f.store.company_project(&f.pm.tenant_id,&before.project_id).unwrap(),Some(before));
    }

    #[test]
    fn corrective_root_requires_exact_new_pm_grant_and_preserves_terminal_journal() {
        let mut f = CommandFixture::new(CompanyRoleV1::Developer);
        let pm = f.pm.clone();
        let old_allowance = f.project.subscription_call.clone().unwrap();
        let (source,plan,execution) = complete_for_project(&f.store,&f.project,&f.source_id,20);
        assert_eq!(source.provider_source.grant.max_tool_calls,4);
        f.finish_source(Some(&execution));
        let original = journal_rows(&f.store,source.provider_source.grant.session_id);
        let feedback = crate::WorkCorrectionFeedbackV1 {summary:"Correct the contracted behavior".into(),artifact_digest:Some(source.artifact_digest.clone())};
        let mut next = old_allowance.grant.clone();
        next.max_calls = 16;
        let correction = CompanyWorkflowCommandV1::RequestWorkCorrection {
            project_id:f.project.project_id.clone(),expected_version:f.project.version,work_item_id:f.source_id.clone(),
            expected_work_version:f.project.work_items[&f.source_id].version,
            execution_revision:crate::ExecutionRevisionV1::from_completed_work(&execution,feedback.canonical_digest().unwrap()).unwrap(),
            feedback_ref:"pm-correction".into(),feedback:Some(feedback),next_subscription_grant:Some(next),
        };
        // Completion and a fresh caller UUID alone are not correction authority.
        let mut unsupported = root_for_project(&f.project,&f.source_id,40);
        unsupported.provider_allowance_id = "not-authorized".into();
        assert!(f.store.begin_adaptive_session(&unsupported,&source.provider_source.grant.authority,40).is_err());
        f.apply(&pm,correction,40);
        let valid = root_for_project(&f.project,&f.source_id,50);
        assert_eq!(valid.max_tool_calls,16);
        let rows = company_rows(&f.store);
        for mutation in 0..8 {
            let mut root = valid.clone();
            root.session_id = Uuid::new_v4();
            match mutation {
                0 => root.provider_allowance_id = old_allowance.allowance_id.clone(),
                1 => root.provider_authority_digest = "0".repeat(64),
                2 => root.max_model_calls += 1,
                3 => root.max_tool_calls = 64,
                4 => root.deadline_ms -= 1,
                5 => root.created_at_ms = 39,
                6 => root.authority.assignment_version += 1,
                _ => root.max_model_calls = 1,
            }
            assert!(f.store.begin_adaptive_session(&root,&valid.authority,50).is_err(),"mutation={mutation}");
            assert_eq!(journal_rows(&f.store,source.provider_source.grant.session_id),original);
            assert_eq!(company_rows(&f.store),rows);
            let head = f.store.adaptive_session_for_authority(&valid.authority).unwrap().unwrap();
            assert_eq!(head.grant.session_id,source.provider_source.grant.session_id);
        }
        let (replayed,fresh) = f.store.begin_adaptive_session(&valid,&valid.authority,50).unwrap();
        assert!(!replayed);
        assert_eq!(fresh.model_calls,0);
        assert_eq!(fresh.tool_calls,0);
        assert!(fresh.continuation.is_none());
        assert!(fresh.last_observation.is_none());
        assert_eq!(f.store.begin_adaptive_session(&valid,&valid.authority,51).unwrap(),(true,fresh));
        f.store = WorkflowStore::open(&f.path).unwrap();
        let (_,heads) = f.store.adaptive_health_inventory().unwrap();
        assert_eq!(heads.len(),1);
        assert_eq!(heads[0].grant,valid);
        assert_eq!(journal_rows(&f.store,source.provider_source.grant.session_id),original);
        assert_eq!(f.store.adaptive_source_completion(&pm.tenant_id,&f.project.project_id,&old_allowance,plan.plan_id).unwrap(),Some(source));
        let connection = f.store.connection.lock().unwrap();
        assert_eq!(connection.execute("DELETE FROM company_events WHERE tenant_id=?1 AND project_id=?2 AND event_type='project_work_correction_requested'",params![pm.tenant_id.0,f.project.project_id.0]).unwrap(),1);
        drop(connection);
        assert!(f.store.adaptive_sessions_for_health().is_err());
        assert!(f.store.adaptive_source_completion(&pm.tenant_id,&f.project.project_id,&old_allowance,plan.plan_id).is_err());
    }
}
