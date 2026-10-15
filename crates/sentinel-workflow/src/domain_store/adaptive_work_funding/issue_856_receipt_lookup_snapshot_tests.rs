use super::*;

fn receipt() -> AdaptiveWorkFundingReceiptV1 {
    review_payload_budget_tests::review_leaf()
        .grant
        .work_funding
        .unwrap()
        .receipt
}

fn persist(store: &WorkflowStore, receipt: &AdaptiveWorkFundingReceiptV1) {
    let mut connection = store.connection.lock().unwrap();
    let transaction = connection.transaction().unwrap();
    put_funding_receipt(&transaction, receipt).unwrap();
    transaction.commit().unwrap();
}

fn lookup(
    store: &WorkflowStore,
    receipt: &AdaptiveWorkFundingReceiptV1,
) -> Result<Option<AdaptiveWorkFundingReceiptV1>, WorkflowError> {
    let source = &receipt.request.source.resume_source;
    store.adaptive_work_funding(
        &source.tenant_id,
        source.session_id,
        receipt.request.operation_id,
    )
}

fn uncached(
    store: &WorkflowStore,
    tenant: &TenantId,
    session_id: Uuid,
    operation_id: Uuid,
) -> Result<Option<AdaptiveWorkFundingReceiptV1>, WorkflowError> {
    let connection = store.connection.lock().unwrap();
    read_funding_leaf(&connection, tenant, session_id, operation_id)
}

fn assert_warm(store: &WorkflowStore, receipt: &AdaptiveWorkFundingReceiptV1) {
    let (found, scopes) = validation_scope::with_completed_validations(|| lookup(store, receipt));
    assert_eq!(found.unwrap(), Some(receipt.clone()));
    assert!(
        scopes.is_empty(),
        "unchanged lookup must reuse the retained proof"
    );
    assert!(store.connection.lock().unwrap().is_autocommit());
}

#[test]
fn exact_receipt_lookup_retains_one_completed_arena_and_reuses_unchanged_evidence() {
    let directory = tempfile::tempdir().unwrap();
    let store = WorkflowStore::open(directory.path().join("receipt-reuse.sqlite")).unwrap();
    let receipt = receipt();
    persist(&store, &receipt);
    let source = &receipt.request.source.resume_source;
    let expected = uncached(
        &store,
        &source.tenant_id,
        source.session_id,
        receipt.request.operation_id,
    )
    .unwrap();
    let (found, scopes) = validation_scope::with_completed_validations(|| lookup(&store, &receipt));
    assert_eq!(found.unwrap(), expected);
    assert_eq!(
        scopes.len(),
        1,
        "all nested validators must share one arena"
    );
    for domain in ["entity", "entity-shape", "event-shape", "leaf-event"] {
        assert_eq!(scopes[0].get(domain), Some(&1), "{domain}");
    }
    for _ in 0..3 {
        assert_warm(&store, &receipt);
    }
    assert_eq!(validation_scope::validations("entity"), 0);
}

#[test]
fn exact_receipt_lookup_keeps_tenant_session_operation_and_legacy_errors_distinct() {
    let directory = tempfile::tempdir().unwrap();
    let store = WorkflowStore::open(directory.path().join("receipt-keys.sqlite")).unwrap();
    let receipt = receipt();
    persist(&store, &receipt);
    let source = &receipt.request.source.resume_source;
    let foreign = TenantId::parse("receipt-foreign-tenant").unwrap();
    let invalid_tenant = TenantId(String::new());
    let queries = [
        (
            &source.tenant_id,
            source.session_id,
            receipt.request.operation_id,
        ),
        (&foreign, source.session_id, receipt.request.operation_id),
        (
            &source.tenant_id,
            Uuid::from_u128(9301),
            receipt.request.operation_id,
        ),
        (&source.tenant_id, source.session_id, Uuid::from_u128(9302)),
        (&source.tenant_id, Uuid::nil(), receipt.request.operation_id),
        (&source.tenant_id, source.session_id, Uuid::nil()),
        (
            &invalid_tenant,
            source.session_id,
            receipt.request.operation_id,
        ),
    ];
    for _ in 0..2 {
        for (tenant, session_id, operation_id) in &queries {
            assert_eq!(lookup(&store, &receipt).unwrap(), Some(receipt.clone()));
            assert_eq!(
                store.adaptive_work_funding(tenant, *session_id, *operation_id),
                uncached(&store, tenant, *session_id, *operation_id),
            );
        }
    }
    assert_eq!(
        store
            .adaptive_work_funding(
                &source.tenant_id,
                Uuid::from_u128(9301),
                receipt.request.operation_id,
            )
            .unwrap_err()
            .code,
        WorkflowErrorCode::CorruptStore,
        "the issuance event must still veto a wrong-session lookup",
    );
    assert_eq!(
        store
            .adaptive_work_funding(&source.tenant_id, source.session_id, Uuid::from_u128(9302))
            .unwrap(),
        None,
    );
    assert!(store.connection.lock().unwrap().is_autocommit());
}

#[test]
fn exact_receipt_lookup_invalidates_cached_absence_on_local_and_external_issuance() {
    for external in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("receipt-absence.sqlite");
        let store = WorkflowStore::open(&path).unwrap();
        let receipt = receipt();
        for expected_scopes in [1, 0] {
            let (found, scopes) =
                validation_scope::with_completed_validations(|| lookup(&store, &receipt));
            assert_eq!(found.unwrap(), None);
            assert_eq!(scopes.len(), expected_scopes);
        }
        if external {
            persist(&WorkflowStore::open(&path).unwrap(), &receipt);
        } else {
            persist(&store, &receipt);
        }
        let (found, scopes) =
            validation_scope::with_completed_validations(|| lookup(&store, &receipt));
        assert_eq!(found.unwrap(), Some(receipt.clone()));
        assert_eq!(scopes.len(), 1, "issuance must invalidate cached absence");
        assert_warm(&store, &receipt);
    }
}

#[test]
fn exact_receipt_lookup_invalidates_local_and_external_corruption_without_caching_errors() {
    for external in [false, true] {
        for damage in [
            "UPDATE company_entities SET payload_digest='invalid'
             WHERE entity_kind='adaptive_work_funding'",
            "UPDATE company_events SET payload_digest='invalid'
             WHERE event_type='adaptive_work_funding_issued'",
            "DELETE FROM company_events WHERE event_type='adaptive_work_funding_issued'",
            "DELETE FROM company_entities WHERE entity_kind='adaptive_work_funding'",
            "UPDATE company_events SET operation_id='00000000-0000-0000-0000-000000009399'
             WHERE event_type='adaptive_work_funding_issued'",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("receipt-corruption.sqlite");
            let store = WorkflowStore::open(&path).unwrap();
            let receipt = receipt();
            persist(&store, &receipt);
            assert_eq!(lookup(&store, &receipt).unwrap(), Some(receipt.clone()));
            assert_warm(&store, &receipt);
            let external_store = external.then(|| WorkflowStore::open(&path).unwrap());
            let writer = external_store.as_ref().unwrap_or(&store);
            // The local case uses exactly the connection that established the
            // proof, without a public write helper announcing the mutation.
            assert_eq!(
                writer
                    .connection
                    .lock()
                    .unwrap()
                    .execute(damage, [])
                    .unwrap(),
                1,
            );
            for _ in 0..2 {
                let source = &receipt.request.source.resume_source;
                let expected = uncached(
                    &store,
                    &source.tenant_id,
                    source.session_id,
                    receipt.request.operation_id,
                );
                assert_eq!(lookup(&store, &receipt), expected);
                assert_eq!(expected.unwrap_err().code, WorkflowErrorCode::CorruptStore);
            }
            {
                let mut connection = writer.connection.lock().unwrap();
                let transaction = connection.transaction().unwrap();
                transaction
                    .execute("DELETE FROM company_events", [])
                    .unwrap();
                transaction
                    .execute("DELETE FROM company_entities", [])
                    .unwrap();
                put_funding_receipt(&transaction, &receipt).unwrap();
                transaction.commit().unwrap();
            }
            let (found, scopes) =
                validation_scope::with_completed_validations(|| lookup(&store, &receipt));
            assert_eq!(found.unwrap(), Some(receipt.clone()));
            assert_eq!(scopes.len(), 1, "failed proofs must not survive repair");
            assert_warm(&store, &receipt);
            assert!(store.connection.lock().unwrap().is_autocommit());
        }
    }
}

#[test]
fn exact_receipt_lookup_rejects_valid_receipts_relocated_under_wrong_session_or_operation() {
    for wrong_session in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let store = WorkflowStore::open(directory.path().join("receipt-binding.sqlite")).unwrap();
        let receipt = receipt();
        persist(&store, &receipt);
        assert_eq!(lookup(&store, &receipt).unwrap(), Some(receipt.clone()));
        let source = &receipt.request.source.resume_source;
        let session_id = if wrong_session {
            Uuid::from_u128(9303)
        } else {
            source.session_id
        };
        let operation_id = if wrong_session {
            receipt.request.operation_id
        } else {
            Uuid::from_u128(9304)
        };
        let wrong_key =
            adaptive_work_funding_id(&source.tenant_id, session_id, operation_id).unwrap();
        assert_eq!(
            store
                .connection
                .lock()
                .unwrap()
                .execute(
                    "UPDATE company_entities SET entity_id=?1 WHERE entity_kind=?2 AND entity_id=?3",
                    params![wrong_key, FUNDING_KIND, receipt.funding_id],
                )
                .unwrap(),
            1,
        );
        for _ in 0..2 {
            let expected = uncached(&store, &source.tenant_id, session_id, operation_id);
            assert_eq!(
                store.adaptive_work_funding(&source.tenant_id, session_id, operation_id),
                expected,
            );
            assert_eq!(expected.unwrap_err().code, WorkflowErrorCode::CorruptStore);
        }
    }
}

#[test]
fn exact_receipt_lookup_bypasses_retained_proofs_inside_caller_transaction_and_after_rollback() {
    let directory = tempfile::tempdir().unwrap();
    let store = WorkflowStore::open(directory.path().join("receipt-rollback.sqlite")).unwrap();
    let receipt = receipt();
    persist(&store, &receipt);
    assert_eq!(lookup(&store, &receipt).unwrap(), Some(receipt.clone()));
    assert_warm(&store, &receipt);
    store
        .connection
        .lock()
        .unwrap()
        .execute_batch("BEGIN DEFERRED")
        .unwrap();
    for _ in 0..2 {
        let (found, scopes) =
            validation_scope::with_completed_validations(|| lookup(&store, &receipt));
        assert_eq!(found.unwrap(), Some(receipt.clone()));
        assert_eq!(
            scopes.len(),
            1,
            "caller-owned transactions must bypass retention"
        );
        assert!(!store.connection.lock().unwrap().is_autocommit());
    }
    store
        .connection
        .lock()
        .unwrap()
        .execute(
            "UPDATE company_entities SET payload_digest='invalid' WHERE entity_kind=?1",
            [FUNDING_KIND],
        )
        .unwrap();
    assert_eq!(
        lookup(&store, &receipt).unwrap_err().code,
        WorkflowErrorCode::CorruptStore,
    );
    {
        let connection = store.connection.lock().unwrap();
        assert!(
            !connection.is_autocommit(),
            "the caller still owns its transaction"
        );
        connection.execute_batch("ROLLBACK").unwrap();
    }
    let (found, scopes) = validation_scope::with_completed_validations(|| lookup(&store, &receipt));
    assert_eq!(found.unwrap(), Some(receipt.clone()));
    assert_eq!(
        scopes.len(),
        1,
        "rollback must rebuild the historical proof"
    );
    assert_warm(&store, &receipt);
}
