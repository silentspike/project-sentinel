//! Immutable funding proposals. Issuance deliberately changes no execution state.

use super::*;
use crate::adaptive_work_funding::*;
use crate::{
    AdaptiveContinuationAuthorizationV1, AdaptiveLeadershipReviewCallV1,
    AdaptiveLeadershipReviewGrantV1, RuntimeAuthoritySnapshotV1,
};

const FUNDING_KIND: &str = "adaptive_work_funding";
const FUNDING_EVENT: &str = "adaptive_work_funding_issued";
const REVIEW_KIND: &str = "adaptive_work_funding_review";
const REVIEW_EVENT: &str = "adaptive_work_funding_review_issued";
const ADOPTION_KIND: &str = "adaptive_work_funding_adoption";
const ADOPTION_EVENT: &str = "adaptive_work_funding_adopted";

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FundingReviewLeaf {
    schema_version: u16,
    membership_id: String,
    grant: AdaptiveLeadershipReviewGrantV1,
    operation_id: Uuid,
    context_digest: String,
    issued_at_unix_ms: u64,
}

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FundingAdoptionLeaf {
    schema_version: u16,
    membership_id: String,
    authorization: AdaptiveContinuationAuthorizationV1,
    authority: RuntimeAuthoritySnapshotV1,
    review_membership_id: String,
    issuer_principal: AuthenticatedCompanyPrincipalV1,
}

fn review_id(epoch: &AdaptiveWorkFundingEpochV1) -> String {
    format!(
        "{}-review-{:03}",
        epoch.binding.funding_id, epoch.binding.ordinal
    )
}

fn require_unique_membership_event(
    connection: &Connection,
    tenant: &TenantId,
    event_type: &str,
    operation_id: Uuid,
    membership_id: &str,
) -> Result<(), WorkflowError> {
    let count: i64 = connection.query_row(
        "SELECT count(*) FROM (SELECT sequence FROM company_events WHERE tenant_id=?1 AND event_type=?2
         AND (operation_id=?3 OR json_extract(payload,'$.membership_id')=?4) LIMIT 2)",
        params![tenant.0, event_type, operation_id.to_string(), membership_id], |row| row.get(0))?;
    if count != 1 {
        return Err(corrupt());
    }
    Ok(())
}

fn adoption_id(
    authority: &RuntimeAuthoritySnapshotV1,
    authorization: &AdaptiveContinuationAuthorizationV1,
) -> Result<String, WorkflowError> {
    Ok(format!(
        "work-funding-adoption-{}",
        canonical_sha256(
            "sentinel.workflow.adaptive-work-funding-adoption-key.v1",
            &(
                &authority.tenant_id,
                authorization.session_id,
                authorization.operation_id
            ),
        )?
    ))
}

fn require_epoch_receipt(
    connection: &Connection,
    epoch: &AdaptiveWorkFundingEpochV1,
) -> Result<(), WorkflowError> {
    epoch.validate()?;
    let source = &epoch.receipt.request.source.resume_source;
    let stored = read_funding_leaf(
        connection,
        &source.tenant_id,
        source.session_id,
        epoch.receipt.request.operation_id,
    )?
    .ok_or_else(unauthorized)?;
    if stored != epoch.receipt {
        return Err(corrupt());
    }
    Ok(())
}

fn require_review_binding(
    grant: &AdaptiveLeadershipReviewGrantV1,
) -> Result<&AdaptiveWorkFundingEpochV1, WorkflowError> {
    let epoch = grant.work_funding.as_deref().ok_or_else(unauthorized)?;
    epoch.validate()?;
    let source = &epoch.receipt.request.source.resume_source;
    if grant.schema_version != 5
        || grant.leadership_principal.tenant_id != source.tenant_id
        || grant.leadership_principal.kind != CompanyPrincipalKindV1::Agent
        || !matches!(
            grant.leadership_principal.role,
            CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
        )
        || grant.session_id != source.session_id
        || grant.project_id != source.project_id
        || grant.work_item_id != source.work_item_id
        || grant.assignee_authority != source.assignee_authority
        || grant.expected_session_version < source.expected_session_version
        || grant.expected_project_version < source.expected_project_version
        || grant.expires_at_unix_ms > epoch.binding.limits.expires_at_unix_ms
        || grant.max_duration_ms > epoch.binding.limits.max_call_duration_ms
        || grant.resume_policy.is_some()
        || grant.recovery_epoch.is_some()
    {
        return Err(unauthorized());
    }
    Ok(epoch)
}

impl CompanyEntity for FundingReviewLeaf {
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        (
            &self.grant.leadership_principal.tenant_id,
            REVIEW_KIND,
            &self.membership_id,
            1,
        )
    }
    fn validate_entity(&self) -> Result<(), WorkflowError> {
        let epoch = require_review_binding(&self.grant)?;
        self.grant.validate(self.issued_at_unix_ms)?;
        validate_digest(&self.context_digest)?;
        if self.schema_version != 1
            || self.operation_id.is_nil()
            || self.membership_id != review_id(epoch)
            || self.issued_at_unix_ms < epoch.receipt.issued_at_unix_ms
            || self.issued_at_unix_ms >= self.grant.expires_at_unix_ms
        {
            return Err(corrupt());
        }
        Ok(())
    }
    fn validate_persisted(&self, connection: &Connection) -> Result<(), WorkflowError> {
        require_epoch_receipt(connection, require_review_binding(&self.grant)?)?;
        require_unique_membership_event(
            connection,
            &self.grant.leadership_principal.tenant_id,
            REVIEW_EVENT,
            self.operation_id,
            &self.membership_id,
        )?;
        adaptive_resume_policy::require_leaf_event(
            connection,
            &self.grant.leadership_principal.tenant_id,
            &self.grant.project_id,
            REVIEW_EVENT,
            self.operation_id,
            &canonical_sha256("sentinel.workflow.adaptive-work-funding-review.v1", self)?,
            &self.grant.leadership_principal,
            self.issued_at_unix_ms,
            self,
        )
    }
}

impl CompanyEntity for FundingAdoptionLeaf {
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        (
            &self.authority.tenant_id,
            ADOPTION_KIND,
            &self.membership_id,
            1,
        )
    }
    fn validate_entity(&self) -> Result<(), WorkflowError> {
        self.authorization.validate()?;
        self.authority.validate()?;
        self.issuer_principal.validate()?;
        let epoch = self
            .authorization
            .work_funding
            .as_deref()
            .ok_or_else(corrupt)?;
        let source = &epoch.receipt.request.source.resume_source;
        if self.schema_version != 1
            || self.membership_id != adoption_id(&self.authority, &self.authorization)?
            || self.review_membership_id != review_id(epoch)
            || source.assignee_authority != self.authority
            || source.session_id != self.authorization.session_id
            || self.issuer_principal.tenant_id != self.authority.tenant_id
            || self.issuer_principal.kind != CompanyPrincipalKindV1::Agent
            || !matches!(
                self.issuer_principal.role,
                CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
            )
        {
            return Err(corrupt());
        }
        Ok(())
    }
    fn validate_persisted(&self, connection: &Connection) -> Result<(), WorkflowError> {
        let review: FundingReviewLeaf = get_entity(
            connection,
            &self.authority.tenant_id,
            REVIEW_KIND,
            &self.review_membership_id,
        )?
        .ok_or_else(corrupt)?;
        require_adoption_review(self, &review)?;
        require_unique_membership_event(
            connection,
            &self.authority.tenant_id,
            ADOPTION_EVENT,
            self.authorization.operation_id,
            &self.membership_id,
        )?;
        adaptive_resume_policy::require_leaf_event(
            connection,
            &self.authority.tenant_id,
            &self.authority.project_id,
            ADOPTION_EVENT,
            self.authorization.operation_id,
            &canonical_sha256("sentinel.workflow.adaptive-work-funding-adoption.v1", self)?,
            &self.issuer_principal,
            self.authorization.issued_at_ms,
            self,
        )
    }
}

fn require_adoption_review(
    leaf: &FundingAdoptionLeaf,
    review: &FundingReviewLeaf,
) -> Result<(), WorkflowError> {
    let auth = &leaf.authorization;
    let epoch = auth.work_funding.as_deref().ok_or_else(corrupt)?;
    if review.grant.work_funding.as_deref() != Some(epoch)
        || review.operation_id != auth.operation_id
        || review.grant.review_id != auth.review_id
        || review.grant.session_id != auth.session_id
        || review.grant.expected_session_version != auth.source_session_version
        || review.grant.assignee_authority != leaf.authority
        || review.grant.leadership_principal != leaf.issuer_principal
        || review.issued_at_unix_ms > auth.issued_at_ms
        || auth.issued_at_ms >= review.grant.expires_at_unix_ms
    {
        return Err(corrupt());
    }
    Ok(())
}

pub(crate) fn require_funding_review_membership(
    connection: &Connection,
    grant: &AdaptiveLeadershipReviewGrantV1,
    context_digest: &str,
    operation_id: Uuid,
) -> Result<(), WorkflowError> {
    validation_scope::with_scope(connection, || {
        let epoch = require_review_binding(grant)?;
        let leaf: FundingReviewLeaf = get_entity(
            connection,
            &grant.leadership_principal.tenant_id,
            REVIEW_KIND,
            &review_id(epoch),
        )?
        .ok_or_else(corrupt)?;
        if leaf.grant != *grant
            || leaf.context_digest != context_digest
            || leaf.operation_id != operation_id
        {
            return Err(corrupt());
        }
        Ok(())
    })
}

pub(super) fn insert_funding_review_membership(
    transaction: &Transaction<'_>,
    grant: &AdaptiveLeadershipReviewGrantV1,
    context_digest: &str,
    operation_id: Uuid,
    issued_at_ms: u64,
) -> Result<(), WorkflowError> {
    let epoch = require_review_binding(grant)?;
    require_epoch_receipt(transaction, epoch)?;
    let leaf = FundingReviewLeaf {
        schema_version: 1,
        membership_id: review_id(epoch),
        grant: grant.clone(),
        operation_id,
        context_digest: context_digest.to_owned(),
        issued_at_unix_ms: issued_at_ms,
    };
    leaf.validate_entity()?;
    let tenant = &grant.leadership_principal.tenant_id;
    if let Some(prior) =
        get_entity::<FundingReviewLeaf>(transaction, tenant, REVIEW_KIND, &leaf.membership_id)?
    {
        return if prior == leaf {
            Ok(())
        } else {
            Err(funding_conflict())
        };
    }
    let duplicate: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM company_events WHERE tenant_id=?1 AND event_type=?2
         AND (operation_id=?3 OR json_extract(payload,'$.membership_id')=?4))",
        params![
            tenant.0,
            REVIEW_EVENT,
            operation_id.to_string(),
            leaf.membership_id
        ],
        |row| row.get(0),
    )?;
    if duplicate {
        return Err(corrupt());
    }
    put_entity(
        transaction,
        tenant,
        REVIEW_KIND,
        &leaf.membership_id,
        1,
        &leaf,
    )?;
    append_event(
        transaction,
        &grant.leadership_principal,
        operation_id,
        &canonical_sha256("sentinel.workflow.adaptive-work-funding-review.v1", &leaf)?,
        Some(&grant.project_id),
        REVIEW_EVENT,
        &leaf,
        issued_at_ms,
    )?;
    Ok(())
}

pub(crate) fn require_funding_authorization_membership(
    connection: &Connection,
    authorization: &AdaptiveContinuationAuthorizationV1,
    authority: &RuntimeAuthoritySnapshotV1,
) -> Result<(), WorkflowError> {
    validation_scope::with_scope(connection, || {
        let leaf: FundingAdoptionLeaf = get_entity(
            connection,
            &authority.tenant_id,
            ADOPTION_KIND,
            &adoption_id(authority, authorization)?,
        )?
        .ok_or_else(corrupt)?;
        if leaf.authorization != *authorization || leaf.authority != *authority {
            return Err(corrupt());
        }
        Ok(())
    })
}

pub(crate) fn insert_funding_adoption_membership(
    transaction: &Transaction<'_>,
    authorization: &AdaptiveContinuationAuthorizationV1,
    authority: &RuntimeAuthoritySnapshotV1,
) -> Result<(), WorkflowError> {
    let epoch = authorization
        .work_funding
        .as_deref()
        .ok_or_else(unauthorized)?;
    let review: FundingReviewLeaf = get_entity(
        transaction,
        &authority.tenant_id,
        REVIEW_KIND,
        &review_id(epoch),
    )?
    .ok_or_else(corrupt)?;
    let leaf = FundingAdoptionLeaf {
        schema_version: 1,
        membership_id: adoption_id(authority, authorization)?,
        authorization: authorization.clone(),
        authority: authority.clone(),
        review_membership_id: review.membership_id.clone(),
        issuer_principal: review.grant.leadership_principal.clone(),
    };
    leaf.validate_entity()?;
    require_adoption_review(&leaf, &review)?;
    if let Some(prior) = get_entity::<FundingAdoptionLeaf>(
        transaction,
        &authority.tenant_id,
        ADOPTION_KIND,
        &leaf.membership_id,
    )? {
        return if prior == leaf {
            Ok(())
        } else {
            Err(funding_conflict())
        };
    }
    let orphan: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM company_events WHERE tenant_id=?1 AND event_type=?2
         AND (operation_id=?3 OR json_extract(payload,'$.membership_id')=?4))",
        params![
            authority.tenant_id.0,
            ADOPTION_EVENT,
            authorization.operation_id.to_string(),
            leaf.membership_id
        ],
        |row| row.get(0),
    )?;
    if orphan {
        return Err(corrupt());
    }
    put_entity(
        transaction,
        &authority.tenant_id,
        ADOPTION_KIND,
        &leaf.membership_id,
        1,
        &leaf,
    )?;
    append_event(
        transaction,
        &leaf.issuer_principal,
        authorization.operation_id,
        &canonical_sha256("sentinel.workflow.adaptive-work-funding-adoption.v1", &leaf)?,
        Some(&authority.project_id),
        ADOPTION_EVENT,
        &leaf,
        authorization.issued_at_ms,
    )?;
    Ok(())
}

impl CompanyEntity for AdaptiveWorkFundingReceiptV1 {
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        (
            &self.request.source.resume_source.tenant_id,
            FUNDING_KIND,
            &self.funding_id,
            1,
        )
    }

    fn validate_entity(&self) -> Result<(), WorkflowError> {
        self.validate()
    }

    fn validate_persisted(&self, connection: &Connection) -> Result<(), WorkflowError> {
        let source = &self.request.source.resume_source;
        let mut statement = connection.prepare(
            "SELECT sequence FROM company_events WHERE tenant_id=?1 AND event_type=?2
             AND (operation_id=?3 OR json_extract(payload,'$.funding_id')=?4) LIMIT 2",
        )?;
        let ids = statement
            .query_map(
                params![
                    source.tenant_id.0,
                    FUNDING_EVENT,
                    self.request.operation_id.to_string(),
                    self.funding_id
                ],
                |row| row.get::<_, i64>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        if ids.len() != 1 {
            return Err(corrupt());
        }
        adaptive_resume_policy::require_leaf_event(
            connection,
            &source.tenant_id,
            &source.project_id,
            FUNDING_EVENT,
            self.request.operation_id,
            &self.request.canonical_digest()?,
            &self.issuer_principal,
            self.issued_at_unix_ms,
            self,
        )
    }
}

fn read_funding_leaf(
    connection: &Connection,
    tenant: &TenantId,
    session_id: Uuid,
    operation_id: Uuid,
) -> Result<Option<AdaptiveWorkFundingReceiptV1>, WorkflowError> {
    read_funding_leaf_with_observer(connection, tenant, session_id, operation_id, || Ok(()))
}

fn read_funding_leaf_with_observer(
    connection: &Connection,
    tenant: &TenantId,
    session_id: Uuid,
    operation_id: Uuid,
    after_entity_read: impl FnOnce() -> Result<(), WorkflowError>,
) -> Result<Option<AdaptiveWorkFundingReceiptV1>, WorkflowError> {
    validation_scope::with_scope(connection, || {
        let key = adaptive_work_funding_id(tenant, session_id, operation_id)?;
        let receipt = get_entity(connection, tenant, FUNDING_KIND, &key)?;
        after_entity_read()?;
        if receipt.is_none() {
            let orphan: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM company_events WHERE tenant_id=?1 AND event_type=?2
             AND (operation_id=?3 OR json_extract(payload,'$.funding_id')=?4))",
                params![tenant.0, FUNDING_EVENT, operation_id.to_string(), key],
                |row| row.get(0),
            )?;
            if orphan {
                return Err(corrupt());
            }
        }
        Ok(receipt)
    })
}

fn funding_receipts(
    connection: &Connection,
    tenant: &TenantId,
    session_id: Uuid,
) -> Result<Vec<AdaptiveWorkFundingReceiptV1>, WorkflowError> {
    let mut statement = connection.prepare(
        "SELECT entity_id FROM company_entities WHERE tenant_id=?1 AND entity_kind=?2
         ORDER BY entity_id LIMIT 4097",
    )?;
    let ids = statement
        .query_map(params![tenant.0, FUNDING_KIND], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if ids.len() > 4096 {
        return Err(corrupt());
    }
    let mut receipts = Vec::with_capacity(ids.len());
    for id in ids {
        let receipt: AdaptiveWorkFundingReceiptV1 =
            get_entity(connection, tenant, FUNDING_KIND, &id)?.ok_or_else(corrupt)?;
        // Validate key, payload, issuer and event before selecting by payload.
        // A corrupted session field must not hide a pending epoch.
        if receipt.request.source.resume_source.session_id == session_id {
            receipts.push(receipt);
        }
    }
    if receipts.len() > 128 {
        return Err(corrupt());
    }
    let mut events = connection.prepare(
        "SELECT sequence FROM company_events WHERE tenant_id=?1 AND event_type=?2 ORDER BY sequence LIMIT 4097")?;
    let ids = events
        .query_map(params![tenant.0, FUNDING_EVENT], |row| row.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if ids.len() > 4096 {
        return Err(corrupt());
    }
    // The event selector cannot trust an unvalidated payload either. Match
    // every bounded issuance event to its typed, event-verified entity first.
    for sequence in ids {
        let row = read_company_event_row(connection, stored_u64(sequence)?)?.ok_or_else(corrupt)?;
        validation_scope::charge_bytes(connection, row.payload.len())?;
        let event_receipt: AdaptiveWorkFundingReceiptV1 = decode(&row.payload)?;
        let stored: AdaptiveWorkFundingReceiptV1 =
            get_entity(connection, tenant, FUNDING_KIND, &event_receipt.funding_id)?
                .ok_or_else(corrupt)?;
        if stored != event_receipt {
            return Err(corrupt());
        }
    }
    Ok(receipts)
}

fn adopted_epochs(session: &crate::AdaptiveSessionV1) -> Vec<&AdaptiveWorkFundingEpochV1> {
    let mut epochs = Vec::new();
    if let Some(state) = &session.continuation {
        for authorization in &state.authorizations {
            if let Some(epoch) = authorization.work_funding.as_deref() {
                if !epochs
                    .iter()
                    .any(|prior: &&AdaptiveWorkFundingEpochV1| prior.same_epoch(epoch))
                {
                    epochs.push(epoch);
                }
            }
        }
    }
    epochs
}

// Journal replay proves every adopted leaf; a pending unadopted proposal cannot
// be replaced or refunded by another issuance.
fn require_unfunded_source(
    connection: &Connection,
    tenant: &TenantId,
    project_id: &ProjectId,
    session_id: Uuid,
    now_ms: u64,
) -> Result<(AdaptiveWorkFundingSourceV1, u64), WorkflowError> {
    let receipts = funding_receipts(connection, tenant, session_id)?;
    let (resume_source, session) = adaptive_resume_policy::fresh_resume_source(
        connection, tenant, project_id, session_id, now_ms, None,
    )?;
    let adopted = adopted_epochs(&session);
    if receipts
        .iter()
        .any(|receipt| !adopted.iter().any(|epoch| epoch.receipt == *receipt))
    {
        return Err(transition());
    }
    if adopted.len() != receipts.len() {
        return Err(corrupt());
    }
    let source = AdaptiveWorkFundingSourceV1 {
        resume_source,
        original_model_call_ceiling: session.grant.max_model_calls,
        original_tool_call_ceiling: session.grant.max_tool_calls,
        current_model_call_ceiling: session.funded_model_call_ceiling(),
        current_tool_call_ceiling: session.funded_tool_call_ceiling(),
        predecessor_receipt_digest: session
            .active_work_funding()
            .map(|epoch| epoch.receipt.receipt_digest())
            .transpose()?,
    };
    source.validate()?;
    Ok((source, session.grant.max_call_duration_ms))
}

pub(super) fn require_fresh_funding_review_source(
    connection: &Connection,
    call: &AdaptiveLeadershipReviewCallV1,
) -> Result<(), WorkflowError> {
    let epoch = require_review_binding(&call.grant)?;
    require_epoch_receipt(connection, epoch)?;
    let source = &epoch.receipt.request.source;
    let (session, root_digest, head_digest) =
        crate::store::adaptive::adaptive_resume_journal_source(connection, call.grant.session_id)?
            .ok_or_else(not_found)?;
    if session != call.context.source_session
        || !matches!(session.cursor, crate::AdaptiveCursorV1::ReadyForModel)
        || session.grant.authority != source.resume_source.assignee_authority
        || session.grant.max_model_calls != source.original_model_call_ceiling
        || session.grant.max_tool_calls != source.original_tool_call_ceiling
        || session.model_calls >= epoch.binding.limits.total_model_call_ceiling
        || session.tool_calls >= epoch.binding.limits.total_tool_call_ceiling
        || session
            .continuation
            .as_ref()
            .map_or(0, |state| state.authorizations.len())
            >= usize::from(epoch.binding.limits.total_window_ceiling)
    {
        return Err(transition());
    }
    if session
        .active_work_funding()
        .is_some_and(|active| active.same_epoch(epoch))
    {
        if session.funded_model_call_ceiling() != epoch.binding.limits.total_model_call_ceiling
            || session.funded_tool_call_ceiling() != epoch.binding.limits.total_tool_call_ceiling
        {
            return Err(corrupt());
        }
    } else {
        let initial = &source.resume_source;
        // The stored row digest has its own domain. Read it instead of deriving
        // one from a possibly caller-supplied context representation.
        let stored_project_digest: String = connection.query_row(
            "SELECT payload_digest FROM company_entities WHERE tenant_id=?1 AND entity_kind='project' AND entity_id=?2",
            params![initial.tenant_id.0, initial.project_id.0], |row| row.get(0))?;
        if session.version != initial.expected_session_version
            || call.context.source_project.version != initial.expected_project_version
            || stored_project_digest != initial.project_payload_digest
            || root_digest != initial.root_entry_digest
            || head_digest != initial.head_entry_digest
            || crate::adaptive_budget_history_digest(&session.continuation)?
                != initial.continuation_history_digest
            || session.model_calls != initial.base_model_calls
            || session.tool_calls != initial.base_tool_calls
            || session.funded_model_call_ceiling() != source.current_model_call_ceiling
            || session.funded_tool_call_ceiling() != source.current_tool_call_ceiling
            || session
                .active_work_funding()
                .map(|active| active.receipt.receipt_digest())
                .transpose()?
                != source.predecessor_receipt_digest
            || epoch.binding.ordinal
                != initial
                    .base_review_count
                    .checked_add(1)
                    .ok_or_else(corrupt)?
        {
            return Err(transition());
        }
    }
    Ok(())
}

impl WorkflowStore {
    /// Selects a proposed or already adopted epoch for one fresh real review.
    /// Refusal, retirement and expiry on the same head never generate a reroll.
    pub fn adaptive_work_funding_for_review(
        &self,
        tenant: &TenantId,
        session_id: Uuid,
        now_ms: u64,
    ) -> Result<Option<AdaptiveWorkFundingEpochV1>, WorkflowError> {
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let scope = validation_scope::enter(&transaction)?;
        let (session, _) =
            crate::store::adaptive::load(&transaction, session_id)?.ok_or_else(not_found)?;
        crate::store::adaptive::require_head(&transaction, &session)?;
        if session.grant.authority.tenant_id != *tenant {
            return Err(unauthorized());
        }
        if !matches!(session.cursor, crate::AdaptiveCursorV1::ReadyForModel)
            || !session.model_window_exhausted_at(now_ms)
        {
            scope.finish()?;
            return Ok(None);
        }
        let calls =
            adaptive_leadership_review::calls_for_session(&transaction, tenant, session_id)?;
        if calls
            .iter()
            .any(|call| call.decision.is_none() && call.retired_at_unix_ms.is_none())
        {
            scope.finish()?;
            return Ok(None);
        }
        let receipts = funding_receipts(&transaction, tenant, session_id)?;
        let adopted = adopted_epochs(&session);
        let pending: Vec<_> = receipts
            .iter()
            .filter(|receipt| !adopted.iter().any(|epoch| epoch.receipt == **receipt))
            .collect();
        let receipt = match pending.as_slice() {
            [] => session.active_work_funding().map(|epoch| &epoch.receipt),
            [receipt] => Some(*receipt),
            _ => return Err(corrupt()),
        };
        let Some(receipt) = receipt else {
            scope.finish()?;
            return Ok(None);
        };
        if now_ms < receipt.issued_at_unix_ms
            || now_ms
                .checked_add(receipt.request.limits.max_call_duration_ms)
                .and_then(|time| time.checked_add(receipt.request.limits.dispatch_margin_ms))
                .is_none_or(|deadline| deadline > receipt.request.limits.expires_at_unix_ms)
            || session.model_calls >= receipt.resulting_model_call_ceiling()?
            || session.tool_calls >= receipt.resulting_tool_call_ceiling()?
            || session
                .continuation
                .as_ref()
                .map_or(0, |state| state.authorizations.len())
                >= usize::from(
                    receipt
                        .binding(
                            receipt
                                .request
                                .source
                                .resume_source
                                .base_review_count
                                .checked_add(1)
                                .ok_or_else(corrupt)?,
                        )?
                        .limits
                        .total_window_ceiling,
                )
        {
            scope.finish()?;
            return Ok(None);
        }
        let ordinal = u16::try_from(calls.len())
            .map_err(|_| corrupt())?
            .checked_add(1)
            .ok_or_else(corrupt)?;
        if ordinal
            > receipt
                .request
                .source
                .resume_source
                .base_review_count
                .checked_add(receipt.request.limits.additional_reviews)
                .ok_or_else(corrupt)?
            || calls.iter().any(|call| {
                call.grant.work_funding.as_ref().is_some_and(|prior| {
                    prior.receipt == *receipt
                        && (call.grant.expected_session_version >= session.version
                            || (session.model_calls <= call.context.source_session.model_calls
                                && session.tool_calls <= call.context.source_session.tool_calls))
                })
            })
        {
            scope.finish()?;
            return Ok(None);
        }
        let epoch = AdaptiveWorkFundingEpochV1 {
            receipt: receipt.clone(),
            binding: receipt.binding(ordinal)?,
        };
        // First review is tied to the exact proposal head. A progressed adopted
        // epoch is checked again at claim/dispatch against the journal.
        if !session
            .active_work_funding()
            .is_some_and(|active| active.same_epoch(&epoch))
        {
            let (fresh, _) = adaptive_resume_policy::fresh_resume_source(
                &transaction,
                tenant,
                &session.grant.authority.project_id,
                session_id,
                now_ms,
                None,
            )?;
            if fresh != receipt.request.source.resume_source {
                return Err(transition());
            }
        }
        epoch.validate()?;
        scope.finish()?;
        Ok(Some(epoch))
    }

    /// Authenticates the next funding proposal without granting capacity.
    pub fn adaptive_work_funding_draft(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        project_id: &ProjectId,
        session_id: Uuid,
        operation_id: Uuid,
        reason_ref: &str,
        limits: AdaptiveWorkFundingLimitsV1,
        now_ms: u64,
    ) -> Result<AdaptiveWorkFundingRequestV1, WorkflowError> {
        crate::adaptive_resume_policy::require_resume_policy_operator(
            principal,
            &principal.tenant_id,
        )?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let scope = validation_scope::enter(&transaction)?;
        if let Some(prior) =
            read_funding_leaf(&transaction, &principal.tenant_id, session_id, operation_id)?
        {
            if prior.issuer_principal != *principal
                || prior.request.source.resume_source.project_id != *project_id
                || prior.request.reason_ref != reason_ref
                || prior.request.limits != limits
            {
                return Err(funding_conflict());
            }
            scope.finish()?;
            return Ok(prior.request);
        }
        let (source, duration) = require_unfunded_source(
            &transaction,
            &principal.tenant_id,
            project_id,
            session_id,
            now_ms,
        )?;
        let request = AdaptiveWorkFundingRequestV1 {
            schema_version: 1,
            operation_id,
            source,
            limits,
            reason_ref: reason_ref.to_owned(),
        };
        request.validate_at(principal, now_ms)?;
        if request.limits.max_call_duration_ms != duration {
            return Err(transition());
        }
        scope.finish()?;
        Ok(request)
    }

    /// Writes only an immutable receipt and its event. No execution, review or
    /// allowance is issued; productive adoption is a separate model decision.
    pub fn authorize_adaptive_work_funding(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        request: &AdaptiveWorkFundingRequestV1,
        now_ms: u64,
    ) -> Result<(bool, AdaptiveWorkFundingReceiptV1), WorkflowError> {
        let source = &request.source.resume_source;
        crate::adaptive_resume_policy::require_resume_policy_operator(
            principal,
            &source.tenant_id,
        )?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let scope = validation_scope::enter(&transaction)?;
        if let Some(prior) = read_funding_leaf(
            &transaction,
            &source.tenant_id,
            source.session_id,
            request.operation_id,
        )? {
            if prior.request != *request || prior.issuer_principal != *principal {
                return Err(funding_conflict());
            }
            scope.finish()?;
            return Ok((true, prior));
        }
        request.validate_at(principal, now_ms)?;
        let (fresh, duration) = require_unfunded_source(
            &transaction,
            &source.tenant_id,
            &source.project_id,
            source.session_id,
            now_ms,
        )?;
        if fresh != request.source || request.limits.max_call_duration_ms != duration {
            return Err(transition());
        }
        let receipt = AdaptiveWorkFundingReceiptV1 {
            schema_version: 1,
            funding_id: adaptive_work_funding_id(
                &source.tenant_id,
                source.session_id,
                request.operation_id,
            )?,
            request: request.clone(),
            issuer_principal: principal.clone(),
            issued_at_unix_ms: now_ms,
        };
        receipt.validate()?;
        scope.finish()?;
        put_entity(
            &transaction,
            &source.tenant_id,
            FUNDING_KIND,
            &receipt.funding_id,
            1,
            &receipt,
        )?;
        append_event(
            &transaction,
            principal,
            request.operation_id,
            &request.canonical_digest()?,
            Some(&source.project_id),
            FUNDING_EVENT,
            &receipt,
            now_ms,
        )?;
        transaction.commit()?;
        Ok((false, receipt))
    }

    /// Historical receipt lookup, not an executable allowance or latest epoch.
    pub fn adaptive_work_funding(
        &self,
        tenant: &TenantId,
        session_id: Uuid,
        operation_id: Uuid,
    ) -> Result<Option<AdaptiveWorkFundingReceiptV1>, WorkflowError> {
        let connection = self.connection.lock().map_err(|_| persistence())?;
        read_funding_leaf(&connection, tenant, session_id, operation_id)
    }
}

fn funding_conflict() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::IdempotencyConflict,
        false,
        "adaptive work funding is immutable",
    )
}

// Inject a second connection's atomic commit between the two actual reads.
#[cfg(test)]
pub(super) fn read_during_issuance(
    connection: &Connection,
    tenant: &TenantId,
    session_id: Uuid,
    operation_id: Uuid,
    issue: impl FnOnce() -> Result<(), WorkflowError>,
) -> Result<Option<AdaptiveWorkFundingReceiptV1>, WorkflowError> {
    read_funding_leaf_with_observer(connection, tenant, session_id, operation_id, issue)
}
