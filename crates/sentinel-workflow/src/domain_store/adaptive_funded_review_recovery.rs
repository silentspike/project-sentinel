use super::*;
use crate::{
    AdaptiveCursorV1, AdaptiveFundedReviewNoIoV1, AdaptiveFundedReviewRecoveryEvidenceV1,
    AdaptiveFundedReviewRecoveryMemberV1, AdaptiveFundedReviewRecoveryReceiptV1,
    AdaptiveFundedReviewRecoveryRequestV1, AdaptiveFundedReviewRecoverySourceV1,
    AdaptiveFundedReviewRecoveryVerificationSourceV1, AdaptiveLeadershipReviewCallV1,
    AdaptiveLeadershipReviewContextV1, AdaptiveLeadershipReviewGrantV1,
};

const KIND: &str = "adaptive_funded_review_recovery";
const EVENT: &str = "adaptive_funded_review_recovery_issued";

fn review_members(
    calls: &[AdaptiveLeadershipReviewCallV1],
) -> Result<Vec<AdaptiveFundedReviewRecoveryMemberV1>, WorkflowError> {
    calls
        .iter()
        .map(|call| {
            Ok(AdaptiveFundedReviewRecoveryMemberV1 {
                review_id: call.grant.review_id,
                version: call.version,
                digest: canonical_sha256("sentinel.workflow.adaptive-leadership-call.v1", call)?,
            })
        })
        .collect()
}

fn history_digest(
    members: &[AdaptiveFundedReviewRecoveryMemberV1],
) -> Result<String, WorkflowError> {
    canonical_sha256(
        "sentinel.workflow.funded-review-recovery-members.v1",
        &members,
    )
}

fn load_member(
    connection: &Connection,
    tenant: &TenantId,
    session_id: Uuid,
    member: &AdaptiveFundedReviewRecoveryMemberV1,
) -> Result<AdaptiveLeadershipReviewCallV1, WorkflowError> {
    let stored: AdaptiveLeadershipReviewCallV1 = get_entity(
        connection,
        tenant,
        "adaptive_leadership_review_call",
        &member.review_id.to_string(),
    )?
    .ok_or_else(corrupt)?;
    if stored.version != member.version
        || canonical_sha256("sentinel.workflow.adaptive-leadership-call.v1", &stored)?
            != member.digest
        || stored.grant.session_id != session_id
    {
        return Err(corrupt());
    }
    Ok(stored)
}

fn receipt_in_connection(
    connection: &Connection,
    tenant: &TenantId,
    receipt_key: &str,
) -> Result<Option<AdaptiveFundedReviewRecoveryReceiptV1>, WorkflowError> {
    let receipt =
        get_entity::<AdaptiveFundedReviewRecoveryReceiptV1>(connection, tenant, KIND, receipt_key)?;
    if let Some(receipt) = &receipt {
        let _: AdaptiveLeadershipReviewCallV1 = get_entity(
            connection,
            tenant,
            "adaptive_leadership_review_call",
            &receipt.grant_template.review_id.to_string(),
        )?
        .ok_or_else(corrupt)?;
    } else {
        let orphan: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM company_events WHERE tenant_id=?1 AND event_type=?2
             AND json_extract(payload,'$.receipt_key')=?3)",
            params![tenant.0, EVENT, receipt_key],
            |row| row.get(0),
        )?;
        if orphan {
            return Err(corrupt());
        }
    }
    Ok(receipt)
}

fn source_in_connection(
    connection: &Connection,
    operator: &AuthenticatedCompanyPrincipalV1,
    project_id: &ProjectId,
    session_id: Uuid,
    retired_review_id: Uuid,
    now_ms: u64,
) -> Result<AdaptiveFundedReviewRecoveryVerificationSourceV1, WorkflowError> {
    crate::adaptive_resume_policy::require_resume_policy_operator(operator, &operator.tenant_id)?;
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
    let old = calls
        .iter()
        .find(|call| call.grant.review_id == retired_review_id)
        .ok_or_else(not_found)?;
    let epoch = old.grant.work_funding.as_ref().ok_or_else(transition)?;
    adaptive_work_funding::require_epoch_receipt(connection, epoch)?;
    adaptive_work_funding::require_never_dispatched_funding(connection, epoch)?;
    let funding = &epoch.receipt;
    let anchor = &funding.request.source.resume_source;
    let ordinal = fresh
        .base_review_count
        .checked_add(1)
        .ok_or_else(transition)?;
    if old.grant.schema_version != 5
        || old.grant.funded_review_recovery.is_some()
        || old.context.source_project != project
        || old.context.source_session != session
        || old
            .retired_at_unix_ms
            .is_none_or(|at| at < old.grant.expires_at_unix_ms || at > now_ms)
        || now_ms < old.grant.expires_at_unix_ms
        || now_ms
            .checked_add(funding.request.limits.max_call_duration_ms)
            .and_then(|time| time.checked_add(funding.request.limits.dispatch_margin_ms))
            .is_none_or(|time| time > funding.request.limits.expires_at_unix_ms)
        || ordinal > epoch.binding.limits.total_review_ceiling
        || session.model_calls >= epoch.binding.limits.total_model_call_ceiling
        || session.tool_calls >= epoch.binding.limits.total_tool_call_ceiling
        || fresh.base_window_count >= epoch.binding.limits.total_window_ceiling
        || session.continuation.as_ref().is_some_and(|state| {
            state.authorizations.iter().any(|auth| {
                auth.work_funding
                    .as_ref()
                    .is_some_and(|prior| prior.same_epoch(epoch))
            })
        })
        || calls.iter().any(|call| {
            call.grant.work_funding.as_ref().is_some_and(|prior| {
                prior.same_epoch(epoch)
                    && (call.grant.schema_version != 5
                        || call.dispatch.is_some()
                        || call.decision.is_some()
                        || call.model_response_digest.is_some()
                        || call.continuation.is_some()
                        || call.resolution_event_id.is_some()
                        || call.retired_at_unix_ms.is_none())
            })
        })
        || anchor.expected_session_version != session.version
        || anchor.expected_project_version != project.version
        || anchor.root_entry_digest != fresh.root_entry_digest
        || anchor.head_entry_digest != fresh.head_entry_digest
        || anchor.project_payload_digest != fresh.project_payload_digest
        || anchor.continuation_history_digest != fresh.continuation_history_digest
        || anchor.base_model_calls != session.model_calls
        || anchor.base_tool_calls != session.tool_calls
    {
        return Err(transition());
    }
    let source = AdaptiveFundedReviewRecoverySourceV1 {
        schema_version: 1,
        tenant_id: operator.tenant_id.clone(),
        project_id: project_id.clone(),
        work_item_id: session.grant.authority.work_item_id.clone(),
        session_id,
        expected_project_version: project.version,
        expected_session_version: session.version,
        project_digest: canonical_sha256(
            "sentinel.workflow.funded-review-recovery-project.v1",
            &project,
        )?,
        session_digest: canonical_sha256(
            "sentinel.workflow.funded-review-recovery-session.v1",
            &session,
        )?,
        session_head_digest: fresh.head_entry_digest,
        funding_receipt_digest: funding.receipt_digest()?,
        review_history_digest: history_digest(&review_members(&calls)?)?,
        review_count: fresh.base_review_count,
        retired_review_id,
        ordinal,
    };
    Ok(AdaptiveFundedReviewRecoveryVerificationSourceV1 {
        source,
        project: Box::new(project),
        session: Box::new(session),
        funding: Box::new(funding.clone()),
        calls,
    })
}

impl CompanyEntity for AdaptiveFundedReviewRecoveryReceiptV1 {
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        (&self.issuer_principal.tenant_id, KIND, &self.receipt_key, 1)
    }

    fn validate_entity(&self) -> Result<(), WorkflowError> {
        let request = &self.request;
        let source = &request.source;
        let grant = &self.grant_template;
        let session = &self.context.source_session;
        let project = &self.context.source_project;
        self.issuer_principal.validate()?;
        request.release.validate()?;
        validate_digest(&request.proof_digest)?;
        validate_digest(&request.source_digest)?;
        validate_identifier(&request.reason_ref)?;
        validate_digest(&source.session_head_digest)?;
        grant.validate(self.issued_at_unix_ms)?;
        self.context.validate(grant)?;
        let epoch = grant.work_funding.as_ref().ok_or_else(corrupt)?;
        let anchor = &epoch.receipt.request.source.resume_source;
        let limits = &epoch.binding.limits;
        let mut seen = BTreeSet::new();
        for member in &self.review_members {
            validate_digest(&member.digest)?;
            if member.review_id.is_nil() || member.version == 0 || !seen.insert(member.review_id) {
                return Err(corrupt());
            }
        }
        if self.schema_version != 1
            || request.schema_version != 1
            || source.schema_version != 1
            || request.operation_id.is_nil()
            || self.receipt_key != source.receipt_key()?
            || self.issuer_principal.kind != CompanyPrincipalKindV1::Operator
            || !matches!(
                self.issuer_principal.role,
                CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
            )
            || self.issuer_principal.tenant_id != source.tenant_id
            || source.tenant_id != session.grant.authority.tenant_id
            || source.project_id != project.project_id
            || source.session_id != session.grant.session_id
            || source.work_item_id != session.grant.authority.work_item_id
            || source.expected_project_version != project.version
            || source.expected_session_version != session.version
            || source.project_digest
                != canonical_sha256(
                    "sentinel.workflow.funded-review-recovery-project.v1",
                    project,
                )?
            || source.session_digest
                != canonical_sha256(
                    "sentinel.workflow.funded-review-recovery-session.v1",
                    session,
                )?
            || request.source_digest != source.canonical_digest()?
            || grant.schema_version != 5
            || grant.funded_review_recovery.is_some()
            || grant.review_id == source.retired_review_id
            || grant.expected_session_version != session.version
            || grant.expected_project_version != project.version
            || epoch.receipt.receipt_digest()? != source.funding_receipt_digest
            || epoch.binding.ordinal != source.ordinal
            || grant.expires_at_unix_ms != request.expires_at_unix_ms
            || request.expires_at_unix_ms > limits.expires_at_unix_ms
            || request
                .expires_at_unix_ms
                .checked_sub(self.issued_at_unix_ms)
                .is_none_or(|window| window > limits.max_window_ms)
            || self.evidence.source_digest != request.source_digest
            || self.evidence.proof_digest != request.proof_digest
            || self.evidence.release != request.release
            || self.evidence.verified_at_unix_ms != self.issued_at_unix_ms
            || self.review_members.len() >= 128
            || usize::from(source.review_count) != self.review_members.len()
            || source.ordinal != source.review_count.checked_add(1).ok_or_else(corrupt)?
            || source.review_history_digest != history_digest(&self.review_members)?
            || !seen.contains(&source.retired_review_id)
            || source.ordinal > limits.total_review_ceiling
            || !matches!(session.cursor, AdaptiveCursorV1::ReadyForModel)
            || !session.model_window_exhausted_at(self.issued_at_unix_ms)
            || anchor.expected_session_version != session.version
            || anchor.expected_project_version != project.version
            || anchor.head_entry_digest != source.session_head_digest
            || anchor.base_model_calls != session.model_calls
            || anchor.base_tool_calls != session.tool_calls
            || session.model_calls >= limits.total_model_call_ceiling
            || session.tool_calls >= limits.total_tool_call_ceiling
            || session.continuation.as_ref().is_some_and(|state| {
                state.authorizations.len() >= usize::from(limits.total_window_ceiling)
                    || state.authorizations.iter().any(|auth| {
                        auth.work_funding
                            .as_ref()
                            .is_some_and(|prior| prior.same_epoch(epoch))
                    })
            })
            || self
                .issued_at_unix_ms
                .checked_add(limits.max_call_duration_ms)
                .and_then(|time| time.checked_add(limits.dispatch_margin_ms))
                .is_none_or(|time| time > request.expires_at_unix_ms)
        {
            return Err(corrupt());
        }
        match &self.evidence.no_io {
            AdaptiveFundedReviewNoIoV1::NeverReserved { request_id } => {
                if request_id != &format!("company-leadership-{}", source.retired_review_id) {
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
                if request_id != &format!("company-leadership-{}", source.retired_review_id)
                    || *released_at_unix_ms > self.issued_at_unix_ms
                {
                    return Err(corrupt());
                }
            }
        }
        Ok(())
    }

    fn validate_persisted(&self, connection: &Connection) -> Result<(), WorkflowError> {
        // Historical validation intentionally uses the original issuance clock.
        // Current head/clock authority is checked separately at claim/completion.
        let source = &self.request.source;
        // Never enumerate current calls here: a designated schema6 call loads
        // this receipt through its own membership and would recurse.
        crate::store::adaptive::require_journal_source(connection, &self.context.source_session)?;
        let epoch = self
            .grant_template
            .work_funding
            .as_ref()
            .ok_or_else(corrupt)?;
        for member in &self.review_members {
            let stored = load_member(
                connection,
                &self.issuer_principal.tenant_id,
                source.session_id,
                member,
            )?;
            if stored
                .grant
                .work_funding
                .as_ref()
                .is_some_and(|prior| prior.same_epoch(epoch))
                && (stored.grant.schema_version != 5
                    || stored.dispatch.is_some()
                    || stored.decision.is_some()
                    || stored.model_response_digest.is_some()
                    || stored.continuation.is_some()
                    || stored.resolution_event_id.is_some()
                    || stored.retired_at_unix_ms.is_none())
            {
                return Err(corrupt());
            }
            if stored.grant.review_id == source.retired_review_id {
                if stored.grant.schema_version != 5
                    || stored
                        .grant
                        .work_funding
                        .as_ref()
                        .is_none_or(|prior| !prior.same_epoch(epoch))
                    || stored.context.source_project != self.context.source_project
                    || stored.context.source_session != self.context.source_session
                    || stored.retired_at_unix_ms.is_none_or(|at| {
                        at < stored.grant.expires_at_unix_ms || at > self.issued_at_unix_ms
                    })
                {
                    return Err(corrupt());
                }
                if let AdaptiveFundedReviewNoIoV1::ReleasedBeforeIo {
                    released_at_unix_ms,
                    ..
                } = &self.evidence.no_io
                {
                    if *released_at_unix_ms < stored.grant_issued_at_unix_ms
                        || *released_at_unix_ms > stored.retired_at_unix_ms.ok_or_else(corrupt)?
                    {
                        return Err(corrupt());
                    }
                }
            }
        }
        adaptive_work_funding::require_epoch_receipt(
            connection,
            self.grant_template
                .work_funding
                .as_ref()
                .ok_or_else(corrupt)?,
        )?;
        adaptive_work_funding::require_pre_recovery_funding_history(
            connection,
            self.grant_template
                .work_funding
                .as_ref()
                .ok_or_else(corrupt)?,
            Some((
                &self.bound_grant()?,
                &self.context,
                self.request.operation_id,
                self.issued_at_unix_ms,
            )),
        )?;
        let grant = self.bound_grant()?;
        let context_digest = canonical_sha256(
            "sentinel.workflow.adaptive-leadership-context.v1",
            &(&grant, &self.context),
        )?;
        adaptive_work_funding::require_funding_review_membership_leaf(
            connection,
            &grant,
            &context_digest,
            self.request.operation_id,
        )?;
        adaptive_resume_policy::require_leaf_event(
            connection,
            &self.issuer_principal.tenant_id,
            &source.project_id,
            EVENT,
            self.request.operation_id,
            &self.receipt_digest()?,
            &self.issuer_principal,
            self.issued_at_unix_ms,
            self,
        )
    }
}

pub(super) fn require_membership(
    connection: &Connection,
    grant: &AdaptiveLeadershipReviewGrantV1,
    context: Option<&AdaptiveLeadershipReviewContextV1>,
) -> Result<AdaptiveFundedReviewRecoveryReceiptV1, WorkflowError> {
    let binding = grant
        .funded_review_recovery
        .as_ref()
        .ok_or_else(unauthorized)?;
    binding.validate_for(grant)?;
    let receipt: AdaptiveFundedReviewRecoveryReceiptV1 = get_entity(
        connection,
        &grant.leadership_principal.tenant_id,
        KIND,
        &binding.receipt_key,
    )?
    .ok_or_else(corrupt)?;
    if receipt.receipt_digest()? != binding.receipt_digest
        || receipt.bound_grant()? != *grant
        || context.is_some_and(|context| *context != receipt.context)
    {
        return Err(corrupt());
    }
    Ok(receipt)
}

impl WorkflowStore {
    pub fn adaptive_funded_review_recovery_source(
        &self,
        operator: &AuthenticatedCompanyPrincipalV1,
        project_id: &ProjectId,
        session_id: Uuid,
        retired_review_id: Uuid,
        now_ms: u64,
    ) -> Result<AdaptiveFundedReviewRecoverySourceV1, WorkflowError> {
        Ok(self
            .adaptive_funded_review_recovery_verification_source(
                operator,
                project_id,
                session_id,
                retired_review_id,
                now_ms,
            )?
            .source)
    }

    /// Rust-only fresh snapshots for preparation, not an HTTP body or authority grant.
    pub fn adaptive_funded_review_recovery_verification_source(
        &self,
        operator: &AuthenticatedCompanyPrincipalV1,
        project_id: &ProjectId,
        session_id: Uuid,
        retired_review_id: Uuid,
        now_ms: u64,
    ) -> Result<AdaptiveFundedReviewRecoveryVerificationSourceV1, WorkflowError> {
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let scope = validation_scope::enter(&transaction)?;
        let source = source_in_connection(
            &transaction,
            operator,
            project_id,
            session_id,
            retired_review_id,
            now_ms,
        )?;
        if get_entity::<AdaptiveFundedReviewRecoveryReceiptV1>(
            &transaction,
            &operator.tenant_id,
            KIND,
            &source.source.receipt_key()?,
        )?
        .is_some()
        {
            return Err(transition());
        }
        scope.finish()?;
        Ok(source)
    }

    pub fn adaptive_funded_review_recovery(
        &self,
        tenant: &TenantId,
        receipt_key: &str,
    ) -> Result<Option<AdaptiveFundedReviewRecoveryReceiptV1>, WorkflowError> {
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        receipt_in_connection(&transaction, tenant, receipt_key)
    }

    /// Rust-only reconstruction of the immutable issuance snapshot. This remains
    /// available after adoption/expiry and never establishes fresh dispatch eligibility.
    pub fn adaptive_funded_review_recovery_historical_verification_source(
        &self,
        tenant: &TenantId,
        receipt_key: &str,
    ) -> Result<Option<AdaptiveFundedReviewRecoveryVerificationSourceV1>, WorkflowError> {
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let scope = validation_scope::enter(&transaction)?;
        let Some(receipt) = receipt_in_connection(&transaction, tenant, receipt_key)? else {
            scope.finish()?;
            return Ok(None);
        };
        let calls = receipt
            .review_members
            .iter()
            .map(|member| {
                load_member(
                    &transaction,
                    tenant,
                    receipt.request.source.session_id,
                    member,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let funding = receipt
            .grant_template
            .work_funding
            .as_ref()
            .ok_or_else(corrupt)?
            .receipt
            .clone();
        scope.finish()?;
        Ok(Some(AdaptiveFundedReviewRecoveryVerificationSourceV1 {
            source: *receipt.request.source,
            project: Box::new(receipt.context.source_project),
            session: Box::new(receipt.context.source_session),
            funding: Box::new(funding),
            calls,
        }))
    }

    /// The verifier must validate protected NoIO evidence while the daemon holds
    /// admission and mutation exclusion. It must not re-enter this WorkflowStore.
    pub fn authorize_adaptive_funded_review_recovery<F>(
        &self,
        operator: &AuthenticatedCompanyPrincipalV1,
        request: &AdaptiveFundedReviewRecoveryRequestV1,
        grant_template: &AdaptiveLeadershipReviewGrantV1,
        context: &AdaptiveLeadershipReviewContextV1,
        now_ms: u64,
        verifier: F,
    ) -> Result<(bool, AdaptiveFundedReviewRecoveryReceiptV1), WorkflowError>
    where
        F: FnOnce(
            &AdaptiveFundedReviewRecoveryVerificationSourceV1,
            &AdaptiveFundedReviewRecoveryRequestV1,
        ) -> Result<AdaptiveFundedReviewRecoveryEvidenceV1, WorkflowError>,
    {
        crate::adaptive_resume_policy::require_resume_policy_operator(
            operator,
            &operator.tenant_id,
        )?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut normalized_context = context.clone();
        normalized_context.evidence_refs.sort();
        let key = request.source.receipt_key()?;
        if let Some(prior) = get_entity::<AdaptiveFundedReviewRecoveryReceiptV1>(
            &transaction,
            &operator.tenant_id,
            KIND,
            &key,
        )? {
            if prior.issuer_principal != *operator
                || prior.request != *request
                || prior.grant_template != *grant_template
                || prior.context != normalized_context
            {
                return Err(transition());
            }
            let _: AdaptiveLeadershipReviewCallV1 = get_entity(
                &transaction,
                &operator.tenant_id,
                "adaptive_leadership_review_call",
                &prior.grant_template.review_id.to_string(),
            )?
            .ok_or_else(corrupt)?;
            return Ok((true, prior));
        }
        let orphan: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM company_events WHERE tenant_id=?1 AND event_type=?2
             AND (operation_id=?3 OR json_extract(payload,'$.receipt_key')=?4))",
            params![
                operator.tenant_id.0,
                EVENT,
                request.operation_id.to_string(),
                key
            ],
            |row| row.get(0),
        )?;
        if orphan {
            return Err(corrupt());
        }
        let source = source_in_connection(
            &transaction,
            operator,
            &request.source.project_id,
            request.source.session_id,
            request.source.retired_review_id,
            now_ms,
        )?;
        if source.source != *request.source
            || request.source_digest != source.source.canonical_digest()?
            || request.expires_at_unix_ms <= now_ms
            || now_ms
                .checked_add(source.funding.request.limits.max_call_duration_ms)
                .and_then(|time| time.checked_add(source.funding.request.limits.dispatch_margin_ms))
                .is_none_or(|time| time > request.expires_at_unix_ms)
        {
            return Err(transition());
        }
        let evidence = verifier(&source, request)?;
        if evidence.verified_at_unix_ms < now_ms {
            return Err(transition());
        }
        let issued_at_unix_ms = evidence.verified_at_unix_ms;
        let receipt = AdaptiveFundedReviewRecoveryReceiptV1 {
            schema_version: 1,
            receipt_key: key,
            request: request.clone(),
            issuer_principal: operator.clone(),
            grant_template: grant_template.clone(),
            context: normalized_context.clone(),
            evidence,
            issued_at_unix_ms,
            review_members: review_members(&source.calls)?,
        };
        receipt.validate_entity()?;
        let grant = receipt.bound_grant()?;
        let context_digest = canonical_sha256(
            "sentinel.workflow.adaptive-leadership-context.v1",
            &(&grant, &normalized_context),
        )?;
        adaptive_work_funding::insert_funding_review_membership(
            &transaction,
            &grant,
            &context_digest,
            request.operation_id,
            issued_at_unix_ms,
        )?;
        put_entity(
            &transaction,
            &operator.tenant_id,
            KIND,
            &receipt.receipt_key,
            1,
            &receipt,
        )?;
        append_event(
            &transaction,
            operator,
            request.operation_id,
            &receipt.receipt_digest()?,
            Some(&source.project.project_id),
            EVENT,
            &receipt,
            issued_at_unix_ms,
        )?;
        Self::authorize_new_leadership_review_in_transaction(
            &transaction,
            &grant.leadership_principal,
            request.operation_id,
            &format!("{}-allowance", receipt.receipt_key),
            &grant,
            &normalized_context,
            issued_at_unix_ms,
        )?;
        transaction.commit()?;
        Ok((false, receipt))
    }
}
