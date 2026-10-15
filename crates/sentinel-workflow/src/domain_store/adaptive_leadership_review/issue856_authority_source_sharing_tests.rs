use super::*;

type Counts = std::collections::BTreeMap<&'static str, usize>;

fn recovered_dispatch() -> (Fixture, AdaptiveLeadershipReviewCallV1, u64) {
    let (f, request, at) = recovery_fixture();
    let receipt = f
        .store
        .authorize_adaptive_funded_review_recovery(
            &funding_operator(&f),
            &request,
            &f.grant,
            &f.context,
            at,
            recovery_evidence,
        )
        .unwrap()
        .1;
    let call = f
        .store
        .adaptive_leadership_review_call(&f.leader.tenant_id, receipt.grant_template.review_id)
        .unwrap()
        .unwrap();
    assert_eq!(call.grant.schema_version, 6);
    let dispatched = f
        .store
        .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), at + 1)
        .unwrap();
    (f, dispatched, at + 2)
}

fn proofs(scopes: &[Counts], domain: &str) -> usize {
    scopes
        .iter()
        .map(|scope| scope.get(domain).copied().unwrap_or(0))
        .sum()
}

fn assert_full_journal(scope: &Counts, version: u64) {
    let entries = usize::try_from(version).unwrap();
    assert_eq!(scope.get("adaptive-journal"), Some(&1));
    assert_eq!(scope.get("adaptive-journal-entry"), Some(&entries));
    assert_eq!(scope.get("adaptive-journal-step"), Some(&entries));
    assert_eq!(scope.get("historical-project"), Some(&1));
    assert!(scope.values().sum::<usize>() <= 16_384);
}

#[test]
fn budget_source_composition_removes_a_full_replay_without_removing_history_checks() {
    let (f, call, _) = recovered_dispatch();
    let before = rows(&f.store);
    let mut connection = f.store.connection.lock().unwrap();
    let transaction = connection.transaction().unwrap();
    let ((separate, separate_scopes), failures) = validation_scope::with_validation_failures(|| {
        validation_scope::with_completed_validations(|| -> Result<(), WorkflowError> {
            // The previous production composition: each check owns its own arena.
            require_funding_review_membership(
                &transaction,
                &call.grant,
                &call.context_digest()?,
                call.operation_id,
            )?;
            require_budget_source(&transaction, &call.grant, &call.context)
        })
    });
    separate.unwrap();
    assert!(failures.is_empty());
    assert_eq!(separate_scopes.len(), 2);
    assert_eq!(proofs(&separate_scopes, "adaptive-journal"), 2);
    let ((shared, shared_scopes), failures) = validation_scope::with_validation_failures(|| {
        validation_scope::with_completed_validations(|| {
            WorkflowStore::require_adaptive_budget_review_source(&transaction, &call)
        })
    });
    shared.unwrap();
    assert!(failures.is_empty());
    assert_eq!(shared_scopes.len(), 1);
    assert_full_journal(&shared_scopes[0], call.context.source_session.version);
    assert!(
        shared_scopes[0].values().sum::<usize>()
            < separate_scopes
                .iter()
                .flat_map(|scope| scope.values())
                .sum::<usize>()
    );
    transaction.rollback().unwrap();
    assert!(connection.is_autocommit());
    drop(connection);
    assert_eq!(rows(&f.store), before);
}

#[test]
fn schema6_completion_uses_three_distinct_arenas_and_replays_exactly() {
    let (f, call, now) = recovered_dispatch();
    let result = continued(&call, now, 1);
    let (completed, scopes) = validation_scope::with_completed_validations(|| {
        f.store
            .complete_adaptive_leadership_review_call(&f.leader, &result, now)
    });
    let completed = completed.unwrap();
    // Preflight, changed journal, then changed abandonment row. Proofs never cross writes.
    assert_eq!(scopes.len(), 3);
    assert_full_journal(&scopes[0], call.context.source_session.version);
    for scope in &scopes[1..] {
        assert_full_journal(scope, call.context.source_session.version + 1);
    }
    let current = session(&f);
    assert_eq!(current.version, call.context.source_session.version + 1);
    assert_eq!(current.grant, call.context.source_session.grant);
    assert_eq!(current.model_calls, call.context.source_session.model_calls);
    assert_eq!(current.tool_calls, call.context.source_session.tool_calls);
    assert_eq!(completed.grant, call.grant);
    assert_eq!(completed.operation_id, call.operation_id);
    assert_eq!(completed.dispatch, call.dispatch);
    assert_eq!(completed.continuation, result.continuation);
    let before = rows(&f.store);
    let late = call
        .grant
        .work_funding
        .as_ref()
        .unwrap()
        .binding
        .limits
        .expires_at_unix_ms
        + 1;
    let (replayed, scopes) = validation_scope::with_completed_validations(|| {
        f.store
            .complete_adaptive_leadership_review_call(&f.leader, &result, late)
    });
    assert_eq!(replayed.unwrap(), completed);
    assert_eq!(scopes.len(), 1);
    assert_full_journal(&scopes[0], current.version);
    let mut foreign = f.leader.clone();
    foreign.principal_id = "other-leadership-principal".into();
    assert_eq!(
        f.store
            .complete_adaptive_leadership_review_call(&foreign, &result, late)
            .unwrap_err()
            .code,
        unauthorized().code,
    );
    let mut changed = result.clone();
    changed.model_response_digest = "b".repeat(64);
    assert!(f
        .store
        .complete_adaptive_leadership_review_call(&f.leader, &changed, late)
        .is_err());
    assert_eq!(rows(&f.store), before);
    assert_eq!(validation_scope::validations("adaptive-journal"), 0);
}

#[test]
fn schema6_post_abandonment_validation_rejects_new_corruption_and_rolls_back() {
    for target in ["journal", "recovery-event", "membership-event"] {
        let (f, call, now) = recovered_dispatch();
        let result = continued(&call, now, 1);
        let damage = match target {
            "journal" => format!(
                "UPDATE workflow_operations SET request_digest='invalid'
                 WHERE operation_namespace='adaptive-session-v1:{}'
                 AND operation_id='00000000000000000001';",
                call.grant.session_id,
            ),
            "recovery-event" => "DELETE FROM company_events
                WHERE event_type='adaptive_funded_review_recovery_issued';"
                .to_owned(),
            "membership-event" => "DELETE FROM company_events
                WHERE event_type='adaptive_work_funding_review_issued';"
                .to_owned(),
            _ => unreachable!(),
        };
        f.store
            .connection
            .lock()
            .unwrap()
            .execute_batch(&format!(
                "CREATE TRIGGER corrupt_shared_source AFTER INSERT ON company_entities
                 WHEN NEW.entity_kind='{ABANDONED_KIND}' BEGIN {damage} END;",
            ))
            .unwrap();
        let before = rows(&f.store);
        let (failed, scopes) = validation_scope::with_completed_validations(|| {
            f.store
                .complete_adaptive_leadership_review_call(&f.leader, &result, now)
        });
        assert_eq!(
            failed.unwrap_err().code,
            WorkflowErrorCode::CorruptStore,
            "{target}"
        );
        assert_eq!(scopes.len(), 2, "failed post-abandonment arena must not finish");
        assert_full_journal(&scopes[0], call.context.source_session.version);
        assert_full_journal(&scopes[1], call.context.source_session.version + 1);
        assert_eq!(validation_scope::validations("adaptive-journal"), 0);
        assert_eq!(rows(&f.store), before);
        assert_eq!(session(&f), call.context.source_session);
        assert!(f.store.connection.lock().unwrap().is_autocommit());
        f.store
            .connection
            .lock()
            .unwrap()
            .execute_batch("DROP TRIGGER corrupt_shared_source")
            .unwrap();
        let (retried, scopes) = validation_scope::with_completed_validations(|| {
            f.store
                .complete_adaptive_leadership_review_call(&f.leader, &result, now)
        });
        retried.unwrap();
        assert_eq!(scopes.len(), 3);
        assert_full_journal(&scopes[2], call.context.source_session.version + 1);
    }
}

#[test]
fn schema6_completion_replay_and_warm_read_reject_external_old_history_corruption() {
    let (f, call, now) = recovered_dispatch();
    let result = continued(&call, now, 1);
    let completed = f
        .store
        .complete_adaptive_leadership_review_call(&f.leader, &result, now)
        .unwrap();
    for _ in 0..2 {
        assert_eq!(
            f.store
                .adaptive_leadership_review_call(&f.leader.tenant_id, call.grant.review_id)
                .unwrap(),
            Some(completed.clone())
        );
    }
    let external = Connection::open(&f.path).unwrap();
    assert_eq!(
        external
            .execute(
                "UPDATE workflow_operations SET request_digest='invalid'
                 WHERE operation_namespace=?1 AND operation_id='00000000000000000001'",
                [format!("adaptive-session-v1:{}", call.grant.session_id)],
            )
            .unwrap(),
        1
    );
    let damaged = rows(&f.store);
    for _ in 0..2 {
        assert_eq!(
            f.store
                .adaptive_leadership_review_call(&f.leader.tenant_id, call.grant.review_id)
                .unwrap_err()
                .code,
            WorkflowErrorCode::CorruptStore
        );
        assert_eq!(
            f.store
                .complete_adaptive_leadership_review_call(&f.leader, &result, now)
                .unwrap_err()
                .code,
            WorkflowErrorCode::CorruptStore
        );
        assert_eq!(rows(&f.store), damaged);
        assert_eq!(validation_scope::validations("adaptive-journal"), 0);
        assert!(f.store.connection.lock().unwrap().is_autocommit());
    }
}

#[test]
fn schema6_write_failure_after_shared_source_validation_preserves_retry() {
    let (f, call, now) = recovered_dispatch();
    let result = continued(&call, now, 1);
    f.store
        .connection
        .lock()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER reject_after_shared_source BEFORE INSERT ON company_events
             WHEN NEW.event_type='adaptive_leadership_continuation_authorized'
             BEGIN SELECT RAISE(ABORT, 'test completion failure'); END;",
        )
        .unwrap();
    let before = rows(&f.store);
    let (failed, scopes) = validation_scope::with_completed_validations(|| {
        f.store
            .complete_adaptive_leadership_review_call(&f.leader, &result, now)
    });
    assert!(failed.is_err());
    assert_eq!(scopes.len(), 3);
    assert_full_journal(&scopes[2], call.context.source_session.version + 1);
    assert_eq!(rows(&f.store), before);
    assert_eq!(session(&f), call.context.source_session);
    assert_eq!(validation_scope::validations("adaptive-journal"), 0);
    assert!(f.store.connection.lock().unwrap().is_autocommit());
    f.store
        .connection
        .lock()
        .unwrap()
        .execute_batch("DROP TRIGGER reject_after_shared_source")
        .unwrap();
    f.store
        .complete_adaptive_leadership_review_call(&f.leader, &result, now)
        .unwrap();
    assert_eq!(session(&f).version, call.context.source_session.version + 1);
}

#[test]
fn budget_source_shared_arena_keeps_existing_byte_and_node_limits() {
    for pressure in ["bytes", "nodes"] {
        let (f, call, now) = recovered_dispatch();
        let before = rows(&f.store);
        let mut connection = f.store.connection.lock().unwrap();
        let transaction = connection.transaction().unwrap();
        let (failed, failures) = validation_scope::with_validation_failures(|| {
            validation_scope::with_scope(&transaction, || {
                if pressure == "bytes" {
                    validation_scope::charge_bytes(&transaction, 64 * 1024 * 1024)?;
                } else {
                    for index in 0_u32..16_384 {
                        validation_scope::memoize(
                            &transaction,
                            "issue856-source-budget-pressure",
                            &index,
                            || Ok(()),
                        )?;
                    }
                }
                WorkflowStore::require_adaptive_budget_review_source(&transaction, &call)
            })
        });
        assert_eq!(failed.unwrap_err().code, WorkflowErrorCode::CorruptStore);
        let failure = failures.first().expect("unchanged arena limit must reject");
        if pressure == "bytes" {
            assert_eq!(failure.reason, validation_scope::ValidationFailureReason::BytesLimit);
            assert_eq!(failure.bytes, 64 * 1024 * 1024);
        } else {
            assert_eq!(failure.reason, validation_scope::ValidationFailureReason::NodeLimit);
            assert_eq!(failure.nodes, 16_384);
        }
        assert_eq!(validation_scope::validations("adaptive-journal"), 0);
        assert_eq!(validation_scope::validations("issue856-source-budget-pressure"), 0);
        assert!(!transaction.is_autocommit());
        transaction.rollback().unwrap();
        assert!(connection.is_autocommit());
        drop(connection);
        assert_eq!(rows(&f.store), before);
        f.store
            .complete_adaptive_leadership_review_call(&f.leader, &continued(&call, now, 1), now)
            .unwrap();
        assert_eq!(session(&f).version, call.context.source_session.version + 1);
    }
}
