//! Immutable funding proposals. Issuance deliberately changes no execution state.

use super::*;
use crate::adaptive_work_funding::*;
use crate::{
    AdaptiveContinuationAuthorizationV1, AdaptiveLeadershipReviewCallV1,
    AdaptiveLeadershipReviewContextV1, AdaptiveLeadershipReviewGrantV1, RuntimeAuthoritySnapshotV1,
};

mod expired_replacement;
mod standing_policy;

struct HistoricalRecoveryJournal {
    connection: usize,
    source: crate::AdaptiveSessionV1,
    binding: String,
}

thread_local! {
    static HISTORICAL_RECOVERY_JOURNAL: std::cell::RefCell<Option<HistoricalRecoveryJournal>> =
        const { std::cell::RefCell::new(None) };
}

struct HistoricalRecoveryJournalGuard;

impl Drop for HistoricalRecoveryJournalGuard {
    fn drop(&mut self) {
        HISTORICAL_RECOVERY_JOURNAL.with(|active| {
            active.borrow_mut().take();
        });
    }
}

// Historical step proofs must never be reused as ordinary full-membership
// proofs, or collide with an ordinary step currently validating its successor.
pub(crate) fn historical_journal_proof_binding(
    connection: &Connection,
    session_id: Uuid,
) -> Option<String> {
    HISTORICAL_RECOVERY_JOURNAL.with(|active| {
        active.borrow().as_ref().and_then(|proof| {
            (proof.connection == connection as *const Connection as usize
                && proof.source.grant.session_id == session_id)
                .then(|| proof.binding.clone())
        })
    })
}

pub(super) fn require_recovery_journal_source(
    connection: &Connection,
    receipt: &crate::AdaptiveFundedReviewRecoveryReceiptV1,
) -> Result<(), WorkflowError> {
    require_recovery_journal_source_with_observer(connection, receipt, || Ok(()))
}

#[cfg(test)]
pub(super) fn observe_recovery_journal_source(
    connection: &Connection,
    receipt: &crate::AdaptiveFundedReviewRecoveryReceiptV1,
    after_replay: impl FnOnce() -> Result<(), WorkflowError>,
) -> Result<(), WorkflowError> {
    require_recovery_journal_source_with_observer(connection, receipt, after_replay)
}

fn require_recovery_journal_source_with_observer(
    connection: &Connection,
    receipt: &crate::AdaptiveFundedReviewRecoveryReceiptV1,
    after_replay: impl FnOnce() -> Result<(), WorkflowError>,
) -> Result<(), WorkflowError> {
    validation_scope::with_scope(connection, || {
        let source = &receipt.context.source_session;
        let epoch = receipt
            .grant_template
            .work_funding
            .as_ref()
            .ok_or_else(corrupt)?;
        if !source.continuation.as_ref().is_some_and(|state| {
            state
                .authorizations
                .iter()
                .any(|authorization| authorization.work_funding.is_some())
        }) {
            crate::store::adaptive::require_funding_journal_source(connection, epoch, source)?;
            return after_replay();
        }
        let binding = canonical_sha256(
            "sentinel.workflow.recovery-historical-journal-proof.v1",
            &(epoch, source),
        )?;
        // This is a temporary owned proof input, not a pre-write proof or an
        // authority cache. Its clone remains inside the existing scope budget.
        validation_scope::charge_bytes(connection, encode(source)?.len())?;
        HISTORICAL_RECOVERY_JOURNAL.with(|active| {
            let mut active = active.borrow_mut();
            if active.is_some() {
                return Err(corrupt());
            }
            *active = Some(HistoricalRecoveryJournal {
                connection: connection as *const Connection as usize,
                source: source.clone(),
                binding,
            });
            Ok(())
        })?;
        let _guard = HistoricalRecoveryJournalGuard;
        crate::store::adaptive::require_funding_journal_source(connection, epoch, source)?;
        after_replay()
    })
}

fn is_historical_recovery_authorization(
    connection: &Connection,
    authorization: &AdaptiveContinuationAuthorizationV1,
    authority: &RuntimeAuthoritySnapshotV1,
) -> Result<bool, WorkflowError> {
    HISTORICAL_RECOVERY_JOURNAL.with(|active| {
        let active = active.borrow();
        let Some(proof) = active.as_ref().filter(|proof| {
            proof.connection == connection as *const Connection as usize
                && proof.source.grant.session_id == authorization.session_id
        }) else {
            return Ok(false);
        };
        if proof.source.grant.authority != *authority
            || authorization.source_session_version >= proof.source.version
            || !proof
                .source
                .continuation
                .as_ref()
                .is_some_and(|state| state.authorizations.contains(authorization))
        {
            return Err(corrupt());
        }
        Ok(true)
    })
}

#[cfg(test)]
mod issue_856_receipt_lookup_snapshot_tests;

#[cfg(test)]
pub(super) fn historical_review_cut(
    connection: &Connection,
    tenant: &TenantId,
    session_id: Uuid,
    sequence: i64,
) -> Result<(usize, String), WorkflowError> {
    expired_replacement::historical_review_cut(connection, tenant, session_id, sequence)
}

pub(super) fn with_recovery_publication<T>(
    transaction: &Transaction<'_>,
    grant: &AdaptiveLeadershipReviewGrantV1,
    context_digest: &str,
    operation_id: Uuid,
    issued_at_unix_ms: u64,
    publish: impl FnOnce() -> Result<T, WorkflowError>,
) -> Result<T, WorkflowError> {
    let epoch = require_review_binding(grant)?;
    let leaf = FundingReviewLeaf {
        schema_version: 1,
        membership_id: review_id(epoch),
        grant: grant.clone(),
        operation_id,
        context_digest: context_digest.to_owned(),
        issued_at_unix_ms,
    };
    expired_replacement::with_recovery_publication(transaction, &leaf, publish)
}

pub(super) fn require_fresh_epoch_eligibility(
    connection: &Connection,
    epoch: &AdaptiveWorkFundingEpochV1,
) -> Result<(), WorkflowError> {
    require_epoch_receipt(connection, epoch)?;
    let source = &epoch.receipt.request.source.resume_source;
    if expired_replacement::consumed_receipts(connection, &source.tenant_id, source.session_id)?
        .contains(&epoch.receipt.funding_id)
    {
        return Err(transition());
    }
    Ok(())
}

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
    if event_type == ADOPTION_EVENT {
        // Both full and sealed adoption views use this boundary. A late
        // review-expiry exception must retain its durable local authority.
        let sequence: i64 = connection.query_row(
            "SELECT sequence FROM company_events
             WHERE tenant_id=?1 AND event_type=?2 AND operation_id=?3",
            params![tenant.0, event_type, operation_id.to_string()],
            |row| row.get(0),
        )?;
        let row = company_event_shape(connection, stored_u64(sequence)?)?.ok_or_else(corrupt)?;
        let leaf: FundingAdoptionLeaf = decode(&row.payload)?;
        if leaf
            .authorization
            .local_adoption
            .as_ref()
            .is_some_and(|adoption| adoption.request.is_funded())
        {
            WorkflowStore::require_funded_adoption_authority(connection, &leaf.authorization)?;
        }
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

pub(super) fn require_epoch_receipt(
    connection: &Connection,
    epoch: &AdaptiveWorkFundingEpochV1,
) -> Result<(), WorkflowError> {
    validation_scope::with_scope(connection, || {
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
        let superseded =
            superseded_unused_receipts(connection, &source.tenant_id, source.session_id)?;
        if superseded.contains(&stored.funding_id) {
            return Err(unauthorized());
        }
        Ok(())
    })
}

fn require_review_binding(
    grant: &AdaptiveLeadershipReviewGrantV1,
) -> Result<&AdaptiveWorkFundingEpochV1, WorkflowError> {
    let epoch = grant.work_funding.as_deref().ok_or_else(unauthorized)?;
    epoch.validate()?;
    let source = &epoch.receipt.request.source.resume_source;
    if !matches!(grant.schema_version, 5 | 6)
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
    let funded_local = auth
        .local_adoption
        .as_deref()
        .filter(|adoption| adoption.request.is_funded());
    if let Some(adoption) = funded_local {
        auth.validate()?;
        let request = &adoption.request;
        if review.grant.schema_version != 5
            || request.tenant_id != review.grant.leadership_principal.tenant_id
            || request.project_id != review.grant.project_id
            || request.work_item_id != review.grant.work_item_id
            || request.session_id != review.grant.session_id
            || request.review_id != review.grant.review_id
            || request.context_digest != review.context_digest
            || request.decision.schema_version != review.grant.schema_version
        {
            return Err(corrupt());
        }
    }
    if review.grant.work_funding.as_deref() != Some(epoch)
        || review.operation_id != auth.operation_id
        || review.grant.review_id != auth.review_id
        || review.grant.session_id != auth.session_id
        || review.grant.expected_session_version != auth.source_session_version
        || review.grant.assignee_authority != leaf.authority
        || review.grant.leadership_principal != leaf.issuer_principal
        || review.issued_at_unix_ms > auth.issued_at_ms
        || (auth.issued_at_ms >= review.grant.expires_at_unix_ms && funded_local.is_none())
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
        require_funding_review_membership_leaf(connection, grant, context_digest, operation_id)?;
        if grant.schema_version == 6 {
            super::adaptive_funded_review_recovery::require_membership(connection, grant, None)?;
        }
        Ok(())
    })
}

pub(super) fn require_funding_review_membership_leaf(
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
    require_fresh_epoch_eligibility(transaction, epoch)?;
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
        // A complete selected-head replay authenticates exact immutable
        // membership, not future successor history that may depend on this
        // very prefix. The ordinary caller still validates that full relation.
        if is_historical_recovery_authorization(connection, authorization, authority)? {
            let leaf = expired_replacement::sealed_adoption(
                connection,
                &authority.tenant_id,
                &adoption_id(authority, authorization)?,
            )?;
            if leaf.authorization != *authorization || leaf.authority != *authority {
                return Err(corrupt());
            }
            return Ok(());
        }
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
    if authorization
        .local_adoption
        .as_ref()
        .is_some_and(|adoption| adoption.request.is_funded())
    {
        WorkflowStore::require_funded_adoption_authority(transaction, authorization)?;
    }
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
    require_fresh_epoch_eligibility(transaction, epoch)?;
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
        )?;
        expired_replacement::require_successor_membership(connection, self)
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
        let receipt = expired_replacement::read_sealed_funding(connection, tenant, &key)?;
        if let Some(receipt) = &receipt {
            expired_replacement::require_successor_membership(connection, receipt)?;
        }
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
    validation_scope::with_scope(connection, || {
        let ids = funding_inventory(connection, tenant)?;
        let mut receipts = Vec::new();
        for id in ids {
            let receipt = expired_replacement::sealed_funding(connection, tenant, &id)?;
            // Discovery shares sealed row/event proofs. Full successor membership
            // is still checked for every tenant receipt before session filtering.
            expired_replacement::require_successor_membership(connection, &receipt)?;
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

fn funding_inventory(
    connection: &Connection,
    tenant: &TenantId,
) -> Result<Vec<String>, WorkflowError> {
    validation_scope::memoize(connection, "funding-inventory", tenant, || {
        funding_inventory_uncached(connection, tenant)
    })
}

fn funding_inventory_uncached(
    connection: &Connection,
    tenant: &TenantId,
) -> Result<Vec<String>, WorkflowError> {
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
    // Retain sealed keys only, not a claim of full successor membership. The
    // primitive must not traverse relations that may be validating this inventory.
    for id in &ids {
        expired_replacement::sealed_funding(connection, tenant, id)?;
    }
    let mut events = connection.prepare(
        "SELECT sequence FROM company_events WHERE tenant_id=?1 AND event_type=?2 ORDER BY sequence LIMIT 4097")?;
    let sequences = events
        .query_map(params![tenant.0, FUNDING_EVENT], |row| row.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if sequences.len() > 4096 {
        return Err(corrupt());
    }
    // The event selector cannot trust an unvalidated payload either. Match
    // every bounded issuance event to its typed, event-verified entity first.
    for sequence in sequences {
        let row = company_event_shape(connection, stored_u64(sequence)?)?.ok_or_else(corrupt)?;
        let event_receipt: AdaptiveWorkFundingReceiptV1 = decode(&row.payload)?;
        let stored =
            expired_replacement::sealed_funding(connection, tenant, &event_receipt.funding_id)?;
        if stored != event_receipt {
            return Err(corrupt());
        }
    }
    Ok(ids)
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

fn require_never_reviewed_funding(
    connection: &Connection,
    receipt: &AdaptiveWorkFundingReceiptV1,
) -> Result<(), WorkflowError> {
    let used = reviewed_funding_ids(connection, &receipt.request.source.resume_source.tenant_id)?;
    if used.contains(&receipt.funding_id) {
        return Err(transition());
    }
    Ok(())
}

fn reviewed_funding_ids(
    connection: &Connection,
    tenant: &TenantId,
) -> Result<BTreeSet<String>, WorkflowError> {
    reviewed_funding_ids_with_observer(connection, tenant, |_, _| {})
}

pub(super) fn require_never_dispatched_funding(
    connection: &Connection,
    epoch: &AdaptiveWorkFundingEpochV1,
) -> Result<(), WorkflowError> {
    require_pre_recovery_funding_history(connection, epoch, None)
}

pub(super) fn recovery_uses_adopted_epoch(
    source: &crate::AdaptiveSessionV1,
    epoch: &AdaptiveWorkFundingEpochV1,
) -> bool {
    source
        .active_work_funding()
        .is_some_and(|active| active.same_epoch(epoch))
}

pub(super) fn recovery_prior_adopted_call(
    source: &crate::AdaptiveSessionV1,
    call: &AdaptiveLeadershipReviewCallV1,
) -> bool {
    call.version == 3
        && call.retired_at_unix_ms.is_none()
        && call.continuation.as_ref().is_some_and(|authorization| {
            authorization.source_session_version < source.version
                && authorization.session_id == source.grant.session_id
                && call.grant.assignee_authority == source.grant.authority
                && call.context.source_session.grant == source.grant
                && source
                    .continuation
                    .as_ref()
                    .is_some_and(|state| state.authorizations.contains(authorization))
        })
}

impl crate::AdaptiveFundedReviewRecoveryVerificationSourceV1 {
    /// Classification only: the source must come from the store's authenticated
    /// frozen journal. All other reviews still require physical NoIO checks.
    pub fn review_has_prior_adoption(&self, call: &AdaptiveLeadershipReviewCallV1) -> bool {
        call.grant.work_funding.as_ref().is_some_and(|epoch| {
            epoch.receipt == *self.funding
                && recovery_uses_adopted_epoch(&self.session, epoch)
                && recovery_prior_adopted_call(&self.session, call)
        })
    }
}

/// The selected expired review remains NoIO; earlier committed work is history,
/// authenticated by the exact journal prefix rather than relabeled as unused.
pub(super) fn require_funded_recovery_source_history(
    connection: &Connection,
    epoch: &AdaptiveWorkFundingEpochV1,
    source: &crate::AdaptiveSessionV1,
) -> Result<(), WorkflowError> {
    if !recovery_uses_adopted_epoch(source, epoch) {
        return require_never_dispatched_funding(connection, epoch);
    }
    let key = canonical_sha256(
        "sentinel.workflow.funded-recovery-source-history.v1",
        &(epoch, source),
    )?;
    validation_scope::memoize(connection, "funded-recovery-source-history", &key, || {
        crate::store::adaptive::require_funding_journal_source(connection, epoch, source)?;
        require_pre_recovery_funding_history_uncached(connection, epoch, None, Some(source))
    })
}

fn recovery_prior_authorization(
    source: Option<&crate::AdaptiveSessionV1>,
    authorization: &AdaptiveContinuationAuthorizationV1,
    sequence: u64,
    cut: Option<u64>,
) -> bool {
    source.is_some_and(|source| {
        authorization.source_session_version < source.version
            && cut.is_none_or(|cut| sequence < cut)
            && source
                .continuation
                .as_ref()
                .is_some_and(|state| state.authorizations.contains(authorization))
    })
}

fn recovery_prior_trace(
    connection: &Connection,
    source: Option<&crate::AdaptiveSessionV1>,
    call: &AdaptiveLeadershipReviewCallV1,
    sequence: u64,
    cut: Option<u64>,
) -> Result<bool, WorkflowError> {
    let Some(source) = source else {
        return Ok(false);
    };
    if cut.is_some_and(|cut| sequence >= cut) {
        return Ok(false);
    }
    let Some(authorization) = source.continuation.as_ref().and_then(|state| {
        state.authorizations.iter().find(|authorization| {
            authorization.review_id == call.grant.review_id
                && authorization.operation_id == call.operation_id
                && authorization.source_session_version == call.grant.expected_session_version
                && authorization.work_funding == call.grant.work_funding
        })
    }) else {
        return Ok(false);
    };
    let completed: AdaptiveLeadershipReviewCallV1 = entity_shape(
        connection,
        &call.grant.leadership_principal.tenant_id,
        "adaptive_leadership_review_call",
        &call.review_key,
    )?
    .ok_or_else(corrupt)?;
    completed.validate_entity().map_err(|_| corrupt())?;
    Ok(recovery_prior_adopted_call(source, &completed)
        && completed.continuation.as_ref() == Some(authorization)
        && completed.grant == call.grant
        && completed.context == call.context
        && completed.operation_id == call.operation_id
        && completed.allowance_id == call.allowance_id
        && completed.grant_issued_at_unix_ms == call.grant_issued_at_unix_ms
        && call.updated_at_unix_ms <= completed.updated_at_unix_ms
        && match call.version {
            1 => true,
            2 => call.dispatch == completed.dispatch,
            3 => call == &completed,
            _ => false,
        })
}

// The designated tuple ends with issuance time and the sequence of the fully
// validated recovery issuance event. This boundary proves history, not authority.
pub(super) fn require_pre_recovery_funding_history(
    connection: &Connection,
    epoch: &AdaptiveWorkFundingEpochV1,
    designated: Option<(
        &AdaptiveLeadershipReviewGrantV1,
        &AdaptiveLeadershipReviewContextV1,
        Uuid,
        u64,
        u64,
    )>,
) -> Result<(), WorkflowError> {
    let prior_session = designated.and_then(|(_, context, _, _, _)| {
        recovery_uses_adopted_epoch(&context.source_session, epoch)
            .then_some(&context.source_session)
    });
    validation_scope::memoize(
        connection,
        "funded-recovery-history",
        &(epoch, designated),
        || {
            if let Some(source) = prior_session {
                crate::store::adaptive::require_funding_journal_source(connection, epoch, source)?;
            }
            require_pre_recovery_funding_history_uncached(
                connection,
                epoch,
                designated,
                prior_session,
            )
        },
    )
}

fn require_pre_recovery_funding_history_uncached(
    connection: &Connection,
    epoch: &AdaptiveWorkFundingEpochV1,
    designated: Option<(
        &AdaptiveLeadershipReviewGrantV1,
        &AdaptiveLeadershipReviewContextV1,
        Uuid,
        u64,
        u64,
    )>,
    prior_session: Option<&crate::AdaptiveSessionV1>,
) -> Result<(), WorkflowError> {
    let tenant = &epoch.receipt.request.source.resume_source.tenant_id;
    // Validate all sealed review traces before filtering by funding identity.
    reviewed_funding_ids(connection, tenant)?;
    let ids = {
        let mut statement = connection.prepare(
            "SELECT entity_id FROM company_entities WHERE tenant_id=?1 AND entity_kind=?2
             ORDER BY entity_id LIMIT 4097",
        )?;
        let rows = statement.query_map(params![tenant.0, ADOPTION_KIND], |row| {
            row.get::<_, String>(0)
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    if ids.len() > 4096 {
        return Err(corrupt());
    }
    for id in ids {
        let leaf = expired_replacement::sealed_adoption(connection, tenant, &id)?;
        if leaf
            .authorization
            .work_funding
            .as_ref()
            .is_some_and(|prior| prior.same_epoch(epoch))
        {
            // Sealed adoption already proved this issuance event unique and
            // authentic. Use its sequence, never payload time alone, as the cut.
            let sequence: i64 = connection.query_row(
                "SELECT sequence FROM company_events
                 WHERE tenant_id=?1 AND event_type=?2 AND operation_id=?3",
                params![
                    tenant.0,
                    ADOPTION_EVENT,
                    leaf.authorization.operation_id.to_string()
                ],
                |row| row.get(0),
            )?;
            if recovery_prior_authorization(
                prior_session,
                &leaf.authorization,
                stored_u64(sequence)?,
                designated.map(|(_, _, _, _, cut)| cut),
            ) {
                continue;
            }
            let Some((grant, _, operation, issued, cut)) = designated else {
                return Err(transition());
            };
            let original = leaf.authorization.review_id == grant.review_id
                && leaf.authorization.operation_id == operation
                && leaf.authorization.issued_at_ms >= issued
                && stored_u64(sequence)? > cut;
            let later = stored_u64(sequence)? > cut
                && leaf.authorization.issued_at_ms >= issued
                && leaf.authorization.source_session_version > grant.expected_session_version;
            if !original && !later {
                return Err(transition());
            }
        }
    }
    let mut statement = connection.prepare(
        "SELECT sequence,event_type,typeof(payload) FROM company_events WHERE tenant_id=?1
         AND (event_type GLOB 'adaptive_leadership_review_*' OR event_type=?2)
         ORDER BY sequence LIMIT 4097",
    )?;
    let mut rows = statement.query(params![tenant.0, ADOPTION_EVENT])?;
    let mut count = 0;
    while let Some(row) = rows.next()? {
        count += 1;
        if count > 4096 {
            return Err(corrupt());
        }
        let sequence: i64 = row.get(0)?;
        let event_type: String = row.get(1)?;
        let payload_type: String = row.get(2)?;
        if payload_type != "blob" {
            return Err(corrupt());
        }
        let sequence = stored_u64(sequence)?;
        let owner = company_event_shape(connection, sequence)?.ok_or_else(corrupt)?;
        let payload = owner.payload.as_slice();
        // Adoption events have a separate typed shape and sealed membership.
        if event_type == ADOPTION_EVENT {
            let leaf: FundingAdoptionLeaf = decode(payload)?;
            let stored =
                expired_replacement::sealed_adoption(connection, tenant, &leaf.membership_id)?;
            if stored != leaf {
                return Err(corrupt());
            }
            if leaf
                .authorization
                .work_funding
                .as_ref()
                .is_some_and(|prior| prior.same_epoch(epoch))
                && !recovery_prior_authorization(
                    prior_session,
                    &leaf.authorization,
                    sequence,
                    designated.map(|(_, _, _, _, cut)| cut),
                )
                && !designated.is_some_and(|(grant, _, operation, issued, cut)| {
                    (leaf.authorization.review_id == grant.review_id
                        && leaf.authorization.operation_id == operation
                        && leaf.authorization.issued_at_ms >= issued
                        && sequence > cut)
                        || (sequence > cut
                            && leaf.authorization.issued_at_ms >= issued
                            && leaf.authorization.source_session_version
                                > grant.expected_session_version)
                })
            {
                return Err(transition());
            }
        } else {
            let call: AdaptiveLeadershipReviewCallV1 = decode(payload)?;
            call.validate_entity()?;
            // Authenticate every observation before allowing an original or
            // later call. Loading current call membership here would recurse.
            let digest = canonical_sha256("sentinel.workflow.adaptive-leadership-call.v1", &call)?;
            let principal = &call.grant.leadership_principal;
            let payload_digest =
                bytes_digest("sentinel.workflow.company-event-payload.v1", payload)?;
            let authority_digest = principal.binding_digest()?;
            let event_id = canonical_sha256(
                "sentinel.workflow.company-event-id.v1",
                &(
                    tenant,
                    Some(&call.grant.project_id),
                    &event_type,
                    call.operation_id,
                    &digest,
                    &authority_digest,
                    &payload_digest,
                    call.updated_at_unix_ms,
                ),
            )?;
            if company_event_principal(&owner)? != *principal
                || owner.tenant_id != tenant.0
                || owner.project_id.as_deref() != Some(call.grant.project_id.0.as_str())
                || owner.event_type != event_type
                || owner.operation_id != call.operation_id.to_string()
                || owner.operation_digest != digest
                || owner.authority_binding_digest != authority_digest
                || owner.payload_digest != payload_digest
                || owner.event_id != event_id
                || stored_u64(owner.created_at_ms)? != call.updated_at_unix_ms
            {
                return Err(corrupt());
            }
            if designated.is_some_and(|(grant, context, operation, issued, cut)| {
                call.grant == *grant
                    && call.context == *context
                    && call.operation_id == operation
                    && call.grant_issued_at_unix_ms == issued
                    && sequence > cut
            }) {
                continue;
            }
            if call
                .grant
                .work_funding
                .as_ref()
                .is_some_and(|prior| prior.same_epoch(epoch))
            {
                if recovery_prior_trace(
                    connection,
                    prior_session,
                    &call,
                    sequence,
                    designated.map(|(_, _, _, _, cut)| cut),
                )? {
                    continue;
                }
                if let Some((grant, _, _, issued, cut)) = designated {
                    if sequence > cut {
                        if call.grant_issued_at_unix_ms < issued
                            || call.grant.expected_session_version <= grant.expected_session_version
                        {
                            return Err(transition());
                        }
                        // This proves only that a later immutable observation
                        // cannot invalidate the old issuance-time NoIO fact.
                        continue;
                    }
                }
                if call.grant.schema_version != 5
                    || call.dispatch.is_some()
                    || call.decision.is_some()
                    || call.model_response_digest.is_some()
                    || call.continuation.is_some()
                    || call.resolution_event_id.is_some()
                {
                    return Err(transition());
                }
            }
        }
    }
    Ok(())
}

fn reviewed_funding_ids_with_observer(
    connection: &Connection,
    tenant: &TenantId,
    mut before_payload_copy: impl FnMut(&'static str, usize),
) -> Result<BTreeSet<String>, WorkflowError> {
    // Validate before selection. Do not recursively load the epoch from its
    // own review trace; only sealed row/event shape is needed for a veto.
    validation_scope::memoize(connection, "unused-funding-review-traces", tenant, || {
        let mut used = BTreeSet::new();
        let count: i64 = connection.query_row(
            "SELECT count(*) FROM (SELECT 1 FROM company_entities
             WHERE tenant_id=?1 AND entity_kind IN
             ('adaptive_work_funding_review','adaptive_leadership_review_call') LIMIT 4097)",
            [&tenant.0],
            |row| row.get(0),
        )?;
        if count > 4096 {
            return Err(corrupt());
        }
        {
            let mut statement = connection.prepare(
                "SELECT entity_kind,entity_id FROM company_entities
                 WHERE tenant_id=?1 AND entity_kind IN
                 ('adaptive_work_funding_review','adaptive_leadership_review_call')
                 ORDER BY entity_kind,entity_id LIMIT 4097",
            )?;
            let mut rows = statement.query([&tenant.0])?;
            while let Some(row) = rows.next()? {
                let kind: String = row.get(0)?;
                let id: String = row.get(1)?;
                let funding_id = if kind == REVIEW_KIND {
                    let leaf: FundingReviewLeaf =
                        entity_shape_with_observer(connection, tenant, &kind, &id, |len| {
                            before_payload_copy("entity", len)
                        })?
                        .ok_or_else(corrupt)?;
                    leaf.validate_entity()?;
                    Some(
                        require_review_binding(&leaf.grant)?
                            .binding
                            .funding_id
                            .clone(),
                    )
                } else {
                    let call: AdaptiveLeadershipReviewCallV1 =
                        entity_shape_with_observer(connection, tenant, &kind, &id, |len| {
                            before_payload_copy("entity", len)
                        })?
                        .ok_or_else(corrupt)?;
                    call.validate_entity()?;
                    call.grant
                        .work_funding
                        .map(|epoch| epoch.binding.funding_id)
                };
                if let Some(id) = funding_id {
                    used.insert(id);
                }
            }
        }
        let sequences = {
            let mut statement = connection.prepare(
                "SELECT sequence FROM company_events WHERE tenant_id=?1
                 AND (event_type=?2 OR event_type GLOB 'adaptive_leadership_review_*')
                 ORDER BY sequence LIMIT 4097",
            )?;
            let rows =
                statement.query_map(params![tenant.0, REVIEW_EVENT], |row| row.get::<_, i64>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        if sequences.len() > 4096 {
            return Err(corrupt());
        }
        for sequence in sequences {
            let payload_bytes: i64 = connection
                .query_row(
                    "SELECT CASE WHEN typeof(payload)='blob' THEN length(payload) ELSE -1 END
                     FROM company_events WHERE sequence=?1",
                    [sequence],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or_else(corrupt)?;
            usize::try_from(payload_bytes).map_err(|_| corrupt())?;
            let row =
                company_event_shape_with_observer(connection, stored_u64(sequence)?, |len| {
                    before_payload_copy("event", len)
                })?
                .ok_or_else(corrupt)?;
            let (principal, project, operation, digest, time, funding_id) = if row.event_type
                == REVIEW_EVENT
            {
                let leaf: FundingReviewLeaf = decode(&row.payload)?;
                leaf.validate_entity()?;
                let digest =
                    canonical_sha256("sentinel.workflow.adaptive-work-funding-review.v1", &leaf)?;
                let funding_id = require_review_binding(&leaf.grant)?
                    .binding
                    .funding_id
                    .clone();
                (
                    leaf.grant.leadership_principal,
                    leaf.grant.project_id,
                    leaf.operation_id,
                    digest,
                    leaf.issued_at_unix_ms,
                    Some(funding_id),
                )
            } else {
                let call: AdaptiveLeadershipReviewCallV1 = decode(&row.payload)?;
                call.validate_entity()?;
                let digest =
                    canonical_sha256("sentinel.workflow.adaptive-leadership-call.v1", &call)?;
                let funding_id = call
                    .grant
                    .work_funding
                    .as_ref()
                    .map(|epoch| epoch.binding.funding_id.clone());
                (
                    call.grant.leadership_principal,
                    call.grant.project_id,
                    call.operation_id,
                    digest,
                    call.updated_at_unix_ms,
                    funding_id,
                )
            };
            let payload_digest =
                bytes_digest("sentinel.workflow.company-event-payload.v1", &row.payload)?;
            let authority_digest = principal.binding_digest()?;
            let event_id = canonical_sha256(
                "sentinel.workflow.company-event-id.v1",
                &(
                    tenant,
                    Some(&project),
                    &row.event_type,
                    operation,
                    &digest,
                    &authority_digest,
                    &payload_digest,
                    time,
                ),
            )?;
            if company_event_principal(&row)? != principal
                || row.tenant_id != tenant.0
                || row.project_id.as_deref() != Some(project.0.as_str())
                || row.operation_id != operation.to_string()
                || row.operation_digest != digest
                || row.authority_binding_digest != authority_digest
                || row.payload_digest != payload_digest
                || row.event_id != event_id
                || stored_u64(row.created_at_ms)? != time
            {
                return Err(corrupt());
            }
            if let Some(id) = funding_id {
                used.insert(id);
            }
        }
        Ok(used)
    })
}

fn superseded_unused_receipts(
    connection: &Connection,
    tenant: &TenantId,
    session_id: Uuid,
) -> Result<BTreeSet<String>, WorkflowError> {
    validation_scope::memoize(
        connection,
        "funding-supersession",
        &(tenant, session_id),
        || {
            let receipts = expired_replacement::sealed_receipts(connection, tenant, session_id)?;
            superseded_unused_receipts_uncached(connection, &receipts)
        },
    )
}

fn superseded_validated_receipts(
    connection: &Connection,
    tenant: &TenantId,
    session_id: Uuid,
    receipts: &[AdaptiveWorkFundingReceiptV1],
) -> Result<BTreeSet<String>, WorkflowError> {
    // Callers already validated the complete tenant inventory in this scope.
    validation_scope::memoize(
        connection,
        "funding-supersession",
        &(tenant, session_id),
        || superseded_unused_receipts_uncached(connection, receipts),
    )
}

fn superseded_unused_receipts_uncached(
    connection: &Connection,
    receipts: &[AdaptiveWorkFundingReceiptV1],
) -> Result<BTreeSet<String>, WorkflowError> {
    let mut superseded = BTreeSet::new();
    let mut by_digest = BTreeMap::new();
    for receipt in receipts {
        if by_digest
            .insert(receipt.receipt_digest()?, receipt)
            .is_some()
        {
            return Err(corrupt());
        }
    }
    for successor in receipts {
        let Some(digest) = &successor.request.source.supersedes_unused_receipt_digest else {
            continue;
        };
        let old = by_digest.get(digest).ok_or_else(corrupt)?;
        let prior = &old.request.source;
        let fresh = &successor.request.source;
        if old.issued_at_unix_ms >= successor.issued_at_unix_ms
            || old.request.limits.expires_at_unix_ms > successor.issued_at_unix_ms
            || prior.resume_source.tenant_id != fresh.resume_source.tenant_id
            || prior.resume_source.project_id != fresh.resume_source.project_id
            || prior.resume_source.work_item_id != fresh.resume_source.work_item_id
            || prior.resume_source.session_id != fresh.resume_source.session_id
            || prior.resume_source.assignee_authority != fresh.resume_source.assignee_authority
            || prior.original_model_call_ceiling != fresh.original_model_call_ceiling
            || prior.original_tool_call_ceiling != fresh.original_tool_call_ceiling
            || prior.current_model_call_ceiling != fresh.current_model_call_ceiling
            || prior.current_tool_call_ceiling != fresh.current_tool_call_ceiling
            || prior.predecessor_receipt_digest != fresh.predecessor_receipt_digest
            || !superseded.insert(old.funding_id.clone())
        {
            return Err(corrupt());
        }
        require_never_reviewed_funding(connection, old)?;
    }
    Ok(superseded)
}

// A replacement is explicit and atomically recorded in the new issuance event.
// Unused old capacity is neither inherited nor refunded; old leaves are immutable.
fn require_unfunded_source(
    connection: &Connection,
    tenant: &TenantId,
    project_id: &ProjectId,
    session_id: Uuid,
    now_ms: u64,
    supersedes_unused_receipt_digest: Option<&str>,
    limits: &AdaptiveWorkFundingLimitsV1,
) -> Result<(AdaptiveWorkFundingSourceV1, u64), WorkflowError> {
    require_unfunded_source_with_candidate(
        connection,
        tenant,
        project_id,
        session_id,
        now_ms,
        supersedes_unused_receipt_digest.map(PendingFundingReplacement::Unused),
        limits,
    )
}

#[derive(Clone, Copy)]
enum PendingFundingReplacement<'a> {
    Unused(&'a str),
    Reviewed(&'a str),
}

fn require_unfunded_source_with_candidate(
    connection: &Connection,
    tenant: &TenantId,
    project_id: &ProjectId,
    session_id: Uuid,
    now_ms: u64,
    replacement: Option<PendingFundingReplacement<'_>>,
    limits: &AdaptiveWorkFundingLimitsV1,
) -> Result<(AdaptiveWorkFundingSourceV1, u64), WorkflowError> {
    let scope = validation_scope::enter(connection)?;
    let receipts = funding_receipts(connection, tenant, session_id)?;
    if receipts.len() >= 128 {
        return Err(transition());
    }
    let (resume_source, session) = adaptive_resume_policy::fresh_resume_source(
        connection, tenant, project_id, session_id, now_ms, None,
    )?;
    let adopted = adopted_epochs(&session);
    if limits.additional_model_calls == 0 && limits.additional_tool_calls == 0 {
        // Renewal cannot borrow an unadopted proposal or refund actual spending.
        let predecessor = session.active_work_funding().ok_or_else(transition)?;
        if !receipts.contains(&predecessor.receipt)
            || session.model_calls >= session.funded_model_call_ceiling()
            || session.tool_calls >= session.funded_tool_call_ceiling()
        {
            return Err(transition());
        }
    }
    let superseded = consumed_validated_receipts(connection, tenant, session_id, &receipts)?;
    let pending = receipts
        .iter()
        .filter(|receipt| {
            !adopted.iter().any(|epoch| epoch.receipt == **receipt)
                && !superseded.contains(&receipt.funding_id)
        })
        .collect::<Vec<_>>();
    match (pending.as_slice(), replacement) {
        ([], None) => {}
        ([old], Some(PendingFundingReplacement::Reviewed(digest)))
            if old.receipt_digest()? == digest => {}
        ([old], Some(PendingFundingReplacement::Unused(digest)))
            if old.receipt_digest()? == digest
                && old.request.limits.expires_at_unix_ms <= now_ms =>
        {
            require_never_reviewed_funding(connection, old)?;
        }
        _ => return Err(transition()),
    }
    if adopted.len() + superseded.len() + pending.len() != receipts.len() {
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
        supersedes_unused_receipt_digest: match replacement {
            Some(PendingFundingReplacement::Unused(digest)) => Some(digest.to_owned()),
            _ => None,
        },
        expired_reviewed_replacement: None,
    };
    source.validate()?;
    scope.finish()?;
    Ok((source, session.grant.max_call_duration_ms))
}

pub(super) fn require_unfunded_review_lane(
    connection: &Connection,
    session: &crate::AdaptiveSessionV1,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    let tenant = &session.grant.authority.tenant_id;
    let session_id = session.grant.session_id;
    let scope = validation_scope::enter(connection)?;
    let receipts = funding_receipts(connection, tenant, session_id)?;
    let adopted = adopted_epochs(session);
    let superseded = consumed_validated_receipts(connection, tenant, session_id, &receipts)?;
    // Selection and authorization are separate reads. A concurrently issued
    // live proposal must be reselected, not stranded by an older unfunded call.
    if receipts.iter().any(|receipt| {
        now_ms < receipt.request.limits.expires_at_unix_ms
            && !adopted.iter().any(|epoch| epoch.receipt == *receipt)
            && !superseded.contains(&receipt.funding_id)
    }) {
        return Err(transition());
    }
    scope.finish()?;
    Ok(())
}

pub(super) fn require_fresh_funding_review_source(
    connection: &Connection,
    call: &AdaptiveLeadershipReviewCallV1,
) -> Result<(), WorkflowError> {
    let epoch = require_review_binding(&call.grant)?;
    require_fresh_epoch_eligibility(connection, epoch)?;
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
            || (call.grant.schema_version != 6
                && epoch.binding.ordinal
                    != initial
                        .base_review_count
                        .checked_add(1)
                        .ok_or_else(corrupt)?)
        {
            return Err(transition());
        }
        if call.grant.schema_version == 6 {
            let receipt = super::adaptive_funded_review_recovery::require_membership(
                connection,
                &call.grant,
                Some(&call.context),
            )?;
            if receipt.context.source_session != session
                || receipt.request.source.session_head_digest != head_digest
            {
                return Err(transition());
            }
        }
    }
    Ok(())
}

impl WorkflowStore {
    /// Reads persisted funding-review evidence in one validated SQL snapshot.
    /// The final value is the sole active review for the selected session version,
    /// or None when the check is skipped, empty or ambiguous. It is not authority.
    /// Deferred receipt/history errors preserve the caller's comparison order and
    /// never become reusable read-cache proofs.
    #[allow(clippy::type_complexity)]
    pub fn adaptive_work_funding_review_snapshot(
        &self,
        tenant: &TenantId,
        review_id: Uuid,
        session_id: Uuid,
        funding_operation_id: Uuid,
        expected_session_version: u64,
        check_current: bool,
    ) -> Result<
        (
            Option<AdaptiveLeadershipReviewCallV1>,
            Result<Option<AdaptiveWorkFundingReceiptV1>, WorkflowError>,
            Result<Option<Uuid>, WorkflowError>,
        ),
        WorkflowError,
    > {
        type DeferredError = (WorkflowErrorCode, bool, &'static str);
        type Proof = (
            Option<AdaptiveLeadershipReviewCallV1>,
            Result<Option<AdaptiveWorkFundingReceiptV1>, DeferredError>,
            Result<Option<Uuid>, DeferredError>,
        );

        tenant.validate()?;
        if review_id.is_nil() {
            return Err(invalid("invalid leadership review identity"));
        }
        let query_key = serde_json::to_vec(&(
            tenant,
            review_id,
            session_id,
            funding_operation_id,
            expected_session_version,
            check_current,
        ))
        .map_err(|_| persistence())?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let (review, receipt, current) = self.validated_read_snapshot_if(
            &mut connection,
            "work-funding-review",
            &query_key,
            |snapshot| -> Result<Proof, WorkflowError> {
                let scope = validation_scope::enter(snapshot)?;
                let review = get_entity(
                    snapshot,
                    tenant,
                    "adaptive_leadership_review_call",
                    &review_id.to_string(),
                )?;
                if review.is_none() {
                    scope.finish()?;
                    return Ok((review, Ok(None), Ok(None)));
                }
                // Deferred failures drop this scope (rolling back its savepoint)
                // and fail the retention predicate instead of caching partial proofs.
                let receipt =
                    match read_funding_leaf(snapshot, tenant, session_id, funding_operation_id) {
                        Ok(receipt) => receipt,
                        Err(error) => {
                            return Ok((
                                review,
                                Err((error.code, error.retryable, error.message)),
                                Ok(None),
                            ));
                        }
                    };
                if receipt.is_none() || !check_current {
                    scope.finish()?;
                    return Ok((review, Ok(receipt), Ok(None)));
                }
                // Validate the full bounded inventory before filtering, including
                // historical and unrelated-session rows in this tenant.
                let calls = match adaptive_leadership_review::calls_for_session(
                    snapshot, tenant, session_id,
                ) {
                    Ok(calls) => calls,
                    Err(error) => {
                        return Ok((
                            review,
                            Ok(receipt),
                            Err((error.code, error.retryable, error.message)),
                        ));
                    }
                };
                let mut matching = calls.iter().filter(|candidate| {
                    candidate.decision.is_none()
                        && candidate.retired_at_unix_ms.is_none()
                        && candidate.grant.expected_session_version == expected_session_version
                });
                let current = matching.next().map(|call| call.grant.review_id);
                let current = if matching.next().is_none() {
                    current
                } else {
                    None
                };
                scope.finish()?;
                Ok((review, Ok(receipt), Ok(current)))
            },
            |(_, receipt, current)| receipt.is_ok() && current.is_ok(),
        )?;
        Ok((
            review,
            receipt
                .map_err(|(code, retryable, message)| WorkflowError::new(code, retryable, message)),
            current
                .map_err(|(code, retryable, message)| WorkflowError::new(code, retryable, message)),
        ))
    }

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
        let superseded = consumed_validated_receipts(&transaction, tenant, session_id, &receipts)?;
        let pending: Vec<_> = receipts
            .iter()
            .filter(|receipt| {
                !adopted.iter().any(|epoch| epoch.receipt == **receipt)
                    && !superseded.contains(&receipt.funding_id)
            })
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
        self.adaptive_work_funding_draft_with_supersession(
            principal,
            project_id,
            session_id,
            operation_id,
            reason_ref,
            limits,
            now_ms,
            None,
        )
    }

    /// An explicit digest may replace only an expired, never-reviewed proposal.
    pub fn adaptive_work_funding_draft_with_supersession(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        project_id: &ProjectId,
        session_id: Uuid,
        operation_id: Uuid,
        reason_ref: &str,
        limits: AdaptiveWorkFundingLimitsV1,
        now_ms: u64,
        supersedes_unused_receipt_digest: Option<&str>,
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
                || prior.request.source.expired_reviewed_replacement.is_some()
                || prior.request.source.resume_source.project_id != *project_id
                || prior.request.reason_ref != reason_ref
                || prior.request.limits != limits
                || prior
                    .request
                    .source
                    .supersedes_unused_receipt_digest
                    .as_deref()
                    != supersedes_unused_receipt_digest
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
            supersedes_unused_receipt_digest,
            &limits,
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
        if request.source.expired_reviewed_replacement.is_some() {
            return Err(unauthorized());
        }
        let (fresh, duration) = require_unfunded_source(
            &transaction,
            &source.tenant_id,
            &source.project_id,
            source.session_id,
            now_ms,
            request.source.supersedes_unused_receipt_digest.as_deref(),
            &request.limits,
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
        if receipt
            .request
            .source
            .supersedes_unused_receipt_digest
            .is_some()
        {
            let mut receipts =
                funding_receipts(&transaction, &source.tenant_id, source.session_id)?;
            receipts.push(receipt.clone());
            // The prospective successor is not part of the stored snapshot proof.
            superseded_unused_receipts_uncached(&transaction, &receipts)?;
        }
        scope.finish()?;
        put_funding_receipt(&transaction, &receipt)?;
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
        let query_key =
            serde_json::to_vec(&(tenant, session_id, operation_id)).map_err(|_| persistence())?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        // Retain historical evidence only. The leaf reader owns one shared
        // validation arena; current eligibility and authority are separate reads.
        self.validated_read_snapshot(
            &mut connection,
            "work-funding-receipt-exact",
            &query_key,
            |snapshot| read_funding_leaf(snapshot, tenant, session_id, operation_id),
        )
    }
}

fn funding_conflict() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::IdempotencyConflict,
        false,
        "adaptive work funding is immutable",
    )
}

fn put_funding_receipt(
    transaction: &Transaction<'_>,
    receipt: &AdaptiveWorkFundingReceiptV1,
) -> Result<(), WorkflowError> {
    let source = &receipt.request.source.resume_source;
    put_entity(
        transaction,
        &source.tenant_id,
        FUNDING_KIND,
        &receipt.funding_id,
        1,
        receipt,
    )?;
    append_event(
        transaction,
        &receipt.issuer_principal,
        receipt.request.operation_id,
        &receipt.request.canonical_digest()?,
        Some(&source.project_id),
        FUNDING_EVENT,
        receipt,
        receipt.issued_at_unix_ms,
    )?;
    Ok(())
}

fn consumed_validated_receipts(
    connection: &Connection,
    tenant: &TenantId,
    session_id: Uuid,
    receipts: &[AdaptiveWorkFundingReceiptV1],
) -> Result<BTreeSet<String>, WorkflowError> {
    let mut consumed = superseded_validated_receipts(connection, tenant, session_id, receipts)?;
    for id in expired_replacement::consumed_receipts(connection, tenant, session_id)? {
        if !consumed.insert(id) {
            return Err(corrupt());
        }
    }
    Ok(consumed)
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

#[cfg(test)]
#[test]
fn funding_inventory_and_supersession_scope_keys_keep_tenants_and_sessions_distinct() {
    let directory = tempfile::tempdir().unwrap();
    let store = WorkflowStore::open(directory.path().join("funding-proof.sqlite")).unwrap();
    let connection = store.connection.lock().unwrap();
    let first = TenantId::parse("first-tenant").unwrap();
    let second = TenantId::parse("second-tenant").unwrap();
    let one = Uuid::from_u128(9101);
    let two = Uuid::from_u128(9102);
    for _ in 0..2 {
        validation_scope::with_scope(&connection, || {
            assert!(funding_receipts(&connection, &first, one)?.is_empty());
            assert!(funding_receipts(&connection, &first, two)?.is_empty());
            assert_eq!(validation_scope::validations("funding-inventory"), 1);
            assert!(superseded_validated_receipts(&connection, &first, one, &[])?.is_empty());
            for _ in 0..3 {
                assert!(superseded_unused_receipts(&connection, &first, one)?.is_empty());
                assert!(superseded_unused_receipts(&connection, &first, two)?.is_empty());
            }
            assert_eq!(validation_scope::validations("funding-supersession"), 2);
            assert!(superseded_unused_receipts(&connection, &second, one)?.is_empty());
            assert_eq!(validation_scope::validations("funding-inventory"), 2);
            assert_eq!(validation_scope::validations("funding-supersession"), 3);
            Ok(())
        })
        .unwrap();
        assert_eq!(validation_scope::validations("funding-inventory"), 0);
        assert_eq!(validation_scope::validations("funding-supersession"), 0);
    }
    assert!(connection.is_autocommit());
}

#[cfg(test)]
mod review_payload_budget_tests {
    use super::*;

    const ARENA_BYTES: usize = 64 * 1024 * 1024;
    const NOW: u64 = 1_000_000;

    pub(super) fn review_leaf() -> FundingReviewLeaf {
        let tenant = TenantId::parse("review-budget-tenant").unwrap();
        let project = ProjectId::parse("review-budget-project").unwrap();
        let work = crate::WorkItemId::parse("review-budget-work").unwrap();
        let leader =
            crate::PrincipalAuthorityV1::derive("review-budget-leader", 1, &[1; 32]).unwrap();
        let principal = AuthenticatedCompanyPrincipalV1 {
            schema_version: 1,
            tenant_id: tenant.clone(),
            principal_id: leader.principal_id.clone(),
            kind: CompanyPrincipalKindV1::Agent,
            role: CompanyRoleV1::ProjectManager,
            customer_id: None,
            agent_id: Some(crate::AgentId(1)),
            authority_generation: leader.principal_generation,
            authority_digest: leader.authority_digest.clone(),
        };
        let authority = RuntimeAuthoritySnapshotV1 {
            schema_version: 1,
            tenant_id: tenant.clone(),
            project_id: project.clone(),
            work_item_id: work.clone(),
            agent_id: crate::AgentId(2),
            assignment_version: 1,
            assignment_digest: "a".repeat(64),
            organization_generation: 1,
            organization_digest: "a".repeat(64),
            principal: crate::PrincipalAuthorityV1::derive("review-budget-developer", 1, &[2; 32])
                .unwrap(),
            profile_id: "developer-profile".into(),
            profile_generation: 1,
            profile_digest: "a".repeat(64),
            runtime_key: "bwrap-coding-v1".into(),
            runtime_generation: 1,
            runtime_digest: "a".repeat(64),
            policy_generation: 1,
            policy_digest: "a".repeat(64),
            active: true,
            capabilities: BTreeSet::from(["file.inspect".into()]),
        };
        let request = AdaptiveWorkFundingRequestV1 {
            schema_version: 1,
            operation_id: Uuid::from_u128(9201),
            source: AdaptiveWorkFundingSourceV1 {
                resume_source: crate::AdaptiveResumeSourceV1 {
                    tenant_id: tenant.clone(),
                    project_id: project.clone(),
                    work_item_id: work.clone(),
                    session_id: Uuid::from_u128(9202),
                    expected_project_version: 1,
                    expected_session_version: 1,
                    project_payload_digest: "a".repeat(64),
                    root_entry_digest: "a".repeat(64),
                    head_entry_digest: "a".repeat(64),
                    continuation_history_digest: "a".repeat(64),
                    review_history_digest: "a".repeat(64),
                    assignee_authority: authority.clone(),
                    base_model_calls: 0,
                    base_tool_calls: 0,
                    base_review_count: 0,
                    base_window_count: 0,
                    subject: crate::AdaptiveResumeSubjectV1::ReadyForModel {
                        active_allowance_digest: "a".repeat(64),
                    },
                },
                original_model_call_ceiling: 2,
                original_tool_call_ceiling: 2,
                current_model_call_ceiling: 2,
                current_tool_call_ceiling: 2,
                predecessor_receipt_digest: None,
                supersedes_unused_receipt_digest: None,
                expired_reviewed_replacement: None,
            },
            limits: AdaptiveWorkFundingLimitsV1 {
                additional_model_calls: 2,
                additional_tool_calls: 2,
                additional_reviews: 1,
                additional_windows: 1,
                max_window_ms: 180_000,
                max_call_duration_ms: 1_000,
                dispatch_margin_ms: crate::ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
                expires_at_unix_ms: NOW + 3_600_000,
            },
            reason_ref: "review-budget-regression".into(),
        };
        let mut issuer = principal.clone();
        issuer.kind = CompanyPrincipalKindV1::Operator;
        issuer.agent_id = None;
        let receipt = AdaptiveWorkFundingReceiptV1 {
            schema_version: 1,
            funding_id: adaptive_work_funding_id(
                &tenant,
                request.source.resume_source.session_id,
                request.operation_id,
            )
            .unwrap(),
            request,
            issuer_principal: issuer,
            issued_at_unix_ms: NOW,
        };
        let epoch = AdaptiveWorkFundingEpochV1 {
            binding: receipt.binding(1).unwrap(),
            receipt,
        };
        let fingerprint = "a".repeat(64);
        let grant = AdaptiveLeadershipReviewGrantV1 {
            schema_version: 5,
            review_id: crate::adaptive_leadership_review_id(
                epoch.receipt.request.source.resume_source.session_id,
                1,
                &fingerprint,
            )
            .unwrap(),
            project_id: project,
            expected_project_version: 1,
            work_item_id: work.clone(),
            session_id: epoch.receipt.request.source.resume_source.session_id,
            expected_session_version: 1,
            expected_reason_code: String::new(),
            evidence_fingerprint: fingerprint,
            leadership_principal: principal,
            leadership_authority: leader,
            assignment_id: "review-budget-assignment".into(),
            assignee_authority: authority,
            provider: "codex-cli".into(),
            model: "review-budget-model".into(),
            catalog_digest: "a".repeat(64),
            max_duration_ms: 1_000,
            token_policy: crate::SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,
            expires_at_unix_ms: NOW + 60_000,
            subject: Some(
                crate::AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted {
                    budget: Box::new(crate::AdaptiveBudgetWindowAuthorityV1 {
                        schema_version: 1,
                        root_allowance: crate::SubscriptionCallAllowanceV1 {
                            allowance_id: "review-budget-root".into(),
                            grant: crate::SubscriptionCallGrantV1 {
                                schema_version: 1,
                                work_item_id: work,
                                assignment_id: "review-budget-assignment".into(),
                                assignment_version: 1,
                                agent_id: crate::AgentId(2),
                                provider: "codex-cli".into(),
                                model: "review-budget-model".into(),
                                catalog_digest: "a".repeat(64),
                                max_calls: 2,
                                max_concurrent: 1,
                                max_duration_ms: 1_000,
                                token_policy:
                                    crate::SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,
                                expires_at_unix_ms: NOW + 60_000,
                            },
                            created_by: "review-budget-leader".into(),
                            created_at_unix_ms: NOW,
                            dispatch: None,
                        },
                        active_allowance_digest: "a".repeat(64),
                        continuation_history_digest: "a".repeat(64),
                        observed_at_ms: NOW,
                        model_calls_exhausted: true,
                        deadline_expired: false,
                        dispatch_slack_insufficient: false,
                    }),
                },
            ),
            recovery_epoch: None,
            funded_review_recovery: None,
            resume_policy: None,
            work_funding: Some(Box::new(epoch)),
        };
        let leaf = FundingReviewLeaf {
            schema_version: 1,
            membership_id: review_id(grant.work_funding.as_ref().unwrap()),
            grant,
            operation_id: Uuid::from_u128(9203),
            context_digest: "a".repeat(64),
            issued_at_unix_ms: NOW + 1,
        };
        leaf.validate_entity().unwrap();
        leaf
    }

    fn store_review(connection: &mut Connection, leaf: &FundingReviewLeaf) {
        let transaction = connection.transaction().unwrap();
        let tenant = &leaf.grant.leadership_principal.tenant_id;
        put_entity(
            &transaction,
            tenant,
            REVIEW_KIND,
            &leaf.membership_id,
            1,
            leaf,
        )
        .unwrap();
        append_event(
            &transaction,
            &leaf.grant.leadership_principal,
            leaf.operation_id,
            &canonical_sha256("sentinel.workflow.adaptive-work-funding-review.v1", leaf).unwrap(),
            Some(&leaf.grant.project_id),
            REVIEW_EVENT,
            leaf,
            leaf.issued_at_unix_ms,
        )
        .unwrap();
        transaction.commit().unwrap();
    }

    #[test]
    fn selected_review_payload_budget_rejects_before_owned_copy() {
        for event in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let store = WorkflowStore::open(directory.path().join("review-budget.sqlite")).unwrap();
            let mut connection = store.connection.lock().unwrap();
            let leaf = review_leaf();
            let tenant = &leaf.grant.leadership_principal.tenant_id;
            store_review(&mut connection, &leaf);
            if event {
                connection
                    .execute(
                        "DELETE FROM company_entities WHERE entity_kind=?1",
                        [REVIEW_KIND],
                    )
                    .unwrap();
                connection
                    .execute(
                        "UPDATE company_events SET payload=zeroblob(?1) WHERE event_type=?2",
                        params![ARENA_BYTES as i64 + 1, REVIEW_EVENT],
                    )
                    .unwrap();
            } else {
                connection
                    .execute(
                        "UPDATE company_entities SET payload=zeroblob(?1) WHERE entity_kind=?2",
                        params![ARENA_BYTES as i64 + 1, REVIEW_KIND],
                    )
                    .unwrap();
            }
            let mut copies = 0;
            let error = reviewed_funding_ids_with_observer(&connection, tenant, |_, _| {
                copies += 1;
            })
            .unwrap_err();
            assert_eq!(error.code, WorkflowErrorCode::CorruptStore);
            assert_eq!(copies, 0, "event={event}");
            assert!(connection.is_autocommit());
        }
    }

    #[test]
    fn selected_review_payloads_are_charged_once_and_retain_the_veto() {
        let directory = tempfile::tempdir().unwrap();
        let store = WorkflowStore::open(directory.path().join("review-veto.sqlite")).unwrap();
        let mut connection = store.connection.lock().unwrap();
        let leaf = review_leaf();
        let tenant = &leaf.grant.leadership_principal.tenant_id;
        store_review(&mut connection, &leaf);
        let receipt = &leaf.grant.work_funding.as_ref().unwrap().receipt;
        let expected = BTreeSet::from([receipt.funding_id.clone()]);
        let payload_bytes = encode(&leaf).unwrap().len();
        let proof_bytes = serde_json::to_vec(tenant).unwrap().len()
            + 256
            + serde_json::to_vec(&expected).unwrap().len();
        let sequence: i64 = connection
            .query_row(
                "SELECT sequence FROM company_events WHERE event_type=?1 AND operation_id=?2",
                params![REVIEW_EVENT, leaf.operation_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        let sequence = stored_u64(sequence).unwrap();
        let event = read_company_event_row(&connection, sequence)
            .unwrap()
            .unwrap();
        let owner_bytes = encode(&sequence).unwrap().len() + 256 + encode(&event).unwrap().len();
        let entity_owner_bytes = encode(&(tenant, REVIEW_KIND, &leaf.membership_id))
            .unwrap()
            .len()
            + 256
            + encode(&Some(&leaf)).unwrap().len();
        validation_scope::with_scope(&connection, || {
            validation_scope::charge_bytes(
                &connection,
                ARENA_BYTES - proof_bytes - 2 * payload_bytes - owner_bytes - entity_owner_bytes,
            )?;
            let mut copies = Vec::new();
            let used = reviewed_funding_ids_with_observer(&connection, tenant, |kind, bytes| {
                copies.push((kind, bytes));
            })?;
            assert_eq!(used, expected);
            assert_eq!(
                copies,
                vec![("entity", payload_bytes), ("event", payload_bytes)]
            );
            let mut extra_copies = 0;
            let retained: FundingReviewLeaf = entity_shape_with_observer(
                &connection,
                tenant,
                REVIEW_KIND,
                &leaf.membership_id,
                |_| extra_copies += 1,
            )?
            .ok_or_else(corrupt)?;
            assert!(retained == leaf);
            assert_eq!(extra_copies, 0);
            assert_eq!(
                require_never_reviewed_funding(&connection, receipt)
                    .unwrap_err()
                    .code,
                transition().code
            );
            Ok(())
        })
        .unwrap();
        assert!(connection.is_autocommit());
    }

    #[test]
    fn selected_review_overflow_rejects_before_copying_a_valid_first_match() {
        let directory = tempfile::tempdir().unwrap();
        let store = WorkflowStore::open(directory.path().join("review-overflow.sqlite")).unwrap();
        let mut connection = store.connection.lock().unwrap();
        let leaf = review_leaf();
        let tenant = &leaf.grant.leadership_principal.tenant_id;
        store_review(&mut connection, &leaf);
        connection.execute(
            "WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<4096)
             INSERT INTO company_entities(tenant_id,entity_kind,entity_id,version,payload,payload_digest)
             SELECT ?1,?2,'zz-overflow-'||i,1,X'00','invalid' FROM n",
            params![tenant.0, REVIEW_KIND],
        ).unwrap();
        let mut copies = 0;
        assert_eq!(
            reviewed_funding_ids_with_observer(&connection, tenant, |_, _| {
                copies += 1;
            })
            .unwrap_err()
            .code,
            WorkflowErrorCode::CorruptStore
        );
        assert_eq!(copies, 0);
        assert!(connection.is_autocommit());
    }

    mod entity_owner_reuse_tests {
        use super::*;

        #[test]
        fn scan_first_and_owner_first_share_shapes_and_reject_changed_checksum() {
            for owner_first in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let store =
                    WorkflowStore::open(directory.path().join("shared-review.sqlite")).unwrap();
                let mut connection = store.connection.lock().unwrap();
                let leaf = review_leaf();
                let tenant = &leaf.grant.leadership_principal.tenant_id;
                store_review(&mut connection, &leaf);
                let expected = BTreeSet::from([leaf
                    .grant
                    .work_funding
                    .as_ref()
                    .unwrap()
                    .binding
                    .funding_id
                    .clone()]);
                let payload_bytes = encode(&leaf).unwrap().len();
                validation_scope::with_scope(&connection, || {
                    let mut owner_copies = 0;
                    if owner_first {
                        let owner: FundingReviewLeaf =
                            entity_shape(&connection, tenant, REVIEW_KIND, &leaf.membership_id)?
                                .ok_or_else(corrupt)?;
                        owner.validate_entity()?;
                    }
                    let mut copies = Vec::new();
                    let used =
                        reviewed_funding_ids_with_observer(&connection, tenant, |kind, bytes| {
                            copies.push((kind, bytes))
                        })?;
                    assert_eq!(used, expected);
                    let expected_copies = if owner_first {
                        vec![("event", payload_bytes)]
                    } else {
                        vec![("entity", payload_bytes), ("event", payload_bytes)]
                    };
                    assert_eq!(copies, expected_copies);
                    assert_eq!(validation_scope::validations("entity-shape"), 1);
                    let owner: FundingReviewLeaf = entity_shape_with_observer(
                        &connection,
                        tenant,
                        REVIEW_KIND,
                        &leaf.membership_id,
                        |_| owner_copies += 1,
                    )?
                    .ok_or_else(corrupt)?;
                    assert!(owner == leaf);
                    assert_eq!(owner_copies, 0);
                    assert_eq!(
                        reviewed_funding_ids_with_observer(&connection, tenant, |kind, bytes| {
                            copies.push((kind, bytes));
                        })?,
                        expected
                    );
                    assert_eq!(copies, expected_copies);
                    connection.execute(
                        "UPDATE company_entities SET payload_digest='invalid'
                         WHERE tenant_id=?1 AND entity_kind=?2 AND entity_id=?3",
                        params![tenant.0, REVIEW_KIND, leaf.membership_id],
                    )?;
                    let mut invalidated_copies = Vec::new();
                    let error =
                        reviewed_funding_ids_with_observer(&connection, tenant, |kind, bytes| {
                            invalidated_copies.push((kind, bytes))
                        })
                        .unwrap_err();
                    assert_eq!(error.code, WorkflowErrorCode::CorruptStore);
                    assert_eq!(invalidated_copies, vec![("entity", payload_bytes)]);
                    Ok(())
                })
                .unwrap();
                assert!(connection.is_autocommit());
            }
        }
    }
}
