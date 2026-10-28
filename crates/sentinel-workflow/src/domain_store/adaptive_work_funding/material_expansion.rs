//! Exact-head material offers; an immutable selection relation, never a decision.
use super::*;
use crate::{
    adaptive_material_window_slot, material_window_floor, validate_material_window_limits,
    AdaptiveMaterialWindowUsageProofV1, AdaptiveMaterialWindowVerificationSourceV1,
    AdaptiveSessionV1, AdaptiveWorkFundingMaterialExpansionBindingV1,
};

const KIND: &str = "adaptive_work_funding_material_expansion";
const EVENT: &str = "adaptive_work_funding_material_expansion_issued";
const DOMAIN: &str = "sentinel.workflow.material-window-relation.v1";

#[cfg(test)]
#[path = "material_expansion/read_snapshot_tests.rs"]
mod read_snapshot_tests;

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Relation {
    schema_version: u16,
    slot_key: String,
    tenant_id: TenantId,
    project_id: ProjectId,
    session_id: Uuid,
    operation_id: Uuid,
    binding: AdaptiveWorkFundingMaterialExpansionBindingV1,
    new_funding_id: String,
    new_receipt_digest: String,
    issuer: AuthenticatedCompanyPrincipalV1,
    issued_at_unix_ms: u64,
}

impl CompanyEntity for Relation {
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        (&self.tenant_id, KIND, &self.slot_key, 1)
    }
    fn validate_entity(&self) -> Result<(), WorkflowError> {
        if self.schema_version != 1
            || self.slot_key != self.binding.slot_key
            || self.operation_id.is_nil()
            || self.session_id.is_nil()
            || self.issuer.tenant_id != self.tenant_id
            || self.issued_at_unix_ms == 0
        {
            return Err(corrupt());
        }
        validate_digest(&self.new_receipt_digest)?;
        crate::adaptive_resume_policy::require_resume_policy_operator(&self.issuer, &self.tenant_id)
    }
    fn validate_persisted(&self, connection: &Connection) -> Result<(), WorkflowError> {
        let count: i64 = connection.query_row(
            "SELECT count(*) FROM (SELECT sequence FROM company_events WHERE tenant_id=?1 AND event_type=?2
             AND (operation_id=?3 OR json_extract(payload,'$.slot_key')=?4) LIMIT 2)",
            params![self.tenant_id.0, EVENT, self.operation_id.to_string(), self.slot_key], |row| row.get(0))?;
        if count != 1 {
            return Err(corrupt());
        }
        super::super::adaptive_resume_policy::require_leaf_event(
            connection,
            &self.tenant_id,
            &self.project_id,
            EVENT,
            self.operation_id,
            &canonical_sha256(DOMAIN, self)?,
            &self.issuer,
            self.issued_at_unix_ms,
            self,
        )?;
        // Sealed funding avoids traversing this very relation back through its successor.
        let funding =
            expired_replacement::sealed_funding(connection, &self.tenant_id, &self.new_funding_id)?;
        require_relation_body(connection, self, &funding)?;
        require_closed_source_cut(connection, self, &funding)
    }
}

fn require_closed_source_cut(
    connection: &Connection,
    relation: &Relation,
    funding: &AdaptiveWorkFundingReceiptV1,
) -> Result<(), WorkflowError> {
    let anchor = &funding.request.source.resume_source;
    // Reuse tenant-wide sealed trace authentication; never traverse future
    // current-call membership from this historical receipt validator.
    reviewed_funding_ids(connection, &anchor.tenant_id)?;
    let cut: i64 = connection.query_row(
        "SELECT sequence FROM company_events WHERE tenant_id=?1 AND event_type=?2 AND operation_id=?3",
        params![anchor.tenant_id.0, EVENT, relation.operation_id.to_string()], |row| row.get(0))?;
    let mut statement = connection.prepare(
        "SELECT sequence FROM company_events WHERE tenant_id=?1
         AND event_type GLOB 'adaptive_leadership_review_*' AND sequence<?2
         ORDER BY sequence LIMIT 4097",
    )?;
    let ids = statement
        .query_map(params![anchor.tenant_id.0, cut], |row| row.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if ids.len() > 4096 {
        return Err(corrupt());
    }
    let mut history = BTreeMap::new();
    for id in ids {
        let event = company_event_shape(connection, stored_u64(id)?)?.ok_or_else(corrupt)?;
        let call: AdaptiveLeadershipReviewCallV1 = decode(&event.payload)?;
        if call.grant.session_id == anchor.session_id {
            if call.updated_at_unix_ms > relation.issued_at_unix_ms {
                return Err(corrupt());
            }
            validation_scope::charge_bytes(connection, event.payload.len())?;
            history.insert(call.grant.review_id, call);
        }
    }
    if history.len() != usize::from(anchor.base_review_count)
        || history
            .values()
            .any(|call| call.decision.is_none() && call.retired_at_unix_ms.is_none())
        || history
            .get(&relation.binding.refused_review_id)
            .is_none_or(|call| {
                canonical_sha256("sentinel.workflow.adaptive-leadership-call.v1", call)
                    .ok()
                    .as_ref()
                    != Some(&relation.binding.refused_call_digest)
            })
        || canonical_sha256(
            "sentinel.workflow.adaptive-resume-review-history.v1",
            &history.values().collect::<Vec<_>>(),
        )? != anchor.review_history_digest
    {
        return Err(corrupt());
    }
    Ok(())
}

fn lookup(
    connection: &Connection,
    tenant: &TenantId,
    key: &str,
) -> Result<Option<Relation>, WorkflowError> {
    let relation: Option<Relation> = get_entity(connection, tenant, KIND, key)?;
    if relation.is_none() {
        let orphan: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM company_events WHERE tenant_id=?1 AND event_type=?2
             AND json_extract(payload,'$.slot_key')=?3)",
            params![tenant.0, EVENT, key],
            |row| row.get(0),
        )?;
        if orphan {
            return Err(corrupt());
        }
    }
    Ok(relation)
}

fn source_floor(
    session: &AdaptiveSessionV1,
    declined: &AdaptiveWorkFundingEpochV1,
) -> Result<AdaptiveWorkFundingBindingLimitsV1, WorkflowError> {
    session.active_work_funding().ok_or_else(transition)?;
    // Preserve all journal-adopted epochs, not just the latest or smaller declined offer.
    let mut authorities: Vec<_> = adopted_epochs(session)
        .into_iter()
        .map(|epoch| epoch.binding.limits.clone())
        .collect();
    authorities.push(declined.binding.limits.clone());
    let floor = material_window_floor(&authorities)?;
    if session.continuation_window_limit() > usize::from(floor.total_window_ceiling)
        || floor.total_model_call_ceiling != session.funded_model_call_ceiling()
        || floor.total_tool_call_ceiling != session.funded_tool_call_ceiling()
    {
        return Err(transition());
    }
    Ok(floor)
}

fn require_standing_origin(
    connection: &Connection,
    receipt: &AdaptiveWorkFundingReceiptV1,
    session: &AdaptiveSessionV1,
) -> Result<(), WorkflowError> {
    let anchor = &receipt.request.source.resume_source;
    let cause = terminal_manual_defer::standing_cause(receipt)?.ok_or_else(corrupt)?;
    let call = expired_replacement::sealed_funded_refusal(connection, &anchor.tenant_id, cause)?;
    let sequence: i64 = connection.query_row(
        "SELECT sequence FROM company_events WHERE tenant_id=?1 AND event_type=?2 AND operation_id=?3",
        params![anchor.tenant_id.0, FUNDING_EVENT, receipt.request.operation_id.to_string()], |row| row.get(0))?;
    if !standing_policy::terminal_funded_defer(&call, session)
        || call.grant.project_id != anchor.project_id
        || call.grant.work_item_id != anchor.work_item_id
        || !terminal_manual_defer::require_completed_before(
            connection,
            &call,
            receipt.issued_at_unix_ms,
            stored_u64(sequence)?,
        )?
    {
        return Err(corrupt());
    }
    Ok(())
}

fn require_relation_body(
    connection: &Connection,
    relation: &Relation,
    funding: &AdaptiveWorkFundingReceiptV1,
) -> Result<(), WorkflowError> {
    let source = &funding.request.source;
    let anchor = &source.resume_source;
    let binding = source
        .material_window_expansion
        .as_ref()
        .ok_or_else(corrupt)?;
    if binding != &relation.binding
        || relation.new_funding_id != funding.funding_id
        || relation.new_receipt_digest != funding.receipt_digest()?
        || relation.issuer != funding.issuer_principal
        || relation.operation_id != funding.request.operation_id
        || relation.issued_at_unix_ms != funding.issued_at_unix_ms
        || relation.tenant_id != anchor.tenant_id
        || relation.project_id != anchor.project_id
        || relation.session_id != anchor.session_id
    {
        return Err(corrupt());
    }
    let old = expired_replacement::sealed_funding(
        connection,
        &anchor.tenant_id,
        &binding.declined_funding_id,
    )?;
    let old_anchor = &old.request.source.resume_source;
    let call = expired_replacement::sealed_funded_refusal(
        connection,
        &anchor.tenant_id,
        binding.refused_review_id,
    )?;
    if old.receipt_digest()? != binding.declined_receipt_digest
        || !old.request.reason_ref.starts_with("standing-funding:")
        || old.request.limits.additional_model_calls != 0
        || old.request.limits.additional_tool_calls != 0
        || old.request.source.material_window_expansion.is_some()
        || call
            .grant
            .work_funding
            .as_ref()
            .is_none_or(|epoch| epoch.receipt != old)
        || !standing_policy::terminal_funded_defer(&call, &call.context.source_session)
        || canonical_sha256("sentinel.workflow.adaptive-leadership-call.v1", &call)?
            != binding.refused_call_digest
        || call.model_response_digest.as_ref() != Some(&binding.model_response_digest)
        || call.updated_at_unix_ms > funding.issued_at_unix_ms
        || call.context.source_session.version != anchor.expected_session_version
        || call.context.source_session.grant.authority != anchor.assignee_authority
        || call.context.source_project.version != anchor.expected_project_version
        || old_anchor.project_id != anchor.project_id
        || old_anchor.work_item_id != anchor.work_item_id
        || old_anchor.session_id != anchor.session_id
        || old_anchor.expected_project_version != anchor.expected_project_version
        || old_anchor.project_payload_digest != anchor.project_payload_digest
        || old_anchor.root_entry_digest != anchor.root_entry_digest
        || old_anchor.head_entry_digest != anchor.head_entry_digest
        || old_anchor.continuation_history_digest != anchor.continuation_history_digest
        || old_anchor.assignee_authority != anchor.assignee_authority
        || old_anchor.subject != anchor.subject
        || source.original_model_call_ceiling != call.context.source_session.grant.max_model_calls
        || source.original_tool_call_ceiling != call.context.source_session.grant.max_tool_calls
        || source.predecessor_receipt_digest.as_deref()
            != call
                .context
                .source_session
                .active_work_funding()
                .map(|epoch| epoch.binding.receipt_digest.as_str())
        || crate::adaptive_budget_history_digest(&call.context.source_session.continuation)?
            != anchor.continuation_history_digest
        || call.context.source_session.model_calls != anchor.base_model_calls
        || call.context.source_session.tool_calls != anchor.base_tool_calls
        || call
            .context
            .source_session
            .continuation
            .as_ref()
            .map_or(0, |state| state.authorizations.len())
            != usize::from(anchor.base_window_count)
    {
        return Err(corrupt());
    }
    require_standing_origin(connection, &old, &call.context.source_session)?;
    let declined = call.grant.work_funding.as_ref().ok_or_else(corrupt)?;
    if declined.binding.limits != binding.declined_limits
        || source_floor(&call.context.source_session, declined)? != binding.authoritative_floor
    {
        return Err(corrupt());
    }
    validate_material_window_limits(
        source,
        &funding.request.limits,
        &binding.authoritative_floor,
        funding.issued_at_unix_ms,
    )
}

pub(super) fn require_successor_membership(
    connection: &Connection,
    funding: &AdaptiveWorkFundingReceiptV1,
) -> Result<(), WorkflowError> {
    let Some(binding) = &funding.request.source.material_window_expansion else {
        return Ok(());
    };
    let relation = lookup(
        connection,
        &funding.request.source.resume_source.tenant_id,
        &binding.slot_key,
    )?
    .ok_or_else(corrupt)?;
    if relation.new_funding_id != funding.funding_id
        || relation.new_receipt_digest != funding.receipt_digest()?
    {
        return Err(corrupt());
    }
    Ok(())
}

pub(super) fn consumed_receipts(
    connection: &Connection,
    tenant: &TenantId,
    session: Uuid,
) -> Result<BTreeSet<String>, WorkflowError> {
    let mut statement = connection.prepare(
        "SELECT entity_id FROM company_entities
        WHERE tenant_id=?1 AND entity_kind=?2 ORDER BY entity_id LIMIT 4097",
    )?;
    let keys = statement
        .query_map(params![tenant.0, KIND], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if keys.len() > 4096 {
        return Err(corrupt());
    }
    let mut consumed = BTreeSet::new();
    for key in keys {
        let relation = lookup(connection, tenant, &key)?.ok_or_else(corrupt)?;
        if relation.session_id == session && !consumed.insert(relation.binding.declined_funding_id)
        {
            return Err(corrupt());
        }
    }
    let mut events = connection.prepare(
        "SELECT sequence FROM company_events WHERE tenant_id=?1
        AND event_type=?2 ORDER BY sequence LIMIT 4097",
    )?;
    let ids = events
        .query_map(params![tenant.0, EVENT], |row| row.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if ids.len() > 4096 {
        return Err(corrupt());
    }
    for id in ids {
        let row = company_event_shape(connection, stored_u64(id)?)?.ok_or_else(corrupt)?;
        let event: Relation = decode(&row.payload)?;
        let stored = lookup(connection, tenant, &event.slot_key)?.ok_or_else(corrupt)?;
        if encode(&stored)? != encode(&event)? {
            return Err(corrupt());
        }
    }
    Ok(consumed)
}

fn verification_source(
    connection: &Connection,
    operator: &AuthenticatedCompanyPrincipalV1,
    project_id: &ProjectId,
    session_id: Uuid,
    declined_digest: &str,
    review_id: Uuid,
    now: u64,
) -> Result<AdaptiveMaterialWindowVerificationSourceV1, WorkflowError> {
    crate::adaptive_resume_policy::require_resume_policy_operator(operator, &operator.tenant_id)?;
    validate_digest(declined_digest)?;
    let (resume_source, session) = super::super::adaptive_resume_policy::fresh_resume_source(
        connection,
        &operator.tenant_id,
        project_id,
        session_id,
        now,
        None,
    )?;
    let project: ProjectV1 = get_entity(connection, &operator.tenant_id, "project", &project_id.0)?
        .ok_or_else(not_found)?;
    let calls = super::super::adaptive_leadership_review::calls_for_session(
        connection,
        &operator.tenant_id,
        session_id,
    )?;
    if calls
        .iter()
        .any(|call| call.decision.is_none() && call.retired_at_unix_ms.is_none())
    {
        return Err(transition());
    }
    let refused = calls
        .iter()
        .find(|call| call.grant.review_id == review_id)
        .ok_or_else(not_found)?
        .clone();
    let declined = refused.grant.work_funding.as_ref().ok_or_else(transition)?;
    if !standing_policy::terminal_funded_defer(&refused, &session)
        || refused.context.source_project != project
        || declined.receipt.receipt_digest()? != declined_digest
        || !declined
            .receipt
            .request
            .reason_ref
            .starts_with("standing-funding:")
        || declined.receipt.request.limits.additional_model_calls != 0
        || declined.receipt.request.limits.additional_tool_calls != 0
        || declined
            .receipt
            .request
            .source
            .material_window_expansion
            .is_some()
    {
        return Err(transition());
    }
    if terminal_manual_defer::standing_cause(&declined.receipt)?.is_none() {
        return Err(transition());
    }
    require_standing_origin(connection, &declined.receipt, &session)?;
    let receipts = funding_receipts(connection, &operator.tenant_id, session_id)?;
    let consumed =
        consumed_validated_receipts(connection, &operator.tenant_id, session_id, &receipts)?;
    let adopted = adopted_epochs(&session);
    let terminal = terminal_manual_defer::excluded_by_standing_receipts(
        connection, &receipts, &calls, &session,
    )?;
    let pending = receipts
        .iter()
        .filter(|receipt| {
            !adopted.iter().any(|epoch| epoch.receipt == **receipt)
                && !consumed.contains(&receipt.funding_id)
                && !terminal.contains(&receipt.funding_id)
        })
        .collect::<Vec<_>>();
    if pending.len() != 1 || *pending[0] != declined.receipt || receipts.len() >= 128 {
        return Err(transition());
    }
    let floor = source_floor(&session, declined)?;
    let slot = adaptive_material_window_slot(&resume_source)?;
    if lookup(connection, &operator.tenant_id, &slot)?.is_some() {
        return Err(funding_conflict());
    }
    Ok(AdaptiveMaterialWindowVerificationSourceV1 {
        project,
        session,
        declined: declined.receipt.clone(),
        refused,
        resume_source,
        floor,
    })
}

fn request_for(
    source: &AdaptiveMaterialWindowVerificationSourceV1,
    operation: Uuid,
    reason: &str,
    limits: AdaptiveWorkFundingLimitsV1,
    usage: &AdaptiveMaterialWindowUsageProofV1,
    now: u64,
) -> Result<AdaptiveWorkFundingRequestV1, WorkflowError> {
    let dispatch = source.refused.dispatch.as_ref().ok_or_else(transition)?;
    if reason.starts_with("standing-funding:") {
        return Err(unauthorized());
    }
    validate_digest(&usage.usage_event_digest)?;
    if usage.request_id != source.refused.request_id()
        || usage.request_digest != dispatch.request_digest
        || usage.verified_at_unix_ms < source.refused.updated_at_unix_ms
    {
        return Err(unauthorized());
    }
    let predecessor = source
        .session
        .active_work_funding()
        .ok_or_else(transition)?;
    let request = AdaptiveWorkFundingRequestV1 {
        schema_version: 1,
        operation_id: operation,
        reason_ref: reason.to_owned(),
        limits,
        source: AdaptiveWorkFundingSourceV1 {
            resume_source: source.resume_source.clone(),
            original_model_call_ceiling: source.session.grant.max_model_calls,
            original_tool_call_ceiling: source.session.grant.max_tool_calls,
            current_model_call_ceiling: source.session.funded_model_call_ceiling(),
            current_tool_call_ceiling: source.session.funded_tool_call_ceiling(),
            predecessor_receipt_digest: Some(predecessor.receipt.receipt_digest()?),
            supersedes_unused_receipt_digest: None,
            expired_reviewed_replacement: None,
            material_window_expansion: Some(AdaptiveWorkFundingMaterialExpansionBindingV1 {
                schema_version: 1,
                slot_key: adaptive_material_window_slot(&source.resume_source)?,
                declined_funding_id: source.declined.funding_id.clone(),
                declined_receipt_digest: source.declined.receipt_digest()?,
                refused_review_id: source.refused.grant.review_id,
                refused_call_digest: canonical_sha256(
                    "sentinel.workflow.adaptive-leadership-call.v1",
                    &source.refused,
                )?,
                model_response_digest: source
                    .refused
                    .model_response_digest
                    .clone()
                    .ok_or_else(transition)?,
                usage_event_digest: usage.usage_event_digest.clone(),
                declined_limits: source
                    .refused
                    .grant
                    .work_funding
                    .as_ref()
                    .ok_or_else(transition)?
                    .binding
                    .limits
                    .clone(),
                authoritative_floor: source.floor.clone(),
            }),
        },
    };
    validate_material_window_limits(&request.source, &request.limits, &source.floor, now)?;
    Ok(request)
}

// Only SQL-derived facts are retained. The coordinator-facing source remains
// non-deserializable; clocks and external authority are never cache authority.
#[derive(Clone)]
struct MaterialSourceFacts(AdaptiveMaterialWindowVerificationSourceV1);

impl serde::Serialize for MaterialSourceFacts {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serde::Serialize::serialize(
            &(
                &self.0.project,
                &self.0.session,
                &self.0.declined,
                &self.0.refused,
                &self.0.resume_source,
                &self.0.floor,
            ),
            serializer,
        )
    }
}

fn require_source_clock(
    source: &AdaptiveMaterialWindowVerificationSourceV1,
    now: u64,
) -> Result<(), WorkflowError> {
    // These are exactly fresh_resume_source's time-dependent predicates. A
    // byte-identical database is not proof of current window eligibility.
    if now < source.project.updated_at_unix_ms || now < source.session.updated_at_ms {
        return Err(unauthorized());
    }
    if !source.session.model_window_exhausted_at(now) {
        return Err(transition());
    }
    Ok(())
}

impl WorkflowStore {
    #[allow(clippy::too_many_arguments)]
    fn checked_material_source(
        &self,
        connection: &mut Connection,
        operator: &AuthenticatedCompanyPrincipalV1,
        project: &ProjectId,
        session: Uuid,
        declined_digest: &str,
        review: Uuid,
        now: u64,
    ) -> Result<AdaptiveMaterialWindowVerificationSourceV1, WorkflowError> {
        let query_key = encode(&(operator, project, session, declined_digest, review))?;
        let facts = self.validated_read_snapshot(
            connection,
            "material-window-source",
            &query_key,
            |snapshot| {
                let scope = validation_scope::enter(snapshot)?;
                let source = verification_source(
                    snapshot,
                    operator,
                    project,
                    session,
                    declined_digest,
                    review,
                    now,
                )?;
                scope.finish()?;
                Ok(MaterialSourceFacts(source))
            },
        )?;
        require_source_clock(&facts.0, now)?;
        Ok(facts.0)
    }

    pub fn adaptive_material_window_verification_source(
        &self,
        operator: &AuthenticatedCompanyPrincipalV1,
        project: &ProjectId,
        session: Uuid,
        declined_digest: &str,
        review: Uuid,
        now: u64,
    ) -> Result<AdaptiveMaterialWindowVerificationSourceV1, WorkflowError> {
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        self.checked_material_source(
            &mut connection,
            operator,
            project,
            session,
            declined_digest,
            review,
            now,
        )
    }

    pub fn adaptive_material_window_draft(
        &self,
        operator: &AuthenticatedCompanyPrincipalV1,
        source: &AdaptiveMaterialWindowVerificationSourceV1,
        operation: Uuid,
        reason: &str,
        limits: AdaptiveWorkFundingLimitsV1,
        proof: &AdaptiveMaterialWindowUsageProofV1,
        now: u64,
    ) -> Result<AdaptiveWorkFundingRequestV1, WorkflowError> {
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let fresh = self.checked_material_source(
            &mut connection,
            operator,
            &source.project.project_id,
            source.session.grant.session_id,
            &source.declined.receipt_digest()?,
            source.refused.grant.review_id,
            now,
        )?;
        if fresh != *source {
            return Err(transition());
        }
        let request = request_for(&fresh, operation, reason, limits, proof, now)?;
        request.validate_at(operator, now)?;
        Ok(request)
    }

    /// Caller holds its mutation fence. The callback must NEVER call WorkflowStore.
    pub fn authorize_adaptive_material_window<F>(
        &self,
        operator: &AuthenticatedCompanyPrincipalV1,
        request: &AdaptiveWorkFundingRequestV1,
        now: u64,
        verify: F,
    ) -> Result<(bool, AdaptiveWorkFundingReceiptV1), WorkflowError>
    where
        F: FnOnce() -> Result<AdaptiveMaterialWindowUsageProofV1, WorkflowError>,
    {
        let anchor = &request.source.resume_source;
        crate::adaptive_resume_policy::require_resume_policy_operator(operator, &anchor.tenant_id)?;
        let binding = request
            .source
            .material_window_expansion
            .as_ref()
            .ok_or_else(unauthorized)?;
        binding.validate(anchor)?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let scope = validation_scope::enter(&transaction)?;
        if let Some(prior) = lookup(&transaction, &anchor.tenant_id, &binding.slot_key)? {
            let stored = read_funding_leaf(
                &transaction,
                &anchor.tenant_id,
                anchor.session_id,
                prior.operation_id,
            )?
            .ok_or_else(corrupt)?;
            if stored.request != *request || prior.issuer != *operator {
                return Err(funding_conflict());
            }
            scope.finish()?;
            return Ok((true, stored));
        }
        request.validate_at(operator, now)?;
        let source = verification_source(
            &transaction,
            operator,
            &anchor.project_id,
            anchor.session_id,
            &binding.declined_receipt_digest,
            binding.refused_review_id,
            now,
        )?;
        // This stamp is descriptive only: compare the complete SQL-derived
        // source before external reads; the trusted callback must still prove usage.
        let expected = request_for(
            &source,
            request.operation_id,
            &request.reason_ref,
            request.limits.clone(),
            &AdaptiveMaterialWindowUsageProofV1 {
                request_id: source.refused.request_id(),
                request_digest: source
                    .refused
                    .dispatch
                    .as_ref()
                    .ok_or_else(transition)?
                    .request_digest
                    .clone(),
                usage_event_digest: binding.usage_event_digest.clone(),
                verified_at_unix_ms: now,
            },
            now,
        )?;
        if expected != *request {
            return Err(transition());
        }
        let proof = verify()?;
        if proof.verified_at_unix_ms < now {
            return Err(transition());
        }
        let fresh = request_for(
            &source,
            request.operation_id,
            &request.reason_ref,
            request.limits.clone(),
            &proof,
            proof.verified_at_unix_ms,
        )?;
        if fresh != *request {
            return Err(transition());
        }
        request.validate_at(operator, proof.verified_at_unix_ms)?;
        if read_funding_leaf(
            &transaction,
            &anchor.tenant_id,
            anchor.session_id,
            request.operation_id,
        )?
        .is_some()
        {
            return Err(funding_conflict());
        }
        let funding = AdaptiveWorkFundingReceiptV1 {
            schema_version: 1,
            funding_id: adaptive_work_funding_id(
                &anchor.tenant_id,
                anchor.session_id,
                request.operation_id,
            )?,
            request: request.clone(),
            issuer_principal: operator.clone(),
            issued_at_unix_ms: proof.verified_at_unix_ms,
        };
        let relation = Relation {
            schema_version: 1,
            slot_key: binding.slot_key.clone(),
            tenant_id: anchor.tenant_id.clone(),
            project_id: anchor.project_id.clone(),
            session_id: anchor.session_id,
            operation_id: request.operation_id,
            binding: binding.clone(),
            new_funding_id: funding.funding_id.clone(),
            new_receipt_digest: funding.receipt_digest()?,
            issuer: operator.clone(),
            issued_at_unix_ms: funding.issued_at_unix_ms,
        };
        scope.finish()?;
        put_funding_receipt(&transaction, &funding)?;
        put_entity(
            &transaction,
            &anchor.tenant_id,
            KIND,
            &relation.slot_key,
            1,
            &relation,
        )?;
        append_event(
            &transaction,
            operator,
            request.operation_id,
            &canonical_sha256(DOMAIN, &relation)?,
            Some(&anchor.project_id),
            EVENT,
            &relation,
            funding.issued_at_unix_ms,
        )?;
        require_successor_membership(&transaction, &funding)?;
        transaction.commit()?;
        Ok((false, funding))
    }
}
