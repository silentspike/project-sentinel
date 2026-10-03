use super::adaptive_leadership_review::{calls_for_session, require_current_source};
use super::adaptive_resume_policy::read_resume_policy_leaf;
use super::*;
use crate::adaptive_resume_policy::require_resume_policy_operator;
use crate::{
    adaptive_accounting_projection, AdaptiveAccountingReconsiderationReceiptV1,
    AdaptiveAccountingReconsiderationRequestV1, AdaptiveAccountingReconsiderationSourceV1,
    AdaptiveLeadershipReviewCallV1, AdaptiveLeadershipReviewContextV1,
    AdaptiveLeadershipReviewDecisionKindV1, AdaptiveLeadershipReviewGrantV1, PrincipalAuthorityV1,
    RuntimeAuthoritySnapshotV1, ADAPTIVE_ACCOUNTING_RECONSIDERATION_MAX_MS,
};

const KIND: &str = "adaptive_accounting_reconsideration";
const EVENT: &str = "adaptive_accounting_reconsideration_authorized";
const CORRECTION_PREFIX: &str = "adaptive-accounting-correction:";
const PROJECTION_PREFIX: &str = "adaptive-accounting-projection:";

fn receipt_id(tenant: &TenantId, session_id: Uuid) -> Result<String, WorkflowError> {
    Ok(format!(
        "accounting-correction-{}",
        canonical_sha256(
            "sentinel.workflow.adaptive-accounting-reconsideration-id.v1",
            &(tenant, session_id),
        )?
    ))
}

impl CompanyEntity for AdaptiveAccountingReconsiderationReceiptV1 {
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        (&self.issuer_principal.tenant_id, KIND, &self.receipt_id, 1)
    }

    fn validate_entity(&self) -> Result<(), WorkflowError> {
        self.request.validate_shape()?;
        let source = &self.source;
        let session = &source.source_session;
        let refused = &source.refused_review;
        require_resume_policy_operator(&self.issuer_principal, &source.source_project.tenant_id)?;
        validate_project(&source.source_project)?;
        session.grant.validate()?;
        source.policy.validate()?;
        source
            .policy
            .validate_binding(refused.grant.resume_policy.as_deref().ok_or_else(corrupt)?)?;
        // Historical shape only: never read the designated call or its memberships here.
        refused.validate_entity()?;
        source.leadership_principal.validate()?;
        source.leadership_authority.validate()?;
        for digest in [
            &source.root_entry_digest,
            &source.head_entry_digest,
            &source.review_history_digest,
            &self.grant_digest,
            &self.context_digest,
        ] {
            validate_digest(digest)?;
        }
        validate_identifier(&self.allowance_id)?;
        let binding = source.policy.binding(source.next_ordinal)?;
        let mut refs = vec![source.accounting.evidence_ref()?, source.evidence_ref()?];
        refs.sort();
        if self.schema_version != 1
            || source.schema_version != 1
            || self.receipt_id
                != receipt_id(&self.issuer_principal.tenant_id, self.request.session_id)?
            || self.review_id.is_nil()
            || self.review_operation_id.is_nil()
            || self.review_id == self.request.refused_review_id
            || self.review_operation_id == self.request.operation_id
            || self.request.project_id != source.source_project.project_id
            || self.request.session_id != session.grant.session_id
            || self.request.refused_review_id != refused.grant.review_id
            || self.request.source_digest != source.source_digest
            || source.source_digest != source.computed_digest()?
            || source.evidence_refs != refs
            || source.accounting != adaptive_accounting_projection(session, &binding)?
            || source.policy.request.source.session_id != self.request.session_id
            || source.policy.request.source.project_id != self.request.project_id
            || source.policy.request.source.tenant_id != self.issuer_principal.tenant_id
            || source.policy.request.source.assignee_authority != session.grant.authority
            || refused.context.source_project != source.source_project
            || refused.context.source_session != *session
            || refused.grant.schema_version != 3
            || refused.grant.recovery_epoch.is_some()
            || refused.grant.leadership_principal != source.leadership_principal
            || refused.grant.leadership_authority != source.leadership_authority
            || refused.grant.resume_policy.as_ref().is_none_or(|prior| {
                prior.policy_id != binding.policy_id
                    || prior.receipt_digest != binding.receipt_digest
                    || prior.limits != binding.limits
                    || prior.ordinal >= binding.ordinal
            })
            || refused.dispatch.is_none()
            || refused.retired_at_unix_ms.is_some()
            || !matches!(
                refused.decision.as_ref().map(|d| &d.decision),
                Some(AdaptiveLeadershipReviewDecisionKindV1::DeferBudget { .. })
            )
            || self.issued_at_unix_ms < refused.updated_at_unix_ms
            || self.request.expires_at_unix_ms <= self.issued_at_unix_ms
            || self.request.expires_at_unix_ms
                > self
                    .issued_at_unix_ms
                    .saturating_add(ADAPTIVE_ACCOUNTING_RECONSIDERATION_MAX_MS)
            || self.request.expires_at_unix_ms > binding.limits.expires_at_unix_ms
            || source.accounting.root_model_calls_remaining == 0
            || source.accounting.root_tool_calls_remaining == 0
            || source.accounting.windows_remaining == 0
        {
            return Err(corrupt());
        }
        Ok(())
    }

    fn validate_persisted(&self, connection: &Connection) -> Result<(), WorkflowError> {
        // The event and immutable leaf are the proof. No journal/call traversal or real clock.
        require_sealed_event(
            connection,
            &self.issuer_principal,
            &self.request.project_id,
            EVENT,
            self.request.operation_id,
            &self.request.canonical_digest()?,
            self.issued_at_unix_ms,
            self,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn require_sealed_event<T: DeserializeOwned + Serialize + PartialEq>(
    connection: &Connection,
    principal: &AuthenticatedCompanyPrincipalV1,
    project: &ProjectId,
    event: &str,
    operation_id: Uuid,
    operation_digest: &str,
    issued_at_ms: u64,
    value: &T,
) -> Result<(), WorkflowError> {
    let mut statement = connection.prepare(
            "SELECT sequence FROM company_events WHERE tenant_id=?1 AND event_type=?2 AND operation_id=?3 LIMIT 2",
        )?;
    let ids = statement
        .query_map(
            params![principal.tenant_id.0, event, operation_id.to_string()],
            |row| row.get::<_, i64>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    if ids.len() != 1 {
        return Err(corrupt());
    }
    let row = read_company_event_row(connection, stored_u64(ids[0])?)?.ok_or_else(corrupt)?;
    validation_scope::charge_bytes(connection, row.payload.len())?;
    let payload_digest = bytes_digest("sentinel.workflow.company-event-payload.v1", &row.payload)?;
    let binding_digest = principal.binding_digest()?;
    let event_id = canonical_sha256(
        "sentinel.workflow.company-event-id.v1",
        &(
            &principal.tenant_id,
            Some(project),
            event,
            operation_id,
            operation_digest,
            &binding_digest,
            &payload_digest,
            issued_at_ms,
        ),
    )?;
    if company_event_principal(&row)? != *principal
        || row.tenant_id != principal.tenant_id.0
        || row.project_id.as_deref() != Some(project.0.as_str())
        || row.event_type != event
        || row.operation_id != operation_id.to_string()
        || row.operation_digest != operation_digest
        || row.authority_binding_digest != binding_digest
        || row.payload_digest != payload_digest
        || row.event_id != event_id
        || stored_u64(row.created_at_ms)? != issued_at_ms
        || decode::<T>(&row.payload)? != *value
    {
        return Err(corrupt());
    }
    Ok(())
}

fn read_leaf(
    connection: &Connection,
    tenant: &TenantId,
    session_id: Uuid,
) -> Result<Option<AdaptiveAccountingReconsiderationReceiptV1>, WorkflowError> {
    let key = receipt_id(tenant, session_id)?;
    let receipt = get_entity(connection, tenant, KIND, &key)?;
    if receipt.is_none() {
        let orphan: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM company_events WHERE tenant_id=?1 AND event_type=?2
             AND (json_extract(payload,'$.receipt_id')=?3 OR json_extract(payload,'$.request.session_id')=?4))",
            params![tenant.0, EVENT, key, session_id.to_string()], |row| row.get(0),
        )?;
        if orphan {
            return Err(corrupt());
        }
    }
    Ok(receipt)
}

fn require_binding(
    receipt: &AdaptiveAccountingReconsiderationReceiptV1,
    operation_id: Uuid,
    allowance_id: &str,
    grant: &AdaptiveLeadershipReviewGrantV1,
    context: &AdaptiveLeadershipReviewContextV1,
) -> Result<(), WorkflowError> {
    let source = &receipt.source;
    if grant.schema_version != 3
        || grant.recovery_epoch.is_some()
        || grant.review_id != receipt.review_id
        || operation_id != receipt.review_operation_id
        || allowance_id != receipt.allowance_id
        || canonical_sha256("sentinel.workflow.adaptive-resume-review-grant.v1", grant)?
            != receipt.grant_digest
        || canonical_sha256(
            "sentinel.workflow.adaptive-leadership-context.v1",
            &(grant, context),
        )? != receipt.context_digest
        || context.source_project != source.source_project
        || context.source_session != source.source_session
        || grant.resume_policy.as_deref() != Some(&source.policy.binding(source.next_ordinal)?)
        || grant.leadership_principal != source.leadership_principal
        || grant.leadership_authority != source.leadership_authority
        || grant.expires_at_unix_ms > receipt.request.expires_at_unix_ms
        || context
            .evidence_refs
            .iter()
            .filter(|r| r.starts_with(CORRECTION_PREFIX))
            .count()
            != 1
        || !context.evidence_refs.contains(&receipt.evidence_ref()?)
        || !context
            .evidence_refs
            .contains(&source.accounting.evidence_ref()?)
    {
        return Err(unauthorized());
    }
    Ok(())
}

fn require_projection(
    grant: &AdaptiveLeadershipReviewGrantV1,
    context: &AdaptiveLeadershipReviewContextV1,
) -> Result<(), WorkflowError> {
    let markers: Vec<_> = context
        .evidence_refs
        .iter()
        .filter(|r| r.starts_with(PROJECTION_PREFIX))
        .collect();
    if !markers.is_empty() {
        let binding = grant.resume_policy.as_deref().ok_or_else(unauthorized)?;
        let expected =
            adaptive_accounting_projection(&context.source_session, binding)?.evidence_ref()?;
        if markers.len() != 1 || markers[0] != &expected {
            return Err(unauthorized());
        }
    }
    Ok(())
}

pub(super) fn require_designated_review(
    connection: &Connection,
    call: &AdaptiveLeadershipReviewCallV1,
) -> Result<(), WorkflowError> {
    require_projection(&call.grant, &call.context)?;
    let receipt = read_leaf(
        connection,
        &call.grant.leadership_principal.tenant_id,
        call.grant.session_id,
    )?;
    if let Some(receipt) = receipt {
        if call.grant.review_id == receipt.request.refused_review_id
            && *call != receipt.source.refused_review
        {
            return Err(corrupt());
        }
        if call.grant.review_id == receipt.review_id {
            require_binding(
                &receipt,
                call.operation_id,
                &call.allowance_id,
                &call.grant,
                &call.context,
            )?;
            if call.grant_issued_at_unix_ms != receipt.issued_at_unix_ms {
                return Err(corrupt());
            }
            return Ok(());
        }
    }
    if call
        .context
        .evidence_refs
        .iter()
        .any(|r| r.starts_with(CORRECTION_PREFIX))
    {
        return Err(unauthorized());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn authorized_refusal_exception(
    connection: &Connection,
    existing: &[AdaptiveLeadershipReviewCallV1],
    operation_id: Uuid,
    allowance_id: &str,
    grant: &AdaptiveLeadershipReviewGrantV1,
    context: &AdaptiveLeadershipReviewContextV1,
    now_ms: u64,
) -> Result<Option<Uuid>, WorkflowError> {
    require_projection(grant, context)?;
    let receipt = read_leaf(
        connection,
        &grant.leadership_principal.tenant_id,
        grant.session_id,
    )?;
    if let Some(receipt) = receipt {
        if grant.review_id == receipt.review_id {
            require_binding(&receipt, operation_id, allowance_id, grant, context)?;
            if now_ms != receipt.issued_at_unix_ms || now_ms >= receipt.request.expires_at_unix_ms {
                return Err(transition());
            }
            return Ok(Some(receipt.request.refused_review_id));
        }
        // Failed, expired, or genuinely deferred correction buys no further attempt on any head.
        let designated = existing
            .iter()
            .find(|call| call.grant.review_id == receipt.review_id)
            .ok_or_else(corrupt)?;
        if !matches!(
            designated.decision.as_ref().map(|d| &d.decision),
            Some(AdaptiveLeadershipReviewDecisionKindV1::Continue { .. })
        ) {
            return Err(transition());
        }
    }
    if context
        .evidence_refs
        .iter()
        .any(|r| r.starts_with(CORRECTION_PREFIX))
    {
        return Err(unauthorized());
    }
    Ok(None)
}

#[allow(clippy::too_many_arguments)]
fn fresh_source(
    connection: &Connection,
    operator: &AuthenticatedCompanyPrincipalV1,
    project_id: &ProjectId,
    session_id: Uuid,
    refused_review_id: Uuid,
    current: &RuntimeAuthoritySnapshotV1,
    leader: &AuthenticatedCompanyPrincipalV1,
    leadership_authority: &PrincipalAuthorityV1,
    now_ms: u64,
) -> Result<AdaptiveAccountingReconsiderationSourceV1, WorkflowError> {
    require_resume_policy_operator(operator, &current.tenant_id)?;
    current.validate()?;
    leader.validate()?;
    leadership_authority.validate()?;
    if session_id.is_nil()
        || refused_review_id.is_nil()
        || current.project_id != *project_id
        || leader.tenant_id != current.tenant_id
        || !current.active
    {
        return Err(unauthorized());
    }
    if read_leaf(connection, &current.tenant_id, session_id)?.is_some() {
        return Err(transition());
    }
    let policy = read_resume_policy_leaf(connection, &current.tenant_id, session_id)?
        .ok_or_else(unauthorized)?;
    let calls = calls_for_session(connection, &current.tenant_id, session_id)?;
    let refused = calls
        .iter()
        .find(|call| call.grant.review_id == refused_review_id)
        .ok_or_else(not_found)?;
    if refused.grant.schema_version != 3
        || refused.grant.recovery_epoch.is_some()
        || refused.grant.project_id != *project_id
        || refused.grant.assignee_authority != *current
        || refused.grant.leadership_principal != *leader
        || refused.grant.leadership_authority != *leadership_authority
        || refused.dispatch.is_none()
        || refused.retired_at_unix_ms.is_some()
        || !matches!(
            refused.decision.as_ref().map(|d| &d.decision),
            Some(AdaptiveLeadershipReviewDecisionKindV1::DeferBudget { .. })
        )
        || refused.updated_at_unix_ms > now_ms
        || refused
            .grant
            .resume_policy
            .as_ref()
            .is_none_or(|b| policy.validate_binding(b).is_err())
        || policy.request.source.assignee_authority != *current
        || now_ms < policy.issued_at_unix_ms
        || now_ms >= policy.request.limits.expires_at_unix_ms
        || calls
            .iter()
            .any(|c| c.decision.is_none() && c.retired_at_unix_ms.is_none())
        || calls.iter().any(|c| {
            c.grant.review_id != refused_review_id
                && c.context.source_session == refused.context.source_session
                && c.grant
                    .resume_policy
                    .as_ref()
                    .is_some_and(|b| b.policy_id == policy.policy_id)
                && matches!(
                    c.decision.as_ref().map(|d| &d.decision),
                    Some(
                        AdaptiveLeadershipReviewDecisionKindV1::DeferBudget { .. }
                            | AdaptiveLeadershipReviewDecisionKindV1::KeepUnknown { .. }
                    )
                )
        })
    {
        return Err(transition());
    }
    require_current_source(connection, refused)?;
    require_sealed_event(
        connection,
        &refused.grant.leadership_principal,
        &refused.grant.project_id,
        "adaptive_leadership_review_completed",
        refused.operation_id,
        &canonical_sha256("sentinel.workflow.adaptive-leadership-call.v1", refused)?,
        refused.updated_at_unix_ms,
        refused,
    )?;
    let (session, root_entry_digest, head_entry_digest) =
        crate::store::adaptive::adaptive_resume_journal_source(connection, session_id)?
            .ok_or_else(not_found)?;
    if session != refused.context.source_session {
        return Err(transition());
    }
    let next_ordinal = u16::try_from(calls.len())
        .map_err(|_| corrupt())?
        .checked_add(1)
        .ok_or_else(corrupt)?;
    let accounting = adaptive_accounting_projection(&session, &policy.binding(next_ordinal)?)?;
    if accounting.root_model_calls_remaining == 0
        || accounting.root_tool_calls_remaining == 0
        || accounting.windows_remaining == 0
    {
        return Err(transition());
    }
    let mut history = calls;
    let refused_review = history
        .iter()
        .find(|c| c.grant.review_id == refused_review_id)
        .ok_or_else(corrupt)?
        .clone();
    history.sort_by_key(|c| c.grant.review_id);
    let mut source = AdaptiveAccountingReconsiderationSourceV1 {
        schema_version: 1,
        source_project: refused_review.context.source_project.clone(),
        source_session: session,
        policy,
        refused_review,
        root_entry_digest,
        head_entry_digest,
        review_history_digest: canonical_sha256(
            "sentinel.workflow.adaptive-accounting-review-history.v1",
            &history,
        )?,
        leadership_principal: leader.clone(),
        leadership_authority: leadership_authority.clone(),
        next_ordinal,
        accounting,
        source_digest: String::new(),
        evidence_refs: Vec::new(),
    };
    source.source_digest = source.computed_digest()?;
    source.evidence_refs = vec![source.accounting.evidence_ref()?, source.evidence_ref()?];
    source.evidence_refs.sort();
    Ok(source)
}

impl WorkflowStore {
    /// The daemon supplies freshly resolved runtime and leader authority, never HTTP fields.
    #[allow(clippy::too_many_arguments)]
    pub fn adaptive_accounting_reconsideration_source(
        &self,
        operator: &AuthenticatedCompanyPrincipalV1,
        project_id: &ProjectId,
        session_id: Uuid,
        refused_review_id: Uuid,
        current: &RuntimeAuthoritySnapshotV1,
        leader: &AuthenticatedCompanyPrincipalV1,
        leadership_authority: &PrincipalAuthorityV1,
        now_ms: u64,
    ) -> Result<AdaptiveAccountingReconsiderationSourceV1, WorkflowError> {
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let scope = validation_scope::enter(&transaction)?;
        let source = fresh_source(
            &transaction,
            operator,
            project_id,
            session_id,
            refused_review_id,
            current,
            leader,
            leadership_authority,
            now_ms,
        )?;
        scope.finish()?;
        transaction.commit()?;
        Ok(source)
    }

    /// Receipt, event, ordinary membership, and designated review commit or roll back together.
    #[allow(clippy::too_many_arguments)]
    pub fn authorize_adaptive_accounting_reconsideration(
        &self,
        operator: &AuthenticatedCompanyPrincipalV1,
        request: &AdaptiveAccountingReconsiderationRequestV1,
        current: &RuntimeAuthoritySnapshotV1,
        leadership_authority: &PrincipalAuthorityV1,
        review_operation_id: Uuid,
        allowance_id: &str,
        grant: &AdaptiveLeadershipReviewGrantV1,
        context: &AdaptiveLeadershipReviewContextV1,
        now_ms: u64,
    ) -> Result<(bool, AdaptiveAccountingReconsiderationReceiptV1), WorkflowError> {
        request.validate_shape()?;
        require_resume_policy_operator(operator, &current.tenant_id)?;
        current.validate()?;
        leadership_authority.validate()?;
        let mut normalized = context.clone();
        normalized.evidence_refs.sort();
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let scope = validation_scope::enter(&transaction)?;
        if let Some(prior) = read_leaf(&transaction, &current.tenant_id, request.session_id)? {
            if prior.request != *request || prior.issuer_principal != *operator {
                return Err(WorkflowError::new(
                    WorkflowErrorCode::IdempotencyConflict,
                    false,
                    "accounting reconsideration already issued",
                ));
            }
            require_binding(
                &prior,
                review_operation_id,
                allowance_id,
                grant,
                &normalized,
            )?;
            if *current != prior.source.source_session.grant.authority
                || *leadership_authority != prior.source.leadership_authority
            {
                return Err(unauthorized());
            }
            let call: AdaptiveLeadershipReviewCallV1 = get_entity(
                &transaction,
                &current.tenant_id,
                "adaptive_leadership_review_call",
                &prior.review_id.to_string(),
            )?
            .ok_or_else(corrupt)?;
            require_binding(
                &prior,
                call.operation_id,
                &call.allowance_id,
                &call.grant,
                &call.context,
            )?;
            scope.finish()?;
            return Ok((true, prior));
        }
        if request.expires_at_unix_ms <= now_ms
            || request.expires_at_unix_ms
                > now_ms.saturating_add(ADAPTIVE_ACCOUNTING_RECONSIDERATION_MAX_MS)
            || review_operation_id.is_nil()
            || review_operation_id == request.operation_id
        {
            return Err(invalid("invalid accounting reconsideration issuance"));
        }
        let source = fresh_source(
            &transaction,
            operator,
            &request.project_id,
            request.session_id,
            request.refused_review_id,
            current,
            &grant.leadership_principal,
            leadership_authority,
            now_ms,
        )?;
        if request.source_digest != source.source_digest
            || grant.assignee_authority != *current
            || grant.leadership_authority != *leadership_authority
            || request.expires_at_unix_ms > source.policy.request.limits.expires_at_unix_ms
            || grant.expires_at_unix_ms > request.expires_at_unix_ms
        {
            return Err(transition());
        }
        let operation_used: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM company_events WHERE tenant_id=?1 AND operation_id=?2)",
            params![current.tenant_id.0, request.operation_id.to_string()],
            |row| row.get(0),
        )?;
        if operation_used {
            return Err(transition());
        }
        let receipt = AdaptiveAccountingReconsiderationReceiptV1 {
            schema_version: 1,
            receipt_id: receipt_id(&current.tenant_id, request.session_id)?,
            request: request.clone(),
            source,
            issuer_principal: operator.clone(),
            issued_at_unix_ms: now_ms,
            review_id: grant.review_id,
            review_operation_id,
            allowance_id: allowance_id.to_owned(),
            grant_digest: canonical_sha256(
                "sentinel.workflow.adaptive-resume-review-grant.v1",
                grant,
            )?,
            context_digest: canonical_sha256(
                "sentinel.workflow.adaptive-leadership-context.v1",
                &(grant, &normalized),
            )?,
        };
        receipt.validate_entity()?;
        require_binding(
            &receipt,
            review_operation_id,
            allowance_id,
            grant,
            &normalized,
        )?;
        put_entity(
            &transaction,
            &current.tenant_id,
            KIND,
            &receipt.receipt_id,
            1,
            &receipt,
        )?;
        append_event(
            &transaction,
            operator,
            request.operation_id,
            &request.canonical_digest()?,
            Some(&request.project_id),
            EVENT,
            &receipt,
            now_ms,
        )?;
        Self::authorize_new_leadership_review_in_transaction(
            &transaction,
            &grant.leadership_principal,
            review_operation_id,
            allowance_id,
            grant,
            &normalized,
            now_ms,
        )?;
        scope.finish()?;
        transaction.commit()?;
        Ok((false, receipt))
    }

    pub fn adaptive_accounting_reconsideration(
        &self,
        tenant: &TenantId,
        session_id: Uuid,
    ) -> Result<Option<AdaptiveAccountingReconsiderationReceiptV1>, WorkflowError> {
        tenant.validate()?;
        if session_id.is_nil() {
            return Err(invalid("invalid accounting reconsideration session"));
        }
        let connection = self.connection.lock().map_err(|_| persistence())?;
        read_leaf(&connection, tenant, session_id)
    }
}
