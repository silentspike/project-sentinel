use super::*;
use crate::{
    adaptive_leadership_recovery_epoch_key, AdaptiveLeadershipRecoveryEpochV1,
    AdaptiveLeadershipRecoveryRequestV1, PrincipalAuthorityV1,
};

const EPOCH_KIND: &str = "adaptive_leadership_recovery_epoch";
const EPOCH_EVENT: &str = "adaptive_leadership_recovery_epoch_authorized";

fn conflict() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::IdempotencyConflict,
        false,
        "adaptive recovery epoch is bound to another request",
    )
}

impl CompanyEntity for AdaptiveLeadershipRecoveryEpochV1 {
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        (&self.request.tenant_id, EPOCH_KIND, &self.epoch_key, 1)
    }

    fn validate_entity(&self) -> Result<(), WorkflowError> {
        self.validate()
    }

    fn validate_persisted(&self, connection: &Connection) -> Result<(), WorkflowError> {
        let mut statement = connection.prepare(
            "SELECT payload,payload_digest,operation_digest,authority_binding_digest,created_at_ms
             FROM company_events WHERE tenant_id=?1 AND event_type=?2 AND operation_id=?3 LIMIT 2",
        )?;
        let records = statement
            .query_map(
                params![
                    self.request.tenant_id.0,
                    EPOCH_EVENT,
                    self.request.operation_id.to_string()
                ],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        if records.len() != 1 {
            return Err(corrupt());
        }
        let (payload, digest, operation, issuer, time) = &records[0];
        if decode::<AdaptiveLeadershipRecoveryEpochV1>(payload)? != *self
            || !constant_time_eq(
                digest,
                &bytes_digest("sentinel.workflow.company-event-payload.v1", payload)?,
            )
            || !constant_time_eq(operation, &self.request.canonical_digest()?)
            || !constant_time_eq(issuer, &self.issuer_principal.binding_digest()?)
            || stored_u64(*time)? != self.issued_at_unix_ms
        {
            return Err(corrupt());
        }
        Ok(())
    }
}

fn epoch_for_call(
    connection: &Connection,
    call: &AdaptiveLeadershipReviewCallV1,
) -> Result<Option<AdaptiveLeadershipRecoveryEpochV1>, WorkflowError> {
    let key = adaptive_leadership_recovery_epoch_key(
        &call.grant.leadership_principal.tenant_id,
        call.grant.session_id,
    )?;
    let epoch: Option<AdaptiveLeadershipRecoveryEpochV1> = get_entity(
        connection,
        &call.grant.leadership_principal.tenant_id,
        EPOCH_KIND,
        &key,
    )?;
    if call.grant.recovery_epoch.is_none() {
        if epoch
            .as_ref()
            .is_some_and(|epoch| epoch.review_id == call.grant.review_id)
        {
            return Err(unauthorized());
        }
        return Ok(None);
    }
    let epoch = epoch.ok_or_else(unauthorized)?;
    epoch.validate_review(&call.grant, &call.context, call.grant_issued_at_unix_ms)?;
    if call.operation_id != epoch.request.operation_id
        || call.allowance_id != format!("leadership-recovery-{}", epoch.epoch_key)
        || call.created_at_unix_ms != epoch.issued_at_unix_ms
    {
        return Err(unauthorized());
    }
    if let Some(authorization) = &call.continuation {
        if authorization.additional_model_calls > epoch.request.max_additional_model_calls
            || authorization
                .deadline_ms
                .checked_sub(authorization.issued_at_ms)
                .is_none_or(|window| window > epoch.request.max_window_ms)
        {
            return Err(unauthorized());
        }
    }
    if let Some(decision) = &call.decision {
        epoch.validate_decision(decision)?;
    }
    Ok(Some(epoch))
}

pub(super) fn require_epoch_time(
    connection: &Connection,
    call: &AdaptiveLeadershipReviewCallV1,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    if let Some(epoch) = epoch_for_call(connection, call)? {
        if now_ms < epoch.issued_at_unix_ms || now_ms >= epoch.expires_at_unix_ms {
            return Err(transition());
        }
    }
    Ok(())
}

pub(super) fn require_epoch_completion(
    connection: &Connection,
    call: &AdaptiveLeadershipReviewCallV1,
    result: &CompleteAdaptiveLeadershipReviewCallV1,
) -> Result<(), WorkflowError> {
    if let Some(epoch) = epoch_for_call(connection, call)? {
        epoch.validate_decision(&result.decision)?;
    }
    Ok(())
}

impl WorkflowStore {
    /// Validate persisted epoch bounds before an authoritative decision audit is appended.
    /// Historical membership only: no clock, current-head or installed-release check.
    pub fn validate_adaptive_leadership_recovery_decision(
        &self,
        call: &AdaptiveLeadershipReviewCallV1,
        decision: &crate::AdaptiveLeadershipReviewDecisionV1,
    ) -> Result<(), WorkflowError> {
        call.validate_entity()?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        if let Some(epoch) = epoch_for_call(&transaction, call)? {
            epoch.validate_decision(decision)?;
        }
        Ok(())
    }

    /// Historical membership only; callers separately enforce current authority and release.
    pub(crate) fn require_recovery_epoch_review(
        connection: &Connection,
        call: &AdaptiveLeadershipReviewCallV1,
    ) -> Result<(), WorkflowError> {
        epoch_for_call(connection, call).map(|_| ())
    }

    pub fn adaptive_leadership_recovery_epoch(
        &self,
        tenant: &TenantId,
        session_id: Uuid,
    ) -> Result<Option<AdaptiveLeadershipRecoveryEpochV1>, WorkflowError> {
        let key = adaptive_leadership_recovery_epoch_key(tenant, session_id)?;
        let connection = self.connection.lock().map_err(|_| persistence())?;
        get_entity(&connection, tenant, EPOCH_KIND, &key)
    }

    pub fn authorize_adaptive_leadership_recovery_epoch(
        &self,
        operator: &AuthenticatedCompanyPrincipalV1,
        request: &AdaptiveLeadershipRecoveryRequestV1,
        grant: &AdaptiveLeadershipReviewGrantV1,
        context: &AdaptiveLeadershipReviewContextV1,
        now_ms: u64,
    ) -> Result<(bool, AdaptiveLeadershipRecoveryEpochV1), WorkflowError> {
        operator.validate()?;
        if operator.kind != CompanyPrincipalKindV1::Operator
            || !matches!(
                operator.role,
                CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
            )
            || operator.tenant_id != request.tenant_id
        {
            return Err(unauthorized());
        }
        let key = adaptive_leadership_recovery_epoch_key(&request.tenant_id, request.session_id)?;
        let mut normalized_context = context.clone();
        normalized_context.evidence_refs.sort();
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(prior) = get_entity::<AdaptiveLeadershipRecoveryEpochV1>(
            &transaction,
            &request.tenant_id,
            EPOCH_KIND,
            &key,
        )? {
            if prior.request != *request
                || prior.issuer_principal != *operator
                || prior.review_grant != *grant
                || prior.source_context != normalized_context
            {
                return Err(conflict());
            }
            let call: AdaptiveLeadershipReviewCallV1 = get_entity(
                &transaction,
                &request.tenant_id,
                KIND,
                &prior.review_id.to_string(),
            )?
            .ok_or_else(corrupt)?;
            Self::require_recovery_epoch_review(&transaction, &call)?;
            return Ok((true, prior));
        }
        request.validate(operator, now_ms)?;
        if grant.recovery_epoch.is_some() {
            return Err(unauthorized());
        }
        let operation_exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM company_events WHERE tenant_id=?1 AND event_type=?2 AND operation_id=?3)",
            params![request.tenant_id.0, EPOCH_EVENT, request.operation_id.to_string()],
            |row| row.get(0),
        )?;
        if operation_exists {
            return Err(conflict());
        }
        let epoch = AdaptiveLeadershipRecoveryEpochV1 {
            schema_version: 1,
            epoch_key: key,
            request: request.clone(),
            issuer_principal: operator.clone(),
            issuer_authority: PrincipalAuthorityV1 {
                schema_version: 1,
                principal_id: operator.principal_id.clone(),
                principal_generation: operator.authority_generation,
                authority_digest: operator.authority_digest.clone(),
            },
            review_grant: grant.clone(),
            source_context: normalized_context,
            review_id: grant.review_id,
            issued_at_unix_ms: now_ms,
            expires_at_unix_ms: request.expires_at_unix_ms,
        };
        epoch.validate()?;
        let calls = calls_for_session(&transaction, &request.tenant_id, request.session_id)?;
        epoch.validate_history(&calls)?;
        let (session, head_digest) =
            crate::store::adaptive::load(&transaction, request.session_id)?
                .ok_or_else(not_found)?;
        crate::store::adaptive::require_head(&transaction, &session)?;
        if !constant_time_eq(&head_digest, &request.session_head_digest)
            || session != epoch.source_context.source_session
            || now_ms < session.active_deadline_ms()
            || session
                .model_calls
                .checked_add(request.max_additional_model_calls)
                .is_none_or(|calls| calls > session.grant.max_model_calls)
            || session
                .grant
                .deadline_ms
                .checked_sub(session.grant.created_at_ms)
                .is_none_or(|window| request.max_window_ms > window)
            || get_entity::<AdaptiveLeadershipReviewCallV1>(
                &transaction,
                &request.tenant_id,
                KIND,
                &grant.review_id.to_string(),
            )?
            .is_some()
        {
            return Err(transition());
        }
        let mut bound_grant = grant.clone();
        bound_grant.recovery_epoch = Some(epoch.binding()?);
        let call = AdaptiveLeadershipReviewCallV1 {
            schema_version: 2,
            review_key: grant.review_id.to_string(),
            allowance_id: format!("leadership-recovery-{}", epoch.epoch_key),
            operation_id: request.operation_id,
            grant: bound_grant,
            context: epoch.source_context.clone(),
            version: 1,
            created_at_unix_ms: now_ms,
            grant_issued_at_unix_ms: now_ms,
            updated_at_unix_ms: now_ms,
            dispatch: None,
            decision: None,
            model_response_digest: None,
            resolution_event_id: None,
            retired_at_unix_ms: None,
            continuation: None,
        };
        epoch.validate_review(&call.grant, &call.context, now_ms)?;
        call.validate_entity()?;
        require_current_source(&transaction, &call)?;
        require_subject_time(&call, now_ms)?;
        put_entity(
            &transaction,
            &request.tenant_id,
            EPOCH_KIND,
            &epoch.epoch_key,
            1,
            &epoch,
        )?;
        append_event(
            &transaction,
            operator,
            request.operation_id,
            &request.canonical_digest()?,
            Some(&request.project_id),
            EPOCH_EVENT,
            &epoch,
            now_ms,
        )?;
        store_call(
            &transaction,
            &call,
            "adaptive_leadership_recovery_review_authorized",
        )?;
        transaction.commit()?;
        Ok((false, epoch))
    }
}
