use super::*;
use std::cell::Cell;

fn scripted_clock<'a>(times: &'a [u64], samples: &'a Cell<usize>) -> impl FnMut() -> u64 + 'a {
    move || {
        let index = samples.get();
        samples.set(index + 1);
        *times.get(index).expect("unexpected clock sample")
    }
}

fn total_changes(store: &WorkflowStore) -> i64 {
    store.connection.lock().unwrap().query_row(
        "SELECT total_changes()", [], |row| row.get(0),
    ).unwrap()
}

#[test]
fn completion_rechecks_window_funding_and_backwards_clock_after_transaction_writes() {
    for boundary in ["window", "funding", "backwards"] {
        let (f, _, now) = funded_fixture();
        let call = dispatch(&f, now);
        let issued = now + 2;
        let result = continued(&call, issued, 2);
        let final_ms = match boundary {
            "window" => result.continuation.as_ref().unwrap().deadline_ms,
            "funding" => call.grant.work_funding.as_ref().unwrap()
                .binding.limits.expires_at_unix_ms,
            _ => issued - 1,
        };
        let before = rows(&f.store);
        let changes = total_changes(&f.store);
        let samples = Cell::new(0);
        assert!(f.store.complete_adaptive_leadership_review_call_with_clock(
            &f.leader, &result, scripted_clock(&[issued, final_ms], &samples),
        ).is_err(), "{boundary}");
        assert_eq!(samples.get(), 2, "must reach the commit boundary: {boundary}");
        // SQL effects occurred, but rollback preserves all durable journals and heads.
        assert!(total_changes(&f.store) > changes, "{boundary}");
        assert_eq!(rows(&f.store), before, "{boundary}");
        assert_eq!(session(&f), call.context.source_session, "{boundary}");
    }
}

#[test]
fn defer_rechecks_review_and_funding_expiry_before_commit() {
    for boundary in ["review", "funding"] {
        let (f, _, now) = funded_fixture();
        let call = dispatch(&f, now);
        let result = defer(&call);
        let final_ms = if boundary == "review" {
            call.grant.expires_at_unix_ms
        } else {
            call.grant.work_funding.as_ref().unwrap().binding.limits.expires_at_unix_ms
        };
        let before = rows(&f.store);
        let changes = total_changes(&f.store);
        let samples = Cell::new(0);
        assert!(f.store.complete_adaptive_leadership_review_call_with_clock(
            &f.leader, &result, scripted_clock(&[now + 2, final_ms], &samples),
        ).is_err(), "{boundary}");
        assert_eq!(samples.get(), 2);
        assert!(total_changes(&f.store) > changes);
        assert_eq!(rows(&f.store), before);
    }
}

#[test]
fn sealed_normal_continue_can_cross_review_expiry_without_rebasing() {
    let (f, _, now) = funded_fixture();
    let call = dispatch(&f, now);
    let result = continued(&call, now + 2, 2);
    let original = result.continuation.clone().unwrap();
    assert!(call.grant.expires_at_unix_ms < original.deadline_ms);
    let samples = Cell::new(0);
    let completed = f.store.complete_adaptive_leadership_review_call_with_clock(
        &f.leader, &result,
        scripted_clock(&[now + 2, call.grant.expires_at_unix_ms], &samples),
    ).unwrap();
    assert_eq!(samples.get(), 2);
    assert_eq!(completed.continuation, Some(original));
    assert_eq!(completed.updated_at_unix_ms, now + 2);
}

#[test]
fn completed_continue_replay_does_not_sample_or_refresh_clock() {
    let (f, _, now) = funded_fixture();
    let call = dispatch(&f, now);
    let result = continued(&call, now + 2, 2);
    let completed = f.store.complete_adaptive_leadership_review_call(
        &f.leader, &result, now + 2,
    ).unwrap();
    let before = rows(&f.store);
    let replay = f.store.complete_adaptive_leadership_review_call_with_clock(
        &f.leader, &result, || panic!("completed replay must not sample clock"),
    ).unwrap();
    assert_eq!(replay, completed);
    assert_eq!(rows(&f.store), before);
}

#[test]
fn completed_defer_replay_does_not_sample_clock() {
    let (f, _, now) = funded_fixture();
    let call = dispatch(&f, now);
    let result = defer(&call);
    let completed = f.store.complete_adaptive_leadership_review_call(
        &f.leader, &result, now + 2,
    ).unwrap();
    let before = rows(&f.store);
    assert_eq!(f.store.complete_adaptive_leadership_review_call_with_clock(
        &f.leader, &result, || panic!("completed Defer replay must not sample clock"),
    ).unwrap(), completed);
    assert_eq!(rows(&f.store), before);
}

#[test]
fn first_local_receipt_issues_after_preflight_with_the_unchanged_chosen_window() {
    let (f, call, request, issued) = local_fixture();
    let samples = Cell::new(0);
    let fresh_issued = issued + 250;
    let (replayed, record) = f.store.authorize_adaptive_leadership_local_adoption_with_clock(
        &funding_operator(&f), &request,
        scripted_clock(&[issued, fresh_issued, fresh_issued + 1], &samples),
    ).unwrap();
    assert_eq!(samples.get(), 3);
    assert!(!replayed);
    assert_eq!(record, authority(&f, &request, fresh_issued));
    assert_eq!(record.request.decision, request.decision);
    assert_eq!(record.continuation_deadline_ms - record.issued_at_unix_ms, LOCAL_WINDOW);
    record.validate_funded_against(&call).unwrap();
    let result = local_result(&call, &record);
    let completed = f.store.complete_adaptive_leadership_review_call(
        &f.leader, &result, fresh_issued + 2,
    ).unwrap();
    assert_eq!(completed.continuation, result.continuation);
}

#[test]
fn local_preflight_expiry_and_backwards_clock_reject_before_receipt_writes() {
    for boundary in ["admission", "backwards"] {
        let (f, _, request, issued) = local_fixture();
        let issuance_ms = if boundary == "admission" {
            request.expires_at_unix_ms
        } else {
            issued - 1
        };
        let before = rows(&f.store);
        let changes = total_changes(&f.store);
        let samples = Cell::new(0);
        assert!(f.store.authorize_adaptive_leadership_local_adoption_with_clock(
            &funding_operator(&f), &request,
            scripted_clock(&[issued, issuance_ms], &samples),
        ).is_err(), "{boundary}");
        assert_eq!(samples.get(), 2);
        assert_eq!(total_changes(&f.store), changes);
        assert_eq!(rows(&f.store), before);
    }
}

#[test]
fn fresh_local_issuance_cannot_clamp_a_window_to_remaining_funding() {
    let (f, call, mut request, _) = local_fixture();
    let funding_end = call.grant.work_funding.as_ref().unwrap()
        .binding.limits.expires_at_unix_ms;
    let admission_ms = funding_end - LOCAL_WINDOW;
    request.expires_at_unix_ms = admission_ms + 1_000;
    let before = rows(&f.store);
    let changes = total_changes(&f.store);
    let samples = Cell::new(0);
    assert!(f.store.authorize_adaptive_leadership_local_adoption_with_clock(
        &funding_operator(&f), &request,
        scripted_clock(&[admission_ms, admission_ms + 1], &samples),
    ).is_err());
    assert_eq!(samples.get(), 2);
    assert_eq!(total_changes(&f.store), changes);
    assert_eq!(rows(&f.store), before);
}

#[test]
fn local_commit_rechecks_admission_window_funding_and_backwards_clock() {
    for boundary in ["admission", "window", "funding", "backwards"] {
        let (f, call, request, issued) = local_fixture();
        let final_ms = match boundary {
            "admission" => request.expires_at_unix_ms,
            "window" => issued + LOCAL_WINDOW,
            "funding" => call.grant.work_funding.as_ref().unwrap()
                .binding.limits.expires_at_unix_ms,
            _ => issued - 1,
        };
        let before = rows(&f.store);
        let changes = total_changes(&f.store);
        let samples = Cell::new(0);
        assert!(f.store.authorize_adaptive_leadership_local_adoption_with_clock(
            &funding_operator(&f), &request,
            scripted_clock(&[issued, issued, final_ms], &samples),
        ).is_err(), "{boundary}");
        assert_eq!(samples.get(), 3, "{boundary}");
        assert!(total_changes(&f.store) > changes, "{boundary}");
        assert_eq!(rows(&f.store), before, "{boundary}");
        assert!(f.store.adaptive_leadership_local_adoption(
            &request.tenant_id, request.review_id,
        ).unwrap().is_none());
    }
}

#[test]
fn exact_local_receipt_replay_and_reopen_never_sample_or_refresh_clock() {
    let (f, _, request, issued) = local_fixture();
    let operator = funding_operator(&f);
    let record = f.store.authorize_adaptive_leadership_local_adoption(
        &operator, &request, issued,
    ).unwrap().1;
    let before = rows(&f.store);
    assert_eq!(f.store.authorize_adaptive_leadership_local_adoption_with_clock(
        &operator, &request, || panic!("exact local replay must not sample clock"),
    ).unwrap(), (true, record.clone()));
    assert_eq!(rows(&f.store), before);
    let reopened = WorkflowStore::open(&f.path).unwrap();
    assert_eq!(reopened.authorize_adaptive_leadership_local_adoption_with_clock(
        &operator, &request, || panic!("reopened local replay must not sample clock"),
    ).unwrap(), (true, record));
    assert_eq!(rows(&reopened), before);
}

#[test]
fn changed_local_receipt_replay_still_conflicts_without_sampling_clock() {
    let (f, _, request, issued) = local_fixture();
    let operator = funding_operator(&f);
    f.store.authorize_adaptive_leadership_local_adoption(&operator, &request, issued).unwrap();
    let before = rows(&f.store);
    let mut changed = request;
    changed.expires_at_unix_ms += 1;
    assert_eq!(f.store.authorize_adaptive_leadership_local_adoption_with_clock(
        &operator, &changed, || panic!("conflicting replay must not sample clock"),
    ).unwrap_err().code, WorkflowErrorCode::IdempotencyConflict);
    assert_eq!(rows(&f.store), before);
}

#[test]
fn unaudited_local_completion_rechecks_admission_after_proofs() {
    let (f, call, request, issued) = local_fixture();
    let record = f.store.authorize_adaptive_leadership_local_adoption(
        &funding_operator(&f), &request, issued,
    ).unwrap().1;
    let result = local_result(&call, &record);
    let before = rows(&f.store);
    let changes = total_changes(&f.store);
    let samples = Cell::new(0);
    assert!(f.store.complete_adaptive_leadership_review_call_with_clock(
        &f.leader, &result,
        scripted_clock(&[issued, request.expires_at_unix_ms], &samples),
    ).is_err());
    assert_eq!(samples.get(), 2);
    assert!(total_changes(&f.store) > changes);
    assert_eq!(rows(&f.store), before);
}

#[test]
fn audited_local_completion_retains_clocks_after_admission_expiry_and_on_late_replay() {
    let (f, call, request, issued) = local_fixture();
    let record = f.store.authorize_adaptive_leadership_local_adoption(
        &funding_operator(&f), &request, issued,
    ).unwrap().1;
    let result = local_result(&call, &record);
    let audit = sealed_audit(&call, &result, issued + 1);
    let samples = Cell::new(0);
    let completed = f.store
        .complete_adaptive_leadership_review_call_with_local_adoption_audit_and_clock(
            &f.leader, &result, &audit,
            scripted_clock(&[request.expires_at_unix_ms, request.expires_at_unix_ms + 1], &samples),
        ).unwrap();
    assert_eq!(samples.get(), 2);
    assert_eq!(completed.continuation, result.continuation);
    let before = rows(&f.store);
    let replay_samples = Cell::new(0);
    let funding_end = call.grant.work_funding.as_ref().unwrap()
        .binding.limits.expires_at_unix_ms;
    assert_eq!(f.store
        .complete_adaptive_leadership_review_call_with_local_adoption_audit_and_clock(
            &f.leader, &result, &audit,
            scripted_clock(&[funding_end + 1], &replay_samples),
        ).unwrap(), completed);
    assert_eq!(replay_samples.get(), 1);
    assert_eq!(rows(&f.store), before);
}

#[test]
fn audited_local_completion_cannot_cross_productive_or_funding_deadline() {
    for boundary in ["window", "funding"] {
        let (f, call, request, issued) = local_fixture();
        let record = f.store.authorize_adaptive_leadership_local_adoption(
            &funding_operator(&f), &request, issued,
        ).unwrap().1;
        let result = local_result(&call, &record);
        let audit = sealed_audit(&call, &result, issued + 1);
        let final_ms = if boundary == "window" {
            record.continuation_deadline_ms
        } else {
            call.grant.work_funding.as_ref().unwrap().binding.limits.expires_at_unix_ms
        };
        let before = rows(&f.store);
        let changes = total_changes(&f.store);
        let samples = Cell::new(0);
        assert!(f.store
            .complete_adaptive_leadership_review_call_with_local_adoption_audit_and_clock(
                &f.leader, &result, &audit,
                scripted_clock(&[request.expires_at_unix_ms, final_ms], &samples),
            ).is_err(), "{boundary}");
        assert_eq!(samples.get(), 2);
        assert!(total_changes(&f.store) > changes);
        assert_eq!(rows(&f.store), before);
    }
}

#[test]
fn removing_funded_presentation_marker_rejects_retained_membership_after_reopen() {
    let (mut f, _, now) = funded_fixture();
    f.context.presentation_version = Some(2);
    f.context.validate(&f.grant).unwrap();
    let call = issue(&f, now).unwrap();
    let reopened = WorkflowStore::open(&f.path).unwrap();
    assert_eq!(reopened.adaptive_leadership_review_call(
        &f.leader.tenant_id, call.grant.review_id,
    ).unwrap(), Some(call.clone()));

    let retained_digest = call.context_digest().unwrap();
    let mut changed = call.clone();
    changed.context.presentation_version = None;
    changed.validate_entity().unwrap();
    assert_eq!(changed.grant, call.grant);
    assert_eq!(adaptive_leadership_evidence_fingerprint(
        &changed.context.tool_catalog, &changed.context.evidence_refs,
    ).unwrap(), call.grant.evidence_fingerprint);
    assert_ne!(changed.context_digest().unwrap(), retained_digest);
    let before = rows(&reopened);
    {
        let mut connection = reopened.connection.lock().unwrap();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        // Reseal only the entity row, so rejection must come from the retained
        // context binding, not a stale outer row checksum. No proof is rewritten.
        put_entity(
            &transaction, &f.leader.tenant_id, "adaptive_leadership_review_call",
            &changed.review_key, changed.version, &changed,
        ).unwrap();
        transaction.commit().unwrap();
        require_funding_review_membership(
            &connection, &call.grant, &retained_digest, call.operation_id,
        ).unwrap();
    }
    let corrupted = rows(&reopened);
    assert_eq!(corrupted[1..], before[1..]);
    assert!(reopened.adaptive_leadership_review_call(
        &f.leader.tenant_id, call.grant.review_id,
    ).is_err());
    assert_eq!(rows(&reopened), corrupted);
    let reopened_again = WorkflowStore::open(&f.path).unwrap();
    assert!(reopened_again.adaptive_leadership_review_call(
        &f.leader.tenant_id, call.grant.review_id,
    ).is_err());
    assert_eq!(rows(&reopened_again), corrupted);
}
