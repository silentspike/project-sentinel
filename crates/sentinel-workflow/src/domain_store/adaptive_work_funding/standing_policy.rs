//! One immutable funding proposal per actual journal head and standing policy.

use super::*;
use crate::{AdaptiveCursorV1, AdaptiveLeadershipReviewDecisionKindV1, AdaptiveSessionV1};

struct StandingProposalSource<'a> {
    project_id: &'a ProjectId,
    session_id: Uuid,
    current: &'a RuntimeAuthoritySnapshotV1,
    observed: Option<(&'a ProjectV1, &'a AdaptiveSessionV1)>,
}

pub(super) fn terminal_funded_defer(
    call: &AdaptiveLeadershipReviewCallV1,
    session: &AdaptiveSessionV1,
) -> bool {
    matches!(call.grant.schema_version, 5 | 6)
        && call.grant.work_funding.is_some()
        && matches!(
            call.grant.subject,
            Some(crate::AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { .. })
        )
        && call.grant.expected_session_version == session.version
        && call.context.source_session == *session
        && call.grant.assignee_authority == session.grant.authority
        && call.dispatch.is_some()
        && call.model_response_digest.is_some()
        && call.retired_at_unix_ms.is_none()
        && call.continuation.is_none()
        && call.decision.as_ref().is_some_and(|decision| {
            matches!(
                decision.decision,
                AdaptiveLeadershipReviewDecisionKindV1::DeferBudget { .. }
            )
        })
}

impl WorkflowStore {
    /// Trusted-caller wrapper without external rechecks. Production callers use
    /// the checked variant with a trusted policy and registered issuer. Neither
    /// this proposal nor historical replay authorizes a continuation or retry.
    pub fn propose_standing_work_funding_with_clock<Clock: FnMut() -> u64>(
        &self,
        issuer: &AuthenticatedCompanyPrincipalV1,
        policy: &crate::StandingWorkFundingPolicyV1,
        project_id: &ProjectId,
        session_id: Uuid,
        current: &RuntimeAuthoritySnapshotV1,
        clock: Clock,
    ) -> Result<Option<(bool, AdaptiveWorkFundingReceiptV1)>, WorkflowError> {
        self.propose_standing_work_funding_checked_with_clock(
            issuer,
            policy,
            project_id,
            session_id,
            current,
            clock,
            || Ok(()),
        )
    }

    /// Rechecks external policy, registered issuer and employee health under the
    /// SQL fence. The callback MUST NOT call WorkflowStore (its mutex is held).
    /// Policy DTO validation proves no host-file provenance or configuration authority.
    /// Historical replay samples neither clock nor callback and grants no new capacity.
    pub fn propose_standing_work_funding_checked_with_clock<Clock, Check>(
        &self,
        issuer: &AuthenticatedCompanyPrincipalV1,
        policy: &crate::StandingWorkFundingPolicyV1,
        project_id: &ProjectId,
        session_id: Uuid,
        current: &RuntimeAuthoritySnapshotV1,
        clock: Clock,
        before_commit: Check,
    ) -> Result<Option<(bool, AdaptiveWorkFundingReceiptV1)>, WorkflowError>
    where
        Clock: FnMut() -> u64,
        Check: FnMut() -> Result<(), WorkflowError>,
    {
        self.propose_standing_work_funding_from_source(
            issuer,
            policy,
            StandingProposalSource { project_id, session_id, current, observed: None },
            clock,
            before_commit,
        )
    }

    /// Observations are descriptive, not authority. New issuance compares both
    /// complete values to authenticated SQL state before running external checks.
    /// Sealed replay deliberately ignores stale observations, clocks and callbacks.
    /// The callback MUST NOT call WorkflowStore while its SQL mutex is held.
    pub fn propose_standing_work_funding_observed_with_clock<Clock, Check>(
        &self,
        issuer: &AuthenticatedCompanyPrincipalV1,
        policy: &crate::StandingWorkFundingPolicyV1,
        observed: (&ProjectV1, &AdaptiveSessionV1),
        clock: Clock,
        before_commit: Check,
    ) -> Result<Option<(bool, AdaptiveWorkFundingReceiptV1)>, WorkflowError>
    where
        Clock: FnMut() -> u64,
        Check: FnMut() -> Result<(), WorkflowError>,
    {
        self.propose_standing_work_funding_from_source(
            issuer,
            policy,
            StandingProposalSource {
                project_id: &observed.0.project_id,
                session_id: observed.1.grant.session_id,
                current: &observed.1.grant.authority,
                observed: Some(observed),
            },
            clock,
            before_commit,
        )
    }

    fn propose_standing_work_funding_from_source<Clock, Check>(
        &self,
        issuer: &AuthenticatedCompanyPrincipalV1,
        policy: &crate::StandingWorkFundingPolicyV1,
        source: StandingProposalSource<'_>,
        mut clock: Clock,
        mut before_commit: Check,
    ) -> Result<Option<(bool, AdaptiveWorkFundingReceiptV1)>, WorkflowError>
    where
        Clock: FnMut() -> u64,
        Check: FnMut() -> Result<(), WorkflowError>,
    {
        let StandingProposalSource { project_id, session_id, current, observed } = source;
        policy.validate_for(issuer)?;
        current.validate()?;
        project_id.validate()?;
        crate::adaptive_resume_policy::require_resume_policy_operator(issuer, &current.tenant_id)?;
        if session_id.is_nil() || current.project_id != *project_id {
            return Err(unauthorized());
        }
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let scope = validation_scope::enter(&transaction)?;
        let (session, _, head_digest) =
            crate::store::adaptive::adaptive_resume_journal_source(&transaction, session_id)?
                .ok_or_else(not_found)?;
        if session.grant.authority != *current {
            return Err(unauthorized());
        }
        let calls = super::super::adaptive_leadership_review::calls_for_session(
            &transaction,
            &current.tenant_id,
            session_id,
        )?;
        let receipts = funding_receipts(&transaction, &current.tenant_id, session_id)?;
        let adopted = adopted_epochs(&session);
        let consumed =
            consumed_validated_receipts(&transaction, &current.tenant_id, session_id, &receipts)?;
        let operation_id = policy.operation_id(session_id, &head_digest)?;

        // A later same-head review changes review history, not this immutable slot.
        // Validate the original cause, not only the latest refusal, before replay.
        if let Some(prior) = receipts
            .iter()
            .find(|receipt| receipt.request.operation_id == operation_id)
        {
            let source = &prior.request.source.resume_source;
            let mut cause_count = 0;
            for cause in calls
                .iter()
                .filter(|call| terminal_funded_defer(call, &session))
            {
                if policy.reason_ref(cause.grant.review_id)? == prior.request.reason_ref {
                    cause_count += 1;
                }
            }
            if prior.issuer_principal != *issuer
                || source.project_id != *project_id
                || source.session_id != session_id
                || source.expected_session_version != session.version
                || source.head_entry_digest != head_digest
                || source.assignee_authority != *current
                || prior
                    .request
                    .source
                    .supersedes_unused_receipt_digest
                    .is_some()
                || prior.request.source.expired_reviewed_replacement.is_some()
                || prior.request.limits
                    != policy.limits(session.grant.max_call_duration_ms, prior.issued_at_unix_ms)?
                || cause_count != 1
            {
                return Err(funding_conflict());
            }
            // New terminal-manual history is still authenticated on replay;
            // this uses the receipt's immutable cut, never a fresh clock/check.
            terminal_manual_defer::excluded_by_standing_receipts(
                &transaction, &receipts, &calls, &session,
            )?;
            let prior = prior.clone();
            scope.finish()?;
            return Ok(Some((true, prior)));
        }

        if let Some((observed_project, observed_session)) = observed {
            let project: ProjectV1 = get_entity(
                &transaction, &current.tenant_id, "project", &project_id.0,
            )?.ok_or_else(not_found)?;
            if project != *observed_project || session != *observed_session {
                return Err(WorkflowError::new(
                    WorkflowErrorCode::AuthorityConflict,
                    false,
                    "standing funding observed source changed",
                ));
            }
        }

        // Authenticate all retained history before treating a manual pending
        // proposal or an exhausted bound as ordinary ineligibility.
        let historical_terminal = terminal_manual_defer::excluded_by_standing_receipts(
            &transaction, &receipts, &calls, &session,
        )?;
        let pending: Vec<_> = receipts.iter().filter(|receipt| {
            !adopted.iter().any(|epoch| epoch.receipt == **receipt)
                && !consumed.contains(&receipt.funding_id)
                && !historical_terminal.contains(&receipt.funding_id)
        }).collect();
        if pending.len() > 1
            || pending.iter().any(|receipt| terminal_manual_defer::candidate(receipt, &calls, &session).is_none())
            || receipts.len() >= 128
            || !matches!(session.cursor, AdaptiveCursorV1::ReadyForModel)
            || session.active_work_funding().is_none()
            || session.model_calls >= session.funded_model_call_ceiling()
            || session.tool_calls >= session.funded_tool_call_ceiling()
            || calls
                .iter()
                .any(|call| call.decision.is_none() && call.retired_at_unix_ms.is_none())
        {
            scope.finish()?;
            return Ok(None);
        }
        let reviews = u16::try_from(calls.len()).map_err(|_| corrupt())?;
        let windows = session
            .continuation
            .as_ref()
            .map_or(0, |state| state.authorizations.len());
        if reviews
            .checked_add(policy.additional_reviews)
            .is_none_or(|total| total > crate::ADAPTIVE_RESUME_MAX_REVIEWS)
            || windows
                .checked_add(usize::from(policy.additional_windows))
                .is_none_or(|total| total > usize::from(crate::ADAPTIVE_RESUME_MAX_REVIEWS))
        {
            scope.finish()?;
            return Ok(None);
        }
        let mut latest: Option<(u16, &AdaptiveLeadershipReviewCallV1)> = None;
        let mut ordinals = std::collections::BTreeSet::new();
        for call in &calls {
            if call.grant.expected_session_version != session.version {
                continue;
            }
            let Some(epoch) = &call.grant.work_funding else {
                continue;
            };
            let ordinal = epoch.binding.ordinal;
            if !ordinals.insert(ordinal) {
                return Err(corrupt());
            }
            if latest.as_ref().is_none_or(|(prior, _)| ordinal > *prior) {
                latest = Some((ordinal, call));
            }
        }
        let Some((_, cause)) = latest else {
            scope.finish()?;
            return Ok(None);
        };
        if !terminal_funded_defer(cause, &session) {
            scope.finish()?;
            return Ok(None);
        }
        if pending.iter().any(|receipt| {
            terminal_manual_defer::candidate(receipt, &calls, &session)
                .is_none_or(|manual_defer| manual_defer.grant.review_id != cause.grant.review_id)
        }) {
            scope.finish()?;
            return Ok(None);
        }
        let eligible_at = clock();
        if eligible_at < session.updated_at_ms || eligible_at < cause.updated_at_unix_ms {
            return Err(transition());
        }
        if !session.model_window_exhausted_at(eligible_at) {
            scope.finish()?;
            return Ok(None);
        }
        let terminal = terminal_manual_defer::excluded_at_head(
            &transaction, &receipts, &calls, &session, &head_digest, eligible_at,
        )?;
        if pending.iter().any(|receipt| !terminal.contains(&receipt.funding_id)) {
            scope.finish()?;
            return Ok(None);
        }
        // Prove the complete fresh source before choosing the first issuance time.
        let (fresh, _) = super::super::adaptive_resume_policy::fresh_resume_source(
            &transaction,
            &current.tenant_id,
            project_id,
            session_id,
            eligible_at,
            None,
        )?;
        // Provisional limits prove eligibility only; they never become a receipt.
        let probe_limits = policy.limits(session.grant.max_call_duration_ms, eligible_at)?;
        let (source, duration) = require_unfunded_source_with_candidate(
            &transaction,
            &current.tenant_id,
            project_id,
            session_id,
            eligible_at,
            Some(PendingFundingReplacement::StandingAfterManualDefer),
            &probe_limits,
        )?;
        if source.resume_source != fresh || duration != probe_limits.max_call_duration_ms {
            return Err(transition());
        }
        before_commit()?;
        let issued_at = clock();
        if issued_at < eligible_at || issued_at < cause.updated_at_unix_ms {
            return Err(transition());
        }
        let limits = policy.limits(session.grant.max_call_duration_ms, issued_at)?;
        let request = AdaptiveWorkFundingRequestV1 {
            schema_version: 1,
            operation_id,
            source,
            limits,
            reason_ref: policy.reason_ref(cause.grant.review_id)?,
        };
        request.validate_at(issuer, issued_at)?;
        let receipt = AdaptiveWorkFundingReceiptV1 {
            schema_version: 1,
            funding_id: adaptive_work_funding_id(&current.tenant_id, session_id, operation_id)?,
            request,
            issuer_principal: issuer.clone(),
            issued_at_unix_ms: issued_at,
        };
        receipt.validate()?;
        scope.finish()?;
        put_funding_receipt(&transaction, &receipt)?;
        before_commit()?;
        let commit_at = clock();
        if commit_at < issued_at {
            return Err(transition());
        }
        receipt.request.validate_at(issuer, commit_at)?;
        transaction.commit()?;
        Ok(Some((false, receipt)))
    }
}
