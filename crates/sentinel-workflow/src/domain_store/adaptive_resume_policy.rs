use super::*;
use crate::adaptive_resume_policy::*;
use crate::{
    AdaptiveContinuationAuthorizationV1, AdaptiveCursorV1, AdaptiveLeadershipReviewCallV1,
    AdaptiveLeadershipReviewGrantV1, RuntimeAuthoritySnapshotV1,
};

const POLICY_KIND: &str = "adaptive_resume_policy";
const POLICY_EVENT: &str = "adaptive_resume_policy_authorized";
const MEMBERSHIP_KIND: &str = "adaptive_resume_review_membership";
const MEMBERSHIP_EVENT: &str = "adaptive_resume_review_membership_issued";
const REVIEW_KIND: &str = "adaptive_leadership_review_call";
const MAX_REVIEW_SCAN: usize = 4_096;

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ResumeReviewMembershipLeaf {
    schema_version: u16,
    tenant_id: TenantId,
    project_id: ProjectId,
    session_id: Uuid,
    membership_id: String,
    membership: AdaptiveResumeReviewMembershipV1,
    issuer_principal: AuthenticatedCompanyPrincipalV1,
    issued_at_unix_ms: u64,
}

impl CompanyEntity for AdaptiveResumePolicyReceiptV1 {
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        (
            &self.request.source.tenant_id,
            POLICY_KIND,
            &self.policy_id,
            1,
        )
    }

    fn validate_entity(&self) -> Result<(), WorkflowError> {
        self.validate()
    }

    fn validate_persisted(&self, connection: &Connection) -> Result<(), WorkflowError> {
        require_leaf_event(
            connection,
            &self.request.source.tenant_id,
            &self.request.source.project_id,
            POLICY_EVENT,
            self.request.operation_id,
            &self.request.canonical_digest()?,
            &self.issuer_principal,
            self.issued_at_unix_ms,
            self,
        )
    }
}

impl CompanyEntity for ResumeReviewMembershipLeaf {
    fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
        (&self.tenant_id, MEMBERSHIP_KIND, &self.membership_id, 1)
    }

    fn validate_entity(&self) -> Result<(), WorkflowError> {
        self.tenant_id.validate()?;
        self.project_id.validate()?;
        self.issuer_principal.validate()?;
        self.membership.validate()?;
        if self.schema_version != 1
            || self.session_id.is_nil()
            || self.issued_at_unix_ms == 0
            || self.issuer_principal.tenant_id != self.tenant_id
            || self.issuer_principal.kind != CompanyPrincipalKindV1::Agent
            || !matches!(
                self.issuer_principal.role,
                CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
            )
            || self.membership.policy_id
                != adaptive_resume_policy_id(&self.tenant_id, self.session_id)?
            || self.membership_id
                != adaptive_resume_review_membership_id(
                    &self.membership.policy_id,
                    self.membership.ordinal,
                )?
        {
            return Err(corrupt());
        }
        Ok(())
    }

    fn validate_persisted(&self, connection: &Connection) -> Result<(), WorkflowError> {
        require_leaf_event(
            connection,
            &self.tenant_id,
            &self.project_id,
            MEMBERSHIP_EVENT,
            self.membership.operation_id,
            &self.membership.canonical_digest()?,
            &self.issuer_principal,
            self.issued_at_unix_ms,
            self,
        )
    }
}

// Leaf verification deliberately has no project, journal, policy or review dependencies.
fn require_leaf_event<T: serde::de::DeserializeOwned + Serialize + PartialEq>(
    connection: &Connection,
    tenant: &TenantId,
    project: &ProjectId,
    event_type: &str,
    operation_id: Uuid,
    operation_digest: &str,
    principal: &AuthenticatedCompanyPrincipalV1,
    issued_at_ms: u64,
    value: &T,
) -> Result<(), WorkflowError> {
    let mut statement = connection.prepare(
        "SELECT sequence FROM company_events WHERE tenant_id=?1 AND event_type=?2 AND operation_id=?3 LIMIT 2",
    )?;
    let ids = statement
        .query_map(
            params![tenant.0, event_type, operation_id.to_string()],
            |row| row.get::<_, i64>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    if ids.len() != 1 {
        return Err(corrupt());
    }
    let row = read_company_event_row(connection, stored_u64(ids[0])?)?.ok_or_else(corrupt)?;
    validation_scope::charge_bytes(connection, row.payload.len())?;
    let issuer = company_event_principal(&row)?;
    let payload_digest = bytes_digest("sentinel.workflow.company-event-payload.v1", &row.payload)?;
    let event_id = canonical_sha256(
        "sentinel.workflow.company-event-id.v1",
        &(
            tenant,
            Some(project),
            event_type,
            operation_id,
            operation_digest,
            principal.binding_digest()?,
            &payload_digest,
            issued_at_ms,
        ),
    )?;
    if issuer != *principal
        || row.tenant_id != tenant.0
        || row.project_id.as_deref() != Some(project.0.as_str())
        || row.event_type != event_type
        || row.operation_id != operation_id.to_string()
        || row.operation_digest != operation_digest
        || row.authority_binding_digest != principal.binding_digest()?
        || row.payload_digest != payload_digest
        || row.event_id != event_id
        || stored_u64(row.created_at_ms)? != issued_at_ms
        || decode::<T>(&row.payload)? != *value
    {
        return Err(corrupt());
    }
    Ok(())
}

pub(crate) fn read_resume_policy_leaf(
    connection: &Connection,
    tenant: &TenantId,
    session_id: Uuid,
) -> Result<Option<AdaptiveResumePolicyReceiptV1>, WorkflowError> {
    let key = adaptive_resume_policy_id(tenant, session_id)?;
    let receipt = get_entity(connection, tenant, POLICY_KIND, &key)?;
    if receipt.is_none() {
        let orphan: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM company_events WHERE tenant_id=?1 AND event_type=?2
             AND (json_extract(payload,'$.policy_id')=?3 OR json_extract(payload,'$.request.source.session_id')=?4))",
            params![tenant.0, POLICY_EVENT, key, session_id.to_string()], |row| row.get(0),
        )?;
        if orphan {
            return Err(corrupt());
        }
    }
    Ok(receipt)
}

fn review_grant_digest(grant: &AdaptiveLeadershipReviewGrantV1) -> Result<String, WorkflowError> {
    canonical_sha256("sentinel.workflow.adaptive-resume-review-grant.v1", grant)
}

fn require_review_binding(
    receipt: &AdaptiveResumePolicyReceiptV1,
    grant: &AdaptiveLeadershipReviewGrantV1,
) -> Result<(), WorkflowError> {
    let binding = grant.resume_policy.as_deref().ok_or_else(unauthorized)?;
    receipt.validate_binding(binding)?;
    let source = &receipt.request.source;
    grant.leadership_principal.validate()?;
    if grant.session_id != source.session_id
        || grant.project_id != source.project_id
        || grant.work_item_id != source.work_item_id
        || grant.assignee_authority != source.assignee_authority
        || grant.leadership_principal.tenant_id != source.tenant_id
        || grant.leadership_principal.kind != CompanyPrincipalKindV1::Agent
        || !matches!(
            grant.leadership_principal.role,
            CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
        )
        || grant.expected_project_version < source.expected_project_version
        || grant.expected_session_version < source.expected_session_version
        || grant.expires_at_unix_ms > binding.limits.expires_at_unix_ms
        || grant.max_duration_ms > binding.limits.max_call_duration_ms
    {
        return Err(unauthorized());
    }
    Ok(())
}

pub(crate) fn require_resume_review_membership(
    connection: &Connection,
    grant: &AdaptiveLeadershipReviewGrantV1,
    context_digest: &str,
    operation_id: Uuid,
) -> Result<(), WorkflowError> {
    validation_scope::with_scope(connection, || {
        validate_digest(context_digest)?;
        let binding = grant.resume_policy.as_deref().ok_or_else(unauthorized)?;
        let tenant = &grant.leadership_principal.tenant_id;
        let receipt = read_resume_policy_leaf(connection, tenant, grant.session_id)?
            .ok_or_else(unauthorized)?;
        require_review_binding(&receipt, grant)?;
        let id = adaptive_resume_review_membership_id(&binding.policy_id, binding.ordinal)?;
        let leaf: ResumeReviewMembershipLeaf =
            get_entity(connection, tenant, MEMBERSHIP_KIND, &id)?.ok_or_else(corrupt)?;
        if leaf.session_id != grant.session_id
            || leaf.project_id != grant.project_id
            || leaf.issuer_principal != grant.leadership_principal
            || leaf.membership.review_id != grant.review_id
            || leaf.membership.operation_id != operation_id
            || leaf.membership.receipt_digest != binding.receipt_digest
            || leaf.membership.grant_digest != review_grant_digest(grant)?
            || leaf.membership.context_digest != context_digest
            || leaf.issued_at_unix_ms < receipt.issued_at_unix_ms
            || leaf.issued_at_unix_ms >= grant.expires_at_unix_ms
        {
            return Err(corrupt());
        }
        Ok(())
    })
}

pub(crate) fn require_resume_authorization_membership(
    connection: &Connection,
    authorization: &AdaptiveContinuationAuthorizationV1,
    authority: &RuntimeAuthoritySnapshotV1,
) -> Result<(), WorkflowError> {
    validation_scope::with_scope(connection, || {
        authorization.validate()?;
        let binding = authorization
            .resume_policy
            .as_deref()
            .ok_or_else(unauthorized)?;
        let receipt =
            read_resume_policy_leaf(connection, &authority.tenant_id, authorization.session_id)?
                .ok_or_else(corrupt)?;
        receipt.validate_binding(binding)?;
        let source = &receipt.request.source;
        let id = adaptive_resume_review_membership_id(&binding.policy_id, binding.ordinal)?;
        let leaf: ResumeReviewMembershipLeaf =
            get_entity(connection, &authority.tenant_id, MEMBERSHIP_KIND, &id)?
                .ok_or_else(corrupt)?;
        if source.assignee_authority != *authority
            || authorization.source_session_version < source.expected_session_version
            || authorization.issued_at_ms < receipt.issued_at_unix_ms
            || authorization.issued_at_ms >= binding.limits.expires_at_unix_ms
            || authorization.deadline_ms > binding.limits.expires_at_unix_ms
            || authorization.deadline_ms - authorization.issued_at_ms > binding.limits.max_window_ms
            || leaf.project_id != source.project_id
            || leaf.session_id != source.session_id
            || leaf.membership.policy_id != binding.policy_id
            || leaf.membership.ordinal != binding.ordinal
            || leaf.membership.receipt_digest != binding.receipt_digest
            || leaf.membership.review_id != authorization.review_id
            || leaf.membership.operation_id != authorization.operation_id
            || leaf.issued_at_unix_ms < receipt.issued_at_unix_ms
            || leaf.issued_at_unix_ms > authorization.issued_at_ms
        {
            return Err(corrupt());
        }
        Ok(())
    })
}

pub(super) fn insert_resume_review_membership(
    transaction: &Transaction<'_>,
    grant: &AdaptiveLeadershipReviewGrantV1,
    operation_id: Uuid,
    context_digest: &str,
    issued_at_ms: u64,
) -> Result<AdaptiveResumeReviewMembershipV1, WorkflowError> {
    validate_digest(context_digest)?;
    let binding = grant.resume_policy.as_deref().ok_or_else(unauthorized)?;
    let tenant = &grant.leadership_principal.tenant_id;
    let receipt =
        read_resume_policy_leaf(transaction, tenant, grant.session_id)?.ok_or_else(unauthorized)?;
    require_review_binding(&receipt, grant)?;
    let membership = AdaptiveResumeReviewMembershipV1 {
        schema_version: 1,
        policy_id: binding.policy_id.clone(),
        receipt_digest: binding.receipt_digest.clone(),
        ordinal: binding.ordinal,
        review_id: grant.review_id,
        operation_id,
        grant_digest: review_grant_digest(grant)?,
        context_digest: context_digest.to_owned(),
    };
    membership.validate()?;
    let id = adaptive_resume_review_membership_id(&binding.policy_id, binding.ordinal)?;
    if let Some(prior) =
        get_entity::<ResumeReviewMembershipLeaf>(transaction, tenant, MEMBERSHIP_KIND, &id)?
    {
        if prior.membership != membership
            || prior.issuer_principal != grant.leadership_principal
            || prior.project_id != grant.project_id
            || prior.session_id != grant.session_id
        {
            return Err(conflict());
        }
        return Ok(prior.membership);
    }
    if issued_at_ms < receipt.issued_at_unix_ms
        || issued_at_ms >= binding.limits.expires_at_unix_ms
        || issued_at_ms >= grant.expires_at_unix_ms
    {
        return Err(transition());
    }
    let mut statement = transaction.prepare(
        "SELECT entity_id FROM company_entities WHERE tenant_id=?1 AND entity_kind=?2 AND entity_id GLOB ?3 ORDER BY entity_id LIMIT 129",
    )?;
    let ids = statement
        .query_map(
            params![
                tenant.0,
                MEMBERSHIP_KIND,
                format!("{}-review-*", binding.policy_id)
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    if ids.len() > usize::from(ADAPTIVE_RESUME_MAX_REVIEWS) {
        return Err(corrupt());
    }
    for (index, id) in ids.iter().enumerate() {
        let prior: ResumeReviewMembershipLeaf =
            get_entity(transaction, tenant, MEMBERSHIP_KIND, id)?.ok_or_else(corrupt)?;
        let ordinal = receipt
            .request
            .source
            .base_review_count
            .checked_add(u16::try_from(index + 1).map_err(|_| corrupt())?)
            .ok_or_else(corrupt)?;
        if prior.membership.ordinal != ordinal
            || prior.membership.policy_id != binding.policy_id
            || prior.membership.receipt_digest != binding.receipt_digest
            || prior.session_id != grant.session_id
            || prior.project_id != grant.project_id
            || prior.membership.review_id == grant.review_id
            || prior.membership.operation_id == operation_id
        {
            return Err(conflict());
        }
    }
    let next = receipt
        .request
        .source
        .base_review_count
        .checked_add(u16::try_from(ids.len() + 1).map_err(|_| corrupt())?)
        .ok_or_else(corrupt)?;
    if binding.ordinal != next {
        return Err(conflict());
    }
    let duplicate: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM company_events WHERE tenant_id=?1 AND event_type=?2
         AND (operation_id=?3 OR json_extract(payload,'$.membership_id')=?4))",
        params![tenant.0, MEMBERSHIP_EVENT, operation_id.to_string(), id],
        |row| row.get(0),
    )?;
    if duplicate {
        return Err(conflict());
    }
    let leaf = ResumeReviewMembershipLeaf {
        schema_version: 1,
        tenant_id: tenant.clone(),
        project_id: grant.project_id.clone(),
        session_id: grant.session_id,
        membership_id: id.clone(),
        membership: membership.clone(),
        issuer_principal: grant.leadership_principal.clone(),
        issued_at_unix_ms: issued_at_ms,
    };
    leaf.validate_entity()?;
    put_entity(transaction, tenant, MEMBERSHIP_KIND, &id, 1, &leaf)?;
    append_event(
        transaction,
        &grant.leadership_principal,
        operation_id,
        &membership.canonical_digest()?,
        Some(&grant.project_id),
        MEMBERSHIP_EVENT,
        &leaf,
        issued_at_ms,
    )?;
    Ok(membership)
}

impl WorkflowStore {
    pub fn adaptive_resume_policy_draft(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        project_id: &ProjectId,
        session_id: Uuid,
        operation_id: Uuid,
        reason_ref: &str,
        expires_at_unix_ms: u64,
        now_ms: u64,
    ) -> Result<AdaptiveResumePolicyRequestV1, WorkflowError> {
        self.resume_policy_draft_impl(
            principal,
            project_id,
            session_id,
            operation_id,
            reason_ref,
            expires_at_unix_ms,
            now_ms,
            None,
        )
    }

    /// The daemon supplies a server-loaded proof digest; this draft does not authorize issuance.
    pub fn adaptive_resume_policy_draft_with_unknown_proof(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        project_id: &ProjectId,
        session_id: Uuid,
        operation_id: Uuid,
        reason_ref: &str,
        expires_at_unix_ms: u64,
        now_ms: u64,
        sealed_unknown_proof_digest: &str,
    ) -> Result<AdaptiveResumePolicyRequestV1, WorkflowError> {
        self.resume_policy_draft_impl(
            principal,
            project_id,
            session_id,
            operation_id,
            reason_ref,
            expires_at_unix_ms,
            now_ms,
            Some(sealed_unknown_proof_digest),
        )
    }

    fn resume_policy_draft_impl(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        project_id: &ProjectId,
        session_id: Uuid,
        operation_id: Uuid,
        reason_ref: &str,
        expires_at_unix_ms: u64,
        now_ms: u64,
        unknown_proof: Option<&str>,
    ) -> Result<AdaptiveResumePolicyRequestV1, WorkflowError> {
        require_resume_policy_operator(principal, &principal.tenant_id)?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let scope = validation_scope::enter(&transaction)?;
        if let Some(prior) =
            read_resume_policy_leaf(&transaction, &principal.tenant_id, session_id)?
        {
            if prior.issuer_principal != *principal
                || prior.request.source.project_id != *project_id
                || prior.request.operation_id != operation_id
                || prior.request.reason_ref != reason_ref
                || prior.request.limits.expires_at_unix_ms != expires_at_unix_ms
            {
                return Err(conflict());
            }
            scope.finish()?;
            return Ok(prior.request);
        }
        let (source, capacity, duration) = fresh_source(
            &transaction,
            &principal.tenant_id,
            project_id,
            session_id,
            now_ms,
            unknown_proof,
        )?;
        let request = AdaptiveResumePolicyRequestV1 {
            schema_version: 1,
            operation_id,
            limits: AdaptiveResumePolicyLimitsV1 {
                total_review_ceiling: source
                    .base_review_count
                    .checked_add(capacity)
                    .ok_or_else(corrupt)?,
                total_window_ceiling: source
                    .base_window_count
                    .checked_add(capacity)
                    .ok_or_else(corrupt)?,
                max_window_ms: ADAPTIVE_RESUME_MAX_WINDOW_MS,
                max_call_duration_ms: duration,
                dispatch_margin_ms: ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
                expires_at_unix_ms,
            },
            source,
            reason_ref: reason_ref.to_owned(),
        };
        request.validate_at(principal, now_ms)?;
        scope.finish()?;
        Ok(request)
    }

    pub fn authorize_adaptive_resume_policy(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        request: &AdaptiveResumePolicyRequestV1,
        now_ms: u64,
    ) -> Result<(bool, AdaptiveResumePolicyReceiptV1), WorkflowError> {
        self.authorize_adaptive_resume_policy_with_unknown_proof(
            principal,
            request,
            now_ms,
            |_, _| Err(unauthorized()),
        )
    }

    /// Called with the store mutex held. Verify immutable external proof without re-entering this store.
    pub fn authorize_adaptive_resume_policy_with_unknown_proof<F>(
        &self,
        principal: &AuthenticatedCompanyPrincipalV1,
        request: &AdaptiveResumePolicyRequestV1,
        now_ms: u64,
        verify_unknown: F,
    ) -> Result<(bool, AdaptiveResumePolicyReceiptV1), WorkflowError>
    where
        F: FnOnce(&Connection, &AdaptiveResumeSourceV1) -> Result<String, WorkflowError>,
    {
        require_resume_policy_operator(principal, &request.source.tenant_id)?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let scope = validation_scope::enter(&transaction)?;
        if let Some(prior) = read_resume_policy_leaf(
            &transaction,
            &request.source.tenant_id,
            request.source.session_id,
        )? {
            if prior.request != *request || prior.issuer_principal != *principal {
                return Err(conflict());
            }
            scope.finish()?;
            return Ok((true, prior));
        }
        request.validate_at(principal, now_ms)?;
        let proof = match &request.source.subject {
            AdaptiveResumeSubjectV1::ModelUnknown {
                sealed_unknown_proof_digest,
                ..
            } => Some(sealed_unknown_proof_digest.as_str()),
            _ => None,
        };
        let (source, capacity, duration) = fresh_source(
            &transaction,
            &request.source.tenant_id,
            &request.source.project_id,
            request.source.session_id,
            now_ms,
            proof,
        )?;
        if source != request.source
            || request.limits.total_review_ceiling - source.base_review_count > capacity
            || request.limits.total_window_ceiling - source.base_window_count > capacity
            || request.limits.max_call_duration_ms != duration
        {
            return Err(transition());
        }
        if let Some(expected) = proof {
            let changes: i64 =
                transaction.query_row("SELECT total_changes()", [], |row| row.get(0))?;
            let verified = verify_unknown(&transaction, &source)?;
            validate_digest(&verified)?;
            let after: i64 =
                transaction.query_row("SELECT total_changes()", [], |row| row.get(0))?;
            if changes != after || !constant_time_eq(expected, &verified) {
                return Err(unauthorized());
            }
        }
        let duplicate: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM company_events WHERE tenant_id=?1 AND event_type=?2 AND operation_id=?3)",
            params![request.source.tenant_id.0, POLICY_EVENT, request.operation_id.to_string()], |row| row.get(0),
        )?;
        if duplicate {
            return Err(conflict());
        }
        let receipt = AdaptiveResumePolicyReceiptV1 {
            schema_version: 1,
            request: request.clone(),
            policy_id: adaptive_resume_policy_id(&source.tenant_id, source.session_id)?,
            issuer_principal: principal.clone(),
            issued_at_unix_ms: now_ms,
        };
        receipt.validate()?;
        scope.finish()?;
        put_entity(
            &transaction,
            &source.tenant_id,
            POLICY_KIND,
            &receipt.policy_id,
            1,
            &receipt,
        )?;
        append_event(
            &transaction,
            principal,
            request.operation_id,
            &request.canonical_digest()?,
            Some(&source.project_id),
            POLICY_EVENT,
            &receipt,
            now_ms,
        )?;
        transaction.commit()?;
        Ok((false, receipt))
    }

    pub fn adaptive_resume_policy(
        &self,
        tenant: &TenantId,
        session_id: Uuid,
    ) -> Result<Option<AdaptiveResumePolicyReceiptV1>, WorkflowError> {
        let connection = self.connection.lock().map_err(|_| persistence())?;
        read_resume_policy_leaf(&connection, tenant, session_id)
    }
}

fn fresh_source(
    connection: &Connection,
    tenant: &TenantId,
    project_id: &ProjectId,
    session_id: Uuid,
    now_ms: u64,
    unknown_proof: Option<&str>,
) -> Result<(AdaptiveResumeSourceV1, u16, u64), WorkflowError> {
    let project: ProjectV1 =
        get_entity(connection, tenant, "project", &project_id.0)?.ok_or_else(not_found)?;
    let (session, root_entry_digest, head_entry_digest) =
        crate::store::adaptive::adaptive_resume_journal_source(connection, session_id)?
            .ok_or_else(not_found)?;
    let authority = &session.grant.authority;
    if authority.tenant_id != *tenant
        || authority.project_id != *project_id
        || now_ms < project.updated_at_unix_ms
        || now_ms < session.updated_at_ms
        || project.lifecycle_state != ProjectLifecycleStateV1::Active
    {
        return Err(unauthorized());
    }
    let work = project
        .work_items
        .get(&authority.work_item_id)
        .ok_or_else(unauthorized)?;
    let assignments: Vec<_> = work
        .assignments
        .iter()
        .filter(|assignment| assignment.active)
        .collect();
    let [assignment] = assignments.as_slice() else {
        return Err(unauthorized());
    };
    if !matches!(
        work.state,
        CompanyWorkStateV1::Assigned
            | CompanyWorkStateV1::InProgress
            | CompanyWorkStateV1::InReview
    ) || work.spec.owner != authority.agent_id
        || assignment.agent_id != authority.agent_id
        || assignment.assignment_version != authority.assignment_version
        || assignment.canonical_digest()? != authority.assignment_digest
        || assignment.profile.profile_id != authority.profile_id
        || assignment.profile.generation != authority.profile_generation
        || assignment.profile.digest != authority.profile_digest
        || assignment.organization_generation != authority.organization_generation
        || assignment.organization_digest != authority.organization_digest
        || project.governance.project_profile.generation != authority.policy_generation
        || project.governance.project_profile.digest != authority.policy_digest
        || !project.governance.participants.iter().any(|participant| {
            participant.agent_id == authority.agent_id
                && participant.principal_id == authority.principal.principal_id
                && participant.role == work.spec.required_role
        })
    {
        return Err(unauthorized());
    }
    let allowance = project
        .subscription_call
        .as_ref()
        .ok_or_else(unauthorized)?;
    let effective = session.effective_grant();
    if allowance.allowance_id != session.active_provider_allowance_id()
        || allowance.dispatch.is_some()
        || allowance.grant.assignment_id != assignment.assignment_id
        || crate::adaptive_leadership_continuation_provider_authority_digest(allowance, authority)?
            != effective.provider_authority_digest
    {
        return Err(unauthorized());
    }
    let subject = match &session.cursor {
        AdaptiveCursorV1::ReadyForModel
            if session.model_calls >= session.active_model_ceiling()
                || now_ms >= session.active_deadline_ms() =>
        {
            if unknown_proof.is_some() {
                return Err(unauthorized());
            }
            AdaptiveResumeSubjectV1::ReadyForModel {
                active_allowance_digest: crate::adaptive_budget_allowance_digest(allowance)?,
            }
        }
        AdaptiveCursorV1::ModelUnknown { effect } => {
            let digest = unknown_proof.ok_or_else(unauthorized)?;
            validate_digest(digest)?;
            AdaptiveResumeSubjectV1::ModelUnknown {
                effect: effect.clone(),
                sealed_unknown_proof_digest: digest.to_owned(),
            }
        }
        _ => return Err(transition()),
    };
    let mut statement = connection.prepare("SELECT entity_id FROM company_entities WHERE tenant_id=?1 AND entity_kind=?2 ORDER BY entity_id LIMIT 4097")?;
    let ids = statement
        .query_map(params![tenant.0, REVIEW_KIND], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if ids.len() > MAX_REVIEW_SCAN {
        return Err(corrupt());
    }
    let mut calls = Vec::new();
    for id in ids {
        let call: AdaptiveLeadershipReviewCallV1 =
            get_entity(connection, tenant, REVIEW_KIND, &id)?.ok_or_else(corrupt)?;
        if call.grant.session_id == session_id {
            if call.grant.project_id != *project_id
                || call.grant.work_item_id != authority.work_item_id
                || call.context.source_session.grant != session.grant
            {
                return Err(corrupt());
            }
            if call.decision.is_none() && call.retired_at_unix_ms.is_none() {
                return Err(transition());
            }
            calls.push(call);
        }
    }
    calls.sort_by_key(|call| call.grant.review_id);
    let base_review_count = u16::try_from(calls.len()).map_err(|_| corrupt())?;
    let base_window_count = u16::try_from(
        session
            .continuation
            .as_ref()
            .map_or(0, |state| state.authorizations.len()),
    )
    .map_err(|_| corrupt())?;
    let capacity = session
        .grant
        .max_model_calls
        .checked_sub(session.model_calls)
        .ok_or_else(corrupt)?
        .min(
            session
                .grant
                .max_tool_calls
                .checked_sub(session.tool_calls)
                .ok_or_else(corrupt)?,
        )
        .min(
            ADAPTIVE_RESUME_MAX_REVIEWS
                .checked_sub(base_review_count)
                .ok_or_else(corrupt)?,
        );
    if capacity == 0 {
        return Err(transition());
    }
    let project_payload_digest: String = connection.query_row(
        "SELECT payload_digest FROM company_entities WHERE tenant_id=?1 AND entity_kind='project' AND entity_id=?2",
        params![tenant.0, project_id.0], |row| row.get(0),
    )?;
    let source = AdaptiveResumeSourceV1 {
        tenant_id: tenant.clone(),
        project_id: project_id.clone(),
        work_item_id: authority.work_item_id.clone(),
        session_id,
        expected_project_version: project.version,
        expected_session_version: session.version,
        project_payload_digest,
        root_entry_digest,
        head_entry_digest,
        continuation_history_digest: crate::adaptive_budget_history_digest(&session.continuation)?,
        review_history_digest: canonical_sha256(
            "sentinel.workflow.adaptive-resume-review-history.v1",
            &calls,
        )?,
        assignee_authority: authority.clone(),
        base_model_calls: session.model_calls,
        base_tool_calls: session.tool_calls,
        base_review_count,
        base_window_count,
        subject,
    };
    source.validate()?;
    Ok((source, capacity, session.grant.max_call_duration_ms))
}

fn conflict() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::IdempotencyConflict,
        false,
        "adaptive resume policy or membership is immutable",
    )
}

#[cfg(test)]
mod tests;
