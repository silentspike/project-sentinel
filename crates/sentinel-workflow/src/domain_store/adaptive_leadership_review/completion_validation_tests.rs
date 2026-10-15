use super::*;

fn assert_completion_proof_phases(
    scopes: &[std::collections::BTreeMap<&'static str, usize>],
    source: &AdaptiveSessionV1,
    funded: bool,
) {
    let preflight = scopes.first().expect("completed production preflight");
    let entries = usize::try_from(source.version).unwrap();
    assert_eq!(preflight.get("adaptive-journal"), Some(&1));
    assert_eq!(preflight.get("adaptive-journal-entry"), Some(&entries));
    assert_eq!(preflight.get("adaptive-journal-step"), Some(&entries));
    assert_eq!(preflight.get("historical-project"), Some(&1));
    if funded {
        // Eligibility and the later adoption preflight share the initial review's arena.
        assert_eq!(
            preflight.get("expired-funding-replacement-inventory"),
            Some(&1),
        );
    }
    let post_write = scopes.get(1).expect("fresh post-write allowance preflight");
    assert_eq!(post_write.get("adaptive-journal"), Some(&1));
    assert_eq!(post_write.get("adaptive-journal-entry"), Some(&(entries + 1)));
    assert_eq!(post_write.get("adaptive-journal-step"), Some(&(entries + 1)));
    assert_eq!(post_write.get("historical-project"), Some(&1));
    assert!(post_write.get("budget-source").is_some_and(|count| *count > 0));
    let post_write_nodes: usize = post_write.values().sum();
    assert!(post_write_nodes <= 16_384);
    assert_eq!(validation_scope::validations("adaptive-journal"), 0);
}

#[test]
fn completion_preflight_reuses_proofs_and_rebuilds_post_write_journal() {
    for funded in [false, true] {
        let (f, now) = if funded {
            let (f, _, now) = funded_fixture();
            (f, now)
        } else {
            (budget_fixture(4), CONTINUATION_AT + 7)
        };
        let call = dispatch(&f, now);
        let result = if funded {
            continued(&call, now + 2, 5)
        } else {
            budget_result(&call, 1, now + 2)
        };
        let (completed, scopes) = validation_scope::with_completed_validations(|| {
            f.store
                .complete_adaptive_leadership_review_call(&f.leader, &result, now + 2)
        });
        let completed = completed.unwrap();
        assert_completion_proof_phases(&scopes, &call.context.source_session, funded);
        let current = session(&f);
        assert_eq!(current.version, call.context.source_session.version + 1);
        assert_eq!(current.grant, call.context.source_session.grant);
        assert_eq!(current.model_calls, call.context.source_session.model_calls);
        assert_eq!(current.tool_calls, call.context.source_session.tool_calls);
        let before = rows(&f.store);
        let (replay, replay_scopes) = validation_scope::with_completed_validations(|| {
            f.store.complete_adaptive_leadership_review_call(
                &f.leader,
                &result,
                call.grant.expires_at_unix_ms + WINDOW_MS + 1,
            )
        });
        assert_eq!(replay.unwrap(), completed);
        assert_eq!(replay_scopes.len(), 1);
        assert_eq!(validation_scope::validations("adaptive-journal"), 0);
        assert_eq!(rows(&f.store), before);
        assert!(f.store.connection.lock().unwrap().is_autocommit());
    }
}

#[test]
fn completion_post_write_corruption_rolls_back_and_retry_rebuilds_proofs() {
    for target in ["journal", "membership-event"] {
        let (f, _, now) = funded_fixture();
        let call = dispatch(&f, now);
        let result = continued(&call, now + 2, 5);
        let namespace = format!("adaptive-session-v1:{}", call.grant.session_id);
        let next = format!("{:020}", call.context.source_session.version + 1);
        let damage = match target {
            "journal" => "UPDATE workflow_operations SET request_digest='invalid'
                WHERE operation_namespace=NEW.operation_namespace
                AND operation_id='00000000000000000001';",
            "membership-event" => "DELETE FROM company_events
                WHERE event_type='adaptive_work_funding_review_issued';",
            _ => unreachable!(),
        };
        // Damage already-proven evidence only after the continuation journal is appended.
        f.store
            .connection
            .lock()
            .unwrap()
            .execute_batch(&format!(
                "CREATE TRIGGER corrupt_completion_after_journal
                 AFTER INSERT ON workflow_operations
                 WHEN NEW.operation_namespace='{namespace}' AND NEW.operation_id='{next}'
                 BEGIN {damage} END;",
            ))
            .unwrap();
        let before = rows(&f.store);
        let (failed, scopes) = validation_scope::with_completed_validations(|| {
            f.store
                .complete_adaptive_leadership_review_call(&f.leader, &result, now + 2)
        });
        assert_eq!(failed.unwrap_err().code, WorkflowErrorCode::CorruptStore);
        assert!(!scopes.is_empty(), "preflight must finish before rejecting {target}");
        assert_eq!(scopes[0].get("adaptive-journal"), Some(&1));
        assert!(scopes[1..]
            .iter()
            .all(|scope| !scope.contains_key("adaptive-journal")));
        assert_eq!(validation_scope::validations("adaptive-journal"), 0);
        assert_eq!(rows(&f.store), before);
        assert_eq!(session(&f), call.context.source_session);
        assert!(f.store.connection.lock().unwrap().is_autocommit());
        f.store
            .connection
            .lock()
            .unwrap()
            .execute_batch("DROP TRIGGER corrupt_completion_after_journal")
            .unwrap();
        let (retried, scopes) = validation_scope::with_completed_validations(|| {
            f.store
                .complete_adaptive_leadership_review_call(&f.leader, &result, now + 2)
        });
        retried.unwrap();
        assert_completion_proof_phases(&scopes, &call.context.source_session, true);
        let adopted = session(&f);
        assert_eq!(adopted.version, call.context.source_session.version + 1);
        assert_eq!(adopted.model_calls, call.context.source_session.model_calls);
        assert_eq!(adopted.tool_calls, call.context.source_session.tool_calls);
    }
}

#[test]
fn completion_failed_preflight_cannot_hide_later_corruption() {
    for target in ["review", "project", "planning", "journal", "receipt", "membership"] {
        let (f, _, now) = funded_fixture();
        let call = dispatch(&f, now);
        let result = continued(&call, now + 2, 5);
        let mut invalid = result.clone();
        invalid.request_digest = "b".repeat(64);
        let before = rows(&f.store);
        let (failed, scopes) = validation_scope::with_completed_validations(|| {
            f.store
                .complete_adaptive_leadership_review_call(&f.leader, &invalid, now + 2)
        });
        assert_eq!(failed.unwrap_err().code, unauthorized().code);
        assert!(scopes.is_empty());
        assert_eq!(validation_scope::validations("adaptive-journal"), 0);
        assert_eq!(rows(&f.store), before);
        let connection = f.store.connection.lock().unwrap();
        let changed = if target == "journal" {
            connection
                .execute(
                    "UPDATE workflow_operations SET request_digest='invalid'
                     WHERE operation_namespace=?1 AND operation_id='00000000000000000001'",
                    [format!("adaptive-session-v1:{}", call.grant.session_id)],
                )
                .unwrap()
        } else {
            let kind = match target {
                "review" => KIND,
                "project" => "project",
                "planning" => "project_planning_call",
                "receipt" => "adaptive_work_funding",
                "membership" => "adaptive_work_funding_review",
                _ => unreachable!(),
            };
            connection
                .execute(
                    "UPDATE company_entities SET payload_digest='invalid' WHERE entity_kind=?1",
                    [kind],
                )
                .unwrap()
        };
        assert!(changed > 0, "missing corruption target {target}");
        drop(connection);
        let damaged = rows(&f.store);
        for _ in 0..2 {
            // A failed operation must not lend proofs to the next valid proposal.
            let (failed, scopes) = validation_scope::with_completed_validations(|| {
                f.store
                    .complete_adaptive_leadership_review_call(&f.leader, &result, now + 2)
            });
            assert_eq!(failed.unwrap_err().code, WorkflowErrorCode::CorruptStore);
            assert!(scopes.is_empty());
            assert_eq!(validation_scope::validations("adaptive-journal"), 0);
            assert_eq!(rows(&f.store), damaged);
            assert!(f.store.connection.lock().unwrap().is_autocommit());
        }
    }
}

#[test]
fn completion_substantial_legitimate_history_fits_preflight_budgets_after_reopen() {
    let (mut f, receipt, _) = capped_renewal_fixture(20);
    let mut source = session(&f);
    let initial_version = source.version;
    // Append real model/tool transitions within the already-authorized window.
    for _ in 0..15 {
        let at = source.updated_at_ms + 1;
        source = observe_budget_inspection(&f, source, at);
    }
    assert_eq!(source.version, initial_version + 60);
    assert!(source.version >= 64);
    assert_eq!((source.model_calls, source.tool_calls), (18, 17));
    assert_eq!(source.continuation.as_ref().unwrap().authorizations.len(), 2);
    let now = source.active_deadline_ms();
    budget_context(&mut f, now, "substantial-completion-history");
    bind(
        &mut f,
        &receipt,
        receipt.request.source.resume_source.base_review_count + 2,
    );
    let call = dispatch(&f, now);
    let result = continued(&call, now + 2, 5);
    let (journal_rows, journal_bytes): (i64, i64) = f
        .store
        .connection
        .lock()
        .unwrap()
        .query_row(
            "SELECT count(*),sum(length(response)) FROM workflow_operations
             WHERE operation_namespace=?1",
            [format!("adaptive-session-v1:{}", call.grant.session_id)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(u64::try_from(journal_rows).unwrap(), source.version);
    assert!(journal_bytes > 64 * 1024);
    f.store = WorkflowStore::open(&f.path).unwrap();
    let ((completed, scopes), failures) = validation_scope::with_validation_failures(|| {
        validation_scope::with_completed_validations(|| {
            f.store
                .complete_adaptive_leadership_review_call(&f.leader, &result, now + 2)
        })
    });
    let completed = completed.unwrap_or_else(|error| {
        panic!("substantial completion failed: {error:?}; validation failures: {failures:?}")
    });
    assert!(failures.is_empty(), "unchanged budgets rejected history: {failures:?}");
    assert_completion_proof_phases(&scopes, &source, true);
    let preflight_nodes: usize = scopes[0].values().sum();
    assert!(preflight_nodes >= 2 * usize::try_from(source.version).unwrap());
    assert!(preflight_nodes <= 16_384);
    let adopted = session(&f);
    assert_eq!(adopted.version, source.version + 1);
    assert_eq!(adopted.grant, source.grant);
    assert_eq!((adopted.model_calls, adopted.tool_calls), (18, 17));
    assert_eq!(adopted.continuation.as_ref().unwrap().authorizations.len(), 3);
    assert!(adopted.continuation.as_ref().unwrap().authorizations.starts_with(
        &source.continuation.as_ref().unwrap().authorizations,
    ));
    let before = rows(&f.store);
    assert_eq!(
        f.store
            .complete_adaptive_leadership_review_call(
                &f.leader,
                &result,
                receipt.request.limits.expires_at_unix_ms + 1,
            )
            .unwrap(),
        completed,
    );
    assert_eq!(rows(&f.store), before);
}

#[test]
fn completion_preflight_budget_errors_clean_up_before_production_retry() {
    for pressure in ["bytes", "nodes"] {
        let (f, _, now) = funded_fixture();
        let call = dispatch(&f, now);
        let result = continued(&call, now + 2, 5);
        let before = rows(&f.store);
        let mut connection = f.store.connection.lock().unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        // Exhaust the existing arena before the production preflight's first validator.
        // This allocates no large payload and does not inject a production callback.
        let (failed, failures) = validation_scope::with_validation_failures(|| {
            validation_scope::with_scope(&transaction, || {
                if pressure == "bytes" {
                    validation_scope::charge_bytes(&transaction, 64 * 1024 * 1024)?;
                } else {
                    for index in 0_u32..16_384 {
                        validation_scope::memoize(
                            &transaction,
                            "completion-preflight-budget-pressure",
                            &index,
                            || Ok(()),
                        )?;
                    }
                }
                get_entity::<AdaptiveLeadershipReviewCallV1>(
                    &transaction,
                    &f.leader.tenant_id,
                    KIND,
                    &call.review_key,
                )
            })
        });
        assert_eq!(failed.unwrap_err().code, WorkflowErrorCode::CorruptStore);
        let first = failures.first().expect("recorded arena exhaustion");
        if pressure == "bytes" {
            assert_eq!(first.reason, validation_scope::ValidationFailureReason::BytesLimit);
            assert_eq!(first.bytes, 64 * 1024 * 1024);
        } else {
            assert_eq!(first.reason, validation_scope::ValidationFailureReason::NodeLimit);
            assert_eq!(first.nodes, 16_384);
        }
        assert_eq!(validation_scope::validations("adaptive-journal"), 0);
        assert_eq!(validation_scope::validations("completion-preflight-budget-pressure"), 0);
        assert!(!transaction.is_autocommit());
        transaction.rollback().unwrap();
        assert!(connection.is_autocommit());
        drop(connection);
        assert_eq!(rows(&f.store), before);
        let (completed, scopes) = validation_scope::with_completed_validations(|| {
            f.store
                .complete_adaptive_leadership_review_call(&f.leader, &result, now + 2)
        });
        completed.unwrap();
        assert_completion_proof_phases(&scopes, &call.context.source_session, true);
        assert_eq!(session(&f).version, call.context.source_session.version + 1);
        assert!(f.store.connection.lock().unwrap().is_autocommit());
    }
}

#[test]
fn completion_post_write_scope_ends_before_abandonment_write_and_rejects_corruption() {
    for target in ["journal", "membership-event"] {
        let (f, _, now) = funded_fixture();
        let call = dispatch(&f, now);
        let result = continued(&call, now + 2, 5);
        let damage = match target {
            "journal" => format!(
                "UPDATE workflow_operations SET request_digest='invalid'
                 WHERE operation_namespace='adaptive-session-v1:{}'
                 AND operation_id='00000000000000000001';",
                call.grant.session_id,
            ),
            "membership-event" => "DELETE FROM company_events
                WHERE event_type='adaptive_work_funding_review_issued';"
                .to_owned(),
            _ => unreachable!(),
        };
        // These proofs were valid in the fresh post-write scope, before the next write.
        f.store
            .connection
            .lock()
            .unwrap()
            .execute_batch(&format!(
                "CREATE TRIGGER corrupt_completion_after_abandonment
                 AFTER INSERT ON company_entities WHEN NEW.entity_kind='{ABANDONED_KIND}'
                 BEGIN {damage} END;",
            ))
            .unwrap();
        let before = rows(&f.store);
        let (failed, scopes) = validation_scope::with_completed_validations(|| {
            f.store
                .complete_adaptive_leadership_review_call(&f.leader, &result, now + 2)
        });
        assert_eq!(failed.unwrap_err().code, WorkflowErrorCode::CorruptStore);
        assert_completion_proof_phases(&scopes, &call.context.source_session, true);
        assert!(scopes[2..]
            .iter()
            .all(|scope| !scope.contains_key("adaptive-journal")));
        assert_eq!(rows(&f.store), before);
        assert_eq!(session(&f), call.context.source_session);
        assert!(f.store.connection.lock().unwrap().is_autocommit());
        f.store
            .connection
            .lock()
            .unwrap()
            .execute_batch("DROP TRIGGER corrupt_completion_after_abandonment")
            .unwrap();
        let (retried, scopes) = validation_scope::with_completed_validations(|| {
            f.store
                .complete_adaptive_leadership_review_call(&f.leader, &result, now + 2)
        });
        retried.unwrap();
        assert_completion_proof_phases(&scopes, &call.context.source_session, true);
        assert_eq!(session(&f).version, call.context.source_session.version + 1);
    }
}

#[test]
fn completion_post_write_scope_budget_errors_preserve_outer_transaction_and_retry() {
    for pressure in ["bytes", "nodes"] {
        let (f, _, now) = funded_fixture();
        let call = dispatch(&f, now);
        let result = continued(&call, now + 2, 5);
        let authorization = result.continuation.as_ref().unwrap();
        let allowance = call
            .continuation_allowance(
                authorization.issued_at_ms,
                authorization.deadline_ms,
                authorization.additional_model_calls,
            )
            .unwrap();
        let before = rows(&f.store);
        let mut connection = f.store.connection.lock().unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let (_, continued) = crate::store::adaptive::continue_adaptive_session_in_transaction(
            &transaction,
            authorization,
            &call,
            &allowance,
            &call.grant.assignee_authority,
            now + 2,
        )
        .unwrap();
        // Apply bounded pressure only to the new post-write arena, not the earlier proofs.
        let (failed, failures) = validation_scope::with_validation_failures(|| {
            validation_scope::with_scope(&transaction, || {
                if pressure == "bytes" {
                    validation_scope::charge_bytes(&transaction, 64 * 1024 * 1024)?;
                } else {
                    for index in 0_u32..16_384 {
                        validation_scope::memoize(
                            &transaction,
                            "completion-post-write-budget-pressure",
                            &index,
                            || Ok(()),
                        )?;
                    }
                }
                commit_continuation_allowance(
                    &transaction,
                    &call,
                    authorization,
                    &result,
                    &continued,
                    now + 2,
                )
            })
        });
        assert_eq!(failed.unwrap_err().code, WorkflowErrorCode::CorruptStore);
        let first = failures.first().expect("recorded post-write arena exhaustion");
        if pressure == "bytes" {
            assert_eq!(first.reason, validation_scope::ValidationFailureReason::BytesLimit);
            assert_eq!(first.bytes, 64 * 1024 * 1024);
        } else {
            assert_eq!(first.reason, validation_scope::ValidationFailureReason::NodeLimit);
            assert_eq!(first.nodes, 16_384);
        }
        assert_eq!(validation_scope::validations("adaptive-journal"), 0);
        assert_eq!(validation_scope::validations("completion-post-write-budget-pressure"), 0);
        assert!(!transaction.is_autocommit());
        let head_version: i64 = transaction
            .query_row(
                "SELECT version FROM workflow_adaptive_heads WHERE session_id=?1",
                [call.grant.session_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(u64::try_from(head_version).unwrap(), continued.version);
        transaction.rollback().unwrap();
        assert!(connection.is_autocommit());
        drop(connection);
        assert_eq!(rows(&f.store), before);
        let (completed, scopes) = validation_scope::with_completed_validations(|| {
            f.store
                .complete_adaptive_leadership_review_call(&f.leader, &result, now + 2)
        });
        completed.unwrap();
        assert_completion_proof_phases(&scopes, &call.context.source_session, true);
        assert_eq!(session(&f).version, continued.version);
    }
}
