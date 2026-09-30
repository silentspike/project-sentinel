use super::*;
#[path = "adaptive_leadership_recovery.rs"]
mod recovery;
use crate::{
    adaptive_leadership_continuation_audit_id, AdaptiveCursorV1,
    AdaptiveLeadershipAbandonedAllowanceV2, AdaptiveLeadershipReviewCallV1,
    AdaptiveLeadershipReviewContextV1, AdaptiveLeadershipReviewDecisionKindV1,
    AdaptiveLeadershipReviewGrantV1, AdaptiveLeadershipReviewSubjectV2,
    ClaimAdaptiveLeadershipReviewCallV1, CompleteAdaptiveLeadershipReviewCallV1,
    RequestProviderDispatchV1, ADAPTIVE_LEADERSHIP_MAX_REVIEWS,
};

const KIND: &str = "adaptive_leadership_review_call";
const MAX_TENANT_REVIEW_SCAN: usize = 4096;
const ABANDONED_KIND: &str = "adaptive_leadership_abandoned_allowance";
const EXPIRED_CONTINUATION_KIND: &str = "adaptive_leadership_expired_continuation";

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpiredAdaptiveContinuationRetirementV1 {
    schema_version: u16,
    review: AdaptiveLeadershipReviewCallV1,
    result: CompleteAdaptiveLeadershipReviewCallV1,
}

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
    require_planning_policy(connection, call, &project)
}

fn require_planning_policy(
    connection: &Connection,
    call: &AdaptiveLeadershipReviewCallV1,
    project: &ProjectV1,
) -> Result<(), WorkflowError> {
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
        || (call.grant.schema_version == 2
            && (call.context.source_session.grant.provider != planning.grant.provider
                || call.context.source_session.grant.model != planning.grant.model
                || call.context.source_session.grant.catalog_digest
                    != planning.grant.catalog_digest
                || call.context.source_session.grant.max_call_duration_ms
                    > planning.grant.max_duration_ms))
    {
        return Err(unauthorized());
    }
    Ok(())
}

fn validate_continuation(
    call: &AdaptiveLeadershipReviewCallV1,
    decision: &crate::AdaptiveLeadershipReviewDecisionV1,
    request_digest: &str,
    model_response_digest: &str,
    resolution: Option<Uuid>,
    continuation: Option<&crate::adaptive::AdaptiveContinuationAuthorizationV1>,
) -> Result<(), WorkflowError> {
    decision.validate_subject(&call.grant)?;
    let AdaptiveLeadershipReviewDecisionKindV1::Continue {
        additional_model_calls,
        window_ms,
        ..
    } = &decision.decision
    else {
        if continuation.is_some() {
            return Err(unauthorized());
        }
        return Ok(());
    };
    let authorization = continuation.ok_or_else(unauthorized)?;
    authorization.validate()?;
    let source = &call.context.source_session;
    let (expected_source, abandoned) = match &call.grant.subject {
        Some(AdaptiveLeadershipReviewSubjectV2::UnknownModel { effect, .. }) => (
            crate::adaptive::AdaptiveContinuationSourceV1::ModelUnknown,
            Some(effect.clone()),
        ),
        Some(AdaptiveLeadershipReviewSubjectV2::BlockedContinuation {
            reason_code,
            resolution_event_id,
        }) => {
            let source = match resolution_event_id {
                Some(id) => crate::adaptive::AdaptiveContinuationSourceV1::BlockedResolved {
                    reason_code: reason_code.clone(),
                    resolution_event_id: id.clone(),
                },
                None => crate::adaptive::AdaptiveContinuationSourceV1::Blocked {
                    reason_code: reason_code.clone(),
                },
            };
            (source, None)
        }
        None => return Err(unauthorized()),
    };
    let allowance = call.continuation_allowance(
        authorization.issued_at_ms,
        authorization.deadline_ms,
        *additional_model_calls,
    )?;
    if authorization.operation_id != call.operation_id
        || authorization.review_id != call.grant.review_id
        || authorization.session_id != call.grant.session_id
        || authorization.source_session_version != call.grant.expected_session_version
        || authorization.abandoned_model_effect != abandoned
        || authorization.source != expected_source
        || authorization.additional_model_calls != *additional_model_calls
        || *additional_model_calls > source.grant.max_model_calls
        || source
            .model_calls
            .checked_add(*additional_model_calls)
            .is_none_or(|total| total > source.grant.max_model_calls)
        || authorization
            .deadline_ms
            .checked_sub(authorization.issued_at_ms)
            != Some(*window_ms)
        || *window_ms > source.grant.deadline_ms - source.grant.created_at_ms
        || authorization.issued_at_ms
            < call
                .dispatch
                .as_ref()
                .ok_or_else(unauthorized)?
                .dispatched_at_unix_ms
        || authorization.issued_at_ms >= call.grant.expires_at_unix_ms
        || authorization.issued_at_ms < source.active_deadline_ms()
        || authorization.provider_allowance_id
            != crate::domain::stable_domain_id(
                "subscription",
                &call.grant.leadership_principal.tenant_id,
                call.operation_id,
            )?
        || authorization.provider_allowance_id == source.grant.provider_allowance_id
        || authorization.provider_allowance_id == call.allowance_id
        || authorization.provider_authority_digest
            != crate::adaptive_leadership_continuation_provider_authority_digest(
                &allowance,
                &call.grant.assignee_authority,
            )?
        || resolution != Some(authorization.resolution_event_id)
        || authorization.resolution_event_id
            != adaptive_leadership_continuation_audit_id(
                call.grant.review_id,
                request_digest,
                model_response_digest,
                decision,
            )?
    {
        return Err(unauthorized());
    }
    Ok(())
}

fn require_subject_time(
    call: &AdaptiveLeadershipReviewCallV1,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    if call.grant.schema_version == 2
        && (now_ms < call.context.source_session.updated_at_ms
            || (matches!(
                call.grant.subject,
                Some(AdaptiveLeadershipReviewSubjectV2::BlockedContinuation { .. })
            ) && now_ms < call.context.source_session.active_deadline_ms()))
    {
        return Err(transition());
    }
    Ok(())
}

fn commit_continuation_allowance(
    transaction: &Transaction<'_>,
    call: &AdaptiveLeadershipReviewCallV1,
    authorization: &crate::adaptive::AdaptiveContinuationAuthorizationV1,
    result: &CompleteAdaptiveLeadershipReviewCallV1,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    let mut project = call.context.source_project.clone();
    let prior = project
        .subscription_call
        .as_ref()
        .ok_or_else(unauthorized)?;
    require_adaptive_allowance_source(call)?;
    let allowance = call.continuation_allowance(
        authorization.issued_at_ms,
        authorization.deadline_ms,
        authorization.additional_model_calls,
    )?;
    if prior.allowance_id != call.context.source_session.active_provider_allowance_id()
        || prior.grant.work_item_id != call.grant.work_item_id
        || prior.grant.assignment_id != call.grant.assignment_id
        || prior.grant.assignment_version != call.grant.assignee_authority.assignment_version
        || prior.grant.agent_id != call.grant.assignee_authority.agent_id
        || prior.grant.provider != allowance.grant.provider
        || prior.grant.model != allowance.grant.model
        || prior.grant.catalog_digest != allowance.grant.catalog_digest
        || prior.grant.max_duration_ms
            != call
                .context
                .source_session
                .effective_grant()
                .max_call_duration_ms
        || prior.grant.token_policy != allowance.grant.token_policy
        || project
            .abandoned_subscription_calls
            .iter()
            .any(|entry| entry.allowance.allowance_id == allowance.allowance_id)
    {
        return Err(unauthorized());
    }
    let mut completed = call.clone();
    completed.decision = Some(result.decision.clone());
    completed.model_response_digest = Some(result.model_response_digest.clone());
    completed.resolution_event_id = result.resolution_event_id;
    completed.continuation = Some(authorization.clone());
    completed.updated_at_unix_ms = now_ms;
    completed.version = 3;
    let abandoned = AdaptiveLeadershipAbandonedAllowanceV2 {
        schema_version: 2,
        allowance_id: prior.allowance_id.clone(),
        review: completed.clone(),
    };
    abandoned.validate_entity()?;
    if get_entity::<AdaptiveLeadershipAbandonedAllowanceV2>(
        transaction,
        &project.tenant_id,
        ABANDONED_KIND,
        &abandoned.allowance_id,
    )?
    .is_some()
    {
        return Err(transition());
    }
    put_entity(
        transaction,
        &project.tenant_id,
        ABANDONED_KIND,
        &abandoned.allowance_id,
        1,
        &abandoned,
    )?;
    // The exact source journal was verified by the continuation transaction helper.
    // Preserve the undispatched allowance in its own receipt, not legacy dispatch history.
    project.subscription_call = None;
    subscription::grant_governed_continuation(&mut project, &completed, &allowance)?;
    if project.subscription_call.as_ref() != Some(&allowance) {
        return Err(unauthorized());
    }
    project.version = project.version.checked_add(1).ok_or_else(transition)?;
    project.updated_at_unix_ms = now_ms;
    validate_project(&project)?;
    put_entity(
        transaction,
        &project.tenant_id,
        "project",
        &project.project_id.0,
        project.version,
        &project,
    )?;
    let payload = (
        authorization,
        &result.request_digest,
        &result.model_response_digest,
        &result.decision,
        &project,
    );
    let sequence = append_event(
        transaction,
        &call.grant.leadership_principal,
        authorization.operation_id,
        &canonical_sha256(
            "sentinel.workflow.adaptive-leadership-continuation.v1",
            &payload,
        )?,
        Some(&project.project_id),
        "adaptive_leadership_continuation_authorized",
        &payload,
        now_ms,
    )?;
    put_projection(
        transaction,
        &project.tenant_id,
        &project.project_id,
        sequence,
        &project,
    )?;
    Ok(())
}

fn require_adaptive_allowance_source(
    call: &AdaptiveLeadershipReviewCallV1,
) -> Result<(), WorkflowError> {
    let source = &call.context.source_session;
    let prior = call
        .context
        .source_project
        .subscription_call
        .as_ref()
        .ok_or_else(unauthorized)?;
    if call.grant.schema_version != 2
        || prior.dispatch.is_some()
        || prior.allowance_id != source.active_provider_allowance_id()
        || prior.grant.work_item_id != call.grant.work_item_id
        || prior.grant.assignment_id != call.grant.assignment_id
        || prior.grant.assignment_version != call.grant.assignee_authority.assignment_version
        || prior.grant.agent_id != call.grant.assignee_authority.agent_id
        || prior.grant.provider != source.grant.provider
        || prior.grant.model != source.grant.model
        || prior.grant.catalog_digest != source.grant.catalog_digest
        || prior.grant.max_duration_ms != source.effective_grant().max_call_duration_ms
        || prior.grant.expires_at_unix_ms != source.active_deadline_ms()
        || prior.grant.token_policy != call.grant.token_policy
        || crate::adaptive_leadership_continuation_provider_authority_digest(
            prior,
            &call.grant.assignee_authority,
        )? != source.effective_grant().provider_authority_digest
    {
        return Err(unauthorized());
    }
    // Context validation binds either the exact unknown effect/proof refs or the
    // recorded model-result digest and blocked reason/resolution at this journal head.
    call.context.validate(&call.grant)?;
    Ok(())
}

pub(super) fn validate_governed_allowance_receipt(
    receipt: &AdaptiveLeadershipReviewCallV1,
    allowance: &SubscriptionCallAllowanceV1,
) -> Result<(), WorkflowError> {
    receipt.validate_entity()?;
    let authorization = receipt.continuation.as_ref().ok_or_else(unauthorized)?;
    if receipt.version != 3
        || receipt.grant.schema_version != 2
        || receipt.grant.subject.is_none()
        || receipt.retired_at_unix_ms.is_some()
        || *allowance
            != receipt.continuation_allowance(
                authorization.issued_at_ms,
                authorization.deadline_ms,
                authorization.additional_model_calls,
            )?
    {
        return Err(unauthorized());
    }
    Ok(())
}

pub(super) fn validate_persisted_governed_allowance(
    connection: &Connection,
    project: &ProjectV1,
    allowance: &SubscriptionCallAllowanceV1,
) -> Result<(), WorkflowError> {
    let mut statement = connection.prepare(
        "SELECT entity_id FROM company_entities WHERE tenant_id=?1 AND entity_kind=?2 AND json_extract(payload,'$.continuation.provider_allowance_id')=?3 LIMIT 2",
    )?;
    let ids = statement
        .query_map(
            params![project.tenant_id.0, KIND, allowance.allowance_id],
            |row| row.get::<_, String>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    if ids.len() != 1 {
        return Err(corrupt());
    }
    let receipt: AdaptiveLeadershipReviewCallV1 =
        get_entity(connection, &project.tenant_id, KIND, &ids[0])?.ok_or_else(corrupt)?;
    validate_governed_allowance_receipt(&receipt, allowance).map_err(|_| corrupt())?;
    let authorization = receipt.continuation.as_ref().ok_or_else(corrupt)?;
    let prior = receipt
        .context
        .source_project
        .subscription_call
        .as_ref()
        .ok_or_else(corrupt)?;
    let abandoned: AdaptiveLeadershipAbandonedAllowanceV2 = get_entity(
        connection,
        &project.tenant_id,
        ABANDONED_KIND,
        &prior.allowance_id,
    )?
    .ok_or_else(corrupt)?;
    let (session, _) =
        crate::store::adaptive::load(connection, receipt.grant.session_id)?.ok_or_else(corrupt)?;
    crate::store::adaptive::require_head(connection, &session)?;
    if receipt.grant.project_id != project.project_id
        || abandoned.review != receipt
        || session.grant != receipt.context.source_session.grant
        || session
            .continuation
            .as_ref()
            .is_none_or(|state| !state.authorizations.contains(authorization))
    {
        return Err(corrupt());
    }
    Ok(())
}

impl CompanyEntity for AdaptiveLeadershipAbandonedAllowanceV2 {
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        (
            &self.review.grant.leadership_principal.tenant_id,
            ABANDONED_KIND,
            &self.allowance_id,
            1,
        )
    }

    fn validate_entity(&self) -> Result<(), WorkflowError> {
        self.review.validate_entity()?;
        require_adaptive_allowance_source(&self.review)?;
        let prior = self
            .review
            .context
            .source_project
            .subscription_call
            .as_ref()
            .ok_or_else(corrupt)?;
        if self.schema_version != 2
            || self.allowance_id != prior.allowance_id
            || self.review.version != 3
            || self.review.continuation.is_none()
        {
            return Err(corrupt());
        }
        Ok(())
    }
}

impl CompanyEntity for ExpiredAdaptiveContinuationRetirementV1 {
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        (
            &self.review.grant.leadership_principal.tenant_id,
            EXPIRED_CONTINUATION_KIND,
            &self.review.review_key,
            1,
        )
    }

    fn validate_entity(&self) -> Result<(), WorkflowError> {
        self.review.validate_entity()?;
        let authorization = self.result.continuation.as_ref().ok_or_else(corrupt)?;
        if self.schema_version != 1
            || self.review.grant.schema_version != 2
            || self.review.version != 4
            || self.review.retired_at_unix_ms.is_none()
            || self.review.decision.is_some()
            || authorization.deadline_ms > self.review.updated_at_unix_ms
        {
            return Err(corrupt());
        }
        // Validate the exact dispatched proposal, not a completed continuation receipt.
        let mut dispatched = self.review.clone();
        dispatched.retired_at_unix_ms = None;
        dispatched.version = 2;
        dispatched.updated_at_unix_ms = dispatched
            .dispatch
            .as_ref()
            .ok_or_else(corrupt)?
            .dispatched_at_unix_ms;
        dispatched.validate_completion_proposal(&self.result)
    }
}

impl AdaptiveLeadershipReviewCallV1 {
    pub(crate) fn validate_recovery_history_entity(&self) -> Result<(), WorkflowError> {
        <Self as CompanyEntity>::validate_entity(self)
    }

    /// Validate immutable audit inputs before external persistence. This does not
    /// renew time authority or replace the transaction's current-source checks.
    pub fn validate_completion_proposal(
        &self,
        result: &CompleteAdaptiveLeadershipReviewCallV1,
    ) -> Result<(), WorkflowError> {
        self.validate_entity()?;
        validate_digest(&result.request_digest)?;
        validate_digest(&result.model_response_digest)?;
        result.decision.validate(&self.context.evidence_refs)?;
        if result.review_id != self.grant.review_id
            || result.allowance_id != self.allowance_id
            || self.retired_at_unix_ms.is_some()
            || self
                .dispatch
                .as_ref()
                .is_none_or(|dispatch| dispatch.request_digest != result.request_digest)
        {
            return Err(unauthorized());
        }
        validate_continuation(
            self,
            &result.decision,
            &result.request_digest,
            &result.model_response_digest,
            result.resolution_event_id,
            result.continuation.as_ref(),
        )?;
        if (result.decision.resolves_blocked() || result.continuation.is_some())
            != result.resolution_event_id.is_some()
            || result.resolution_event_id.is_some_and(|id| id.is_nil())
        {
            return Err(unauthorized());
        }
        if result.continuation.is_some() {
            require_adaptive_allowance_source(self)?;
            let source = &self.context.source_session.grant;
            if source.provider != self.grant.provider
                || source.model != self.grant.model
                || source.catalog_digest != self.grant.catalog_digest
            {
                return Err(unauthorized());
            }
        }
        if self.decision.is_some()
            && (self.decision.as_ref() != Some(&result.decision)
                || self.model_response_digest.as_ref() != Some(&result.model_response_digest)
                || self.resolution_event_id != result.resolution_event_id
                || self.continuation != result.continuation)
        {
            return Err(WorkflowError::new(
                WorkflowErrorCode::IdempotencyConflict,
                false,
                "adaptive leadership result changed",
            ));
        }
        Ok(())
    }
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
        if self.schema_version != self.grant.schema_version
            || self.review_key != self.grant.review_id.to_string()
            || self.operation_id.is_nil()
            || self.allowance_id == self.grant.review_id.to_string()
            || self.allowance_id == self.context.source_session.grant.provider_allowance_id
            || (self.grant.schema_version == 2
                && self.allowance_id == self.context.source_session.active_provider_allowance_id())
            || self.created_at_unix_ms == 0
            || self.grant_issued_at_unix_ms < self.created_at_unix_ms
            || self.updated_at_unix_ms < self.grant_issued_at_unix_ms
            || self
                .continuation
                .as_ref()
                .is_some_and(|authorization| authorization.issued_at_ms > self.updated_at_unix_ms)
        {
            return Err(corrupt());
        }
        let expected = if self.retired_at_unix_ms.is_some() {
            4
        } else if self.decision.is_some() {
            3
        } else if self.dispatch.is_some() {
            2
        } else {
            1
        };
        if self.version != expected
            || self.decision.is_some() != self.model_response_digest.is_some()
            || self.retired_at_unix_ms.is_some_and(|at| {
                at != self.updated_at_unix_ms
                    || at < self.grant_issued_at_unix_ms
                    || self.decision.is_some()
                    || self.model_response_digest.is_some()
                    || self.resolution_event_id.is_some()
                    || self.continuation.is_some()
            })
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
            let dispatch = self.dispatch.as_ref().ok_or_else(corrupt)?;
            validate_continuation(
                self,
                decision,
                &dispatch.request_digest,
                self.model_response_digest.as_deref().ok_or_else(corrupt)?,
                self.resolution_event_id,
                self.continuation.as_ref(),
            )?;
            if self.dispatch.is_none()
                || (decision.resolves_blocked() || self.continuation.is_some())
                    != self.resolution_event_id.is_some()
                || self.resolution_event_id.is_some_and(|id| id.is_nil())
            {
                return Err(corrupt());
            }
        } else if self.resolution_event_id.is_some() || self.continuation.is_some() {
            return Err(corrupt());
        }
        require_subject_time(self, self.grant_issued_at_unix_ms)?;
        Ok(())
    }

    fn validate_persisted(&self, connection: &Connection) -> Result<(), WorkflowError> {
        WorkflowStore::require_recovery_epoch_review(connection, self)
    }
}

impl WorkflowStore {
    pub fn adaptive_leadership_abandoned_allowance(
        &self,
        tenant: &TenantId,
        allowance_id: &str,
    ) -> Result<Option<AdaptiveLeadershipAbandonedAllowanceV2>, WorkflowError> {
        tenant.validate()?;
        validate_identifier(allowance_id)?;
        let connection = self.connection.lock().map_err(|_| persistence())?;
        let Some(abandoned) = get_entity::<AdaptiveLeadershipAbandonedAllowanceV2>(
            &connection,
            tenant,
            ABANDONED_KIND,
            allowance_id,
        )?
        else {
            return Ok(None);
        };
        let review: AdaptiveLeadershipReviewCallV1 =
            get_entity(&connection, tenant, KIND, &abandoned.review.review_key)?
                .ok_or_else(corrupt)?;
        let (session, _) = crate::store::adaptive::load(&connection, review.grant.session_id)?
            .ok_or_else(corrupt)?;
        let authorization = review.continuation.as_ref().ok_or_else(corrupt)?;
        if review != abandoned.review
            || session.grant != review.context.source_session.grant
            || session
                .continuation
                .as_ref()
                .is_none_or(|state| !state.authorizations.contains(authorization))
        {
            return Err(corrupt());
        }
        Ok(Some(abandoned))
    }

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
                || prior.grant.schema_version == 2
                || prior.decision.is_some()
                || prior.retired_at_unix_ms.is_some()
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
        if grant.recovery_epoch.is_some() {
            return Err(unauthorized());
        }
        grant.validate(now_ms)?;
        context.validate(grant)?;
        let existing = calls_for_session(&transaction, &leader.tenant_id, grant.session_id)?;
        let same_head: Vec<_> = existing
            .iter()
            .filter(|call| call.grant.expected_session_version == grant.expected_session_version)
            .collect();
        if same_head.len() >= ADAPTIVE_LEADERSHIP_MAX_REVIEWS
            || (grant.schema_version == 2
                && existing
                    .iter()
                    .filter(|call| call.grant.schema_version == 2)
                    .count()
                    >= ADAPTIVE_LEADERSHIP_MAX_REVIEWS)
            || same_head
                .iter()
                .any(|call| call.decision.is_none() && call.retired_at_unix_ms.is_none())
        {
            return Err(transition());
        }
        let call = AdaptiveLeadershipReviewCallV1 {
            schema_version: grant.schema_version,
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
            retired_at_unix_ms: None,
            continuation: None,
        };
        require_subject_time(&call, now_ms)?;
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

    /// Retire stale input or an expired undispatched schema2 subject without releasing authority.
    pub fn retire_stale_adaptive_leadership_review_call(
        &self,
        leader: &AuthenticatedCompanyPrincipalV1,
        review_id: Uuid,
        expected_version: u64,
        now_ms: u64,
    ) -> Result<AdaptiveLeadershipReviewCallV1, WorkflowError> {
        self.retire_adaptive_leadership_review_call(
            leader,
            review_id,
            expected_version,
            now_ms,
            false,
        )
    }

    /// Explicit schema2 expiry after scheduler or durable provider-completion verification.
    /// Keeps dispatch, usage history and allowance; never fabricates a model decision.
    pub fn expire_adaptive_leadership_review_call(
        &self,
        leader: &AuthenticatedCompanyPrincipalV1,
        review_id: Uuid,
        expected_version: u64,
        now_ms: u64,
    ) -> Result<AdaptiveLeadershipReviewCallV1, WorkflowError> {
        self.retire_adaptive_leadership_review_call(
            leader,
            review_id,
            expected_version,
            now_ms,
            true,
        )
    }

    /// Retire an exact daemon-verified Limbo audit whose original continuation window expired.
    /// Records termination only: no model decision, fresh allowance, adoption or refund.
    pub fn retire_expired_adaptive_continuation_call(
        &self,
        leader: &AuthenticatedCompanyPrincipalV1,
        result: &CompleteAdaptiveLeadershipReviewCallV1,
        now_ms: u64,
    ) -> Result<AdaptiveLeadershipReviewCallV1, WorkflowError> {
        leader.validate()?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut call: AdaptiveLeadershipReviewCallV1 = get_entity(
            &transaction,
            &leader.tenant_id,
            KIND,
            &result.review_id.to_string(),
        )?
        .ok_or_else(not_found)?;
        if leader != &call.grant.leadership_principal || call.grant.schema_version != 2 {
            return Err(unauthorized());
        }
        if call.decision.is_some() || now_ms < call.updated_at_unix_ms {
            return Err(transition());
        }
        let authorization = result.continuation.as_ref().ok_or_else(unauthorized)?;
        if now_ms < authorization.deadline_ms {
            return Err(transition());
        }
        let recorded: Option<ExpiredAdaptiveContinuationRetirementV1> = get_entity(
            &transaction,
            &leader.tenant_id,
            EXPIRED_CONTINUATION_KIND,
            &call.review_key,
        )?;
        if call.retired_at_unix_ms.is_some() {
            let recorded = recorded.ok_or_else(transition)?;
            if recorded.review != call || recorded.result != *result {
                return Err(unauthorized());
            }
        } else {
            if recorded.is_some() {
                return Err(corrupt());
            }
            call.validate_completion_proposal(result)?;
        }
        let (session, _) = crate::store::adaptive::load(&transaction, call.grant.session_id)?
            .ok_or_else(not_found)?;
        crate::store::adaptive::require_head(&transaction, &session)?;
        if session.continuation.as_ref().is_some_and(|state| {
            state
                .authorizations
                .iter()
                .any(|entry| entry.review_id == call.grant.review_id)
        }) {
            return Err(transition());
        }
        if call.retired_at_unix_ms.is_some() {
            return Ok(call);
        }
        call.retired_at_unix_ms = Some(now_ms);
        call.version = 4;
        call.updated_at_unix_ms = now_ms;
        let retirement = ExpiredAdaptiveContinuationRetirementV1 {
            schema_version: 1,
            review: call.clone(),
            result: result.clone(),
        };
        retirement.validate_entity()?;
        put_entity(
            &transaction,
            &leader.tenant_id,
            EXPIRED_CONTINUATION_KIND,
            &call.review_key,
            1,
            &retirement,
        )?;
        store_call(&transaction, &call, "adaptive_leadership_review_retired")?;
        transaction.commit()?;
        Ok(call)
    }

    fn retire_adaptive_leadership_review_call(
        &self,
        leader: &AuthenticatedCompanyPrincipalV1,
        review_id: Uuid,
        expected_version: u64,
        now_ms: u64,
        explicit_expiry: bool,
    ) -> Result<AdaptiveLeadershipReviewCallV1, WorkflowError> {
        leader.validate()?;
        if review_id.is_nil() {
            return Err(invalid("invalid leadership review identity"));
        }
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut call: AdaptiveLeadershipReviewCallV1 = get_entity(
            &transaction,
            &leader.tenant_id,
            KIND,
            &review_id.to_string(),
        )?
        .ok_or_else(not_found)?;
        if leader != &call.grant.leadership_principal {
            return Err(unauthorized());
        }
        if explicit_expiry
            && (call.grant.schema_version != 2
                || call.grant.subject.is_none()
                || now_ms < call.grant.expires_at_unix_ms)
        {
            return Err(transition());
        }
        if call.retired_at_unix_ms.is_some() {
            let source_version = if call.dispatch.is_some() { 2 } else { 1 };
            if (explicit_expiry && expected_version != source_version && expected_version != 4)
                || (!explicit_expiry && !matches!(expected_version, 1 | 2 | 4))
            {
                return Err(transition());
            }
            return Ok(call);
        }
        if call.version != expected_version
            || call.decision.is_some()
            || now_ms < call.updated_at_unix_ms
        {
            return Err(transition());
        }
        let project: ProjectV1 = get_entity(
            &transaction,
            &leader.tenant_id,
            "project",
            &call.grant.project_id.0,
        )?
        .ok_or_else(not_found)?;
        let (session, _) = crate::store::adaptive::load(&transaction, call.grant.session_id)?
            .ok_or_else(not_found)?;
        crate::store::adaptive::require_head(&transaction, &session)?;
        // A committed resolution needs receipt recovery, not retirement.
        if session.continuation.as_ref().is_some_and(|state| {
            state
                .authorizations
                .iter()
                .any(|authorization| authorization.review_id == call.grant.review_id)
        }) {
            return Err(transition());
        }
        if session.version
            == call
                .grant
                .expected_session_version
                .checked_add(1)
                .ok_or_else(transition)?
            && session.grant == call.context.source_session.grant
            && matches!(session.cursor, AdaptiveCursorV1::BlockedResolved { .. })
        {
            return Err(transition());
        }
        let expired_subject = call.grant.schema_version == 2
            && call.grant.subject.is_some()
            && (call.dispatch.is_none() || explicit_expiry)
            && now_ms >= call.grant.expires_at_unix_ms;
        if project == call.context.source_project
            && session == call.context.source_session
            && !expired_subject
        {
            return Err(transition());
        }
        call.retired_at_unix_ms = Some(now_ms);
        call.version = 4;
        call.updated_at_unix_ms = now_ms;
        store_call(&transaction, &call, "adaptive_leadership_review_retired")?;
        transaction.commit()?;
        Ok(call)
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
            || call.retired_at_unix_ms.is_some()
            || claim.context_digest != call.context_digest()?
            || now_ms < call.updated_at_unix_ms
            || now_ms >= call.grant.expires_at_unix_ms
        {
            return Err(unauthorized());
        }
        require_current_source(&transaction, &call)?;
        require_subject_time(&call, now_ms)?;
        recovery::require_epoch_time(&transaction, &call, now_ms)?;
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
        call.validate_completion_proposal(result)?;
        if leader != &call.grant.leadership_principal {
            return Err(unauthorized());
        }
        if call.decision.is_some() {
            return Ok(call);
        }
        recovery::require_epoch_time(
            &transaction,
            &call,
            result
                .continuation
                .as_ref()
                .map_or(now_ms, |authorization| authorization.issued_at_ms),
        )?;
        recovery::require_epoch_completion(&transaction, &call, result)?;
        if call.grant.schema_version == 2
            && result.continuation.is_none()
            && now_ms >= call.grant.expires_at_unix_ms
        {
            return Err(transition());
        }
        if now_ms < call.updated_at_unix_ms {
            return Err(transition());
        }
        if let Some(authorization) = &result.continuation {
            if now_ms < authorization.issued_at_ms || now_ms >= authorization.deadline_ms {
                return Err(transition());
            }
            let project: ProjectV1 = get_entity(
                &transaction,
                &leader.tenant_id,
                "project",
                &call.grant.project_id.0,
            )?
            .ok_or_else(not_found)?;
            if project != call.context.source_project {
                return Err(transition());
            }
            require_planning_policy(&transaction, &call, &project)?;
            require_subject_time(&call, now_ms)?;
            // All effects share this transaction: no independent receipt or allowance grant.
            let allowance = call.continuation_allowance(
                authorization.issued_at_ms,
                authorization.deadline_ms,
                authorization.additional_model_calls,
            )?;
            let (_, continued) = crate::store::adaptive::continue_adaptive_session_in_transaction(
                &transaction,
                authorization,
                &call,
                &allowance,
                &call.grant.assignee_authority,
                now_ms,
            )?;
            let (current, _) = crate::store::adaptive::load(&transaction, call.grant.session_id)?
                .ok_or_else(not_found)?;
            if current != continued {
                return Err(transition());
            }
            commit_continuation_allowance(&transaction, &call, authorization, result, now_ms)?;
        } else if result.decision.resolves_blocked() {
            let resolution = result.resolution_event_id.ok_or_else(transition)?;
            let mut project: ProjectV1 = get_entity(
                &transaction,
                &leader.tenant_id,
                "project",
                &call.grant.project_id.0,
            )?
            .ok_or_else(not_found)?;
            // Receipt recovery permits append-only discussion, never changed work authority.
            if !project
                .decisions
                .starts_with(&call.context.source_project.decisions)
            {
                return Err(transition());
            }
            project.decisions = call.context.source_project.decisions.clone();
            project.version = call.context.source_project.version;
            project.updated_at_unix_ms = call.context.source_project.updated_at_unix_ms;
            if project != call.context.source_project {
                return Err(transition());
            }
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
        call.continuation = result.continuation.clone();
        call.version = 3;
        call.updated_at_unix_ms = now_ms;
        store_call(&transaction, &call, "adaptive_leadership_review_completed")?;
        transaction.commit()?;
        Ok(call)
    }
}

#[cfg(test)]
mod tests {
    include!("adaptive_leadership_review/tests.rs");
    mod recovery_tests {
        include!("adaptive_leadership_review/recovery_tests.rs");
    }

    const CONTINUATION_AT: u64 = 900_002;

    fn continuation_fixture(unknown: bool, resolved: bool) -> Fixture {
        continuation_fixture_with_calls(unknown, resolved, 4)
    }

    fn continuation_fixture_with_calls(
        unknown: bool,
        resolved: bool,
        max_model_calls: u16,
    ) -> Fixture {
        let mut f = fixture();
        resolve(&f, Uuid::new_v4(), 21);
        let mut project = f.context.source_project.clone();
        let mut session_grant = f.context.source_session.grant.clone();
        session_grant.session_id = Uuid::new_v4();
        session_grant.max_model_calls = max_model_calls;
        session_grant.max_tool_calls = max_model_calls;
        session_grant.created_at_ms = 600_001;
        session_grant.deadline_ms = 900_001;
        let original = crate::SubscriptionCallGrantV1 {
            schema_version: 1,
            work_item_id: f.grant.work_item_id.clone(),
            assignment_id: f.grant.assignment_id.clone(),
            assignment_version: f.grant.assignee_authority.assignment_version,
            agent_id: f.grant.assignee_authority.agent_id,
            provider: f.grant.provider.clone(),
            model: f.grant.model.clone(),
            catalog_digest: f.grant.catalog_digest.clone(),
            max_calls: max_model_calls,
            max_concurrent: 1,
            max_duration_ms: 120_000,
            token_policy: f.grant.token_policy,
            expires_at_unix_ms: session_grant.deadline_ms,
        };
        let response = f
            .store
            .apply_company_command(
                &f.leader,
                Uuid::new_v4(),
                &CompanyWorkflowCommandV1::GrantSubscriptionCall {
                    project_id: project.project_id.clone(),
                    expected_version: project.version,
                    grant: original,
                },
                600_001,
            )
            .unwrap()
            .response;
        let CompanyWorkflowResponseV1::Project(updated) = response else {
            panic!("project");
        };
        project = *updated;
        let allowance = project.subscription_call.as_ref().unwrap();
        session_grant.provider_allowance_id = allowance.allowance_id.clone();
        session_grant.provider_authority_digest =
            adaptive_leadership_continuation_provider_authority_digest(
                allowance,
                &f.grant.assignee_authority,
            )
            .unwrap();
        let (_, initial) = f
            .store
            .begin_adaptive_session(&session_grant, &f.grant.assignee_authority, 600_001)
            .unwrap();
        assert!(project
            .subscription_call
            .as_ref()
            .unwrap()
            .dispatch
            .is_none());
        let effect = AdaptiveEffectV1 {
            id: Uuid::new_v4(),
            request_digest: DIGEST.into(),
        };
        let (_, pending) = f
            .store
            .advance_adaptive_session(
                session_grant.session_id,
                initial.version,
                Uuid::new_v4(),
                &AdaptiveTransitionV1::ClaimModel {
                    effect: effect.clone(),
                    previous_observation_digest: None,
                },
                &f.grant.assignee_authority,
                600_003,
            )
            .unwrap();
        let transition = if unknown {
            AdaptiveTransitionV1::MarkUnknown {
                effect: effect.clone(),
            }
        } else {
            AdaptiveTransitionV1::ResolveModel {
                effect: effect.clone(),
                result_digest: DIGEST.into(),
                decision: AdaptiveModelDecisionV1::Blocked {
                    reason_code: REASON.into(),
                },
            }
        };
        let (_, mut source) = f
            .store
            .advance_adaptive_session(
                session_grant.session_id,
                pending.version,
                Uuid::new_v4(),
                &transition,
                &f.grant.assignee_authority,
                600_004,
            )
            .unwrap();
        let resolution_event_id = if resolved {
            let id = Uuid::new_v4().to_string();
            source = f
                .store
                .advance_adaptive_session(
                    session_grant.session_id,
                    source.version,
                    Uuid::new_v4(),
                    &AdaptiveTransitionV1::ResolveBlocked {
                        expected_reason_code: REASON.into(),
                        resolution_event_id: id.clone(),
                    },
                    &f.grant.assignee_authority,
                    600_005,
                )
                .unwrap()
                .1;
            Some(id)
        } else {
            None
        };
        f.context.source_project = project.clone();
        f.context.source_session = source.clone();
        f.context.evidence_refs = if unknown {
            vec![
                format!(
                    "adaptive-model-unknown:{}:{}",
                    effect.id, effect.request_digest
                ),
                format!("sealed-provider-unknown:{DIGEST}"),
            ]
        } else {
            vec![format!("adaptive-model-result:{DIGEST}")]
        };
        f.grant.schema_version = 2;
        f.grant.session_id = session_grant.session_id;
        f.grant.expected_session_version = source.version;
        f.grant.expected_project_version = project.version;
        f.grant.expected_reason_code = if unknown {
            String::new()
        } else {
            REASON.into()
        };
        f.grant.subject = Some(if unknown {
            AdaptiveLeadershipReviewSubjectV2::UnknownModel {
                effect,
                sealed_unknown_proof_digest: DIGEST.into(),
            }
        } else {
            AdaptiveLeadershipReviewSubjectV2::BlockedContinuation {
                reason_code: REASON.into(),
                resolution_event_id,
            }
        });
        f.grant.evidence_fingerprint = adaptive_leadership_evidence_fingerprint(
            &f.context.tool_catalog,
            &f.context.evidence_refs,
        )
        .unwrap();
        f.grant.review_id = adaptive_leadership_review_id(
            f.grant.session_id,
            f.grant.expected_session_version,
            &f.grant.evidence_fingerprint,
        )
        .unwrap();
        f.grant.expires_at_unix_ms = CONTINUATION_AT + 120_000;
        f
    }

    fn dispatch_continuation(f: &Fixture) -> AdaptiveLeadershipReviewCallV1 {
        let call = f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                Uuid::new_v4(),
                "continuation-review",
                &f.grant,
                &f.context,
                CONTINUATION_AT,
            )
            .unwrap();
        f.store
            .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), CONTINUATION_AT + 1)
            .unwrap()
    }

    fn continue_result(
        call: &AdaptiveLeadershipReviewCallV1,
    ) -> CompleteAdaptiveLeadershipReviewCallV1 {
        let mut result = completion(call, false);
        result.decision = AdaptiveLeadershipReviewDecisionV1 {
            schema_version: 2,
            decision: AdaptiveLeadershipReviewDecisionKindV1::Continue {
                additional_model_calls: 1,
                window_ms: 120_000,
                rationale: "Continue with the remaining original budget".into(),
                evidence_refs: vec![call.context.evidence_refs[0].clone()],
            },
        };
        let audit = adaptive_leadership_continuation_audit_id(
            call.grant.review_id,
            &result.request_digest,
            &result.model_response_digest,
            &result.decision,
        )
        .unwrap();
        let allowance = call
            .continuation_allowance(CONTINUATION_AT + 2, CONTINUATION_AT + 120_002, 1)
            .unwrap();
        let (source, abandoned_model_effect) = match &call.grant.subject {
            Some(AdaptiveLeadershipReviewSubjectV2::UnknownModel { effect, .. }) => (
                crate::adaptive::AdaptiveContinuationSourceV1::ModelUnknown,
                Some(effect.clone()),
            ),
            Some(AdaptiveLeadershipReviewSubjectV2::BlockedContinuation {
                reason_code,
                resolution_event_id,
            }) => {
                let source = match resolution_event_id {
                    Some(id) => crate::adaptive::AdaptiveContinuationSourceV1::BlockedResolved {
                        reason_code: reason_code.clone(),
                        resolution_event_id: id.clone(),
                    },
                    None => crate::adaptive::AdaptiveContinuationSourceV1::Blocked {
                        reason_code: reason_code.clone(),
                    },
                };
                (source, None)
            }
            None => panic!("continuation subject"),
        };
        result.resolution_event_id = Some(audit);
        result.continuation = Some(crate::adaptive::AdaptiveContinuationAuthorizationV1 {
            schema_version: 1,
            operation_id: call.operation_id,
            review_id: call.grant.review_id,
            resolution_event_id: audit,
            session_id: call.grant.session_id,
            source_session_version: call.grant.expected_session_version,
            source,
            abandoned_model_effect,
            provider_allowance_id: allowance.allowance_id.clone(),
            provider_authority_digest: adaptive_leadership_continuation_provider_authority_digest(
                &allowance,
                &call.grant.assignee_authority,
            )
            .unwrap(),
            issued_at_ms: CONTINUATION_AT + 2,
            deadline_ms: CONTINUATION_AT + 120_002,
            additional_model_calls: 1,
        });
        result
    }

    #[test]
    fn schema2_continuation_commits_same_session_allowance_and_receipt_without_budget_reset() {
        for (unknown, resolved) in [(true, false), (false, false), (false, true)] {
            let f = continuation_fixture(unknown, resolved);
            let call = dispatch_continuation(&f);
            let result = continue_result(&call);
            let completed = f
                .store
                .complete_adaptive_leadership_review_call(&f.leader, &result, CONTINUATION_AT + 2)
                .unwrap();
            assert_eq!(completed.continuation, result.continuation);
            let current = session(&f);
            assert_eq!(current.grant, f.context.source_session.grant);
            assert_eq!(current.model_calls, f.context.source_session.model_calls);
            assert_eq!(current.tool_calls, f.context.source_session.tool_calls);
            assert_eq!(current.version, f.context.source_session.version + 1);
            assert_eq!(current.cursor, AdaptiveCursorV1::ReadyForModel);
            assert_eq!(current.active_model_ceiling(), current.model_calls + 1);
            let project = f
                .store
                .company_project(&f.leader.tenant_id, &f.grant.project_id)
                .unwrap()
                .unwrap();
            assert_eq!(
                project.subscription_call.as_ref().unwrap(),
                &call
                    .continuation_allowance(CONTINUATION_AT + 2, CONTINUATION_AT + 120_002, 1)
                    .unwrap()
            );
            assert!(project.abandoned_subscription_calls.is_empty());
            let abandoned = f
                .store
                .adaptive_leadership_abandoned_allowance(
                    &f.leader.tenant_id,
                    &f.context.source_session.grant.provider_allowance_id,
                )
                .unwrap()
                .unwrap();
            assert_eq!(abandoned.review, completed);
            assert!(abandoned
                .review
                .context
                .source_project
                .subscription_call
                .as_ref()
                .unwrap()
                .dispatch
                .is_none());
            let before = rows(&f.store);
            assert_eq!(
                f.store
                    .complete_adaptive_leadership_review_call(
                        &f.leader,
                        &result,
                        CONTINUATION_AT + 900_000
                    )
                    .unwrap(),
                completed
            );
            assert_eq!(rows(&f.store), before);
            let mut changed = result.clone();
            changed.model_response_digest = "d".repeat(64);
            assert!(f
                .store
                .complete_adaptive_leadership_review_call(&f.leader, &changed, CONTINUATION_AT + 3)
                .is_err());
            assert_eq!(rows(&f.store), before);
        }
    }

    #[test]
    fn schema2_keep_unknown_records_only_review_and_cannot_carry_continuation() {
        let f = continuation_fixture(true, false);
        let call = dispatch_continuation(&f);
        let mut result = completion(&call, false);
        result.decision = AdaptiveLeadershipReviewDecisionV1 {
            schema_version: 2,
            decision: AdaptiveLeadershipReviewDecisionKindV1::KeepUnknown {
                rationale: "Retain the unresolved model effect".into(),
                evidence_refs: vec![f.context.evidence_refs[0].clone()],
            },
        };
        let before = session(&f);
        let project = f.context.source_project.clone();
        f.store
            .complete_adaptive_leadership_review_call(&f.leader, &result, CONTINUATION_AT + 2)
            .unwrap();
        assert_eq!(session(&f), before);
        assert_eq!(
            f.store
                .company_project(&f.leader.tenant_id, &f.grant.project_id)
                .unwrap()
                .unwrap(),
            project
        );
        let durable = rows(&f.store);
        result.continuation = continue_result(&call).continuation;
        assert!(f
            .store
            .complete_adaptive_leadership_review_call(&f.leader, &result, CONTINUATION_AT + 3)
            .is_err());
        assert_eq!(rows(&f.store), durable);
    }

    #[test]
    fn schema2_allowance_duration_is_capped_by_exact_window_without_effects() {
        let f = continuation_fixture(true, false);
        let call = dispatch_continuation(&f);
        let before = rows(&f.store);
        let issued = CONTINUATION_AT + 2;
        for (window, expected) in [
            (1_000, 1_000),
            (60_000, 60_000),
            (120_000, 120_000),
            (300_000, 120_000),
        ] {
            let allowance = call
                .continuation_allowance(issued, issued + window, 1)
                .unwrap();
            assert_eq!(allowance.grant.max_duration_ms, expected);
            assert_eq!(allowance.grant.max_calls, 1);
            assert_eq!(allowance.grant.expires_at_unix_ms, issued + window);
        }
        for deadline in [issued - 1, issued, issued + 999, issued + 300_001] {
            assert!(call.continuation_allowance(issued, deadline, 1).is_err());
        }
        assert!(call.continuation_allowance(0, 1_000, 1).is_err());
        assert!(call.continuation_allowance(u64::MAX, 1_000, 1).is_err());
        assert!(call
            .continuation_allowance(issued, issued + 1_000, 0)
            .is_err());
        assert_eq!(rows(&f.store), before);
    }

    fn continue_result_with_window(
        call: &AdaptiveLeadershipReviewCallV1,
        window: u64,
    ) -> CompleteAdaptiveLeadershipReviewCallV1 {
        continue_result_with_window_at(call, window, CONTINUATION_AT + 2)
    }

    fn continue_result_with_window_at(
        call: &AdaptiveLeadershipReviewCallV1,
        window: u64,
        issued: u64,
    ) -> CompleteAdaptiveLeadershipReviewCallV1 {
        let mut result = continue_result(call);
        if let AdaptiveLeadershipReviewDecisionKindV1::Continue { window_ms, .. } =
            &mut result.decision.decision
        {
            *window_ms = window;
        }
        let audit = adaptive_leadership_continuation_audit_id(
            call.grant.review_id,
            &result.request_digest,
            &result.model_response_digest,
            &result.decision,
        )
        .unwrap();
        let authorization = result.continuation.as_mut().unwrap();
        authorization.issued_at_ms = issued;
        authorization.deadline_ms = authorization.issued_at_ms + window;
        authorization.resolution_event_id = audit;
        let allowance = call
            .continuation_allowance(authorization.issued_at_ms, authorization.deadline_ms, 1)
            .unwrap();
        authorization.provider_authority_digest =
            adaptive_leadership_continuation_provider_authority_digest(
                &allowance,
                &call.grant.assignee_authority,
            )
            .unwrap();
        result.resolution_event_id = Some(audit);
        result
    }

    #[test]
    fn schema2_short_window_commits_a_single_call_with_matching_effective_duration() {
        for (unknown, resolved) in [(true, false), (false, false), (false, true)] {
            for window in [1_000, 1_001, 60_000, 119_999, 120_000, 300_000] {
                let f = continuation_fixture(unknown, resolved);
                let call = dispatch_continuation(&f);
                let result = continue_result_with_window(&call, window);
                call.validate_completion_proposal(&result).unwrap();
                let authorization = result.continuation.as_ref().unwrap();
                let allowance = call
                    .continuation_allowance(
                        authorization.issued_at_ms,
                        authorization.deadline_ms,
                        1,
                    )
                    .unwrap();
                let completed = f
                    .store
                    .complete_adaptive_leadership_review_call(
                        &f.leader,
                        &result,
                        authorization.issued_at_ms + 7,
                    )
                    .unwrap();
                let current = session(&f);
                assert_eq!(current.grant, f.context.source_session.grant);
                assert_eq!(current.model_calls, f.context.source_session.model_calls);
                assert_eq!(current.tool_calls, f.context.source_session.tool_calls);
                assert_eq!(current.active_model_ceiling(), current.model_calls + 1);
                assert_eq!(
                    current.effective_grant().max_call_duration_ms,
                    window.min(120_000)
                );
                assert!(current.requires_fresh_observation());
                let reopened = WorkflowStore::open(&f.path).unwrap();
                let project = reopened
                    .company_project(&f.leader.tenant_id, &f.grant.project_id)
                    .unwrap()
                    .unwrap();
                assert_eq!(project.subscription_call.as_ref(), Some(&allowance));
                assert_eq!(allowance.grant.max_calls, 1);
                assert_eq!(allowance.grant.max_duration_ms, window.min(120_000));
                assert!(reopened.company_projects().unwrap().contains(&project));
                assert_eq!(
                    reopened
                        .adaptive_leadership_abandoned_allowance(
                            &f.leader.tenant_id,
                            f.context.source_session.active_provider_allowance_id()
                        )
                        .unwrap()
                        .unwrap()
                        .review,
                    completed
                );
                let before = rows(&f.store);
                assert_eq!(
                    reopened
                        .complete_adaptive_leadership_review_call(
                            &f.leader,
                            &result,
                            authorization.deadline_ms + 1
                        )
                        .unwrap(),
                    completed
                );
                assert_eq!(rows(&f.store), before);
            }
        }
    }

    #[test]
    fn ordinary_short_grants_and_unfinished_governed_receipts_are_rejected_without_effects() {
        let f = continuation_fixture(true, false);
        let call = dispatch_continuation(&f);
        let issued = CONTINUATION_AT + 2;
        for window in [1_000, 60_000, 119_999] {
            let allowance = call
                .continuation_allowance(issued, issued + window, 1)
                .unwrap();
            let mut candidate = f.context.source_project.clone();
            candidate.subscription_call = None;
            let before = candidate.clone();
            assert!(subscription::grant(
                &mut candidate,
                &f.leader,
                call.operation_id,
                &allowance.grant,
                issued
            )
            .is_err());
            assert_eq!(candidate, before);
            assert!(
                subscription::grant_governed_continuation(&mut candidate, &call, &allowance)
                    .is_err()
            );
            assert_eq!(candidate, before);
            candidate.subscription_call = Some(allowance.clone());
            let before_claim = candidate.clone();
            let mut employee = f.leader.clone();
            employee.role = CompanyRoleV1::Developer;
            employee.agent_id = Some(allowance.grant.agent_id);
            assert!(subscription::claim(
                &mut candidate,
                &employee,
                &allowance.allowance_id,
                &format!("company-provider-{}", allowance.allowance_id),
                DIGEST,
                issued
            )
            .is_err());
            assert_eq!(candidate, before_claim);
            let durable = rows(&f.store);
            assert!(f
                .store
                .apply_company_command(
                    &f.leader,
                    Uuid::new_v4(),
                    &CompanyWorkflowCommandV1::GrantSubscriptionCall {
                        project_id: f.grant.project_id.clone(),
                        expected_version: f.context.source_project.version,
                        grant: allowance.grant,
                    },
                    issued
                )
                .is_err());
            assert_eq!(rows(&f.store), durable);
        }
    }

    #[test]
    fn persisted_short_allowance_requires_exact_completed_receipt_and_journal() {
        for committed in [false, true] {
            let f = continuation_fixture(true, false);
            let call = dispatch_continuation(&f);
            let result = continue_result_with_window(&call, 60_000);
            if committed {
                f.store
                    .complete_adaptive_leadership_review_call(
                        &f.leader,
                        &result,
                        CONTINUATION_AT + 2,
                    )
                    .unwrap();
            }
            let mut project = f
                .store
                .company_project(&f.leader.tenant_id, &f.grant.project_id)
                .unwrap()
                .unwrap();
            let authorization = result.continuation.as_ref().unwrap();
            project.subscription_call = Some(
                call.continuation_allowance(
                    authorization.issued_at_ms,
                    authorization.deadline_ms,
                    1,
                )
                .unwrap(),
            );
            if committed {
                project
                    .subscription_call
                    .as_mut()
                    .unwrap()
                    .grant
                    .max_duration_ms -= 1;
            }
            project.updated_at_unix_ms = CONTINUATION_AT + 2;
            project.version += 1;
            // The production writer seals the forged row; receipt validation must still reject it.
            persist_entity(&f.store, &project);
            let before = rows(&f.store);
            assert!(f
                .store
                .company_project(&f.leader.tenant_id, &f.grant.project_id)
                .is_err());
            assert!(f.store.company_projects().is_err());
            assert_eq!(rows(&f.store), before);
        }
    }

    #[test]
    fn schema2_audit_gap_completion_keeps_original_authorization_clock_and_deadline() {
        // Domain-side retry of a daemon-verified audit; no Limbo persistence is simulated here.
        for (unknown, expired) in [(true, false), (false, false), (true, true)] {
            let f = continuation_fixture(unknown, false);
            let call = dispatch_continuation(&f);
            let result = continue_result(&call);
            let authorization = result.continuation.as_ref().unwrap();
            call.validate_completion_proposal(&result).unwrap();
            let now = if expired {
                authorization.deadline_ms
            } else {
                authorization.issued_at_ms + 7
            };
            let before = rows(&f.store);
            let completed = f
                .store
                .complete_adaptive_leadership_review_call(&f.leader, &result, now);
            if expired {
                assert!(completed.is_err());
                assert_eq!(rows(&f.store), before);
                assert_eq!(session(&f), f.context.source_session);
                continue;
            }
            let completed = completed.unwrap();
            completed.validate_completion_proposal(&result).unwrap();
            assert_eq!(completed.continuation.as_ref(), Some(authorization));
            assert_eq!(completed.updated_at_unix_ms, now);
            let current = session(&f);
            assert_eq!(current.active_deadline_ms(), authorization.deadline_ms);
            assert_eq!(current.updated_at_ms, now);
            let project = f
                .store
                .company_project(&f.leader.tenant_id, &f.grant.project_id)
                .unwrap()
                .unwrap();
            let fresh = project.subscription_call.as_ref().unwrap();
            assert_eq!(fresh.created_at_unix_ms, authorization.issued_at_ms);
            assert_eq!(fresh.grant.expires_at_unix_ms, authorization.deadline_ms);
            assert_eq!(fresh.allowance_id, authorization.provider_allowance_id);
            let before = rows(&f.store);
            assert_eq!(
                f.store
                    .complete_adaptive_leadership_review_call(
                        &f.leader,
                        &result,
                        authorization.deadline_ms + 1
                    )
                    .unwrap(),
                completed
            );
            assert_eq!(rows(&f.store), before);
        }
    }

    #[test]
    fn schema2_authorization_must_precede_review_expiry_but_audit_recovery_may_follow_it() {
        for offset in [0, 1] {
            let f = continuation_fixture(true, false);
            let call = dispatch_continuation(&f);
            let issued = call.grant.expires_at_unix_ms + offset;
            let result = continue_result_with_window_at(&call, 60_000, issued);
            let before = rows(&f.store);
            assert!(call.validate_completion_proposal(&result).is_err());
            assert!(f
                .store
                .complete_adaptive_leadership_review_call(&f.leader, &result, issued)
                .is_err());
            assert_eq!(rows(&f.store), before);
            assert_eq!(session(&f), f.context.source_session);
        }
        let f = continuation_fixture(true, false);
        let call = dispatch_continuation(&f);
        let result =
            continue_result_with_window_at(&call, 60_000, call.grant.expires_at_unix_ms - 1);
        call.validate_completion_proposal(&result).unwrap();
        let recovered_at = call.grant.expires_at_unix_ms + 7;
        let receipt = f
            .store
            .complete_adaptive_leadership_review_call(&f.leader, &result, recovered_at)
            .unwrap();
        assert_eq!(receipt.continuation, result.continuation);
        assert_eq!(
            session(&f).active_deadline_ms(),
            result.continuation.as_ref().unwrap().deadline_ms
        );
        let before = rows(&f.store);
        assert_eq!(
            f.store
                .complete_adaptive_leadership_review_call(&f.leader, &result, recovered_at + 1)
                .unwrap(),
            receipt
        );
        assert_eq!(rows(&f.store), before);
    }

    #[test]
    fn schema2_expired_audit_window_retires_before_review_expiry_and_replays_without_authority_effects(
    ) {
        for (unknown, resolved) in [(true, false), (false, false), (false, true)] {
            let f = continuation_fixture(unknown, resolved);
            let call = dispatch_continuation(&f);
            let result = continue_result_with_window(&call, 1_000);
            let authorization = result.continuation.as_ref().unwrap();
            let now = authorization.deadline_ms;
            assert!(now < call.grant.expires_at_unix_ms);
            call.validate_completion_proposal(&result).unwrap();
            let before = rows(&f.store);
            assert!(f
                .store
                .complete_adaptive_leadership_review_call(&f.leader, &result, now)
                .is_err());
            assert!(f
                .store
                .expire_adaptive_leadership_review_call(
                    &f.leader,
                    call.grant.review_id,
                    call.version,
                    now
                )
                .is_err());
            assert_eq!(rows(&f.store), before);
            if resolved {
                change_project(&f, now - 1);
            }
            let project = f
                .store
                .company_project(&f.leader.tenant_id, &f.grant.project_id)
                .unwrap()
                .unwrap();
            let retired = f
                .store
                .retire_expired_adaptive_continuation_call(&f.leader, &result, now)
                .unwrap();
            let mut expected = call.clone();
            expected.retired_at_unix_ms = Some(now);
            expected.version = 4;
            expected.updated_at_unix_ms = now;
            assert_eq!(retired, expected);
            assert_eq!(session(&f), f.context.source_session);
            assert_eq!(
                f.store
                    .company_project(&f.leader.tenant_id, &f.grant.project_id)
                    .unwrap()
                    .unwrap(),
                project
            );
            assert!(f
                .store
                .adaptive_leadership_abandoned_allowance(
                    &f.leader.tenant_id,
                    f.context.source_session.active_provider_allowance_id()
                )
                .unwrap()
                .is_none());
            let before = rows(&f.store);
            let reopened = WorkflowStore::open(&f.path).unwrap();
            assert_eq!(
                reopened
                    .retire_expired_adaptive_continuation_call(&f.leader, &result, now + 7)
                    .unwrap(),
                retired
            );
            assert!(reopened
                .complete_adaptive_leadership_review_call(&f.leader, &result, now + 7)
                .is_err());
            assert_eq!(rows(&f.store), before);
        }
    }

    #[test]
    fn schema2_expired_audit_retirement_rejects_unbound_or_unexpired_proposals_without_effects() {
        let f = continuation_fixture(true, false);
        let call = dispatch_continuation(&f);
        let valid = continue_result_with_window(&call, 1_000);
        let deadline = valid.continuation.as_ref().unwrap().deadline_ms;
        let before = rows(&f.store);
        assert!(f
            .store
            .retire_expired_adaptive_continuation_call(&f.leader, &valid, deadline - 1)
            .is_err());
        let mut foreign = f.leader.clone();
        foreign.principal_id = "other-leader".into();
        assert!(f
            .store
            .retire_expired_adaptive_continuation_call(&foreign, &valid, deadline)
            .is_err());
        assert_eq!(rows(&f.store), before);
        for variant in 0..10 {
            let mut invalid = valid.clone();
            match variant {
                0 => invalid.allowance_id = "borrowed-review-allowance".into(),
                1 => invalid.request_digest = "e".repeat(64),
                2 => invalid.model_response_digest = "e".repeat(64),
                3 => invalid.resolution_event_id = Some(Uuid::new_v4()),
                4 => {
                    invalid
                        .continuation
                        .as_mut()
                        .unwrap()
                        .source_session_version += 1
                }
                5 => {
                    invalid.continuation.as_mut().unwrap().provider_allowance_id =
                        "borrowed-model-allowance".into()
                }
                6 => invalid.continuation.as_mut().unwrap().deadline_ms -= 1,
                7 => {
                    invalid
                        .continuation
                        .as_mut()
                        .unwrap()
                        .abandoned_model_effect = None
                }
                8 => invalid.continuation = None,
                _ => {
                    invalid.decision.decision =
                        AdaptiveLeadershipReviewDecisionKindV1::KeepUnknown {
                            rationale: "This is not a continuation audit".into(),
                            evidence_refs: vec![call.context.evidence_refs[0].clone()],
                        };
                }
            }
            assert!(f
                .store
                .retire_expired_adaptive_continuation_call(&f.leader, &invalid, deadline)
                .is_err());
            assert_eq!(rows(&f.store), before);
        }
        let late = continue_result_with_window_at(&call, 1_000, call.grant.expires_at_unix_ms);
        assert!(f
            .store
            .retire_expired_adaptive_continuation_call(
                &f.leader,
                &late,
                late.continuation.as_ref().unwrap().deadline_ms
            )
            .is_err());
        assert_eq!(rows(&f.store), before);
        assert_eq!(session(&f), f.context.source_session);
    }

    #[test]
    fn schema2_expired_audit_retirement_replay_is_bound_to_original_clock_and_raw_response() {
        let f = continuation_fixture(true, false);
        let call = dispatch_continuation(&f);
        let result = continue_result_with_window(&call, 1_000);
        let now = result.continuation.as_ref().unwrap().deadline_ms + 7;
        f.store
            .retire_expired_adaptive_continuation_call(&f.leader, &result, now)
            .unwrap();
        let before = rows(&f.store);
        let changed_clock = continue_result_with_window_at(
            &call,
            1_000,
            result.continuation.as_ref().unwrap().issued_at_ms + 1,
        );
        call.validate_completion_proposal(&changed_clock).unwrap();
        assert!(f
            .store
            .retire_expired_adaptive_continuation_call(&f.leader, &changed_clock, now + 1)
            .is_err());
        let mut changed_response = result.clone();
        changed_response.model_response_digest = "e".repeat(64);
        let changed_id = adaptive_leadership_continuation_audit_id(
            call.grant.review_id,
            &changed_response.request_digest,
            &changed_response.model_response_digest,
            &changed_response.decision,
        )
        .unwrap();
        changed_response.resolution_event_id = Some(changed_id);
        changed_response
            .continuation
            .as_mut()
            .unwrap()
            .resolution_event_id = changed_id;
        call.validate_completion_proposal(&changed_response)
            .unwrap();
        assert!(f
            .store
            .retire_expired_adaptive_continuation_call(&f.leader, &changed_response, now + 1)
            .is_err());
        assert_eq!(rows(&f.store), before);
    }

    #[test]
    fn schema2_expired_audit_retirement_cannot_adopt_other_retirement_or_committed_continuation() {
        let f = continuation_fixture(true, false);
        let call = dispatch_continuation(&f);
        let result = continue_result_with_window(&call, 1_000);
        f.store
            .expire_adaptive_leadership_review_call(
                &f.leader,
                call.grant.review_id,
                call.version,
                call.grant.expires_at_unix_ms,
            )
            .unwrap();
        let before = rows(&f.store);
        assert!(f
            .store
            .retire_expired_adaptive_continuation_call(
                &f.leader,
                &result,
                call.grant.expires_at_unix_ms
            )
            .is_err());
        assert_eq!(rows(&f.store), before);

        let f = continuation_fixture(true, false);
        let call = dispatch_continuation(&f);
        let result = continue_result_with_window(&call, 1_000);
        f.store
            .complete_adaptive_leadership_review_call(&f.leader, &result, CONTINUATION_AT + 2)
            .unwrap();
        let now = result.continuation.as_ref().unwrap().deadline_ms;
        let before = rows(&f.store);
        assert!(f
            .store
            .retire_expired_adaptive_continuation_call(&f.leader, &result, now)
            .is_err());
        assert_eq!(rows(&f.store), before);
        // An exact committed journal with a pending review receipt is recovery, not expiry.
        persist_entity(&f.store, &call);
        let before = rows(&f.store);
        assert!(f
            .store
            .retire_expired_adaptive_continuation_call(&f.leader, &result, now)
            .is_err());
        assert_eq!(rows(&f.store), before);

        let legacy = fixture();
        let call = authorize(&legacy);
        let call = legacy
            .store
            .claim_adaptive_leadership_review_call(&legacy.leader, &claim(&call), AUTHORIZED_AT + 1)
            .unwrap();
        let before = rows(&legacy.store);
        assert!(legacy
            .store
            .retire_expired_adaptive_continuation_call(
                &legacy.leader,
                &completion(&call, false),
                call.grant.expires_at_unix_ms
            )
            .is_err());
        assert_eq!(rows(&legacy.store), before);
    }

    #[test]
    fn schema2_expired_audit_retirement_race_writes_one_immutable_result() {
        let f = continuation_fixture(true, false);
        let call = dispatch_continuation(&f);
        let result = continue_result_with_window(&call, 1_000);
        let now = result.continuation.as_ref().unwrap().deadline_ms;
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let path = f.path.clone();
                let leader = f.leader.clone();
                let result = result.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let store = WorkflowStore::open(path).unwrap();
                    barrier.wait();
                    store
                        .retire_expired_adaptive_continuation_call(&leader, &result, now)
                        .unwrap()
                })
            })
            .collect();
        let retired: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(retired[0], retired[1]);
        assert_eq!(retired[0].retired_at_unix_ms, Some(now));
        assert_eq!(session(&f), f.context.source_session);
        assert_eq!(
            f.store
                .company_project(&f.leader.tenant_id, &f.grant.project_id)
                .unwrap()
                .unwrap(),
            f.context.source_project
        );
        let before = rows(&f.store);
        assert_eq!(
            f.store
                .retire_expired_adaptive_continuation_call(&f.leader, &result, now + 1)
                .unwrap(),
            retired[0]
        );
        assert_eq!(rows(&f.store), before);
    }

    #[test]
    fn schema2_keep_decisions_reject_fresh_expiry_but_allow_completed_replay_and_legacy_completion()
    {
        for (unknown, resolved) in [(true, false), (false, false), (false, true)] {
            let f = continuation_fixture(unknown, resolved);
            let call = dispatch_continuation(&f);
            let mut result = completion(&call, false);
            result.decision.schema_version = 2;
            if unknown {
                result.decision.decision = AdaptiveLeadershipReviewDecisionKindV1::KeepUnknown {
                    rationale: "Retain the exact unknown model effect".into(),
                    evidence_refs: vec![call.context.evidence_refs[0].clone()],
                };
            }
            let expires = call.grant.expires_at_unix_ms;
            let before = rows(&f.store);
            for now in [expires, expires + 1] {
                assert!(f
                    .store
                    .complete_adaptive_leadership_review_call(&f.leader, &result, now)
                    .is_err());
                assert_eq!(rows(&f.store), before);
            }
            assert_eq!(session(&f), f.context.source_session);
            let receipt = f
                .store
                .complete_adaptive_leadership_review_call(&f.leader, &result, expires - 1)
                .unwrap();
            let before = rows(&f.store);
            assert_eq!(
                f.store
                    .complete_adaptive_leadership_review_call(&f.leader, &result, expires + 1)
                    .unwrap(),
                receipt
            );
            assert_eq!(rows(&f.store), before);
        }
        let legacy = fixture();
        let call = authorize(&legacy);
        let call = legacy
            .store
            .claim_adaptive_leadership_review_call(&legacy.leader, &claim(&call), AUTHORIZED_AT + 1)
            .unwrap();
        let result = completion(&call, false);
        let receipt = legacy
            .store
            .complete_adaptive_leadership_review_call(
                &legacy.leader,
                &result,
                call.grant.expires_at_unix_ms + 1,
            )
            .unwrap();
        assert_eq!(receipt.decision.as_ref(), Some(&result.decision));
    }

    #[test]
    fn schema2_journal_abandonment_does_not_weaken_ordinary_subscription_checks() {
        let f = continuation_fixture(true, false);
        let call = dispatch_continuation(&f);
        let result = continue_result(&call);
        let mut project = f.context.source_project.clone();
        assert!(project
            .subscription_call
            .as_ref()
            .unwrap()
            .dispatch
            .is_none());
        let before = project.clone();
        assert!(subscription::abandon(
            &mut project,
            &f.leader,
            &f.context.source_session.grant.provider_allowance_id,
            DIGEST,
            &result.resolution_event_id.unwrap().to_string(),
            &f.leader.principal_id,
            CONTINUATION_AT + 2
        )
        .is_err());
        assert_eq!(project, before);
        f.store
            .complete_adaptive_leadership_review_call(&f.leader, &result, CONTINUATION_AT + 2)
            .unwrap();
        let abandoned = f
            .store
            .adaptive_leadership_abandoned_allowance(
                &f.leader.tenant_id,
                &f.context.source_session.grant.provider_allowance_id,
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            abandoned.allowance_id,
            f.context.source_session.grant.provider_allowance_id
        );
        assert!(abandoned
            .review
            .context
            .source_project
            .subscription_call
            .as_ref()
            .unwrap()
            .dispatch
            .is_none());
    }

    #[test]
    fn schema2_abandonment_readback_rejects_checksum_valid_unrelated_receipt() {
        let f = continuation_fixture(true, false);
        let call = dispatch_continuation(&f);
        let result = continue_result(&call);
        f.store
            .complete_adaptive_leadership_review_call(&f.leader, &result, CONTINUATION_AT + 2)
            .unwrap();
        let mut abandoned = f
            .store
            .adaptive_leadership_abandoned_allowance(
                &f.leader.tenant_id,
                &f.context.source_session.grant.provider_allowance_id,
            )
            .unwrap()
            .unwrap();
        abandoned.review.model_response_digest = Some("d".repeat(64));
        let audit = adaptive_leadership_continuation_audit_id(
            call.grant.review_id,
            &result.request_digest,
            abandoned.review.model_response_digest.as_deref().unwrap(),
            abandoned.review.decision.as_ref().unwrap(),
        )
        .unwrap();
        abandoned.review.resolution_event_id = Some(audit);
        abandoned
            .review
            .continuation
            .as_mut()
            .unwrap()
            .resolution_event_id = audit;
        persist_entity(&f.store, &abandoned);
        let before = rows(&f.store);
        assert!(f
            .store
            .adaptive_leadership_abandoned_allowance(
                &f.leader.tenant_id,
                &f.context.source_session.grant.provider_allowance_id
            )
            .is_err());
        assert_eq!(rows(&f.store), before);
    }

    #[test]
    fn schema2_exhausted_single_call_source_does_not_reset_original_budget() {
        let f = continuation_fixture_with_calls(true, false, 1);
        let call = dispatch_continuation(&f);
        assert_eq!(call.context.source_session.grant.max_model_calls, 1);
        assert_eq!(call.context.source_session.model_calls, 1);
        let before = rows(&f.store);
        assert!(f
            .store
            .complete_adaptive_leadership_review_call(
                &f.leader,
                &continue_result(&call),
                CONTINUATION_AT + 2
            )
            .is_err());
        assert_eq!(rows(&f.store), before);
        assert_eq!(session(&f), f.context.source_session);
        let mut keep = completion(&call, false);
        keep.decision = AdaptiveLeadershipReviewDecisionV1 {
            schema_version: 2,
            decision: AdaptiveLeadershipReviewDecisionKindV1::KeepUnknown {
                rationale: "The original model budget is exhausted".into(),
                evidence_refs: vec![f.context.evidence_refs[0].clone()],
            },
        };
        f.store
            .complete_adaptive_leadership_review_call(&f.leader, &keep, CONTINUATION_AT + 3)
            .unwrap();
        assert_eq!(session(&f), f.context.source_session);
    }

    #[test]
    fn schema2_unknown_subject_rejects_tool_unknown_and_stale_or_borrowed_model_effect() {
        let f = continuation_fixture(true, false);
        let before = rows(&f.store);
        for case in 0..4 {
            let mut context = f.context.clone();
            let mut grant = f.grant.clone();
            match case {
                0 => {
                    let AdaptiveLeadershipReviewSubjectV2::UnknownModel { effect, .. } =
                        grant.subject.as_mut().unwrap()
                    else {
                        panic!("subject");
                    };
                    effect.id = Uuid::new_v4();
                }
                1 => context.source_session.version += 1,
                2 => {
                    context.source_session.cursor = AdaptiveCursorV1::ToolUnknown {
                        effect: AdaptiveEffectV1 {
                            id: Uuid::new_v4(),
                            request_digest: DIGEST.into(),
                        },
                        tool: sentinel_common::WorkbenchTool::InspectFile {
                            path: "src/main.rs".into(),
                            max_bytes: 1024,
                        },
                        tool_digest: DIGEST.into(),
                    }
                }
                _ => grant.expected_reason_code = REASON.into(),
            }
            assert!(f
                .store
                .authorize_adaptive_leadership_review_call(
                    &f.leader,
                    Uuid::new_v4(),
                    "invalid-subject",
                    &grant,
                    &context,
                    CONTINUATION_AT
                )
                .is_err());
            assert_eq!(rows(&f.store), before);
        }
    }

    #[test]
    fn schema2_unknown_preflight_rejects_early_issuance_without_renewing_audit_clock() {
        let f = continuation_fixture(true, false);
        let deadline = f.context.source_session.active_deadline_ms();
        let pending = f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                Uuid::new_v4(),
                "early-unknown-review",
                &f.grant,
                &f.context,
                deadline - 1_000,
            )
            .unwrap();
        let call = f
            .store
            .claim_adaptive_leadership_review_call(&f.leader, &claim(&pending), deadline - 999)
            .unwrap();
        let before = rows(&f.store);
        let early = continue_result_with_window_at(&call, 1_000, deadline - 1);
        assert!(call.validate_completion_proposal(&early).is_err());
        assert!(f
            .store
            .complete_adaptive_leadership_review_call(&f.leader, &early, deadline + 1,)
            .is_err());
        assert_eq!(rows(&f.store), before);
        for issued in [deadline, deadline + 1] {
            let valid = continue_result_with_window_at(&call, 1_000, issued);
            call.validate_completion_proposal(&valid).unwrap();
            assert_eq!(rows(&f.store), before);
        }
    }

    #[test]
    fn schema2_completion_rejects_spoofed_authorization_decision_refs_and_source_without_effects() {
        let f = continuation_fixture(true, false);
        let call = dispatch_continuation(&f);
        let result = continue_result(&call);
        let before = rows(&f.store);
        call.validate_completion_proposal(&result).unwrap();
        assert_eq!(rows(&f.store), before);
        for case in 0..15 {
            let mut candidate = result.clone();
            let auth = candidate.continuation.as_mut().unwrap();
            match case {
                0 => auth.review_id = Uuid::new_v4(),
                1 => auth.operation_id = Uuid::new_v4(),
                2 => auth.session_id = Uuid::new_v4(),
                3 => auth.source_session_version += 1,
                4 => auth.abandoned_model_effect.as_mut().unwrap().id = Uuid::new_v4(),
                5 => {
                    auth.provider_allowance_id = call
                        .context
                        .source_session
                        .grant
                        .provider_allowance_id
                        .clone()
                }
                6 => auth.deadline_ms += 1,
                7 => auth.additional_model_calls += 1,
                8 => auth.provider_authority_digest = "e".repeat(64),
                9 => candidate.resolution_event_id = Some(Uuid::new_v4()),
                10 => candidate.continuation = None,
                11 => {
                    candidate.decision = AdaptiveLeadershipReviewDecisionV1 {
                        schema_version: 1,
                        decision: AdaptiveLeadershipReviewDecisionKindV1::ResolveBlocked {
                            rationale: "Not a blocked subject".into(),
                            evidence_refs: vec![f.context.evidence_refs[0].clone()],
                        },
                    }
                }
                12 => {
                    if let AdaptiveLeadershipReviewDecisionKindV1::Continue {
                        evidence_refs, ..
                    } = &mut candidate.decision.decision
                    {
                        *evidence_refs = vec!["invented-provider-response".into()];
                    }
                }
                13 => {
                    candidate.decision = AdaptiveLeadershipReviewDecisionV1 {
                        schema_version: 2,
                        decision: AdaptiveLeadershipReviewDecisionKindV1::KeepBlocked {
                            rationale: "Cannot disguise unknown evidence as blocked".into(),
                            evidence_refs: vec![f.context.evidence_refs[0].clone()],
                        },
                    }
                }
                _ => candidate.decision.schema_version = 3,
            }
            assert!(call.validate_completion_proposal(&candidate).is_err());
            assert!(f
                .store
                .complete_adaptive_leadership_review_call(
                    &f.leader,
                    &candidate,
                    CONTINUATION_AT + 2
                )
                .is_err());
            assert_eq!(rows(&f.store), before);
        }
        change_project(&f, CONTINUATION_AT + 2);
        let changed = rows(&f.store);
        call.validate_completion_proposal(&result).unwrap();
        assert!(f
            .store
            .complete_adaptive_leadership_review_call(&f.leader, &result, CONTINUATION_AT + 2)
            .is_err());
        assert_eq!(rows(&f.store), changed);
        let f = continuation_fixture(false, true);
        let call = dispatch_continuation(&f);
        f.store
            .advance_adaptive_session(
                f.grant.session_id,
                f.grant.expected_session_version,
                Uuid::new_v4(),
                &AdaptiveTransitionV1::Cancel,
                &f.grant.assignee_authority,
                CONTINUATION_AT + 2,
            )
            .unwrap();
        let changed = rows(&f.store);
        assert!(f
            .store
            .complete_adaptive_leadership_review_call(
                &f.leader,
                &continue_result(&call),
                CONTINUATION_AT + 2
            )
            .is_err());
        assert_eq!(rows(&f.store), changed);
    }

    #[test]
    fn schema2_completion_rechecks_checksum_valid_completed_planning_policy() {
        let f = continuation_fixture(true, false);
        let call = dispatch_continuation(&f);
        tamper_planning_policy(&f, true);
        let before = rows(&f.store);
        assert!(f
            .store
            .complete_adaptive_leadership_review_call(
                &f.leader,
                &continue_result(&call),
                CONTINUATION_AT + 2
            )
            .is_err());
        assert_eq!(rows(&f.store), before);
        assert_eq!(session(&f), f.context.source_session);
    }

    #[test]
    fn schema2_missing_allowance_source_rejects_without_journal_effects() {
        let mut f = continuation_fixture(true, false);
        // Real fixture without subscription authority is valid leadership input, not continuation authority.
        f.context.source_project.subscription_call = None;
        persist_entity(&f.store, &f.context.source_project);
        let call = dispatch_continuation(&f);
        let before = rows(&f.store);
        assert!(f
            .store
            .complete_adaptive_leadership_review_call(
                &f.leader,
                &continue_result(&call),
                CONTINUATION_AT + 2
            )
            .is_err());
        assert_eq!(rows(&f.store), before);
        assert_eq!(session(&f), f.context.source_session);
    }

    #[test]
    fn schema2_audit_write_failure_rolls_back_journal_allowance_and_abandonment_receipt() {
        let f = continuation_fixture(true, false);
        let call = dispatch_continuation(&f);
        f.store
            .connection
            .lock()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER reject_continuation_audit BEFORE INSERT ON company_events
             WHEN NEW.event_type = 'adaptive_leadership_continuation_authorized'
             BEGIN SELECT RAISE(ABORT, 'test continuation audit failure'); END;",
            )
            .unwrap();
        let result = continue_result(&call);
        call.validate_completion_proposal(&result).unwrap();
        let before = rows(&f.store);
        assert!(f
            .store
            .complete_adaptive_leadership_review_call(&f.leader, &result, CONTINUATION_AT + 2)
            .is_err());
        assert_eq!(rows(&f.store), before);
        assert_eq!(session(&f), f.context.source_session);
        assert!(f
            .store
            .adaptive_leadership_abandoned_allowance(
                &f.leader.tenant_id,
                &f.context.source_session.grant.provider_allowance_id
            )
            .unwrap()
            .is_none());
    }

    #[test]
    fn schema2_proposal_validation_rejects_spoofed_identity_request_and_unclaimed_input_without_effects(
    ) {
        let f = continuation_fixture(true, false);
        let pending = f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                Uuid::new_v4(),
                "unclaimed-proposal",
                &f.grant,
                &f.context,
                CONTINUATION_AT,
            )
            .unwrap();
        let call = f
            .store
            .claim_adaptive_leadership_review_call(&f.leader, &claim(&pending), CONTINUATION_AT + 1)
            .unwrap();
        let result = continue_result(&call);
        let before = rows(&f.store);
        assert!(pending.validate_completion_proposal(&result).is_err());
        for case in 0..4 {
            let mut invalid = result.clone();
            match case {
                0 => invalid.review_id = Uuid::new_v4(),
                1 => invalid.allowance_id = "borrowed-allowance".into(),
                2 => invalid.request_digest = "b".repeat(64),
                _ => invalid.model_response_digest = "not-a-raw-response-hash".into(),
            }
            assert!(call.validate_completion_proposal(&invalid).is_err());
        }
        assert_eq!(rows(&f.store), before);
        call.validate_completion_proposal(&result).unwrap();
        assert_eq!(rows(&f.store), before);
    }

    #[test]
    fn schema2_journal_only_commit_cannot_substitute_for_atomic_domain_receipt() {
        for drift in [false, true] {
            let f = continuation_fixture(true, false);
            let call = dispatch_continuation(&f);
            let result = continue_result(&call);
            let continued = {
                let mut connection = f.store.connection.lock().unwrap();
                let tx = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .unwrap();
                let allowance = call
                    .continuation_allowance(CONTINUATION_AT + 2, CONTINUATION_AT + 120_002, 1)
                    .unwrap();
                let (_, current) =
                    crate::store::adaptive::continue_adaptive_session_in_transaction(
                        &tx,
                        result.continuation.as_ref().unwrap(),
                        &call,
                        &allowance,
                        &f.grant.assignee_authority,
                        CONTINUATION_AT + 2,
                    )
                    .unwrap();
                tx.commit().unwrap();
                current
            };
            if drift {
                f.store
                    .advance_adaptive_session(
                        f.grant.session_id,
                        continued.version,
                        Uuid::new_v4(),
                        &AdaptiveTransitionV1::Cancel,
                        &f.grant.assignee_authority,
                        CONTINUATION_AT + 3,
                    )
                    .unwrap();
            }
            let before = rows(&f.store);
            let completed = f.store.complete_adaptive_leadership_review_call(
                &f.leader,
                &result,
                CONTINUATION_AT + 4,
            );
            // The product transaction commits journal and receipt together. A
            // standalone journal commit cannot authorize filling in the receipt.
            assert!(completed.is_err());
            assert_eq!(rows(&f.store), before);
        }
    }

    #[test]
    fn schema2_concurrent_completion_commits_one_fresh_allowance_and_identical_receipt() {
        let f = continuation_fixture(false, false);
        let call = dispatch_continuation(&f);
        let result = continue_result(&call);
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let path = f.path.clone();
                let leader = f.leader.clone();
                let result = result.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let store = WorkflowStore::open(path).unwrap();
                    barrier.wait();
                    store
                        .complete_adaptive_leadership_review_call(
                            &leader,
                            &result,
                            CONTINUATION_AT + 2,
                        )
                        .unwrap()
                })
            })
            .collect();
        let receipts: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(receipts[0], receipts[1]);
        assert_eq!(session(&f).version, f.context.source_session.version + 1);
        assert!(f
            .store
            .company_project(&f.leader.tenant_id, &f.grant.project_id)
            .unwrap()
            .unwrap()
            .abandoned_subscription_calls
            .is_empty());
        let abandoned = f
            .store
            .adaptive_leadership_abandoned_allowance(
                &f.leader.tenant_id,
                &f.context.source_session.grant.provider_allowance_id,
            )
            .unwrap()
            .unwrap();
        assert_eq!(abandoned.review, receipts[0]);
    }

    #[test]
    fn schema2_retired_reviews_count_against_root_limit_across_continued_heads() {
        let mut f = continuation_fixture(true, false);
        let first = f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                Uuid::new_v4(),
                "first-root-review",
                &f.grant,
                &f.context,
                CONTINUATION_AT,
            )
            .unwrap();
        change_project(&f, CONTINUATION_AT);
        f.store
            .retire_stale_adaptive_leadership_review_call(
                &f.leader,
                first.grant.review_id,
                first.version,
                CONTINUATION_AT,
            )
            .unwrap();
        f.context.source_project = f
            .store
            .company_project(&f.leader.tenant_id, &f.grant.project_id)
            .unwrap()
            .unwrap();
        f.grant.expected_project_version = f.context.source_project.version;
        f.context
            .evidence_refs
            .push("project-decision:first-retirement".into());
        f.grant.evidence_fingerprint = adaptive_leadership_evidence_fingerprint(
            &f.context.tool_catalog,
            &f.context.evidence_refs,
        )
        .unwrap();
        f.grant.review_id = adaptive_leadership_review_id(
            f.grant.session_id,
            f.grant.expected_session_version,
            &f.grant.evidence_fingerprint,
        )
        .unwrap();
        let second = dispatch_continuation(&f);
        let result = continue_result(&second);
        f.store
            .complete_adaptive_leadership_review_call(&f.leader, &result, CONTINUATION_AT + 2)
            .unwrap();
        let current = session(&f);
        let effect = AdaptiveEffectV1 {
            id: Uuid::new_v4(),
            request_digest: "f".repeat(64),
        };
        let (_, pending) = f
            .store
            .advance_adaptive_session(
                f.grant.session_id,
                current.version,
                Uuid::new_v4(),
                &AdaptiveTransitionV1::ClaimModel {
                    effect: effect.clone(),
                    previous_observation_digest: None,
                },
                &f.grant.assignee_authority,
                CONTINUATION_AT + 3,
            )
            .unwrap();
        let (_, unknown) = f
            .store
            .advance_adaptive_session(
                f.grant.session_id,
                pending.version,
                Uuid::new_v4(),
                &AdaptiveTransitionV1::MarkUnknown {
                    effect: effect.clone(),
                },
                &f.grant.assignee_authority,
                CONTINUATION_AT + 4,
            )
            .unwrap();
        f.context.source_session = unknown;
        f.context.source_project = f
            .store
            .company_project(&f.leader.tenant_id, &f.grant.project_id)
            .unwrap()
            .unwrap();
        f.grant.expected_project_version = f.context.source_project.version;
        f.grant.expected_session_version = f.context.source_session.version;
        f.grant.subject = Some(AdaptiveLeadershipReviewSubjectV2::UnknownModel {
            effect: effect.clone(),
            sealed_unknown_proof_digest: DIGEST.into(),
        });
        f.context.evidence_refs = vec![
            format!(
                "adaptive-model-unknown:{}:{}",
                effect.id, effect.request_digest
            ),
            format!("sealed-provider-unknown:{DIGEST}"),
        ];
        f.grant.evidence_fingerprint = adaptive_leadership_evidence_fingerprint(
            &f.context.tool_catalog,
            &f.context.evidence_refs,
        )
        .unwrap();
        f.grant.review_id = adaptive_leadership_review_id(
            f.grant.session_id,
            f.grant.expected_session_version,
            &f.grant.evidence_fingerprint,
        )
        .unwrap();
        let third = f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                Uuid::new_v4(),
                "third-root-review",
                &f.grant,
                &f.context,
                CONTINUATION_AT + 5,
            )
            .unwrap();
        change_project(&f, CONTINUATION_AT + 6);
        f.store
            .retire_stale_adaptive_leadership_review_call(
                &f.leader,
                third.grant.review_id,
                third.version,
                CONTINUATION_AT + 6,
            )
            .unwrap();
        f.context.source_project = f
            .store
            .company_project(&f.leader.tenant_id, &f.grant.project_id)
            .unwrap()
            .unwrap();
        f.grant.expected_project_version = f.context.source_project.version;
        f.context
            .evidence_refs
            .push("project-decision:third-retirement".into());
        f.grant.evidence_fingerprint = adaptive_leadership_evidence_fingerprint(
            &f.context.tool_catalog,
            &f.context.evidence_refs,
        )
        .unwrap();
        f.grant.review_id = adaptive_leadership_review_id(
            f.grant.session_id,
            f.grant.expected_session_version,
            &f.grant.evidence_fingerprint,
        )
        .unwrap();
        let before = rows(&f.store);
        assert!(f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                Uuid::new_v4(),
                "fourth-root-review",
                &f.grant,
                &f.context,
                CONTINUATION_AT + 7
            )
            .is_err());
        assert_eq!(rows(&f.store), before);
    }

    #[test]
    fn schema2_expired_blocked_subject_and_review_cannot_renew_automatically() {
        let f = continuation_fixture(false, false);
        let mut early = f.grant.clone();
        early.expires_at_unix_ms = 720_010;
        let before = rows(&f.store);
        assert!(f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                Uuid::new_v4(),
                "not-expired",
                &early,
                &f.context,
                600_010
            )
            .is_err());
        assert_eq!(rows(&f.store), before);
        let call = f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                Uuid::new_v4(),
                "no-auto-renewal",
                &f.grant,
                &f.context,
                CONTINUATION_AT,
            )
            .unwrap();
        let mut renewed = f.grant.clone();
        renewed.expires_at_unix_ms += 120_000;
        let before = rows(&f.store);
        assert!(f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                call.operation_id,
                &call.allowance_id,
                &renewed,
                &f.context,
                f.grant.expires_at_unix_ms
            )
            .is_err());
        assert_eq!(rows(&f.store), before);
    }

    #[test]
    fn schema2_undispatched_expiry_retires_unchanged_subject_without_releasing_authority() {
        for (unknown, resolved) in [(true, false), (false, false), (false, true)] {
            let f = continuation_fixture(unknown, resolved);
            let call = f
                .store
                .authorize_adaptive_leadership_review_call(
                    &f.leader,
                    Uuid::new_v4(),
                    "expire-subject",
                    &f.grant,
                    &f.context,
                    CONTINUATION_AT,
                )
                .unwrap();
            let expires = call.grant.expires_at_unix_ms;
            let before = rows(&f.store);
            assert!(f
                .store
                .retire_stale_adaptive_leadership_review_call(
                    &f.leader,
                    call.grant.review_id,
                    call.version,
                    expires - 1
                )
                .is_err());
            assert!(f
                .store
                .retire_stale_adaptive_leadership_review_call(
                    &f.leader,
                    call.grant.review_id,
                    call.version + 1,
                    expires
                )
                .is_err());
            let mut foreign = f.leader.clone();
            foreign.principal_id = "other-leader".into();
            assert!(f
                .store
                .retire_stale_adaptive_leadership_review_call(
                    &foreign,
                    call.grant.review_id,
                    call.version,
                    expires
                )
                .is_err());
            assert_eq!(rows(&f.store), before);
            let retired = f
                .store
                .retire_stale_adaptive_leadership_review_call(
                    &f.leader,
                    call.grant.review_id,
                    call.version,
                    expires,
                )
                .unwrap();
            assert_eq!(retired.retired_at_unix_ms, Some(expires));
            assert_eq!(retired.version, 4);
            assert!(
                retired.dispatch.is_none()
                    && retired.decision.is_none()
                    && retired.continuation.is_none()
                    && retired.model_response_digest.is_none()
                    && retired.resolution_event_id.is_none()
            );
            assert_eq!(session(&f), f.context.source_session);
            assert_eq!(
                f.store
                    .company_project(&f.leader.tenant_id, &f.grant.project_id)
                    .unwrap()
                    .unwrap(),
                f.context.source_project
            );
            let before = rows(&f.store);
            assert_eq!(
                f.store
                    .retire_stale_adaptive_leadership_review_call(
                        &f.leader,
                        call.grant.review_id,
                        call.version,
                        expires + 1
                    )
                    .unwrap(),
                retired
            );
            assert!(f
                .store
                .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), expires)
                .is_err());
            assert_eq!(rows(&f.store), before);
        }
    }

    #[test]
    fn schema2_expiry_retirement_keeps_dispatched_legacy_and_committed_recovery_guards() {
        let f = continuation_fixture(true, false);
        let call = dispatch_continuation(&f);
        let before = rows(&f.store);
        assert!(f
            .store
            .retire_stale_adaptive_leadership_review_call(
                &f.leader,
                call.grant.review_id,
                call.version,
                call.grant.expires_at_unix_ms
            )
            .is_err());
        assert_eq!(rows(&f.store), before);
        let result = continue_result(&call);
        f.store
            .complete_adaptive_leadership_review_call(&f.leader, &result, CONTINUATION_AT + 2)
            .unwrap();
        // Recoverable committed journal state must never be retired, even with its receipt pending.
        persist_entity(&f.store, &call);
        let before = rows(&f.store);
        assert!(f
            .store
            .retire_stale_adaptive_leadership_review_call(
                &f.leader,
                call.grant.review_id,
                call.version,
                call.grant.expires_at_unix_ms
            )
            .is_err());
        assert!(f
            .store
            .expire_adaptive_leadership_review_call(
                &f.leader,
                call.grant.review_id,
                call.version,
                call.grant.expires_at_unix_ms
            )
            .is_err());
        assert_eq!(rows(&f.store), before);

        let legacy = fixture();
        let call = legacy
            .store
            .authorize_adaptive_leadership_review_call(
                &legacy.leader,
                Uuid::new_v4(),
                "legacy-expiry",
                &legacy.grant,
                &legacy.context,
                AUTHORIZED_AT,
            )
            .unwrap();
        let before = rows(&legacy.store);
        assert!(legacy
            .store
            .retire_stale_adaptive_leadership_review_call(
                &legacy.leader,
                call.grant.review_id,
                call.version,
                call.grant.expires_at_unix_ms
            )
            .is_err());
        assert!(legacy
            .store
            .expire_adaptive_leadership_review_call(
                &legacy.leader,
                call.grant.review_id,
                call.version,
                call.grant.expires_at_unix_ms
            )
            .is_err());
        assert_eq!(rows(&legacy.store), before);
        let mut renewed = call.grant.clone();
        renewed.expires_at_unix_ms += 120_000;
        let renewed_call = legacy
            .store
            .authorize_adaptive_leadership_review_call(
                &legacy.leader,
                call.operation_id,
                &call.allowance_id,
                &renewed,
                &call.context,
                call.grant.expires_at_unix_ms,
            )
            .unwrap();
        assert_eq!(renewed_call.grant, renewed);
        assert!(renewed_call.retired_at_unix_ms.is_none());
    }

    #[test]
    fn schema2_explicit_expiry_terminates_uncompleted_dispatch_without_rewriting_history() {
        for dispatched in [false, true] {
            for source_changed in [false, true] {
                let f = continuation_fixture(true, false);
                let call = if dispatched {
                    dispatch_continuation(&f)
                } else {
                    f.store
                        .authorize_adaptive_leadership_review_call(
                            &f.leader,
                            Uuid::new_v4(),
                            "explicit-expiry",
                            &f.grant,
                            &f.context,
                            CONTINUATION_AT,
                        )
                        .unwrap()
                };
                let expires = call.grant.expires_at_unix_ms;
                if source_changed {
                    change_project(&f, expires - 1);
                }
                let project = f
                    .store
                    .company_project(&f.leader.tenant_id, &f.grant.project_id)
                    .unwrap()
                    .unwrap();
                let before = rows(&f.store);
                assert!(f
                    .store
                    .expire_adaptive_leadership_review_call(
                        &f.leader,
                        call.grant.review_id,
                        call.version,
                        expires - 1
                    )
                    .is_err());
                assert!(f
                    .store
                    .expire_adaptive_leadership_review_call(
                        &f.leader,
                        call.grant.review_id,
                        if dispatched { 1 } else { 2 },
                        expires
                    )
                    .is_err());
                assert_eq!(rows(&f.store), before);
                let retired = f
                    .store
                    .expire_adaptive_leadership_review_call(
                        &f.leader,
                        call.grant.review_id,
                        call.version,
                        expires,
                    )
                    .unwrap();
                assert_eq!(retired.dispatch, call.dispatch);
                assert_eq!(retired.context, call.context);
                assert_eq!(retired.grant, call.grant);
                assert_eq!(retired.retired_at_unix_ms, Some(expires));
                assert!(
                    retired.decision.is_none()
                        && retired.continuation.is_none()
                        && retired.model_response_digest.is_none()
                        && retired.resolution_event_id.is_none()
                );
                assert_eq!(session(&f), f.context.source_session);
                assert_eq!(
                    f.store
                        .company_project(&f.leader.tenant_id, &f.grant.project_id)
                        .unwrap()
                        .unwrap(),
                    project
                );
                let before = rows(&f.store);
                assert_eq!(
                    f.store
                        .expire_adaptive_leadership_review_call(
                            &f.leader,
                            call.grant.review_id,
                            call.version,
                            expires + 1
                        )
                        .unwrap(),
                    retired
                );
                assert!(f
                    .store
                    .expire_adaptive_leadership_review_call(
                        &f.leader,
                        call.grant.review_id,
                        if dispatched { 1 } else { 2 },
                        expires + 1
                    )
                    .is_err());
                if dispatched {
                    let result = continue_result(&call);
                    assert!(f
                        .store
                        .complete_adaptive_leadership_review_call(&f.leader, &result, expires + 1)
                        .is_err());
                }
                assert_eq!(rows(&f.store), before);
            }
        }
    }

    #[test]
    fn schema2_dispatched_expiry_successors_preserve_history_and_total_budget_across_heads() {
        let mut f = continuation_fixture(false, false);
        let project = f.context.source_project.clone();
        let original_session = session(&f);
        let mut retired_calls: Vec<AdaptiveLeadershipReviewCallV1> = Vec::new();
        let mut now = CONTINUATION_AT;
        for index in 0..ADAPTIVE_LEADERSHIP_MAX_REVIEWS {
            if index == 2 {
                // A real journal transition changes the head, not the session's review budget.
                let event = Uuid::new_v4();
                f.context.source_session = resolve(&f, event, now);
                f.grant.expected_session_version = f.context.source_session.version;
                f.grant.subject = Some(AdaptiveLeadershipReviewSubjectV2::BlockedContinuation {
                    reason_code: REASON.into(),
                    resolution_event_id: Some(event.to_string()),
                });
                assert_ne!(f.context.source_session.version, original_session.version);
                assert_eq!(
                    f.context.source_session.model_calls,
                    original_session.model_calls
                );
                assert_eq!(
                    f.context.source_session.tool_calls,
                    original_session.tool_calls
                );
            }
            if let Some(prior) = retired_calls.last() {
                f.context.evidence_refs.push(format!(
                    "leadership-review-retired:{}:{}",
                    prior.grant.review_id,
                    prior.retired_at_unix_ms.unwrap()
                ));
            }
            f.grant.evidence_fingerprint = adaptive_leadership_evidence_fingerprint(
                &f.context.tool_catalog,
                &f.context.evidence_refs,
            )
            .unwrap();
            f.grant.review_id = adaptive_leadership_review_id(
                f.grant.session_id,
                f.grant.expected_session_version,
                &f.grant.evidence_fingerprint,
            )
            .unwrap();
            f.grant.expires_at_unix_ms = now + 120_000;
            let authorized = f
                .store
                .authorize_adaptive_leadership_review_call(
                    &f.leader,
                    Uuid::new_v4(),
                    &format!("dispatched-expiry-successor-{index}"),
                    &f.grant,
                    &f.context,
                    now,
                )
                .unwrap();
            for prior in &retired_calls {
                assert_ne!(authorized.grant.review_id, prior.grant.review_id);
                assert_ne!(authorized.request_id(), prior.request_id());
                assert_ne!(authorized.allowance_id, prior.allowance_id);
                assert_ne!(authorized.operation_id, prior.operation_id);
                assert_eq!(
                    f.store
                        .adaptive_leadership_review_call(&f.leader.tenant_id, prior.grant.review_id)
                        .unwrap()
                        .as_ref(),
                    Some(prior)
                );
            }
            let call = f
                .store
                .claim_adaptive_leadership_review_call(&f.leader, &claim(&authorized), now + 1)
                .unwrap();
            let expires = call.grant.expires_at_unix_ms;
            let mut keep = completion(&call, false);
            keep.decision.schema_version = 2;
            let continuation = continue_result_with_window_at(&call, 2_000, expires - 1);
            call.validate_completion_proposal(&keep).unwrap();
            call.validate_completion_proposal(&continuation).unwrap();
            let source = session(&f);
            let before = rows(&f.store);
            assert!(f
                .store
                .expire_adaptive_leadership_review_call(
                    &f.leader,
                    call.grant.review_id,
                    call.version,
                    expires - 1
                )
                .is_err());
            assert_eq!(rows(&f.store), before);
            let retired = f
                .store
                .expire_adaptive_leadership_review_call(
                    &f.leader,
                    call.grant.review_id,
                    call.version,
                    expires,
                )
                .unwrap();
            let mut expected = call.clone();
            expected.retired_at_unix_ms = Some(expires);
            expected.updated_at_unix_ms = expires;
            expected.version = 4;
            assert_eq!(retired, expected);
            assert_eq!(session(&f), source);
            assert_eq!(
                f.store
                    .company_project(&f.leader.tenant_id, &f.grant.project_id)
                    .unwrap()
                    .as_ref(),
                Some(&project)
            );
            let before = rows(&f.store);
            for result in [&keep, &continuation] {
                assert!(f
                    .store
                    .complete_adaptive_leadership_review_call(&f.leader, result, expires + 1)
                    .is_err());
            }
            assert!(f
                .store
                .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), expires + 1)
                .is_err());
            assert_eq!(rows(&f.store), before);
            retired_calls.push(retired);
            now = expires + 1;
        }
        let calls = f
            .store
            .adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id)
            .unwrap();
        assert_eq!(calls.len(), ADAPTIVE_LEADERSHIP_MAX_REVIEWS);
        assert_eq!(
            calls
                .iter()
                .filter(
                    |call| call.grant.expected_session_version == f.grant.expected_session_version
                )
                .count(),
            1
        );
        let prior = retired_calls.last().unwrap();
        f.context.evidence_refs.push(format!(
            "leadership-review-retired:{}:{}",
            prior.grant.review_id,
            prior.retired_at_unix_ms.unwrap()
        ));
        f.grant.evidence_fingerprint = adaptive_leadership_evidence_fingerprint(
            &f.context.tool_catalog,
            &f.context.evidence_refs,
        )
        .unwrap();
        f.grant.review_id = adaptive_leadership_review_id(
            f.grant.session_id,
            f.grant.expected_session_version,
            &f.grant.evidence_fingerprint,
        )
        .unwrap();
        f.grant.expires_at_unix_ms = now + 120_000;
        f.grant.validate(now).unwrap();
        f.context.validate(&f.grant).unwrap();
        let before = rows(&f.store);
        assert!(f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                Uuid::new_v4(),
                "dispatched-expiry-over-total-budget",
                &f.grant,
                &f.context,
                now
            )
            .is_err());
        assert_eq!(rows(&f.store), before);
        for prior in &retired_calls {
            assert_eq!(
                f.store
                    .adaptive_leadership_review_call(&f.leader.tenant_id, prior.grant.review_id)
                    .unwrap()
                    .as_ref(),
                Some(prior)
            );
        }
    }

    #[test]
    fn schema2_expiry_retirements_count_against_root_review_bound() {
        let mut f = continuation_fixture(true, false);
        let mut now = CONTINUATION_AT;
        for index in 0..ADAPTIVE_LEADERSHIP_MAX_REVIEWS {
            f.context
                .evidence_refs
                .push(format!("expiry-review:{index}"));
            f.grant.evidence_fingerprint = adaptive_leadership_evidence_fingerprint(
                &f.context.tool_catalog,
                &f.context.evidence_refs,
            )
            .unwrap();
            f.grant.review_id = adaptive_leadership_review_id(
                f.grant.session_id,
                f.grant.expected_session_version,
                &f.grant.evidence_fingerprint,
            )
            .unwrap();
            f.grant.expires_at_unix_ms = now + 120_000;
            let call = f
                .store
                .authorize_adaptive_leadership_review_call(
                    &f.leader,
                    Uuid::new_v4(),
                    &format!("expiry-bound-{index}"),
                    &f.grant,
                    &f.context,
                    now,
                )
                .unwrap();
            now = call.grant.expires_at_unix_ms;
            f.store
                .retire_stale_adaptive_leadership_review_call(
                    &f.leader,
                    call.grant.review_id,
                    call.version,
                    now,
                )
                .unwrap();
        }
        f.context.evidence_refs.push("expiry-review:excess".into());
        f.grant.evidence_fingerprint = adaptive_leadership_evidence_fingerprint(
            &f.context.tool_catalog,
            &f.context.evidence_refs,
        )
        .unwrap();
        f.grant.review_id = adaptive_leadership_review_id(
            f.grant.session_id,
            f.grant.expected_session_version,
            &f.grant.evidence_fingerprint,
        )
        .unwrap();
        f.grant.expires_at_unix_ms = now + 120_000;
        let before = rows(&f.store);
        assert!(f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                Uuid::new_v4(),
                "expiry-bound-excess",
                &f.grant,
                &f.context,
                now
            )
            .is_err());
        assert_eq!(rows(&f.store), before);
        assert_eq!(session(&f), f.context.source_session);
    }

    #[test]
    fn schema2_decision_strict_serde_and_original_limits_and_historical_bytes() {
        let legacy = fixture();
        let original = serde_json::to_value(&legacy.grant).unwrap();
        assert!(original.get("subject").is_none());
        let roundtrip: AdaptiveLeadershipReviewGrantV1 =
            serde_json::from_value(original.clone()).unwrap();
        assert_eq!(serde_json::to_value(roundtrip).unwrap(), original);
        let f = continuation_fixture(true, false);
        let call = dispatch_continuation(&f);
        let result = continue_result(&call);
        let mut json = serde_json::to_value(&result.decision).unwrap();
        json["decision"]["operator_override"] = serde_json::json!(true);
        assert!(serde_json::from_value::<AdaptiveLeadershipReviewDecisionV1>(json).is_err());
        let mut json = serde_json::to_value(&f.grant).unwrap();
        json["subject"]["unknown_tool"] = serde_json::json!(true);
        assert!(serde_json::from_value::<AdaptiveLeadershipReviewGrantV1>(json).is_err());
        let before = rows(&f.store);
        for (calls, window) in [(0, 120_000), (4, 120_000), (1, 300_001), (1, 0)] {
            let mut candidate = result.clone();
            if let AdaptiveLeadershipReviewDecisionKindV1::Continue {
                additional_model_calls,
                window_ms,
                ..
            } = &mut candidate.decision.decision
            {
                *additional_model_calls = calls;
                *window_ms = window;
            }
            assert!(f
                .store
                .complete_adaptive_leadership_review_call(
                    &f.leader,
                    &candidate,
                    CONTINUATION_AT + 2
                )
                .is_err());
            assert_eq!(rows(&f.store), before);
        }
        for schema in [0, 1, 3, u16::MAX] {
            let mut decision = result.decision.clone();
            decision.schema_version = schema;
            assert!(decision.validate(&f.context.evidence_refs).is_err());
        }
        let mut old = legacy.grant.clone();
        old.subject = f.grant.subject.clone();
        assert!(old.validate(AUTHORIZED_AT).is_err());
        for schema in [0, 1, 3, u16::MAX] {
            let mut grant = f.grant.clone();
            grant.schema_version = schema;
            assert!(grant.validate(CONTINUATION_AT).is_err());
        }
        let mut missing_subject = f.grant.clone();
        missing_subject.subject = None;
        assert!(missing_subject.validate(CONTINUATION_AT).is_err());
        let legacy_call = authorize(&legacy);
        let encoded = serde_json::to_value(&legacy_call).unwrap();
        assert!(encoded.get("continuation").is_none());
        let digest = legacy_call.context_digest().unwrap();
        let decoded: AdaptiveLeadershipReviewCallV1 = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.context_digest().unwrap(), digest);
    }
}
