use super::adaptive_resume_policy::{
    insert_resume_review_membership, read_resume_policy_leaf, require_resume_review_membership,
};
use super::adaptive_work_funding::{
    insert_funding_review_membership, require_fresh_funding_review_source,
    require_funding_authorization_membership, require_funding_review_membership,
};
use super::*;
#[path = "adaptive_budget_review_extension.rs"]
pub(crate) mod budget_review_extension;
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
pub use budget_review_extension::{
    AdaptiveBudgetReviewExtensionReceiptV1, AdaptiveBudgetReviewExtensionRequestV1,
};

const KIND: &str = "adaptive_leadership_review_call";
const MAX_TENANT_REVIEW_SCAN: usize = 4096;
const ABANDONED_KIND: &str = "adaptive_leadership_abandoned_allowance";
const EXPIRED_CONTINUATION_KIND: &str = "adaptive_leadership_expired_continuation";
const BUDGET_LIMIT_KIND: &str = "adaptive_budget_window_limit";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum AdaptiveBudgetWindowLimitCauseV1 {
    ReviewLimit,
    HeadReviewLimit,
    WindowLimit,
    RootCallsExhausted,
}

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum AdaptiveBudgetWindowDispositionV1 {
    SystemPolicyLimit,
}

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AdaptiveBudgetWindowLimitReceiptV1 {
    schema_version: u16,
    disposition: AdaptiveBudgetWindowDispositionV1,
    receipt_id: String,
    source_digest: String,
    grant: AdaptiveLeadershipReviewGrantV1,
    context: AdaptiveLeadershipReviewContextV1,
    causes: Vec<AdaptiveBudgetWindowLimitCauseV1>,
    recorded_at_ms: u64,
}

fn budget_limit_id(session_id: Uuid, version: u64) -> Result<String, WorkflowError> {
    Ok(crate::adaptive_leadership_review_id(
        session_id,
        version,
        &canonical_sha256(
            "sentinel.workflow.adaptive-budget-limit-identity.v1",
            &(session_id, version),
        )?,
    )?
    .to_string())
}

fn budget_limit_source_digest(
    grant: &AdaptiveLeadershipReviewGrantV1,
    context: &AdaptiveLeadershipReviewContextV1,
) -> Result<String, WorkflowError> {
    let Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }) = &grant.subject
    else {
        return Err(unauthorized());
    };
    canonical_sha256(
        "sentinel.workflow.adaptive-budget-limit-source.v1",
        &(
            &context.source_project,
            &context.source_session,
            &budget.root_allowance,
            &budget.active_allowance_digest,
            &budget.continuation_history_digest,
            &grant.leadership_principal,
            &grant.leadership_authority,
            &grant.assignee_authority,
            &grant.assignment_id,
            &grant.provider,
            &grant.model,
            &grant.catalog_digest,
            grant.token_policy,
        ),
    )
}

fn budget_limit_causes(
    connection: &Connection,
    grant: &AdaptiveLeadershipReviewGrantV1,
    context: &AdaptiveLeadershipReviewContextV1,
) -> Result<Vec<AdaptiveBudgetWindowLimitCauseV1>, WorkflowError> {
    let calls = calls_for_session(
        connection,
        &grant.leadership_principal.tenant_id,
        grant.session_id,
    )?;
    let mut causes = Vec::new();
    if let Some(binding) = &grant.resume_policy {
        if calls.len() >= usize::from(binding.limits.total_review_ceiling) {
            causes.push(AdaptiveBudgetWindowLimitCauseV1::ReviewLimit);
        }
    } else {
        if calls
            .iter()
            .filter(|call| call.grant.schema_version == 3)
            .count()
            >= ADAPTIVE_LEADERSHIP_MAX_REVIEWS
        {
            causes.push(AdaptiveBudgetWindowLimitCauseV1::ReviewLimit);
        }
        if calls
            .iter()
            .filter(|call| {
                call.grant.expected_session_version == grant.expected_session_version
                    && call.grant.schema_version != 4
            })
            .count()
            >= ADAPTIVE_LEADERSHIP_MAX_REVIEWS
        {
            causes.push(AdaptiveBudgetWindowLimitCauseV1::HeadReviewLimit);
        }
    }
    if context
        .source_session
        .continuation
        .as_ref()
        .is_some_and(|state| {
            state.authorizations.len()
                >= grant.resume_policy.as_ref().map_or(
                    crate::adaptive::ADAPTIVE_CONTINUATION_MAX_WINDOWS,
                    |binding| usize::from(binding.limits.total_window_ceiling),
                )
        })
    {
        causes.push(AdaptiveBudgetWindowLimitCauseV1::WindowLimit);
    }
    if context.source_session.model_calls >= context.source_session.grant.max_model_calls {
        causes.push(AdaptiveBudgetWindowLimitCauseV1::RootCallsExhausted);
    }
    Ok(causes)
}

impl CompanyEntity for AdaptiveBudgetWindowLimitReceiptV1 {
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        (
            &self.grant.leadership_principal.tenant_id,
            BUDGET_LIMIT_KIND,
            &self.receipt_id,
            1,
        )
    }

    fn validate_entity(&self) -> Result<(), WorkflowError> {
        self.grant.validate(self.recorded_at_ms)?;
        self.context.validate(&self.grant)?;
        validate_project(&self.context.source_project)?;
        if self.schema_version != 1
            || self.grant.schema_version != 3
            || self.grant.recovery_epoch.is_some()
            || self.causes.is_empty()
            || self.causes.windows(2).any(|pair| pair[0] >= pair[1])
            || self.receipt_id
                != budget_limit_id(self.grant.session_id, self.grant.expected_session_version)?
            || self.source_digest != budget_limit_source_digest(&self.grant, &self.context)?
        {
            return Err(corrupt());
        }
        Ok(())
    }

    fn validate_persisted(&self, connection: &Connection) -> Result<(), WorkflowError> {
        require_budget_source(connection, &self.grant, &self.context)?;
        let current = budget_limit_causes(connection, &self.grant, &self.context)?;
        if self.causes.iter().any(|cause| !current.contains(cause)) {
            return Err(corrupt());
        }
        let mut statement = connection.prepare(
            "SELECT payload,payload_digest,operation_digest,authority_binding_digest,created_at_ms
             FROM company_events WHERE tenant_id=?1 AND event_type=?2 AND operation_id=?3 LIMIT 2",
        )?;
        let records = statement
            .query_map(
                params![
                    self.grant.leadership_principal.tenant_id.0,
                    "adaptive_budget_window_limit_recorded",
                    self.grant.review_id.to_string()
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
        if decode::<AdaptiveBudgetWindowLimitReceiptV1>(payload)? != *self
            || !constant_time_eq(
                digest,
                &bytes_digest("sentinel.workflow.company-event-payload.v1", payload)?,
            )
            || !constant_time_eq(
                operation,
                &canonical_sha256("sentinel.workflow.adaptive-budget-limit.v1", self)?,
            )
            || !constant_time_eq(issuer, &self.grant.leadership_principal.binding_digest()?)
            || stored_u64(*time)? != self.recorded_at_ms
        {
            return Err(corrupt());
        }
        Ok(())
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalAdoptionContinuationAudit {
    schema_version: u16,
    call: AdaptiveLeadershipReviewCallV1,
    result: CompleteAdaptiveLeadershipReviewCallV1,
}

// Shared local-adoption wire contract for daemon append and store replay verification.
fn local_adoption_audit_proposal(
    call: &AdaptiveLeadershipReviewCallV1,
    result: &CompleteAdaptiveLeadershipReviewCallV1,
) -> Result<sentinel_common::AppendProposalV2, WorkflowError> {
    use sentinel_common::{AuthorityKindV1, AuthorityRefV1, CausalContextV1};
    let authorization = result.continuation.as_ref().ok_or_else(transition)?;
    authorization
        .local_adoption
        .as_ref()
        .ok_or_else(transition)?;
    let payload = sentinel_common::canonical_json(&LocalAdoptionContinuationAudit {
        schema_version: 2,
        call: call.clone(),
        result: result.clone(),
    })
    .map_err(|_| transition())?;
    let digest = sentinel_common::sha256_hex(&payload);
    let authority = &call.grant.assignee_authority;
    let reference = |kind, id, generation, digest| AuthorityRefV1 {
        kind,
        id,
        authority_generation: generation,
        authority_digest: digest,
    };
    let tenant_bytes = authority.tenant_id.0.as_bytes();
    let mut tenant_material = b"sentinel.adaptive-recovery.tenant.v1".to_vec();
    tenant_material.extend_from_slice(&(tenant_bytes.len() as u64).to_be_bytes());
    tenant_material.extend_from_slice(tenant_bytes);
    Ok(sentinel_common::AppendProposalV2 {
        proposal_version: sentinel_common::EVENT_PROPOSAL_VERSION_V2,
        requested_event_id: Some(authorization.resolution_event_id.to_string()),
        event_type: "adaptive_leadership_continuation_authorized".into(),
        schema_version: 1,
        payload_codec: sentinel_common::EventPayloadCodec::Json,
        payload_digest: digest.clone(),
        payload,
        causal_context: CausalContextV1 {
            schema_version: sentinel_common::CAUSAL_CONTEXT_VERSION_V1,
            tenant: reference(
                AuthorityKindV1::Tenant,
                authority.tenant_id.0.clone(),
                1,
                sentinel_common::sha256_hex(&tenant_material),
            ),
            company: reference(
                AuthorityKindV1::Company,
                "virtual-company".into(),
                authority.organization_generation,
                authority.organization_digest.clone(),
            ),
            project: reference(
                AuthorityKindV1::Project,
                authority.project_id.0.clone(),
                authority.policy_generation,
                authority.policy_digest.clone(),
            ),
            workflow: Some(reference(
                AuthorityKindV1::Workflow,
                format!("adaptive-continuation:{}", call.grant.review_id),
                1,
                authority.canonical_digest()?,
            )),
            work_item: Some(reference(
                AuthorityKindV1::WorkItem,
                authority.work_item_id.0.clone(),
                authority.assignment_version,
                authority.assignment_digest.clone(),
            )),
            request_id: call.request_id(),
            request_digest: result.request_digest.clone(),
            correlation_id: call.grant.session_id.to_string(),
            causation_event_id: None,
            operation_id: authorization.resolution_event_id.to_string(),
            attempt: 1,
            source_generation: call.grant.leadership_principal.authority_generation,
            source_digest: digest,
            invocation_id: None,
            agent_id: Some(authority.agent_id.to_string()),
            tick: None,
            artifact_id: None,
            artifact_digest: None,
            qa_run_id: None,
            release_id: None,
            delivery_id: None,
            diagnostic_trace_id: None,
            diagnostic_span_id: None,
        },
        producer: "sentinel-daemon-adaptive-continuation".into(),
        owner_term: None,
        tick: None,
        requested_durability: sentinel_common::EventDurability::Authoritative,
        expected_stream_revision: sentinel_common::ExpectedStreamRevision::NoStream,
        delivery_intents: Vec::new(),
        effect_reservations: Vec::new(),
    })
}

fn require_local_adoption_audit(
    call: &AdaptiveLeadershipReviewCallV1,
    result: &CompleteAdaptiveLeadershipReviewCallV1,
    event: &sentinel_common::EventEnvelopeV2,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    event.validate_seals().map_err(|_| transition())?;
    let audit: LocalAdoptionContinuationAudit =
        serde_json::from_slice(&event.payload).map_err(|_| transition())?;
    if audit.schema_version != 2 || audit.call != *call || audit.result != *result {
        return Err(transition());
    }
    let proposal = WorkflowStore::local_adoption_continuation_audit_proposal(call, result)?;
    let adoption = result
        .continuation
        .as_ref()
        .and_then(|a| a.local_adoption.as_deref())
        .ok_or_else(transition)?;
    let appended_at = u64::try_from(event.appended_at_ms).map_err(|_| transition())?;
    if proposal.requested_event_id.as_deref() != Some(event.event_id.as_str())
        || event.event_type != proposal.event_type
        || event.producer != proposal.producer
        || event.schema_version != proposal.schema_version
        || event.payload_codec != proposal.payload_codec
        || event.payload != proposal.payload
        || event.payload_digest != proposal.payload_digest
        || event.causal_context != proposal.causal_context
        || event.owner_term != proposal.owner_term
        || event.tick != proposal.tick
        || event.durability != proposal.requested_durability
        || event.canonical_request_digest
            != proposal
                .canonical_request_digest()
                .map_err(|_| transition())?
        || appended_at < adoption.issued_at_unix_ms
        || appended_at >= adoption.request.expires_at_unix_ms
        || now_ms < appended_at
    {
        return Err(transition());
    }
    Ok(())
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
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

pub(super) fn calls_for_session(
    connection: &Connection,
    tenant: &TenantId,
    session_id: Uuid,
) -> Result<Vec<AdaptiveLeadershipReviewCallV1>, WorkflowError> {
    validation_scope::memoize(connection, "review-calls", &(tenant, session_id), || {
        calls_for_session_uncached(connection, tenant, session_id)
    })
}

fn calls_for_session_uncached(
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

pub(super) fn require_current_source(
    connection: &Connection,
    call: &AdaptiveLeadershipReviewCallV1,
) -> Result<(), WorkflowError> {
    validation_scope::with_scope(connection, || {
        require_current_source_uncached(connection, call)
    })
}

fn require_current_source_uncached(
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
    require_planning_policy(connection, call, &project)?;
    require_fresh_resume_review_source(connection, call)?;
    require_budget_source(connection, &call.grant, &call.context)
}

fn require_fresh_resume_review_source(
    connection: &Connection,
    call: &AdaptiveLeadershipReviewCallV1,
) -> Result<(), WorkflowError> {
    if call.grant.work_funding.is_some() {
        return require_fresh_funding_review_source(connection, call);
    }
    let receipt = read_resume_policy_leaf(
        connection,
        &call.grant.leadership_principal.tenant_id,
        call.grant.session_id,
    )?;
    let Some(binding) = &call.grant.resume_policy else {
        return if receipt.is_some() {
            Err(unauthorized())
        } else {
            Ok(())
        };
    };
    let receipt = receipt.ok_or_else(unauthorized)?;
    if binding.policy_id != receipt.policy_id
        || binding.receipt_digest != receipt.receipt_digest()?
        || binding.limits != receipt.request.limits
        || call.grant.assignee_authority != receipt.request.source.assignee_authority
        || call.grant.project_id != receipt.request.source.project_id
        || call.grant.work_item_id != receipt.request.source.work_item_id
        || call.context.source_session.grant.authority != receipt.request.source.assignee_authority
        || call.grant_issued_at_unix_ms < receipt.issued_at_unix_ms
    {
        return Err(unauthorized());
    }
    let anchor = &receipt.request.source;
    if call.context.source_session.version == anchor.expected_session_version {
        if call.context.source_project.version != anchor.expected_project_version
            || bytes_digest(
                "sentinel.workflow.company-entity-row.v1",
                &encode(&call.context.source_project)?,
            )? != anchor.project_payload_digest
        {
            return Err(unauthorized());
        }
        match (&anchor.subject, &call.grant.subject) {
            (
                crate::AdaptiveResumeSubjectV1::ReadyForModel {
                    active_allowance_digest,
                },
                Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }),
            ) if active_allowance_digest == &budget.active_allowance_digest => {}
            (
                crate::AdaptiveResumeSubjectV1::ModelUnknown {
                    effect,
                    sealed_unknown_proof_digest,
                },
                Some(AdaptiveLeadershipReviewSubjectV2::UnknownModel {
                    effect: actual,
                    sealed_unknown_proof_digest: proof,
                }),
            ) if effect == actual && sealed_unknown_proof_digest == proof => {}
            _ => return Err(unauthorized()),
        }
    }
    crate::store::adaptive::require_resume_policy_anchor(
        connection,
        &receipt,
        &call.context.source_session,
    )?;
    Ok(())
}

fn require_budget_source(
    connection: &Connection,
    grant: &AdaptiveLeadershipReviewGrantV1,
    context: &AdaptiveLeadershipReviewContextV1,
) -> Result<(), WorkflowError> {
    validation_scope::memoize(connection, "budget-source", &(grant, context), || {
        require_budget_source_uncached(connection, grant, context)
    })
}

fn require_budget_source_uncached(
    connection: &Connection,
    grant: &AdaptiveLeadershipReviewGrantV1,
    context: &AdaptiveLeadershipReviewContextV1,
) -> Result<(), WorkflowError> {
    let Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }) = &grant.subject
    else {
        return Ok(());
    };
    let source = &context.source_session;
    if !matches!(grant.schema_version, 3..=5)
        || (grant.schema_version == 3 && grant.recovery_epoch.is_some())
        || (grant.schema_version == 4
            && grant
                .recovery_epoch
                .as_ref()
                .is_none_or(|binding| binding.schema_version != 2))
        || budget.schema_version != 1
        || !matches!(source.cursor, AdaptiveCursorV1::ReadyForModel)
        || budget.observed_at_ms < source.updated_at_ms
        || budget.model_calls_exhausted != (source.model_calls >= source.active_model_ceiling())
        || budget.deadline_expired != (budget.observed_at_ms >= source.active_deadline_ms())
        || budget.dispatch_slack_insufficient
            != (source.model_admission_at(budget.observed_at_ms)
                == crate::AdaptiveModelAdmissionV1::InsufficientSlack)
        || !(budget.model_calls_exhausted
            || budget.deadline_expired
            || budget.dispatch_slack_insufficient)
    {
        return Err(unauthorized());
    }
    crate::store::adaptive::require_journal_source(connection, source)?;
    let original = super::historical_allowance::historical_adaptive_provider_project_in_connection(
        connection,
        &source.grant,
        source.grant.created_at_ms,
    )?
    .ok_or_else(unauthorized)?;
    let root = original
        .subscription_call
        .as_ref()
        .ok_or_else(unauthorized)?;
    let active = context
        .source_project
        .subscription_call
        .as_ref()
        .ok_or_else(unauthorized)?;
    if *root != budget.root_allowance
        || active.allowance_id != source.active_provider_allowance_id()
        || active.dispatch.is_some()
        || crate::adaptive_budget_allowance_digest(active)? != budget.active_allowance_digest
        || crate::adaptive_budget_history_digest(&source.continuation)?
            != budget.continuation_history_digest
        || original.governance.project_profile != context.source_project.governance.project_profile
        || root.grant.provider != active.grant.provider
        || root.grant.model != active.grant.model
        || root.grant.catalog_digest != active.grant.catalog_digest
        || root.grant.token_policy != active.grant.token_policy
        || root.grant.max_concurrent != active.grant.max_concurrent
    {
        return Err(unauthorized());
    }
    Ok(())
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
        || (matches!(call.grant.schema_version, 2..=5)
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
    if authorization.resume_policy != call.grant.resume_policy
        || authorization.work_funding != call.grant.work_funding
    {
        return Err(unauthorized());
    }
    if call.grant.schema_version == 3
        && (authorization.local_adoption.is_some() || call.grant.recovery_epoch.is_some())
    {
        return Err(unauthorized());
    }
    if matches!(call.grant.schema_version, 4 | 5) && authorization.local_adoption.is_some() {
        return Err(unauthorized());
    }
    if let Some(adoption) = &authorization.local_adoption {
        adoption.validate_call(call)?;
        if adoption.request.decision != *decision
            || adoption.request.model_response_digest != model_response_digest
            || adoption.request.request_digest != request_digest
            || authorization.issued_at_ms != adoption.issued_at_unix_ms
            || authorization.deadline_ms != adoption.continuation_deadline_ms
        {
            return Err(unauthorized());
        }
    }
    require_adaptive_allowance_source(call)?;
    let source = &call.context.source_session;
    let current = call
        .context
        .source_project
        .subscription_call
        .as_ref()
        .ok_or_else(unauthorized)?;
    let policy = match &call.grant.subject {
        Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }) => {
            &budget.root_allowance
        }
        _ => current,
    };
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
        Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }) => (
            crate::adaptive::AdaptiveContinuationSourceV1::BudgetWindowExhausted {
                active_allowance_digest: budget.active_allowance_digest.clone(),
                continuation_history_digest: budget.continuation_history_digest.clone(),
            },
            None,
        ),
        None => return Err(unauthorized()),
    };
    let allowance = call.continuation_allowance(
        authorization.issued_at_ms,
        authorization.deadline_ms,
        *additional_model_calls,
    )?;
    let model_ceiling = call
        .grant
        .work_funding
        .as_ref()
        .map_or(source.funded_model_call_ceiling(), |epoch| {
            epoch.binding.limits.total_model_call_ceiling
        });
    if authorization.operation_id != call.operation_id
        || authorization.review_id != call.grant.review_id
        || authorization.session_id != call.grant.session_id
        || authorization.source_session_version != call.grant.expected_session_version
        || authorization.abandoned_model_effect != abandoned
        || authorization.source != expected_source
        || authorization.additional_model_calls != *additional_model_calls
        || (call.grant.resume_policy.is_none()
            && !matches!(call.grant.schema_version, 3..=5)
            && *additional_model_calls > current.grant.max_calls)
        || *additional_model_calls > model_ceiling
        || source
            .model_calls
            .checked_add(*additional_model_calls)
            .is_none_or(|total| total > model_ceiling)
        || authorization
            .deadline_ms
            .checked_sub(authorization.issued_at_ms)
            != Some(*window_ms)
        || (call.grant.resume_policy.is_none()
            && call.grant.work_funding.is_none()
            && source
                .grant
                .deadline_ms
                .checked_sub(source.grant.created_at_ms)
                .is_none_or(|window| *window_ms > window))
        || (call.grant.resume_policy.is_none()
            && call.grant.work_funding.is_none()
            && allowance.grant.max_duration_ms > policy.grant.max_duration_ms)
        || allowance.grant.max_concurrent > policy.grant.max_concurrent
        || authorization.issued_at_ms
            < call
                .dispatch
                .as_ref()
                .ok_or_else(unauthorized)?
                .dispatched_at_unix_ms
        || (authorization.local_adoption.is_none()
            && authorization.issued_at_ms >= call.grant.expires_at_unix_ms)
        || (call.grant.resume_policy.is_none()
            && !matches!(call.grant.schema_version, 3..=5)
            && authorization.issued_at_ms < source.active_deadline_ms())
        || (matches!(call.grant.schema_version, 3..=5)
            && (authorization.issued_at_ms < source.updated_at_ms
                || !matches!(source.cursor, AdaptiveCursorV1::ReadyForModel)
                || !source.model_window_exhausted_at(authorization.issued_at_ms)))
        || authorization.provider_allowance_id
            != crate::domain::stable_domain_id(
                "subscription",
                &call.grant.leadership_principal.tenant_id,
                call.operation_id,
            )?
        || authorization.provider_allowance_id == source.grant.provider_allowance_id
        || authorization.provider_allowance_id == call.allowance_id
        || authorization.provider_allowance_id == current.allowance_id
        || source.continuation.as_ref().is_some_and(|state| {
            state
                .authorizations
                .iter()
                .any(|prior| prior.provider_allowance_id == authorization.provider_allowance_id)
        })
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
    if let Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }) =
        &call.grant.subject
    {
        if !matches!(call.grant.schema_version, 3..=5)
            || now_ms < budget.observed_at_ms
            || now_ms < call.context.source_session.updated_at_ms
            || !call
                .context
                .source_session
                .model_window_exhausted_at(now_ms)
        {
            return Err(transition());
        }
    }
    if call.grant.work_funding.as_ref().is_some_and(|epoch| {
        now_ms < epoch.receipt.issued_at_unix_ms
            || now_ms >= epoch.binding.limits.expires_at_unix_ms
    }) {
        return Err(transition());
    }
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
    require_budget_source(transaction, &call.grant, &call.context)?;
    let policy = match &call.grant.subject {
        Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }) => {
            &budget.root_allowance
        }
        _ => prior,
    };
    let allowance = call.continuation_allowance(
        authorization.issued_at_ms,
        authorization.deadline_ms,
        authorization.additional_model_calls,
    )?;
    if prior.allowance_id == allowance.allowance_id
        || (call.grant.resume_policy.is_none()
            && !matches!(call.grant.schema_version, 3..=5)
            && allowance.grant.max_calls > prior.grant.max_calls)
        || (call.grant.resume_policy.is_none()
            && call.grant.work_funding.is_none()
            && allowance.grant.max_duration_ms > policy.grant.max_duration_ms)
        || allowance.grant.max_concurrent > policy.grant.max_concurrent
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
    // Preserve the replaced current allowance separately from the immutable root journal grant.
    project.subscription_call = None;
    subscription::grant_governed_continuation(transaction, &mut project, &completed, &allowance)?;
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
    if !matches!(call.grant.schema_version, 2..=5)
        || prior.dispatch.is_some()
        || prior.allowance_id == call.allowance_id
        || prior.grant.work_item_id != call.grant.work_item_id
        || prior.grant.assignment_id != call.grant.assignment_id
        || prior.grant.assignment_version != call.grant.assignee_authority.assignment_version
        || prior.grant.agent_id != call.grant.assignee_authority.agent_id
        || prior.grant.provider != source.grant.provider
        || prior.grant.model != source.grant.model
        || prior.grant.catalog_digest != source.grant.catalog_digest
        || source.grant.provider != call.grant.provider
        || source.grant.model != call.grant.model
        || source.grant.catalog_digest != call.grant.catalog_digest
        || prior.grant.max_concurrent != 1
        || prior.grant.token_policy != call.grant.token_policy
        || call
            .context
            .source_project
            .abandoned_subscription_calls
            .iter()
            .any(|entry| entry.allowance.allowance_id == prior.allowance_id)
    {
        return Err(unauthorized());
    }
    let effective = source.effective_grant();
    if prior.allowance_id == source.active_provider_allowance_id() {
        if prior.grant.max_duration_ms != effective.max_call_duration_ms
            || prior.grant.expires_at_unix_ms != source.active_deadline_ms()
            || crate::adaptive_leadership_continuation_provider_authority_digest(
                prior,
                &call.grant.assignee_authority,
            )? != effective.provider_authority_digest
        {
            return Err(unauthorized());
        }
    } else if prior.created_at_unix_ms <= effective.created_at_ms
        || prior.allowance_id == source.grant.provider_allowance_id
        || source.continuation.as_ref().is_some_and(|state| {
            state
                .authorizations
                .iter()
                .any(|authorization| authorization.provider_allowance_id == prior.allowance_id)
        })
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
        || !matches!(receipt.grant.schema_version, 2..=5)
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
    validation_scope::memoize(
        connection,
        "governed-allowance",
        &(project, allowance),
        || validate_persisted_governed_allowance_uncached(connection, project, allowance),
    )
}

fn validate_persisted_governed_allowance_uncached(
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
    if receipt.grant.resume_policy.is_none() {
        require_budget_source(connection, &receipt.grant, &receipt.context)
            .map_err(|_| corrupt())?;
    }
    let authorization = receipt.continuation.as_ref().ok_or_else(corrupt)?;
    if receipt.grant.work_funding.is_some() {
        require_funding_authorization_membership(
            connection,
            authorization,
            &receipt.grant.assignee_authority,
        )
        .map_err(|_| corrupt())?;
    }
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
            || !matches!(self.review.grant.schema_version, 2..=5)
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
    /// Read-only source composition validation, including retired historical reviews.
    pub fn validate_continuation_source(&self) -> Result<(), WorkflowError> {
        self.validate_entity()?;
        require_adaptive_allowance_source(self)
    }

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
            || (matches!(self.grant.schema_version, 2..=5)
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
            if self.grant.work_funding.as_ref().is_some_and(|epoch| {
                self.updated_at_unix_ms >= epoch.binding.limits.expires_at_unix_ms
                    || self.continuation.as_ref().map_or(
                        self.updated_at_unix_ms >= self.grant.expires_at_unix_ms,
                        |authorization| self.updated_at_unix_ms >= authorization.deadline_ms,
                    )
            }) {
                return Err(corrupt());
            }
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
        if let Some(adoption) = self
            .continuation
            .as_ref()
            .and_then(|authorization| authorization.local_adoption.as_deref())
        {
            super::adaptive_leadership_local_adoption::require_local_adoption(
                connection, self, adoption,
            )?;
        }
        if self.grant.work_funding.is_some() {
            require_funding_review_membership(
                connection,
                &self.grant,
                &self.context_digest()?,
                self.operation_id,
            )?;
            if let Some(authorization) = &self.continuation {
                require_funding_authorization_membership(
                    connection,
                    authorization,
                    &self.grant.assignee_authority,
                )?;
            }
            return require_budget_source(connection, &self.grant, &self.context);
        }
        if self.grant.resume_policy.is_some() {
            require_resume_review_membership(
                connection,
                &self.grant,
                &self.context_digest()?,
                self.operation_id,
            )?;
            return super::adaptive_accounting_reconsideration::require_designated_review(
                connection, self,
            );
        }
        WorkflowStore::require_recovery_epoch_review(connection, self)?;
        require_budget_source(connection, &self.grant, &self.context)
    }
}

impl WorkflowStore {
    /// System-policy disposition only: no provider reservation or invented model decision.
    pub fn record_adaptive_budget_window_limit(
        &self,
        leader: &AuthenticatedCompanyPrincipalV1,
        grant: &AdaptiveLeadershipReviewGrantV1,
        context: &AdaptiveLeadershipReviewContextV1,
        now_ms: u64,
    ) -> Result<(), WorkflowError> {
        leader.validate()?;
        let Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }) =
            &grant.subject
        else {
            return Err(unauthorized());
        };
        grant.validate(budget.observed_at_ms)?;
        context.validate(grant)?;
        if grant.schema_version != 3
            || grant.recovery_epoch.is_some()
            || *leader != grant.leadership_principal
        {
            return Err(unauthorized());
        }
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_budget_source(&transaction, grant, context)?;
        let project: ProjectV1 = get_entity(
            &transaction,
            &leader.tenant_id,
            "project",
            &grant.project_id.0,
        )?
        .ok_or_else(not_found)?;
        let (session, _) =
            crate::store::adaptive::load(&transaction, grant.session_id)?.ok_or_else(not_found)?;
        crate::store::adaptive::require_head(&transaction, &session)?;
        if project != context.source_project || session != context.source_session {
            return Err(transition());
        }
        // Validate the same accepted policy as review admission, without admitting a call.
        let provisional = AdaptiveLeadershipReviewCallV1 {
            schema_version: 3,
            review_key: grant.review_id.to_string(),
            allowance_id: "budget-limit-no-provider".into(),
            operation_id: grant.review_id,
            grant: grant.clone(),
            context: context.clone(),
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
        require_planning_policy(&transaction, &provisional, &project)?;
        require_subject_time(&provisional, now_ms)?;
        let causes = budget_limit_causes(&transaction, grant, context)?;
        if causes.is_empty() {
            return Err(transition());
        }
        let receipt_id = budget_limit_id(grant.session_id, grant.expected_session_version)?;
        let source_digest = budget_limit_source_digest(grant, context)?;
        if let Some(prior) = get_entity::<AdaptiveBudgetWindowLimitReceiptV1>(
            &transaction,
            &leader.tenant_id,
            BUDGET_LIMIT_KIND,
            &receipt_id,
        )? {
            if prior.source_digest != source_digest
                || prior.causes != causes
                || now_ms < prior.recorded_at_ms
            {
                return Err(transition());
            }
            return Ok(());
        }
        let receipt = AdaptiveBudgetWindowLimitReceiptV1 {
            schema_version: 1,
            disposition: AdaptiveBudgetWindowDispositionV1::SystemPolicyLimit,
            receipt_id: receipt_id.clone(),
            source_digest,
            grant: grant.clone(),
            context: context.clone(),
            causes,
            recorded_at_ms: now_ms,
        };
        receipt.validate_entity()?;
        put_entity(
            &transaction,
            &leader.tenant_id,
            BUDGET_LIMIT_KIND,
            &receipt_id,
            1,
            &receipt,
        )?;
        append_event(
            &transaction,
            leader,
            grant.review_id,
            &canonical_sha256("sentinel.workflow.adaptive-budget-limit.v1", &receipt)?,
            Some(&grant.project_id),
            "adaptive_budget_window_limit_recorded",
            &receipt,
            now_ms,
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn adaptive_budget_window_limit_recorded(
        &self,
        tenant: &TenantId,
        session_id: Uuid,
        session_version: u64,
    ) -> Result<bool, WorkflowError> {
        tenant.validate()?;
        let connection = self.connection.lock().map_err(|_| persistence())?;
        Ok(get_entity::<AdaptiveBudgetWindowLimitReceiptV1>(
            &connection,
            tenant,
            BUDGET_LIMIT_KIND,
            &budget_limit_id(session_id, session_version)?,
        )?
        .is_some())
    }

    pub(crate) fn require_adaptive_budget_review_source(
        connection: &Connection,
        call: &AdaptiveLeadershipReviewCallV1,
    ) -> Result<(), WorkflowError> {
        if call.grant.work_funding.is_some() {
            require_funding_review_membership(
                connection,
                &call.grant,
                &call.context_digest()?,
                call.operation_id,
            )?;
            require_budget_source(connection, &call.grant, &call.context)
        } else if call.grant.resume_policy.is_some() {
            require_resume_review_membership(
                connection,
                &call.grant,
                &call.context_digest()?,
                call.operation_id,
            )
        } else {
            require_budget_source(connection, &call.grant, &call.context)
        }
    }

    /// Build the shared local-adoption audit proposal; this is not proof of a durable append.
    pub fn local_adoption_continuation_audit_proposal(
        call: &AdaptiveLeadershipReviewCallV1,
        result: &CompleteAdaptiveLeadershipReviewCallV1,
    ) -> Result<sentinel_common::AppendProposalV2, WorkflowError> {
        call.validate_completion_proposal(result)?;
        local_adoption_audit_proposal(call, result)
    }

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
        if leader != &grant.leadership_principal
            || operation_id.is_nil()
            || grant.schema_version == 4
        {
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
                || matches!(prior.grant.schema_version, 2..=5)
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
        let call = Self::authorize_new_leadership_review_in_transaction(
            &transaction,
            leader,
            operation_id,
            allowance_id,
            grant,
            &normalized_context,
            now_ms,
        )?;
        transaction.commit()?;
        Ok(call)
    }

    pub(super) fn authorize_new_leadership_review_in_transaction(
        transaction: &Transaction<'_>,
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
        if grant.recovery_epoch.is_some() || grant.schema_version == 4 {
            return Err(unauthorized());
        }
        grant.validate(now_ms)?;
        context.validate(grant)?;
        let already_exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM company_entities WHERE tenant_id=?1 AND entity_kind=?2 AND entity_id=?3)",
            params![leader.tenant_id.0, KIND, grant.review_id.to_string()], |row| row.get(0),
        )?;
        if already_exists {
            return Err(transition());
        }
        let existing = calls_for_session(transaction, &leader.tenant_id, grant.session_id)?;
        if existing
            .iter()
            .any(|call| call.decision.is_none() && call.retired_at_unix_ms.is_none())
        {
            return Err(transition());
        }
        let correction = if grant.work_funding.is_some() {
            None
        } else {
            super::adaptive_accounting_reconsideration::authorized_refusal_exception(
                transaction,
                &existing,
                operation_id,
                allowance_id,
                grant,
                &normalized_context,
                now_ms,
            )?
        };
        if let Some(epoch) = &grant.work_funding {
            epoch.validate()?;
            let binding = &epoch.binding;
            let source = &context.source_session;
            // Completed recovery and extension history remains part of the
            // exact funding source; it is not authority for this new review.
            // Pending reviews are excluded above, and require_budget_source
            // validates the receipt, journal head and continuation lineage.
            if now_ms >= binding.limits.expires_at_unix_ms
                || existing.len() != usize::from(binding.ordinal) - 1
                || existing.len() >= usize::from(binding.limits.total_review_ceiling)
                || source.model_calls >= binding.limits.total_model_call_ceiling
                || source.tool_calls >= binding.limits.total_tool_call_ceiling
                || source
                    .continuation
                    .as_ref()
                    .map_or(0, |state| state.authorizations.len())
                    >= usize::from(binding.limits.total_window_ceiling)
                || existing.iter().any(|call| {
                    call.grant
                        .work_funding
                        .as_ref()
                        .is_some_and(|prior| prior.same_epoch(epoch))
                        && (call.grant.expected_session_version >= grant.expected_session_version
                            || (source.model_calls <= call.context.source_session.model_calls
                                && source.tool_calls <= call.context.source_session.tool_calls))
                })
            {
                return Err(transition());
            }
            require_budget_source(transaction, grant, context)?;
        } else if let Some(binding) = &grant.resume_policy {
            let receipt =
                read_resume_policy_leaf(transaction, &leader.tenant_id, grant.session_id)?
                    .ok_or_else(unauthorized)?;
            receipt.validate_binding(binding)?;
            let expected_count = usize::from(binding.ordinal) - 1;
            if now_ms >= binding.limits.expires_at_unix_ms
                || existing.len() != expected_count
                || existing.len() >= usize::from(binding.limits.total_review_ceiling)
                || context.source_session.model_calls
                    >= context.source_session.grant.max_model_calls
                || context.source_session.tool_calls >= context.source_session.grant.max_tool_calls
                || context
                    .source_session
                    .continuation
                    .as_ref()
                    .map_or(0, |state| state.authorizations.len())
                    >= usize::from(binding.limits.total_window_ceiling)
                || existing
                    .iter()
                    .any(|call| call.decision.is_none() && call.retired_at_unix_ms.is_none())
                || existing.iter().any(|call| {
                    call.grant.resume_policy.as_ref().is_some_and(|prior| {
                        prior.policy_id == binding.policy_id
                            && prior.receipt_digest == binding.receipt_digest
                    }) && call.context.source_session == context.source_session
                        && matches!(
                            call.decision.as_ref().map(|decision| &decision.decision),
                            Some(
                                AdaptiveLeadershipReviewDecisionKindV1::KeepUnknown { .. }
                                    | AdaptiveLeadershipReviewDecisionKindV1::DeferBudget { .. }
                            )
                        )
                        && correction != Some(call.grant.review_id)
                })
            {
                return Err(transition());
            }
        } else {
            if read_resume_policy_leaf(transaction, &leader.tenant_id, grant.session_id)?.is_some()
            {
                return Err(unauthorized());
            }
            let (global_review_limit, head_review_limit, extension_expiry) =
                if grant.schema_version == 3 {
                    budget_review_extension::limits_for_review(transaction, grant, context, now_ms)?
                } else {
                    (
                        ADAPTIVE_LEADERSHIP_MAX_REVIEWS,
                        ADAPTIVE_LEADERSHIP_MAX_REVIEWS,
                        None,
                    )
                };
            if grant.schema_version == 3 {
                require_budget_source(transaction, grant, context)?;
                if context.source_session.model_calls
                    >= context.source_session.grant.max_model_calls
                    || context
                        .source_session
                        .continuation
                        .as_ref()
                        .is_some_and(|state| {
                            state.authorizations.len()
                                >= crate::adaptive::ADAPTIVE_CONTINUATION_MAX_WINDOWS
                        })
                    || (extension_expiry.is_none()
                        && get_entity::<AdaptiveBudgetWindowLimitReceiptV1>(
                            transaction,
                            &leader.tenant_id,
                            BUDGET_LIMIT_KIND,
                            &budget_limit_id(grant.session_id, grant.expected_session_version)?,
                        )?
                        .is_some())
                    || extension_expiry.is_some_and(|expires| grant.expires_at_unix_ms > expires)
                {
                    return Err(transition());
                }
            }
            let same_head: Vec<_> = existing
                .iter()
                .filter(|call| {
                    call.grant.expected_session_version == grant.expected_session_version
                })
                .collect();
            if same_head
                .iter()
                .filter(|call| call.grant.schema_version != 4)
                .count()
                >= head_review_limit
                || (grant.schema_version == 2
                    && existing
                        .iter()
                        .filter(|call| call.grant.schema_version == 2)
                        .count()
                        >= ADAPTIVE_LEADERSHIP_MAX_REVIEWS)
                || (grant.schema_version == 3
                    && existing
                        .iter()
                        .filter(|call| call.grant.schema_version == 3)
                        .count()
                        >= global_review_limit)
                || same_head
                    .iter()
                    .any(|call| call.decision.is_none() && call.retired_at_unix_ms.is_none())
            {
                return Err(transition());
            }
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
        require_current_source(transaction, &call)?;
        if call.grant.work_funding.is_some() {
            insert_funding_review_membership(
                transaction,
                &call.grant,
                &call.context_digest()?,
                call.operation_id,
                now_ms,
            )?;
        } else if call.grant.resume_policy.is_some() {
            insert_resume_review_membership(
                transaction,
                &call.grant,
                call.operation_id,
                &call.context_digest()?,
                now_ms,
            )?;
        }
        store_call(transaction, &call, "adaptive_leadership_review_authorized")?;
        Ok(call)
    }

    pub fn authorize_resume_leadership_review(
        &self,
        leader: &AuthenticatedCompanyPrincipalV1,
        operation_id: Uuid,
        allowance_id: &str,
        grant: &AdaptiveLeadershipReviewGrantV1,
        context: &AdaptiveLeadershipReviewContextV1,
        now_ms: u64,
    ) -> Result<AdaptiveLeadershipReviewCallV1, WorkflowError> {
        if grant.resume_policy.is_none() {
            return Err(unauthorized());
        }
        self.authorize_adaptive_leadership_review_call(
            leader,
            operation_id,
            allowance_id,
            grant,
            context,
            now_ms,
        )
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
        if leader != &call.grant.leadership_principal || !matches!(call.grant.schema_version, 2..=5)
        {
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

    /// Retire expired local admission without depending on the original grant's expiry.
    /// Neither retirement nor terminal replay renews the persisted adoption.
    pub fn retire_expired_adaptive_local_adoption(
        &self,
        leader: &AuthenticatedCompanyPrincipalV1,
        review_id: Uuid,
        expected_version: u64,
        now_ms: u64,
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
        if call.decision.is_some() || call.continuation.is_some() {
            return Err(transition());
        }
        let adoption: crate::AdaptiveLeadershipLocalAdoptionV1 = get_entity(
            &transaction,
            &leader.tenant_id,
            "adaptive_leadership_local_adoption",
            &crate::local_adoption_key(&leader.tenant_id, review_id)?,
        )?
        .ok_or_else(unauthorized)?;
        // Terminal records retain the exact dispatched source; validate that historical binding.
        let mut pending = call.clone();
        pending.version = 2;
        pending.retired_at_unix_ms = None;
        pending.updated_at_unix_ms = pending
            .dispatch
            .as_ref()
            .ok_or_else(transition)?
            .dispatched_at_unix_ms;
        super::adaptive_leadership_local_adoption::require_local_adoption(
            &transaction,
            &pending,
            &adoption,
        )?;
        if now_ms < adoption.request.expires_at_unix_ms {
            return Err(transition());
        }
        if call.retired_at_unix_ms.is_some() {
            if !matches!(expected_version, 2 | 4) {
                return Err(transition());
            }
            return Ok(call);
        }
        if call.version != expected_version || now_ms < call.updated_at_unix_ms {
            return Err(transition());
        }
        let (session, _) = crate::store::adaptive::load(&transaction, call.grant.session_id)?
            .ok_or_else(not_found)?;
        crate::store::adaptive::require_head(&transaction, &session)?;
        if session.continuation.as_ref().is_some_and(|state| {
            state
                .authorizations
                .iter()
                .any(|entry| entry.review_id == review_id)
        }) || (session.version
            == call
                .grant
                .expected_session_version
                .checked_add(1)
                .ok_or_else(transition)?
            && session.grant == call.context.source_session.grant
            && matches!(session.cursor, AdaptiveCursorV1::BlockedResolved { .. }))
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
            && (!matches!(call.grant.schema_version, 2..=5)
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
        let expired_subject = matches!(call.grant.schema_version, 2..=5)
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
        // Share proofs only within this read-only claim preflight, never with dispatch writes.
        let scope = validation_scope::enter(&transaction)?;
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
        if calls_for_session(&transaction, &leader.tenant_id, call.grant.session_id)?
            .iter()
            .any(|other| {
                other.grant.review_id != call.grant.review_id
                    && other.decision.is_none()
                    && other.retired_at_unix_ms.is_none()
            })
        {
            return Err(transition());
        }
        recovery::require_epoch_time(&transaction, &call, now_ms)?;
        scope.finish()?;
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
        self.complete_adaptive_leadership_review_call_impl(leader, result, None, now_ms)
    }

    /// Replay using the actual authoritative envelope read by the trusted daemon from EventStore.
    /// Seals verify integrity, not durable provenance: callers must not supply HTTP proof objects
    /// or synthesized envelopes. The admission clock may expire, never the fixed continuation clock.
    pub fn complete_adaptive_leadership_review_call_with_local_adoption_audit(
        &self,
        leader: &AuthenticatedCompanyPrincipalV1,
        result: &CompleteAdaptiveLeadershipReviewCallV1,
        audit: &sentinel_common::EventEnvelopeV2,
        now_ms: u64,
    ) -> Result<AdaptiveLeadershipReviewCallV1, WorkflowError> {
        self.complete_adaptive_leadership_review_call_impl(leader, result, Some(audit), now_ms)
    }

    fn complete_adaptive_leadership_review_call_impl(
        &self,
        leader: &AuthenticatedCompanyPrincipalV1,
        result: &CompleteAdaptiveLeadershipReviewCallV1,
        audit: Option<&sentinel_common::EventEnvelopeV2>,
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
            if let Some(event) = audit {
                let mut pending = call.clone();
                pending.version = 2;
                pending.updated_at_unix_ms = pending
                    .dispatch
                    .as_ref()
                    .ok_or_else(transition)?
                    .dispatched_at_unix_ms;
                pending.decision = None;
                pending.model_response_digest = None;
                pending.resolution_event_id = None;
                pending.continuation = None;
                require_local_adoption_audit(&pending, result, event, now_ms)?;
            }
            return Ok(call);
        }
        if let Some(event) = audit {
            require_local_adoption_audit(&call, result, event, now_ms)?;
        }
        if let Some(adoption) = result
            .continuation
            .as_ref()
            .and_then(|authorization| authorization.local_adoption.as_deref())
        {
            super::adaptive_leadership_local_adoption::require_local_adoption(
                &transaction,
                &call,
                adoption,
            )?;
            if now_ms < adoption.issued_at_unix_ms
                || (audit.is_none() && now_ms >= adoption.request.expires_at_unix_ms)
            {
                return Err(transition());
            }
        } else {
            recovery::require_epoch_time(
                &transaction,
                &call,
                result
                    .continuation
                    .as_ref()
                    .map_or(now_ms, |authorization| authorization.issued_at_ms),
            )?;
        }
        recovery::require_epoch_completion(&transaction, &call, result)?;
        if matches!(call.grant.schema_version, 2..=5)
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
            require_fresh_resume_review_source(&transaction, &call)?;
            require_budget_source(&transaction, &call.grant, &call.context)?;
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
            if call.grant.work_funding.is_some() {
                require_funding_authorization_membership(
                    &transaction,
                    authorization,
                    &call.grant.assignee_authority,
                )?;
            }
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
    mod resume_policy_tests {
        include!("adaptive_leadership_review/resume_policy_tests.rs");
        mod accounting_reconsideration_tests {
            include!("adaptive_accounting_reconsideration/tests.rs");
        }
    }
    mod work_funding_tests {
        include!("adaptive_leadership_review/work_funding_tests.rs");
    }
    mod recovery_tests {
        include!("adaptive_leadership_review/recovery_tests.rs");

        #[test]
        fn recovery_claim_validation_scope_reuses_epoch_and_rejects_changed_proofs() {
            for target in ["epoch-authority", "epoch-event", "missing-epoch"] {
                let (mut f, retired, now) = recovery_source();
                let request = recovery_request(&mut f, &retired);
                let mut epoch = authorize_recovery(&f, &request, now);
                let call = recovery_call(&f, &epoch);
                {
                    let mut connection = f.store.connection.lock().unwrap();
                    let transaction = connection
                        .transaction_with_behavior(TransactionBehavior::Immediate)
                        .unwrap();
                    let scope = validation_scope::enter(&transaction).unwrap();
                    let loaded: AdaptiveLeadershipReviewCallV1 =
                        get_entity(&transaction, &f.leader.tenant_id, KIND, &call.review_key)
                            .unwrap()
                            .unwrap();
                    assert_eq!(loaded, call);
                    require_current_source(&transaction, &loaded).unwrap();
                    require_subject_time(&loaded, now + 1).unwrap();
                    let entities = validation_scope::validations("entity");
                    // Review, epoch, project, planning call and absent resume policy.
                    assert_eq!(entities, 5);
                    assert_eq!(validation_scope::validations("adaptive-journal"), 1);
                    for _ in 0..3 {
                        recovery::require_epoch_time(&transaction, &loaded, now + 1).unwrap();
                        assert_eq!(validation_scope::validations("entity"), entities);
                    }
                    scope.finish().unwrap();
                    assert_eq!(validation_scope::validations("entity"), 0);
                    transaction.commit().unwrap();
                }
                match target {
                    "epoch-authority" => {
                        epoch.issuer_principal.authority_generation += 1;
                        epoch.issuer_authority.principal_generation += 1;
                        persist_entity(&f.store, &epoch);
                    }
                    "epoch-event" => {
                        let changed = f.store.connection.lock().unwrap().execute(
                            "UPDATE company_events SET payload=X'00'
                             WHERE tenant_id=?1 AND event_type='adaptive_leadership_recovery_epoch_authorized'
                             AND operation_id=?2",
                            params![f.leader.tenant_id.0, request.operation_id.to_string()],
                        ).unwrap();
                        assert_eq!(changed, 1);
                    }
                    "missing-epoch" => {
                        let changed = f
                            .store
                            .connection
                            .lock()
                            .unwrap()
                            .execute(
                                "DELETE FROM company_entities WHERE tenant_id=?1
                             AND entity_kind='adaptive_leadership_recovery_epoch' AND entity_id=?2",
                                params![f.leader.tenant_id.0, epoch.epoch_key],
                            )
                            .unwrap();
                        assert_eq!(changed, 1);
                    }
                    _ => unreachable!(),
                }
                let before = rows(&f.store);
                assert!(
                    f.store
                        .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), now + 1)
                        .is_err(),
                    "accepted changed {target}"
                );
                assert_eq!(validation_scope::validations("entity"), 0);
                assert_eq!(rows(&f.store), before);
                assert_eq!(session(&f), f.context.source_session);
            }
        }
    }

    const CONTINUATION_AT: u64 = 900_002;

    mod budget_review_extension_tests {
        use super::*;

        fn operator(f: &Fixture) -> AuthenticatedCompanyPrincipalV1 {
            let authority =
                PrincipalAuthorityV1::derive("review-extension-operator", 1, &[9; 32]).unwrap();
            AuthenticatedCompanyPrincipalV1 {
                schema_version: 1,
                tenant_id: f.leader.tenant_id.clone(),
                principal_id: authority.principal_id,
                kind: CompanyPrincipalKindV1::Operator,
                role: CompanyRoleV1::ProjectManager,
                customer_id: None,
                agent_id: None,
                authority_generation: authority.principal_generation,
                authority_digest: authority.authority_digest,
            }
        }

        fn limit_fixture() -> (Fixture, u64) {
            let mut f = budget_fixture(4);
            let mut now = CONTINUATION_AT + 7;
            for index in 0..ADAPTIVE_LEADERSHIP_MAX_REVIEWS {
                budget_context(&mut f, now, &format!("extension-base-{index}"));
                let call = authorize_review(&f, now);
                now = call.grant.expires_at_unix_ms;
                f.store
                    .expire_adaptive_leadership_review_call(
                        &f.leader,
                        call.grant.review_id,
                        call.version,
                        now,
                    )
                    .unwrap();
                now += 1;
            }
            budget_context(&mut f, now, "extension-limit");
            f.store
                .record_adaptive_budget_window_limit(&f.leader, &f.grant, &f.context, now)
                .unwrap();
            (f, now)
        }

        fn authorize_review(f: &Fixture, now: u64) -> AdaptiveLeadershipReviewCallV1 {
            f.store
                .authorize_adaptive_leadership_review_call(
                    &f.leader,
                    Uuid::new_v4(),
                    &format!("extension-review-{}", f.grant.review_id),
                    &f.grant,
                    &f.context,
                    now,
                )
                .unwrap()
        }

        fn draft(
            f: &Fixture,
            now: u64,
            extra: u16,
            duration: u64,
        ) -> AdaptiveBudgetReviewExtensionRequestV1 {
            f.store
                .budget_review_extension_draft(
                    &operator(f),
                    &f.grant.project_id,
                    f.grant.session_id,
                    Uuid::new_v4(),
                    extra,
                    "operator-reviewed-evidence",
                    now + duration,
                    now,
                )
                .unwrap()
        }

        fn recorded_request(f: &Fixture, now: u64) -> AdaptiveBudgetReviewExtensionRequestV1 {
            let connection = f.store.connection.lock().unwrap();
            let limit: AdaptiveBudgetWindowLimitReceiptV1 = get_entity(
                &connection,
                &f.leader.tenant_id,
                BUDGET_LIMIT_KIND,
                &budget_limit_id(f.grant.session_id, f.grant.expected_session_version).unwrap(),
            )
            .unwrap()
            .unwrap();
            let calls =
                calls_for_session(&connection, &f.leader.tenant_id, f.grant.session_id).unwrap();
            AdaptiveBudgetReviewExtensionRequestV1 {
                schema_version: 1,
                operation_id: Uuid::new_v4(),
                prior_operation_id: None,
                tenant_id: f.leader.tenant_id.clone(),
                project_id: f.grant.project_id.clone(),
                session_id: f.grant.session_id,
                expected_session_version: f.grant.expected_session_version,
                budget_limit_receipt_digest: canonical_sha256(
                    "sentinel.workflow.adaptive-budget-limit.v1",
                    &limit,
                )
                .unwrap(),
                source_digest: limit.source_digest,
                base_global_review_count: calls
                    .iter()
                    .filter(|call| call.grant.schema_version == 3)
                    .count(),
                base_head_review_count: calls
                    .iter()
                    .filter(|call| {
                        call.grant.expected_session_version == f.grant.expected_session_version
                    })
                    .count(),
                additional_reviews: 1,
                reason_ref: "operator-reviewed-evidence".into(),
                expires_at_unix_ms: now + 3_600_000,
            }
        }

        #[test]
        fn funded_review_preserves_expired_budget_extension_history() {
            let (mut f, now) = limit_fixture();
            let request = draft(&f, now, 1, 120_000);
            let (_, extension) = f
                .store
                .authorize_budget_review_extension(&operator(&f), &request, now)
                .unwrap();
            let now = request.expires_at_unix_ms + 1;
            budget_context(&mut f, now, "fund-after-expired-extension");
            let history = f
                .store
                .adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id)
                .unwrap();
            let source = session(&f);
            let funding = super::work_funding_tests::fund(&f, now);
            super::work_funding_tests::bind(
                &mut f,
                &funding,
                funding.request.source.resume_source.base_review_count + 1,
            );
            let call = super::work_funding_tests::dispatch(&f, now);
            let result = super::work_funding_tests::continued(&call, now + 2, 1);
            f.store
                .complete_adaptive_leadership_review_call(&f.leader, &result, now + 2)
                .unwrap();
            let reopened = WorkflowStore::open(&f.path).unwrap();
            assert!(
                reopened
                    .budget_review_extension(
                        &f.leader.tenant_id,
                        f.grant.session_id,
                        source.version,
                    )
                    .unwrap()
                    == Some(extension)
            );
            let calls = reopened
                .adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id)
                .unwrap();
            assert_eq!(calls.len(), history.len() + 1);
            assert!(history.iter().all(|prior| calls.contains(prior)));
            let adopted = session(&f);
            assert_eq!(adopted.grant, source.grant);
            assert_eq!(
                (adopted.model_calls, adopted.tool_calls),
                (source.model_calls, source.tool_calls)
            );
            assert_eq!(
                adopted.active_work_funding(),
                call.grant.work_funding.as_deref()
            );
        }

        #[test]
        fn budget_review_extension_missing_receipt_and_default_denial_are_read_only() {
            let f = budget_fixture(4);
            let now = CONTINUATION_AT + 7;
            let before = rows(&f.store);
            assert_eq!(
                f.store
                    .budget_review_limits(
                        &f.leader.tenant_id,
                        f.grant.session_id,
                        f.grant.expected_session_version,
                        now,
                    )
                    .unwrap(),
                (3, 3, None)
            );
            assert!(f
                .store
                .budget_review_extension(
                    &f.leader.tenant_id,
                    f.grant.session_id,
                    f.grant.expected_session_version,
                )
                .unwrap()
                .is_none());
            assert!(f
                .store
                .budget_review_extension_draft(
                    &operator(&f),
                    &f.grant.project_id,
                    f.grant.session_id,
                    Uuid::new_v4(),
                    1,
                    "operator-reviewed-evidence",
                    now + 3_600_000,
                    now,
                )
                .is_err());
            assert!(!f
                .store
                .adaptive_budget_window_limit_recorded(
                    &f.leader.tenant_id,
                    f.grant.session_id,
                    f.grant.expected_session_version,
                )
                .unwrap());
            assert_eq!(rows(&f.store), before);

            let (f, now) = limit_fixture();
            let before = rows(&f.store);
            assert!(f
                .store
                .authorize_adaptive_leadership_review_call(
                    &f.leader,
                    Uuid::new_v4(),
                    "no-implicit-extension",
                    &f.grant,
                    &f.context,
                    now,
                )
                .is_err());
            assert_eq!(rows(&f.store), before);
        }

        #[test]
        fn budget_review_extension_exact_extra_reviews_preserve_root_and_old_receipt() {
            for extra in 1..=3 {
                let (mut f, mut now) = limit_fixture();
                let source = session(&f);
                let project = f.context.source_project.clone();
                let prior = f
                    .store
                    .adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id)
                    .unwrap();
                let request = draft(&f, now, extra, 3_600_000);
                assert_eq!(
                    (
                        request.base_global_review_count,
                        request.base_head_review_count
                    ),
                    (3, 3)
                );
                let (replayed, receipt) = f
                    .store
                    .authorize_budget_review_extension(&operator(&f), &request, now)
                    .unwrap();
                assert!(!replayed);
                assert!(receipt.request == request);
                assert_eq!(session(&f), source);
                assert_eq!(
                    f.store
                        .adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id)
                        .unwrap(),
                    prior
                );
                assert_eq!(
                    f.store
                        .company_project(&f.leader.tenant_id, &f.grant.project_id)
                        .unwrap(),
                    Some(project.clone())
                );
                assert_eq!(
                    f.store
                        .budget_review_limits(
                            &f.leader.tenant_id,
                            f.grant.session_id,
                            source.version,
                            now,
                        )
                        .unwrap(),
                    (
                        3 + usize::from(extra),
                        3 + usize::from(extra),
                        Some(request.expires_at_unix_ms)
                    )
                );
                for index in 0..extra {
                    now += 1;
                    budget_context(&mut f, now, &format!("extension-extra-{index}"));
                    let call = authorize_review(&f, now);
                    now = call.grant.expires_at_unix_ms;
                    f.store
                        .expire_adaptive_leadership_review_call(
                            &f.leader,
                            call.grant.review_id,
                            call.version,
                            now,
                        )
                        .unwrap();
                }
                now += 1;
                budget_context(&mut f, now, "extension-exhausted");
                let before = rows(&f.store);
                assert!(f
                    .store
                    .authorize_adaptive_leadership_review_call(
                        &f.leader,
                        Uuid::new_v4(),
                        "extension-no-more",
                        &f.grant,
                        &f.context,
                        now,
                    )
                    .is_err());
                assert_eq!(rows(&f.store), before);
                assert_eq!(session(&f), source);
                assert_eq!(
                    f.store
                        .company_project(&f.leader.tenant_id, &f.grant.project_id)
                        .unwrap(),
                    Some(project)
                );
                let reopened = WorkflowStore::open(&f.path).unwrap();
                let calls = reopened
                    .adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id)
                    .unwrap();
                assert_eq!(
                    calls
                        .iter()
                        .filter(|call| call.grant.schema_version == 3)
                        .count(),
                    3 + usize::from(extra)
                );
                for old in &prior {
                    assert!(calls.iter().any(|call| call == old));
                }
                assert!(reopened
                    .adaptive_budget_window_limit_recorded(
                        &f.leader.tenant_id,
                        f.grant.session_id,
                        source.version,
                    )
                    .unwrap());
                assert!(
                    reopened
                        .budget_review_extension(
                            &f.leader.tenant_id,
                            f.grant.session_id,
                            source.version
                        )
                        .unwrap()
                        .unwrap()
                        == receipt
                );
                let before = rows(&reopened);
                let (replayed, same) = reopened
                    .authorize_budget_review_extension(
                        &operator(&f),
                        &request,
                        request.expires_at_unix_ms + 1,
                    )
                    .unwrap();
                assert!(replayed && same == receipt);
                assert_eq!(rows(&reopened), before);
            }
        }

        #[test]
        fn budget_review_extension_replay_precedes_clock_source_and_never_renews() {
            let (f, now) = limit_fixture();
            let request = draft(&f, now, 1, 3_600_000);
            let (_, receipt) = f
                .store
                .authorize_budget_review_extension(&operator(&f), &request, now)
                .unwrap();
            change_project(&f, now + 1);
            let reopened = WorkflowStore::open(&f.path).unwrap();
            let before = rows(&reopened);
            for replay_at in [
                0,
                now - 1,
                request.expires_at_unix_ms,
                request.expires_at_unix_ms + 1,
            ] {
                let (replayed, same) = reopened
                    .authorize_budget_review_extension(&operator(&f), &request, replay_at)
                    .unwrap();
                assert!(replayed && same == receipt);
            }
            assert_eq!(
                reopened
                    .budget_review_limits(
                        &f.leader.tenant_id,
                        f.grant.session_id,
                        f.grant.expected_session_version,
                        now + 1,
                    )
                    .unwrap(),
                (3, 3, None)
            );
            assert_eq!(
                reopened
                    .budget_review_limits(
                        &f.leader.tenant_id,
                        f.grant.session_id,
                        f.grant.expected_session_version + 1,
                        now + 1,
                    )
                    .unwrap(),
                (3, 3, None)
            );
            for mutation in 0..4 {
                let mut changed = request.clone();
                match mutation {
                    0 => changed.operation_id = Uuid::new_v4(),
                    1 => changed.reason_ref = "another-reason".into(),
                    2 => changed.additional_reviews = 2,
                    _ => changed.expires_at_unix_ms += 1,
                }
                let error = reopened
                    .authorize_budget_review_extension(&operator(&f), &changed, now)
                    .err()
                    .unwrap();
                assert_eq!(error.code, WorkflowErrorCode::IdempotencyConflict);
            }
            let mut changed_issuer = operator(&f);
            changed_issuer.authority_generation += 1;
            let error = reopened
                .authorize_budget_review_extension(&changed_issuer, &request, now)
                .err()
                .unwrap();
            assert_eq!(error.code, WorkflowErrorCode::IdempotencyConflict);
            assert!(reopened
                .authorize_adaptive_leadership_review_call(
                    &f.leader,
                    Uuid::new_v4(),
                    "extension-stale-source",
                    &f.grant,
                    &f.context,
                    now + 1,
                )
                .is_err());
            assert_eq!(rows(&reopened), before);
        }

        #[test]
        fn budget_review_extension_invalid_authority_body_clock_and_source_do_not_write() {
            let (f, now) = limit_fixture();
            let request = draft(&f, now, 1, 3_600_000);
            let before = rows(&f.store);
            for mutation in 0..15 {
                let mut changed = request.clone();
                match mutation {
                    0 => changed.schema_version = 2,
                    1 => changed.operation_id = Uuid::nil(),
                    2 => changed.session_id = Uuid::nil(),
                    3 => changed.expected_session_version += 1,
                    4 => changed.additional_reviews = 0,
                    5 => changed.additional_reviews = 4,
                    6 => changed.reason_ref = "../unsafe reason".into(),
                    7 => changed.expires_at_unix_ms = now + 999,
                    8 => changed.expires_at_unix_ms = now + 86_400_001,
                    9 => changed.base_global_review_count += 1,
                    10 => changed.base_head_review_count += 1,
                    11 => changed.source_digest = "f".repeat(64),
                    12 => changed.budget_limit_receipt_digest = "f".repeat(64),
                    13 => changed.tenant_id = TenantId("foreign-tenant".into()),
                    _ => changed.project_id = ProjectId("foreign-project".into()),
                }
                assert!(
                    f.store
                        .authorize_budget_review_extension(&operator(&f), &changed, now)
                        .is_err(),
                    "mutation {mutation}"
                );
            }
            for invalid_at in [0, now - 1, request.expires_at_unix_ms] {
                assert!(f
                    .store
                    .authorize_budget_review_extension(&operator(&f), &request, invalid_at)
                    .is_err());
            }
            assert!(f
                .store
                .authorize_budget_review_extension(&f.leader, &request, now)
                .is_err());
            let mut wrong_role = operator(&f);
            wrong_role.role = CompanyRoleV1::Developer;
            assert!(f
                .store
                .authorize_budget_review_extension(&wrong_role, &request, now)
                .is_err());
            assert!(f
                .store
                .budget_review_extension_draft(
                    &operator(&f),
                    &ProjectId("foreign-project".into()),
                    f.grant.session_id,
                    Uuid::new_v4(),
                    1,
                    "reason",
                    now + 3_600_000,
                    now,
                )
                .is_err());
            let mut foreign = operator(&f);
            foreign.tenant_id = TenantId("foreign-tenant".into());
            assert!(f
                .store
                .budget_review_extension_draft(
                    &foreign,
                    &f.grant.project_id,
                    f.grant.session_id,
                    Uuid::new_v4(),
                    1,
                    "reason",
                    now + 3_600_000,
                    now,
                )
                .is_err());
            assert_eq!(rows(&f.store), before);
            change_project(&f, now + 1);
            let before = rows(&f.store);
            assert!(f
                .store
                .authorize_budget_review_extension(&operator(&f), &request, now + 1)
                .is_err());
            assert!(f
                .store
                .budget_review_extension_draft(
                    &operator(&f),
                    &f.grant.project_id,
                    f.grant.session_id,
                    Uuid::new_v4(),
                    1,
                    "reason",
                    now + 3_600_000,
                    now + 1,
                )
                .is_err());
            assert_eq!(rows(&f.store), before);
        }

        #[test]
        fn budget_review_extension_expiry_caps_review_and_is_not_a_new_head_budget() {
            let (mut f, now) = limit_fixture();
            let request = draft(&f, now, 1, 1_000);
            f.store
                .authorize_budget_review_extension(&operator(&f), &request, now)
                .unwrap();
            budget_context(&mut f, now + 1, "extension-expiry-bound");
            let before = rows(&f.store);
            assert!(f
                .store
                .authorize_adaptive_leadership_review_call(
                    &f.leader,
                    Uuid::new_v4(),
                    "extension-too-long",
                    &f.grant,
                    &f.context,
                    now + 1,
                )
                .is_err());
            assert_eq!(rows(&f.store), before);
            f.grant.expires_at_unix_ms = request.expires_at_unix_ms;
            let old_head = f.grant.expected_session_version;
            let call = dispatch_budget(&f, now + 1);
            let result = budget_result(&call, 1, now + 3);
            f.store
                .complete_adaptive_leadership_review_call(&f.leader, &result, now + 3)
                .unwrap();
            let current = session(&f);
            assert!(current.version > old_head);
            for head in [old_head, current.version] {
                assert_eq!(
                    f.store
                        .budget_review_limits(
                            &f.leader.tenant_id,
                            f.grant.session_id,
                            head,
                            now + 4
                        )
                        .unwrap(),
                    (3, 3, None)
                );
            }
            let before = rows(&f.store);
            assert!(
                f.store
                    .authorize_budget_review_extension(&operator(&f), &request, now + 4)
                    .unwrap()
                    .0
            );
            assert!(f
                .store
                .adaptive_budget_window_limit_recorded(
                    &f.leader.tenant_id,
                    f.grant.session_id,
                    old_head
                )
                .unwrap());
            assert_eq!(rows(&f.store), before);

            let (mut f, now) = limit_fixture();
            let request = draft(&f, now, 1, 1_000);
            f.store
                .authorize_budget_review_extension(&operator(&f), &request, now)
                .unwrap();
            assert_eq!(
                f.store
                    .budget_review_limits(
                        &f.leader.tenant_id,
                        f.grant.session_id,
                        f.grant.expected_session_version,
                        now - 1
                    )
                    .unwrap(),
                (3, 3, None)
            );
            assert_eq!(
                f.store
                    .budget_review_limits(
                        &f.leader.tenant_id,
                        f.grant.session_id,
                        f.grant.expected_session_version,
                        request.expires_at_unix_ms
                    )
                    .unwrap(),
                (3, 3, None)
            );
            budget_context(&mut f, request.expires_at_unix_ms, "extension-expired");
            let before = rows(&f.store);
            assert!(f
                .store
                .authorize_adaptive_leadership_review_call(
                    &f.leader,
                    Uuid::new_v4(),
                    "extension-expired",
                    &f.grant,
                    &f.context,
                    request.expires_at_unix_ms,
                )
                .is_err());
            assert_eq!(rows(&f.store), before);
        }

        #[test]
        fn budget_review_extension_rejects_active_review_root_and_window_exhaustion() {
            let mut f = budget_fixture(4);
            let mut now = CONTINUATION_AT + 7;
            for index in 0..3 {
                budget_context(&mut f, now, &format!("extension-active-{index}"));
                let call = authorize_review(&f, now);
                if index < 2 {
                    now = call.grant.expires_at_unix_ms;
                    f.store
                        .expire_adaptive_leadership_review_call(
                            &f.leader,
                            call.grant.review_id,
                            call.version,
                            now,
                        )
                        .unwrap();
                    now += 1;
                }
            }
            f.store
                .record_adaptive_budget_window_limit(&f.leader, &f.grant, &f.context, now)
                .unwrap();
            let before = rows(&f.store);
            assert!(f
                .store
                .budget_review_extension_draft(
                    &operator(&f),
                    &f.grant.project_id,
                    f.grant.session_id,
                    Uuid::new_v4(),
                    1,
                    "reason",
                    now + 3_600_000,
                    now
                )
                .is_err());
            assert!(f
                .store
                .authorize_budget_review_extension(&operator(&f), &recorded_request(&f, now), now)
                .is_err());
            assert_eq!(rows(&f.store), before);

            for exhausted_windows in [false, true] {
                let mut f = budget_fixture(if exhausted_windows { 8 } else { 2 });
                let mut now = CONTINUATION_AT + 7;
                if exhausted_windows {
                    for index in 0..2 {
                        let call = dispatch_budget(&f, now);
                        let result = budget_result(&call, 1, now + 2);
                        f.store
                            .complete_adaptive_leadership_review_call(&f.leader, &result, now + 2)
                            .unwrap();
                        observe_budget_inspection(&f, session(&f), now + 3);
                        now += 7;
                        budget_context(&mut f, now, &format!("extension-window-limit-{index}"));
                    }
                }
                f.store
                    .record_adaptive_budget_window_limit(&f.leader, &f.grant, &f.context, now)
                    .unwrap();
                let before = rows(&f.store);
                assert!(f
                    .store
                    .budget_review_extension_draft(
                        &operator(&f),
                        &f.grant.project_id,
                        f.grant.session_id,
                        Uuid::new_v4(),
                        1,
                        "reason",
                        now + 3_600_000,
                        now
                    )
                    .is_err());
                assert!(f
                    .store
                    .authorize_budget_review_extension(
                        &operator(&f),
                        &recorded_request(&f, now),
                        now
                    )
                    .is_err());
                assert_eq!(rows(&f.store), before);
            }
        }

        #[test]
        fn budget_review_extension_last_slot_is_atomic_across_store_connections() {
            let (mut f, now) = limit_fixture();
            let request = draft(&f, now, 1, 3_600_000);
            f.store
                .authorize_budget_review_extension(&operator(&f), &request, now)
                .unwrap();
            let barrier = Arc::new(Barrier::new(2));
            let mut threads = Vec::new();
            for index in 0..2 {
                budget_context(&mut f, now + 1, &format!("extension-racing-{index}"));
                let grant = f.grant.clone();
                let context = f.context.clone();
                let leader = f.leader.clone();
                let store = WorkflowStore::open(&f.path).unwrap();
                let barrier = Arc::clone(&barrier);
                threads.push(std::thread::spawn(move || {
                    barrier.wait();
                    store
                        .authorize_adaptive_leadership_review_call(
                            &leader,
                            Uuid::new_v4(),
                            &format!("extension-racing-{index}"),
                            &grant,
                            &context,
                            now + 1,
                        )
                        .is_ok()
                }));
            }
            let admitted = threads
                .into_iter()
                .map(|thread| usize::from(thread.join().unwrap()))
                .sum::<usize>();
            assert_eq!(admitted, 1);
            let calls = f
                .store
                .adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id)
                .unwrap();
            assert_eq!(
                calls
                    .iter()
                    .filter(|call| call.grant.schema_version == 3)
                    .count(),
                4
            );
            assert_eq!(
                calls
                    .iter()
                    .filter(|call| call.decision.is_none() && call.retired_at_unix_ms.is_none())
                    .count(),
                1
            );
        }

        #[test]
        fn budget_review_extension_mixed_heads_freeze_counts_and_keep_baseline_receipt_valid() {
            let mut f = budget_fixture(6);
            let mut now = CONTINUATION_AT + 7;
            let continued = dispatch_budget(&f, now);
            let result = budget_result(&continued, 1, now + 2);
            f.store
                .complete_adaptive_leadership_review_call(&f.leader, &result, now + 2)
                .unwrap();
            observe_budget_inspection(&f, session(&f), now + 3);
            now += 7;
            for index in 0..2 {
                budget_context(&mut f, now, &format!("extension-mixed-base-{index}"));
                let call = authorize_review(&f, now);
                now = call.grant.expires_at_unix_ms;
                f.store
                    .expire_adaptive_leadership_review_call(
                        &f.leader,
                        call.grant.review_id,
                        call.version,
                        now,
                    )
                    .unwrap();
                now += 1;
            }
            budget_context(&mut f, now, "extension-mixed-limit");
            f.store
                .record_adaptive_budget_window_limit(&f.leader, &f.grant, &f.context, now)
                .unwrap();
            let old_limit = {
                let connection = f.store.connection.lock().unwrap();
                get_entity::<AdaptiveBudgetWindowLimitReceiptV1>(
                    &connection,
                    &f.leader.tenant_id,
                    BUDGET_LIMIT_KIND,
                    &budget_limit_id(f.grant.session_id, f.grant.expected_session_version).unwrap(),
                )
                .unwrap()
                .unwrap()
            };
            assert_eq!(
                old_limit.causes,
                vec![AdaptiveBudgetWindowLimitCauseV1::ReviewLimit]
            );
            let request = draft(&f, now, 1, 3_600_000);
            assert_eq!(
                (
                    request.base_global_review_count,
                    request.base_head_review_count
                ),
                (3, 2)
            );
            f.store
                .authorize_budget_review_extension(&operator(&f), &request, now)
                .unwrap();
            assert_eq!(
                f.store
                    .budget_review_limits(
                        &f.leader.tenant_id,
                        f.grant.session_id,
                        f.grant.expected_session_version,
                        now
                    )
                    .unwrap(),
                (4, 3, Some(request.expires_at_unix_ms))
            );
            now += 1;
            budget_context(&mut f, now, "extension-mixed-extra");
            let call = authorize_review(&f, now);
            now = call.grant.expires_at_unix_ms;
            f.store
                .expire_adaptive_leadership_review_call(
                    &f.leader,
                    call.grant.review_id,
                    call.version,
                    now,
                )
                .unwrap();
            now += 1;
            budget_context(&mut f, now, "extension-mixed-exhausted");
            let before = rows(&f.store);
            assert!(f
                .store
                .authorize_adaptive_leadership_review_call(
                    &f.leader,
                    Uuid::new_v4(),
                    "extension-mixed-no-more",
                    &f.grant,
                    &f.context,
                    now
                )
                .is_err());
            let connection = f.store.connection.lock().unwrap();
            let current = budget_limit_causes(&connection, &f.grant, &f.context).unwrap();
            assert!(current.contains(&AdaptiveBudgetWindowLimitCauseV1::HeadReviewLimit));
            let persisted: AdaptiveBudgetWindowLimitReceiptV1 = get_entity(
                &connection,
                &f.leader.tenant_id,
                BUDGET_LIMIT_KIND,
                &old_limit.receipt_id,
            )
            .unwrap()
            .unwrap();
            assert!(persisted == old_limit);
            drop(connection);
            assert_eq!(rows(&f.store), before);
        }

        #[test]
        fn budget_review_extension_event_failure_rolls_back_issuance() {
            let (f, now) = limit_fixture();
            let request = draft(&f, now, 1, 3_600_000);
            f.store
                .connection
                .lock()
                .unwrap()
                .execute_batch(
                    "CREATE TRIGGER reject_review_extension_event BEFORE INSERT ON company_events
                 WHEN NEW.event_type='adaptive_budget_review_extension_authorized'
                 BEGIN SELECT RAISE(ABORT, 'test extension event failure'); END;",
                )
                .unwrap();
            let before = rows(&f.store);
            assert!(f
                .store
                .authorize_budget_review_extension(&operator(&f), &request, now)
                .is_err());
            assert_eq!(rows(&f.store), before);
            assert!(f
                .store
                .budget_review_extension(
                    &f.leader.tenant_id,
                    f.grant.session_id,
                    f.grant.expected_session_version
                )
                .unwrap()
                .is_none());
        }

        #[test]
        fn budget_review_validation_scope_reuses_the_exact_journal_proof() {
            let (f, now) = limit_fixture();
            let request = draft(&f, now, 1, 3_600_000);
            f.store
                .authorize_budget_review_extension(&operator(&f), &request, now)
                .unwrap();
            let connection = f.store.connection.lock().unwrap();
            validation_scope::with_scope(&connection, || {
                let receipt = f.grant.expected_session_version;
                let first = budget_review_extension::limits_in_connection(
                    &connection,
                    &f.leader.tenant_id,
                    f.grant.session_id,
                    receipt,
                    now,
                )?;
                let journals = validation_scope::validations("adaptive-journal");
                assert!(journals > 0);
                let second = budget_review_extension::limits_in_connection(
                    &connection,
                    &f.leader.tenant_id,
                    f.grant.session_id,
                    receipt,
                    now,
                )?;
                assert_eq!(first, second);
                assert_eq!(validation_scope::validations("adaptive-journal"), journals);
                Ok(())
            })
            .unwrap();
        }

        #[test]
        fn budget_review_validation_scope_rejects_corrupt_old_proof_in_a_new_operation() {
            for target in ["review", "event", "journal"] {
                let (f, now) = limit_fixture();
                let request = draft(&f, now, 1, 3_600_000);
                f.store
                    .authorize_budget_review_extension(&operator(&f), &request, now)
                    .unwrap();
                assert!(f
                    .store
                    .budget_review_limits(
                        &f.leader.tenant_id,
                        f.grant.session_id,
                        f.grant.expected_session_version,
                        now,
                    )
                    .is_ok());
                let connection = f.store.connection.lock().unwrap();
                let changed = match target {
                    "review" => connection.execute(
                        "UPDATE company_entities SET payload=X'00' WHERE rowid=(SELECT rowid FROM company_entities WHERE entity_kind='adaptive_leadership_review_call' LIMIT 1)", [],
                    ).unwrap(),
                    "event" => connection.execute(
                        "UPDATE company_events SET payload=X'00' WHERE sequence=(SELECT MIN(sequence) FROM company_events WHERE event_type='adaptive_budget_review_extension_authorized')", [],
                    ).unwrap(),
                    "journal" => connection.execute(
                        "UPDATE workflow_operations SET response=X'00' WHERE operation_namespace=?1",
                        [format!("adaptive-session-v1:{}", f.grant.session_id)],
                    ).unwrap(),
                    _ => unreachable!(),
                };
                assert!(changed > 0);
                drop(connection);
                assert!(
                    f.store
                        .budget_review_limits(
                            &f.leader.tenant_id,
                            f.grant.session_id,
                            f.grant.expected_session_version,
                            now,
                        )
                        .is_err(),
                    "tampered {target} was accepted"
                );
            }
        }
    }

    fn budget_context(f: &mut Fixture, now: u64, evidence: &str) {
        let source = session(f);
        let project = f
            .store
            .company_project(&f.leader.tenant_id, &f.grant.project_id)
            .unwrap()
            .unwrap();
        let original = f
            .store
            .historical_adaptive_provider_project(&source.grant, source.grant.created_at_ms)
            .unwrap()
            .unwrap();
        let budget = crate::AdaptiveBudgetWindowAuthorityV1 {
            schema_version: 1,
            root_allowance: original.subscription_call.unwrap(),
            active_allowance_digest: crate::adaptive_budget_allowance_digest(
                project.subscription_call.as_ref().unwrap(),
            )
            .unwrap(),
            continuation_history_digest: crate::adaptive_budget_history_digest(
                &source.continuation,
            )
            .unwrap(),
            observed_at_ms: now,
            model_calls_exhausted: source.model_calls >= source.active_model_ceiling(),
            deadline_expired: now >= source.active_deadline_ms(),
            dispatch_slack_insufficient: source.model_admission_at(now)
                == crate::AdaptiveModelAdmissionV1::InsufficientSlack,
        };
        f.context.source_project = project;
        f.context.source_session = source.clone();
        f.context.evidence_refs = vec![
            format!(
                "adaptive-budget-root:{}:{}",
                budget.root_allowance.allowance_id, source.grant.provider_authority_digest
            ),
            format!("adaptive-budget-current:{}", budget.active_allowance_digest),
            format!(
                "adaptive-budget-history:{}",
                budget.continuation_history_digest
            ),
            evidence.into(),
        ];
        if let Some(observation) = &source.last_observation {
            f.context.evidence_refs.push(format!(
                "workbench-observation:{}:{}",
                observation.effect.id, observation.observation_digest
            ));
        }
        f.grant.schema_version = 3;
        f.grant.expected_session_version = source.version;
        f.grant.expected_project_version = f.context.source_project.version;
        f.grant.expected_reason_code.clear();
        f.grant.subject = Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted {
            budget: Box::new(budget),
        });
        f.grant.recovery_epoch = None;
        f.grant.expires_at_unix_ms = now + 120_000;
        rebind_budget_evidence(f);
    }

    fn rebind_budget_evidence(f: &mut Fixture) {
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
    }

    fn observe_budget_inspection(
        f: &Fixture,
        mut source: crate::AdaptiveSessionV1,
        now: u64,
    ) -> crate::AdaptiveSessionV1 {
        let effect = AdaptiveEffectV1 {
            id: Uuid::new_v4(),
            request_digest: DIGEST.into(),
        };
        let tool = sentinel_common::WorkbenchTool::InspectFile {
            path: "src/lib.rs".into(),
            max_bytes: 128,
        };
        let tool_digest = crate::adaptive_tool_digest(&tool).unwrap();
        let commands = [
            AdaptiveTransitionV1::ClaimModel {
                effect: effect.clone(),
                previous_observation_digest: source
                    .last_observation
                    .as_ref()
                    .map(|o| o.observation_digest.clone()),
            },
            AdaptiveTransitionV1::ResolveModel {
                effect,
                result_digest: DIGEST.into(),
                decision: AdaptiveModelDecisionV1::Tool {
                    tool,
                    tool_digest: tool_digest.clone(),
                },
            },
        ];
        for (offset, command) in commands.iter().enumerate() {
            source = f
                .store
                .advance_adaptive_session(
                    source.grant.session_id,
                    source.version,
                    Uuid::new_v4(),
                    command,
                    &f.grant.assignee_authority,
                    now + offset as u64,
                )
                .unwrap()
                .1;
        }
        let effect = AdaptiveEffectV1 {
            id: Uuid::new_v4(),
            request_digest: DIGEST.into(),
        };
        source = f
            .store
            .advance_adaptive_session(
                source.grant.session_id,
                source.version,
                Uuid::new_v4(),
                &AdaptiveTransitionV1::ClaimTool {
                    effect: effect.clone(),
                    tool_digest,
                },
                &f.grant.assignee_authority,
                now + 2,
            )
            .unwrap()
            .1;
        f.store
            .advance_adaptive_session(
                source.grant.session_id,
                source.version,
                Uuid::new_v4(),
                &AdaptiveTransitionV1::ObserveTool {
                    observation: crate::AdaptiveObservationRefV1 {
                        effect,
                        observation_digest: DIGEST.into(),
                    },
                },
                &f.grant.assignee_authority,
                now + 3,
            )
            .unwrap()
            .1
    }

    fn budget_fixture(root_calls: u16) -> Fixture {
        budget_fixture_with_window(root_calls, 120_000)
    }

    fn budget_fixture_with_window(root_calls: u16, window: u64) -> Fixture {
        let mut f = continuation_fixture_with_calls(false, false, root_calls);
        let call = dispatch_continuation(&f);
        let result = continue_result_with_window(&call, window);
        f.store
            .complete_adaptive_leadership_review_call(&f.leader, &result, CONTINUATION_AT + 2)
            .unwrap();
        let source = observe_budget_inspection(&f, session(&f), CONTINUATION_AT + 3);
        assert!(matches!(source.cursor, AdaptiveCursorV1::ReadyForModel));
        assert_eq!(source.model_calls, 2);
        budget_context(&mut f, CONTINUATION_AT + 7, "normal-budget-review");
        f
    }

    fn dispatch_budget(f: &Fixture, now: u64) -> AdaptiveLeadershipReviewCallV1 {
        let call = f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                Uuid::new_v4(),
                &format!("normal-review-{}", f.grant.review_id),
                &f.grant,
                &f.context,
                now,
            )
            .unwrap();
        f.store
            .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), now + 1)
            .unwrap()
    }

    #[test]
    fn leadership_claim_validation_scope_bounds_repeated_compound_proofs() {
        let f = budget_fixture(4);
        let now = CONTINUATION_AT + 7;
        let call = f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                Uuid::new_v4(),
                "claim-proof-count",
                &f.grant,
                &f.context,
                now,
            )
            .unwrap();
        {
            let mut connection = f.store.connection.lock().unwrap();
            // A new transaction must build its own proofs, even on the same connection.
            for _ in 0..2 {
                let transaction = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .unwrap();
                let scope = validation_scope::enter(&transaction).unwrap();
                assert_eq!(validation_scope::validations("adaptive-journal"), 0);
                let loaded: AdaptiveLeadershipReviewCallV1 =
                    get_entity(&transaction, &f.leader.tenant_id, KIND, &call.review_key)
                        .unwrap()
                        .unwrap();
                assert_eq!(loaded, call);
                assert_eq!(validation_scope::validations("adaptive-journal"), 1);
                require_current_source(&transaction, &loaded).unwrap();
                require_subject_time(&loaded, now + 1).unwrap();
                recovery::require_epoch_time(&transaction, &loaded, now + 1).unwrap();
                assert_eq!(validation_scope::validations("adaptive-journal"), 1);
                let entities = validation_scope::validations("entity");
                let budgets = validation_scope::validations("budget-source");
                // Current/prior reviews, absent epoch/policy, project, abandonment and planning.
                assert_eq!(entities, 7);
                assert_eq!(budgets, 2);
                assert_eq!(validation_scope::validations("historical-project"), 1);
                assert_eq!(validation_scope::validations("governed-allowance"), 1);
                for _ in 0..3 {
                    let repeated: AdaptiveLeadershipReviewCallV1 =
                        get_entity(&transaction, &f.leader.tenant_id, KIND, &call.review_key)
                            .unwrap()
                            .unwrap();
                    assert_eq!(repeated, loaded);
                    require_current_source(&transaction, &repeated).unwrap();
                    require_subject_time(&repeated, now + 1).unwrap();
                    recovery::require_epoch_time(&transaction, &repeated, now + 1).unwrap();
                    assert_eq!(validation_scope::validations("adaptive-journal"), 1);
                    assert_eq!(validation_scope::validations("entity"), entities);
                    assert_eq!(validation_scope::validations("budget-source"), budgets);
                    assert_eq!(validation_scope::validations("historical-project"), 1);
                    assert_eq!(validation_scope::validations("governed-allowance"), 1);
                }
                scope.finish().unwrap();
                assert_eq!(validation_scope::validations("adaptive-journal"), 0);
                transaction.commit().unwrap();
            }
        }
        let dispatched = f
            .store
            .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), now + 1)
            .unwrap();
        assert_eq!(dispatched.version, 2);
        assert_eq!(dispatched.grant, call.grant);
        assert_eq!(dispatched.context, call.context);
        assert_eq!(session(&f), f.context.source_session);
        assert_eq!(
            f.store
                .company_project(&f.leader.tenant_id, &f.grant.project_id)
                .unwrap(),
            Some(f.context.source_project.clone())
        );
        assert_eq!(
            f.store
                .adaptive_leadership_review_call(&f.leader.tenant_id, call.grant.review_id)
                .unwrap(),
            Some(dispatched)
        );
        let before = rows(&f.store);
        assert!(f
            .store
            .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), now + 2)
            .is_err());
        assert_eq!(rows(&f.store), before);
    }

    #[test]
    fn leadership_claim_validation_scope_rejects_corruption_after_failed_preflight() {
        for target in ["review", "project", "planning", "journal"] {
            let f = budget_fixture(4);
            let now = CONTINUATION_AT + 7;
            let call = f
                .store
                .authorize_adaptive_leadership_review_call(
                    &f.leader,
                    Uuid::new_v4(),
                    "claim-corrupt-proof",
                    &f.grant,
                    &f.context,
                    now,
                )
                .unwrap();
            let mut changed_leader = f.leader.clone();
            changed_leader.authority_generation += 1;
            let before = rows(&f.store);
            assert!(f
                .store
                .claim_adaptive_leadership_review_call(&changed_leader, &claim(&call), now + 1)
                .is_err());
            assert_eq!(rows(&f.store), before);
            assert_eq!(validation_scope::validations("entity"), 0);
            {
                let connection = f.store.connection.lock().unwrap();
                let changed = match target {
                    "review" | "project" | "planning" => {
                        let (kind, id) = match target {
                            "review" => (KIND, call.review_key.as_str()),
                            "project" => ("project", f.grant.project_id.0.as_str()),
                            "planning" => ("project_planning_call", f.grant.project_id.0.as_str()),
                            _ => unreachable!(),
                        };
                        connection
                            .execute(
                                "UPDATE company_entities SET payload=X'00'
                             WHERE tenant_id=?1 AND entity_kind=?2 AND entity_id=?3",
                                params![f.leader.tenant_id.0, kind, id],
                            )
                            .unwrap()
                    }
                    "journal" => connection
                        .execute(
                            "UPDATE workflow_operations SET response=X'00' WHERE rowid=(
                         SELECT rowid FROM workflow_operations WHERE operation_namespace=?1
                         ORDER BY operation_id LIMIT 1)",
                            [format!("adaptive-session-v1:{}", f.grant.session_id)],
                        )
                        .unwrap(),
                    _ => unreachable!(),
                };
                assert_eq!(changed, 1);
            }
            let before = rows(&f.store);
            assert!(
                f.store
                    .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), now + 1)
                    .is_err(),
                "accepted corrupt {target}"
            );
            assert_eq!(validation_scope::validations("entity"), 0);
            assert_eq!(rows(&f.store), before);
        }
    }

    #[test]
    fn leadership_claim_validation_scope_write_failure_rolls_back_and_allows_retry() {
        let f = fixture();
        let call = authorize(&f);
        f.store
            .connection
            .lock()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER reject_claim_event BEFORE INSERT ON company_events
             WHEN NEW.event_type='adaptive_leadership_review_dispatched'
             BEGIN SELECT RAISE(ABORT, 'test claim event failure'); END;",
            )
            .unwrap();
        let before = rows(&f.store);
        assert!(f
            .store
            .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), AUTHORIZED_AT + 1)
            .is_err());
        assert_eq!(validation_scope::validations("entity"), 0);
        assert_eq!(rows(&f.store), before);
        assert_eq!(
            f.store
                .adaptive_leadership_review_call(&f.leader.tenant_id, call.grant.review_id)
                .unwrap(),
            Some(call.clone())
        );
        f.store
            .connection
            .lock()
            .unwrap()
            .execute_batch("DROP TRIGGER reject_claim_event")
            .unwrap();
        let dispatched = f
            .store
            .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), AUTHORIZED_AT + 1)
            .unwrap();
        assert_eq!(dispatched.version, 2);
        assert_eq!(dispatched.grant, call.grant);
        assert_eq!(dispatched.context, call.context);
        assert_eq!(validation_scope::validations("entity"), 0);
        assert_eq!(session(&f), f.context.source_session);
    }

    fn budget_result(
        call: &AdaptiveLeadershipReviewCallV1,
        calls: u16,
        issued: u64,
    ) -> CompleteAdaptiveLeadershipReviewCallV1 {
        let mut result = completion(call, false);
        result.decision = AdaptiveLeadershipReviewDecisionV1 {
            schema_version: 3,
            decision: AdaptiveLeadershipReviewDecisionKindV1::Continue {
                additional_model_calls: calls,
                window_ms: 120_000,
                rationale: "Use the verified unspent original budget".into(),
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
            .continuation_allowance(issued, issued + 120_000, calls)
            .unwrap();
        let Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }) =
            &call.grant.subject
        else {
            panic!("budget subject")
        };
        result.resolution_event_id = Some(audit);
        result.continuation = Some(crate::AdaptiveContinuationAuthorizationV1 {
            schema_version: 1,
            operation_id: call.operation_id,
            review_id: call.grant.review_id,
            resolution_event_id: audit,
            session_id: call.grant.session_id,
            source_session_version: call.grant.expected_session_version,
            source: crate::AdaptiveContinuationSourceV1::BudgetWindowExhausted {
                active_allowance_digest: budget.active_allowance_digest.clone(),
                continuation_history_digest: budget.continuation_history_digest.clone(),
            },
            abandoned_model_effect: None,
            provider_allowance_id: allowance.allowance_id.clone(),
            provider_authority_digest: adaptive_leadership_continuation_provider_authority_digest(
                &allowance,
                &call.grant.assignee_authority,
            )
            .unwrap(),
            issued_at_ms: issued,
            deadline_ms: issued + 120_000,
            additional_model_calls: calls,
            local_adoption: None,
            resume_policy: None,
            work_funding: None,
        });
        result
    }

    #[test]
    fn schema3_current_single_call_window_can_continue_with_multiple_verified_root_calls() {
        let f = budget_fixture(4);
        assert_eq!(
            f.context
                .source_project
                .subscription_call
                .as_ref()
                .unwrap()
                .grant
                .max_calls,
            1
        );
        assert!(CONTINUATION_AT + 7 < f.context.source_session.active_deadline_ms());
        let call = dispatch_budget(&f, CONTINUATION_AT + 7);
        let result = budget_result(&call, 2, CONTINUATION_AT + 9);
        // An interrupted pre-domain audit retains its fixed proposal across reopen.
        call.validate_completion_proposal(&result).unwrap();
        let reopened = WorkflowStore::open(&f.path).unwrap();
        let completed = reopened
            .complete_adaptive_leadership_review_call(&f.leader, &result, CONTINUATION_AT + 10)
            .unwrap();
        let continued = reopened
            .adaptive_session(f.grant.session_id, &f.grant.assignee_authority)
            .unwrap()
            .unwrap();
        assert_eq!(continued.grant, f.context.source_session.grant);
        assert_eq!(continued.model_calls, 2);
        assert_eq!(continued.tool_calls, f.context.source_session.tool_calls);
        assert_eq!(
            continued.last_observation,
            f.context.source_session.last_observation
        );
        assert_eq!(continued.active_model_ceiling(), 4);
        assert!(continued.requires_fresh_observation());
        let project = reopened
            .company_project(&f.leader.tenant_id, &f.grant.project_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            project.subscription_call.as_ref().unwrap().grant.max_calls,
            2
        );
        let before = rows(&reopened);
        assert_eq!(
            reopened
                .complete_adaptive_leadership_review_call(
                    &f.leader,
                    &result,
                    CONTINUATION_AT + 900_000
                )
                .unwrap(),
            completed
        );
        assert_eq!(rows(&reopened), before);
        let advanced = observe_budget_inspection(&f, continued, CONTINUATION_AT + 12);
        assert!(
            advanced.version
                > completed
                    .continuation
                    .as_ref()
                    .unwrap()
                    .source_session_version
                    + 1
        );
        let before = rows(&reopened);
        assert_eq!(
            reopened
                .complete_adaptive_leadership_review_call(
                    &f.leader,
                    &result,
                    CONTINUATION_AT + 900_000
                )
                .unwrap(),
            completed
        );
        assert_eq!(rows(&reopened), before);
        assert_eq!(
            reopened
                .adaptive_session(f.grant.session_id, &f.grant.assignee_authority)
                .unwrap(),
            Some(advanced)
        );
        {
            let authorization = completed.continuation.as_ref().unwrap();
            let allowance = completed
                .continuation_allowance(
                    authorization.issued_at_ms,
                    authorization.deadline_ms,
                    authorization.additional_model_calls,
                )
                .unwrap();
            let mut connection = reopened.connection.lock().unwrap();
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .unwrap();
            let (replayed, response) =
                crate::store::adaptive::continue_adaptive_session_in_transaction(
                    &transaction,
                    authorization,
                    &completed,
                    &allowance,
                    &f.grant.assignee_authority,
                    CONTINUATION_AT + 900_000,
                )
                .unwrap();
            assert!(replayed);
            assert_eq!(response.version, authorization.source_session_version + 1);
            assert_eq!(response.model_calls, 2);
        }
        assert_eq!(rows(&reopened), before);
        let old = f
            .store
            .adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id)
            .unwrap();
        assert_eq!(
            old.iter()
                .filter(|call| call.grant.schema_version == 2)
                .count(),
            1
        );
    }

    #[test]
    fn schema3_normal_review_budget_is_separate_from_three_old_schema2_reviews() {
        let mut f = continuation_fixture(false, false);
        let mut now = CONTINUATION_AT;
        for index in 0..2 {
            f.context
                .evidence_refs
                .push(format!("old-review-expiry-{index}"));
            rebind_budget_evidence(&mut f);
            f.grant.expires_at_unix_ms = now + 120_000;
            let call = f
                .store
                .authorize_adaptive_leadership_review_call(
                    &f.leader,
                    Uuid::new_v4(),
                    &format!("old-review-{index}"),
                    &f.grant,
                    &f.context,
                    now,
                )
                .unwrap();
            now = call.grant.expires_at_unix_ms;
            f.store
                .expire_adaptive_leadership_review_call(&f.leader, call.grant.review_id, 1, now)
                .unwrap();
            now += 1;
        }
        f.context
            .evidence_refs
            .push("old-review-third-continue".into());
        rebind_budget_evidence(&mut f);
        f.grant.expires_at_unix_ms = now + 120_000;
        let call = f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                Uuid::new_v4(),
                "old-review-third",
                &f.grant,
                &f.context,
                now,
            )
            .unwrap();
        let call = f
            .store
            .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), now + 1)
            .unwrap();
        let result = continue_result_with_window_at(&call, 120_000, now + 2);
        f.store
            .complete_adaptive_leadership_review_call(&f.leader, &result, now + 2)
            .unwrap();
        observe_budget_inspection(&f, session(&f), now + 3);
        now += 7;
        budget_context(&mut f, now, "normal-after-three-old-reviews");
        let call = dispatch_budget(&f, now);
        let result = budget_result(&call, 2, now + 2);
        f.store
            .complete_adaptive_leadership_review_call(&f.leader, &result, now + 2)
            .unwrap();
        let calls = f
            .store
            .adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id)
            .unwrap();
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.grant.schema_version == 2)
                .count(),
            3
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.grant.schema_version == 3)
                .count(),
            1
        );
        assert_eq!(session(&f).active_model_ceiling(), 4);
    }

    #[test]
    fn schema3_forged_root_history_and_observation_sources_fail_before_reservation() {
        for changed in 0..3 {
            let mut f = budget_fixture(4);
            let before = rows(&f.store);
            match changed {
                0 => {
                    let Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }) =
                        &mut f.grant.subject
                    else {
                        panic!("budget")
                    };
                    budget.root_allowance.created_by = "forged-leader".into();
                }
                1 => {
                    let Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }) =
                        &mut f.grant.subject
                    else {
                        panic!("budget")
                    };
                    let old = format!(
                        "adaptive-budget-history:{}",
                        budget.continuation_history_digest
                    );
                    budget.continuation_history_digest = "b".repeat(64);
                    for reference in &mut f.context.evidence_refs {
                        if *reference == old {
                            *reference = format!(
                                "adaptive-budget-history:{}",
                                budget.continuation_history_digest
                            );
                        }
                    }
                }
                _ => {
                    let observation = f.context.source_session.last_observation.as_mut().unwrap();
                    let old = format!(
                        "workbench-observation:{}:{}",
                        observation.effect.id, observation.observation_digest
                    );
                    observation.observation_digest = "c".repeat(64);
                    for reference in &mut f.context.evidence_refs {
                        if *reference == old {
                            *reference = format!(
                                "workbench-observation:{}:{}",
                                observation.effect.id, observation.observation_digest
                            );
                        }
                    }
                }
            }
            rebind_budget_evidence(&mut f);
            assert!(f
                .store
                .authorize_adaptive_leadership_review_call(
                    &f.leader,
                    Uuid::new_v4(),
                    "forged-budget",
                    &f.grant,
                    &f.context,
                    CONTINUATION_AT + 7
                )
                .is_err());
            assert_eq!(rows(&f.store), before);
        }
    }

    #[test]
    fn schema3_defer_budget_is_an_immutable_model_refusal_without_new_employee_authority() {
        let f = budget_fixture(4);
        let call = dispatch_budget(&f, CONTINUATION_AT + 7);
        let mut result = completion(&call, false);
        result.decision = AdaptiveLeadershipReviewDecisionV1 {
            schema_version: 3,
            decision: AdaptiveLeadershipReviewDecisionKindV1::DeferBudget {
                rationale: "Do not spend more budget on this head".into(),
                evidence_refs: vec![call.context.evidence_refs[0].clone()],
            },
        };
        let receipt = f
            .store
            .complete_adaptive_leadership_review_call(&f.leader, &result, CONTINUATION_AT + 9)
            .unwrap();
        assert_eq!(session(&f), f.context.source_session);
        assert_eq!(
            f.store
                .company_project(&f.leader.tenant_id, &f.grant.project_id)
                .unwrap(),
            Some(f.context.source_project.clone())
        );
        assert!(receipt.continuation.is_none());
        assert!(receipt.resolution_event_id.is_none());
        let reopened = WorkflowStore::open(&f.path).unwrap();
        let before = rows(&reopened);
        assert_eq!(
            reopened
                .complete_adaptive_leadership_review_call(
                    &f.leader,
                    &result,
                    CONTINUATION_AT + 900_000
                )
                .unwrap(),
            receipt
        );
        assert_eq!(rows(&reopened), before);
        result.model_response_digest = "d".repeat(64);
        assert!(reopened
            .complete_adaptive_leadership_review_call(&f.leader, &result, CONTINUATION_AT + 10)
            .is_err());
        assert_eq!(rows(&reopened), before);
    }

    #[test]
    fn schema3_short_prior_window_does_not_cap_new_normal_duration() {
        let f = budget_fixture_with_window(4, 1_000);
        assert_eq!(
            f.context
                .source_project
                .subscription_call
                .as_ref()
                .unwrap()
                .grant
                .max_duration_ms,
            1_000
        );
        let call = dispatch_budget(&f, CONTINUATION_AT + 7);
        let result = budget_result(&call, 2, CONTINUATION_AT + 9);
        let allowance = call
            .continuation_allowance(CONTINUATION_AT + 9, CONTINUATION_AT + 120_009, 2)
            .unwrap();
        assert_eq!(allowance.grant.max_duration_ms, 120_000);
        f.store
            .complete_adaptive_leadership_review_call(&f.leader, &result, CONTINUATION_AT + 9)
            .unwrap();
        let reopened = WorkflowStore::open(&f.path).unwrap();
        let project = reopened
            .company_project(&f.leader.tenant_id, &f.grant.project_id)
            .unwrap()
            .unwrap();
        assert_eq!(project.subscription_call, Some(allowance));
        assert_eq!(
            reopened
                .adaptive_session(f.grant.session_id, &f.grant.assignee_authority)
                .unwrap()
                .unwrap()
                .model_calls,
            2
        );
    }

    #[test]
    fn schema3_deadline_only_exhaustion_can_continue_without_resetting_unused_calls() {
        let mut f = budget_fixture(4);
        let call = dispatch_budget(&f, CONTINUATION_AT + 7);
        let result = budget_result(&call, 2, CONTINUATION_AT + 9);
        f.store
            .complete_adaptive_leadership_review_call(&f.leader, &result, CONTINUATION_AT + 9)
            .unwrap();
        let now = result.continuation.as_ref().unwrap().deadline_ms;
        budget_context(&mut f, now, "normal-deadline-only");
        let Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }) =
            &f.grant.subject
        else {
            panic!("budget")
        };
        assert!(!budget.model_calls_exhausted);
        assert!(budget.deadline_expired);
        let source = session(&f);
        let call = dispatch_budget(&f, now);
        let result = budget_result(&call, 2, now + 2);
        f.store
            .complete_adaptive_leadership_review_call(&f.leader, &result, now + 2)
            .unwrap();
        let continued = session(&f);
        assert_eq!(continued.model_calls, source.model_calls);
        assert_eq!(continued.tool_calls, source.tool_calls);
        assert_eq!(continued.grant, source.grant);
        assert_eq!(continued.active_model_ceiling(), 4);
        assert_eq!(
            continued
                .continuation
                .as_ref()
                .unwrap()
                .authorizations
                .len(),
            3
        );
        assert!(continued.requires_fresh_observation());
    }

    #[test]
    fn schema3_retired_normal_reviews_count_toward_limit_and_system_receipt_replays() {
        let mut f = budget_fixture(4);
        let mut now = CONTINUATION_AT + 7;
        for index in 0..ADAPTIVE_LEADERSHIP_MAX_REVIEWS {
            budget_context(&mut f, now, &format!("normal-retired-{index}"));
            let call = f
                .store
                .authorize_adaptive_leadership_review_call(
                    &f.leader,
                    Uuid::new_v4(),
                    &format!("normal-retired-{index}"),
                    &f.grant,
                    &f.context,
                    now,
                )
                .unwrap();
            now = call.grant.expires_at_unix_ms;
            let retired = f
                .store
                .expire_adaptive_leadership_review_call(&f.leader, call.grant.review_id, 1, now)
                .unwrap();
            assert_eq!(retired.version, 4);
            assert!(retired.decision.is_none());
            now += 1;
        }
        budget_context(&mut f, now, "normal-review-limit");
        let source = session(&f);
        let project = f.context.source_project.clone();
        assert!(f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                Uuid::new_v4(),
                "normal-over-limit",
                &f.grant,
                &f.context,
                now
            )
            .is_err());
        f.store
            .record_adaptive_budget_window_limit(&f.leader, &f.grant, &f.context, now)
            .unwrap();
        assert_eq!(session(&f), source);
        assert_eq!(
            f.store
                .company_project(&f.leader.tenant_id, &f.grant.project_id)
                .unwrap(),
            Some(project)
        );
        let reopened = WorkflowStore::open(&f.path).unwrap();
        assert!(reopened
            .adaptive_budget_window_limit_recorded(
                &f.leader.tenant_id,
                f.grant.session_id,
                source.version
            )
            .unwrap());
        let before = rows(&reopened);
        reopened
            .record_adaptive_budget_window_limit(&f.leader, &f.grant, &f.context, now + 1)
            .unwrap();
        assert_eq!(rows(&reopened), before);
        reopened
            .record_adaptive_budget_window_limit(&f.leader, &f.grant, &f.context, now + 900_000)
            .unwrap();
        assert_eq!(rows(&reopened), before);
        let calls = reopened
            .adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id)
            .unwrap();
        assert_eq!(
            calls.iter().filter(|c| c.grant.schema_version == 3).count(),
            3
        );
        assert_eq!(
            calls.iter().filter(|c| c.grant.schema_version == 2).count(),
            1
        );
    }

    #[test]
    fn schema3_zero_root_remaining_records_limit_without_review_or_model_decision() {
        let f = budget_fixture(2);
        let source = session(&f);
        assert_eq!(source.model_calls, source.grant.max_model_calls);
        let calls = f
            .store
            .adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id)
            .unwrap();
        f.store
            .record_adaptive_budget_window_limit(
                &f.leader,
                &f.grant,
                &f.context,
                CONTINUATION_AT + 7,
            )
            .unwrap();
        assert_eq!(session(&f), source);
        assert_eq!(
            f.store
                .adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id)
                .unwrap(),
            calls
        );
        assert!(f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                Uuid::new_v4(),
                "no-root-budget",
                &f.grant,
                &f.context,
                CONTINUATION_AT + 8
            )
            .is_err());
        let connection = f.store.connection.lock().unwrap();
        let receipt: AdaptiveBudgetWindowLimitReceiptV1 = get_entity(
            &connection,
            &f.leader.tenant_id,
            BUDGET_LIMIT_KIND,
            &budget_limit_id(f.grant.session_id, source.version).unwrap(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            receipt.causes,
            vec![AdaptiveBudgetWindowLimitCauseV1::RootCallsExhausted]
        );
    }

    #[test]
    fn schema3_three_windows_record_limit_without_resetting_or_adding_a_review() {
        let mut f = budget_fixture(8);
        let mut now = CONTINUATION_AT + 7;
        for index in 0..2 {
            let call = dispatch_budget(&f, now);
            let result = budget_result(&call, 1, now + 2);
            f.store
                .complete_adaptive_leadership_review_call(&f.leader, &result, now + 2)
                .unwrap();
            observe_budget_inspection(&f, session(&f), now + 3);
            now += 7;
            budget_context(&mut f, now, &format!("normal-window-{index}"));
        }
        let source = session(&f);
        assert_eq!(
            source.continuation.as_ref().unwrap().authorizations.len(),
            3
        );
        assert!(source.model_calls < source.grant.max_model_calls);
        let calls = f
            .store
            .adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id)
            .unwrap();
        f.store
            .record_adaptive_budget_window_limit(&f.leader, &f.grant, &f.context, now)
            .unwrap();
        assert_eq!(session(&f), source);
        assert_eq!(
            f.store
                .adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id)
                .unwrap(),
            calls
        );
        let reopened = WorkflowStore::open(&f.path).unwrap();
        assert!(reopened
            .adaptive_budget_window_limit_recorded(
                &f.leader.tenant_id,
                f.grant.session_id,
                source.version
            )
            .unwrap());
        assert!(f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                Uuid::new_v4(),
                "fourth-window",
                &f.grant,
                &f.context,
                now + 1
            )
            .is_err());
    }

    #[test]
    fn schema3_limit_requires_actual_limit_exact_source_and_exact_leader() {
        let mut f = budget_fixture(4);
        let before = rows(&f.store);
        assert!(f
            .store
            .record_adaptive_budget_window_limit(
                &f.leader,
                &f.grant,
                &f.context,
                CONTINUATION_AT + 7
            )
            .is_err());
        let mut foreign = f.leader.clone();
        foreign.authority_generation += 1;
        assert!(f
            .store
            .record_adaptive_budget_window_limit(
                &foreign,
                &f.grant,
                &f.context,
                CONTINUATION_AT + 7
            )
            .is_err());
        f.context.source_session.model_calls = f.context.source_session.grant.max_model_calls;
        assert!(f
            .store
            .record_adaptive_budget_window_limit(
                &f.leader,
                &f.grant,
                &f.context,
                CONTINUATION_AT + 7
            )
            .is_err());
        assert_eq!(rows(&f.store), before);
    }

    #[test]
    fn schema3_claim_and_commit_recheck_current_project_before_effects() {
        for after_dispatch in [false, true] {
            let f = budget_fixture(4);
            let call = f
                .store
                .authorize_adaptive_leadership_review_call(
                    &f.leader,
                    Uuid::new_v4(),
                    "normal-stale-project",
                    &f.grant,
                    &f.context,
                    CONTINUATION_AT + 7,
                )
                .unwrap();
            let call = if after_dispatch {
                f.store
                    .claim_adaptive_leadership_review_call(
                        &f.leader,
                        &claim(&call),
                        CONTINUATION_AT + 8,
                    )
                    .unwrap()
            } else {
                call
            };
            change_project(&f, CONTINUATION_AT + 9);
            let before = rows(&f.store);
            if after_dispatch {
                let result = budget_result(&call, 2, CONTINUATION_AT + 10);
                assert!(f
                    .store
                    .complete_adaptive_leadership_review_call(
                        &f.leader,
                        &result,
                        CONTINUATION_AT + 10
                    )
                    .is_err());
            } else {
                assert!(f
                    .store
                    .claim_adaptive_leadership_review_call(
                        &f.leader,
                        &claim(&call),
                        CONTINUATION_AT + 10
                    )
                    .is_err());
            }
            assert_eq!(rows(&f.store), before);
            assert_eq!(session(&f), f.context.source_session);
        }
    }

    #[test]
    fn schema3_expired_uncommitted_continuation_retires_exact_proposal_across_reopen() {
        let f = budget_fixture(4);
        let call = dispatch_budget(&f, CONTINUATION_AT + 7);
        let result = budget_result(&call, 2, CONTINUATION_AT + 9);
        let deadline = result.continuation.as_ref().unwrap().deadline_ms;
        let reopened = WorkflowStore::open(&f.path).unwrap();
        assert!(reopened
            .complete_adaptive_leadership_review_call(&f.leader, &result, deadline)
            .is_err());
        let retired = reopened
            .retire_expired_adaptive_continuation_call(&f.leader, &result, deadline)
            .unwrap();
        assert_eq!(retired.version, 4);
        assert!(retired.decision.is_none());
        assert!(retired.continuation.is_none());
        assert_eq!(
            reopened
                .adaptive_session(f.grant.session_id, &f.grant.assignee_authority)
                .unwrap(),
            Some(f.context.source_session.clone())
        );
        assert_eq!(
            reopened
                .company_project(&f.leader.tenant_id, &f.grant.project_id)
                .unwrap(),
            Some(f.context.source_project.clone())
        );
        let before = rows(&reopened);
        assert_eq!(
            reopened
                .retire_expired_adaptive_continuation_call(&f.leader, &result, deadline + 1)
                .unwrap(),
            retired
        );
        assert_eq!(rows(&reopened), before);
        let mut changed = result.clone();
        changed.model_response_digest = "e".repeat(64);
        assert!(reopened
            .retire_expired_adaptive_continuation_call(&f.leader, &changed, deadline + 2)
            .is_err());
        assert_eq!(rows(&reopened), before);
    }

    #[test]
    fn schema3_full_duration_allowance_requires_review_and_abandonment_provenance_after_reopen() {
        for (abandoned, corrupt_record) in
            [(false, false), (false, true), (true, false), (true, true)]
        {
            let f = budget_fixture(4);
            let call = dispatch_budget(&f, CONTINUATION_AT + 7);
            let result = budget_result(&call, 2, CONTINUATION_AT + 9);
            f.store
                .complete_adaptive_leadership_review_call(&f.leader, &result, CONTINUATION_AT + 9)
                .unwrap();
            let project = f
                .store
                .company_project(&f.leader.tenant_id, &f.grant.project_id)
                .unwrap()
                .unwrap();
            assert_eq!(
                project
                    .subscription_call
                    .as_ref()
                    .unwrap()
                    .grant
                    .max_duration_ms,
                120_000
            );
            let (kind, id) = if abandoned {
                (
                    ABANDONED_KIND,
                    call.context
                        .source_project
                        .subscription_call
                        .as_ref()
                        .unwrap()
                        .allowance_id
                        .clone(),
                )
            } else {
                (KIND, call.grant.review_id.to_string())
            };
            {
                let connection = f.store.connection.lock().unwrap();
                let sql = if corrupt_record {
                    "UPDATE company_entities SET payload_digest=?4 WHERE tenant_id=?1 AND entity_kind=?2 AND entity_id=?3"
                } else {
                    "DELETE FROM company_entities WHERE tenant_id=?1 AND entity_kind=?2 AND entity_id=?3 AND ?4 IS NOT NULL"
                };
                assert_eq!(
                    connection
                        .execute(sql, params![f.leader.tenant_id.0, kind, id, "f".repeat(64)])
                        .unwrap(),
                    1
                );
            }
            let reopened = WorkflowStore::open(&f.path).unwrap();
            let before = rows(&reopened);
            assert!(reopened
                .company_project(&f.leader.tenant_id, &f.grant.project_id)
                .is_err());
            assert_eq!(rows(&reopened), before);
            // Original attribution remains available; corruption cannot mint a new allowance.
            let original = reopened
                .historical_adaptive_provider_project(
                    &f.context.source_session.grant,
                    f.context.source_session.grant.created_at_ms,
                )
                .unwrap()
                .unwrap();
            let Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }) =
                &f.grant.subject
            else {
                panic!("budget")
            };
            assert_eq!(
                original.subscription_call.as_ref(),
                Some(&budget.root_allowance)
            );
            assert_eq!(rows(&reopened), before);
        }
    }

    #[test]
    fn schema3_full_duration_journal_still_requires_receipt_when_issuance_event_is_missing() {
        let f = budget_fixture(4);
        let call = dispatch_budget(&f, CONTINUATION_AT + 7);
        let result = budget_result(&call, 2, CONTINUATION_AT + 9);
        f.store
            .complete_adaptive_leadership_review_call(&f.leader, &result, CONTINUATION_AT + 9)
            .unwrap();
        let project = f
            .store
            .company_project(&f.leader.tenant_id, &f.grant.project_id)
            .unwrap()
            .unwrap();
        {
            let connection = f.store.connection.lock().unwrap();
            assert_eq!(connection.execute(
                "DELETE FROM company_events WHERE tenant_id=?1 AND event_type='adaptive_leadership_continuation_authorized' AND operation_id=?2",
                params![f.leader.tenant_id.0, call.operation_id.to_string()],
            ).unwrap(), 1);
            assert_eq!(connection.execute("DELETE FROM company_entities WHERE tenant_id=?1 AND entity_kind=?2 AND entity_id=?3",
                params![f.leader.tenant_id.0, KIND, call.grant.review_id.to_string()]).unwrap(), 1);
        }
        let reopened = WorkflowStore::open(&f.path).unwrap();
        let before = rows(&reopened);
        {
            let connection = reopened.connection.lock().unwrap();
            assert!(crate::store::adaptive::allowance_is_governed_in_journal(
                &connection,
                &project.tenant_id,
                &project.project_id,
                project.subscription_call.as_ref().unwrap()
            )
            .unwrap());
            assert!(subscription::validate_persisted(&connection, &project).is_err());
        }
        assert!(reopened
            .company_project(&f.leader.tenant_id, &f.grant.project_id)
            .is_err());
        assert_eq!(rows(&reopened), before);
    }

    #[test]
    fn ordinary_allowance_reads_and_historical_attribution_survive_65_never_claimed_rollovers() {
        let f = fixture();
        resolve(&f, Uuid::new_v4(), 21);
        let mut project = f.context.source_project.clone();
        let mut roots = Vec::new();
        let mut original_project = None;
        for index in 0..66u64 {
            let now = 600_001 + index * 300_001;
            let mut root = f.context.source_session.grant.clone();
            root.session_id = Uuid::new_v4();
            root.created_at_ms = now;
            root.deadline_ms = now + 300_000;
            let grant = crate::SubscriptionCallGrantV1 {
                schema_version: 1,
                work_item_id: f.grant.work_item_id.clone(),
                assignment_id: f.grant.assignment_id.clone(),
                assignment_version: f.grant.assignee_authority.assignment_version,
                agent_id: f.grant.assignee_authority.agent_id,
                provider: f.grant.provider.clone(),
                model: f.grant.model.clone(),
                catalog_digest: f.grant.catalog_digest.clone(),
                max_calls: root.max_model_calls,
                max_concurrent: 1,
                max_duration_ms: 120_000,
                token_policy: f.grant.token_policy,
                expires_at_unix_ms: root.deadline_ms,
            };
            let response = f
                .store
                .apply_company_command(
                    &f.leader,
                    Uuid::new_v4(),
                    &CompanyWorkflowCommandV1::GrantSubscriptionCall {
                        project_id: project.project_id.clone(),
                        expected_version: project.version,
                        grant,
                    },
                    now,
                )
                .unwrap()
                .response;
            let CompanyWorkflowResponseV1::Project(updated) = response else {
                panic!("project")
            };
            project = *updated;
            let allowance = project.subscription_call.as_ref().unwrap();
            assert!(allowance.dispatch.is_none());
            root.provider_allowance_id = allowance.allowance_id.clone();
            root.provider_authority_digest =
                adaptive_leadership_continuation_provider_authority_digest(
                    allowance,
                    &f.grant.assignee_authority,
                )
                .unwrap();
            let (_, initial) = f
                .store
                .begin_adaptive_session(&root, &f.grant.assignee_authority, now)
                .unwrap();
            assert_eq!(
                (initial.version, initial.model_calls, initial.tool_calls),
                (1, 0, 0)
            );
            assert!(initial.continuation.is_none());
            if index == 0 {
                original_project = Some(project.clone());
            }
            roots.push(root);
        }
        let reopened = WorkflowStore::open(&f.path).unwrap();
        let before = rows(&reopened);
        {
            let connection = reopened.connection.lock().unwrap();
            for root in &roots[..65] {
                let (archived, _) = crate::store::adaptive::load(&connection, root.session_id)
                    .unwrap()
                    .unwrap();
                assert_eq!(archived.grant, *root);
                assert_eq!((archived.model_calls, archived.tool_calls), (0, 0));
                assert!(matches!(archived.cursor, AdaptiveCursorV1::Cancelled));
                assert!(archived.continuation.is_none());
            }
            assert!(!crate::store::adaptive::allowance_is_governed_in_journal(
                &connection,
                &project.tenant_id,
                &project.project_id,
                project.subscription_call.as_ref().unwrap(),
            )
            .unwrap());
            subscription::validate_persisted(&connection, &project).unwrap();
        }
        assert_eq!(
            reopened
                .company_project(&f.leader.tenant_id, &f.grant.project_id)
                .unwrap(),
            Some(project)
        );
        assert_eq!(
            reopened
                .historical_adaptive_provider_project(&roots[0], roots[0].created_at_ms,)
                .unwrap(),
            original_project
        );
        assert_eq!(rows(&reopened), before);
    }

    fn continuation_fixture(unknown: bool, resolved: bool) -> Fixture {
        continuation_fixture_with_calls(unknown, resolved, 4)
    }

    fn continuation_fixture_with_calls(
        unknown: bool,
        resolved: bool,
        max_model_calls: u16,
    ) -> Fixture {
        continuation_fixture_with_policy(unknown, resolved, max_model_calls, max_model_calls)
    }

    fn continuation_fixture_with_policy(
        unknown: bool,
        resolved: bool,
        max_model_calls: u16,
        policy_max_calls: u16,
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
            max_calls: policy_max_calls,
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

    fn renew_continuation_source(f: &mut Fixture) -> SubscriptionCallAllowanceV1 {
        let source = session(f);
        let mut grant = f
            .context
            .source_project
            .subscription_call
            .as_ref()
            .unwrap()
            .grant
            .clone();
        grant.expires_at_unix_ms = CONTINUATION_AT + 300_000;
        let response = f
            .store
            .apply_company_command(
                &f.leader,
                Uuid::new_v4(),
                &CompanyWorkflowCommandV1::GrantSubscriptionCall {
                    project_id: f.grant.project_id.clone(),
                    expected_version: f.context.source_project.version,
                    grant,
                },
                CONTINUATION_AT,
            )
            .unwrap()
            .response;
        let CompanyWorkflowResponseV1::Project(project) = response else {
            panic!("project");
        };
        let current = project.subscription_call.as_ref().unwrap().clone();
        assert_ne!(current.allowance_id, source.grant.provider_allowance_id);
        assert_ne!(current.grant.expires_at_unix_ms, source.grant.deadline_ms);
        assert!(current.dispatch.is_none());
        f.grant.expected_project_version = project.version;
        f.context.source_project = *project;
        assert_eq!(session(f), source);
        current
    }

    #[test]
    fn continuation_supersedes_current_unused_allowance_without_replacing_root_history() {
        for unknown in [true, false] {
            let mut f = continuation_fixture(unknown, false);
            let original = f.context.source_project.clone();
            let source = session(&f);
            let current = renew_continuation_source(&mut f);
            let call = dispatch_continuation(&f);
            call.validate_continuation_source().unwrap();
            let result = continue_result(&call);
            let receipt = f
                .store
                .complete_adaptive_leadership_review_call(&f.leader, &result, CONTINUATION_AT + 2)
                .unwrap();
            let continued = session(&f);
            assert_eq!(continued.grant, source.grant);
            assert_eq!(continued.model_calls, source.model_calls);
            assert_eq!(continued.tool_calls, source.tool_calls);
            assert_eq!(receipt.context.source_session, source);
            assert_eq!(
                receipt.context.source_project.subscription_call.as_ref(),
                Some(&current)
            );
            assert_eq!(
                f.store
                    .historical_adaptive_provider_project(&source.grant, 600_003)
                    .unwrap(),
                Some(original)
            );
            let reopened = WorkflowStore::open(&f.path).unwrap();
            let abandoned = reopened
                .adaptive_leadership_abandoned_allowance(&f.leader.tenant_id, &current.allowance_id)
                .unwrap()
                .unwrap();
            assert_eq!(abandoned.review, receipt);
            assert!(reopened
                .adaptive_leadership_abandoned_allowance(
                    &f.leader.tenant_id,
                    &source.grant.provider_allowance_id,
                )
                .unwrap()
                .is_none());
            let project = reopened
                .company_project(&f.leader.tenant_id, &f.grant.project_id)
                .unwrap()
                .unwrap();
            let fresh = project.subscription_call.as_ref().unwrap();
            assert_eq!(fresh.grant.max_calls, 1);
            assert_eq!(fresh.grant.max_duration_ms, 120_000);
            assert_eq!(fresh.grant.max_concurrent, 1);
            assert_ne!(fresh.allowance_id, current.allowance_id);
            assert!(project.abandoned_subscription_calls.is_empty());
            let before = rows(&reopened);
            assert_eq!(
                reopened
                    .complete_adaptive_leadership_review_call(
                        &f.leader,
                        &result,
                        result.continuation.as_ref().unwrap().deadline_ms + 1,
                    )
                    .unwrap(),
                receipt
            );
            assert_eq!(rows(&reopened), before);
        }
    }

    #[test]
    fn continuation_with_broader_current_policy_is_limited_by_unspent_root_budget() {
        let mut f = continuation_fixture_with_policy(true, false, 4, 8);
        let source = session(&f);
        let current = renew_continuation_source(&mut f);
        assert!(current.grant.max_calls > source.grant.max_model_calls);
        let call = dispatch_continuation(&f);
        call.validate_continuation_source().unwrap();
        let remaining = source.grant.max_model_calls - source.model_calls;
        let allowance = call
            .continuation_allowance(CONTINUATION_AT + 2, CONTINUATION_AT + 120_002, remaining)
            .unwrap();
        assert_eq!(allowance.grant.max_calls, remaining);
        assert!(call
            .continuation_allowance(
                CONTINUATION_AT + 2,
                CONTINUATION_AT + 120_002,
                remaining + 1
            )
            .is_err());
        let mut result = continue_result(&call);
        if let AdaptiveLeadershipReviewDecisionKindV1::Continue {
            additional_model_calls,
            ..
        } = &mut result.decision.decision
        {
            *additional_model_calls = remaining;
        }
        let audit = adaptive_leadership_continuation_audit_id(
            call.grant.review_id,
            &result.request_digest,
            &result.model_response_digest,
            &result.decision,
        )
        .unwrap();
        result.resolution_event_id = Some(audit);
        let authorization = result.continuation.as_mut().unwrap();
        authorization.resolution_event_id = audit;
        authorization.additional_model_calls = remaining;
        authorization.provider_authority_digest =
            adaptive_leadership_continuation_provider_authority_digest(
                &allowance,
                &call.grant.assignee_authority,
            )
            .unwrap();
        f.store
            .complete_adaptive_leadership_review_call(&f.leader, &result, CONTINUATION_AT + 2)
            .unwrap();
        let continued = session(&f);
        assert_eq!(continued.grant, source.grant);
        assert_eq!(continued.model_calls, source.model_calls);
        assert_eq!(
            continued.active_model_ceiling(),
            source.grant.max_model_calls
        );
    }

    #[test]
    fn continuation_constructor_intersects_root_current_policy_and_window() {
        let mut f = continuation_fixture(true, false);
        renew_continuation_source(&mut f);
        let call = dispatch_continuation(&f);
        for (current_calls, root_duration, current_duration, window) in [
            (2, 120_000, 60_000, 120_000),
            (8, 60_000, 120_000, 120_000),
            (8, 120_000, 120_000, 1_000),
        ] {
            let mut bounded = call.clone();
            bounded.context.source_session.grant.max_call_duration_ms = root_duration;
            let current = bounded
                .context
                .source_project
                .subscription_call
                .as_mut()
                .unwrap();
            current.grant.max_calls = current_calls;
            current.grant.max_duration_ms = current_duration;
            let remaining = bounded.context.source_session.grant.max_model_calls
                - bounded.context.source_session.model_calls;
            let calls = remaining.min(current_calls);
            let allowance = bounded
                .continuation_allowance(CONTINUATION_AT + 2, CONTINUATION_AT + 2 + window, calls)
                .unwrap();
            assert_eq!(allowance.grant.max_calls, calls);
            assert_eq!(
                allowance.grant.max_duration_ms,
                root_duration.min(current_duration).min(window)
            );
            assert_eq!(allowance.grant.max_concurrent, 1);
            assert!(bounded
                .continuation_allowance(
                    CONTINUATION_AT + 2,
                    CONTINUATION_AT + 2 + window,
                    calls + 1
                )
                .is_err());
        }
    }

    #[test]
    fn continuation_source_rejects_dispatched_foreign_and_duplicate_allowances_without_writes() {
        let mut f = continuation_fixture(true, false);
        renew_continuation_source(&mut f);
        let call = dispatch_continuation(&f);
        let before = rows(&f.store);
        for change in 0..9 {
            let mut invalid = call.clone();
            let current = invalid
                .context
                .source_project
                .subscription_call
                .as_mut()
                .unwrap();
            match change {
                0 => {
                    current.dispatch = Some(crate::SubscriptionCallDispatchV1 {
                        request_id: "already-dispatched".into(),
                        request_digest: DIGEST.into(),
                        dispatched_at_unix_ms: CONTINUATION_AT,
                    })
                }
                1 => current.grant.assignment_id = "foreign-assignment".into(),
                2 => current.grant.assignment_version += 1,
                3 => current.grant.provider = "foreign-provider".into(),
                4 => current.grant.model = "foreign-model".into(),
                5 => current.grant.catalog_digest = "b".repeat(64),
                6 => current.grant.max_concurrent = 2,
                7 => current.allowance_id = invalid.allowance_id.clone(),
                _ => {
                    current.created_at_unix_ms = invalid.context.source_session.grant.created_at_ms
                }
            }
            assert!(
                invalid.validate_continuation_source().is_err(),
                "change {change}"
            );
        }
        let mut retired = call.clone();
        retired.retired_at_unix_ms = Some(CONTINUATION_AT + 3);
        retired.updated_at_unix_ms = CONTINUATION_AT + 3;
        retired.version = 4;
        retired.validate_continuation_source().unwrap();
        let mut duplicate = call.clone();
        duplicate
            .context
            .source_project
            .subscription_call
            .as_mut()
            .unwrap()
            .allowance_id = crate::domain::stable_domain_id(
            "subscription",
            &f.leader.tenant_id,
            duplicate.operation_id,
        )
        .unwrap();
        let allowance = duplicate
            .continuation_allowance(CONTINUATION_AT + 2, CONTINUATION_AT + 120_002, 1)
            .unwrap();
        let mut result = continue_result(&call);
        result
            .continuation
            .as_mut()
            .unwrap()
            .provider_authority_digest =
            adaptive_leadership_continuation_provider_authority_digest(
                &allowance,
                &call.grant.assignee_authority,
            )
            .unwrap();
        assert!(validate_continuation(
            &duplicate,
            &result.decision,
            &result.request_digest,
            &result.model_response_digest,
            result.resolution_event_id,
            result.continuation.as_ref()
        )
        .is_err());
        assert_eq!(rows(&f.store), before);
    }

    #[test]
    fn recovery_binding_allows_only_exact_unresolved_blocked_continuation_subject() {
        let f = continuation_fixture(false, false);
        let mut grant = f.grant.clone();
        grant.recovery_epoch = Some(crate::AdaptiveLeadershipRecoveryBindingV1 {
            schema_version: 1,
            epoch_key: crate::adaptive_leadership_recovery_epoch_key(
                &f.leader.tenant_id,
                grant.session_id,
            )
            .unwrap(),
            epoch_digest: "b".repeat(64),
            review_id: grant.review_id,
            max_window_ms: 120_000,
            max_additional_model_calls: 1,
        });
        grant.validate(CONTINUATION_AT).unwrap();
        f.context.validate(&grant).unwrap();
        let decision = AdaptiveLeadershipReviewDecisionV1 {
            schema_version: 2,
            decision: AdaptiveLeadershipReviewDecisionKindV1::Continue {
                additional_model_calls: 1,
                window_ms: 120_000,
                rationale: "Continue only the exact blocked model source".into(),
                evidence_refs: f.context.evidence_refs.clone(),
            },
        };
        decision.validate_subject(&grant).unwrap();
        let mut wrong_reason = grant.clone();
        wrong_reason.expected_reason_code = "different-reason".into();
        assert!(wrong_reason.validate(CONTINUATION_AT).is_err());
        let mut wrong_result = f.context.clone();
        wrong_result.source_session.last_model_result_digest = Some("c".repeat(64));
        assert!(wrong_result.validate(&grant).is_err());
        if let Some(AdaptiveLeadershipReviewSubjectV2::BlockedContinuation {
            resolution_event_id,
            ..
        }) = &mut grant.subject
        {
            *resolution_event_id = Some(Uuid::new_v4().to_string());
        }
        assert!(grant.validate(CONTINUATION_AT).is_err());
        assert!(decision.validate_subject(&grant).is_err());
    }

    #[test]
    fn attached_local_adoption_without_durable_authority_cannot_complete_or_change_source() {
        let f = continuation_fixture(true, false);
        let mut call = dispatch_continuation(&f);
        // A synthetic binding can satisfy pure shape checks, never durable membership.
        call.grant.recovery_epoch = Some(crate::AdaptiveLeadershipRecoveryBindingV1 {
            schema_version: 1,
            epoch_key: crate::adaptive_leadership_recovery_epoch_key(
                &f.leader.tenant_id,
                call.grant.session_id,
            )
            .unwrap(),
            epoch_digest: "b".repeat(64),
            review_id: call.grant.review_id,
            max_window_ms: 120_000,
            max_additional_model_calls: 1,
        });
        let context_digest = call.context_digest().unwrap();
        call.dispatch.as_mut().unwrap().context_digest = context_digest;
        let mut result = continue_result(&call);
        let issued = call.grant.expires_at_unix_ms + 1;
        let mut operator = f.leader.clone();
        operator.kind = CompanyPrincipalKindV1::Operator;
        operator.agent_id = None;
        let authority = PrincipalAuthorityV1 {
            schema_version: 1,
            principal_id: operator.principal_id.clone(),
            principal_generation: operator.authority_generation,
            authority_digest: operator.authority_digest.clone(),
        };
        let request = crate::AdaptiveLeadershipLocalAdoptionRequestV1 {
            schema_version: 1,
            operation_id: Uuid::new_v4(),
            tenant_id: f.leader.tenant_id.clone(),
            project_id: f.grant.project_id.clone(),
            work_item_id: f.grant.work_item_id.clone(),
            session_id: f.grant.session_id,
            review_id: call.grant.review_id,
            epoch_digest: "b".repeat(64),
            original_call_digest: crate::adaptive_leadership_local_adoption_source_call_digest(
                &call,
            )
            .unwrap(),
            project_digest: crate::adaptive_leadership_recovery_project_digest(
                &call.context.source_project,
            )
            .unwrap(),
            session_digest: crate::adaptive_leadership_recovery_session_digest(
                &call.context.source_session,
            )
            .unwrap(),
            session_head_digest: crate::store::adaptive::load(
                &f.store.connection.lock().unwrap(),
                f.grant.session_id,
            )
            .unwrap()
            .unwrap()
            .1,
            request_id: call.request_id(),
            request_digest: result.request_digest.clone(),
            context_digest: call.context_digest().unwrap(),
            payload_digest: "c".repeat(64),
            model_response_digest: result.model_response_digest.clone(),
            usage_event_digest: "d".repeat(64),
            original_completion_error: "continuation audit invalid".into(),
            completion_attempts: 1,
            release: crate::AdaptiveRecoveryReleaseV1 {
                schema_version: 1,
                source_git_sha: "a".repeat(40),
                release_manifest_digest: "b".repeat(64),
                gateway_binary_digest: "c".repeat(64),
            },
            repair_digest: "d".repeat(64),
            decision: result.decision.clone(),
            expires_at_unix_ms: issued + 120_000,
        };
        let adoption = crate::AdaptiveLeadershipLocalAdoptionV1 {
            adoption_key: request.key().unwrap(),
            request,
            issuer_principal: operator,
            issuer_authority: authority,
            issued_at_unix_ms: issued,
            continuation_deadline_ms: issued + 120_000,
        };
        adoption.validate().unwrap();
        adoption.validate_call(&call).unwrap();
        let before = rows(&f.store);
        {
            let connection = f.store.connection.lock().unwrap();
            assert!(
                super::super::adaptive_leadership_local_adoption::require_local_adoption(
                    &connection,
                    &call,
                    &adoption,
                )
                .is_err()
            );
        }
        let allowance = call
            .continuation_allowance(issued, issued + 120_000, 1)
            .unwrap();
        let authorization = result.continuation.as_mut().unwrap();
        authorization.issued_at_ms = issued;
        authorization.deadline_ms = issued + 120_000;
        authorization.provider_authority_digest =
            adaptive_leadership_continuation_provider_authority_digest(
                &allowance,
                &call.grant.assignee_authority,
            )
            .unwrap();
        authorization.local_adoption = Some(Box::new(adoption));
        validate_continuation(
            &call,
            &result.decision,
            &result.request_digest,
            &result.model_response_digest,
            result.resolution_event_id,
            result.continuation.as_ref(),
        )
        .unwrap();
        assert!(f
            .store
            .complete_adaptive_leadership_review_call(&f.leader, &result, issued)
            .is_err());
        assert_eq!(rows(&f.store), before);
        assert_eq!(session(&f), call.context.source_session);
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
            Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }) => (
                crate::adaptive::AdaptiveContinuationSourceV1::BudgetWindowExhausted {
                    active_allowance_digest: budget.active_allowance_digest.clone(),
                    continuation_history_digest: budget.continuation_history_digest.clone(),
                },
                None,
            ),
            None => panic!("continuation subject"),
        };
        result.resolution_event_id = Some(audit);
        result.continuation = Some(crate::adaptive::AdaptiveContinuationAuthorizationV1 {
            schema_version: 1,
            local_adoption: None,
            resume_policy: None,
            work_funding: None,
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
            assert!(subscription::grant_governed_continuation(
                &f.store.connection.lock().unwrap(),
                &mut candidate,
                &call,
                &allowance
            )
            .is_err());
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
        assert!(call
            .continuation_allowance(CONTINUATION_AT + 2, CONTINUATION_AT + 120_002, 1)
            .is_err());
        // Construct an invalid proposal without bypassing the production constructor's cap.
        let mut unspent = call.clone();
        unspent.context.source_session.model_calls = 0;
        let invalid_result = continue_result(&unspent);
        assert!(f
            .store
            .complete_adaptive_leadership_review_call(
                &f.leader,
                &invalid_result,
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
        let source_allowance = f.context.source_project.subscription_call.clone();
        // Real fixture without subscription authority is valid leadership input, not continuation authority.
        f.context.source_project.subscription_call = None;
        persist_entity(&f.store, &f.context.source_project);
        let call = dispatch_continuation(&f);
        let mut valid_source = call.clone();
        valid_source.context.source_project.subscription_call = source_allowance;
        let proposed = continue_result(&valid_source);
        let before = rows(&f.store);
        assert!(f
            .store
            .complete_adaptive_leadership_review_call(&f.leader, &proposed, CONTINUATION_AT + 2)
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
        let mut normal_decision = result.decision.clone();
        normal_decision.schema_version = 3;
        normal_decision.validate(&f.context.evidence_refs).unwrap();
        assert!(normal_decision.validate_subject(&f.grant).is_err());
        for schema in [0, 1, u16::MAX] {
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
