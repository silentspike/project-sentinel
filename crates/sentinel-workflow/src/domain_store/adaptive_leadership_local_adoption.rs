use super::*;
use crate::{
    adaptive_leadership_recovery_epoch_key, local_adoption_key,
    AdaptiveLeadershipLocalAdoptionRequestV1, AdaptiveLeadershipLocalAdoptionV1,
    AdaptiveLeadershipRecoveryEpochV1, AdaptiveLeadershipReviewCallV1, PrincipalAuthorityV1,
};

const KIND: &str = "adaptive_leadership_local_adoption";
const EVENT: &str = "adaptive_leadership_local_adoption_authorized";

impl CompanyEntity for AdaptiveLeadershipLocalAdoptionV1 {
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        (&self.request.tenant_id, KIND, &self.adoption_key, 1)
    }

    fn validate_entity(&self) -> Result<(), WorkflowError> {
        self.validate()
    }

    fn validate_persisted(&self, connection: &Connection) -> Result<(), WorkflowError> {
        let rows = {
            let mut statement = connection.prepare(
                "SELECT payload,payload_digest,operation_digest,authority_binding_digest,created_at_ms
                 FROM company_events WHERE tenant_id=?1 AND event_type=?2 AND operation_id=?3 LIMIT 2",
            )?;
            let rows = statement.query_map(
                params![
                    self.request.tenant_id.0,
                    EVENT,
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
            )?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        if rows.len() != 1 {
            return Err(corrupt());
        }
        let (payload, digest, operation, issuer, time) = &rows[0];
        if decode::<Self>(payload)? != *self
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

pub(super) fn require_local_adoption(
    connection: &Connection,
    call: &AdaptiveLeadershipReviewCallV1,
    adoption: &AdaptiveLeadershipLocalAdoptionV1,
) -> Result<(), WorkflowError> {
    let stored: AdaptiveLeadershipLocalAdoptionV1 = get_entity(
        connection,
        &adoption.request.tenant_id,
        KIND,
        &adoption.adoption_key,
    )?
    .ok_or_else(unauthorized)?;
    if stored != *adoption {
        return Err(unauthorized());
    }
    let epoch: AdaptiveLeadershipRecoveryEpochV1 = get_entity(
        connection,
        &adoption.request.tenant_id,
        "adaptive_leadership_recovery_epoch",
        &adaptive_leadership_recovery_epoch_key(
            &adoption.request.tenant_id,
            adoption.request.session_id,
        )?,
    )?
    .ok_or_else(unauthorized)?;
    adoption.validate_against(&epoch, call)
}

impl WorkflowStore {
    pub fn adaptive_leadership_local_adoption(
        &self,
        tenant: &TenantId,
        review_id: Uuid,
    ) -> Result<Option<AdaptiveLeadershipLocalAdoptionV1>, WorkflowError> {
        let key = local_adoption_key(tenant, review_id)?;
        let connection = self.connection.lock().map_err(|_| persistence())?;
        get_entity(&connection, tenant, KIND, &key)
    }

    /// Authorization is distinct from the old review; replay never renews either clock.
    pub fn authorize_adaptive_leadership_local_adoption(
        &self,
        operator: &AuthenticatedCompanyPrincipalV1,
        request: &AdaptiveLeadershipLocalAdoptionRequestV1,
        now_ms: u64,
    ) -> Result<(bool, AdaptiveLeadershipLocalAdoptionV1), WorkflowError> {
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
        let key = local_adoption_key(&request.tenant_id, request.review_id)?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(prior) = get_entity::<AdaptiveLeadershipLocalAdoptionV1>(
            &transaction,
            &request.tenant_id,
            KIND,
            &key,
        )? {
            if prior.request != *request || prior.issuer_principal != *operator {
                return Err(WorkflowError::new(
                    WorkflowErrorCode::IdempotencyConflict,
                    false,
                    "local adoption already bound to another request",
                ));
            }
            return Ok((true, prior));
        }
        request.validate(operator, now_ms)?;
        let operation_exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM company_events WHERE tenant_id=?1 AND event_type=?2 AND operation_id=?3)",
            params![request.tenant_id.0, EVENT, request.operation_id.to_string()],
            |row| row.get(0),
        )?;
        if operation_exists {
            return Err(WorkflowError::new(
                WorkflowErrorCode::IdempotencyConflict,
                false,
                "local adoption operation already used",
            ));
        }
        let call: AdaptiveLeadershipReviewCallV1 = get_entity(
            &transaction,
            &request.tenant_id,
            "adaptive_leadership_review_call",
            &request.review_id.to_string(),
        )?
        .ok_or_else(not_found)?;
        let epoch: AdaptiveLeadershipRecoveryEpochV1 = get_entity(
            &transaction,
            &request.tenant_id,
            "adaptive_leadership_recovery_epoch",
            &adaptive_leadership_recovery_epoch_key(&request.tenant_id, request.session_id)?,
        )?
        .ok_or_else(not_found)?;
        let crate::AdaptiveLeadershipReviewDecisionKindV1::Continue { window_ms, .. } =
            &request.decision.decision
        else {
            return Err(unauthorized());
        };
        let record = AdaptiveLeadershipLocalAdoptionV1 {
            adoption_key: key,
            request: request.clone(),
            issuer_principal: operator.clone(),
            issuer_authority: PrincipalAuthorityV1 {
                schema_version: 1,
                principal_id: operator.principal_id.clone(),
                principal_generation: operator.authority_generation,
                authority_digest: operator.authority_digest.clone(),
            },
            issued_at_unix_ms: now_ms,
            continuation_deadline_ms: now_ms.checked_add(*window_ms).ok_or_else(transition)?,
        };
        record.validate_against(&epoch, &call)?;
        let project: ProjectV1 = get_entity(
            &transaction,
            &request.tenant_id,
            "project",
            &request.project_id.0,
        )?
        .ok_or_else(not_found)?;
        let (session, head) = crate::store::adaptive::load(&transaction, request.session_id)?
            .ok_or_else(not_found)?;
        crate::store::adaptive::require_head(&transaction, &session)?;
        if project != call.context.source_project
            || session != call.context.source_session
            || head != request.session_head_digest
        {
            return Err(transition());
        }
        call.validate_continuation_source()?;
        put_entity(
            &transaction,
            &request.tenant_id,
            KIND,
            &record.adoption_key,
            1,
            &record,
        )?;
        append_event(
            &transaction,
            operator,
            request.operation_id,
            &request.canonical_digest()?,
            Some(&request.project_id),
            EVENT,
            &record,
            now_ms,
        )?;
        transaction.commit()?;
        Ok((false, record))
    }
}
