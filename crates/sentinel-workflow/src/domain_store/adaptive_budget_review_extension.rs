use super::*;

const EXTENSION_KIND: &str = "adaptive_budget_review_extension";
const EXTENSION_EVENT: &str = "adaptive_budget_review_extension_authorized";
const MAX_EXTENSION_MS: u64 = 24 * 60 * 60 * 1_000;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveBudgetReviewExtensionRequestV1 {
    pub schema_version: u16,
    pub operation_id: Uuid,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub session_id: Uuid,
    pub expected_session_version: u64,
    pub budget_limit_receipt_digest: String,
    pub source_digest: String,
    pub base_global_review_count: usize,
    pub base_head_review_count: usize,
    pub additional_reviews: u16,
    pub reason_ref: String,
    pub expires_at_unix_ms: u64,
}

impl AdaptiveBudgetReviewExtensionRequestV1 {
    fn validate(&self, issued_at_ms: u64) -> Result<(), WorkflowError> {
        self.tenant_id.validate()?;
        self.project_id.validate()?;
        validate_digest(&self.budget_limit_receipt_digest)?;
        validate_digest(&self.source_digest)?;
        validate_identifier(&self.reason_ref)?;
        if self.schema_version != 1
            || self.operation_id.is_nil()
            || self.session_id.is_nil()
            || self.expected_session_version == 0
            || !(1..=3).contains(&self.additional_reviews)
            || self.base_global_review_count > MAX_AGGREGATE_ITEMS
            || self.base_head_review_count > MAX_AGGREGATE_ITEMS
            || issued_at_ms == 0
            || self.expires_at_unix_ms > i64::MAX as u64
            || self
                .expires_at_unix_ms
                .checked_sub(issued_at_ms)
                .is_none_or(|duration| !(1_000..=MAX_EXTENSION_MS).contains(&duration))
        {
            return Err(invalid("invalid bounded budget review extension"));
        }
        Ok(())
    }

    pub fn canonical_digest(&self) -> Result<String, WorkflowError> {
        canonical_sha256(
            "sentinel.workflow.adaptive-budget-review-extension.v1",
            self,
        )
    }
}

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveBudgetReviewExtensionReceiptV1 {
    pub schema_version: u16,
    pub request: AdaptiveBudgetReviewExtensionRequestV1,
    pub issuer_principal: AuthenticatedCompanyPrincipalV1,
    pub issued_at_unix_ms: u64,
    extension_key: String,
    budget_limit_receipt: AdaptiveBudgetWindowLimitReceiptV1,
    prior_reviews: Vec<AdaptiveLeadershipReviewCallV1>,
}

fn conflict() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::IdempotencyConflict,
        false,
        "budget review extension is immutable for this head",
    )
}

fn extension_key(tenant: &TenantId, session: Uuid, head: u64) -> Result<String, WorkflowError> {
    tenant.validate()?;
    if session.is_nil() || head == 0 {
        return Err(invalid("invalid budget review extension head"));
    }
    Ok(format!(
        "budget-review-extension-{}",
        canonical_sha256(
            "sentinel.workflow.adaptive-budget-review-extension-key.v1",
            &(tenant, session, head),
        )?
    ))
}

fn require_operator(
    operator: &AuthenticatedCompanyPrincipalV1,
    tenant: &TenantId,
) -> Result<(), WorkflowError> {
    operator.validate()?;
    if operator.kind != CompanyPrincipalKindV1::Operator
        || !matches!(
            operator.role,
            CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
        )
        || &operator.tenant_id != tenant
    {
        return Err(unauthorized());
    }
    Ok(())
}

fn review_counts(calls: &[AdaptiveLeadershipReviewCallV1], head: u64) -> (usize, usize) {
    (
        calls
            .iter()
            .filter(|call| call.grant.schema_version == 3)
            .count(),
        calls
            .iter()
            .filter(|call| call.grant.expected_session_version == head)
            .count(),
    )
}

fn require_review_only_limit(
    limit: &AdaptiveBudgetWindowLimitReceiptV1,
) -> Result<(), WorkflowError> {
    if limit.causes.is_empty()
        || limit.causes.iter().any(|cause| {
            !matches!(
                cause,
                AdaptiveBudgetWindowLimitCauseV1::ReviewLimit
                    | AdaptiveBudgetWindowLimitCauseV1::HeadReviewLimit
            )
        })
        || limit.context.source_session.model_calls
            >= limit.context.source_session.grant.max_model_calls
        || limit
            .context
            .source_session
            .continuation
            .as_ref()
            .is_some_and(|state| {
                state.authorizations.len() >= crate::adaptive::ADAPTIVE_CONTINUATION_MAX_WINDOWS
            })
    {
        return Err(transition());
    }
    Ok(())
}

fn limit_call(limit: &AdaptiveBudgetWindowLimitReceiptV1) -> AdaptiveLeadershipReviewCallV1 {
    AdaptiveLeadershipReviewCallV1 {
        schema_version: 3,
        review_key: limit.grant.review_id.to_string(),
        allowance_id: "budget-limit-no-provider".into(),
        operation_id: limit.grant.review_id,
        grant: limit.grant.clone(),
        context: limit.context.clone(),
        version: 1,
        created_at_unix_ms: limit.recorded_at_ms,
        grant_issued_at_unix_ms: limit.recorded_at_ms,
        updated_at_unix_ms: limit.recorded_at_ms,
        dispatch: None,
        decision: None,
        model_response_digest: None,
        resolution_event_id: None,
        retired_at_unix_ms: None,
        continuation: None,
    }
}

fn require_issuance_source(
    connection: &Connection,
    limit: &AdaptiveBudgetWindowLimitReceiptV1,
    now: u64,
) -> Result<Vec<AdaptiveLeadershipReviewCallV1>, WorkflowError> {
    require_review_only_limit(limit)?;
    require_current_source(connection, &limit_call(limit))?;
    let calls = calls_for_session(
        connection,
        &limit.grant.leadership_principal.tenant_id,
        limit.grant.session_id,
    )?;
    if now < limit.recorded_at_ms
        || now < limit.context.source_project.updated_at_unix_ms
        || now < limit.context.source_session.updated_at_ms
        || calls.iter().any(|call| {
            now < call.updated_at_unix_ms
                || (call.decision.is_none() && call.retired_at_unix_ms.is_none())
        })
    {
        return Err(transition());
    }
    Ok(calls)
}

impl CompanyEntity for AdaptiveBudgetReviewExtensionReceiptV1 {
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        (
            &self.request.tenant_id,
            EXTENSION_KIND,
            &self.extension_key,
            1,
        )
    }

    fn validate_entity(&self) -> Result<(), WorkflowError> {
        self.request.validate(self.issued_at_unix_ms)?;
        require_operator(&self.issuer_principal, &self.request.tenant_id)?;
        let limit = &self.budget_limit_receipt;
        limit.validate_entity()?;
        require_review_only_limit(limit)?;
        if self.schema_version != 1
            || self.extension_key
                != extension_key(
                    &self.request.tenant_id,
                    self.request.session_id,
                    self.request.expected_session_version,
                )?
            || self.request.tenant_id != limit.grant.leadership_principal.tenant_id
            || self.request.project_id != limit.grant.project_id
            || self.request.session_id != limit.grant.session_id
            || self.request.expected_session_version != limit.grant.expected_session_version
            || self.request.source_digest != limit.source_digest
            || self.request.budget_limit_receipt_digest
                != canonical_sha256("sentinel.workflow.adaptive-budget-limit.v1", limit)?
            || self.issued_at_unix_ms < limit.recorded_at_ms
            || self.prior_reviews.len() > MAX_AGGREGATE_ITEMS
            || self
                .prior_reviews
                .windows(2)
                .any(|pair| pair[0].grant.review_id >= pair[1].grant.review_id)
            || review_counts(&self.prior_reviews, self.request.expected_session_version)
                != (
                    self.request.base_global_review_count,
                    self.request.base_head_review_count,
                )
        {
            return Err(corrupt());
        }
        for call in &self.prior_reviews {
            call.validate_entity()?;
            if call.grant.leadership_principal.tenant_id != self.request.tenant_id
                || call.grant.project_id != self.request.project_id
                || call.grant.session_id != self.request.session_id
                || call.updated_at_unix_ms > self.issued_at_unix_ms
                || (call.decision.is_none() && call.retired_at_unix_ms.is_none())
            {
                return Err(corrupt());
            }
        }
        let (global, head) =
            review_counts(&self.prior_reviews, self.request.expected_session_version);
        if limit.causes.iter().any(|cause| match cause {
            AdaptiveBudgetWindowLimitCauseV1::ReviewLimit => {
                global < ADAPTIVE_LEADERSHIP_MAX_REVIEWS
            }
            AdaptiveBudgetWindowLimitCauseV1::HeadReviewLimit => {
                head < ADAPTIVE_LEADERSHIP_MAX_REVIEWS
            }
            _ => true,
        }) {
            return Err(corrupt());
        }
        Ok(())
    }

    fn validate_persisted(&self, connection: &Connection) -> Result<(), WorkflowError> {
        let limit: AdaptiveBudgetWindowLimitReceiptV1 = get_entity(
            connection,
            &self.request.tenant_id,
            BUDGET_LIMIT_KIND,
            &self.budget_limit_receipt.receipt_id,
        )?
        .ok_or_else(corrupt)?;
        if limit != self.budget_limit_receipt {
            return Err(corrupt());
        }
        for prior in &self.prior_reviews {
            let current: AdaptiveLeadershipReviewCallV1 =
                get_entity(connection, &self.request.tenant_id, KIND, &prior.review_key)?
                    .ok_or_else(corrupt)?;
            if current != *prior {
                return Err(corrupt());
            }
        }
        let mut statement = connection.prepare(
            "SELECT payload,payload_digest,operation_digest,authority_binding_digest,created_at_ms
             FROM company_events WHERE tenant_id=?1 AND event_type=?2 AND operation_id=?3 LIMIT 2",
        )?;
        let records = statement
            .query_map(
                params![
                    self.request.tenant_id.0,
                    EXTENSION_EVENT,
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

pub(super) fn limits_in_connection(
    connection: &Connection,
    tenant: &TenantId,
    session_id: Uuid,
    head: u64,
    now: u64,
) -> Result<(usize, usize, Option<u64>), WorkflowError> {
    let baseline = (
        ADAPTIVE_LEADERSHIP_MAX_REVIEWS,
        ADAPTIVE_LEADERSHIP_MAX_REVIEWS,
        None,
    );
    let key = extension_key(tenant, session_id, head)?;
    let Some(receipt) = get_entity::<AdaptiveBudgetReviewExtensionReceiptV1>(
        connection,
        tenant,
        EXTENSION_KIND,
        &key,
    )?
    else {
        return Ok(baseline);
    };
    if now < receipt.issued_at_unix_ms || now >= receipt.request.expires_at_unix_ms {
        return Ok(baseline);
    }
    let Some((current, _)) = crate::store::adaptive::load(connection, session_id)? else {
        return Ok(baseline);
    };
    crate::store::adaptive::require_head(connection, &current)?;
    let project: Option<ProjectV1> =
        get_entity(connection, tenant, "project", &receipt.request.project_id.0)?;
    if current != receipt.budget_limit_receipt.context.source_session
        || project.as_ref() != Some(&receipt.budget_limit_receipt.context.source_project)
    {
        return Ok(baseline);
    }
    require_current_source(connection, &limit_call(&receipt.budget_limit_receipt))?;
    let extra = usize::from(receipt.request.additional_reviews);
    Ok((
        receipt.request.base_global_review_count + extra,
        receipt.request.base_head_review_count + extra,
        Some(receipt.request.expires_at_unix_ms),
    ))
}

pub(super) fn limits_for_review(
    connection: &Connection,
    grant: &AdaptiveLeadershipReviewGrantV1,
    context: &AdaptiveLeadershipReviewContextV1,
    now: u64,
) -> Result<(usize, usize, Option<u64>), WorkflowError> {
    let limits = limits_in_connection(
        connection,
        &grant.leadership_principal.tenant_id,
        grant.session_id,
        grant.expected_session_version,
        now,
    )?;
    if limits.2.is_some() {
        let receipt: AdaptiveBudgetReviewExtensionReceiptV1 = get_entity(
            connection,
            &grant.leadership_principal.tenant_id,
            EXTENSION_KIND,
            &extension_key(
                &grant.leadership_principal.tenant_id,
                grant.session_id,
                grant.expected_session_version,
            )?,
        )?
        .ok_or_else(corrupt)?;
        if budget_limit_source_digest(grant, context)? != receipt.request.source_digest {
            return Err(unauthorized());
        }
    }
    Ok(limits)
}

impl WorkflowStore {
    pub fn budget_review_extension_draft(
        &self,
        operator: &AuthenticatedCompanyPrincipalV1,
        project_id: &ProjectId,
        session_id: Uuid,
        operation_id: Uuid,
        additional_reviews: u16,
        reason_ref: &str,
        expires_at: u64,
        now: u64,
    ) -> Result<AdaptiveBudgetReviewExtensionRequestV1, WorkflowError> {
        require_operator(operator, &operator.tenant_id)?;
        project_id.validate()?;
        if session_id.is_nil() {
            return Err(invalid("invalid extension session"));
        }
        let connection = self.connection.lock().map_err(|_| persistence())?;
        let (source, _) =
            crate::store::adaptive::load(&connection, session_id)?.ok_or_else(not_found)?;
        if source.grant.authority.tenant_id != operator.tenant_id
            || source.grant.authority.project_id != *project_id
        {
            return Err(unauthorized());
        }
        let limit: AdaptiveBudgetWindowLimitReceiptV1 = get_entity(
            &connection,
            &operator.tenant_id,
            BUDGET_LIMIT_KIND,
            &budget_limit_id(session_id, source.version)?,
        )?
        .ok_or_else(not_found)?;
        let calls = require_issuance_source(&connection, &limit, now)?;
        let (global, head) = review_counts(&calls, source.version);
        let request = AdaptiveBudgetReviewExtensionRequestV1 {
            schema_version: 1,
            operation_id,
            tenant_id: operator.tenant_id.clone(),
            project_id: project_id.clone(),
            session_id,
            expected_session_version: source.version,
            budget_limit_receipt_digest: canonical_sha256(
                "sentinel.workflow.adaptive-budget-limit.v1",
                &limit,
            )?,
            source_digest: limit.source_digest,
            base_global_review_count: global,
            base_head_review_count: head,
            additional_reviews,
            reason_ref: reason_ref.into(),
            expires_at_unix_ms: expires_at,
        };
        request.validate(now)?;
        Ok(request)
    }

    pub fn budget_review_extension(
        &self,
        tenant: &TenantId,
        session_id: Uuid,
        head_version: u64,
    ) -> Result<Option<AdaptiveBudgetReviewExtensionReceiptV1>, WorkflowError> {
        let key = extension_key(tenant, session_id, head_version)?;
        let connection = self.connection.lock().map_err(|_| persistence())?;
        get_entity(&connection, tenant, EXTENSION_KIND, &key)
    }

    pub fn authorize_budget_review_extension(
        &self,
        operator: &AuthenticatedCompanyPrincipalV1,
        request: &AdaptiveBudgetReviewExtensionRequestV1,
        now: u64,
    ) -> Result<(bool, AdaptiveBudgetReviewExtensionReceiptV1), WorkflowError> {
        require_operator(operator, &request.tenant_id)?;
        let key = extension_key(
            &request.tenant_id,
            request.session_id,
            request.expected_session_version,
        )?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(prior) = get_entity::<AdaptiveBudgetReviewExtensionReceiptV1>(
            &transaction,
            &request.tenant_id,
            EXTENSION_KIND,
            &key,
        )? {
            if prior.request != *request || prior.issuer_principal != *operator {
                return Err(conflict());
            }
            return Ok((true, prior));
        }
        request.validate(now)?;
        let operation_exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM company_events WHERE tenant_id=?1 AND event_type=?2 AND operation_id=?3)",
            params![request.tenant_id.0, EXTENSION_EVENT, request.operation_id.to_string()], |row| row.get(0),
        )?;
        if operation_exists {
            return Err(conflict());
        }
        let limit: AdaptiveBudgetWindowLimitReceiptV1 = get_entity(
            &transaction,
            &request.tenant_id,
            BUDGET_LIMIT_KIND,
            &budget_limit_id(request.session_id, request.expected_session_version)?,
        )?
        .ok_or_else(not_found)?;
        let prior_reviews = require_issuance_source(&transaction, &limit, now)?;
        let receipt = AdaptiveBudgetReviewExtensionReceiptV1 {
            schema_version: 1,
            request: request.clone(),
            issuer_principal: operator.clone(),
            issued_at_unix_ms: now,
            extension_key: key,
            budget_limit_receipt: limit,
            prior_reviews,
        };
        receipt.validate_entity()?;
        put_entity(
            &transaction,
            &request.tenant_id,
            EXTENSION_KIND,
            &receipt.extension_key,
            1,
            &receipt,
        )?;
        append_event(
            &transaction,
            operator,
            request.operation_id,
            &request.canonical_digest()?,
            Some(&request.project_id),
            EXTENSION_EVENT,
            &receipt,
            now,
        )?;
        transaction.commit()?;
        Ok((false, receipt))
    }

    pub fn budget_review_limits(
        &self,
        tenant: &TenantId,
        session_id: Uuid,
        head_version: u64,
        now: u64,
    ) -> Result<(usize, usize, Option<u64>), WorkflowError> {
        let connection = self.connection.lock().map_err(|_| persistence())?;
        limits_in_connection(&connection, tenant, session_id, head_version, now)
    }
}
