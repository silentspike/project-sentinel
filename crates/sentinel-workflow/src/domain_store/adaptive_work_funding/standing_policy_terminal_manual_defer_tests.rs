// Synthetic store-backed Python-shaped lineage: adopted A, manual B, standing C.
use super::*;

fn manual_fixture(
    finish: &str,
) -> (Fixture, AdaptiveWorkFundingReceiptV1, AdaptiveWorkFundingReceiptV1, Option<AdaptiveLeadershipReviewCallV1>, u64) {
    manual_fixture_with_reason(finish, None)
}

fn manual_fixture_with_reason(
    finish: &str,
    reason: Option<&str>,
) -> (Fixture, AdaptiveWorkFundingReceiptV1, AdaptiveWorkFundingReceiptV1, Option<AdaptiveLeadershipReviewCallV1>, u64) {
    let (mut f, _, now) = refused_fixture();
    let a = session(&f).active_work_funding().unwrap().receipt.clone();
    let mut request = zero_call_renewal_draft(&f, &a, now).unwrap();
    request.limits.expires_at_unix_ms = now + WINDOW_MS;
    if let Some(reason) = reason { request.reason_ref = reason.to_owned(); }
    let b = f.store.authorize_adaptive_work_funding(
        &funding_operator(&f), &request, now,
    ).unwrap().1;
    let call = match finish {
        "unreviewed" => None,
        _ => {
            refresh(&mut f, &b, now + 1);
            if finish == "undispatched" {
                Some(issue(&f, now + 1).unwrap())
            } else {
                let call = dispatch(&f, now + 1);
                if finish == "defer" {
                    Some(f.store.complete_adaptive_leadership_review_call(
                        &f.leader, &defer(&call), now + 3,
                    ).unwrap())
                } else {
                    assert_eq!(finish, "unknown");
                    Some(call)
                }
            }
        }
    };
    let expiry = b.request.limits.expires_at_unix_ms;
    (f, a, b, call, expiry)
}

fn select(f: &Fixture, now: u64) -> Result<Option<AdaptiveWorkFundingEpochV1>, WorkflowError> {
    f.store.adaptive_work_funding_for_review(&f.leader.tenant_id, f.grant.session_id, now)
}

#[test]
fn python_manual_defer_expires_then_standing_c_is_selected_once_and_only_real_continue_adopts() {
    let (mut f, a, b, deferred, now) = manual_fixture("defer");
    let deferred = deferred.unwrap();
    let policy = policy(&f);
    let source = session(&f);
    let prefix = source.continuation.as_ref().unwrap().authorizations.clone();
    let old_calls = f.store.adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id).unwrap();
    let before = rows(&f.store);
    // Exclusion does not turn the expired B into employee authority or a review.
    assert!(select(&f, now).unwrap().is_none());
    assert!(zero_call_renewal_draft(&f, &a, now).is_err(), "ordinary manual API must not bypass B");
    let c = propose(&f, &policy, now).unwrap().unwrap().1;
    assert_eq!(c.request.reason_ref, policy.reason_ref(deferred.grant.review_id).unwrap());
    assert_eq!(c.request.source.predecessor_receipt_digest, Some(a.receipt_digest().unwrap()));
    assert_eq!((c.request.limits.additional_model_calls, c.request.limits.additional_tool_calls), (0, 0));
    assert_eq!((c.resulting_model_call_ceiling().unwrap(), c.resulting_tool_call_ceiling().unwrap()), (64, 64));
    assert_eq!(session(&f), source);
    let after = rows(&f.store);
    // Old 53's selector sees both B and C as unadopted, without a supersession.
    let old_pending: Vec<_> = [&a, &b, &c].into_iter().filter(|receipt| {
        !source.continuation.as_ref().unwrap().authorizations.iter().any(|authorization| {
            authorization.work_funding.as_ref().is_some_and(|epoch| epoch.receipt == **receipt)
        })
    }).collect();
    assert_eq!(old_pending, vec![&b, &c]);
    assert_eq!(after[0].len(), before[0].len() + 1);
    assert_eq!(after[2].len(), before[2].len() + 1);
    for index in [1, 3, 4, 5] { assert_eq!(after[index], before[index]); }
    assert!(before[0].iter().all(|row| after[0].contains(row)));
    assert!(before[2].iter().all(|row| after[2].contains(row)));
    assert_eq!(select(&f, now + 1).unwrap().unwrap().receipt, c);
    assert_eq!(propose(&f, &policy, now + 1).unwrap(), Some((true, c.clone())));
    assert_eq!(rows(&f.store), after);
    f.store = WorkflowStore::open(&f.path).unwrap();
    assert_eq!(select(&f, now + 1).unwrap().unwrap().receipt, c);
    refresh(&mut f, &c, now + 1);
    let review = dispatch(&f, now + 1);
    assert_eq!(session(&f), source);
    assert_eq!(f.store.propose_standing_work_funding_checked_with_clock(
        &funding_operator(&f), &policy, &f.grant.project_id, f.grant.session_id,
        &source.grant.authority, || panic!("sealed C replay sampled a clock"),
        || panic!("sealed C replay ran issuance checks"),
    ).unwrap(), Some((true, c.clone())));
    let completed = f.store.complete_adaptive_leadership_review_call(
        &f.leader, &continued(&review, now + 3, 5), now + 3,
    ).unwrap();
    let adopted = session(&f);
    assert_eq!(adopted.grant, source.grant);
    assert_eq!((adopted.model_calls, adopted.tool_calls), (source.model_calls, source.tool_calls));
    assert_eq!(adopted.active_work_funding().unwrap().receipt, c);
    assert_eq!((adopted.funded_model_call_ceiling(), adopted.funded_tool_call_ceiling()), (64, 64));
    let windows = &adopted.continuation.as_ref().unwrap().authorizations;
    assert_eq!(windows.len(), prefix.len() + 1);
    assert!(windows.starts_with(&prefix));
    assert_eq!(completed.continuation.as_ref(), windows.last());
    let retained = f.store.adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id).unwrap();
    assert!(old_calls.iter().all(|call| retained.contains(call)));
    assert_eq!(f.store.adaptive_work_funding(&f.leader.tenant_id, f.grant.session_id, b.request.operation_id).unwrap(), Some(b));
    assert_eq!(f.store.adaptive_leadership_review_call(&f.leader.tenant_id, deferred.grant.review_id).unwrap(), Some(deferred));
    // Historical exclusion survives adoption and actual later journal progress.
    let progressed = observe_budget_inspection(&f, adopted, now + 4);
    let next = progressed.active_deadline_ms();
    let selected = select(&f, next).unwrap().unwrap();
    assert!(selected.same_epoch(completed.grant.work_funding.as_ref().unwrap()));
    assert!(zero_call_renewal_draft(&f, &c, next).is_ok());
    f.store = WorkflowStore::open(&f.path).unwrap();
    assert_eq!(select(&f, next).unwrap().unwrap(), selected);
}

#[test]
fn manual_live_undispatched_unknown_and_unreviewed_receipts_are_never_excluded() {
    for finish in ["defer", "undispatched", "unknown", "unreviewed"] {
        let (f, _, b, _, expiry) = manual_fixture(finish);
        let now = if finish == "defer" { expiry - 1 } else { expiry + 1 };
        let before = rows(&f.store);
        assert_eq!(propose(&f, &policy(&f), now).unwrap(), None, "{finish}");
        assert_eq!(rows(&f.store), before);
        let selected = select(&f, now).unwrap();
        assert!(selected.is_none(), "{finish}");
        assert_eq!(f.store.adaptive_work_funding(&f.leader.tenant_id, f.grant.session_id, b.request.operation_id).unwrap(), Some(b));
    }
}

#[test]
fn standing_looking_receipt_is_never_treated_as_an_excludable_manual_proposal() {
    let (f, _, _, _, now) = manual_fixture_with_reason(
        "defer", Some("standing-funding:unrecognized-policy:99:00000000-0000-4000-8000-000000000001"),
    );
    let before = rows(&f.store);
    assert_eq!(propose(&f, &policy(&f), now).unwrap(), None);
    assert_eq!(rows(&f.store), before);
}

#[test]
fn new_standing_defer_even_after_expiry_never_opens_another_generation_or_slot() {
    let (mut f, _, _, _, now) = manual_fixture("defer");
    let policy = policy(&f);
    let c = propose(&f, &policy, now).unwrap().unwrap().1;
    refresh(&mut f, &c, now + 1);
    let review = dispatch(&f, now + 1);
    let refusal = f.store.complete_adaptive_leadership_review_call(
        &f.leader, &defer(&review), now + 3,
    ).unwrap();
    let before = rows(&f.store);
    let expired = c.request.limits.expires_at_unix_ms + 1;
    for at in [now + 4, expired] {
        assert_eq!(propose(&f, &policy, at).unwrap(), Some((true, c.clone())));
        let mut another = policy.clone();
        another.generation += 1;
        assert_eq!(propose(&f, &another, at).unwrap(), None);
        assert_eq!(select(&f, at).unwrap(), None);
    }
    assert_eq!(rows(&f.store), before);
    assert_eq!(f.store.adaptive_leadership_review_call(&f.leader.tenant_id, refusal.grant.review_id).unwrap(), Some(refusal));
}

#[test]
fn missing_completed_event_cannot_turn_projection_defer_into_exclusion() {
    let (f, _, _, deferred, now) = manual_fixture("defer");
    let deferred = deferred.unwrap();
    Connection::open(&f.path).unwrap().execute(
        "DELETE FROM company_events WHERE event_type='adaptive_leadership_review_completed' AND operation_id=?1",
        [deferred.operation_id.to_string()],
    ).unwrap();
    let before = rows(&f.store);
    assert!(propose(&f, &policy(&f), now).is_err());
    assert_eq!(rows(&f.store), before);
}

#[test]
fn backdated_completion_moved_after_c_issuance_cannot_supply_a_historical_cut() {
    let (f, _, _, deferred, now) = manual_fixture("defer");
    let deferred = deferred.unwrap();
    let policy = policy(&f);
    let c = propose(&f, &policy, now).unwrap().unwrap().1;
    Connection::open(&f.path).unwrap().execute(
        "UPDATE company_events SET sequence=(SELECT max(sequence)+1 FROM company_events)
         WHERE event_type='adaptive_leadership_review_completed' AND operation_id=?1",
        [deferred.operation_id.to_string()],
    ).unwrap();
    let before = rows(&f.store);
    assert!(select(&f, now + 1).is_err());
    assert!(propose(&f, &policy, now + 1).is_err());
    assert_eq!(rows(&f.store), before);
    assert_eq!(f.store.adaptive_work_funding(&f.leader.tenant_id, f.grant.session_id, c.request.operation_id).unwrap(), Some(c));
}

#[test]
fn current_head_or_authority_changes_do_not_reclassify_an_old_manual_refusal() {
    let (f, _, b, deferred, now) = manual_fixture("defer");
    let deferred = deferred.unwrap();
    let connection = f.store.connection.lock().unwrap();
    validation_scope::with_scope(&connection, || {
        let source = deferred.context.source_session.clone();
        for change in ["head", "version", "authority", "tenant"] {
            let mut altered = source.clone();
            let mut head = b.request.source.resume_source.head_entry_digest.clone();
            match change {
                "head" => head = "0".repeat(64),
                "version" => altered.version += 1,
                "authority" => altered.grant.authority.assignment_version += 1,
                "tenant" => altered.grant.authority.tenant_id = TenantId::parse("foreign").unwrap(),
                _ => unreachable!(),
            }
            assert!(adaptive_work_funding::terminal_manual_exclusions_for_test(
                &connection, &f.leader.tenant_id, f.grant.session_id, &altered, &head, now,
            )?.is_empty(), "{change}");
        }
        Ok(())
    }).unwrap();
}
