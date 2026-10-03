use super::*;
use crate::{
    adaptive_leadership_admission_repair_history_digest, adaptive_leadership_admission_repair_key,
    adaptive_leadership_recovery_epoch_key, AdaptiveLeadershipAdmissionRepairEpochV1,
    AdaptiveLeadershipAdmissionRepairEvidenceV1, AdaptiveLeadershipAdmissionRepairSourceV1,
    AdaptiveLeadershipRecoveryEpochV1, AdaptiveLeadershipRecoveryRequestV1, PrincipalAuthorityV1,
};

const EPOCH_KIND: &str = "adaptive_leadership_recovery_epoch";
const EPOCH_EVENT: &str = "adaptive_leadership_recovery_epoch_authorized";
const REPAIR_KIND: &str = "adaptive_leadership_admission_repair_epoch";
const REPAIR_EVENT: &str = "adaptive_leadership_admission_repair_authorized";

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
        if self.schema_version != 1 || self.request.admission_repair.is_some() {
            return Err(corrupt());
        }
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

impl CompanyEntity for AdaptiveLeadershipAdmissionRepairEpochV1 {
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        (
            &self.epoch.request.tenant_id,
            REPAIR_KIND,
            &self.epoch.epoch_key,
            1,
        )
    }

    fn validate_entity(&self) -> Result<(), WorkflowError> {
        self.validate()
    }

    fn validate_persisted(&self, connection: &Connection) -> Result<(), WorkflowError> {
        validate_repair_source(connection, &self.source)?;
        require_repair_event(connection, self)
    }
}

fn require_repair_event(
    connection: &Connection,
    receipt: &AdaptiveLeadershipAdmissionRepairEpochV1,
) -> Result<(), WorkflowError> {
    let request = &receipt.epoch.request;
    let mut statement = connection.prepare(
        "SELECT payload,payload_digest,operation_digest,authority_binding_digest,created_at_ms
             FROM company_events WHERE tenant_id=?1 AND event_type=?2 AND operation_id=?3 LIMIT 2",
    )?;
    let records = statement
        .query_map(
            params![
                request.tenant_id.0,
                REPAIR_EVENT,
                request.operation_id.to_string()
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
    if decode::<AdaptiveLeadershipAdmissionRepairEpochV1>(payload)? != *receipt
        || !constant_time_eq(
            digest,
            &bytes_digest("sentinel.workflow.company-event-payload.v1", payload)?,
        )
        || !constant_time_eq(operation, &request.canonical_digest()?)
        || !constant_time_eq(issuer, &receipt.epoch.issuer_principal.binding_digest()?)
        || stored_u64(*time)? != receipt.epoch.issued_at_unix_ms
    {
        return Err(corrupt());
    }
    Ok(())
}

fn immutable_repair_epoch(
    connection: &Connection,
    tenant: &TenantId,
    session_id: Uuid,
) -> Result<Option<AdaptiveLeadershipRecoveryEpochV1>, WorkflowError> {
    validation_scope::memoize(
        connection,
        "admission-repair-identity",
        &(tenant, session_id),
        || {
            let key = adaptive_leadership_admission_repair_key(tenant, session_id)?;
            let row: Option<(Vec<u8>, String, i64)> = connection
                .query_row(
                    "SELECT payload,payload_digest,version FROM company_entities
             WHERE tenant_id=?1 AND entity_kind=?2 AND entity_id=?3",
                    params![tenant.0, REPAIR_KIND, key],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let Some((payload, digest, version)) = row else {
                return Ok(None);
            };
            if stored_u64(version)? != 1
                || !constant_time_eq(
                    &digest,
                    &bytes_digest("sentinel.workflow.company-entity-row.v1", &payload)?,
                )
            {
                return Err(corrupt());
            }
            let receipt: AdaptiveLeadershipAdmissionRepairEpochV1 = decode(&payload)?;
            receipt.validate()?;
            if receipt.epoch.request.tenant_id != *tenant || receipt.epoch.epoch_key != key {
                return Err(corrupt());
            }
            // Only the immutable issuance proof is needed here: validating its old calls
            // recursively would make their membership depend on their own validation.
            require_repair_event(connection, &receipt)?;
            Ok(Some(*receipt.epoch))
        },
    )
}

pub(super) fn has_repair_disposition(call: &AdaptiveLeadershipReviewCallV1) -> bool {
    // A continuation is only a candidate here; its persisted proofs are checked below.
    call.decision.is_some() || call.retired_at_unix_ms.is_some() || call.continuation.is_some()
}

fn validate_repair_source(
    connection: &Connection,
    source: &AdaptiveLeadershipAdmissionRepairSourceV1,
) -> Result<(), WorkflowError> {
    let session = &source.session;
    let tenant = &session.grant.authority.tenant_id;
    if source.schema_version != 1
        || !matches!(session.cursor, AdaptiveCursorV1::ReadyForModel)
        || session.model_calls >= session.grant.max_model_calls
        || source.project.tenant_id != *tenant
        || source.project.project_id != session.grant.authority.project_id
        || source.calls.is_empty()
        || source.calls.len() > MAX_AGGREGATE_ITEMS
        || source
            .calls
            .windows(2)
            .any(|pair| pair[0].grant.review_id >= pair[1].grant.review_id)
        || source.qualifying_review_ids.is_empty()
        || session.continuation.as_ref().is_none_or(|state| {
            state.authorizations.len() >= crate::ADAPTIVE_CONTINUATION_MAX_WINDOWS
        })
    {
        return Err(transition());
    }
    validate_project(&source.project)?;
    crate::store::adaptive::require_journal_source(connection, session)?;
    let (original, successor) = budget_review_extension::admission_repair_extensions(
        connection,
        tenant,
        session.grant.session_id,
        session.version,
        &source.calls,
    )?;
    if &original != source.original_extension.as_ref()
        || &successor != source.successor_extension.as_ref()
        || original.request.project_id != source.project.project_id
        || get_entity::<AdaptiveLeadershipRecoveryEpochV1>(
            connection,
            tenant,
            EPOCH_KIND,
            &adaptive_leadership_recovery_epoch_key(tenant, session.grant.session_id)?,
        )?
        .as_ref()
            != Some(source.legacy_epoch.as_ref())
        || !source
            .calls
            .iter()
            .any(|call| call.grant.review_id == source.legacy_epoch.review_id)
    {
        return Err(corrupt());
    }
    let mut qualifying = Vec::new();
    let mut continuations = Vec::new();
    for call in &source.calls {
        let stored: AdaptiveLeadershipReviewCallV1 =
            get_entity(connection, tenant, KIND, &call.review_key)?.ok_or_else(corrupt)?;
        if stored != *call
            || !matches!(call.grant.schema_version, 1..=3)
            || call.grant.session_id != session.grant.session_id
            || call.grant.project_id != source.project.project_id
            || call.grant.work_item_id != session.grant.authority.work_item_id
            || call.context.source_session.grant != session.grant
            || !has_repair_disposition(call)
        {
            return Err(transition());
        }
        if call.grant.schema_version == 3
            && call.grant.expected_session_version == session.version
            && call.context.source_session == *session
            && call.context.source_project == source.project
            && call.dispatch.is_none()
            && call.decision.is_none()
            && call
                .retired_at_unix_ms
                .is_some_and(|at| at >= call.grant.expires_at_unix_ms)
        {
            qualifying.push(call.grant.review_id);
        }
        if let Some(authorization) = &call.continuation {
            let allowance = call.continuation_allowance(
                authorization.issued_at_ms,
                authorization.deadline_ms,
                authorization.additional_model_calls,
            )?;
            validate_governed_allowance_receipt(call, &allowance)?;
            if session
                .continuation
                .as_ref()
                .is_none_or(|state| !state.authorizations.contains(authorization))
            {
                return Err(corrupt());
            }
            let abandoned: AdaptiveLeadershipAbandonedAllowanceV2 = get_entity(
                connection,
                tenant,
                ABANDONED_KIND,
                &call
                    .context
                    .source_project
                    .subscription_call
                    .as_ref()
                    .ok_or_else(corrupt)?
                    .allowance_id,
            )?
            .ok_or_else(corrupt)?;
            if abandoned.review != *call {
                return Err(corrupt());
            }
            crate::store::adaptive::require_journal_source(
                connection,
                &call.context.source_session,
            )?;
            if let Some(adoption) = &authorization.local_adoption {
                super::super::adaptive_leadership_local_adoption::require_local_adoption(
                    connection, call, adoption,
                )?;
            }
            continuations.push(call.clone());
        }
    }
    if qualifying != source.qualifying_review_ids
        || continuations != source.continuation_reviews
        || session
            .continuation
            .as_ref()
            .is_none_or(|state| state.authorizations.len() != continuations.len())
    {
        return Err(corrupt());
    }
    Ok(())
}

fn repair_source(
    connection: &Connection,
    tenant: &TenantId,
    session_id: Uuid,
) -> Result<AdaptiveLeadershipAdmissionRepairSourceV1, WorkflowError> {
    let (session, session_head_digest) =
        crate::store::adaptive::load(connection, session_id)?.ok_or_else(not_found)?;
    crate::store::adaptive::require_head(connection, &session)?;
    if session.grant.authority.tenant_id != *tenant {
        return Err(unauthorized());
    }
    let project: ProjectV1 = get_entity(
        connection,
        tenant,
        "project",
        &session.grant.authority.project_id.0,
    )?
    .ok_or_else(not_found)?;
    let calls = calls_for_session(connection, tenant, session_id)?;
    let (original_extension, successor_extension) =
        budget_review_extension::admission_repair_extensions(
            connection,
            tenant,
            session_id,
            session.version,
            &calls,
        )?;
    let legacy_epoch = get_entity(
        connection,
        tenant,
        EPOCH_KIND,
        &adaptive_leadership_recovery_epoch_key(tenant, session_id)?,
    )?
    .ok_or_else(not_found)?;
    let source = AdaptiveLeadershipAdmissionRepairSourceV1 {
        schema_version: 1,
        qualifying_review_ids: calls
            .iter()
            .filter(|call| {
                call.grant.schema_version == 3
                    && call.grant.expected_session_version == session.version
                    && call.context.source_session == session
                    && call.context.source_project == project
                    && call.dispatch.is_none()
                    && call.decision.is_none()
                    && call
                        .retired_at_unix_ms
                        .is_some_and(|at| at >= call.grant.expires_at_unix_ms)
            })
            .map(|call| call.grant.review_id)
            .collect(),
        continuation_reviews: calls
            .iter()
            .filter(|call| call.continuation.is_some())
            .cloned()
            .collect(),
        session,
        session_head_digest,
        project,
        calls,
        original_extension: Box::new(original_extension),
        successor_extension: Box::new(successor_extension),
        legacy_epoch: Box::new(legacy_epoch),
    };
    validate_repair_source(connection, &source)?;
    Ok(source)
}

fn epoch_for_call(
    connection: &Connection,
    call: &AdaptiveLeadershipReviewCallV1,
) -> Result<Option<AdaptiveLeadershipRecoveryEpochV1>, WorkflowError> {
    if call.grant.schema_version == 4 {
        // Membership needs the immutable issuance proof, not a recursive read
        // of every historical call through extension/limit validation.
        let epoch = immutable_repair_epoch(
            connection,
            &call.grant.leadership_principal.tenant_id,
            call.grant.session_id,
        )?
        .ok_or_else(unauthorized)?;
        epoch.validate_review(&call.grant, &call.context, call.grant_issued_at_unix_ms)?;
        if call.operation_id != epoch.request.operation_id
            || call.allowance_id != format!("leadership-recovery-{}", epoch.epoch_key)
            || call.created_at_unix_ms != epoch.issued_at_unix_ms
        {
            return Err(unauthorized());
        }
        if let Some(decision) = &call.decision {
            epoch.validate_decision(decision)?;
        }
        return Ok(Some(epoch));
    }
    if immutable_repair_epoch(
        connection,
        &call.grant.leadership_principal.tenant_id,
        call.grant.session_id,
    )?
    .is_some_and(|epoch| epoch.review_id == call.grant.review_id)
    {
        return Err(unauthorized());
    }
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
        if call.grant.resume_policy.is_some() {
            crate::domain_store::adaptive_resume_policy::require_resume_review_membership(
                &transaction,
                &call.grant,
                &call.context_digest()?,
                call.operation_id,
            )?;
        }
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

    /// Both disjoint request subjects consume the same permanent original-session slot.
    /// Epoch validation binds Blocked to its retained result, never an invented unknown proof.
    pub fn authorize_adaptive_leadership_recovery_epoch(
        &self,
        operator: &AuthenticatedCompanyPrincipalV1,
        request: &AdaptiveLeadershipRecoveryRequestV1,
        grant: &AdaptiveLeadershipReviewGrantV1,
        context: &AdaptiveLeadershipReviewContextV1,
        now_ms: u64,
    ) -> Result<(bool, AdaptiveLeadershipRecoveryEpochV1), WorkflowError> {
        operator.validate()?;
        if request.admission_repair.is_some()
            || request.schema_version == 3
            || grant.schema_version == 4
        {
            return Err(unauthorized());
        }
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
        if crate::domain_store::adaptive_resume_policy::read_resume_policy_leaf(
            &transaction,
            &request.tenant_id,
            request.session_id,
        )?
        .is_some()
        {
            return Err(unauthorized());
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

    pub fn adaptive_leadership_admission_repair_source(
        &self,
        tenant: &TenantId,
        session_id: Uuid,
    ) -> Result<AdaptiveLeadershipAdmissionRepairSourceV1, WorkflowError> {
        tenant.validate()?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        validation_scope::with_scope(&transaction, || {
            repair_source(&transaction, tenant, session_id)
        })
    }

    pub fn adaptive_leadership_admission_repair_epoch(
        &self,
        tenant: &TenantId,
        session_id: Uuid,
    ) -> Result<Option<AdaptiveLeadershipAdmissionRepairEpochV1>, WorkflowError> {
        let key = adaptive_leadership_admission_repair_key(tenant, session_id)?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        validation_scope::with_scope(&transaction, || {
            get_entity(&transaction, tenant, REPAIR_KIND, &key)
        })
    }

    /// The trusted verifier must classify all provider effects and attest deployed repair.
    /// It runs before any write, must not re-enter this store, and is skipped on exact replay.
    pub fn authorize_adaptive_leadership_admission_repair<F>(
        &self,
        operator: &AuthenticatedCompanyPrincipalV1,
        request: &AdaptiveLeadershipRecoveryRequestV1,
        grant: &AdaptiveLeadershipReviewGrantV1,
        context: &AdaptiveLeadershipReviewContextV1,
        now_ms: u64,
        verifier: F,
    ) -> Result<(bool, AdaptiveLeadershipAdmissionRepairEpochV1), WorkflowError>
    where
        F: FnOnce(
            &AdaptiveLeadershipAdmissionRepairSourceV1,
            &AdaptiveLeadershipRecoveryRequestV1,
        ) -> Result<AdaptiveLeadershipAdmissionRepairEvidenceV1, WorkflowError>,
    {
        operator.validate()?;
        if operator.kind != CompanyPrincipalKindV1::Operator
            || !matches!(
                operator.role,
                CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
            )
            || operator.tenant_id != request.tenant_id
            || request.schema_version != 3
            || request.admission_repair.is_none()
        {
            return Err(unauthorized());
        }
        let key = adaptive_leadership_admission_repair_key(&request.tenant_id, request.session_id)?;
        let mut normalized_context = context.clone();
        normalized_context.evidence_refs.sort();
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let scope = validation_scope::enter(&transaction)?;
        if let Some(prior) = get_entity::<AdaptiveLeadershipAdmissionRepairEpochV1>(
            &transaction,
            &request.tenant_id,
            REPAIR_KIND,
            &key,
        )? {
            if prior.epoch.request != *request
                || prior.epoch.issuer_principal != *operator
                || prior.epoch.review_grant != *grant
                || prior.epoch.source_context != normalized_context
            {
                return Err(conflict());
            }
            let call: AdaptiveLeadershipReviewCallV1 = get_entity(
                &transaction,
                &request.tenant_id,
                KIND,
                &prior.epoch.review_id.to_string(),
            )?
            .ok_or_else(corrupt)?;
            Self::require_recovery_epoch_review(&transaction, &call)?;
            scope.finish()?;
            return Ok((true, prior));
        }
        if crate::domain_store::adaptive_resume_policy::read_resume_policy_leaf(
            &transaction,
            &request.tenant_id,
            request.session_id,
        )?
        .is_some()
        {
            return Err(unauthorized());
        }
        request.validate(operator, now_ms)?;
        let source = repair_source(&transaction, &request.tenant_id, request.session_id)?;
        let epoch = AdaptiveLeadershipRecoveryEpochV1 {
            schema_version: 2,
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
        if source.session != epoch.source_context.source_session
            || source.project != epoch.source_context.source_project
            || source.session_head_digest != request.session_head_digest
            || source.canonical_digest()?
                != request
                    .admission_repair
                    .as_ref()
                    .ok_or_else(unauthorized)?
                    .source_digest
            || adaptive_leadership_admission_repair_history_digest(&source.calls)?
                != request.prior_review_history_digest
            || source
                .calls
                .iter()
                .any(|call| call.updated_at_unix_ms > now_ms)
            || source
                .session
                .grant
                .deadline_ms
                .checked_sub(source.session.grant.created_at_ms)
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
        let operation_exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM company_events WHERE tenant_id=?1 AND operation_id=?2)",
            params![request.tenant_id.0, request.operation_id.to_string()],
            |row| row.get(0),
        )?;
        if operation_exists {
            return Err(conflict());
        }
        let mut bound_grant = grant.clone();
        bound_grant.recovery_epoch = Some(epoch.binding()?);
        let call = AdaptiveLeadershipReviewCallV1 {
            schema_version: 4,
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
        call.validate_entity()?;
        require_current_source(&transaction, &call)?;
        require_subject_time(&call, now_ms)?;
        // The file-only trusted verifier supplies proof; metadata absence is never no-I/O evidence.
        let evidence = verifier(&source, request)?;
        let receipt = AdaptiveLeadershipAdmissionRepairEpochV1 {
            epoch: Box::new(epoch),
            source: Box::new(source),
            evidence,
        };
        receipt.validate()?;
        scope.finish()?;
        put_entity(
            &transaction,
            &request.tenant_id,
            REPAIR_KIND,
            &receipt.epoch.epoch_key,
            1,
            &receipt,
        )?;
        append_event(
            &transaction,
            operator,
            request.operation_id,
            &request.canonical_digest()?,
            Some(&request.project_id),
            REPAIR_EVENT,
            &receipt,
            now_ms,
        )?;
        store_call(
            &transaction,
            &call,
            "adaptive_leadership_admission_repair_review_authorized",
        )?;
        transaction.commit()?;
        Ok((false, receipt))
    }
}
