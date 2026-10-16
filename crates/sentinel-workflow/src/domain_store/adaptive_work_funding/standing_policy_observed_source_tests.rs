// Child of the existing store-backed standing-policy fixtures.
use super::*;

fn observed_project(f: &Fixture) -> ProjectV1 {
    f.store.company_project(&f.leader.tenant_id, &f.grant.project_id).unwrap().unwrap()
}

#[test]
fn observed_issuance_checks_full_values_before_any_clock_or_callback() {
    let (f, _, now) = refused_fixture();
    let policy = policy(&f);
    let project = observed_project(&f);
    let source = session(&f);
    let before = rows(&f.store);
    for change in ["project-version", "project-content", "session-version", "session-content", "history", "tenant"] {
        let mut observed_project = project.clone();
        let mut observed_session = source.clone();
        match change {
            "project-version" => observed_project.version += 1,
            "project-content" => observed_project.cost_ceiling_micros += 1,
            "session-version" => observed_session.version += 1,
            "session-content" => observed_session.updated_at_ms += 1,
            "history" => { observed_session.continuation.as_mut().unwrap().authorizations.pop().unwrap(); }
            "tenant" => observed_project.tenant_id = TenantId::parse("other-tenant").unwrap(),
            _ => unreachable!(),
        }
        let error = f.store.propose_standing_work_funding_observed_with_clock(
            &funding_operator(&f), &policy, (&observed_project, &observed_session),
            || panic!("stale observation sampled a clock"),
            || panic!("stale observation ran external checks"),
        ).unwrap_err();
        assert_eq!(error.code, WorkflowErrorCode::AuthorityConflict, "{change}");
        assert_eq!(rows(&f.store), before, "{change}");
        assert!(f.store.connection.lock().unwrap().is_autocommit());
    }
    let checks = std::cell::Cell::new(0);
    let (replayed, receipt) = f.store.propose_standing_work_funding_observed_with_clock(
        &funding_operator(&f), &policy, (&project, &source), || now,
        || { checks.set(checks.get() + 1); Ok(()) },
    ).unwrap().unwrap();
    assert!(!replayed);
    assert_eq!(checks.get(), 2);
    assert_eq!(receipt.request.source.resume_source.expected_session_version, source.version);
    assert_eq!(session(&f), source);
}

#[test]
fn observed_source_detects_actual_intervening_journal_progress() {
    let (mut f, _, now) = refused_fixture();
    let project = observed_project(&f);
    let source = session(&f);
    let policy = policy(&f);
    let receipt = propose(&f, &policy, now).unwrap().unwrap().1;
    refresh(&mut f, &receipt, now + 1);
    let review = dispatch(&f, now + 1);
    f.store.complete_adaptive_leadership_review_call(
        &f.leader, &continued(&review, now + 3, 5), now + 3,
    ).unwrap();
    let newer = observe_budget_inspection(&f, session(&f), now + 4);
    assert!(newer.version > source.version);
    assert_eq!(newer.grant.authority, source.grant.authority);
    let before = rows(&f.store);
    assert_eq!(f.store.propose_standing_work_funding_observed_with_clock(
        &funding_operator(&f), &policy, (&project, &source),
        || panic!("changed head sampled a clock"),
        || panic!("changed head ran external checks"),
    ).unwrap_err().code, WorkflowErrorCode::AuthorityConflict);
    assert_eq!(rows(&f.store), before);
    assert_eq!(session(&f), newer);
}

#[test]
fn observed_source_detects_actual_intervening_project_commit() {
    let (f, _, now) = refused_fixture();
    let project = observed_project(&f);
    let source = session(&f);
    f.store.apply_company_command(
        &f.leader, Uuid::new_v4(),
        &CompanyWorkflowCommandV1::RecordDecision {
            project_id: project.project_id.clone(), expected_version: project.version,
            work_item_id: None, choice_ref: "unrelated-approved-choice".into(),
            rationale_ref: "Synthetic concurrent project change".into(),
        }, now,
    ).unwrap();
    assert!(observed_project(&f).version > project.version);
    assert_eq!(session(&f), source);
    let before = rows(&f.store);
    assert_eq!(f.store.propose_standing_work_funding_observed_with_clock(
        &funding_operator(&f), &policy(&f), (&project, &source),
        || panic!("changed project sampled a clock"),
        || panic!("changed project ran external checks"),
    ).unwrap_err().code, WorkflowErrorCode::AuthorityConflict);
    assert_eq!(rows(&f.store), before);
}

#[test]
fn observed_replay_ignores_stale_content_and_preserves_old_api_receipt_after_reopen() {
    let (mut f, _, now) = refused_fixture();
    let policy = policy(&f);
    let mut project = observed_project(&f);
    let mut source = session(&f);
    let receipt = propose(&f, &policy, now).unwrap().unwrap().1;
    let bytes = encode(&receipt).unwrap();
    project.version += 1;
    project.cost_ceiling_micros += 1;
    source.version += 1;
    source.updated_at_ms += 1;
    source.continuation.as_mut().unwrap().authorizations.clear();
    f.store = WorkflowStore::open(&f.path).unwrap();
    let before = rows(&f.store);
    for _ in 0..2 {
        let (replayed, replay) = f.store.propose_standing_work_funding_observed_with_clock(
            &funding_operator(&f), &policy, (&project, &source),
            || panic!("replay sampled a clock"), || panic!("replay ran external checks"),
        ).unwrap().unwrap();
        assert!(replayed);
        assert_eq!(encode(&replay).unwrap(), bytes);
        assert_eq!(rows(&f.store), before);
    }
}

#[test]
fn observed_replay_never_hides_broken_provenance_or_issuer_binding() {
    for corrupt_event in [false, true] {
        let (f, _, now) = refused_fixture();
        let policy = policy(&f);
        let project = observed_project(&f);
        let source = session(&f);
        propose(&f, &policy, now).unwrap().unwrap();
        let mut issuer = funding_operator(&f);
        if corrupt_event {
            Connection::open(&f.path).unwrap().execute(
                "DELETE FROM company_events WHERE event_type='adaptive_work_funding_issued'", [],
            ).unwrap();
        } else {
            issuer.authority_digest = "b".repeat(64);
        }
        let before = rows(&f.store);
        assert!(f.store.propose_standing_work_funding_observed_with_clock(
            &issuer, &policy, (&project, &source),
            || panic!("invalid replay sampled a clock"), || panic!("invalid replay ran checks"),
        ).is_err());
        assert_eq!(rows(&f.store), before);
    }
}

#[test]
fn observed_external_failure_rolls_back_at_both_original_phases() {
    let (f, _, now) = refused_fixture();
    let policy = policy(&f);
    let project = observed_project(&f);
    let source = session(&f);
    for fail_at in [1, 2] {
        let before = rows(&f.store);
        let mut checks = 0;
        let ticks = std::cell::Cell::new(0);
        let error = f.store.propose_standing_work_funding_observed_with_clock(
            &funding_operator(&f), &policy, (&project, &source),
            || { ticks.set(ticks.get() + 1); now },
            || {
                checks += 1;
                if checks == fail_at {
                    Err(WorkflowError::new(WorkflowErrorCode::AuthorityConflict, false, "revoked"))
                } else { Ok(()) }
            },
        ).unwrap_err();
        assert_eq!(error.code, WorkflowErrorCode::AuthorityConflict);
        assert_eq!(checks, fail_at);
        assert_eq!(ticks.get(), fail_at);
        assert_eq!(rows(&f.store), before);
        assert!(f.store.connection.lock().unwrap().is_autocommit());
    }
}

#[test]
fn observed_same_head_defer_replays_only_original_slot_and_other_generation_stays_blocked() {
    let (mut f, _, now) = refused_fixture();
    let mut policy = policy(&f);
    let project = observed_project(&f);
    let source = session(&f);
    let receipt = f.store.propose_standing_work_funding_observed_with_clock(
        &funding_operator(&f), &policy, (&project, &source), || now, || Ok(()),
    ).unwrap().unwrap().1;
    refresh(&mut f, &receipt, now + 1);
    let review = dispatch(&f, now + 1);
    let deferred = f.store.complete_adaptive_leadership_review_call(
        &f.leader, &defer(&review), now + 3,
    ).unwrap();
    let before = rows(&f.store);
    let replay = f.store.propose_standing_work_funding_observed_with_clock(
        &funding_operator(&f), &policy, (&project, &source),
        || panic!("same-head replay sampled a clock"), || panic!("replay ran checks"),
    ).unwrap();
    assert_eq!(replay, Some((true, receipt)));
    policy.generation += 1;
    assert_eq!(f.store.propose_standing_work_funding_observed_with_clock(
        &funding_operator(&f), &policy, (&project, &source),
        || panic!("pending proposal sampled a clock"), || panic!("pending proposal ran checks"),
    ).unwrap(), None);
    assert_eq!(rows(&f.store), before);
    assert_eq!(session(&f), source);
    assert_eq!(f.store.adaptive_leadership_review_call(
        &f.leader.tenant_id, deferred.grant.review_id,
    ).unwrap(), Some(deferred));
}
