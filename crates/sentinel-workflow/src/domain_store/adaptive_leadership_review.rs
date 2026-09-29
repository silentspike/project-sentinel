use super::*;
use crate::{
    AdaptiveCursorV1, AdaptiveLeadershipReviewCallV1, AdaptiveLeadershipReviewContextV1,
    AdaptiveLeadershipReviewGrantV1, ClaimAdaptiveLeadershipReviewCallV1,
    CompleteAdaptiveLeadershipReviewCallV1, RequestProviderDispatchV1,
    ADAPTIVE_LEADERSHIP_MAX_REVIEWS,
};

const KIND: &str = "adaptive_leadership_review_call";
const MAX_TENANT_REVIEW_SCAN: usize = 4096;

#[cfg(test)]
mod tests;

fn store_call(
    transaction: &Transaction<'_>,
    call: &AdaptiveLeadershipReviewCallV1,
    event: &str,
) -> Result<(), WorkflowError> {
    call.validate_entity()?;
    put_entity(
        transaction,
        &call.grant.leadership_principal.tenant_id,
        KIND,
        &call.grant.review_id.to_string(),
        call.version,
        call,
    )?;
    append_event(
        transaction,
        &call.grant.leadership_principal,
        call.operation_id,
        &canonical_sha256("sentinel.workflow.adaptive-leadership-call.v1", call)?,
        Some(&call.grant.project_id),
        event,
        call,
        call.updated_at_unix_ms,
    )?;
    Ok(())
}

fn calls_for_session(
    connection: &Connection,
    tenant: &TenantId,
    session_id: Uuid,
) -> Result<Vec<AdaptiveLeadershipReviewCallV1>, WorkflowError> {
    tenant.validate()?;
    if session_id.is_nil() {
        return Err(invalid("invalid adaptive session identity"));
    }
    let mut statement = connection.prepare(
        "SELECT entity_id FROM company_entities WHERE tenant_id=?1 AND entity_kind=?2
         ORDER BY entity_id LIMIT ?3",
    )?;
    let ids = statement
        .query_map(
            params![tenant.0, KIND, (MAX_TENANT_REVIEW_SCAN + 1) as i64],
            |row| row.get::<_, String>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    if ids.len() > MAX_TENANT_REVIEW_SCAN {
        return Err(corrupt());
    }
    let mut matching = Vec::new();
    // Verify every candidate before using payload fields to select the session.
    for id in ids {
        let call: AdaptiveLeadershipReviewCallV1 =
            get_entity(connection, tenant, KIND, &id)?.ok_or_else(corrupt)?;
        if call.grant.session_id == session_id {
            matching.push(call);
        }
    }
    if matching.len() > MAX_AGGREGATE_ITEMS {
        return Err(corrupt());
    }
    Ok(matching)
}

fn require_current_source(
    connection: &Connection,
    call: &AdaptiveLeadershipReviewCallV1,
) -> Result<(), WorkflowError> {
    let project: ProjectV1 = get_entity(
        connection,
        &call.grant.leadership_principal.tenant_id,
        "project",
        &call.grant.project_id.0,
    )?
    .ok_or_else(not_found)?;
    let (session, _) =
        crate::store::adaptive::load(connection, call.grant.session_id)?.ok_or_else(not_found)?;
    crate::store::adaptive::require_head(connection, &session)?;
    if project != call.context.source_project || session != call.context.source_session {
        return Err(transition());
    }
    let planning: crate::ProjectPlanningCallV1 = get_entity(
        connection,
        &call.grant.leadership_principal.tenant_id,
        "project_planning_call",
        &call.grant.project_id.0,
    )?
    .ok_or_else(not_found)?;
    let planned = planning.planned_project.as_ref().ok_or_else(transition)?;
    if planning.model_response_digest.is_none()
        || planned.agreement_id != project.agreement_id
        || planned.agreement_digest != project.agreement_digest
        || planning.grant.provider != call.grant.provider
        || planning.grant.model != call.grant.model
        || planning.grant.catalog_digest != call.grant.catalog_digest
        || planning.grant.token_policy != call.grant.token_policy
        || call.grant.max_duration_ms > planning.grant.max_duration_ms
    {
        return Err(unauthorized());
    }
    Ok(())
}

impl CompanyEntity for AdaptiveLeadershipReviewCallV1 {
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        (
            &self.grant.leadership_principal.tenant_id,
            KIND,
            &self.review_key,
            self.version,
        )
    }

    fn validate_entity(&self) -> Result<(), WorkflowError> {
        self.grant.validate(self.grant_issued_at_unix_ms)?;
        self.context.validate(&self.grant)?;
        validate_project(&self.context.source_project)?;
        validate_identifier(&self.allowance_id)?;
        if self.schema_version != 1
            || self.review_key != self.grant.review_id.to_string()
            || self.operation_id.is_nil()
            || self.allowance_id == self.grant.review_id.to_string()
            || self.allowance_id == self.context.source_session.grant.provider_allowance_id
            || self.created_at_unix_ms == 0
            || self.grant_issued_at_unix_ms < self.created_at_unix_ms
            || self.updated_at_unix_ms < self.grant_issued_at_unix_ms
        {
            return Err(corrupt());
        }
        let expected = if self.decision.is_some() {
            3
        } else if self.dispatch.is_some() {
            2
        } else {
            1
        };
        if self.version != expected
            || self.decision.is_some() != self.model_response_digest.is_some()
        {
            return Err(corrupt());
        }
        if let Some(dispatch) = &self.dispatch {
            validate_digest(&dispatch.request_digest)?;
            validate_digest(&dispatch.context_digest)?;
            if dispatch.request_id != self.request_id()
                || dispatch.context_digest != self.context_digest()?
                || dispatch.dispatched_at_unix_ms < self.grant_issued_at_unix_ms
                || dispatch.dispatched_at_unix_ms >= self.grant.expires_at_unix_ms
                || dispatch.dispatched_at_unix_ms > self.updated_at_unix_ms
            {
                return Err(corrupt());
            }
        }
        if let Some(decision) = &self.decision {
            decision.validate(&self.context.evidence_refs)?;
            validate_digest(self.model_response_digest.as_deref().ok_or_else(corrupt)?)?;
            if self.dispatch.is_none()
                || decision.resolves_blocked() != self.resolution_event_id.is_some()
                || self.resolution_event_id.is_some_and(|id| id.is_nil())
            {
                return Err(corrupt());
            }
        } else if self.resolution_event_id.is_some() {
            return Err(corrupt());
        }
        Ok(())
    }
}

impl WorkflowStore {
    pub fn authorize_adaptive_leadership_review_call(
        &self,
        leader: &AuthenticatedCompanyPrincipalV1,
        operation_id: Uuid,
        allowance_id: &str,
        grant: &AdaptiveLeadershipReviewGrantV1,
        context: &AdaptiveLeadershipReviewContextV1,
        now_ms: u64,
    ) -> Result<AdaptiveLeadershipReviewCallV1, WorkflowError> {
        leader.validate()?;
        validate_identifier(allowance_id)?;
        if leader != &grant.leadership_principal || operation_id.is_nil() {
            return Err(unauthorized());
        }
        let mut normalized_context = context.clone();
        normalized_context.evidence_refs.sort();
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(mut prior) = get_entity::<AdaptiveLeadershipReviewCallV1>(
            &transaction,
            &leader.tenant_id,
            KIND,
            &grant.review_id.to_string(),
        )? {
            let mut renewed_grant = prior.grant.clone();
            renewed_grant.expires_at_unix_ms = grant.expires_at_unix_ms;
            if prior.operation_id != operation_id
                || prior.allowance_id != allowance_id
                || prior.context != normalized_context
                || renewed_grant != *grant
            {
                return Err(WorkflowError::new(
                    WorkflowErrorCode::IdempotencyConflict,
                    false,
                    "adaptive leadership grant changed",
                ));
            }
            if prior.grant == *grant {
                return Ok(prior);
            }
            if prior.dispatch.is_some()
                || prior.decision.is_some()
                || now_ms < prior.grant.expires_at_unix_ms
            {
                return Err(transition());
            }
            grant.validate(now_ms)?;
            require_current_source(&transaction, &prior)?;
            prior.grant = grant.clone();
            prior.grant_issued_at_unix_ms = now_ms;
            prior.updated_at_unix_ms = now_ms;
            store_call(&transaction, &prior, "adaptive_leadership_review_renewed")?;
            transaction.commit()?;
            return Ok(prior);
        }
        grant.validate(now_ms)?;
        context.validate(grant)?;
        let existing = calls_for_session(&transaction, &leader.tenant_id, grant.session_id)?;
        let same_head: Vec<_> = existing
            .iter()
            .filter(|call| call.grant.expected_session_version == grant.expected_session_version)
            .collect();
        if same_head.len() >= ADAPTIVE_LEADERSHIP_MAX_REVIEWS
            || same_head.iter().any(|call| call.decision.is_none())
        {
            return Err(transition());
        }
        let call = AdaptiveLeadershipReviewCallV1 {
            schema_version: 1,
            review_key: grant.review_id.to_string(),
            allowance_id: allowance_id.to_owned(),
            operation_id,
            grant: grant.clone(),
            context: normalized_context,
            version: 1,
            created_at_unix_ms: now_ms,
            grant_issued_at_unix_ms: now_ms,
            updated_at_unix_ms: now_ms,
            dispatch: None,
            decision: None,
            model_response_digest: None,
            resolution_event_id: None,
        };
        require_current_source(&transaction, &call)?;
        store_call(&transaction, &call, "adaptive_leadership_review_authorized")?;
        transaction.commit()?;
        Ok(call)
    }

    pub fn adaptive_leadership_review_call(
        &self,
        tenant: &TenantId,
        review_id: Uuid,
    ) -> Result<Option<AdaptiveLeadershipReviewCallV1>, WorkflowError> {
        tenant.validate()?;
        if review_id.is_nil() {
            return Err(invalid("invalid leadership review identity"));
        }
        let connection = self.connection.lock().map_err(|_| persistence())?;
        get_entity(&connection, tenant, KIND, &review_id.to_string())
    }

    pub fn adaptive_leadership_review_calls(
        &self,
        tenant: &TenantId,
        session_id: Uuid,
    ) -> Result<Vec<AdaptiveLeadershipReviewCallV1>, WorkflowError> {
        let connection = self.connection.lock().map_err(|_| persistence())?;
        calls_for_session(&connection, tenant, session_id)
    }

    pub fn claim_adaptive_leadership_review_call(
        &self,
        leader: &AuthenticatedCompanyPrincipalV1,
        claim: &ClaimAdaptiveLeadershipReviewCallV1,
        now_ms: u64,
    ) -> Result<AdaptiveLeadershipReviewCallV1, WorkflowError> {
        leader.validate()?;
        validate_digest(&claim.request_digest)?;
        validate_digest(&claim.context_digest)?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut call: AdaptiveLeadershipReviewCallV1 = get_entity(
            &transaction,
            &leader.tenant_id,
            KIND,
            &claim.review_id.to_string(),
        )?
        .ok_or_else(not_found)?;
        if leader != &call.grant.leadership_principal
            || claim.allowance_id != call.allowance_id
            || claim.request_id != call.request_id()
            || call.dispatch.is_some()
            || claim.context_digest != call.context_digest()?
            || now_ms < call.updated_at_unix_ms
            || now_ms >= call.grant.expires_at_unix_ms
        {
            return Err(unauthorized());
        }
        require_current_source(&transaction, &call)?;
        call.dispatch = Some(RequestProviderDispatchV1 {
            request_id: claim.request_id.clone(),
            request_digest: claim.request_digest.clone(),
            context_digest: claim.context_digest.clone(),
            dispatched_at_unix_ms: now_ms,
        });
        call.version = 2;
        call.updated_at_unix_ms = now_ms;
        store_call(&transaction, &call, "adaptive_leadership_review_dispatched")?;
        transaction.commit()?;
        Ok(call)
    }

    pub fn complete_adaptive_leadership_review_call(
        &self,
        leader: &AuthenticatedCompanyPrincipalV1,
        result: &CompleteAdaptiveLeadershipReviewCallV1,
        now_ms: u64,
    ) -> Result<AdaptiveLeadershipReviewCallV1, WorkflowError> {
        leader.validate()?;
        validate_digest(&result.request_digest)?;
        validate_digest(&result.model_response_digest)?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut call: AdaptiveLeadershipReviewCallV1 = get_entity(
            &transaction,
            &leader.tenant_id,
            KIND,
            &result.review_id.to_string(),
        )?
        .ok_or_else(not_found)?;
        result.decision.validate(&call.context.evidence_refs)?;
        if leader != &call.grant.leadership_principal
            || result.allowance_id != call.allowance_id
            || call
                .dispatch
                .as_ref()
                .is_none_or(|dispatch| dispatch.request_digest != result.request_digest)
        {
            return Err(unauthorized());
        }
        if let Some(decision) = &call.decision {
            if decision == &result.decision
                && call.model_response_digest.as_ref() == Some(&result.model_response_digest)
                && call.resolution_event_id == result.resolution_event_id
            {
                return Ok(call);
            }
            return Err(WorkflowError::new(
                WorkflowErrorCode::IdempotencyConflict,
                false,
                "adaptive leadership result changed",
            ));
        }
        if now_ms < call.updated_at_unix_ms {
            return Err(transition());
        }
        if result.decision.resolves_blocked() {
            let resolution = result.resolution_event_id.ok_or_else(transition)?;
            let (session, _) = crate::store::adaptive::load(&transaction, call.grant.session_id)?
                .ok_or_else(not_found)?;
            crate::store::adaptive::require_head(&transaction, &session)?;
            if session.version
                != call
                    .grant
                    .expected_session_version
                    .checked_add(1)
                    .ok_or_else(transition)?
                || session.grant != call.context.source_session.grant
                || !matches!(&session.cursor, AdaptiveCursorV1::BlockedResolved { reason_code, resolution_event_id }
                    if reason_code == &call.grant.expected_reason_code && resolution_event_id == &resolution.to_string())
            {
                return Err(transition());
            }
        } else {
            if result.resolution_event_id.is_some() {
                return Err(transition());
            }
            require_current_source(&transaction, &call)?;
        }
        call.decision = Some(result.decision.clone());
        call.model_response_digest = Some(result.model_response_digest.clone());
        call.resolution_event_id = result.resolution_event_id;
        call.version = 3;
        call.updated_at_unix_ms = now_ms;
        store_call(&transaction, &call, "adaptive_leadership_review_completed")?;
        transaction.commit()?;
        Ok(call)
    }
}
