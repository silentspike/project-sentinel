use super::*;
use crate::{
    AdoptSalesProposalV1, AdoptSalesQuestionV1, ClaimRequestProviderCallV1, RequestProviderCallV1,
    RequestProviderDispatchV1, RequestProviderGrantV1, SalesProposalResponseV1,
};

const KIND: &str = "request_provider_call";
const MAX_TOTAL_CALLS: usize = 40;

#[cfg(test)]
mod tests;

fn grant_actor(principal: &AuthenticatedCompanyPrincipalV1) -> Result<(), WorkflowError> {
    principal.validate()?;
    if principal.kind != CompanyPrincipalKindV1::Operator {
        return Err(unauthorized());
    }
    require_role(
        principal,
        &[CompanyRoleV1::TechnicalLead, CompanyRoleV1::ProjectManager],
    )
}

fn validate_grant(grant: &RequestProviderGrantV1, now_ms: u64) -> Result<(), WorkflowError> {
    grant.sales_principal.validate()?;
    validate_identifier(&grant.request_id)?;
    validate_identifier(&grant.model)?;
    validate_digest(&grant.catalog_digest)?;
    if grant.schema_version != 1
        || grant.expected_version == 0
        || grant.sales_principal.kind != CompanyPrincipalKindV1::Agent
        || grant.sales_principal.role != CompanyRoleV1::Sales
        || grant.provider != "codex-cli"
        || !matches!(grant.total_call_limit, 1 | 10 | 40)
        || !(1..=2).contains(&grant.concurrent_call_limit)
        || grant.concurrent_call_limit > grant.total_call_limit
        || grant.max_duration_ms != 120_000
        || now_ms == 0
        || grant.expires_at_unix_ms <= now_ms
        || grant.expires_at_unix_ms - now_ms > 300_000
    {
        return Err(invalid("invalid request provider grant"));
    }
    Ok(())
}

fn sales_operation(
    call: &RequestProviderCallV1,
    domain: &'static str,
) -> Result<Uuid, WorkflowError> {
    let digest = canonical_sha256(domain, &call.allowance_id)?;
    Uuid::parse_str(&digest[..32]).map_err(|_| corrupt())
}

fn question_operation(call: &RequestProviderCallV1) -> Result<Uuid, WorkflowError> {
    sales_operation(call, "sentinel.workflow.sales-question-operation.v1")
}

fn qualification_operation(call: &RequestProviderCallV1) -> Result<Uuid, WorkflowError> {
    sales_operation(call, "sentinel.workflow.sales-qualification-operation.v1")
}

fn proposal_operation(call: &RequestProviderCallV1) -> Result<Uuid, WorkflowError> {
    sales_operation(call, "sentinel.workflow.sales-proposal-operation.v1")
}

impl CompanyEntity for RequestProviderCallV1 {
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        (
            &self.granted_by.tenant_id,
            KIND,
            &self.allowance_id,
            self.version,
        )
    }

    fn validate_entity(&self) -> Result<(), WorkflowError> {
        grant_actor(&self.granted_by)?;
        validate_grant(&self.grant, self.created_at_unix_ms)?;
        validate_customer_request(&self.source_request)?;
        if self.schema_version != 1
            || self.operation_id.is_nil()
            || self.allowance_id
                != stable_domain_id(
                    "subscription",
                    &self.granted_by.tenant_id,
                    self.operation_id,
                )?
            || self.granted_by.tenant_id != self.grant.sales_principal.tenant_id
            || self.source_request.tenant_id != self.granted_by.tenant_id
            || self.source_request.request_id != self.grant.request_id
            || self.source_request.version != self.grant.expected_version
            || !matches!(
                self.source_request.state,
                CustomerRequestStateV1::Submitted | CustomerRequestStateV1::Clarifying
            )
            || request_has_unanswered_question(&self.source_request)
            || self.source_request.consultation.len() >= MAX_AGGREGATE_ITEMS
            || self.created_at_unix_ms < self.source_request.updated_at_unix_ms
            || self.updated_at_unix_ms < self.created_at_unix_ms
        {
            return Err(corrupt());
        }
        let answered = self.question_response.is_some() || self.proposal_response.is_some();
        let expected_version = if answered || self.abandonment_event_id.is_some() {
            3
        } else if self.dispatch.is_some() {
            2
        } else {
            1
        };
        if self.version != expected_version
            || answered != self.model_response_digest.is_some()
            || (self.question_response.is_some() && self.proposal_response.is_some())
            || (self.abandonment_event_id.is_some() && answered)
        {
            return Err(corrupt());
        }
        if let Some(event_id) = &self.abandonment_event_id {
            if self.dispatch.is_none() || Uuid::parse_str(event_id).is_err() {
                return Err(corrupt());
            }
        }
        if let Some(dispatch) = &self.dispatch {
            validate_digest(&dispatch.request_digest)?;
            validate_digest(&dispatch.context_digest)?;
            if dispatch.request_id != format!("company-provider-{}", self.allowance_id)
                || dispatch.dispatched_at_unix_ms < self.created_at_unix_ms
                || dispatch.dispatched_at_unix_ms >= self.grant.expires_at_unix_ms
                || dispatch.dispatched_at_unix_ms > self.updated_at_unix_ms
            {
                return Err(corrupt());
            }
        } else if answered || self.updated_at_unix_ms != self.created_at_unix_ms {
            return Err(corrupt());
        }
        if let Some(response) = &self.question_response {
            validate_digest(self.model_response_digest.as_deref().ok_or_else(corrupt)?)?;
            validate_customer_request(response)?;
            let message = response.consultation.last().ok_or_else(corrupt)?;
            let mut expected = self.source_request.clone();
            expected.version = expected.version.checked_add(1).ok_or_else(corrupt)?;
            expected.updated_at_unix_ms = self.updated_at_unix_ms;
            expected.state = CustomerRequestStateV1::Clarifying;
            expected.consultation.push(CustomerConsultationMessageV1 {
                message_id: stable_domain_id(
                    "consultation",
                    &self.granted_by.tenant_id,
                    question_operation(self)?,
                )?,
                in_reply_to: None,
                content: message.content.clone(),
                role: CompanyRoleV1::Sales,
                recorded_by: self.grant.sales_principal.principal_id.clone(),
                recorded_at_unix_ms: self.updated_at_unix_ms,
            });
            if &expected != response {
                return Err(corrupt());
            }
        }
        if let Some(response) = &self.proposal_response {
            validate_digest(self.model_response_digest.as_deref().ok_or_else(corrupt)?)?;
            validate_customer_request(&response.request)?;
            response
                .proposal
                .binding
                .validate(self.updated_at_unix_ms)?;
            let mut expected = self.source_request.clone();
            expected.version = expected.version.checked_add(2).ok_or_else(corrupt)?;
            expected.updated_at_unix_ms = self.updated_at_unix_ms;
            expected.state = CustomerRequestStateV1::Proposed;
            expected
                .proposal_ids
                .push(response.proposal.proposal_id.clone());
            if response.request != expected
                || response.proposal.schema_version != COMPANY_DOMAIN_SCHEMA_VERSION
                || response.proposal.tenant_id != self.granted_by.tenant_id
                || response.proposal.request_id != self.source_request.request_id
                || response.proposal.generation
                    != u32::try_from(self.source_request.proposal_ids.len() + 1)
                        .map_err(|_| corrupt())?
                || response.proposal.created_by != self.grant.sales_principal.principal_id
                || response.proposal.created_at_unix_ms != self.updated_at_unix_ms
                || response.proposal.proposal_id
                    != stable_domain_id(
                        "proposal",
                        &self.granted_by.tenant_id,
                        proposal_operation(self)?,
                    )?
                || response.proposal.proposal_digest
                    != canonical_sha256(
                        "sentinel.workflow.proposal-binding.v1",
                        &response.proposal.binding,
                    )?
            {
                return Err(corrupt());
            }
        }
        Ok(())
    }
}

fn all_calls(connection: &Connection) -> Result<Vec<RequestProviderCallV1>, WorkflowError> {
    let mut statement = connection.prepare("SELECT tenant_id,entity_id FROM company_entities WHERE entity_kind=?1 ORDER BY tenant_id,entity_id LIMIT 41")?;
    let keys = statement
        .query_map([KIND], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if keys.len() > MAX_TOTAL_CALLS {
        return Err(corrupt());
    }
    keys.into_iter()
        .map(|(tenant, id)| {
            get_entity(connection, &TenantId::parse(&tenant)?, KIND, &id)?.ok_or_else(corrupt)
        })
        .collect()
}

// Legacy grants are never reset or inferred successful. They consume total
// allowance, and a dispatched legacy grant conservatively occupies a slot.
fn legacy_usage(connection: &Connection) -> Result<(usize, usize), WorkflowError> {
    // Inspect the validated aggregate, not a JSON filter that could hide a
    // corrupted allowance before its row digest is checked.
    let mut statement = connection.prepare("SELECT tenant_id,entity_id FROM company_entities WHERE entity_kind='project' ORDER BY tenant_id,entity_id LIMIT 4097")?;
    let keys = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if keys.len() > 4096 {
        return Err(corrupt());
    }
    let mut total = 0;
    let mut dispatched = 0;
    for (tenant, id) in &keys {
        let project: ProjectV1 = get_entity(connection, &TenantId::parse(tenant)?, "project", id)?
            .ok_or_else(corrupt)?;
        let mut allowances = BTreeMap::new();
        for allowance in project
            .subscription_call
            .iter()
            .chain(project.source_review_previous_call.iter())
            .chain(
                project
                    .work_corrections
                    .iter()
                    .filter_map(|record| record.previous_subscription_call.as_ref()),
            )
        {
            if let Some(previous) = allowances.insert(&allowance.allowance_id, allowance) {
                if previous != allowance {
                    return Err(corrupt());
                }
            }
        }
        for allowance in allowances.values() {
            total += 1;
            if allowance.dispatch.is_some() {
                dispatched += 1;
            }
        }
    }
    Ok((total, dispatched))
}

pub(super) fn ensure_legacy_grant_allowed(connection: &Connection) -> Result<(), WorkflowError> {
    let calls = all_calls(connection)?;
    if let Some(limit) = calls.iter().map(|call| call.grant.total_call_limit).max() {
        if calls.len() + legacy_usage(connection)?.0 >= usize::from(limit) {
            return Err(invalid("provider campaign allowance exhausted"));
        }
    }
    Ok(())
}

pub(super) fn ensure_legacy_dispatch_allowed(connection: &Connection) -> Result<(), WorkflowError> {
    if all_calls(connection)?.iter().any(|call| {
        call.dispatch.is_some()
            && call.question_response.is_none()
            && call.proposal_response.is_none()
            && call.abandonment_event_id.is_none()
    }) {
        return Err(invalid("request provider call occupies dispatch capacity"));
    }
    Ok(())
}

fn store_call(
    transaction: &Transaction<'_>,
    call: &RequestProviderCallV1,
    actor: &AuthenticatedCompanyPrincipalV1,
    event: &str,
) -> Result<(), WorkflowError> {
    call.validate_entity()?;
    put_entity(
        transaction,
        &call.granted_by.tenant_id,
        KIND,
        &call.allowance_id,
        call.version,
        call,
    )?;
    append_event(
        transaction,
        actor,
        call.operation_id,
        &canonical_sha256("sentinel.workflow.request-provider-call.v1", call)?,
        None,
        event,
        call,
        call.updated_at_unix_ms,
    )?;
    Ok(())
}

fn current_request_matches(
    connection: &Connection,
    call: &RequestProviderCallV1,
) -> Result<(), WorkflowError> {
    let current: CustomerRequestV1 = get_entity(
        connection,
        &call.granted_by.tenant_id,
        "request",
        &call.grant.request_id,
    )?
    .ok_or_else(not_found)?;
    if current != call.source_request {
        return Err(WorkflowError::new(
            WorkflowErrorCode::VersionConflict,
            false,
            "Sales request changed",
        ));
    }
    Ok(())
}

fn owned_call(
    connection: &Connection,
    principal: &AuthenticatedCompanyPrincipalV1,
    allowance_id: &str,
) -> Result<RequestProviderCallV1, WorkflowError> {
    principal.validate()?;
    validate_identifier(allowance_id)?;
    let call: RequestProviderCallV1 =
        get_entity(connection, &principal.tenant_id, KIND, allowance_id)?.ok_or_else(not_found)?;
    if principal != &call.grant.sales_principal {
        return Err(unauthorized());
    }
    Ok(call)
}

impl WorkflowStore {
    /// Trusted daemon admission only; never expose grant creation to an agent.
    /// Existing grants, including expired/unknown ones, permanently count.
    pub fn authorize_request_provider_call(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        operation_id: Uuid,
        grant: &RequestProviderGrantV1,
        now_ms: u64,
    ) -> Result<RequestProviderCallV1, WorkflowError> {
        grant_actor(principal)?;
        if operation_id.is_nil() {
            return Err(invalid("provider grant operation is missing"));
        }
        let allowance_id = stable_domain_id("subscription", &principal.tenant_id, operation_id)?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = get_entity::<RequestProviderCallV1>(
            &transaction,
            &principal.tenant_id,
            KIND,
            &allowance_id,
        )? {
            if existing.granted_by != *principal || existing.grant != *grant {
                return Err(WorkflowError::new(
                    WorkflowErrorCode::IdempotencyConflict,
                    false,
                    "provider grant operation changed",
                ));
            }
            return Ok(existing);
        }
        validate_grant(grant, now_ms)?;
        if principal.tenant_id != grant.sales_principal.tenant_id {
            return Err(unauthorized());
        }
        let source_request: CustomerRequestV1 = get_entity(
            &transaction,
            &principal.tenant_id,
            "request",
            &grant.request_id,
        )?
        .ok_or_else(not_found)?;
        require_version(source_request.version, grant.expected_version)?;
        let calls = all_calls(&transaction)?;
        let (legacy_total, _) = legacy_usage(&transaction)?;
        if calls.len() + legacy_total >= usize::from(grant.total_call_limit)
            || calls.iter().any(|call| {
                call.granted_by.tenant_id == principal.tenant_id
                    && call.grant.request_id == grant.request_id
                    && call.grant.expected_version == grant.expected_version
                    && call.abandonment_event_id.is_none()
                    && (call.dispatch.is_some() || now_ms < call.grant.expires_at_unix_ms)
            })
        {
            return Err(invalid(
                "request provider allowance exhausted or already reserved",
            ));
        }
        let call = RequestProviderCallV1 {
            schema_version: 1,
            allowance_id,
            operation_id,
            granted_by: principal.clone(),
            grant: grant.clone(),
            source_request,
            version: 1,
            created_at_unix_ms: now_ms,
            updated_at_unix_ms: now_ms,
            dispatch: None,
            question_response: None,
            proposal_response: None,
            model_response_digest: None,
            abandonment_event_id: None,
        };
        store_call(
            &transaction,
            &call,
            principal,
            "request_provider_call_authorized",
        )?;
        transaction.commit()?;
        Ok(call)
    }

    pub fn request_provider_call(
        &self,
        tenant: &TenantId,
        allowance_id: &str,
    ) -> Result<Option<RequestProviderCallV1>, WorkflowError> {
        tenant.validate()?;
        validate_identifier(allowance_id)?;
        let connection = self.connection.lock().map_err(|_| persistence())?;
        get_entity(&connection, tenant, KIND, allowance_id)
    }

    /// Not replayable permission: a second claim must never reach a provider.
    pub fn claim_request_provider_call(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        claim: &ClaimRequestProviderCallV1,
        now_ms: u64,
    ) -> Result<RequestProviderCallV1, WorkflowError> {
        validate_digest(&claim.request_digest)?;
        validate_digest(&claim.context_digest)?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut call = owned_call(&transaction, principal, &claim.allowance_id)?;
        if call.dispatch.is_some()
            || now_ms < call.created_at_unix_ms
            || now_ms >= call.grant.expires_at_unix_ms
            || claim.request_id != format!("company-provider-{}", call.allowance_id)
        {
            return Err(unauthorized());
        }
        current_request_matches(&transaction, &call)?;
        let active = all_calls(&transaction)?
            .iter()
            .filter(|call| {
                call.dispatch.is_some()
                    && call.question_response.is_none()
                    && call.proposal_response.is_none()
                    && call.abandonment_event_id.is_none()
            })
            .count()
            + legacy_usage(&transaction)?.1;
        if active >= usize::from(call.grant.concurrent_call_limit) {
            return Err(invalid("request provider concurrency exhausted"));
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
            principal,
            "request_provider_call_dispatched",
        )?;
        transaction.commit()?;
        Ok(call)
    }

    /// Record the daemon-verified immutable operator-abandonment event. The
    /// dispatch and cumulative authorization remain permanently consumed.
    pub fn abandon_request_provider_call(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        allowance_id: &str,
        request_digest: &str,
        resolution_event_id: &str,
        now_ms: u64,
    ) -> Result<RequestProviderCallV1, WorkflowError> {
        grant_actor(principal)?;
        validate_digest(request_digest)?;
        Uuid::parse_str(resolution_event_id).map_err(|_| invalid("invalid resolution event"))?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut call: RequestProviderCallV1 =
            get_entity(&transaction, &principal.tenant_id, KIND, allowance_id)?
                .ok_or_else(not_found)?;
        if call.granted_by != *principal
            || call.question_response.is_some()
            || call.proposal_response.is_some()
            || call
                .dispatch
                .as_ref()
                .is_none_or(|dispatch| dispatch.request_digest != request_digest)
        {
            return Err(unauthorized());
        }
        if let Some(existing) = &call.abandonment_event_id {
            if existing != resolution_event_id {
                return Err(invalid("resolution event changed"));
            }
            return Ok(call);
        }
        if now_ms < call.updated_at_unix_ms {
            return Err(invalid("resolution clock moved backwards"));
        }
        call.abandonment_event_id = Some(resolution_event_id.to_owned());
        call.version = 3;
        call.updated_at_unix_ms = now_ms;
        store_call(
            &transaction,
            &call,
            principal,
            "request_provider_call_abandoned",
        )?;
        transaction.commit()?;
        Ok(call)
    }

    /// The daemon must first verify its durable Limbo completion and live owner
    /// fence. Question adoption and the permanent result receipt commit together.
    pub fn adopt_sales_question(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        adoption: &AdoptSalesQuestionV1,
        now_ms: u64,
    ) -> Result<CustomerRequestV1, WorkflowError> {
        self.adopt_sales_question_inner(principal, adoption, now_ms, false)
    }

    fn adopt_sales_question_inner(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        adoption: &AdoptSalesQuestionV1,
        now_ms: u64,
        fail_after_question: bool,
    ) -> Result<CustomerRequestV1, WorkflowError> {
        validate_digest(&adoption.request_digest)?;
        validate_digest(&adoption.model_response_digest)?;
        validate_text(&adoption.content)?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut call = owned_call(&transaction, principal, &adoption.allowance_id)?;
        let dispatch = call.dispatch.as_ref().ok_or_else(unauthorized)?;
        if dispatch.request_digest != adoption.request_digest || call.abandonment_event_id.is_some()
        {
            return Err(unauthorized());
        }
        if let Some(response) = &call.question_response {
            if call.model_response_digest.as_ref() != Some(&adoption.model_response_digest)
                || response
                    .consultation
                    .last()
                    .is_none_or(|message| message.content != adoption.content)
            {
                return Err(WorkflowError::new(
                    WorkflowErrorCode::IdempotencyConflict,
                    false,
                    "Sales completion changed",
                ));
            }
            return Ok(response.clone());
        }
        if now_ms < call.updated_at_unix_ms {
            return Err(invalid("Sales completion time regressed"));
        }
        current_request_matches(&transaction, &call)?;
        let command = CompanyWorkflowCommandV1::SendCustomerRequestMessage {
            request_id: call.grant.request_id.clone(),
            expected_version: call.grant.expected_version,
            in_reply_to: None,
            content: adoption.content.clone(),
        };
        let response = apply_company_command(
            &transaction,
            principal,
            question_operation(&call)?,
            &command.canonical_digest()?,
            &command,
            now_ms,
            false,
        )?;
        let CompanyWorkflowResponseV1::CustomerRequest(response) = response else {
            return Err(corrupt());
        };
        if fail_after_question {
            return Err(persistence());
        }
        call.question_response = Some(response.clone());
        call.model_response_digest = Some(adoption.model_response_digest.clone());
        call.version = 3;
        call.updated_at_unix_ms = now_ms;
        store_call(
            &transaction,
            &call,
            principal,
            "request_provider_question_adopted",
        )?;
        transaction.commit()?;
        Ok(response)
    }

    /// The model supplies business terms, while the daemon supplies the
    /// immutable governance and budget binding. Qualification, proposal and
    /// the permanent provider result receipt commit in one SQLite transaction.
    pub fn adopt_sales_proposal(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        adoption: &AdoptSalesProposalV1,
        now_ms: u64,
    ) -> Result<SalesProposalResponseV1, WorkflowError> {
        self.adopt_sales_proposal_inner(principal, adoption, now_ms, false)
    }

    fn adopt_sales_proposal_inner(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        adoption: &AdoptSalesProposalV1,
        now_ms: u64,
        fail_after_proposal: bool,
    ) -> Result<SalesProposalResponseV1, WorkflowError> {
        validate_digest(&adoption.request_digest)?;
        validate_digest(&adoption.model_response_digest)?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut call = owned_call(&transaction, principal, &adoption.allowance_id)?;
        let dispatch = call.dispatch.as_ref().ok_or_else(unauthorized)?;
        if dispatch.request_digest != adoption.request_digest || call.abandonment_event_id.is_some()
        {
            return Err(unauthorized());
        }
        if let Some(response) = &call.proposal_response {
            if call.model_response_digest.as_ref() != Some(&adoption.model_response_digest)
                || response.proposal.binding != adoption.binding
            {
                return Err(WorkflowError::new(
                    WorkflowErrorCode::IdempotencyConflict,
                    false,
                    "Sales completion changed",
                ));
            }
            return Ok(response.clone());
        }
        if call.question_response.is_some() || now_ms < call.updated_at_unix_ms {
            return Err(invalid("Sales completion is invalid"));
        }
        adoption.binding.validate(now_ms)?;
        current_request_matches(&transaction, &call)?;
        let qualification = CompanyWorkflowCommandV1::QualifyCustomerRequest {
            request_id: call.grant.request_id.clone(),
            expected_version: call.grant.expected_version,
            reason_ref: "model-qualified-customer-request".to_owned(),
        };
        let qualification_response = apply_company_command(
            &transaction,
            principal,
            qualification_operation(&call)?,
            &qualification.canonical_digest()?,
            &qualification,
            now_ms,
            false,
        )?;
        let CompanyWorkflowResponseV1::CustomerRequest(qualified) = qualification_response else {
            return Err(corrupt());
        };
        let proposal_command = CompanyWorkflowCommandV1::CreateProposal {
            request_id: call.grant.request_id.clone(),
            expected_version: qualified.version,
            binding: adoption.binding.clone(),
        };
        let proposal_response = apply_company_command(
            &transaction,
            principal,
            proposal_operation(&call)?,
            &proposal_command.canonical_digest()?,
            &proposal_command,
            now_ms,
            false,
        )?;
        let CompanyWorkflowResponseV1::Proposal(proposal) = proposal_response else {
            return Err(corrupt());
        };
        let request = required_request(&transaction, principal, &call.grant.request_id, now_ms)?;
        let response = SalesProposalResponseV1 { request, proposal };
        if fail_after_proposal {
            return Err(persistence());
        }
        call.proposal_response = Some(response.clone());
        call.model_response_digest = Some(adoption.model_response_digest.clone());
        call.version = 3;
        call.updated_at_unix_ms = now_ms;
        store_call(
            &transaction,
            &call,
            principal,
            "request_provider_proposal_adopted",
        )?;
        transaction.commit()?;
        Ok(response)
    }
}
