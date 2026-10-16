use super::*;
use std::cell::Cell;

#[test]
fn continuation_row_shape_warmth_never_authorizes_invalid_full_view() {
    for intrinsic in [true, false] {
        let (f, mut call, _) = funded_review(false);
        if intrinsic {
            call.schema_version = 0;
            assert!(call.validate_entity().is_err());
        } else {
            let original = call.context.evidence_refs.clone();
            call.context.evidence_refs.reverse();
            assert_ne!(call.context.evidence_refs, original);
            let digest = call.context_digest().unwrap();
            call.dispatch.as_mut().unwrap().context_digest = digest;
            call.validate_entity().unwrap();
        }
        let payload = encode(&call).unwrap();
        let digest = bytes_digest("sentinel.workflow.company-entity-row.v1", &payload).unwrap();
        let connection = f.store.connection.lock().unwrap();
        replace_row(
            &connection,
            &call,
            i64::try_from(call.version).unwrap(),
            &payload,
            &digest,
        );
        validation_scope::with_scope(&connection, || {
            assert_eq!(read_row(&connection, &call)?, call);
            assert_eq!(validation_scope::validations("entity"), 0);
            let error = get_entity::<AdaptiveLeadershipReviewCallV1>(
                &connection,
                &f.leader.tenant_id,
                KIND,
                &call.grant.review_id.to_string(),
            )
            .unwrap_err();
            assert_eq!(error.code, WorkflowErrorCode::CorruptStore);
            Ok(())
        })
        .unwrap();
    }
}

fn funded_review(recovered: bool) -> (Fixture, AdaptiveLeadershipReviewCallV1, u64) {
    let mut f = budget_fixture(2);
    let now = CONTINUATION_AT + 7;
    let funding = work_funding_tests::fund(&f, now);
    work_funding_tests::bind(
        &mut f,
        &funding,
        funding.request.source.resume_source.base_review_count + 1,
    );
    if !recovered {
        let call = work_funding_tests::dispatch(&f, now);
        return (f, call, now + 2);
    }
    let old = f
        .store
        .authorize_adaptive_leadership_review_call(
            &f.leader,
            Uuid::new_v4(),
            "row-shape-expiring-review",
            &f.grant,
            &f.context,
            now,
        )
        .unwrap();
    let at = old.grant.expires_at_unix_ms;
    f.store
        .expire_adaptive_leadership_review_call(&f.leader, old.grant.review_id, old.version, at)
        .unwrap();
    let mut operator = f.leader.clone();
    operator.kind = CompanyPrincipalKindV1::Operator;
    operator.agent_id = None;
    let source = f
        .store
        .adaptive_funded_review_recovery_source(
            &operator,
            &f.grant.project_id,
            f.grant.session_id,
            old.grant.review_id,
            at,
        )
        .unwrap();
    budget_context(&mut f, at, "row-shape-recovery");
    work_funding_tests::bind(&mut f, &funding, source.ordinal);
    f.grant.expires_at_unix_ms = at + 180_000;
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
        reason_ref: "row-shape-recovery".into(),
        expires_at_unix_ms: f.grant.expires_at_unix_ms,
    };
    let receipt = f
        .store
        .authorize_adaptive_funded_review_recovery(
            &operator,
            &request,
            &f.grant,
            &f.context,
            at,
            |source, request| {
                let old = source
                    .calls
                    .iter()
                    .find(|call| call.grant.review_id == source.source.retired_review_id)
                    .unwrap();
                Ok(crate::AdaptiveFundedReviewRecoveryEvidenceV1 {
                    source_digest: source.source.canonical_digest()?,
                    proof_digest: request.proof_digest.clone(),
                    release: request.release.clone(),
                    no_io: crate::AdaptiveFundedReviewNoIoV1::NeverReserved {
                        request_id: old.request_id(),
                    },
                    verified_at_unix_ms: old.retired_at_unix_ms.unwrap(),
                })
            },
        )
        .unwrap()
        .1;
    let call = f
        .store
        .adaptive_leadership_review_call(&f.leader.tenant_id, receipt.grant_template.review_id)
        .unwrap()
        .unwrap();
    let dispatched = f
        .store
        .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), at + 1)
        .unwrap();
    (f, dispatched, at + 2)
}

fn read_row(
    connection: &Connection,
    call: &AdaptiveLeadershipReviewCallV1,
) -> Result<AdaptiveLeadershipReviewCallV1, WorkflowError> {
    WorkflowStore::adaptive_leadership_review_row_in_snapshot(
        connection,
        &call.grant.leadership_principal.tenant_id,
        call.grant.review_id,
        call.version,
    )
}

fn replace_row(
    connection: &Connection,
    call: &AdaptiveLeadershipReviewCallV1,
    version: i64,
    payload: &[u8],
    digest: &str,
) {
    assert_eq!(
        connection
            .execute(
                "UPDATE company_entities SET version=?1,payload=?2,payload_digest=?3
                 WHERE tenant_id=?4 AND entity_kind=?5 AND entity_id=?6",
                params![
                    version,
                    payload,
                    digest,
                    call.grant.leadership_principal.tenant_id.0,
                    KIND,
                    call.grant.review_id.to_string(),
                ],
            )
            .unwrap(),
        1,
    );
}

#[test]
fn continuation_row_shape_materializes_once_across_full_and_row_views() {
    for recovered in [false, true] {
        let (f, call, _) = funded_review(recovered);
        let original = encode(&call).unwrap();
        let mut connection = f.store.connection.lock().unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        validation_scope::with_scope(&transaction, || {
            let copies = Cell::new(0);
            let first: AdaptiveLeadershipReviewCallV1 = entity_shape_with_observer(
                &transaction,
                &f.leader.tenant_id,
                KIND,
                &call.grant.review_id.to_string(),
                |bytes| {
                    assert_eq!(bytes, original.len());
                    copies.set(copies.get() + 1);
                },
            )?
            .unwrap();
            let full: AdaptiveLeadershipReviewCallV1 = get_entity(
                &transaction,
                &f.leader.tenant_id,
                KIND,
                &call.grant.review_id.to_string(),
            )?
            .unwrap();
            let shapes = validation_scope::validations("entity-shape");
            let second = adaptive_leadership_review_row_with_observer(
                &transaction,
                &f.leader.tenant_id,
                call.grant.review_id,
                call.version,
                |_| copies.set(copies.get() + 1),
            )?;
            assert_eq!(copies.get(), 1);
            assert_eq!(validation_scope::validations("entity-shape"), shapes);
            assert_eq!(first, call);
            assert_eq!(full, call);
            assert_eq!(second, call);
            assert_eq!(read_row(&transaction, &call)?, call);
            assert_eq!(encode(&second).unwrap(), original);
            Ok(())
        })
        .unwrap();
    }
}

#[test]
fn continuation_row_shape_standalone_miss_seeds_the_same_authenticated_shape() {
    let (f, call, _) = funded_review(false);
    let connection = f.store.connection.lock().unwrap();
    validation_scope::with_scope(&connection, || {
        let copies = Cell::new(0);
        let first = adaptive_leadership_review_row_with_observer(
            &connection,
            &f.leader.tenant_id,
            call.grant.review_id,
            call.version,
            |_| copies.set(copies.get() + 1),
        )?;
        let shape: AdaptiveLeadershipReviewCallV1 = entity_shape_with_observer(
            &connection,
            &f.leader.tenant_id,
            KIND,
            &call.grant.review_id.to_string(),
            |_| copies.set(copies.get() + 1),
        )?
        .unwrap();
        assert_eq!(copies.get(), 1);
        assert_eq!(first, call);
        assert_eq!(shape, call);
        Ok(())
    })
    .unwrap();
    assert_eq!(read_row(&connection, &call).unwrap(), call);
}

#[test]
fn continuation_row_shape_preserves_cold_digest_version_decode_error_order() {
    for damage in 0..4 {
        let (f, call, _) = funded_review(false);
        let connection = f.store.connection.lock().unwrap();
        let payload = b"not-json";
        let digest = bytes_digest("sentinel.workflow.company-entity-row.v1", payload).unwrap();
        let version = i64::try_from(call.version).unwrap();
        replace_row(
            &connection,
            &call,
            if damage == 3 {
                -1
            } else {
                version + i64::from(damage < 2)
            },
            payload,
            if damage == 0 { "invalid" } else { &digest },
        );
        let error = read_row(&connection, &call).unwrap_err();
        match damage {
            0 | 3 => assert_eq!(error.code, WorkflowErrorCode::CorruptStore),
            1 => {
                assert_eq!(error.code, WorkflowErrorCode::VersionConflict);
                assert_eq!(error.message, "adaptive continuation source row changed");
            }
            _ => assert_eq!(
                error,
                decode::<AdaptiveLeadershipReviewCallV1>(payload).unwrap_err(),
            ),
        }
    }
}

#[test]
fn continuation_row_shape_checks_warm_expected_version_and_complete_key() {
    let (f, call, _) = funded_review(false);
    let connection = f.store.connection.lock().unwrap();
    validation_scope::with_scope(&connection, || {
        assert_eq!(read_row(&connection, &call)?, call);
        let mut changed = call.clone();
        changed.version += 1;
        assert_eq!(
            read_row(&connection, &changed).unwrap_err().code,
            WorkflowErrorCode::VersionConflict,
        );
        changed = call.clone();
        changed.grant.review_id = Uuid::new_v4();
        assert_eq!(
            read_row(&connection, &changed).unwrap_err().code,
            WorkflowErrorCode::NotFound,
        );
        changed = call.clone();
        changed.grant.leadership_principal.tenant_id = TenantId::parse("foreign-tenant").unwrap();
        assert_eq!(
            read_row(&connection, &changed).unwrap_err().code,
            WorkflowErrorCode::NotFound,
        );
        assert_eq!(read_row(&connection, &call)?, call);
        Ok(())
    })
    .unwrap();
}

#[test]
fn continuation_row_shape_cold_reader_cannot_seed_mismatched_row_binding() {
    for damage in 0..3 {
        let (f, call, _) = funded_review(false);
        let mut changed = call.clone();
        match damage {
            0 => changed.review_key = Uuid::new_v4().to_string(),
            1 => {
                changed.grant.leadership_principal.tenant_id =
                    TenantId::parse("foreign-tenant").unwrap();
            }
            _ => changed.version += 1,
        }
        let payload = encode(&changed).unwrap();
        let digest = bytes_digest("sentinel.workflow.company-entity-row.v1", &payload).unwrap();
        let connection = f.store.connection.lock().unwrap();
        replace_row(
            &connection,
            &call,
            i64::try_from(call.version).unwrap(),
            &payload,
            &digest,
        );
        assert_eq!(
            read_row(&connection, &call).unwrap_err().code,
            WorkflowErrorCode::CorruptStore,
        );
    }
}

#[test]
fn continuation_row_shape_local_writes_invalidate_warm_proofs_without_before_write() {
    for damage in 0..4 {
        let (f, call, _) = funded_review(false);
        let mut connection = f.store.connection.lock().unwrap();
        let transaction = connection.transaction().unwrap();
        validation_scope::with_scope(&transaction, || {
            assert_eq!(read_row(&transaction, &call)?, call);
            let payload = if damage == 0 {
                b"invalid-json".to_vec()
            } else {
                encode(&call).unwrap()
            };
            let digest = bytes_digest("sentinel.workflow.company-entity-row.v1", &payload).unwrap();
            replace_row(
                &transaction,
                &call,
                i64::try_from(call.version).unwrap() + i64::from(damage == 2),
                &payload,
                if damage == 1 { "invalid" } else { &digest },
            );
            if damage == 3 {
                transaction.execute(
                    "DELETE FROM company_entities WHERE tenant_id=?1 AND entity_kind=?2 AND entity_id=?3",
                    params![f.leader.tenant_id.0, KIND, call.grant.review_id.to_string()],
                )?;
            }
            assert!(read_row(&transaction, &call).is_err());
            Ok(())
        })
        .unwrap();
        transaction.rollback().unwrap();
        assert_eq!(read_row(&connection, &call).unwrap(), call);
    }
}

#[test]
fn continuation_row_shape_same_version_valid_replacement_is_not_old_input() {
    let (f, call, _) = funded_review(false);
    let mut changed = call.clone();
    changed.context.evidence_refs.reverse();
    assert_ne!(changed.context, call.context);
    let payload = encode(&changed).unwrap();
    let digest = bytes_digest("sentinel.workflow.company-entity-row.v1", &payload).unwrap();
    let mut connection = f.store.connection.lock().unwrap();
    let transaction = connection.transaction().unwrap();
    validation_scope::with_scope(&transaction, || {
        assert_eq!(read_row(&transaction, &call)?, call);
        replace_row(
            &transaction,
            &call,
            i64::try_from(call.version).unwrap(),
            &payload,
            &digest,
        );
        assert_eq!(read_row(&transaction, &call)?, changed);
        assert_ne!(read_row(&transaction, &call)?, call);
        Ok(())
    })
    .unwrap();
    transaction.rollback().unwrap();
}

#[test]
fn continuation_row_shape_full_compare_still_rejects_changed_caller() {
    let (f, call, now) = funded_review(false);
    let result = work_funding_tests::continued(&call, now, 1);
    let authorization = result.continuation.as_ref().unwrap();
    let allowance = call
        .continuation_allowance(now, authorization.deadline_ms, 1)
        .unwrap();
    let mut changed = call.clone();
    changed.updated_at_unix_ms += 1;
    let before = rows(&f.store);
    let mut connection = f.store.connection.lock().unwrap();
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    validation_scope::with_scope(&transaction, || {
        assert_eq!(read_row(&transaction, &call)?, call);
        assert_eq!(
            crate::store::adaptive::continue_adaptive_session_in_transaction(
                &transaction,
                authorization,
                &changed,
                &allowance,
                &call.grant.assignee_authority,
                now,
            )
            .unwrap_err()
            .code,
            WorkflowErrorCode::AuthorityConflict,
        );
        Ok(())
    })
    .unwrap();
    transaction.rollback().unwrap();
    drop(connection);
    assert_eq!(rows(&f.store), before);
}

#[test]
fn continuation_row_shape_external_write_is_pinned_only_until_scope_end_and_reopen() {
    let (f, call, _) = funded_review(false);
    let connection = f.store.connection.lock().unwrap();
    let writer = Connection::open(&f.path).unwrap();
    validation_scope::with_scope(&connection, || {
        assert_eq!(read_row(&connection, &call)?, call);
        let payload = encode(&call).unwrap();
        replace_row(
            &writer,
            &call,
            i64::try_from(call.version).unwrap(),
            &payload,
            "invalid",
        );
        assert_eq!(read_row(&connection, &call)?, call);
        Ok(())
    })
    .unwrap();
    assert_eq!(
        read_row(&connection, &call).unwrap_err().code,
        WorkflowErrorCode::CorruptStore,
    );
    drop(connection);
    let reopened = WorkflowStore::open(&f.path).unwrap();
    let connection = reopened.connection.lock().unwrap();
    assert_eq!(
        read_row(&connection, &call).unwrap_err().code,
        WorkflowErrorCode::CorruptStore,
    );
}

#[test]
fn continuation_row_shape_funded_continue_commits_and_replays_without_resetting_spend() {
    for recovered in [false, true] {
        let (f, call, now) = funded_review(recovered);
        let source = session(&f);
        let result = work_funding_tests::continued(&call, now, 1);
        let completed = f
            .store
            .complete_adaptive_leadership_review_call(&f.leader, &result, now)
            .unwrap();
        let adopted = session(&f);
        assert_eq!(adopted.grant, source.grant);
        assert_eq!(adopted.model_calls, source.model_calls);
        assert_eq!(adopted.tool_calls, source.tool_calls);
        assert_eq!(adopted.version, source.version + 1);
        assert_eq!(
            adopted.active_work_funding(),
            call.grant.work_funding.as_deref(),
        );
        assert!(adopted.requires_fresh_observation());
        assert_eq!(
            adopted.continuation.as_ref().unwrap().authorizations.last(),
            result.continuation.as_ref(),
        );
        let before = rows(&f.store);
        let reopened = WorkflowStore::open(&f.path).unwrap();
        assert_eq!(
            reopened
                .complete_adaptive_leadership_review_call(&f.leader, &result, now + 1)
                .unwrap(),
            completed,
        );
        assert_eq!(rows(&reopened), before);
        assert_eq!(
            reopened
                .adaptive_session(call.grant.session_id, &call.grant.assignee_authority)
                .unwrap(),
            Some(adopted),
        );
    }
}
