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
    validation_scope::with_scope(connection, || {
        let ids = funding_inventory(connection, tenant)?;
        let mut receipts = Vec::new();
        for id in ids {
            let receipt: AdaptiveWorkFundingReceiptV1 =
                get_entity(connection, tenant, FUNDING_KIND, &id)?.ok_or_else(corrupt)?;
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
    // Retain only validated keys; typed payload proofs already belong to this scope.
    // Validate every row and issuance event before any session selection.
    for id in &ids {
        let _: AdaptiveWorkFundingReceiptV1 =
            get_entity(connection, tenant, FUNDING_KIND, id)?.ok_or_else(corrupt)?;
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
                "SELECT entity_kind,entity_id,version,payload,payload_digest FROM company_entities
                 WHERE tenant_id=?1 AND entity_kind IN
                 ('adaptive_work_funding_review','adaptive_leadership_review_call')
                 ORDER BY entity_kind,entity_id LIMIT 4097",
            )?;
            let mut rows = statement.query([&tenant.0])?;
            while let Some(row) = rows.next()? {
                let kind: String = row.get(0)?;
                let id: String = row.get(1)?;
                let version: i64 = row.get(2)?;
                let value = row.get_ref(3)?;
                let rusqlite::types::ValueRef::Blob(payload) = value else {
                    return Err(rusqlite::Error::InvalidColumnType(
                        3,
                        "payload".into(),
                        value.data_type(),
                    )
                    .into());
                };
                let digest: String = row.get(4)?;
                // Charge the borrowed BLOB before any Rust-owned payload copy.
                validation_scope::charge_bytes(connection, payload.len())?;
                before_payload_copy("entity", payload.len());
                let payload = payload.to_vec();
                if !constant_time_eq(
                    &digest,
                    &bytes_digest("sentinel.workflow.company-entity-row.v1", &payload)?,
                ) {
                    return Err(corrupt());
                }
                let funding_id = if kind == REVIEW_KIND {
                    let leaf: FundingReviewLeaf = decode(&payload)?;
                    leaf.validate_entity()?;
                    if leaf.row_binding()
                        != (tenant, kind.as_str(), id.as_str(), stored_u64(version)?)
                    {
                        return Err(corrupt());
                    }
                    Some(
                        require_review_binding(&leaf.grant)?
                            .binding
                            .funding_id
                            .clone(),
                    )
                } else {
                    let call: AdaptiveLeadershipReviewCallV1 = decode(&payload)?;
                    call.validate_entity()?;
                    if call.row_binding()
                        != (tenant, kind.as_str(), id.as_str(), stored_u64(version)?)
                    {
                        return Err(corrupt());
                    }
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
            let payload_bytes = usize::try_from(payload_bytes).map_err(|_| corrupt())?;
            validation_scope::charge_bytes(connection, payload_bytes)?;
            before_payload_copy("event", payload_bytes);
            let row =
                read_company_event_row(connection, stored_u64(sequence)?)?.ok_or_else(corrupt)?;
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
            let receipts = funding_receipts(connection, tenant, session_id)?;
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
    let superseded = superseded_validated_receipts(connection, tenant, session_id, &receipts)?;
    let pending = receipts
        .iter()
        .filter(|receipt| {
            !adopted.iter().any(|epoch| epoch.receipt == **receipt)
                && !superseded.contains(&receipt.funding_id)
        })
        .collect::<Vec<_>>();
    match (pending.as_slice(), supersedes_unused_receipt_digest) {
        ([], None) => {}
        ([old], Some(digest))
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
        supersedes_unused_receipt_digest: supersedes_unused_receipt_digest.map(str::to_owned),
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
    let superseded = superseded_validated_receipts(connection, tenant, session_id, &receipts)?;
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
        let superseded =
            superseded_validated_receipts(&transaction, tenant, session_id, &receipts)?;
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
            request.source.supersedes_unused_receipt_digest.as_deref(),
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

    fn review_leaf() -> FundingReviewLeaf {
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
        validation_scope::with_scope(&connection, || {
            validation_scope::charge_bytes(
                &connection,
                ARENA_BYTES - proof_bytes - 2 * payload_bytes,
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
}
