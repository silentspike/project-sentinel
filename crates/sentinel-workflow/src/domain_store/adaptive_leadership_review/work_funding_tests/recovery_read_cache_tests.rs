use super::*;

fn historical_fingerprint(
    value: &crate::AdaptiveFundedReviewRecoveryVerificationSourceV1,
) -> String {
    canonical_sha256(
        "sentinel.workflow.recovery-read-cache-test.v1",
        &(
            &value.source,
            &value.project,
            &value.session,
            &value.funding,
            &value.calls,
        ),
    )
    .unwrap()
}

#[test]
fn recovery_receipt_and_history_reuse_exact_sql_facts_without_replaying_journals() {
    let (f, request, at) = recovery_fixture();
    let receipt = f
        .store
        .authorize_adaptive_funded_review_recovery(
            &funding_operator(&f), &request, &f.grant, &f.context, at, recovery_evidence,
        )
        .unwrap()
        .1;
    let store = WorkflowStore::open(&f.path).unwrap();
    let before = rows(&store);
    let read = || {
        let actual = store
            .adaptive_funded_review_recovery(&f.leader.tenant_id, &receipt.receipt_key)?
            .unwrap();
        let historical = store
            .adaptive_funded_review_recovery_historical_verification_source(
                &f.leader.tenant_id, &receipt.receipt_key,
            )?
            .unwrap();
        Ok::<_, WorkflowError>((actual, historical_fingerprint(&historical)))
    };
    let (first, cold_scopes) = validation_scope::with_completed_validations(read);
    let first = first.unwrap();
    assert_eq!(first.0, receipt);
    assert!(cold_scopes.iter().any(|scope| scope.get("adaptive-journal").copied().unwrap_or(0) > 0));
    let (second, warm_scopes) = validation_scope::with_completed_validations(read);
    assert_eq!(second.unwrap(), first);
    assert!(warm_scopes.is_empty(), "unchanged immutable facts must not replay history");
    assert_eq!(rows(&store), before);
}

#[test]
fn recovery_read_cache_invalidates_same_connection_and_external_history_damage() {
    for external in [false, true] {
        for historical in [false, true] {
            let (f, request, at) = recovery_fixture();
            let receipt = f.store.authorize_adaptive_funded_review_recovery(
                &funding_operator(&f), &request, &f.grant, &f.context, at, recovery_evidence,
            ).unwrap().1;
            let read = || -> Result<(), WorkflowError> {
                if historical {
                    f.store.adaptive_funded_review_recovery_historical_verification_source(
                        &f.leader.tenant_id, &receipt.receipt_key,
                    )?.unwrap();
                } else {
                    f.store.adaptive_funded_review_recovery(
                        &f.leader.tenant_id, &receipt.receipt_key,
                    )?.unwrap();
                }
                Ok(())
            };
            read().unwrap();
            let (warm, warm_scopes) = validation_scope::with_completed_validations(read);
            warm.unwrap();
            assert!(warm_scopes.is_empty());
            let damage = |connection: &Connection| {
                assert_eq!(connection.execute(
                    "DELETE FROM company_events WHERE tenant_id=?1 AND event_type='adaptive_funded_review_recovery_issued'",
                    params![f.leader.tenant_id.0],
                ).unwrap(), 1);
            };
            if external {
                damage(&Connection::open(&f.path).unwrap());
            } else {
                damage(&f.store.connection.lock().unwrap());
            }
            assert_eq!(read().unwrap_err().code, WorkflowErrorCode::CorruptStore,
                "external={external} historical={historical}");
        }
    }
}

#[test]
fn recovery_read_cache_separates_tenants_keys_and_refreshes_cached_absence() {
    let (f, request, at) = recovery_fixture();
    let key = request.source.receipt_key().unwrap();
    for _ in 0..2 {
        assert!(f.store.adaptive_funded_review_recovery(&f.leader.tenant_id, &key).unwrap().is_none());
        assert!(f.store.adaptive_funded_review_recovery_historical_verification_source(
            &f.leader.tenant_id, &key,
        ).unwrap().is_none());
    }
    let receipt = f.store.authorize_adaptive_funded_review_recovery(
        &funding_operator(&f), &request, &f.grant, &f.context, at, recovery_evidence,
    ).unwrap().1;
    assert_eq!(f.store.adaptive_funded_review_recovery(&f.leader.tenant_id, &key).unwrap(), Some(receipt));
    let foreign = TenantId("foreign-tenant".to_owned());
    let other_key = format!("funded-review-recovery-{}", "f".repeat(64));
    for (tenant, key) in [(&foreign, key.as_str()), (&f.leader.tenant_id, other_key.as_str())] {
        assert!(f.store.adaptive_funded_review_recovery(tenant, key).unwrap().is_none());
        assert!(f.store.adaptive_funded_review_recovery_historical_verification_source(tenant, key)
            .unwrap().is_none());
    }
}

#[test]
fn recovery_read_cache_does_not_publish_or_commit_caller_transaction_proofs() {
    let (f, request, at) = recovery_fixture();
    let receipt = f.store.authorize_adaptive_funded_review_recovery(
        &funding_operator(&f), &request, &f.grant, &f.context, at, recovery_evidence,
    ).unwrap().1;
    let read = || f.store.adaptive_funded_review_recovery_historical_verification_source(
        &f.leader.tenant_id, &receipt.receipt_key,
    );
    let expected = historical_fingerprint(&read().unwrap().unwrap());
    {
        let connection = f.store.connection.lock().unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        connection.execute("DELETE FROM company_events WHERE event_type='adaptive_funded_review_recovery_issued'", []).unwrap();
    }
    assert_eq!(read().unwrap_err().code, WorkflowErrorCode::CorruptStore);
    {
        let connection = f.store.connection.lock().unwrap();
        assert!(!connection.is_autocommit());
        connection.execute_batch("ROLLBACK").unwrap();
    }
    let (restored, scopes) = validation_scope::with_completed_validations(read);
    assert_eq!(historical_fingerprint(&restored.unwrap().unwrap()), expected);
    assert!(!scopes.is_empty(), "failed validation must not retain a proof");
    assert!(f.store.connection.lock().unwrap().is_autocommit());
}
