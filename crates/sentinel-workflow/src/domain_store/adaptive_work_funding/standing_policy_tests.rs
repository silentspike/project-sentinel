// Include as a child of adaptive_leadership_review::tests::work_funding_tests.
// Store-backed typed decisions only, not evidence of a live model response.

use super::*;
use crate::StandingWorkFundingPolicyV1;

mod observed_source_tests {
    include!("standing_policy_observed_source_tests.rs");
}

mod terminal_manual_defer_tests {
    include!("standing_policy_terminal_manual_defer_tests.rs");
}

fn policy(f: &Fixture) -> StandingWorkFundingPolicyV1 {
    StandingWorkFundingPolicyV1 {
        schema_version: 1,
        policy_id: "standing-test".into(),
        generation: 1,
        tenant_id: f.leader.tenant_id.clone(),
        issuer_principal_id: funding_operator(f).principal_id,
        epoch_duration_ms: 3_600_000,
        additional_reviews: 4,
        additional_windows: 4,
        max_window_ms: WINDOW_MS,
    }
}

fn refused_fixture() -> (Fixture, AdaptiveLeadershipReviewCallV1, u64) {
    let (mut f, predecessor, now) = capped_renewal_fixture(5);
    bind(&mut f, &predecessor, predecessor.request.source.resume_source.base_review_count + 2);
    let call = dispatch(&f, now);
    let refused = f.store.complete_adaptive_leadership_review_call(
        &f.leader, &defer(&call), now + 2,
    ).unwrap();
    assert!(refused.dispatch.is_some() && refused.continuation.is_none());
    (f, refused, now + 3)
}

fn propose(
    f: &Fixture,
    policy: &StandingWorkFundingPolicyV1,
    now: u64,
) -> Result<Option<(bool, AdaptiveWorkFundingReceiptV1)>, WorkflowError> {
    f.store.propose_standing_work_funding_with_clock(
        &funding_operator(f), policy, &f.grant.project_id, f.grant.session_id,
        &session(f).grant.authority, || now,
    )
}

fn refresh(f: &mut Fixture, receipt: &AdaptiveWorkFundingReceiptV1, now: u64) {
    budget_context(f, now, "standing-independent-proposal");
    bind(f, receipt, receipt.request.source.resume_source.base_review_count + 1);
}

#[test]
fn standing_proposal_preserves_defer_and_spending_until_real_continue_adoption() {
    let (mut f, refused, now) = refused_fixture();
    let policy = policy(&f);
    let source = session(&f);
    let project = f.store.company_project(&f.leader.tenant_id, &f.grant.project_id).unwrap();
    let old_calls = f.store.adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id).unwrap();
    let before = rows(&f.store);
    let (replayed, receipt) = propose(&f, &policy, now).unwrap().unwrap();
    assert!(!replayed);
    assert_eq!(receipt.request.reason_ref, policy.reason_ref(refused.grant.review_id).unwrap());
    assert_eq!((receipt.request.limits.additional_model_calls, receipt.request.limits.additional_tool_calls), (0, 0));
    assert_eq!((receipt.resulting_model_call_ceiling().unwrap(), receipt.resulting_tool_call_ceiling().unwrap()), (64, 64));
    assert_eq!(session(&f), source);
    assert_eq!(f.store.company_project(&f.leader.tenant_id, &f.grant.project_id).unwrap(), project);
    assert_eq!(f.store.adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id).unwrap(), old_calls);
    let after = rows(&f.store);
    assert_eq!(after[0].len(), before[0].len() + 1);
    assert_eq!(after[2].len(), before[2].len() + 1);
    for index in [1, 3, 4, 5] {
        assert_eq!(after[index], before[index]);
    }
    assert_eq!(&after[0][..before[0].len()], before[0].as_slice());
    assert_eq!(&after[2][..before[2].len()], before[2].as_slice());

    refresh(&mut f, &receipt, now + 1);
    let review = dispatch(&f, now + 1);
    assert_eq!(review.grant.schema_version, 5);
    assert_ne!(review.grant.review_id, refused.grant.review_id);
    assert_eq!(session(&f), source);
    let completed = f.store.complete_adaptive_leadership_review_call(
        &f.leader, &continued(&review, now + 3, 5), now + 3,
    ).unwrap();
    assert!(completed.continuation.is_some());
    let adopted = session(&f);
    assert_eq!(adopted.grant, source.grant);
    assert_eq!((adopted.model_calls, adopted.tool_calls), (source.model_calls, source.tool_calls));
    assert_eq!((adopted.funded_model_call_ceiling(), adopted.funded_tool_call_ceiling()), (64, 64));
    let old_windows = &source.continuation.as_ref().unwrap().authorizations;
    let new_windows = &adopted.continuation.as_ref().unwrap().authorizations;
    assert_eq!(new_windows.len(), old_windows.len() + 1);
    assert!(new_windows.starts_with(old_windows));
    assert!(adopted.requires_fresh_observation());
    assert_eq!(f.store.adaptive_leadership_review_call(&f.leader.tenant_id, refused.grant.review_id).unwrap(), Some(refused));
}

#[test]
fn standing_new_defer_does_not_create_a_window_or_reroll_the_same_head() {
    let (mut f, refused, now) = refused_fixture();
    let policy = policy(&f);
    let source = session(&f);
    let receipt = propose(&f, &policy, now).unwrap().unwrap().1;
    refresh(&mut f, &receipt, now + 1);
    let review = dispatch(&f, now + 1);
    let next_defer = f.store.complete_adaptive_leadership_review_call(
        &f.leader, &defer(&review), now + 3,
    ).unwrap();
    assert_ne!(next_defer.grant.review_id, refused.grant.review_id);
    assert_eq!(session(&f), source);
    let before = rows(&f.store);
    let replay = f.store.propose_standing_work_funding_with_clock(
        &funding_operator(&f), &policy, &f.grant.project_id, f.grant.session_id,
        &source.grant.authority, || panic!("same-head replay sampled a clock"),
    ).unwrap();
    assert_eq!(replay, Some((true, receipt.clone())));
    assert_eq!(receipt.request.reason_ref, policy.reason_ref(refused.grant.review_id).unwrap());
    assert_eq!(rows(&f.store), before);
    assert_eq!(f.store.adaptive_work_funding_for_review(&f.leader.tenant_id, f.grant.session_id, now + 4).unwrap(), None);
    let mut another = policy;
    another.generation += 1;
    assert_eq!(propose(&f, &another, now + 4).unwrap(), None);
    assert_eq!(rows(&f.store), before);
}

#[test]
fn standing_duplicate_and_reopen_recover_exact_expired_receipt_without_clock_or_checks() {
    let (mut f, _, now) = refused_fixture();
    let policy = policy(&f);
    let receipt = propose(&f, &policy, now).unwrap().unwrap().1;
    let bytes = encode(&receipt).unwrap();
    let before = rows(&f.store);
    f.store = WorkflowStore::open(&f.path).unwrap();
    for _ in 0..2 {
        let replay = f.store.propose_standing_work_funding_checked_with_clock(
            &funding_operator(&f), &policy, &f.grant.project_id, f.grant.session_id,
            &session(&f).grant.authority, || panic!("immutable replay sampled a clock"),
            || panic!("immutable receipt replay is not fresh issuance"),
        ).unwrap().unwrap();
        assert!(replay.0);
        assert_eq!(encode(&replay.1).unwrap(), bytes);
        assert_eq!(rows(&f.store), before);
    }
    assert_eq!(propose(&f, &policy, receipt.request.limits.expires_at_unix_ms + 1).unwrap(), Some((true, receipt)));
    assert_eq!(rows(&f.store), before);
}

#[test]
fn standing_concurrent_callers_commit_one_immutable_head_slot() {
    let (f, _, now) = refused_fixture();
    let policy = policy(&f);
    let source = session(&f);
    let barrier = Arc::new(Barrier::new(2));
    let before = rows(&f.store);
    let handles: Vec<_> = (0..2).map(|_| {
        let path = f.path.clone();
        let policy = policy.clone();
        let issuer = funding_operator(&f);
        let source = source.clone();
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            let store = WorkflowStore::open(path).unwrap();
            barrier.wait();
            store.propose_standing_work_funding_with_clock(
                &issuer, &policy, &source.grant.authority.project_id,
                source.grant.session_id, &source.grant.authority, || now,
            ).unwrap().unwrap()
        })
    }).collect();
    let results: Vec<_> = handles.into_iter().map(|handle| handle.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|(replayed, _)| !replayed).count(), 1);
    assert_eq!(results[0].1, results[1].1);
    let after = rows(&f.store);
    assert_eq!(after[0].len(), before[0].len() + 1);
    assert_eq!(after[2].len(), before[2].len() + 1);
    assert_eq!(session(&f), source);
}

#[test]
fn standing_manual_pending_proposal_is_honest_none_without_clocks_or_replacement() {
    let (f, _, now) = refused_fixture();
    let policy = policy(&f);
    let predecessor = session(&f).active_work_funding().unwrap().receipt.clone();
    let request = zero_call_renewal_draft(&f, &predecessor, now).unwrap();
    let manual = f.store.authorize_adaptive_work_funding(&funding_operator(&f), &request, now).unwrap().1;
    let before = rows(&f.store);
    assert_eq!(f.store.propose_standing_work_funding_with_clock(
        &funding_operator(&f), &policy, &f.grant.project_id, f.grant.session_id,
        &session(&f).grant.authority, || panic!("manual pending proposal sampled clock"),
    ).unwrap(), None);
    assert_eq!(rows(&f.store), before);
    assert_eq!(f.store.adaptive_work_funding(&f.leader.tenant_id, f.grant.session_id, request.operation_id).unwrap(), Some(manual));
}

#[test]
fn standing_unresolved_review_with_all_funding_adopted_is_not_a_trigger() {
    let (mut f, predecessor, now) = capped_renewal_fixture(5);
    let policy = policy(&f);
    let source = session(&f);
    let receipt_count: i64 = f.store.connection.lock().unwrap().query_row(
        "SELECT count(*) FROM company_entities WHERE tenant_id=?1 AND entity_kind='adaptive_work_funding'",
        [f.leader.tenant_id.0.as_str()], |row| row.get(0),
    ).unwrap();
    assert_eq!(receipt_count, 1);
    assert_eq!(source.active_work_funding().unwrap().receipt, predecessor);
    assert!(source.continuation.as_ref().unwrap().authorizations.iter().any(|authorization| {
        authorization.work_funding.as_ref().is_some_and(|epoch| epoch.receipt == predecessor)
    }));
    assert!(source.version > predecessor.request.source.resume_source.expected_session_version);
    assert!(source.model_calls < source.funded_model_call_ceiling());
    assert!(source.tool_calls < source.funded_tool_call_ceiling());
    assert!(matches!(source.cursor, crate::AdaptiveCursorV1::ReadyForModel));

    bind(&mut f, &predecessor, predecessor.request.source.resume_source.base_review_count + 2);
    let call = dispatch(&f, now);
    assert_eq!(call.context.source_session, source);
    assert!(call.dispatch.is_some() && call.decision.is_none() && call.retired_at_unix_ms.is_none());
    let before = rows(&f.store);
    assert_eq!(f.store.propose_standing_work_funding_with_clock(
        &funding_operator(&f), &policy, &f.grant.project_id, f.grant.session_id,
        &source.grant.authority, || panic!("unresolved review sampled a clock"),
    ).unwrap(), None);
    assert_eq!(rows(&f.store), before);
    assert_eq!(session(&f), source);
}

#[test]
fn standing_unadopted_or_unresolved_funding_and_unfunded_refusals_are_not_triggers() {
    let (f, _, now) = funded_fixture();
    let policy = policy(&f);
    let before = rows(&f.store);
    assert_eq!(propose(&f, &policy, now).unwrap(), None);
    assert_eq!(rows(&f.store), before);
    let call = dispatch(&f, now);
    let before = rows(&f.store);
    assert_eq!(propose(&f, &policy, now + 1).unwrap(), None);
    assert_eq!(rows(&f.store), before);
    f.store.complete_adaptive_leadership_review_call(&f.leader, &defer(&call), now + 2).unwrap();
    let before = rows(&f.store);
    assert_eq!(propose(&f, &policy, now + 3).unwrap(), None);
    assert_eq!(rows(&f.store), before);
    let f = budget_fixture(4);
    let now = CONTINUATION_AT + 7;
    let call = dispatch_budget(&f, now);
    f.store.complete_adaptive_leadership_review_call(&f.leader, &defer(&call), now + 2).unwrap();
    let before = rows(&f.store);
    assert_eq!(propose(&f, &self::policy(&f), now + 3).unwrap(), None);
    assert_eq!(rows(&f.store), before);
}

#[test]
fn standing_invalid_policy_issuer_and_current_authority_are_errors_without_writes() {
    let (f, _, now) = refused_fixture();
    let valid = policy(&f);
    let before = rows(&f.store);
    for change in 0..5 {
        let mut policy = valid.clone();
        match change {
            0 => policy.schema_version = 0,
            1 => policy.generation = 0,
            2 => policy.issuer_principal_id = "unregistered".into(),
            3 => policy.additional_reviews = 5,
            _ => policy.additional_windows = 0,
        }
        assert!(propose(&f, &policy, now).is_err());
        assert_eq!(rows(&f.store), before);
    }
    assert!(f.store.propose_standing_work_funding_with_clock(
        &f.leader, &valid, &f.grant.project_id, f.grant.session_id,
        &session(&f).grant.authority, || now,
    ).is_err());
    for change in 0..6 {
        let mut current = session(&f).grant.authority;
        match change {
            0 => current.active = false,
            1 => current.tenant_id = TenantId::parse("foreign-tenant").unwrap(),
            2 => current.assignment_version += 1,
            3 => current.principal.principal_generation += 1,
            4 => current.capabilities.clear(),
            _ => current.profile_digest = "b".repeat(64),
        }
        assert!(f.store.propose_standing_work_funding_with_clock(
            &funding_operator(&f), &valid, &f.grant.project_id, f.grant.session_id,
            &current, || now,
        ).is_err());
        assert_eq!(rows(&f.store), before);
    }
}

#[test]
fn standing_policy_changes_without_generation_change_cannot_rewrite_receipt() {
    let (f, _, now) = refused_fixture();
    let mut policy = policy(&f);
    propose(&f, &policy, now).unwrap().unwrap();
    let before = rows(&f.store);
    policy.epoch_duration_ms += 1_000;
    assert_eq!(propose(&f, &policy, now + 1).unwrap_err().code, WorkflowErrorCode::IdempotencyConflict);
    assert_eq!(rows(&f.store), before);
}

#[test]
fn standing_corrupt_journal_receipt_event_and_defer_are_not_hidden_as_none_or_replay() {
    for target in ["journal", "funding-event", "defer"] {
        for issued in [false, true] {
            let (f, refused, now) = refused_fixture();
            let policy = policy(&f);
            if issued { propose(&f, &policy, now).unwrap().unwrap(); }
            let connection = Connection::open(&f.path).unwrap();
            match target {
                "journal" => { connection.execute(
                    "UPDATE workflow_operations SET request_digest='invalid' WHERE operation_namespace=?1 AND operation_id='00000000000000000001'",
                    [format!("adaptive-session-v1:{}", f.grant.session_id)],
                ).unwrap(); }
                "funding-event" => { connection.execute(
                    "DELETE FROM company_events WHERE event_type='adaptive_work_funding_issued'", [],
                ).unwrap(); }
                _ => { connection.execute(
                    "UPDATE company_entities SET payload_digest='invalid' WHERE entity_kind='adaptive_leadership_review_call' AND entity_id=?1",
                    [refused.grant.review_id.to_string()],
                ).unwrap(); }
            }
            let before = rows(&f.store);
            let current = f.context.source_session.grant.authority.clone();
            assert!(f.store.propose_standing_work_funding_with_clock(
                &funding_operator(&f), &policy, &f.grant.project_id, f.grant.session_id,
                &current, || now + 1,
            ).is_err(), "{target}, issued={issued}");
            assert_eq!(rows(&f.store), before);
            assert!(f.store.connection.lock().unwrap().is_autocommit());
        }
    }
}

#[test]
fn standing_backward_and_expired_precommit_clocks_roll_back_every_row() {
    let (f, _, now) = refused_fixture();
    let policy = policy(&f);
    for ticks in [
        [now - 2, now, now],
        [now, now - 1, now],
        [now, now + 1, now],
        [now, now + 1, now + 1 + policy.epoch_duration_ms],
        [u64::MAX - 1, u64::MAX, u64::MAX],
    ] {
        let before = rows(&f.store);
        let mut ticks = ticks.into_iter();
        assert!(f.store.propose_standing_work_funding_with_clock(
            &funding_operator(&f), &policy, &f.grant.project_id, f.grant.session_id,
            &session(&f).grant.authority, || ticks.next().expect("unexpected clock sample"),
        ).is_err());
        assert_eq!(rows(&f.store), before);
        assert!(f.store.connection.lock().unwrap().is_autocommit());
    }
    assert!(!propose(&f, &policy, now).unwrap().unwrap().0);
}

#[test]
fn standing_external_policy_and_health_rechecks_rollback_at_both_phases() {
    let (f, _, now) = refused_fixture();
    let policy = policy(&f);
    let current = session(&f).grant.authority;
    for fail_at in [1, 2] {
        let before = rows(&f.store);
        let mut checks = 0;
        let result = f.store.propose_standing_work_funding_checked_with_clock(
            &funding_operator(&f), &policy, &f.grant.project_id, f.grant.session_id,
            &current, || now,
            || {
                checks += 1;
                if checks == fail_at {
                    Err(WorkflowError::new(WorkflowErrorCode::AuthorityConflict, false, "standing policy revoked"))
                } else { Ok(()) }
            },
        );
        assert_eq!(result.unwrap_err().code, WorkflowErrorCode::AuthorityConflict);
        assert_eq!(checks, fail_at);
        assert_eq!(rows(&f.store), before);
        assert!(f.store.connection.lock().unwrap().is_autocommit());
    }
    let mut order = Vec::new();
    let clock = std::cell::Cell::new(0);
    let (replayed, receipt) = f.store.propose_standing_work_funding_checked_with_clock(
        &funding_operator(&f), &policy, &f.grant.project_id, f.grant.session_id,
        &current, || {
            let sample = clock.get();
            clock.set(sample + 1);
            now + sample * 100
        },
        || { order.push(clock.get()); Ok(()) },
    ).unwrap().unwrap();
    assert!(!replayed);
    assert_eq!(order, [1, 2]);
    assert_eq!(clock.get(), 3);
    assert_eq!(receipt.issued_at_unix_ms, now + 100);
    assert_eq!(receipt.request.limits.expires_at_unix_ms,
        receipt.issued_at_unix_ms + policy.epoch_duration_ms);
    assert_eq!(receipt.request.limits, policy.limits(
        receipt.request.limits.max_call_duration_ms, now + 100,
    ).unwrap());
}

#[test]
fn standing_receipt_event_failure_rolls_back_and_next_attempt_can_issue() {
    let (f, _, now) = refused_fixture();
    let policy = policy(&f);
    let connection = Connection::open(&f.path).unwrap();
    connection.execute_batch(
        "CREATE TRIGGER reject_standing_funding BEFORE INSERT ON company_events
         WHEN NEW.event_type='adaptive_work_funding_issued'
         BEGIN SELECT RAISE(ABORT,'standing publication rejected'); END;",
    ).unwrap();
    let before = rows(&f.store);
    assert!(propose(&f, &policy, now).is_err());
    assert_eq!(rows(&f.store), before);
    assert!(f.store.connection.lock().unwrap().is_autocommit());
    connection.execute_batch("DROP TRIGGER reject_standing_funding").unwrap();
    assert!(!propose(&f, &policy, now).unwrap().unwrap().0);
}

#[test]
fn standing_actual_lifetime_exhaustion_is_not_refunded() {
    let (f, _, _) = capped_renewal_fixture(62);
    let mut source = session(&f);
    let mut at = source.updated_at_ms + 1;
    while source.model_calls < 64 {
        source = observe_budget_inspection(&f, source, at);
        at += 4;
    }
    let before = rows(&f.store);
    assert_eq!(propose(&f, &policy(&f), source.active_deadline_ms()).unwrap(), None);
    assert_eq!(rows(&f.store), before);
    assert_eq!(session(&f), source);
}
