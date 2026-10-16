//! Selection-only exclusions. No receipt, review or adoption validator calls here.
use super::*;

const COMPLETED: &str = "adaptive_leadership_review_completed";
const STANDING: &str = "standing-funding:";

pub(super) fn candidate<'a>(
    receipt: &AdaptiveWorkFundingReceiptV1,
    calls: &'a [AdaptiveLeadershipReviewCallV1],
    source: &crate::AdaptiveSessionV1,
) -> Option<&'a AdaptiveLeadershipReviewCallV1> {
    if receipt.request.reason_ref.starts_with(STANDING)
        || receipt.request.source.expired_reviewed_replacement.is_some()
        || receipt.request.source.supersedes_unused_receipt_digest.is_some()
        || adopted_epochs(source).iter().any(|epoch| epoch.receipt == *receipt)
    {
        return None;
    }
    let mut found = None;
    for call in calls.iter().filter(|call| {
        call.grant.work_funding.as_ref().is_some_and(|epoch| epoch.receipt == *receipt)
    }) {
        if call.retired_at_unix_ms.is_some()
            && call.dispatch.is_none() && call.decision.is_none()
            && call.model_response_digest.is_none() && call.continuation.is_none()
            && call.resolution_event_id.is_none()
        {
            continue;
        }
        if found.is_some() || !super::standing_policy::terminal_funded_defer(call, source) {
            return None;
        }
        found = Some(call);
    }
    found
}

fn matches_head(
    anchor: &crate::AdaptiveResumeSourceV1,
    source: &crate::AdaptiveSessionV1,
    head_digest: &str,
) -> Result<bool, WorkflowError> {
    let authority = &source.grant.authority;
    Ok(anchor.tenant_id == authority.tenant_id
        && anchor.project_id == authority.project_id
        && anchor.work_item_id == authority.work_item_id
        && anchor.session_id == source.grant.session_id
        && anchor.assignee_authority == *authority
        && anchor.expected_session_version == source.version
        && anchor.head_entry_digest == head_digest
        && anchor.base_model_calls == source.model_calls
        && anchor.base_tool_calls == source.tool_calls
        && usize::from(anchor.base_window_count)
            == source.continuation.as_ref().map_or(0, |state| state.authorizations.len())
        && anchor.continuation_history_digest == crate::adaptive_budget_history_digest(&source.continuation)?)
}

fn require_completed_before(
    connection: &Connection,
    call: &AdaptiveLeadershipReviewCallV1,
    time: u64,
    sequence: u64,
) -> Result<bool, WorkflowError> {
    // Authenticate exact dispatch/response/Defer bytes, not just their presence
    // in a mutable projection or a timestamp that can be backdated later.
    adaptive_resume_policy::require_leaf_event(
        connection, &call.grant.leadership_principal.tenant_id, &call.grant.project_id,
        COMPLETED, call.operation_id,
        &canonical_sha256("sentinel.workflow.adaptive-leadership-call.v1", call)?,
        &call.grant.leadership_principal, call.updated_at_unix_ms, call,
    )?;
    let completed: i64 = connection.query_row(
        "SELECT sequence FROM company_events WHERE tenant_id=?1 AND event_type=?2 AND operation_id=?3",
        params![call.grant.leadership_principal.tenant_id.0, COMPLETED, call.operation_id.to_string()],
        |row| row.get(0),
    )?;
    let epoch = call.grant.work_funding.as_ref().ok_or_else(corrupt)?;
    crate::store::adaptive::require_funding_journal_source(connection, epoch, &call.context.source_session)?;
    Ok(call.updated_at_unix_ms <= time && stored_u64(completed)? < sequence)
}

pub(super) fn excluded_at_head(
    connection: &Connection,
    receipts: &[AdaptiveWorkFundingReceiptV1],
    calls: &[AdaptiveLeadershipReviewCallV1],
    source: &crate::AdaptiveSessionV1,
    head_digest: &str,
    now_ms: u64,
) -> Result<BTreeSet<String>, WorkflowError> {
    if calls.iter().any(|call| call.decision.is_none() && call.retired_at_unix_ms.is_none()) {
        return Ok(BTreeSet::new());
    }
    let last: i64 = connection.query_row("SELECT coalesce(max(sequence),0) FROM company_events", [], |row| row.get(0))?;
    let cut = stored_u64(last)?.checked_add(1).ok_or_else(corrupt)?;
    let mut result = BTreeSet::new();
    for receipt in receipts {
        let Some(call) = candidate(receipt, calls, source) else { continue; };
        if receipt.request.limits.expires_at_unix_ms <= now_ms
            && matches_head(&receipt.request.source.resume_source, source, head_digest)?
            && require_completed_before(connection, call, now_ms, cut)?
        {
            result.insert(receipt.funding_id.clone());
        }
    }
    Ok(result)
}

fn standing_cause(receipt: &AdaptiveWorkFundingReceiptV1) -> Result<Option<Uuid>, WorkflowError> {
    let Some(reason) = receipt.request.reason_ref.strip_prefix(STANDING) else { return Ok(None); };
    let fields: Vec<_> = reason.split(':').collect();
    if fields.len() != 3 || receipt.request.limits.additional_model_calls != 0
        || receipt.request.limits.additional_tool_calls != 0
        || receipt.request.source.supersedes_unused_receipt_digest.is_some()
        || receipt.request.source.expired_reviewed_replacement.is_some()
    {
        return Ok(None);
    }
    let Ok(generation) = fields[1].parse::<u64>() else { return Ok(None); };
    let Ok(cause) = Uuid::parse_str(fields[2]) else { return Ok(None); };
    let policy = crate::StandingWorkFundingPolicyV1 {
        schema_version: 1, policy_id: fields[0].to_owned(), generation,
        tenant_id: receipt.issuer_principal.tenant_id.clone(),
        issuer_principal_id: receipt.issuer_principal.principal_id.clone(),
        epoch_duration_ms: receipt.request.limits.expires_at_unix_ms
            .checked_sub(receipt.issued_at_unix_ms).ok_or_else(corrupt)?,
        additional_reviews: receipt.request.limits.additional_reviews,
        additional_windows: receipt.request.limits.additional_windows,
        max_window_ms: receipt.request.limits.max_window_ms,
    };
    let anchor = &receipt.request.source.resume_source;
    // This reconstructs a descriptive key only. Issuer and event authenticity
    // were already established by the caller's complete receipt inventory.
    if policy.validate_for(&receipt.issuer_principal).is_err()
        || policy.reason_ref(cause).ok().as_ref() != Some(&receipt.request.reason_ref)
        || policy.operation_id(anchor.session_id, &anchor.head_entry_digest).ok() != Some(receipt.request.operation_id)
    {
        return Ok(None);
    }
    Ok(Some(cause))
}

fn closed_review_cut(
    connection: &Connection,
    anchor: &crate::AdaptiveResumeSourceV1,
    calls: &[AdaptiveLeadershipReviewCallV1],
    before_sequence: u64,
    time: u64,
) -> Result<bool, WorkflowError> {
    validation_scope::memoize(
        connection, "terminal-manual-defer-review-cut",
        &(&anchor.tenant_id, anchor.session_id, before_sequence, time,
            anchor.base_review_count, &anchor.review_history_digest),
        || {
            // This existing prologue authenticates every bounded tenant trace,
            // including rows after the cut and unrelated sessions, without
            // loading current call membership from a receipt validator.
            reviewed_funding_ids(connection, &anchor.tenant_id)?;
            let mut statement = connection.prepare(
                "SELECT sequence FROM company_events WHERE tenant_id=?1
                 AND event_type GLOB 'adaptive_leadership_review_*' AND sequence<?2
                 ORDER BY sequence LIMIT 4097",
            )?;
            let ids = statement.query_map(
                params![anchor.tenant_id.0, sql_u64(before_sequence)?], |row| row.get::<_, i64>(0),
            )?.collect::<Result<Vec<_>, _>>()?;
            if ids.len() > 4096 { return Err(corrupt()); }
            let mut states = BTreeMap::new();
            for id in ids {
                let row = company_event_shape(connection, stored_u64(id)?)?.ok_or_else(corrupt)?;
                let call: AdaptiveLeadershipReviewCallV1 = decode(&row.payload)?;
                if call.grant.session_id == anchor.session_id {
                    if call.updated_at_unix_ms > time { return Ok(false); }
                    if !states.contains_key(&call.grant.review_id) {
                        validation_scope::charge_bytes(connection, 256)?;
                    }
                    states.insert(call.grant.review_id, row);
                }
            }
            if states.len() != usize::from(anchor.base_review_count) { return Ok(false); }
            let mut history = Vec::with_capacity(states.len());
            for (id, row) in &states {
                let historical: AdaptiveLeadershipReviewCallV1 = decode(&row.payload)?;
                let Some(current) = calls.iter().find(|call| call.grant.review_id == *id) else {
                    return Err(corrupt());
                };
                if historical != *current
                    || (historical.decision.is_none() && historical.retired_at_unix_ms.is_none())
                {
                    return Ok(false);
                }
                // Borrow already-owned full calls. Do not retain a second
                // decoded history or substitute per-call hashes for wire bytes.
                history.push(current);
            }
            Ok(canonical_sha256("sentinel.workflow.adaptive-resume-review-history.v1", &history)?
                == anchor.review_history_digest)
        },
    )
}

pub(super) fn excluded_by_standing_receipts(
    connection: &Connection,
    receipts: &[AdaptiveWorkFundingReceiptV1],
    calls: &[AdaptiveLeadershipReviewCallV1],
    current: &crate::AdaptiveSessionV1,
) -> Result<BTreeSet<String>, WorkflowError> {
    let mut result = BTreeSet::new();
    for standing in receipts {
        let Some(cause) = standing_cause(standing)? else { continue; };
        let Some(call) = calls.iter().find(|call| call.grant.review_id == cause) else { return Err(corrupt()); };
        let source = &call.context.source_session;
        let Some(manual) = call.grant.work_funding.as_ref().map(|epoch| &epoch.receipt) else { continue; };
        if !receipts.contains(manual) || adopted_epochs(current).iter().any(|epoch| epoch.receipt == *manual)
            || candidate(manual, calls, source).is_none()
        {
            continue;
        }
        let anchor = &standing.request.source.resume_source;
        if current.grant != source.grant || current.version < source.version
            || current.model_calls < source.model_calls || current.tool_calls < source.tool_calls
            || source.continuation.as_ref().is_some_and(|prior| {
                current.continuation.as_ref().is_none_or(|later| !later.authorizations.starts_with(&prior.authorizations))
            })
            || manual.request.limits.expires_at_unix_ms > standing.issued_at_unix_ms
            || !matches_head(anchor, source, &manual.request.source.resume_source.head_entry_digest)?
            || anchor.root_entry_digest != manual.request.source.resume_source.root_entry_digest
            || standing.request.source.original_model_call_ceiling != source.grant.max_model_calls
            || standing.request.source.original_tool_call_ceiling != source.grant.max_tool_calls
            || standing.request.source.predecessor_receipt_digest.as_deref()
                != source.active_work_funding().map(|epoch| epoch.binding.receipt_digest.as_str())
            || standing.request.source.current_model_call_ceiling != source.funded_model_call_ceiling()
            || standing.request.source.current_tool_call_ceiling != source.funded_tool_call_ceiling()
        {
            continue;
        }
        let sequence: i64 = connection.query_row(
            "SELECT sequence FROM company_events WHERE tenant_id=?1 AND event_type=?2 AND operation_id=?3",
            params![anchor.tenant_id.0, FUNDING_EVENT, standing.request.operation_id.to_string()],
            |row| row.get(0),
        )?;
        let sequence = stored_u64(sequence)?;
        if !require_completed_before(connection, call, standing.issued_at_unix_ms, sequence)?
            || !closed_review_cut(connection, anchor, calls, sequence, standing.issued_at_unix_ms)?
        {
            return Err(corrupt());
        }
        result.insert(manual.funding_id.clone());
    }
    Ok(result)
}
