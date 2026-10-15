use super::*;

fn receipt(tenant: &TenantId, session_id: Uuid) -> AdaptiveResumePolicyReceiptV1 {
    let mut authority = crate::adaptive::continuation_tests::grant().authority;
    authority.tenant_id = tenant.clone();
    let issuer_principal = AuthenticatedCompanyPrincipalV1 {
        schema_version: 1,
        tenant_id: tenant.clone(),
        principal_id: "policy-read-operator".into(),
        kind: CompanyPrincipalKindV1::Operator,
        role: CompanyRoleV1::ProjectManager,
        customer_id: None,
        agent_id: None,
        authority_generation: 1,
        authority_digest: "a".repeat(64),
    };
    let value = AdaptiveResumePolicyReceiptV1 {
        schema_version: 1,
        request: AdaptiveResumePolicyRequestV1 {
            schema_version: 1,
            operation_id: Uuid::new_v4(),
            source: AdaptiveResumeSourceV1 {
                tenant_id: tenant.clone(),
                project_id: authority.project_id.clone(),
                work_item_id: authority.work_item_id.clone(),
                session_id,
                expected_project_version: 1,
                expected_session_version: 1,
                project_payload_digest: "b".repeat(64),
                root_entry_digest: "c".repeat(64),
                head_entry_digest: "d".repeat(64),
                continuation_history_digest: "e".repeat(64),
                review_history_digest: "f".repeat(64),
                assignee_authority: authority,
                base_model_calls: 0,
                base_tool_calls: 0,
                base_review_count: 0,
                base_window_count: 0,
                subject: AdaptiveResumeSubjectV1::ReadyForModel {
                    active_allowance_digest: "a".repeat(64),
                },
            },
            limits: AdaptiveResumePolicyLimitsV1 {
                total_review_ceiling: 2,
                total_window_ceiling: 2,
                max_window_ms: 300_000,
                max_call_duration_ms: 120_000,
                dispatch_margin_ms: 1_000,
                expires_at_unix_ms: 600_000,
            },
            reason_ref: "issue-856.policy-read".into(),
        },
        policy_id: adaptive_resume_policy_id(tenant, session_id).unwrap(),
        issuer_principal,
        issued_at_unix_ms: 1,
    };
    value.validate().unwrap();
    value
}

// These are sealed-leaf read fixtures, not evidence of fresh issuance authorization.
fn persist(connection: &mut Connection, value: &AdaptiveResumePolicyReceiptV1) {
    let tx = connection.transaction().unwrap();
    let source = &value.request.source;
    put_entity(
        &tx,
        &source.tenant_id,
        POLICY_KIND,
        &value.policy_id,
        1,
        value,
    )
    .unwrap();
    append_event(
        &tx,
        &value.issuer_principal,
        value.request.operation_id,
        &value.request.canonical_digest().unwrap(),
        Some(&source.project_id),
        POLICY_EVENT,
        value,
        value.issued_at_unix_ms,
    )
    .unwrap();
    tx.commit().unwrap();
}

fn lookup(
    store: &WorkflowStore,
    value: &AdaptiveResumePolicyReceiptV1,
) -> Result<Option<AdaptiveResumePolicyReceiptV1>, WorkflowError> {
    store.adaptive_resume_policy(
        &value.request.source.tenant_id,
        value.request.source.session_id,
    )
}

#[test]
fn unchanged_policy_lookup_reuses_completed_validation() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkflowStore::open(temp.path().join("policy.sqlite")).unwrap();
    let value = receipt(&TenantId::parse("tenant-policy").unwrap(), Uuid::new_v4());
    persist(&mut store.connection.lock().unwrap(), &value);

    let (first, scopes) = validation_scope::with_completed_validations(|| lookup(&store, &value));
    assert_eq!(first.unwrap(), Some(value.clone()));
    assert_eq!(scopes.len(), 1);
    assert_eq!(scopes[0].get("entity"), Some(&1));
    assert_eq!(scopes[0].get("leaf-event"), Some(&1));

    let (second, scopes) = validation_scope::with_completed_validations(|| lookup(&store, &value));
    assert_eq!(second.unwrap(), Some(value));
    assert!(
        scopes.is_empty(),
        "unchanged lookup must reuse the validated read"
    );
    assert!(store.connection.lock().unwrap().is_autocommit());
}

#[test]
fn warm_policy_lookup_rejects_local_and_external_corruption() {
    let mutations = [
        "UPDATE company_entities SET payload_digest='corrupt'",
        "UPDATE company_entities SET version=2",
        "UPDATE company_entities SET entity_id='wrong-policy'",
        "DELETE FROM company_entities",
        "DELETE FROM company_events",
        "UPDATE company_events SET project_id='wrong-project'",
        "UPDATE company_events SET principal_id='wrong-issuer'",
        "UPDATE company_events SET authority_binding_digest='corrupt'",
        "UPDATE company_events SET operation_digest='corrupt'",
        "UPDATE company_events SET payload_digest='corrupt'",
        "UPDATE company_events SET created_at_ms=2",
    ];
    for external in [false, true] {
        for mutation in mutations {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("policy.sqlite");
            let store = WorkflowStore::open(&path).unwrap();
            let value = receipt(&TenantId::parse("tenant-policy").unwrap(), Uuid::new_v4());
            persist(&mut store.connection.lock().unwrap(), &value);
            assert_eq!(lookup(&store, &value).unwrap(), Some(value.clone()));
            if external {
                Connection::open(&path)
                    .unwrap()
                    .execute(mutation, [])
                    .unwrap();
            } else {
                store
                    .connection
                    .lock()
                    .unwrap()
                    .execute(mutation, [])
                    .unwrap();
            }
            for _ in 0..2 {
                assert_eq!(
                    lookup(&store, &value).unwrap_err().code,
                    WorkflowErrorCode::CorruptStore,
                    "external={external}, mutation={mutation}"
                );
                assert!(store.connection.lock().unwrap().is_autocommit());
            }
        }
    }
}

#[test]
fn missing_policy_lookup_invalidates_after_local_and_external_insertion() {
    for external in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("policy.sqlite");
        let store = WorkflowStore::open(&path).unwrap();
        let value = receipt(&TenantId::parse("tenant-policy").unwrap(), Uuid::new_v4());
        assert_eq!(lookup(&store, &value).unwrap(), None);
        assert_eq!(lookup(&store, &value).unwrap(), None);
        if external {
            persist(&mut Connection::open(&path).unwrap(), &value);
        } else {
            persist(&mut store.connection.lock().unwrap(), &value);
        }
        let (result, scopes) =
            validation_scope::with_completed_validations(|| lookup(&store, &value));
        assert_eq!(result.unwrap(), Some(value));
        assert_eq!(
            scopes.len(),
            1,
            "insertion must invalidate the absent proof"
        );
    }
}

#[test]
fn missing_policy_lookup_rejects_new_local_and_external_orphan_event() {
    for external in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("policy.sqlite");
        let store = WorkflowStore::open(&path).unwrap();
        let value = receipt(&TenantId::parse("tenant-policy").unwrap(), Uuid::new_v4());
        assert_eq!(lookup(&store, &value).unwrap(), None);
        assert_eq!(lookup(&store, &value).unwrap(), None);
        let write_orphan = |connection: &mut Connection| {
            let tx = connection.transaction().unwrap();
            append_event(
                &tx,
                &value.issuer_principal,
                value.request.operation_id,
                &value.request.canonical_digest().unwrap(),
                Some(&value.request.source.project_id),
                POLICY_EVENT,
                &value,
                value.issued_at_unix_ms,
            )
            .unwrap();
            tx.commit().unwrap();
        };
        if external {
            write_orphan(&mut Connection::open(&path).unwrap());
        } else {
            write_orphan(&mut store.connection.lock().unwrap());
        }
        for _ in 0..2 {
            assert_eq!(
                lookup(&store, &value).unwrap_err().code,
                WorkflowErrorCode::CorruptStore
            );
            assert!(store.connection.lock().unwrap().is_autocommit());
        }
    }
}

#[test]
fn policy_lookup_key_separates_tenants_and_sessions() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkflowStore::open(temp.path().join("policy.sqlite")).unwrap();
    let tenant = TenantId::parse("tenant-policy").unwrap();
    let foreign = TenantId::parse("tenant-foreign").unwrap();
    let session_id = Uuid::new_v4();
    let values = [
        receipt(&tenant, session_id),
        receipt(&foreign, session_id),
        receipt(&tenant, Uuid::new_v4()),
    ];
    for value in &values {
        persist(&mut store.connection.lock().unwrap(), value);
    }
    let absent = receipt(&foreign, values[2].request.source.session_id);
    for round in 0..2 {
        for value in &values {
            let (result, scopes) =
                validation_scope::with_completed_validations(|| lookup(&store, value));
            assert_eq!(result.unwrap(), Some(value.clone()));
            assert_eq!(scopes.is_empty(), round == 1);
        }
        assert_eq!(lookup(&store, &absent).unwrap(), None);
    }
}

#[test]
fn policy_lookup_keeps_invalid_identity_errors_and_missing_rows() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkflowStore::open(temp.path().join("policy.sqlite")).unwrap();
    let tenant = TenantId::parse("tenant-policy").unwrap();
    let absent = receipt(&TenantId::parse("tenant-absent").unwrap(), Uuid::new_v4());
    assert_eq!(lookup(&store, &absent).unwrap(), None);
    for damaged in [false, true] {
        if damaged {
            store
                .connection
                .lock()
                .unwrap()
                .execute("DROP TABLE company_events", [])
                .unwrap();
        }
        for (tenant, session) in [
            (TenantId(String::new()), Uuid::new_v4()),
            (tenant.clone(), Uuid::nil()),
        ] {
            let expected = {
                let connection = store.connection.lock().unwrap();
                read_resume_policy_leaf(&connection, &tenant, session).unwrap_err()
            };
            assert_eq!(
                store.adaptive_resume_policy(&tenant, session).unwrap_err(),
                expected
            );
            assert!(store.connection.lock().unwrap().is_autocommit());
        }
    }
}

#[test]
fn policy_lookup_bypasses_reuse_and_preserves_caller_transaction() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkflowStore::open(temp.path().join("policy.sqlite")).unwrap();
    let value = receipt(&TenantId::parse("tenant-policy").unwrap(), Uuid::new_v4());
    persist(&mut store.connection.lock().unwrap(), &value);
    assert_eq!(lookup(&store, &value).unwrap(), Some(value.clone()));
    store
        .connection
        .lock()
        .unwrap()
        .execute_batch("BEGIN")
        .unwrap();
    for _ in 0..2 {
        let (result, scopes) =
            validation_scope::with_completed_validations(|| lookup(&store, &value));
        assert_eq!(result.unwrap(), Some(value.clone()));
        assert_eq!(
            scopes.len(),
            1,
            "caller transaction must not reuse a retained read"
        );
        assert!(!store.connection.lock().unwrap().is_autocommit());
    }
    store
        .connection
        .lock()
        .unwrap()
        .execute("UPDATE company_events SET principal_id='wrong-issuer'", [])
        .unwrap();
    assert_eq!(
        lookup(&store, &value).unwrap_err().code,
        WorkflowErrorCode::CorruptStore
    );
    {
        let connection = store.connection.lock().unwrap();
        assert!(!connection.is_autocommit());
        let issuer: String = connection
            .query_row("SELECT principal_id FROM company_events", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(issuer, "wrong-issuer");
        connection.execute_batch("ROLLBACK").unwrap();
    }
    assert_eq!(lookup(&store, &value).unwrap(), Some(value));
    assert!(store.connection.lock().unwrap().is_autocommit());
}
