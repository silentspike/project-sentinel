//! Sealed replacement relations are historical evidence, never fresh allowances.

use super::super::{adaptive_leadership_review, adaptive_resume_policy, unauthorized};
use super::*;
use crate::*;
use serde::Deserialize;

const KIND: &str = "adaptive_work_funding_expired_replacement";
const EVENT: &str = "adaptive_work_funding_expired_replacement_issued";
const MEMBER_DOMAIN: &str = "sentinel.workflow.expired-funding-replacement-members.v1";

struct RecoveryPublication {
    connection: usize,
    leaf: FundingReviewLeaf,
}

thread_local! {
    static RECOVERY_PUBLICATION: std::cell::RefCell<Option<RecoveryPublication>> =
        const { std::cell::RefCell::new(None) };
}

struct RecoveryPublicationGuard;

impl Drop for RecoveryPublicationGuard {
    fn drop(&mut self) {
        RECOVERY_PUBLICATION.with(|active| {
            active.borrow_mut().take();
        });
    }
}

fn is_exact_recovery_publication(connection: &Connection, leaf: &FundingReviewLeaf) -> bool {
    !connection.is_autocommit()
        && leaf.grant.schema_version == 6
        && RECOVERY_PUBLICATION.with(|active| {
            active.borrow().as_ref().is_some_and(|publication| {
                publication.connection == connection as *const Connection as usize
                    && publication.leaf == *leaf
            })
        })
}

pub(super) fn with_recovery_publication<T>(
    connection: &Connection,
    leaf: &FundingReviewLeaf,
    publish: impl FnOnce() -> Result<T, WorkflowError>,
) -> Result<T, WorkflowError> {
    leaf.validate_entity()?;
    if connection.is_autocommit() || leaf.grant.schema_version != 6 {
        return Err(corrupt());
    }
    RECOVERY_PUBLICATION.with(|active| {
        let mut active = active.borrow_mut();
        if active.is_some() {
            return Err(corrupt());
        }
        *active = Some(RecoveryPublication {
            connection: connection as *const Connection as usize,
            leaf: leaf.clone(),
        });
        Ok(())
    })?;
    let guard = RecoveryPublicationGuard;
    // An enclosing scope must not retain publication-bound proofs, even on
    // failure. Otherwise a fresh scope can share reads until the first write
    // and is finished or dropped before the temporary exception is cleared.
    validation_scope::before_write(connection)?;
    let result = validation_scope::with_scope(connection, publish);
    drop(guard);
    let result = result?;
    validation_scope::with_scope(connection, || {
        let call = get_entity::<SealedCall>(
            connection,
            &leaf.grant.leadership_principal.tenant_id,
            "adaptive_leadership_review_call",
            &leaf.grant.review_id.to_string(),
        )?
        .ok_or_else(corrupt)?
        .0;
        require_call_review_link(connection, &call)?;
        if call.grant != leaf.grant
            || call.operation_id != leaf.operation_id
            || call.context_digest()? != leaf.context_digest
            || call.grant_issued_at_unix_ms != leaf.issued_at_unix_ms
        {
            return Err(corrupt());
        }
        Ok(())
    })?;
    Ok(result)
}

// These projections validate sealed rows/events without following successor
// relations back into the relation currently being checked.
#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
struct SealedFunding(AdaptiveWorkFundingReceiptV1);

impl CompanyEntity for SealedFunding {
    fn load_shape(
        connection: &Connection,
        tenant: &TenantId,
        kind: &str,
        id: &str,
    ) -> Result<Option<Self>, WorkflowError> {
        super::super::entity_shape(connection, tenant, kind, id).map(|value| value.map(Self))
    }
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        self.0.row_binding()
    }
    fn validate_entity(&self) -> Result<(), WorkflowError> {
        self.0.validate()
    }
    fn validate_persisted(&self, connection: &Connection) -> Result<(), WorkflowError> {
        let receipt = &self.0;
        let source = &receipt.request.source.resume_source;
        let count: i64 = connection.query_row(
            "SELECT count(*) FROM (SELECT sequence
            FROM company_events WHERE tenant_id=?1 AND event_type=?2 AND (operation_id=?3
            OR json_extract(payload,'$.funding_id')=?4) LIMIT 2)",
            params![
                source.tenant_id.0,
                FUNDING_EVENT,
                receipt.request.operation_id.to_string(),
                receipt.funding_id
            ],
            |row| row.get(0),
        )?;
        if count != 1 {
            return Err(corrupt());
        }
        adaptive_resume_policy::require_leaf_event(
            connection,
            &source.tenant_id,
            &source.project_id,
            FUNDING_EVENT,
            receipt.request.operation_id,
            &receipt.request.canonical_digest()?,
            &receipt.issuer_principal,
            receipt.issued_at_unix_ms,
            receipt,
        )
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
struct SealedAdoption(FundingAdoptionLeaf);

impl CompanyEntity for SealedAdoption {
    fn load_shape(
        connection: &Connection,
        tenant: &TenantId,
        kind: &str,
        id: &str,
    ) -> Result<Option<Self>, WorkflowError> {
        super::super::entity_shape(connection, tenant, kind, id).map(|value| value.map(Self))
    }
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        self.0.row_binding()
    }
    fn validate_entity(&self) -> Result<(), WorkflowError> {
        self.0.validate_entity()
    }
    fn validate_persisted(&self, connection: &Connection) -> Result<(), WorkflowError> {
        let leaf = &self.0;
        let review = get_entity::<SealedReview>(
            connection,
            &leaf.authority.tenant_id,
            REVIEW_KIND,
            &leaf.review_membership_id,
        )?
        .ok_or_else(corrupt)?
        .0;
        require_adoption_review(leaf, &review)?;
        let epoch = leaf
            .authorization
            .work_funding
            .as_ref()
            .ok_or_else(corrupt)?;
        if sealed_funding(
            connection,
            &leaf.authority.tenant_id,
            &epoch.receipt.funding_id,
        )? != epoch.receipt
        {
            return Err(corrupt());
        }
        require_unique_membership_event(
            connection,
            &leaf.authority.tenant_id,
            ADOPTION_EVENT,
            leaf.authorization.operation_id,
            &leaf.membership_id,
        )?;
        adaptive_resume_policy::require_leaf_event(
            connection,
            &leaf.authority.tenant_id,
            &leaf.authority.project_id,
            ADOPTION_EVENT,
            leaf.authorization.operation_id,
            &canonical_sha256("sentinel.workflow.adaptive-work-funding-adoption.v1", leaf)?,
            &leaf.issuer_principal,
            leaf.authorization.issued_at_ms,
            leaf,
        )
    }
}

pub(super) fn sealed_adoption(
    connection: &Connection,
    tenant: &TenantId,
    id: &str,
) -> Result<FundingAdoptionLeaf, WorkflowError> {
    Ok(
        get_entity::<SealedAdoption>(connection, tenant, ADOPTION_KIND, id)?
            .ok_or_else(corrupt)?
            .0,
    )
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
struct SealedReview(FundingReviewLeaf);

impl CompanyEntity for SealedReview {
    fn load_shape(
        connection: &Connection,
        tenant: &TenantId,
        kind: &str,
        id: &str,
    ) -> Result<Option<Self>, WorkflowError> {
        super::super::entity_shape(connection, tenant, kind, id).map(|value| value.map(Self))
    }
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        self.0.row_binding()
    }
    fn validate_entity(&self) -> Result<(), WorkflowError> {
        self.0.validate_entity()
    }
    fn validate_persisted(&self, connection: &Connection) -> Result<(), WorkflowError> {
        let leaf = &self.0;
        let epoch = leaf.grant.work_funding.as_ref().ok_or_else(corrupt)?;
        if sealed_funding(
            connection,
            &leaf.grant.leadership_principal.tenant_id,
            &epoch.receipt.funding_id,
        )? != epoch.receipt
        {
            return Err(corrupt());
        }
        require_unique_membership_event(
            connection,
            &leaf.grant.leadership_principal.tenant_id,
            REVIEW_EVENT,
            leaf.operation_id,
            &leaf.membership_id,
        )?;
        adaptive_resume_policy::require_leaf_event(
            connection,
            &leaf.grant.leadership_principal.tenant_id,
            &leaf.grant.project_id,
            REVIEW_EVENT,
            leaf.operation_id,
            &canonical_sha256("sentinel.workflow.adaptive-work-funding-review.v1", leaf)?,
            &leaf.grant.leadership_principal,
            leaf.issued_at_unix_ms,
            leaf,
        )
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
struct SealedCall(AdaptiveLeadershipReviewCallV1);

impl CompanyEntity for SealedCall {
    fn load_shape(
        connection: &Connection,
        tenant: &TenantId,
        kind: &str,
        id: &str,
    ) -> Result<Option<Self>, WorkflowError> {
        super::super::entity_shape(connection, tenant, kind, id).map(|value| value.map(Self))
    }
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        self.0.row_binding()
    }
    fn validate_entity(&self) -> Result<(), WorkflowError> {
        self.0.validate_entity()
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
struct SealedRecovery(AdaptiveFundedReviewRecoveryReceiptV1);

impl CompanyEntity for SealedRecovery {
    fn load_shape(
        connection: &Connection,
        tenant: &TenantId,
        kind: &str,
        id: &str,
    ) -> Result<Option<Self>, WorkflowError> {
        super::super::entity_shape(connection, tenant, kind, id).map(|value| value.map(Self))
    }
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        self.0.row_binding()
    }
    fn validate_entity(&self) -> Result<(), WorkflowError> {
        self.0.validate_entity()
    }
    fn validate_persisted(&self, connection: &Connection) -> Result<(), WorkflowError> {
        let receipt = &self.0;
        let tenant = &receipt.issuer_principal.tenant_id;
        let source = &receipt.request.source;
        // Trace authentication plus typed sealed projections avoid traversing
        // unrelated future funding replacements through ordinary call validation.
        reviewed_funding_ids(connection, tenant)?;
        let epoch = receipt
            .grant_template
            .work_funding
            .as_ref()
            .ok_or_else(corrupt)?;
        super::super::adaptive_funded_review_recovery::require_recovery_journal_head(
            connection, receipt,
        )?;
        if sealed_funding(connection, tenant, &epoch.receipt.funding_id)? != epoch.receipt {
            return Err(corrupt());
        }
        for member in &receipt.review_members {
            let call = get_entity::<SealedCall>(
                connection,
                tenant,
                "adaptive_leadership_review_call",
                &member.review_id.to_string(),
            )?
            .ok_or_else(corrupt)?
            .0;
            if call.version != member.version
                || call.grant.session_id != source.session_id
                || canonical_sha256("sentinel.workflow.adaptive-leadership-call.v1", &call)?
                    != member.digest
            {
                return Err(corrupt());
            }
            if call
                .grant
                .work_funding
                .as_ref()
                .is_some_and(|prior| prior.same_epoch(epoch))
                && !recovery_prior_adopted_call(&receipt.context.source_session, &call)
            {
                require_retired(&call, receipt.issued_at_unix_ms)?;
                if call.grant.schema_version != 5 {
                    return Err(corrupt());
                }
            }
            if member.review_id == source.retired_review_id {
                require_retired(&call, receipt.issued_at_unix_ms)?;
                if call
                    .grant
                    .work_funding
                    .as_ref()
                    .is_none_or(|prior| !prior.same_epoch(epoch))
                    || call.grant.schema_version != 5
                    || call.context.source_project != receipt.context.source_project
                    || call.context.source_session != receipt.context.source_session
                {
                    return Err(corrupt());
                }
                if let AdaptiveFundedReviewNoIoV1::ReleasedBeforeIo {
                    released_at_unix_ms,
                    ..
                } = &receipt.evidence.no_io
                {
                    if *released_at_unix_ms < call.grant_issued_at_unix_ms
                        || *released_at_unix_ms > call.retired_at_unix_ms.ok_or_else(corrupt)?
                    {
                        return Err(corrupt());
                    }
                }
            }
        }
        let grant = receipt.bound_grant()?;
        let review = get_entity::<SealedReview>(
            connection,
            tenant,
            REVIEW_KIND,
            &review_id(grant.work_funding.as_ref().ok_or_else(corrupt)?),
        )?
        .ok_or_else(corrupt)?
        .0;
        if review.grant != grant
            || review.operation_id != receipt.request.operation_id
            || review.issued_at_unix_ms != receipt.issued_at_unix_ms
            || review.context_digest
                != canonical_sha256(
                    "sentinel.workflow.adaptive-leadership-context.v1",
                    &(&grant, &receipt.context),
                )?
        {
            return Err(corrupt());
        }
        let count: i64 = connection.query_row(
            "SELECT count(*) FROM (SELECT sequence FROM company_events
            WHERE tenant_id=?1 AND event_type='adaptive_funded_review_recovery_issued'
            AND (operation_id=?2 OR json_extract(payload,'$.receipt_key')=?3) LIMIT 2)",
            params![
                tenant.0,
                receipt.request.operation_id.to_string(),
                receipt.receipt_key
            ],
            |row| row.get(0),
        )?;
        if count != 1 {
            return Err(corrupt());
        }
        adaptive_resume_policy::require_leaf_event(
            connection,
            tenant,
            &source.project_id,
            "adaptive_funded_review_recovery_issued",
            receipt.request.operation_id,
            &receipt.receipt_digest()?,
            &receipt.issuer_principal,
            receipt.issued_at_unix_ms,
            receipt,
        )
    }
}

fn require_sealed_recovery(
    connection: &Connection,
    call: &AdaptiveLeadershipReviewCallV1,
) -> Result<(), WorkflowError> {
    let binding = call
        .grant
        .funded_review_recovery
        .as_ref()
        .ok_or_else(corrupt)?;
    binding.validate_for(&call.grant)?;
    let receipt = get_entity::<SealedRecovery>(
        connection,
        &call.grant.leadership_principal.tenant_id,
        "adaptive_funded_review_recovery",
        &binding.receipt_key,
    )?
    .ok_or_else(corrupt)?
    .0;
    if receipt.receipt_digest()? != binding.receipt_digest
        || receipt.bound_grant()? != call.grant
        || receipt.context != call.context
        || receipt.request.operation_id != call.operation_id
        || receipt.issued_at_unix_ms != call.grant_issued_at_unix_ms
    {
        return Err(corrupt());
    }
    Ok(())
}

fn require_sealed_recovery_review(
    connection: &Connection,
    leaf: &FundingReviewLeaf,
) -> Result<(), WorkflowError> {
    let binding = leaf
        .grant
        .funded_review_recovery
        .as_ref()
        .ok_or_else(corrupt)?;
    binding.validate_for(&leaf.grant)?;
    let receipt = get_entity::<SealedRecovery>(
        connection,
        &leaf.grant.leadership_principal.tenant_id,
        "adaptive_funded_review_recovery",
        &binding.receipt_key,
    )?
    .ok_or_else(corrupt)?
    .0;
    if receipt.receipt_digest()? != binding.receipt_digest
        || receipt.bound_grant()? != leaf.grant
        || receipt.request.operation_id != leaf.operation_id
        || receipt.issued_at_unix_ms != leaf.issued_at_unix_ms
        || canonical_sha256(
            "sentinel.workflow.adaptive-leadership-context.v1",
            &(&leaf.grant, &receipt.context),
        )? != leaf.context_digest
    {
        return Err(corrupt());
    }
    Ok(())
}

fn require_call_review_link(
    connection: &Connection,
    call: &AdaptiveLeadershipReviewCallV1,
) -> Result<(), WorkflowError> {
    let Some(epoch) = &call.grant.work_funding else {
        if matches!(call.grant.schema_version, 5 | 6) {
            return Err(corrupt());
        }
        return Ok(());
    };
    let leaf = get_entity::<SealedReview>(
        connection,
        &call.grant.leadership_principal.tenant_id,
        REVIEW_KIND,
        &review_id(epoch),
    )?
    .ok_or_else(corrupt)?
    .0;
    if leaf.grant != call.grant
        || leaf.operation_id != call.operation_id
        || leaf.context_digest != call.context_digest()?
        || leaf.issued_at_unix_ms != call.grant_issued_at_unix_ms
    {
        return Err(corrupt());
    }
    if call.grant.schema_version == 6 {
        require_sealed_recovery(connection, call)?;
    }
    Ok(())
}

pub(super) fn read_sealed_funding(
    connection: &Connection,
    tenant: &TenantId,
    id: &str,
) -> Result<Option<AdaptiveWorkFundingReceiptV1>, WorkflowError> {
    Ok(get_entity::<SealedFunding>(connection, tenant, FUNDING_KIND, id)?.map(|receipt| receipt.0))
}

pub(super) fn sealed_funding(
    connection: &Connection,
    tenant: &TenantId,
    id: &str,
) -> Result<AdaptiveWorkFundingReceiptV1, WorkflowError> {
    read_sealed_funding(connection, tenant, id)?.ok_or_else(corrupt)
}

pub(super) fn sealed_receipts(
    connection: &Connection,
    tenant: &TenantId,
    session_id: Uuid,
) -> Result<Vec<AdaptiveWorkFundingReceiptV1>, WorkflowError> {
    validation_scope::with_scope(connection, || {
        let ids = funding_inventory(connection, tenant)?;
        let mut receipts = Vec::new();
        for id in ids {
            let receipt = sealed_funding(connection, tenant, &id)?;
            if receipt.request.source.resume_source.session_id == session_id {
                receipts.push(receipt);
            }
        }
        if receipts.len() > 128 {
            return Err(corrupt());
        }
        Ok(receipts)
    })
}

fn members(
    calls: &[AdaptiveLeadershipReviewCallV1],
    digest: &str,
) -> Result<Vec<AdaptiveFundedReviewRecoveryMemberV1>, WorkflowError> {
    let mut result = Vec::new();
    for call in calls {
        if call
            .grant
            .work_funding
            .as_ref()
            .map(|epoch| epoch.receipt.receipt_digest())
            .transpose()?
            .as_deref()
            == Some(digest)
        {
            result.push(AdaptiveFundedReviewRecoveryMemberV1 {
                review_id: call.grant.review_id,
                version: call.version,
                digest: canonical_sha256("sentinel.workflow.adaptive-leadership-call.v1", call)?,
            });
        }
    }
    result.sort_by_key(|member| member.review_id);
    if result.is_empty() || result.len() > 128 {
        return Err(transition());
    }
    Ok(result)
}

fn require_no_effects(call: &AdaptiveLeadershipReviewCallV1) -> Result<(), WorkflowError> {
    if !matches!(call.grant.schema_version, 5 | 6)
        || call.dispatch.is_some()
        || call.decision.is_some()
        || call.model_response_digest.is_some()
        || call.continuation.is_some()
        || call.resolution_event_id.is_some()
    {
        return Err(transition());
    }
    Ok(())
}

fn require_retired(call: &AdaptiveLeadershipReviewCallV1, time: u64) -> Result<(), WorkflowError> {
    require_no_effects(call)?;
    if call
        .retired_at_unix_ms
        .is_none_or(|at| at < call.grant.expires_at_unix_ms || at > time)
        || call.updated_at_unix_ms > time
    {
        return Err(transition());
    }
    Ok(())
}

fn require_history(
    connection: &Connection,
    old: &AdaptiveWorkFundingReceiptV1,
) -> Result<(), WorkflowError> {
    let tenant = &old.request.source.resume_source.tenant_id;
    // This authenticates every review row/event before any funding filter.
    reviewed_funding_ids(connection, tenant)?;
    let mut review_rows = connection.prepare(
        "SELECT entity_id FROM company_entities WHERE tenant_id=?1
        AND entity_kind=?2 ORDER BY entity_id LIMIT 4097",
    )?;
    let review_ids = review_rows
        .query_map(params![tenant.0, REVIEW_KIND], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if review_ids.len() > 4096 {
        return Err(corrupt());
    }
    for id in review_ids {
        let leaf = get_entity::<SealedReview>(connection, tenant, REVIEW_KIND, &id)?
            .ok_or_else(corrupt)?
            .0;
        if leaf.grant.schema_version == 6 {
            require_sealed_recovery_review(connection, &leaf)?;
        }
        let call = get_entity::<SealedCall>(
            connection,
            tenant,
            "adaptive_leadership_review_call",
            &leaf.grant.review_id.to_string(),
        )?;
        // Only the exact call currently being published on this connection may
        // be absent. Persisted unrelated leaves never receive this exception.
        let Some(SealedCall(call)) = call else {
            if is_exact_recovery_publication(connection, &leaf)
                && leaf
                    .grant
                    .work_funding
                    .as_ref()
                    .is_some_and(|epoch| epoch.receipt != *old)
            {
                continue;
            }
            return Err(corrupt());
        };
        if call.grant != leaf.grant
            || call.context_digest()? != leaf.context_digest
            || call.operation_id != leaf.operation_id
            || call.grant_issued_at_unix_ms != leaf.issued_at_unix_ms
        {
            return Err(corrupt());
        }
        if call.grant.schema_version == 6 {
            require_sealed_recovery(connection, &call)?;
        }
    }
    let mut statement = connection.prepare(
        "SELECT entity_id FROM company_entities
        WHERE tenant_id=?1 AND entity_kind=?2 ORDER BY entity_id LIMIT 4097",
    )?;
    let ids = statement
        .query_map(params![tenant.0, ADOPTION_KIND], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if ids.len() > 4096 {
        return Err(corrupt());
    }
    for id in ids {
        let leaf = get_entity::<SealedAdoption>(connection, tenant, ADOPTION_KIND, &id)?
            .ok_or_else(corrupt)?
            .0;
        if leaf
            .authorization
            .work_funding
            .as_ref()
            .is_some_and(|epoch| epoch.receipt == *old)
        {
            return Err(transition());
        }
    }
    let mut events = connection.prepare(
        "SELECT sequence FROM company_events WHERE tenant_id=?1
        AND (event_type GLOB 'adaptive_leadership_review_*' OR event_type=?2)
        ORDER BY sequence LIMIT 4097",
    )?;
    let sequences = events
        .query_map(params![tenant.0, ADOPTION_EVENT], |row| {
            row.get::<_, i64>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if sequences.len() > 4096 {
        return Err(corrupt());
    }
    for sequence in sequences {
        let row = super::super::company_event_shape(connection, stored_u64(sequence)?)?
            .ok_or_else(corrupt)?;
        if row.event_type == ADOPTION_EVENT {
            let event: FundingAdoptionLeaf = decode(&row.payload)?;
            let stored = get_entity::<SealedAdoption>(
                connection,
                tenant,
                ADOPTION_KIND,
                &event.membership_id,
            )?
            .ok_or_else(corrupt)?
            .0;
            if stored != event {
                return Err(corrupt());
            }
            if event
                .authorization
                .work_funding
                .as_ref()
                .is_some_and(|epoch| epoch.receipt == *old)
            {
                return Err(transition());
            }
        } else {
            let call: AdaptiveLeadershipReviewCallV1 = decode(&row.payload)?;
            if call
                .grant
                .work_funding
                .as_ref()
                .is_some_and(|epoch| epoch.receipt == *old)
            {
                require_no_effects(&call)?;
            }
        }
    }
    Ok(())
}

fn require_member(
    connection: &Connection,
    old: &AdaptiveWorkFundingReceiptV1,
    member: &AdaptiveFundedReviewRecoveryMemberV1,
    time: u64,
) -> Result<AdaptiveLeadershipReviewCallV1, WorkflowError> {
    let tenant = &old.request.source.resume_source.tenant_id;
    let call = get_entity::<SealedCall>(
        connection,
        tenant,
        "adaptive_leadership_review_call",
        &member.review_id.to_string(),
    )?
    .ok_or_else(corrupt)?
    .0;
    if call.version != member.version
        || canonical_sha256("sentinel.workflow.adaptive-leadership-call.v1", &call)?
            != member.digest
        || call
            .grant
            .work_funding
            .as_ref()
            .is_none_or(|epoch| epoch.receipt != *old)
    {
        return Err(corrupt());
    }
    require_retired(&call, time)?;
    require_call_review_link(connection, &call)?;
    crate::store::adaptive::require_funding_journal_source(
        connection,
        call.grant.work_funding.as_ref().ok_or_else(corrupt)?,
        &call.context.source_session,
    )?;
    Ok(call)
}

impl CompanyEntity for AdaptiveWorkFundingExpiredReplacementReceiptV1 {
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        (
            &self.issuer_principal.tenant_id,
            KIND,
            &self.replacement_key,
            1,
        )
    }
    fn validate_entity(&self) -> Result<(), WorkflowError> {
        self.request
            .validate_at(&self.issuer_principal, self.issued_at_unix_ms)?;
        validate_digest(&self.new_funding_receipt_digest)?;
        validate_identifier(&self.new_funding_id)?;
        if self.schema_version != 1
            || self.replacement_key != self.request.source.replacement_key()?
            || self.evidence.source_digest != self.request.source_digest
            || self.evidence.proof_digest != self.request.proof_digest
            || self.evidence.verified_at_unix_ms != self.issued_at_unix_ms
            || self.review_members.len() != usize::from(self.request.source.prior_review_count)
            || self.review_members.is_empty()
            || self.review_members.len() > 128
            || canonical_sha256(MEMBER_DOMAIN, &self.review_members)?
                != self.request.source.review_members_digest
            || self.evidence.per_review.len() != self.review_members.len()
        {
            return Err(corrupt());
        }
        for (index, member) in self.review_members.iter().enumerate() {
            validate_digest(&member.digest)?;
            if member.review_id.is_nil()
                || member.version == 0
                || index > 0 && self.review_members[index - 1].review_id >= member.review_id
                || self.evidence.per_review[index].member != *member
            {
                return Err(corrupt());
            }
            self.evidence.per_review[index].release.validate()?;
            validate_digest(&self.evidence.per_review[index].proof_digest)?;
        }
        Ok(())
    }
    fn validate_persisted(&self, connection: &Connection) -> Result<(), WorkflowError> {
        let source = &self.request.source;
        let new = sealed_funding(connection, &source.tenant_id, &self.new_funding_id)?;
        let old = sealed_funding(connection, &source.tenant_id, &source.old_funding_id)?;
        if new.request != self.request.funding_request
            || new.issuer_principal != self.issuer_principal
            || new.issued_at_unix_ms != self.issued_at_unix_ms
            || new.receipt_digest()? != self.new_funding_receipt_digest
            || old.receipt_digest()? != source.old_funding_receipt_digest
            || old.request.source.resume_source.session_id != source.session_id
            || old.request.source.resume_source.project_id != source.project_id
            || old.issued_at_unix_ms >= self.issued_at_unix_ms
            || old.request.limits.expires_at_unix_ms > self.issued_at_unix_ms
            || old.request.source.resume_source.assignee_authority
                != source.current_resume_source.assignee_authority
            || old.request.source.resume_source
                != fresh_with_old_review_count(&source.current_resume_source, &old)
        {
            return Err(corrupt());
        }
        // Strict decreasing issuance clocks bound explicit multi-link validation.
        require_history(connection, &old)?;
        for (member, evidence) in self.review_members.iter().zip(&self.evidence.per_review) {
            let call = require_member(connection, &old, member, self.issued_at_unix_ms)?;
            let session = &call.context.source_session;
            let fresh = &source.current_resume_source;
            let funding_source = &self.request.funding_request.source;
            if session.version != fresh.expected_session_version
                || session.model_calls != fresh.base_model_calls
                || session.tool_calls != fresh.base_tool_calls
                || funding_source.original_model_call_ceiling != session.grant.max_model_calls
                || funding_source.original_tool_call_ceiling != session.grant.max_tool_calls
                || funding_source.current_model_call_ceiling != session.funded_model_call_ceiling()
                || funding_source.current_tool_call_ceiling != session.funded_tool_call_ceiling()
                || funding_source.predecessor_receipt_digest
                    != session
                        .active_work_funding()
                        .map(|epoch| epoch.receipt.receipt_digest())
                        .transpose()?
                || call.context.source_project.version != fresh.expected_project_version
                || bytes_digest(
                    "sentinel.workflow.company-entity-row.v1",
                    &encode(&call.context.source_project)?,
                )? != fresh.project_payload_digest
            {
                return Err(corrupt());
            }
            let id = call.request_id();
            match &evidence.no_io {
                AdaptiveFundedReviewNoIoV1::NeverReserved { request_id }
                | AdaptiveFundedReviewNoIoV1::UndispatchedAtDurableProviderGate { request_id } => {
                    if *request_id != id {
                        return Err(corrupt());
                    }
                }
                AdaptiveFundedReviewNoIoV1::ReleasedBeforeIo {
                    request_id,
                    request_digest,
                    release_digest,
                    released_at_unix_ms,
                } => {
                    validate_digest(request_digest)?;
                    validate_digest(release_digest)?;
                    if *request_id != id
                        || *released_at_unix_ms < call.created_at_unix_ms
                        || *released_at_unix_ms > self.issued_at_unix_ms
                    {
                        return Err(corrupt());
                    }
                }
            }
        }
        // Row/event completeness is independent of present selector eligibility.
        let mut statement = connection.prepare("SELECT entity_id FROM company_entities
            WHERE tenant_id=?1 AND entity_kind='adaptive_leadership_review_call' ORDER BY entity_id LIMIT 4097")?;
        let ids = statement
            .query_map([&source.tenant_id.0], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if ids.len() > 4096 {
            return Err(corrupt());
        }
        let mut matched = BTreeSet::new();
        for id in ids {
            // Crosscheck every funded call through sealed leaves, never through
            // future successor relations, before selecting target old members.
            let call = get_entity::<SealedCall>(
                connection,
                &source.tenant_id,
                "adaptive_leadership_review_call",
                &id,
            )?
            .ok_or_else(corrupt)?
            .0;
            require_call_review_link(connection, &call)?;
            if call
                .grant
                .work_funding
                .as_ref()
                .is_some_and(|epoch| epoch.receipt == old)
            {
                matched.insert(call.grant.review_id);
            }
        }
        if matched
            != self
                .review_members
                .iter()
                .map(|member| member.review_id)
                .collect()
        {
            return Err(corrupt());
        }
        require_unique_relation_event(connection, self)?;
        let issuance_sequence: i64 = connection.query_row(
            "SELECT sequence FROM company_events
            WHERE tenant_id=?1 AND event_type=?2 AND operation_id=?3",
            params![
                source.tenant_id.0,
                EVENT,
                self.request.funding_request.operation_id.to_string()
            ],
            |row| row.get(0),
        )?;
        let (count, digest) = historical_review_cut(
            connection,
            &source.tenant_id,
            source.session_id,
            issuance_sequence,
        )?;
        if count != usize::from(source.current_resume_source.base_review_count)
            || digest != source.current_resume_source.review_history_digest
        {
            return Err(corrupt());
        }
        adaptive_resume_policy::require_leaf_event(
            connection,
            &source.tenant_id,
            &source.project_id,
            EVENT,
            self.request.funding_request.operation_id,
            &self.request.canonical_digest()?,
            &self.issuer_principal,
            self.issued_at_unix_ms,
            self,
        )
    }
}

#[derive(Clone, Serialize)]
struct HistoricalReviewCut {
    session_id: Uuid,
    count: usize,
    digest: String,
}

// Discovery supplies cut positions, not authority. Each replacement still
// validates its sealed relation, complete membership and exact historical digest.
pub(super) fn historical_review_cut(
    connection: &Connection,
    tenant: &TenantId,
    session_id: Uuid,
    sequence: i64,
) -> Result<(usize, String), WorkflowError> {
    let cuts = validation_scope::memoize(connection, "replacement-review-history", tenant, || {
        let mut statement = connection.prepare(
            "SELECT sequence,json_extract(payload,'$.request.source.session_id') FROM company_events
             WHERE tenant_id=?1 AND event_type=?2 ORDER BY sequence LIMIT 4097",
        )?;
        let mut rows = statement.query(params![tenant.0, EVENT])?;
        let mut cuts = Vec::new();
        while let Some(row) = rows.next()? {
            if cuts.len() >= 4096 {
                return Err(corrupt());
            }
            let sequence: i64 = row.get(0)?;
            let rusqlite::types::ValueRef::Text(session) = row.get_ref(1)? else {
                return Err(corrupt());
            };
            if sequence <= 0 || session.len() != 36 {
                return Err(corrupt());
            }
            let session = std::str::from_utf8(session).map_err(|_| corrupt())?;
            cuts.push((sequence, session.to_owned()));
        }
        let Some(last) = cuts.last().map(|(sequence, _)| *sequence) else {
            return Ok(BTreeMap::new());
        };
        let mut cuts = cuts.into_iter().peekable();
        let mut history = BTreeMap::<Uuid, BTreeMap<Uuid, AdaptiveLeadershipReviewCallV1>>::new();
        let mut result = BTreeMap::new();
        let mut record_cut =
            |sequence,
             session: String,
             history: &BTreeMap<Uuid, BTreeMap<Uuid, AdaptiveLeadershipReviewCallV1>>| {
                let session_id = Uuid::parse_str(&session).map_err(|_| corrupt())?;
                if session_id.is_nil() || session_id.to_string() != session {
                    return Err(corrupt());
                }
                let calls = history
                    .get(&session_id)
                    .map(|calls| calls.values().collect::<Vec<_>>())
                    .unwrap_or_default();
                let digest = canonical_sha256(
                    "sentinel.workflow.adaptive-resume-review-history.v1",
                    &calls,
                )?;
                result.insert(
                    sequence,
                    HistoricalReviewCut {
                        session_id,
                        count: calls.len(),
                        digest,
                    },
                );
                Ok::<_, WorkflowError>(())
            };
        let mut statement = connection.prepare(
            "SELECT sequence FROM company_events WHERE tenant_id=?1
             AND event_type GLOB 'adaptive_leadership_review_*' AND sequence<?2 ORDER BY sequence LIMIT 4097",
        )?;
        let mut rows = statement.query(params![tenant.0, last])?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            count += 1;
            if count > 4096 {
                return Err(corrupt());
            }
            let sequence: i64 = row.get(0)?;
            while cuts.peek().is_some_and(|(cut, _)| *cut <= sequence) {
                let (cut, session) = cuts.next().ok_or_else(corrupt)?;
                record_cut(cut, session, &history)?;
            }
            // The shared owner charges both raw source bytes and retained data.
            let event = super::super::company_event_shape(connection, stored_u64(sequence)?)?
                .ok_or_else(corrupt)?;
            // Decode every tenant row before selecting a session, as before.
            let call: AdaptiveLeadershipReviewCallV1 = decode(&event.payload)?;
            history
                .entry(call.grant.session_id)
                .or_default()
                .insert(call.grant.review_id, call);
        }
        for (cut, session) in cuts {
            record_cut(cut, session, &history)?;
        }
        Ok(result)
    })?;
    let cut = cuts.get(&sequence).ok_or_else(corrupt)?;
    if cut.session_id != session_id {
        return Err(corrupt());
    }
    Ok((cut.count, cut.digest.clone()))
}

fn require_unique_relation_event(
    connection: &Connection,
    receipt: &AdaptiveWorkFundingExpiredReplacementReceiptV1,
) -> Result<(), WorkflowError> {
    let count: i64 = connection.query_row(
        "SELECT count(*) FROM (SELECT sequence FROM company_events
        WHERE tenant_id=?1 AND event_type=?2 AND (operation_id=?3
        OR json_extract(payload,'$.replacement_key')=?4) LIMIT 2)",
        params![
            receipt.issuer_principal.tenant_id.0,
            EVENT,
            receipt.request.funding_request.operation_id.to_string(),
            receipt.replacement_key
        ],
        |row| row.get(0),
    )?;
    if count != 1 {
        return Err(corrupt());
    }
    Ok(())
}

fn lookup(
    connection: &Connection,
    tenant: &TenantId,
    key: &str,
) -> Result<Option<AdaptiveWorkFundingExpiredReplacementReceiptV1>, WorkflowError> {
    let value = get_entity(connection, tenant, KIND, key)?;
    if value.is_none() {
        let orphan: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM company_events
            WHERE tenant_id=?1 AND event_type=?2 AND json_extract(payload,'$.replacement_key')=?3)",
            params![tenant.0, EVENT, key],
            |row| row.get(0),
        )?;
        if orphan {
            return Err(corrupt());
        }
    }
    Ok(value)
}

pub(super) fn require_successor_membership(
    connection: &Connection,
    receipt: &AdaptiveWorkFundingReceiptV1,
) -> Result<(), WorkflowError> {
    if let Some(binding) = &receipt.request.source.expired_reviewed_replacement {
        let source = &receipt.request.source.resume_source;
        let relation =
            lookup(connection, &source.tenant_id, &binding.replacement_key)?.ok_or_else(corrupt)?;
        if relation.new_funding_receipt_digest != receipt.receipt_digest()?
            || relation.new_funding_id != receipt.funding_id
        {
            return Err(corrupt());
        }
    }
    Ok(())
}

pub(super) fn consumed_receipts(
    connection: &Connection,
    tenant: &TenantId,
    session_id: Uuid,
) -> Result<BTreeSet<String>, WorkflowError> {
    validation_scope::memoize(
        connection,
        "expired-funding-replacement-inventory",
        &(tenant, session_id),
        || {
            let mut statement = connection.prepare(
                "SELECT entity_id FROM company_entities
            WHERE tenant_id=?1 AND entity_kind=?2 ORDER BY entity_id LIMIT 4097",
            )?;
            let ids = statement
                .query_map(params![tenant.0, KIND], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            if ids.len() > 4096 {
                return Err(corrupt());
            }
            let mut consumed = BTreeSet::new();
            for id in &ids {
                let relation = lookup(connection, tenant, id)?.ok_or_else(corrupt)?;
                if relation.request.source.session_id == session_id
                    && !consumed.insert(relation.request.source.old_funding_id)
                {
                    return Err(corrupt());
                }
            }
            let mut events = connection.prepare(
                "SELECT sequence FROM company_events WHERE tenant_id=?1
            AND event_type=?2 ORDER BY sequence LIMIT 4097",
            )?;
            let sequences = events
                .query_map(params![tenant.0, EVENT], |row| row.get::<_, i64>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            if sequences.len() > 4096 {
                return Err(corrupt());
            }
            for sequence in sequences {
                let row = read_company_event_row(connection, stored_u64(sequence)?)?
                    .ok_or_else(corrupt)?;
                validation_scope::charge_bytes(connection, row.payload.len())?;
                let event: AdaptiveWorkFundingExpiredReplacementReceiptV1 = decode(&row.payload)?;
                if lookup(connection, tenant, &event.replacement_key)?.as_ref() != Some(&event) {
                    return Err(corrupt());
                }
            }
            Ok(consumed)
        },
    )
}

fn fresh_source(
    connection: &Connection,
    operator: &AuthenticatedCompanyPrincipalV1,
    project_id: &ProjectId,
    session_id: Uuid,
    old_digest: &str,
    now_ms: u64,
) -> Result<AdaptiveWorkFundingExpiredReplacementVerificationSourceV1, WorkflowError> {
    crate::adaptive_resume_policy::require_resume_policy_operator(operator, &operator.tenant_id)?;
    validate_digest(old_digest)?;
    let receipts = funding_receipts(connection, &operator.tenant_id, session_id)?;
    let mut old = None;
    for receipt in &receipts {
        if receipt.receipt_digest()? == old_digest {
            old = Some(receipt.clone());
        }
    }
    let old = old.ok_or_else(not_found)?;
    let (fresh, session) = adaptive_resume_policy::fresh_resume_source(
        connection,
        &operator.tenant_id,
        project_id,
        session_id,
        now_ms,
        None,
    )?;
    let project: ProjectV1 = get_entity(connection, &operator.tenant_id, "project", &project_id.0)?
        .ok_or_else(not_found)?;
    let calls =
        adaptive_leadership_review::calls_for_session(connection, &operator.tenant_id, session_id)?;
    let target = members(&calls, old_digest)?;
    let consumed =
        consumed_validated_receipts(connection, &operator.tenant_id, session_id, &receipts)?;
    let adopted = adopted_epochs(&session);
    if old.request.limits.expires_at_unix_ms > now_ms
        || consumed.contains(&old.funding_id)
        || adopted.iter().any(|epoch| epoch.receipt == old)
        || receipts
            .iter()
            .filter(|receipt| {
                !adopted.iter().any(|epoch| epoch.receipt == **receipt)
                    && !consumed.contains(&receipt.funding_id)
            })
            .count()
            != 1
        || old.request.source.resume_source != fresh_with_old_review_count(&fresh, &old)
    {
        return Err(transition());
    }
    require_history(connection, &old)?;
    for member in &target {
        let call = require_member(connection, &old, member, now_ms)?;
        if call.context.source_project != project || call.context.source_session != session {
            return Err(transition());
        }
    }
    let source = AdaptiveWorkFundingExpiredReplacementSourceV1 {
        schema_version: 1,
        tenant_id: operator.tenant_id.clone(),
        project_id: project_id.clone(),
        session_id,
        old_funding_id: old.funding_id.clone(),
        old_funding_receipt_digest: old_digest.into(),
        current_resume_source: fresh,
        review_members_digest: canonical_sha256(MEMBER_DOMAIN, &target)?,
        prior_review_count: u16::try_from(target.len()).map_err(|_| corrupt())?,
    };
    source.validate()?;
    Ok(AdaptiveWorkFundingExpiredReplacementVerificationSourceV1 {
        source,
        project: Box::new(project),
        session: Box::new(session),
        old_funding: Box::new(old),
        calls,
        review_members: target,
    })
}

// Issuing/retiring reviews changes review history, never the execution head.
fn fresh_with_old_review_count(
    fresh: &AdaptiveResumeSourceV1,
    old: &AdaptiveWorkFundingReceiptV1,
) -> AdaptiveResumeSourceV1 {
    let mut value = fresh.clone();
    value.base_review_count = old.request.source.resume_source.base_review_count;
    value.review_history_digest = old
        .request
        .source
        .resume_source
        .review_history_digest
        .clone();
    value
}

impl WorkflowStore {
    pub fn adaptive_work_funding_expired_replacement_source(
        &self,
        operator: &AuthenticatedCompanyPrincipalV1,
        project_id: &ProjectId,
        session_id: Uuid,
        old_receipt_digest: &str,
        now_ms: u64,
    ) -> Result<AdaptiveWorkFundingExpiredReplacementSourceV1, WorkflowError> {
        crate::adaptive_resume_policy::require_resume_policy_operator(
            operator,
            &operator.tenant_id,
        )?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let scope = validation_scope::enter(&transaction)?;
        let key = adaptive_work_funding_expired_replacement_key(
            &operator.tenant_id,
            session_id,
            old_receipt_digest,
        )?;
        let source = if let Some(prior) = lookup(&transaction, &operator.tenant_id, &key)? {
            if prior.request.source.project_id != *project_id {
                return Err(unauthorized());
            }
            prior.request.source
        } else {
            fresh_source(
                &transaction,
                operator,
                project_id,
                session_id,
                old_receipt_digest,
                now_ms,
            )?
            .source
        };
        scope.finish()?;
        Ok(source)
    }

    pub fn adaptive_work_funding_expired_replacement_verification_source(
        &self,
        operator: &AuthenticatedCompanyPrincipalV1,
        project_id: &ProjectId,
        session_id: Uuid,
        old_receipt_digest: &str,
        now_ms: u64,
    ) -> Result<AdaptiveWorkFundingExpiredReplacementVerificationSourceV1, WorkflowError> {
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let scope = validation_scope::enter(&transaction)?;
        let source = fresh_source(
            &transaction,
            operator,
            project_id,
            session_id,
            old_receipt_digest,
            now_ms,
        )?;
        scope.finish()?;
        Ok(source)
    }

    pub fn adaptive_work_funding_expired_replacement_draft(
        &self,
        operator: &AuthenticatedCompanyPrincipalV1,
        project_id: &ProjectId,
        session_id: Uuid,
        draft: &AdaptiveWorkFundingExpiredReplacementDraftV1,
        now_ms: u64,
    ) -> Result<AdaptiveWorkFundingExpiredReplacementRequestV1, WorkflowError> {
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let scope = validation_scope::enter(&transaction)?;
        let source = fresh_source(
            &transaction,
            operator,
            project_id,
            session_id,
            &draft.old_funding_receipt_digest,
            now_ms,
        )?
        .source;
        let (mut funding_source, duration) = require_unfunded_source_with_candidate(
            &transaction,
            &operator.tenant_id,
            project_id,
            session_id,
            now_ms,
            Some(PendingFundingReplacement::Reviewed(
                &draft.old_funding_receipt_digest,
            )),
            &draft.limits,
        )?;
        funding_source.expired_reviewed_replacement = Some(source.binding()?);
        let request = AdaptiveWorkFundingExpiredReplacementRequestV1 {
            schema_version: 1,
            source_digest: source.canonical_digest()?,
            source,
            funding_request: AdaptiveWorkFundingRequestV1 {
                schema_version: 1,
                source: funding_source,
                operation_id: draft.operation_id,
                reason_ref: draft.reason_ref.clone(),
                limits: draft.limits.clone(),
            },
            release: draft.release.clone(),
            proof_digest: draft.proof_digest.clone(),
        };
        request.validate_at(operator, now_ms)?;
        if request.funding_request.limits.max_call_duration_ms != duration {
            return Err(transition());
        }
        scope.finish()?;
        Ok(request)
    }

    /// The caller holds admission/mutation exclusion; verify must not reenter the store.
    pub fn authorize_adaptive_work_funding_expired_replacement<F>(
        &self,
        operator: &AuthenticatedCompanyPrincipalV1,
        request: &AdaptiveWorkFundingExpiredReplacementRequestV1,
        now_ms: u64,
        verify: F,
    ) -> Result<(bool, AdaptiveWorkFundingExpiredReplacementReceiptV1), WorkflowError>
    where
        F: FnOnce(
            &AdaptiveWorkFundingExpiredReplacementVerificationSourceV1,
            &AdaptiveWorkFundingExpiredReplacementRequestV1,
        )
            -> Result<AdaptiveWorkFundingExpiredReplacementEvidenceV1, WorkflowError>,
    {
        crate::adaptive_resume_policy::require_resume_policy_operator(
            operator,
            &request.source.tenant_id,
        )?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let scope = validation_scope::enter(&transaction)?;
        let key = request.source.replacement_key()?;
        if let Some(prior) = lookup(&transaction, &request.source.tenant_id, &key)? {
            if prior.request != *request || prior.issuer_principal != *operator {
                return Err(funding_conflict());
            }
            scope.finish()?;
            return Ok((true, prior));
        }
        request.validate_at(operator, now_ms)?;
        let source = fresh_source(
            &transaction,
            operator,
            &request.source.project_id,
            request.source.session_id,
            &request.source.old_funding_receipt_digest,
            now_ms,
        )?;
        let (mut fresh, duration) = require_unfunded_source_with_candidate(
            &transaction,
            &request.source.tenant_id,
            &request.source.project_id,
            request.source.session_id,
            now_ms,
            Some(PendingFundingReplacement::Reviewed(
                &request.source.old_funding_receipt_digest,
            )),
            &request.funding_request.limits,
        )?;
        fresh.expired_reviewed_replacement = Some(source.source.binding()?);
        if request.source != source.source
            || request.funding_request.source != fresh
            || request.funding_request.limits.max_call_duration_ms != duration
        {
            return Err(transition());
        }
        let evidence = verify(&source, request)?;
        if evidence.verified_at_unix_ms < now_ms {
            return Err(transition());
        }
        request.validate_at(operator, evidence.verified_at_unix_ms)?;
        let issued = evidence.verified_at_unix_ms;
        let funding = AdaptiveWorkFundingReceiptV1 {
            schema_version: 1,
            funding_id: adaptive_work_funding_id(
                &request.source.tenant_id,
                request.source.session_id,
                request.funding_request.operation_id,
            )?,
            request: request.funding_request.clone(),
            issuer_principal: operator.clone(),
            issued_at_unix_ms: issued,
        };
        if read_funding_leaf(
            &transaction,
            &request.source.tenant_id,
            request.source.session_id,
            request.funding_request.operation_id,
        )?
        .is_some()
        {
            return Err(funding_conflict());
        }
        let receipt = AdaptiveWorkFundingExpiredReplacementReceiptV1 {
            schema_version: 1,
            replacement_key: key,
            request: request.clone(),
            issuer_principal: operator.clone(),
            review_members: source.review_members,
            evidence,
            new_funding_id: funding.funding_id.clone(),
            new_funding_receipt_digest: funding.receipt_digest()?,
            issued_at_unix_ms: issued,
        };
        receipt.validate_entity()?;
        scope.finish()?;
        put_funding_receipt(&transaction, &funding)?;
        put_entity(
            &transaction,
            &request.source.tenant_id,
            KIND,
            &receipt.replacement_key,
            1,
            &receipt,
        )?;
        append_event(
            &transaction,
            operator,
            request.funding_request.operation_id,
            &request.canonical_digest()?,
            Some(&request.source.project_id),
            EVENT,
            &receipt,
            issued,
        )?;
        // Validate exact NoIO IDs and both sealed links before commit, after
        // writes invalidated all earlier scoped inventory proofs.
        validation_scope::with_scope(&transaction, || receipt.validate_persisted(&transaction))?;
        transaction.commit()?;
        Ok((false, receipt))
    }

    pub fn adaptive_work_funding_expired_replacement(
        &self,
        tenant: &TenantId,
        replacement_key: &str,
    ) -> Result<Option<AdaptiveWorkFundingExpiredReplacementReceiptV1>, WorkflowError> {
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let scope = validation_scope::enter(&transaction)?;
        let receipt = lookup(&transaction, tenant, replacement_key)?;
        scope.finish()?;
        Ok(receipt)
    }
}
