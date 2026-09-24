use super::*;
use crate::{
    ClaimProjectPlanningCallV1, ProjectPlanningCallV1, ProjectPlanningGrantV1,
    RequestProviderDispatchV1,
};

const KIND: &str = "project_planning_call";

#[cfg(test)]
mod tests;

fn store_call(
    transaction: &Transaction<'_>,
    call: &ProjectPlanningCallV1,
    planner: &AuthenticatedCompanyPrincipalV1,
    event: &str,
) -> Result<(), WorkflowError> {
    call.validate_entity()?;
    put_entity(
        transaction,
        &call.grant.planner_principal.tenant_id,
        KIND,
        &call.grant.project_id.0,
        call.version,
        call,
    )?;
    append_event(
        transaction,
        planner,
        call.operation_id,
        &canonical_sha256("sentinel.workflow.project-planning-call.v1", call)?,
        Some(&call.grant.project_id),
        event,
        call,
        call.updated_at_unix_ms,
    )?;
    Ok(())
}

fn validate_grant(grant: &ProjectPlanningGrantV1, now_ms: u64) -> Result<(), WorkflowError> {
    grant.project_id.validate()?;
    grant.planner_principal.validate()?;
    validate_identifier(&grant.model)?;
    validate_digest(&grant.catalog_digest)?;
    if grant.schema_version != 1
        || grant.expected_version == 0
        || grant.planner_principal.kind != CompanyPrincipalKindV1::Agent
        || grant.planner_principal.role != CompanyRoleV1::ProjectManager
        || grant.planner_principal.agent_id.is_none()
        || grant.provider != "codex-cli"
        || grant.max_duration_ms != 120_000
        || now_ms == 0
        || grant.expires_at_unix_ms <= now_ms
        || grant.expires_at_unix_ms - now_ms > 300_000
    {
        return Err(invalid("invalid project planning grant"));
    }
    Ok(())
}

impl CompanyEntity for ProjectPlanningCallV1 {
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        (
            &self.grant.planner_principal.tenant_id,
            KIND,
            &self.grant.project_id.0,
            self.version,
        )
    }

    fn validate_entity(&self) -> Result<(), WorkflowError> {
        validate_identifier(&self.allowance_id)?;
        validate_grant(&self.grant, self.grant_issued_at_unix_ms)?;
        validate_project(&self.source_project)?;
        let Some(planner_agent_id) = self.grant.planner_principal.agent_id else {
            return Err(corrupt());
        };
        if self.schema_version != 1
            || self.operation_id.is_nil()
            || self.source_project.tenant_id != self.grant.planner_principal.tenant_id
            || self.source_project.project_id != self.grant.project_id
            || self.source_project.version != self.grant.expected_version
            || self.source_project.lifecycle_state != ProjectLifecycleStateV1::Planning
            || !self.source_project.work_items.is_empty()
            || !self
                .source_project
                .governance
                .participants
                .iter()
                .any(|participant| {
                    participant.agent_id == planner_agent_id
                        && participant.principal_id == self.grant.planner_principal.principal_id
                        && participant.role == CompanyRoleV1::ProjectManager
                })
            || self.grant_issued_at_unix_ms < self.created_at_unix_ms
            || self.grant_issued_at_unix_ms > self.updated_at_unix_ms
            || self.updated_at_unix_ms < self.created_at_unix_ms
        {
            return Err(corrupt());
        }
        let complete = self.planned_project.is_some();
        let expected_version = if complete {
            3
        } else if self.dispatch.is_some() {
            2
        } else {
            1
        };
        if self.version != expected_version || complete != self.model_response_digest.is_some() {
            return Err(corrupt());
        }
        if let Some(dispatch) = &self.dispatch {
            validate_digest(&dispatch.request_digest)?;
            validate_digest(&dispatch.context_digest)?;
            if dispatch.request_id != self.request_id()
                || dispatch.dispatched_at_unix_ms < self.created_at_unix_ms
                || dispatch.dispatched_at_unix_ms >= self.grant.expires_at_unix_ms
                || dispatch.dispatched_at_unix_ms > self.updated_at_unix_ms
            {
                return Err(corrupt());
            }
        } else if complete || self.updated_at_unix_ms != self.grant_issued_at_unix_ms {
            return Err(corrupt());
        }
        if let Some(project) = &self.planned_project {
            validate_digest(self.model_response_digest.as_deref().ok_or_else(corrupt)?)?;
            validate_project(project)?;
            if project.tenant_id != self.source_project.tenant_id
                || project.project_id != self.source_project.project_id
                || project.agreement_id != self.source_project.agreement_id
                || project.agreement_digest != self.source_project.agreement_digest
                || project.lifecycle_state != ProjectLifecycleStateV1::Active
                || project.work_items.is_empty()
                || project.version <= self.source_project.version
            {
                return Err(corrupt());
            }
        }
        Ok(())
    }
}

impl ProjectPlanningCallV1 {
    pub fn request_id(&self) -> String {
        format!(
            "company-planning-{}-{}",
            self.allowance_id, self.grant.project_id.0
        )
    }
}

impl WorkflowStore {
    pub fn authorize_project_planning_call(
        &self,
        planner: &AuthenticatedCompanyPrincipalV1,
        operation_id: Uuid,
        allowance_id: &str,
        grant: &ProjectPlanningGrantV1,
        now_ms: u64,
    ) -> Result<ProjectPlanningCallV1, WorkflowError> {
        planner.validate()?;
        validate_identifier(allowance_id)?;
        validate_grant(grant, now_ms)?;
        if operation_id.is_nil() || planner != &grant.planner_principal {
            return Err(unauthorized());
        }
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(mut existing) = get_entity::<ProjectPlanningCallV1>(
            &transaction,
            &planner.tenant_id,
            KIND,
            &grant.project_id.0,
        )? {
            let mut prior_grant = existing.grant.clone();
            prior_grant.expires_at_unix_ms = grant.expires_at_unix_ms;
            if existing.operation_id != operation_id || prior_grant != *grant {
                return Err(WorkflowError::new(
                    WorkflowErrorCode::IdempotencyConflict,
                    false,
                    "project planning grant changed",
                ));
            }
            if existing.grant == *grant
                || existing.dispatch.is_some()
                || existing.planned_project.is_some()
                || now_ms < existing.grant.expires_at_unix_ms
            {
                return Ok(existing);
            }
            let current: ProjectV1 = get_entity(
                &transaction,
                &planner.tenant_id,
                "project",
                &grant.project_id.0,
            )?
            .ok_or_else(not_found)?;
            if current != existing.source_project {
                return Err(transition());
            }
            existing.grant = grant.clone();
            existing.grant_issued_at_unix_ms = now_ms;
            existing.updated_at_unix_ms = now_ms;
            store_call(
                &transaction,
                &existing,
                planner,
                "project_planning_call_renewed",
            )?;
            transaction.commit()?;
            return Ok(existing);
        }
        let source_project: ProjectV1 = get_entity(
            &transaction,
            &planner.tenant_id,
            "project",
            &grant.project_id.0,
        )?
        .ok_or_else(not_found)?;
        require_version(source_project.version, grant.expected_version)?;
        let call = ProjectPlanningCallV1 {
            schema_version: 1,
            allowance_id: allowance_id.to_owned(),
            operation_id,
            grant: grant.clone(),
            source_project,
            version: 1,
            created_at_unix_ms: now_ms,
            grant_issued_at_unix_ms: now_ms,
            updated_at_unix_ms: now_ms,
            dispatch: None,
            planned_project: None,
            model_response_digest: None,
        };
        store_call(
            &transaction,
            &call,
            planner,
            "project_planning_call_authorized",
        )?;
        transaction.commit()?;
        Ok(call)
    }

    pub fn project_planning_call(
        &self,
        tenant: &TenantId,
        project_id: &ProjectId,
    ) -> Result<Option<ProjectPlanningCallV1>, WorkflowError> {
        tenant.validate()?;
        project_id.validate()?;
        let connection = self.connection.lock().map_err(|_| persistence())?;
        get_entity(&connection, tenant, KIND, &project_id.0)
    }

    pub fn claim_project_planning_call(
        &self,
        planner: &AuthenticatedCompanyPrincipalV1,
        claim: &ClaimProjectPlanningCallV1,
        now_ms: u64,
    ) -> Result<ProjectPlanningCallV1, WorkflowError> {
        validate_digest(&claim.request_digest)?;
        validate_digest(&claim.context_digest)?;
        claim.project_id.validate()?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut call: ProjectPlanningCallV1 =
            get_entity(&transaction, &planner.tenant_id, KIND, &claim.project_id.0)?
                .ok_or_else(not_found)?;
        let current: ProjectV1 = get_entity(
            &transaction,
            &planner.tenant_id,
            "project",
            &call.grant.project_id.0,
        )?
        .ok_or_else(not_found)?;
        if planner != &call.grant.planner_principal
            || current != call.source_project
            || call.dispatch.is_some()
            || claim.project_id != call.grant.project_id
            || claim.allowance_id != call.allowance_id
            || claim.request_id != call.request_id()
            || now_ms < call.created_at_unix_ms
            || now_ms >= call.grant.expires_at_unix_ms
        {
            return Err(unauthorized());
        }
        call.dispatch = Some(RequestProviderDispatchV1 {
            request_id: claim.request_id.clone(),
            request_digest: claim.request_digest.clone(),
            context_digest: claim.context_digest.clone(),
            dispatched_at_unix_ms: now_ms,
        });
        call.version = 2;
        call.updated_at_unix_ms = now_ms;
        store_call(
            &transaction,
            &call,
            planner,
            "project_planning_call_dispatched",
        )?;
        transaction.commit()?;
        Ok(call)
    }

    pub fn complete_project_planning_call(
        &self,
        planner: &AuthenticatedCompanyPrincipalV1,
        project_id: &ProjectId,
        allowance_id: &str,
        request_digest: &str,
        model_response_digest: &str,
        expected_project: &ProjectV1,
        now_ms: u64,
    ) -> Result<ProjectPlanningCallV1, WorkflowError> {
        validate_digest(request_digest)?;
        validate_digest(model_response_digest)?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut call: ProjectPlanningCallV1 =
            get_entity(&transaction, &planner.tenant_id, KIND, &project_id.0)?
                .ok_or_else(not_found)?;
        if planner != &call.grant.planner_principal
            || project_id != &call.grant.project_id
            || allowance_id != call.allowance_id
            || call
                .dispatch
                .as_ref()
                .is_none_or(|dispatch| dispatch.request_digest != request_digest)
        {
            return Err(unauthorized());
        }
        let current: ProjectV1 = get_entity(
            &transaction,
            &planner.tenant_id,
            "project",
            &call.grant.project_id.0,
        )?
        .ok_or_else(not_found)?;
        if call.planned_project.is_some() {
            if call.model_response_digest.as_deref() == Some(model_response_digest)
                && call.planned_project.as_ref() == Some(expected_project)
            {
                return Ok(call);
            }
            return Err(WorkflowError::new(
                WorkflowErrorCode::IdempotencyConflict,
                false,
                "project planning result changed",
            ));
        }
        if &current != expected_project
            || current.lifecycle_state != ProjectLifecycleStateV1::Active
            || current.work_items.is_empty()
            || current.version <= call.source_project.version
            || now_ms < call.updated_at_unix_ms
        {
            return Err(transition());
        }
        call.planned_project = Some(current);
        call.model_response_digest = Some(model_response_digest.to_owned());
        call.version = 3;
        call.updated_at_unix_ms = now_ms;
        store_call(
            &transaction,
            &call,
            planner,
            "project_planning_call_completed",
        )?;
        transaction.commit()?;
        Ok(call)
    }
}
