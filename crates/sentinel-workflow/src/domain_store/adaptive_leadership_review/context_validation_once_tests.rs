use super::*;
use std::collections::BTreeMap;

// Frozen pre-change entity path: standalone continuation/source validation stays complete.
fn legacy_entity(
    call: &AdaptiveLeadershipReviewCallV1,
    context_digest: &mut impl FnMut() -> Result<String, WorkflowError>,
) -> Result<(), WorkflowError> {
    call.grant.validate(call.grant_issued_at_unix_ms)?;
    call.context.validate(&call.grant)?;
    validate_project(&call.context.source_project)?;
    validate_identifier(&call.allowance_id)?;
    if call.schema_version != call.grant.schema_version
        || call.review_key != call.grant.review_id.to_string()
        || call.operation_id.is_nil()
        || call.allowance_id == call.grant.review_id.to_string()
        || call.allowance_id == call.context.source_session.grant.provider_allowance_id
        || (matches!(call.grant.schema_version, 2..=6)
            && call.allowance_id == call.context.source_session.active_provider_allowance_id())
        || call.created_at_unix_ms == 0
        || call.grant_issued_at_unix_ms < call.created_at_unix_ms
        || call.updated_at_unix_ms < call.grant_issued_at_unix_ms
        || call.continuation.as_ref().is_some_and(|authorization|
            authorization.issued_at_ms > call.updated_at_unix_ms)
    {
        return Err(corrupt());
    }
    let expected = if call.retired_at_unix_ms.is_some() {
        4
    } else if call.decision.is_some() {
        3
    } else if call.dispatch.is_some() {
        2
    } else {
        1
    };
    if call.version != expected
        || call.decision.is_some() != call.model_response_digest.is_some()
        || call.retired_at_unix_ms.is_some_and(|at| {
            at != call.updated_at_unix_ms
                || at < call.grant_issued_at_unix_ms
                || call.decision.is_some()
                || call.model_response_digest.is_some()
                || call.resolution_event_id.is_some()
                || call.continuation.is_some()
        })
    {
        return Err(corrupt());
    }
    if let Some(dispatch) = &call.dispatch {
        validate_digest(&dispatch.request_digest)?;
        validate_digest(&dispatch.context_digest)?;
        if dispatch.request_id != call.request_id()
            || dispatch.context_digest != context_digest()?
            || dispatch.dispatched_at_unix_ms < call.grant_issued_at_unix_ms
            || dispatch.dispatched_at_unix_ms >= call.grant.expires_at_unix_ms
            || dispatch.dispatched_at_unix_ms > call.updated_at_unix_ms
        {
            return Err(corrupt());
        }
    }
    if let Some(decision) = &call.decision {
        if call.grant.work_funding.as_ref().is_some_and(|epoch| {
            call.updated_at_unix_ms >= epoch.binding.limits.expires_at_unix_ms
                || call.continuation.as_ref().map_or(
                    call.updated_at_unix_ms >= call.grant.expires_at_unix_ms,
                    |authorization| call.updated_at_unix_ms >= authorization.deadline_ms,
                )
        }) {
            return Err(corrupt());
        }
        decision.validate(&call.context.evidence_refs)?;
        validate_digest(call.model_response_digest.as_deref().ok_or_else(corrupt)?)?;
        let dispatch = call.dispatch.as_ref().ok_or_else(corrupt)?;
        validate_continuation(
            call,
            decision,
            &dispatch.request_digest,
            call.model_response_digest.as_deref().ok_or_else(corrupt)?,
            call.resolution_event_id,
            call.continuation.as_ref(),
        )?;
        if call.dispatch.is_none()
            || (decision.resolves_blocked() || call.continuation.is_some())
                != call.resolution_event_id.is_some()
            || call.resolution_event_id.is_some_and(|id| id.is_nil())
        {
            return Err(corrupt());
        }
    } else if call.resolution_event_id.is_some() || call.continuation.is_some() {
        return Err(corrupt());
    }
    require_subject_time(call, call.grant_issued_at_unix_ms)?;
    Ok(())
}

fn entity_result(call: &AdaptiveLeadershipReviewCallV1, optimized: bool) -> Result<(), WorkflowError> {
    if optimized {
        call.validate_entity()
    } else {
        legacy_entity(call, &mut || call.context_digest())
    }
}

fn completed_continue(funded: bool) -> (Fixture, AdaptiveLeadershipReviewCallV1) {
    let mut f = budget_fixture(if funded { 2 } else { 4 });
    let now = CONTINUATION_AT + 7;
    let call = if funded {
        let receipt = work_funding_tests::fund(&f, now);
        work_funding_tests::bind(
            &mut f,
            &receipt,
            receipt.request.source.resume_source.base_review_count + 1,
        );
        work_funding_tests::dispatch(&f, now)
    } else {
        let call = f.store.authorize_adaptive_leadership_review_call(
            &f.leader, Uuid::new_v4(), "context-once-review", &f.grant, &f.context, now,
        ).unwrap();
        f.store.claim_adaptive_leadership_review_call(&f.leader, &claim(&call), now + 1).unwrap()
    };
    let result = if funded {
        work_funding_tests::continued(&call, now + 2, 1)
    } else {
        budget_result(&call, 1, now + 2)
    };
    let completed = f.store.complete_adaptive_leadership_review_call(&f.leader, &result, now + 2).unwrap();
    (f, completed)
}

type DigestWork = BTreeMap<&'static str, (u64, u64)>;

fn measured(run: impl FnOnce() -> Result<(), WorkflowError>) -> (Result<(), WorkflowError>, DigestWork) {
    let (result, diagnostics) = crate::digest::with_canonical_digest_cache_enabled(false, || {
        crate::digest::with_digest_diagnostics(run)
    });
    let mut work = DigestWork::new();
    for record in diagnostics {
        if record.canonical.calls > 0 {
            assert_eq!(record.canonical.failures, 0);
            work.insert(record.domain.unwrap(), (record.canonical.calls, record.canonical.serialized_bytes));
        }
    }
    (result, work)
}

#[test]
fn context_once_continue_matches_legacy_and_removes_exactly_one_context_validation() {
    for funded in [false, true] {
        let (_f, call) = completed_continue(funded);
        let original = encode(&call).unwrap();
        let (legacy, old) = measured(|| entity_result(&call, false));
        let (optimized, new) = measured(|| entity_result(&call, true));
        assert_eq!(optimized, legacy);
        optimized.unwrap();
        let (context, expected) = measured(|| call.context.validate(&call.grant));
        context.unwrap();
        let mut removed = DigestWork::new();
        for (&domain, &(calls, bytes)) in &old {
            let (new_calls, new_bytes) = new.get(domain).copied().unwrap_or_default();
            let delta = (calls.checked_sub(new_calls).unwrap(), bytes.checked_sub(new_bytes).unwrap());
            if delta != (0, 0) {
                removed.insert(domain, delta);
            }
        }
        assert!(new.keys().all(|domain| old.contains_key(domain)));
        assert_eq!(removed, expected, "funded={funded}");
        let history = "sentinel.workflow.adaptive-budget-history.v1";
        let history_bytes = u64::try_from(crate::digest::serialized_json_size(
            &call.context.source_session.continuation,
        ).unwrap()).unwrap();
        assert_eq!(removed.get(history), Some(&(1, history_bytes)));
        assert_eq!(encode(&call).unwrap(), original);
        eprintln!(
            "CONTEXT_VALIDATION_ONCE funded={funded} removed_context_validations=1 removed_history_calls=1 removed_history_serialized_bytes={history_bytes}"
        );
    }
}

#[test]
fn context_once_malformed_context_and_competing_errors_match_legacy() {
    let (_f, call) = completed_continue(true);
    for mutation in 0..13 {
        let mut changed = call.clone();
        match mutation {
            0 => changed.context.presentation_version = Some(99),
            1 => changed.context.evidence_refs.clear(),
            2 => {
                let Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }) = &mut changed.grant.subject else { panic!("budget") };
                budget.continuation_history_digest = "0".repeat(64);
            }
            3 => changed.schema_version = 0,
            4 => changed.allowance_id.clear(),
            5 => changed.dispatch.as_mut().unwrap().context_digest = "invalid".into(),
            6 => changed.continuation.as_mut().unwrap().operation_id = Uuid::new_v4(),
            7 => changed.continuation.as_mut().unwrap().source_session_version += 1,
            8 => changed.resolution_event_id = Some(Uuid::new_v4()),
            9 => changed.model_response_digest = Some("invalid".into()),
            10 => {
                changed.context.presentation_version = Some(99);
                changed.allowance_id.clear();
                changed.dispatch.as_mut().unwrap().context_digest = "invalid".into();
            }
            11 => {
                changed.context.presentation_version = Some(99);
                changed.grant.work_funding.as_mut().unwrap().binding.receipt_digest = "invalid".into();
            }
            _ => {
                changed.continuation.as_mut().unwrap().operation_id = Uuid::nil();
                changed.context.source_project.subscription_call.as_mut().unwrap().grant.model.push_str("-wrong");
            }
        }
        let legacy = entity_result(&changed, false);
        assert!(legacy.is_err(), "mutation {mutation}");
        assert_eq!(entity_result(&changed, true), legacy, "mutation {mutation}");
    }
    let mut context_first = call.clone();
    context_first.context.presentation_version = Some(99);
    context_first.allowance_id.clear();
    assert_eq!(entity_result(&context_first, true), context_first.context.validate(&context_first.grant));
    let mut grant_first = context_first;
    grant_first.grant.work_funding.as_mut().unwrap().binding.receipt_digest = "invalid".into();
    assert_eq!(entity_result(&grant_first, true), grant_first.grant.validate(grant_first.grant_issued_at_unix_ms));
}

#[test]
fn context_once_foreign_token_cannot_authorize_even_an_equal_call_clone() {
    let (_f, call) = completed_continue(true);
    let proof = call.validate_entity_with_context_digest(&mut || call.context_digest()).unwrap();
    require_adaptive_allowance_source_with_context(&call, Some(&proof)).unwrap();
    let equal_clone = call.clone();
    assert_eq!(equal_clone, call);
    assert_eq!(require_adaptive_allowance_source_with_context(&equal_clone, Some(&proof)), Err(unauthorized()));
    let dispatch = equal_clone.dispatch.as_ref().unwrap();
    assert_eq!(validate_continuation_with_context(
        &equal_clone, equal_clone.decision.as_ref().unwrap(), &dispatch.request_digest,
        equal_clone.model_response_digest.as_deref().unwrap(), equal_clone.resolution_event_id,
        equal_clone.continuation.as_ref(), Some(&proof),
    ), Err(unauthorized()));
    let mut malformed = equal_clone;
    malformed.context.presentation_version = Some(99);
    assert!(require_adaptive_allowance_source_with_context(&malformed, Some(&proof)).is_err());
}

#[test]
fn context_once_standalone_continuation_and_source_still_validate_context() {
    let (_f, call) = completed_continue(true);
    let (source, work) = measured(|| require_adaptive_allowance_source(&call));
    source.unwrap();
    assert_eq!(work["sentinel.workflow.adaptive-budget-history.v1"].0, 1);
    let dispatch = call.dispatch.as_ref().unwrap();
    validate_continuation(
        &call, call.decision.as_ref().unwrap(), &dispatch.request_digest,
        call.model_response_digest.as_deref().unwrap(), call.resolution_event_id,
        call.continuation.as_ref(),
    ).unwrap();
    let mut malformed = call.clone();
    malformed.context.presentation_version = Some(99);
    let expected = malformed.context.validate(&malformed.grant);
    assert!(expected.is_err());
    assert_eq!(require_adaptive_allowance_source(&malformed), expected);
    assert_eq!(validate_continuation(
        &malformed, malformed.decision.as_ref().unwrap(), &dispatch.request_digest,
        malformed.model_response_digest.as_deref().unwrap(), malformed.resolution_event_id,
        malformed.continuation.as_ref(),
    ), expected);
}

#[test]
fn context_once_non_continue_keeps_legacy_digest_work_and_results() {
    let (_f, mut call) = completed_continue(true);
    call.continuation = None;
    call.resolution_event_id = None;
    call.decision = Some(AdaptiveLeadershipReviewDecisionV1 {
        schema_version: call.grant.schema_version,
        decision: AdaptiveLeadershipReviewDecisionKindV1::DeferBudget {
            rationale: "Independent terminal refusal".into(),
            evidence_refs: vec![call.context.evidence_refs[0].clone()],
        },
    });
    let (legacy, old) = measured(|| entity_result(&call, false));
    let (optimized, new) = measured(|| entity_result(&call, true));
    assert_eq!(optimized, legacy);
    optimized.unwrap();
    assert_eq!(new, old);
}

fn full_result(
    connection: &Connection,
    call: &AdaptiveLeadershipReviewCallV1,
    optimized: bool,
) -> Result<(), WorkflowError> {
    validation_scope::with_scope(connection, || {
        if optimized {
            return call.validate_full(connection);
        }
        let mut digest = None;
        let mut compute = || {
            if digest.is_none() {
                digest = Some(call.context_digest()?);
            }
            Ok(digest.as_ref().unwrap().clone())
        };
        legacy_entity(call, &mut compute)?;
        call.validate_persisted_with_context_digest(connection, &mut compute)
    })
}

#[test]
fn context_once_full_membership_and_journal_checks_remain_independent() {
    let (f, call) = completed_continue(true);
    let mut connection = f.store.connection.lock().unwrap();
    let (legacy, old) = validation_scope::with_completed_validations(|| full_result(&connection, &call, false));
    let (optimized, new) = validation_scope::with_completed_validations(|| full_result(&connection, &call, true));
    assert_eq!(optimized, legacy);
    optimized.unwrap();
    assert_eq!(new, old);
    for target in ["membership", "journal"] {
        let tx = connection.transaction().unwrap();
        let changed = if target == "membership" {
            tx.execute(
                "DELETE FROM company_entities WHERE entity_kind='adaptive_work_funding_review' AND json_extract(payload,'$.grant.review_id')=?1",
                [call.grant.review_id.to_string()],
            ).unwrap()
        } else {
            tx.execute(
                "UPDATE workflow_operations SET request_digest='invalid' WHERE operation_namespace=?1 AND operation_id='00000000000000000001'",
                [format!("adaptive-session-v1:{}", call.grant.session_id)],
            ).unwrap()
        };
        assert!(changed > 0);
        let legacy = full_result(&tx, &call, false);
        assert!(legacy.is_err(), "{target}");
        assert_eq!(full_result(&tx, &call, true), legacy, "{target}");
        tx.rollback().unwrap();
        full_result(&connection, &call, true).unwrap();
    }
}
