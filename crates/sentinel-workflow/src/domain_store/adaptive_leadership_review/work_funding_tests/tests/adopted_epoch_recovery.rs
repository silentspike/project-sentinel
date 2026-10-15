use super::*;

fn replacement_adopted_recovery_fixture() -> (
    Fixture,
    AdaptiveLeadershipReviewCallV1,
    AdaptiveLeadershipReviewCallV1,
    crate::AdaptiveFundedReviewRecoveryRequestV1,
    u64,
) {
    let (mut f, old_funding, issued) = funded_fixture();
    let old = issue(&f, issued).unwrap();
    f.store.expire_adaptive_leadership_review_call(
        &f.leader, old.grant.review_id, old.version, old.grant.expires_at_unix_ms,
    ).unwrap();
    let now = old_funding.request.limits.expires_at_unix_ms + 1;
    let operator = funding_operator(&f);
    let draft = f.store.adaptive_work_funding_expired_replacement_draft(
        &operator, &f.grant.project_id, f.grant.session_id,
        &crate::AdaptiveWorkFundingExpiredReplacementDraftV1 {
            old_funding_receipt_digest: old_funding.receipt_digest().unwrap(),
            operation_id: Uuid::new_v4(),
            reason_ref: "replacement-before-genuine-adopted-recovery".into(),
            limits: AdaptiveWorkFundingLimitsV1 {
                additional_model_calls: 8,
                additional_tool_calls: 8,
                additional_reviews: 4,
                additional_windows: 3,
                max_window_ms: WINDOW_MS,
                max_call_duration_ms: f.context.source_session.grant.max_call_duration_ms,
                dispatch_margin_ms: crate::ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
                expires_at_unix_ms: now + 3_600_000,
            },
            release: crate::AdaptiveRecoveryReleaseV1 {
                schema_version: 1,
                source_git_sha: "a".repeat(40),
                release_manifest_digest: "b".repeat(64),
                gateway_binary_digest: "c".repeat(64),
            },
            proof_digest: "d".repeat(64),
        }, now,
    ).unwrap();
    let (_, replacement) = f.store.authorize_adaptive_work_funding_expired_replacement(
        &operator, &draft, now, |source, request| {
            Ok(crate::AdaptiveWorkFundingExpiredReplacementEvidenceV1 {
                source_digest: source.source.canonical_digest()?,
                proof_digest: request.proof_digest.clone(),
                verified_at_unix_ms: now,
                per_review: source.review_members.iter().map(|member| {
                    let call = source.calls.iter().find(|call|
                        call.grant.review_id == member.review_id).unwrap();
                    crate::AdaptiveWorkFundingExpiredReplacementReviewEvidenceV1 {
                        member: member.clone(),
                        proof_digest: "e".repeat(64),
                        release: request.release.clone(),
                        no_io: crate::AdaptiveFundedReviewNoIoV1::UndispatchedAtDurableProviderGate {
                            request_id: call.request_id(),
                        },
                    }
                }).collect(),
            })
        },
    ).unwrap();
    let funding = f.store.adaptive_work_funding(
        &operator.tenant_id, f.grant.session_id, draft.funding_request.operation_id,
    ).unwrap().unwrap();
    assert!(funding.request.source.expired_reviewed_replacement.is_some());
    assert_eq!(funding.funding_id, replacement.new_funding_id);
    let base = funding.request.source.resume_source.base_review_count;
    budget_context(&mut f, now, "replacement-epoch-first-real-continue");
    bind(&mut f, &funding, base + 1);
    let first = dispatch(&f, now);
    let completed = f.store.complete_adaptive_leadership_review_call(
        &f.leader, &continued(&first, now + 2, 5), now + 2,
    ).unwrap();
    let adopted = session(&f);
    assert_eq!(adopted.active_work_funding().unwrap().receipt, funding);
    assert!(adopted.continuation.as_ref().unwrap().authorizations.iter().any(|authorization|
        completed.continuation.as_ref() == Some(authorization)));

    let progressed = observe_budget_inspection(&f, adopted.clone(), now + 3);
    assert!(progressed.version > adopted.version);
    assert!(progressed.model_calls > adopted.model_calls);
    assert!(progressed.tool_calls > adopted.tool_calls);
    let review_at = progressed.active_deadline_ms();
    budget_context(&mut f, review_at, "replacement-epoch-later-ordinary-review");
    bind(&mut f, &funding, base + 2);
    let ordinary = issue(&f, review_at).unwrap();
    assert_eq!(ordinary.grant.schema_version, 5);
    let at = ordinary.grant.expires_at_unix_ms;
    let retired = f.store.expire_adaptive_leadership_review_call(
        &f.leader, ordinary.grant.review_id, ordinary.version, at,
    ).unwrap();
    let source = f.store.adaptive_funded_review_recovery_source(
        &operator, &f.grant.project_id, f.grant.session_id, retired.grant.review_id, at,
    ).unwrap();
    assert_eq!(source.expected_session_version, progressed.version);
    assert_eq!(source.funding_receipt_digest, funding.receipt_digest().unwrap());
    assert_eq!(source.ordinal, base + 3);
    budget_context(&mut f, at, "replacement-adopted-epoch-selected-schema6");
    bind(&mut f, &funding, source.ordinal);
    f.grant.expires_at_unix_ms = at + WINDOW_MS;
    let request = crate::AdaptiveFundedReviewRecoveryRequestV1 {
        schema_version: 1,
        operation_id: Uuid::new_v4(),
        source_digest: source.canonical_digest().unwrap(),
        source: Box::new(source),
        release: draft.release.clone(),
        proof_digest: "f".repeat(64),
        reason_ref: "replacement-adoption-then-selected-review-recovery".into(),
        expires_at_unix_ms: f.grant.expires_at_unix_ms,
    };
    (f, completed, retired, request, at)
}

#[test]
fn replacement_adopted_epoch_schema6_publication_discriminates_validation_failure() {
    let (f, completed, retired, request, at) = replacement_adopted_recovery_fixture();
    let operator = funding_operator(&f);
    let progressed = session(&f);
    let funding = f.grant.work_funding.as_ref().unwrap().receipt.clone();
    let before = rows(&f.store);
    let callback = std::cell::Cell::new(false);
    let (result, failures) = validation_scope::with_validation_failures(|| {
        f.store.authorize_adaptive_funded_review_recovery(
            &operator, &request, &f.grant, &f.context, at, |fresh, request| {
                assert!(fresh.review_has_prior_adoption(&completed));
                assert!(!fresh.review_has_prior_adoption(&retired));
                let evidence = recovery_evidence(fresh, request)?;
                callback.set(true);
                Ok(evidence)
            },
        )
    });
    assert!(callback.get(), "failed before verifier callback: {failures:#?}");
    let (replayed, receipt) = match result {
        Ok(value) => value,
        Err(error) => {
            assert_eq!(rows(&f.store), before, "publication failure must roll back every row");
            assert!(f.store.connection.lock().unwrap().is_autocommit());
            panic!("replacement-adopted schema6 publication failed after callback: code={:?}; first_failure={:#?}; all_failures={:#?}",
                error.code, failures.first(), failures);
        }
    };
    assert!(!replayed);
    assert!(failures.is_empty(), "successful publication retained validation failures: {failures:#?}");
    assert_eq!(session(&f), progressed);
    assert_eq!(f.store.adaptive_leadership_review_call(
        &operator.tenant_id, completed.grant.review_id,
    ).unwrap(), Some(completed));
    assert_eq!(f.store.adaptive_leadership_review_call(
        &operator.tenant_id, retired.grant.review_id,
    ).unwrap(), Some(retired));
    let grant = receipt.bound_grant().unwrap();
    assert_eq!(grant.schema_version, 6);
    assert_eq!(grant.work_funding.as_ref().unwrap().receipt, funding);
    let reopened = WorkflowStore::open(&f.path).unwrap();
    let published = rows(&reopened);
    assert_eq!(reopened.adaptive_session(f.grant.session_id, &f.grant.assignee_authority)
        .unwrap(), Some(progressed));
    assert_eq!(reopened.authorize_adaptive_funded_review_recovery(
        &operator, &request, &f.grant, &f.context, at,
        |_, _| panic!("immutable publication replay must not reverify"),
    ).unwrap(), (true, receipt));
    assert_eq!(rows(&reopened), published);
}

#[test]
fn replacement_adopted_epoch_postwrite_history_corruption_rolls_back_and_retry_revalidates() {
    use crate::domain_store::adaptive_work_funding::historical_journal_proof_binding;

    for (damage, final_write) in [
        ("root", false), ("continue", false), ("selected-head", false),
        ("adoption", false), ("selected", false), ("replacement", false),
        ("selected-head", true), ("adoption", true), ("replacement", true),
    ] {
        let (f, completed, retired, request, at) = replacement_adopted_recovery_fixture();
        let journal_key = match damage {
            "root" => 1,
            "continue" => completed.continuation.as_ref().unwrap().source_session_version + 1,
            _ => request.source.expected_session_version,
        };
        let mutation = match damage {
            "root" | "continue" | "selected-head" => format!(
                "UPDATE workflow_operations SET request_digest='invalid' WHERE operation_namespace='adaptive-session-v1:{}' AND operation_id='{:020}';",
                f.grant.session_id, journal_key),
            "adoption" => format!(
                "UPDATE company_events SET payload_digest='invalid' WHERE event_type='adaptive_work_funding_adopted' AND operation_id='{}';",
                completed.operation_id),
            "selected" => format!(
                "UPDATE company_entities SET payload_digest='invalid' WHERE entity_kind='adaptive_leadership_review_call' AND entity_id='{}';",
                retired.review_key),
            _ => "UPDATE company_entities SET payload_digest='invalid' WHERE entity_kind='adaptive_work_funding_expired_replacement';".into(),
        };
        let event = if final_write {
            "adaptive_leadership_review_authorized"
        } else {
            "adaptive_funded_review_recovery_issued"
        };
        f.store.connection.lock().unwrap().execute_batch(&format!(
            "CREATE TRIGGER damage_replacement_adopted_recovery AFTER INSERT ON company_events
             WHEN NEW.event_type='{event}'
             BEGIN {mutation} END;"
        )).unwrap();
        let before = rows(&f.store);
        let callback = std::cell::Cell::new(false);
        let (result, failures) = validation_scope::with_validation_failures(|| {
            f.store.authorize_adaptive_funded_review_recovery(
                &funding_operator(&f), &request, &f.grant, &f.context, at,
                |source, request| {
                    let evidence = recovery_evidence(source, request)?;
                    callback.set(true);
                    Ok(evidence)
                },
            )
        });
        assert!(callback.get(), "{damage}/{final_write}: must test postwrite validation");
        assert_eq!(result.unwrap_err().code, WorkflowErrorCode::CorruptStore, "{damage}/{final_write}: {failures:#?}");
        assert!(!failures.iter().any(|failure| matches!(failure.reason,
            validation_scope::ValidationFailureReason::Visiting
                | validation_scope::ValidationFailureReason::BytesLimit)), "{damage}: {failures:#?}");
        assert_eq!(rows(&f.store), before, "{damage}: complete rollback");
        {
            let connection = f.store.connection.lock().unwrap();
            assert!(connection.is_autocommit());
            assert!(historical_journal_proof_binding(&connection, f.grant.session_id).is_none());
            connection.execute_batch("DROP TRIGGER damage_replacement_adopted_recovery").unwrap();
        }
        let receipt = issue_adopted_recovery(&f, &request, at);
        assert_eq!(receipt.request, request);
        assert_eq!(f.store.adaptive_leadership_review_call(&f.leader.tenant_id,
            completed.grant.review_id).unwrap(), Some(completed));
    }
}

#[test]
fn replacement_adopted_epoch_prior_replacement_corruption_during_successor_publication_rolls_back() {
    for damage in ["relation-row", "relation-event"] {
        let (mut f, completed_a, _, unused_a_request, now) = replacement_adopted_recovery_fixture();
        let funding_a = f.grant.work_funding.as_ref().unwrap().receipt.clone();
        let replacement_key = funding_a.request.source.expired_reviewed_replacement
            .as_ref().unwrap().replacement_key.clone();
        let source_a = session(&f);
        let operator = funding_operator(&f);
        let funding_b = fund(&f, now);
        assert_eq!(funding_b.request.source.predecessor_receipt_digest,
            Some(funding_a.receipt_digest().unwrap()));
        assert!(funding_b.request.source.expired_reviewed_replacement.is_none());
        budget_context(&mut f, now, "successor-B-after-authentic-replacement-A-adoption");
        bind(&mut f, &funding_b, funding_b.request.source.resume_source.base_review_count + 1);
        let first_b = dispatch(&f, now);
        let completed_b = f.store.complete_adaptive_leadership_review_call(
            &f.leader, &continued(&first_b, now + 2, 5), now + 2,
        ).unwrap();
        let progressed = observe_budget_inspection(&f, session(&f), now + 3);
        assert!(progressed.model_calls > source_a.model_calls);
        assert!(progressed.tool_calls > source_a.tool_calls);
        let review_at = progressed.active_deadline_ms();
        budget_context(&mut f, review_at, "ordinary-B-after-A-and-B-authentic-adoptions");
        bind(&mut f, &funding_b, funding_b.request.source.resume_source.base_review_count + 2);
        let ordinary = issue(&f, review_at).unwrap();
        let at = ordinary.grant.expires_at_unix_ms;
        let retired = f.store.expire_adaptive_leadership_review_call(
            &f.leader, ordinary.grant.review_id, ordinary.version, at,
        ).unwrap();
        let source = f.store.adaptive_funded_review_recovery_source(
            &operator, &f.grant.project_id, f.grant.session_id, retired.grant.review_id, at,
        ).unwrap();
        assert_eq!(source.funding_receipt_digest, funding_b.receipt_digest().unwrap());
        budget_context(&mut f, at, "schema6-B-retains-authentic-replacement-A-history");
        bind(&mut f, &funding_b, source.ordinal);
        f.grant.expires_at_unix_ms = at + WINDOW_MS;
        let request = crate::AdaptiveFundedReviewRecoveryRequestV1 {
            schema_version: 1,
            operation_id: Uuid::new_v4(),
            source_digest: source.canonical_digest().unwrap(),
            source: Box::new(source),
            release: unused_a_request.release,
            proof_digest: "e".repeat(64),
            reason_ref: "B-selected-recovery-preserves-earlier-adopted-replacement-A".into(),
            expires_at_unix_ms: f.grant.expires_at_unix_ms,
        };
        let history = progressed.continuation.as_ref().unwrap();
        assert_eq!(history.authorizations.iter().filter(|authorization|
            authorization.work_funding.is_some()).count(), 2);
        assert!(history.authorizations.contains(completed_a.continuation.as_ref().unwrap()));
        assert!(history.authorizations.contains(completed_b.continuation.as_ref().unwrap()));
        let mutation = if damage == "relation-row" {
            format!("UPDATE company_entities SET payload_digest='invalid'
                WHERE tenant_id='{}' AND entity_kind='adaptive_work_funding_expired_replacement'
                AND entity_id='{}';", operator.tenant_id.0, replacement_key)
        } else {
            format!("UPDATE company_events SET payload_digest='invalid'
                WHERE tenant_id='{}' AND event_type='adaptive_work_funding_expired_replacement_issued'
                AND operation_id='{}';", operator.tenant_id.0, funding_a.request.operation_id)
        };
        f.store.connection.lock().unwrap().execute_batch(&format!(
            "CREATE TRIGGER damage_prior_adopted_replacement AFTER INSERT ON company_events
             WHEN NEW.event_type='adaptive_leadership_review_authorized'
             BEGIN {mutation} END;"
        )).unwrap();
        let before = rows(&f.store);
        let callback = std::cell::Cell::new(false);
        let (result, failures) = validation_scope::with_validation_failures(|| {
            f.store.authorize_adaptive_funded_review_recovery(
                &operator, &request, &f.grant, &f.context, at, |source, request| {
                    let evidence = recovery_evidence(source, request)?;
                    callback.set(true);
                    Ok(evidence)
                },
            )
        });
        assert!(callback.get(), "{damage}: fresh checks must succeed before staged corruption");
        assert_eq!(result.unwrap_err().code, WorkflowErrorCode::CorruptStore, "{damage}: {failures:#?}");
        assert!(!failures.iter().any(|failure| matches!(failure.reason,
            validation_scope::ValidationFailureReason::Visiting
                | validation_scope::ValidationFailureReason::BytesLimit)), "{damage}: {failures:#?}");
        assert_eq!(rows(&f.store), before, "{damage}: the entire B publication must roll back");
        {
            let connection = f.store.connection.lock().unwrap();
            assert!(connection.is_autocommit());
            assert!(crate::domain_store::adaptive_work_funding::historical_journal_proof_binding(
                &connection, f.grant.session_id).is_none());
            connection.execute_batch("DROP TRIGGER damage_prior_adopted_replacement").unwrap();
        }
        let receipt = issue_adopted_recovery(&f, &request, at);
        assert_eq!(receipt.bound_grant().unwrap().work_funding.as_ref().unwrap().receipt, funding_b);
        assert_eq!(session(&f), progressed);
        for (funding, completed) in [(&funding_a, &completed_a), (&funding_b, &completed_b)] {
            assert_eq!(f.store.adaptive_work_funding(&operator.tenant_id, f.grant.session_id,
                funding.request.operation_id).unwrap(), Some(funding.clone()));
            assert_eq!(f.store.adaptive_leadership_review_call(&operator.tenant_id,
                completed.grant.review_id).unwrap(), Some(completed.clone()));
        }
        let reopened = WorkflowStore::open(&f.path).unwrap();
        let published = rows(&reopened);
        assert_eq!(reopened.authorize_adaptive_funded_review_recovery(
            &operator, &request, &f.grant, &f.context, at,
            |_, _| panic!("intact A-to-B replay must not refresh proof or issue again"),
        ).unwrap(), (true, receipt));
        assert_eq!(rows(&reopened), published);
    }
}

#[test]
fn replacement_adopted_epoch_publication_error_and_unwind_cannot_leave_exception_or_proof() {
    use crate::domain_store::adaptive_work_funding::{
        historical_journal_proof_binding, require_epoch_receipt, with_recovery_publication,
    };

    let (f, _, _, request, at) = replacement_adopted_recovery_fixture();
    let receipt = issue_adopted_recovery(&f, &request, at);
    let grant = receipt.bound_grant().unwrap();
    let call = f.store.adaptive_leadership_review_call(
        &f.leader.tenant_id, grant.review_id,
    ).unwrap().unwrap();
    let before = rows(&f.store);
    let mut connection = f.store.connection.lock().unwrap();
    for outcome in ["error", "unwind", "return-missing"] {
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate).unwrap();
        tx.execute("DELETE FROM company_entities WHERE entity_kind='adaptive_leadership_review_call' AND entity_id=?1",
            [&call.review_key]).unwrap();
        let reached = std::cell::Cell::new(false);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_recovery_publication(&tx, &grant, &call.context_digest().unwrap(),
                call.operation_id, call.grant_issued_at_unix_ms, || {
                    require_epoch_receipt(&tx, grant.work_funding.as_ref().unwrap())?;
                    assert!(historical_journal_proof_binding(&tx, grant.session_id).is_none());
                    reached.set(true);
                    match outcome {
                        "error" => Err(unauthorized()),
                        "unwind" => panic!("controlled replacement-adopted publication unwind"),
                        _ => Ok(()),
                    }
                })
        }));
        assert!(reached.get(), "{outcome}");
        match result {
            Err(_) => assert_eq!(outcome, "unwind"),
            Ok(result) => assert!(result.is_err(), "{outcome}: missing publication must fail"),
        }
        assert!(historical_journal_proof_binding(&tx, grant.session_id).is_none());
        assert_eq!(require_epoch_receipt(&tx, grant.work_funding.as_ref().unwrap())
            .unwrap_err().code, WorkflowErrorCode::CorruptStore);
        tx.rollback().unwrap();
    }
    drop(connection);
    assert_eq!(rows(&f.store), before);
    assert_eq!(f.store.adaptive_funded_review_recovery(&f.leader.tenant_id,
        &receipt.receipt_key).unwrap(), Some(receipt));
}

#[test]
fn replacement_adopted_epoch_historical_proof_is_scoped_and_not_an_ordinary_step_proof() {
    use crate::domain_store::adaptive_work_funding::{
        historical_journal_proof_binding, observe_recovery_journal_source,
    };

    let (f, _, _, request, at) = replacement_adopted_recovery_fixture();
    let receipt = issue_adopted_recovery(&f, &request, at);
    let before = rows(&f.store);
    let mut connection = f.store.connection.lock().unwrap();
    let other = Connection::open(&f.path).unwrap();
    for outcome in ["return", "error", "unwind"] {
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate).unwrap();
        let reached = std::cell::Cell::new(false);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            validation_scope::with_scope(&tx, || {
                observe_recovery_journal_source(&tx, &receipt, || {
                    assert!(historical_journal_proof_binding(&tx, f.grant.session_id).is_some());
                    assert!(historical_journal_proof_binding(&tx, Uuid::new_v4()).is_none());
                    assert!(historical_journal_proof_binding(&other, f.grant.session_id).is_none());
                    reached.set(true);
                    match outcome {
                        "error" => Err(unauthorized()),
                        "unwind" => panic!("controlled exact historical proof unwind"),
                        _ => Ok(()),
                    }
                })?;
                assert!(historical_journal_proof_binding(&tx, f.grant.session_id).is_none());
                let sealed_steps = validation_scope::validations("adaptive-journal-step");
                assert!(sealed_steps > 0);
                crate::store::adaptive::require_funding_journal_source(&tx,
                    receipt.grant_template.work_funding.as_ref().unwrap(),
                    &receipt.context.source_session)?;
                assert!(validation_scope::validations("adaptive-journal-step") > sealed_steps,
                    "sealed historical proofs must not certify ordinary full-membership steps");
                Ok(())
            })
        }));
        assert!(reached.get(), "{outcome}");
        match result {
            Err(_) => assert_eq!(outcome, "unwind"),
            Ok(result) => assert_eq!(result.is_ok(), outcome == "return"),
        }
        assert!(historical_journal_proof_binding(&tx, f.grant.session_id).is_none());
        tx.rollback().unwrap();
    }
    drop(connection);
    assert_eq!(rows(&f.store), before);
}

#[test]
fn replacement_adopted_epoch_exact_prefix_and_warm_external_corruption_are_revalidated() {
    use crate::domain_store::adaptive_work_funding::{
        historical_journal_proof_binding, require_recovery_journal_source,
    };

    let (f, _, _, request, at) = replacement_adopted_recovery_fixture();
    let receipt = issue_adopted_recovery(&f, &request, at);
    let before = rows(&f.store);
    let mut false_source = receipt.clone();
    false_source.context.source_session.continuation.as_mut().unwrap().authorizations
        .iter_mut().find(|authorization| authorization.work_funding.is_some()).unwrap()
        .operation_id = Uuid::new_v4();
    {
        let connection = f.store.connection.lock().unwrap();
        let (result, failures) = validation_scope::with_validation_failures(|| {
            require_recovery_journal_source(&connection, &false_source)
        });
        assert_eq!(result.unwrap_err().code, WorkflowErrorCode::CorruptStore);
        assert!(!failures.iter().any(|failure|
            failure.reason == validation_scope::ValidationFailureReason::Visiting), "{failures:#?}");
        assert!(historical_journal_proof_binding(&connection, f.grant.session_id).is_none());
    }
    assert_eq!(rows(&f.store), before);
    for _ in 0..2 {
        assert_eq!(f.store.adaptive_funded_review_recovery(&f.leader.tenant_id,
            &receipt.receipt_key).unwrap(), Some(receipt.clone()));
    }
    let external = Connection::open(&f.path).unwrap();
    assert_eq!(external.execute(
        "UPDATE workflow_operations SET request_digest='invalid' WHERE operation_namespace=?1 AND operation_id=?2",
        params![format!("adaptive-session-v1:{}", f.grant.session_id),
            format!("{:020}", request.source.expected_session_version)],
    ).unwrap(), 1);
    let corrupted = rows(&f.store);
    for _ in 0..2 {
        assert_eq!(f.store.adaptive_funded_review_recovery(&f.leader.tenant_id,
            &receipt.receipt_key).unwrap_err().code, WorkflowErrorCode::CorruptStore);
        assert_eq!(f.store.authorize_adaptive_funded_review_recovery(&funding_operator(&f),
            &request, &f.grant, &f.context, at,
            |_, _| panic!("corrupt historical replay must not reach fresh verifier"),
        ).unwrap_err().code, WorkflowErrorCode::CorruptStore);
    }
    assert_eq!(rows(&f.store), corrupted);
}

fn adopted_head() -> (Fixture, AdaptiveWorkFundingReceiptV1, u64) {
    let (mut f, funding, now) = funded_fixture();
    let first = dispatch(&f, now);
    f.store
        .complete_adaptive_leadership_review_call(&f.leader, &continued(&first, now + 2, 5), now + 2)
        .unwrap();
    let progressed = observe_budget_inspection(&f, session(&f), now + 3);
    let at = progressed.active_deadline_ms();
    budget_context(&mut f, at, "ordinary-review-after-genuine-adopted-work");
    bind(&mut f, &funding, funding.request.source.resume_source.base_review_count + 2);
    (f, funding, at)
}

fn adopted_recovery_fixture() -> (
    Fixture,
    AdaptiveLeadershipReviewCallV1,
    crate::AdaptiveFundedReviewRecoveryRequestV1,
    u64,
) {
    let (mut f, funding, now) = adopted_head();
    let old = issue(&f, now).unwrap();
    let at = old.grant.expires_at_unix_ms;
    let retired = f.store
        .expire_adaptive_leadership_review_call(&f.leader, old.grant.review_id, old.version, at)
        .unwrap();
    let source = f.store.adaptive_funded_review_recovery_source(
        &funding_operator(&f), &f.grant.project_id, f.grant.session_id, old.grant.review_id, at,
    ).unwrap();
    assert!(source.expected_session_version > funding.request.source.resume_source.expected_session_version);
    assert_eq!(source.ordinal, funding.request.source.resume_source.base_review_count + 3);
    budget_context(&mut f, at, "selected-review-no-io-in-an-adopted-epoch");
    bind(&mut f, &funding, source.ordinal);
    f.grant.expires_at_unix_ms = at + WINDOW_MS;
    let request = crate::AdaptiveFundedReviewRecoveryRequestV1 {
        schema_version: 1,
        operation_id: Uuid::new_v4(),
        source_digest: source.canonical_digest().unwrap(),
        source: Box::new(source),
        release: crate::AdaptiveRecoveryReleaseV1 {
            schema_version: 1,
            source_git_sha: "a".repeat(40),
            release_manifest_digest: "b".repeat(64),
            gateway_binary_digest: "c".repeat(64),
        },
        proof_digest: "d".repeat(64),
        reason_ref: "selected-expired-review-after-productive-adoption".into(),
        expires_at_unix_ms: f.grant.expires_at_unix_ms,
    };
    (f, retired, request, at)
}

fn issue_adopted_recovery(
    f: &Fixture,
    request: &crate::AdaptiveFundedReviewRecoveryRequestV1,
    at: u64,
) -> crate::AdaptiveFundedReviewRecoveryReceiptV1 {
    f.store.authorize_adaptive_funded_review_recovery(
        &funding_operator(f), request, &f.grant, &f.context, at,
        |source, request| {
            let prior = source.calls.iter().find(|call| call.dispatch.is_some()
                && call.continuation.is_some()
                && call.grant.work_funding.as_ref().is_some_and(|epoch|
                    epoch.receipt == *source.funding))
                .expect("genuine prior adoption in the same funding epoch");
            assert!(source.review_has_prior_adoption(prior));
            for call in &source.calls {
                assert_eq!(
                    source.review_has_prior_adoption(call),
                    call.grant.review_id == prior.grant.review_id,
                    "only the genuine prior adoption may skip physical NoIO"
                );
            }
            let selected = source.calls.iter().find(|call|
                call.grant.review_id == source.source.retired_review_id).unwrap();
            assert!(selected.retired_at_unix_ms.is_some());
            assert!(!source.review_has_prior_adoption(selected));

            // Classification must not match merely by a completed call's shape.
            // These clones never replace any persisted call or journal prefix.
            let mut unpinned = prior.clone();
            unpinned.continuation.as_mut().unwrap().operation_id = Uuid::new_v4();
            assert!(!source.review_has_prior_adoption(&unpinned));
            let mut foreign_epoch = prior.clone();
            foreign_epoch.grant.work_funding.as_mut().unwrap().binding.funding_id =
                "work-funding-foreign".into();
            assert!(!source.review_has_prior_adoption(&foreign_epoch));
            let mut foreign_authority = prior.clone();
            foreign_authority.grant.assignee_authority.assignment_version += 1;
            assert!(!source.review_has_prior_adoption(&foreign_authority));
            let evidence = recovery_evidence(source, request)?;
            assert_eq!(evidence.no_io, crate::AdaptiveFundedReviewNoIoV1::NeverReserved {
                request_id: format!("company-leadership-{}", source.source.retired_review_id),
            });
            Ok(evidence)
        },
    ).unwrap().1
}

#[test]
fn adopted_epoch_recovers_only_selected_expired_review_and_preserves_spend_history() {
    let (f, retired, request, at) = adopted_recovery_fixture();
    let before = session(&f);
    let retained = f.store.adaptive_leadership_review_calls(
        &f.leader.tenant_id, f.grant.session_id,
    ).unwrap();
    let prior_bytes = encode(&retained).unwrap();
    let funding = f.grant.work_funding.as_ref().unwrap().receipt.clone();
    {
        let connection = f.store.connection.lock().unwrap();
        // Genuine prior I/O must still veto the old whole-epoch NoIO contract.
        assert_eq!(adaptive_work_funding::require_never_dispatched_funding(
            &connection, f.grant.work_funding.as_ref().unwrap(),
        ).unwrap_err().code, WorkflowErrorCode::InvalidTransition);
    }
    assert!(f.store.adaptive_work_funding_for_review(
        &f.leader.tenant_id, f.grant.session_id, at,
    ).unwrap().is_none(), "ordinary same-head rerolls must remain forbidden");
    let receipt = issue_adopted_recovery(&f, &request, at);
    assert_eq!(session(&f), before);
    let mut calls = f.store.adaptive_leadership_review_calls(
        &f.leader.tenant_id, f.grant.session_id,
    ).unwrap();
    let recovered = calls.iter().find(|call| call.grant.review_id == receipt.grant_template.review_id)
        .unwrap().clone();
    assert_eq!(recovered.grant.schema_version, 6);
    assert_eq!(recovered.grant.work_funding.as_ref().unwrap().receipt, funding);
    assert_eq!(recovered.grant.work_funding.as_ref().unwrap().binding.limits,
        f.grant.work_funding.as_ref().unwrap().binding.limits);
    calls.retain(|call| call.grant.review_id != recovered.grant.review_id);
    assert_eq!(encode(&calls).unwrap(), prior_bytes);
    let claimed = f.store.claim_adaptive_leadership_review_call(
        &f.leader, &claim(&recovered), at + 1,
    ).unwrap();
    let result = continued(&claimed, at + 2, 1);
    let completed = f.store.complete_adaptive_leadership_review_call(
        &f.leader, &result, at + 2,
    ).unwrap();
    let current = session(&f);
    assert_eq!(current.grant, before.grant);
    assert_eq!(current.version, before.version + 1);
    assert_eq!((current.model_calls, current.tool_calls), (before.model_calls, before.tool_calls));
    let history = &current.continuation.as_ref().unwrap().authorizations;
    assert_eq!(history.len(), before.continuation.as_ref().unwrap().authorizations.len() + 1);
    assert!(history.starts_with(&before.continuation.as_ref().unwrap().authorizations));
    assert_eq!(f.store.adaptive_leadership_review_call(
        &f.leader.tenant_id, retired.grant.review_id,
    ).unwrap(), Some(retired));
    assert_eq!(f.store.adaptive_work_funding(
        &f.leader.tenant_id, f.grant.session_id, funding.request.operation_id,
    ).unwrap(), Some(funding.clone()));
    let reopened = WorkflowStore::open(&f.path).unwrap();
    let rows_before = rows(&reopened);
    let late = funding.request.limits.expires_at_unix_ms + 1;
    let replayed = reopened.authorize_adaptive_funded_review_recovery(
        &funding_operator(&f), &request, &f.grant, &f.context, late,
        |_, _| panic!("immutable replay must not reverify fresh NoIO"),
    ).unwrap();
    assert_eq!(replayed, (true, receipt.clone()));
    assert_eq!(reopened.complete_adaptive_leadership_review_call(
        &f.leader, &result, late,
    ).unwrap(), completed);
    let historical = reopened.adaptive_funded_review_recovery_historical_verification_source(
        &f.leader.tenant_id, &receipt.receipt_key,
    ).unwrap().unwrap();
    assert_eq!(*historical.session, before);
    assert_eq!(encode(&historical.calls).unwrap(), prior_bytes);
    assert_eq!(*historical.funding, funding);
    assert_eq!(rows(&reopened), rows_before);
}

#[test]
fn adopted_recovery_rejects_live_dispatched_refused_or_consumed_selected_reviews() {
    for state in ["live", "dispatched", "defer", "continue"] {
        let (f, _, now) = adopted_head();
        let issued = issue(&f, now).unwrap();
        if state != "live" {
            let call = f.store.claim_adaptive_leadership_review_call(
                &f.leader, &claim(&issued), now + 1,
            ).unwrap();
            match state {
                "dispatched" => { f.store.expire_adaptive_leadership_review_call(
                    &f.leader, call.grant.review_id, call.version, call.grant.expires_at_unix_ms,
                ).unwrap(); }
                "defer" => { f.store.complete_adaptive_leadership_review_call(
                    &f.leader, &defer(&call), now + 2,
                ).unwrap(); }
                "continue" => { f.store.complete_adaptive_leadership_review_call(
                    &f.leader, &continued(&call, now + 2, 1), now + 2,
                ).unwrap(); }
                _ => unreachable!(),
            }
        }
        let snapshot = rows(&f.store);
        assert!(f.store.adaptive_funded_review_recovery_source(
            &funding_operator(&f), &f.grant.project_id, f.grant.session_id,
            issued.grant.review_id, issued.grant.expires_at_unix_ms,
        ).is_err(), "{state}");
        assert_eq!(rows(&f.store), snapshot);
    }
}

#[test]
fn adopted_recovery_no_io_verifier_failure_wrong_target_and_publication_failure_are_atomic() {
    for failure in ["verifier", "target", "publication"] {
        let (f, _, request, at) = adopted_recovery_fixture();
        if failure == "publication" {
            f.store.connection.lock().unwrap().execute_batch(
                "CREATE TRIGGER reject_adopted_recovery BEFORE INSERT ON company_events
                 WHEN NEW.event_type='adaptive_leadership_review_authorized'
                 BEGIN SELECT RAISE(ABORT, 'test publication failure'); END;",
            ).unwrap();
        }
        let snapshot = rows(&f.store);
        let failed = f.store.authorize_adaptive_funded_review_recovery(
            &funding_operator(&f), &request, &f.grant, &f.context, at,
            |source, request| {
                if failure == "verifier" { return Err(unauthorized()); }
                let mut evidence = recovery_evidence(source, request)?;
                if failure == "target" {
                    let prior = source.calls.iter().find(|call| call.continuation.is_some()).unwrap();
                    evidence.no_io = crate::AdaptiveFundedReviewNoIoV1::NeverReserved {
                        request_id: prior.request_id(),
                    };
                }
                Ok(evidence)
            },
        );
        assert!(failed.is_err(), "{failure}");
        assert_eq!(rows(&f.store), snapshot);
        assert!(f.store.connection.lock().unwrap().is_autocommit());
        if failure == "publication" {
            f.store.connection.lock().unwrap().execute_batch("DROP TRIGGER reject_adopted_recovery").unwrap();
        }
        issue_adopted_recovery(&f, &request, at);
    }
}

#[test]
fn adopted_recovery_replay_revalidates_original_history_and_exact_request() {
    for damage in ["prior-call", "prior-adoption", "selected-call", "journal"] {
        let (f, retired, request, at) = adopted_recovery_fixture();
        let receipt = issue_adopted_recovery(&f, &request, at);
        f.store.adaptive_funded_review_recovery(&f.leader.tenant_id, &receipt.receipt_key).unwrap();
        let external = Connection::open(&f.path).unwrap();
        let changed = match damage {
            "journal" => external.execute(
                "UPDATE workflow_operations SET request_digest='invalid'
                 WHERE operation_namespace=?1 AND operation_id='00000000000000000001'",
                [format!("adaptive-session-v1:{}", f.grant.session_id)],
            ),
            "prior-adoption" => external.execute(
                "UPDATE company_events SET payload_digest='invalid'
                 WHERE event_type='adaptive_work_funding_adopted'", [],
            ),
            "prior-call" => external.execute(
                "UPDATE company_entities SET payload_digest='invalid'
                 WHERE entity_kind='adaptive_leadership_review_call'
                 AND json_extract(payload,'$.continuation.work_funding.binding.funding_id')=?1",
                [&f.grant.work_funding.as_ref().unwrap().binding.funding_id],
            ),
            _ => external.execute(
                "UPDATE company_entities SET payload_digest='invalid'
                 WHERE entity_kind='adaptive_leadership_review_call' AND entity_id=?1",
                [retired.grant.review_id.to_string()],
            ),
        }.unwrap();
        assert!(changed > 0, "{damage}");
        let snapshot = rows(&f.store);
        for _ in 0..2 {
            assert_eq!(f.store.adaptive_funded_review_recovery(
                &f.leader.tenant_id, &receipt.receipt_key,
            ).unwrap_err().code, WorkflowErrorCode::CorruptStore);
            assert_eq!(f.store.authorize_adaptive_funded_review_recovery(
                &funding_operator(&f), &request, &f.grant, &f.context, at,
                |_, _| panic!("corrupt replay must not reach verifier"),
            ).unwrap_err().code, WorkflowErrorCode::CorruptStore);
        }
        assert_eq!(rows(&f.store), snapshot);
    }
    let (f, _, request, at) = adopted_recovery_fixture();
    issue_adopted_recovery(&f, &request, at);
    let snapshot = rows(&f.store);
    let mut changed = request.clone();
    changed.operation_id = Uuid::new_v4();
    assert!(f.store.authorize_adaptive_funded_review_recovery(
        &funding_operator(&f), &changed, &f.grant, &f.context, at,
        |_, _| panic!("changed replay must not reverify or issue"),
    ).is_err());
    assert_eq!(rows(&f.store), snapshot);
}

#[test]
fn adopted_recovery_keeps_funding_expiry_and_binding_limits_immutable() {
    let (f, retired, request, at) = adopted_recovery_fixture();
    let epoch = f.grant.work_funding.as_ref().unwrap();
    let snapshot = rows(&f.store);
    assert!(f.store.adaptive_funded_review_recovery_source(
        &funding_operator(&f), &f.grant.project_id, f.grant.session_id, retired.grant.review_id,
        epoch.binding.limits.expires_at_unix_ms,
    ).is_err());
    let mut grant = f.grant.clone();
    grant.work_funding.as_mut().unwrap().binding.limits.total_review_ceiling += 1;
    assert!(f.store.authorize_adaptive_funded_review_recovery(
        &funding_operator(&f), &request, &grant, &f.context, at, recovery_evidence,
    ).is_err());
    assert_eq!(rows(&f.store), snapshot);
}

#[test]
fn adopted_recovery_designated_effects_still_require_a_postissuance_cut() {
    for event in ["adaptive_leadership_review_dispatched", "adaptive_work_funding_adopted"] {
        let (f, _, request, at) = adopted_recovery_fixture();
        let receipt = issue_adopted_recovery(&f, &request, at);
        let grant = receipt.bound_grant().unwrap();
        let recovered = f.store.adaptive_leadership_review_call(
            &f.leader.tenant_id, grant.review_id,
        ).unwrap().unwrap();
        let claimed = f.store.claim_adaptive_leadership_review_call(
            &f.leader, &claim(&recovered), at + 1,
        ).unwrap();
        if event == "adaptive_work_funding_adopted" {
            f.store.complete_adaptive_leadership_review_call(
                &f.leader, &continued(&claimed, at + 2, 1), at + 2,
            ).unwrap();
        }
        let mut connection = f.store.connection.lock().unwrap();
        let sequence = |kind: &str| -> i64 {
            connection.query_row(
                "SELECT sequence FROM company_events WHERE event_type=?1 AND operation_id=?2",
                params![kind, request.operation_id.to_string()], |row| row.get(0),
            ).unwrap()
        };
        let cut = sequence("adaptive_funded_review_recovery_issued");
        let before = sequence("adaptive_work_funding_review_issued");
        let effect = sequence(event);
        assert!(before < cut && cut < effect);
        let tx = connection.transaction().unwrap();
        // Event payloads, seals and timestamps stay unchanged; only ordering moves.
        tx.execute("UPDATE company_events SET sequence=-1 WHERE sequence=?1", [before]).unwrap();
        tx.execute("UPDATE company_events SET sequence=?1 WHERE sequence=?2", params![before, effect]).unwrap();
        tx.execute("UPDATE company_events SET sequence=?1 WHERE sequence=-1", [effect]).unwrap();
        tx.commit().unwrap();
        assert_eq!(validation_scope::with_scope(&connection, || {
            adaptive_work_funding::require_pre_recovery_funding_history(
                &connection, grant.work_funding.as_ref().unwrap(),
                Some((&grant, &receipt.context, request.operation_id, receipt.issued_at_unix_ms,
                    stored_u64(cut)?)),
            )
        }).unwrap_err().code, WorkflowErrorCode::InvalidTransition);
        drop(connection);
        let snapshot = rows(&f.store);
        assert_eq!(f.store.adaptive_funded_review_recovery(
            &f.leader.tenant_id, &receipt.receipt_key,
        ).unwrap_err().code, WorkflowErrorCode::CorruptStore);
        assert_eq!(rows(&f.store), snapshot);
    }
}

#[test]
fn adopted_recovery_sealed_publication_preserves_prior_adoption_and_selected_no_io() {
    let (f, retired, request, at) = adopted_recovery_fixture();
    let before = session(&f);
    // Authorization includes the post-publication sealed call/recovery link check.
    let receipt = issue_adopted_recovery(&f, &request, at);
    assert_eq!(session(&f), before);
    let grant = receipt.bound_grant().unwrap();
    let call = f.store.adaptive_leadership_review_call(
        &f.leader.tenant_id, grant.review_id,
    ).unwrap().unwrap();
    assert_eq!(call.grant, grant);
    assert_eq!(f.store.adaptive_leadership_review_call(
        &f.leader.tenant_id, retired.grant.review_id,
    ).unwrap(), Some(retired));
    assert_eq!(f.store.adaptive_work_funding(
        &f.leader.tenant_id, f.grant.session_id,
        grant.work_funding.as_ref().unwrap().receipt.request.operation_id,
    ).unwrap(), Some(grant.work_funding.as_ref().unwrap().receipt.clone()));
    f.store.claim_adaptive_leadership_review_call(&f.leader, &claim(&call), at + 1).unwrap();
    let reopened = WorkflowStore::open(&f.path).unwrap();
    let snapshot = rows(&reopened);
    assert_eq!(reopened.adaptive_funded_review_recovery(
        &f.leader.tenant_id, &receipt.receipt_key,
    ).unwrap(), Some(receipt));
    assert_eq!(rows(&reopened), snapshot);
}

fn reseal_recovery_test_event<T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + 'static>(
    tx: &Transaction<'_>,
    principal: &AuthenticatedCompanyPrincipalV1,
    project: &ProjectId,
    identity: (&str, Uuid, &'static str, u64),
    value: &T,
) {
    let (event, operation, domain, at) = identity;
    let old: i64 = tx.query_row(
        "SELECT sequence FROM company_events WHERE tenant_id=?1 AND event_type=?2 AND operation_id=?3",
        params![principal.tenant_id.0, event, operation.to_string()], |row| row.get(0),
    ).unwrap();
    assert_eq!(tx.execute("DELETE FROM company_events WHERE sequence=?1", [old]).unwrap(), 1);
    let digest = canonical_sha256(domain, value).unwrap();
    let new = append_event(tx, principal, operation, &digest, Some(project), event, value, at).unwrap();
    assert_eq!(tx.execute("UPDATE company_events SET sequence=?1 WHERE sequence=?2", params![old, sql_u64(new).unwrap()]).unwrap(), 1);
    adaptive_resume_policy::require_leaf_event(
        tx, &principal.tenant_id, project, event, operation, &digest, principal, at, value,
    ).unwrap();
}

#[test]
fn adopted_recovery_coherently_resealed_false_source_head_fails_read_only_replay() {
    let (f, _, request, at) = adopted_recovery_fixture();
    let receipt = issue_adopted_recovery(&f, &request, at);
    let mut bad = receipt.clone();
    bad.request.source.session_head_digest = "f".repeat(64);
    assert_ne!(bad.request.source.session_head_digest, receipt.request.source.session_head_digest);
    bad.request.source_digest = bad.request.source.canonical_digest().unwrap();
    bad.evidence.source_digest = bad.request.source_digest.clone();
    bad.receipt_key = bad.request.source.receipt_key().unwrap();
    // The semantic session and all bounded authority remain exact. Only the
    // claimed source-version journal digest is false, with every outer seal rebuilt.
    bad.validate_entity().unwrap();
    let mut call = f.store.adaptive_leadership_review_call(
        &f.leader.tenant_id, receipt.grant_template.review_id,
    ).unwrap().unwrap();
    call.grant = bad.bound_grant().unwrap();
    call.allowance_id = format!("{}-allowance", bad.receipt_key);
    call.validate_entity().unwrap();
    let mut connection = f.store.connection.lock().unwrap();
    let (membership_id, payload): (String, Vec<u8>) = connection.query_row(
        "SELECT entity_id,payload FROM company_entities
         WHERE entity_kind='adaptive_work_funding_review' AND json_extract(payload,'$.operation_id')=?1",
        [request.operation_id.to_string()], |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    let mut membership: serde_json::Value = decode(&payload).unwrap();
    membership["grant"] = serde_json::to_value(&call.grant).unwrap();
    membership["context_digest"] = serde_json::json!(call.context_digest().unwrap());
    let tx = connection.transaction().unwrap();
    assert_eq!(tx.execute(
        "DELETE FROM company_entities WHERE entity_kind='adaptive_funded_review_recovery' AND entity_id=?1",
        [&receipt.receipt_key],
    ).unwrap(), 1);
    put_entity(&tx, &f.leader.tenant_id, "adaptive_funded_review_recovery", &bad.receipt_key, 1, &bad).unwrap();
    put_entity(&tx, &f.leader.tenant_id, "adaptive_leadership_review_call", &call.review_key, call.version, &call).unwrap();
    put_entity(&tx, &f.leader.tenant_id, "adaptive_work_funding_review", &membership_id, 1, &membership).unwrap();
    reseal_recovery_test_event(&tx, &f.leader, &f.grant.project_id, (
        "adaptive_work_funding_review_issued", request.operation_id,
        "sentinel.workflow.adaptive-work-funding-review.v1", at,
    ), &membership);
    reseal_recovery_test_event(&tx, &funding_operator(&f), &f.grant.project_id, (
        "adaptive_funded_review_recovery_issued", request.operation_id,
        "sentinel.workflow.funded-review-recovery-receipt.v1", at,
    ), &bad);
    reseal_recovery_test_event(&tx, &f.leader, &f.grant.project_id, (
        "adaptive_leadership_review_authorized", request.operation_id,
        "sentinel.workflow.adaptive-leadership-call.v1", at,
    ), &call);
    tx.commit().unwrap();
    // Prefix membership is still valid: rejection must check the claimed digest too.
    crate::store::adaptive::require_funding_journal_source(
        &connection, bad.grant_template.work_funding.as_ref().unwrap(), &bad.context.source_session,
    ).unwrap();
    drop(connection);
    let reopened = WorkflowStore::open(&f.path).unwrap();
    let snapshot = rows(&reopened);
    for _ in 0..2 {
        assert_eq!(reopened.adaptive_funded_review_recovery(
            &f.leader.tenant_id, &bad.receipt_key,
        ).unwrap_err().code, WorkflowErrorCode::CorruptStore);
        assert_eq!(reopened.adaptive_funded_review_recovery_historical_verification_source(
            &f.leader.tenant_id, &bad.receipt_key,
        ).unwrap_err().code, WorkflowErrorCode::CorruptStore);
        assert_eq!(reopened.authorize_adaptive_funded_review_recovery(
            &funding_operator(&f), &bad.request, &bad.grant_template, &bad.context, at,
            |_, _| panic!("resealed false-head replay must not reverify or issue"),
        ).unwrap_err().code, WorkflowErrorCode::CorruptStore);
    }
    assert_eq!(rows(&reopened), snapshot);
    assert!(reopened.connection.lock().unwrap().is_autocommit());
}
