use super::*;
use crate::domain_store::tests::accepted_project_fixture;

#[test]
fn customer_batch_matches_existing_getters_and_preserves_rows() {
    let (_root, _path, store, principal, project) = accepted_project_fixture();
    let tenant = &principal.tenant_id;
    let requests = store
        .company_customer_requests(tenant, "customer-a")
        .unwrap();
    let proposals = requests
        .iter()
        .flat_map(|request| &request.proposal_ids)
        .map(|id| store.company_proposal(tenant, id).unwrap().unwrap())
        .collect::<Vec<_>>();
    let projects = store
        .company_customer_projects(tenant, "customer-a")
        .unwrap();
    let changes: i64 = store
        .connection
        .lock()
        .unwrap()
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap();
    let (actual, scopes) = validation_scope::with_completed_validations(|| {
        store.customer_overview_read_snapshot(tenant, "customer-a", None)
    });
    let actual = actual.unwrap();
    assert_eq!(actual.requests, requests);
    assert_eq!(actual.proposals, proposals);
    assert_eq!(
        actual
            .projects
            .iter()
            .map(|value| value.project.clone())
            .collect::<Vec<_>>(),
        projects
    );
    assert_eq!(actual.projects.len(), 1);
    assert_eq!(
        actual.projects[0].agreement,
        store
            .company_agreement(tenant, &project.agreement_id)
            .unwrap()
            .unwrap()
    );
    // Multiple independent arenas, not one arena containing the whole customer.
    assert!(scopes.len() >= 4);
    let (repeat, repeated_scopes) = validation_scope::with_completed_validations(|| {
        store.customer_overview_read_snapshot(tenant, "customer-a", None)
    });
    assert_eq!(
        serde_json::to_vec(&repeat.unwrap()).unwrap(),
        serde_json::to_vec(&actual).unwrap()
    );
    assert!(
        repeated_scopes.is_empty(),
        "an exact warm overview reuses its collection proof without entity replay"
    );
    assert_eq!(
        store
            .connection
            .lock()
            .unwrap()
            .query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        changes
    );
    assert!(store.company_projects_match_snapshot(&projects).unwrap());
    let mut stale = project;
    stale.version += 1;
    assert!(!store.company_projects_match_snapshot(&[stale]).unwrap());
    assert!(store.connection.lock().unwrap().is_autocommit());
}

#[test]
fn customer_batch_rejects_selected_foreign_request_before_proposals() {
    let (_root, _path, store, principal, _project) = accepted_project_fixture();
    store
        .connection
        .lock()
        .unwrap()
        .execute(
            "DELETE FROM company_entities WHERE entity_kind='proposal'",
            [],
        )
        .unwrap();
    let error = store
        .customer_overview_read_snapshot(&principal.tenant_id, "customer-b", Some("request-a"))
        .unwrap_err();
    assert_eq!(error.code, WorkflowErrorCode::AuthorityConflict);
    assert_eq!(error.message, "customer request is foreign");
    assert!(store.connection.lock().unwrap().is_autocommit());
}

#[test]
fn customer_batch_preserves_proposal_before_project_error_order() {
    let (_root, _path, store, principal, _project) = accepted_project_fixture();
    store
        .connection
        .lock()
        .unwrap()
        .execute_batch(
            "DELETE FROM company_entities WHERE entity_kind='proposal';
         UPDATE company_entities SET payload_digest='broken' WHERE entity_kind='project'",
        )
        .unwrap();
    let (result, scopes) = validation_scope::with_completed_validations(|| {
        store.customer_overview_read_snapshot(&principal.tenant_id, "customer-a", None)
    });
    assert_eq!(result.unwrap_err().code, WorkflowErrorCode::CorruptStore);
    // Request and absent proposal finish; no project validation is attempted.
    assert_eq!(scopes.len(), 2);
}

#[test]
fn customer_batch_validates_projects_even_when_request_filter_is_empty() {
    let (_root, _path, store, principal, _project) = accepted_project_fixture();
    store
        .connection
        .lock()
        .unwrap()
        .execute(
            "UPDATE company_entities SET payload_digest='broken' WHERE entity_kind='project'",
            [],
        )
        .unwrap();
    assert_eq!(
        store
            .customer_overview_read_snapshot(
                &principal.tenant_id,
                "customer-a",
                Some("missing-request"),
            )
            .unwrap_err()
            .code,
        WorkflowErrorCode::CorruptStore
    );
    let other = store
        .customer_overview_read_snapshot(&principal.tenant_id, "customer-b", None)
        .unwrap();
    assert!(other.requests.is_empty() && other.projects.is_empty());
}

#[test]
fn customer_final_batch_rejects_external_corruption_after_warm_read() {
    let (_root, path, store, principal, project) = accepted_project_fixture();
    assert!(store
        .company_projects_match_snapshot(std::slice::from_ref(&project))
        .unwrap());
    let external = Connection::open(path).unwrap();
    external
        .execute(
            "UPDATE company_entities SET payload_digest='broken' WHERE entity_kind='project'",
            [],
        )
        .unwrap();
    assert_eq!(
        store
            .company_projects_match_snapshot(&[project])
            .unwrap_err()
            .code,
        WorkflowErrorCode::CorruptStore
    );
    assert_eq!(
        store
            .customer_overview_read_snapshot(&principal.tenant_id, "customer-a", None)
            .unwrap_err()
            .code,
        WorkflowErrorCode::CorruptStore
    );
    assert!(store.connection.lock().unwrap().is_autocommit());
}

#[test]
fn customer_batch_preserves_outer_transaction_and_bounds() {
    let (_root, _path, store, principal, project) = accepted_project_fixture();
    store
        .connection
        .lock()
        .unwrap()
        .execute_batch("BEGIN")
        .unwrap();
    store
        .customer_overview_read_snapshot(&principal.tenant_id, "customer-a", None)
        .unwrap();
    assert!(store
        .company_projects_match_snapshot(std::slice::from_ref(&project))
        .unwrap());
    assert!(!store.connection.lock().unwrap().is_autocommit());
    store
        .connection
        .lock()
        .unwrap()
        .execute_batch("ROLLBACK")
        .unwrap();
    assert_eq!(
        store
            .company_projects_match_snapshot(&vec![project; 129])
            .unwrap_err()
            .code,
        WorkflowErrorCode::InvalidInput
    );
}

#[test]
fn empty_project_match_avoids_unrelated_database_validation() {
    let (_root, _path, store, _principal, _project) = accepted_project_fixture();
    store
        .connection
        .lock()
        .unwrap()
        .execute_batch("DROP TABLE company_entities")
        .unwrap();
    assert!(store.company_projects_match_snapshot(&[]).unwrap());
}

#[test]
fn provider_batch_healthy_project_is_reused_and_project_corruption_is_deferred() {
    let (_root, path, store, principal, project) = accepted_project_fixture();
    let mut grant = crate::adaptive::continuation_tests::grant();
    grant.authority.tenant_id = principal.tenant_id;
    grant.authority.project_id = project.project_id;
    let initial = store
        .begin_adaptive_session(&grant, &grant.authority, grant.created_at_ms)
        .unwrap()
        .1;
    let model = crate::adaptive::continuation_tests::effect(909);
    let session = store
        .advance_adaptive_session(
            grant.session_id,
            initial.version,
            Uuid::from_u128(910),
            &crate::AdaptiveTransitionV1::ClaimModel {
                effect: model.clone(),
                previous_observation_digest: None,
            },
            &grant.authority,
            grant.created_at_ms + 1,
        )
        .unwrap()
        .1;
    let read = || {
        store.adaptive_model_preparation_read_snapshot(
            grant.session_id,
            initial.version,
            model.id,
            &grant.authority,
        )
    };
    let view = read().unwrap().unwrap();
    assert_eq!(view.session(), &session);
    assert!(view.project().is_ok());
    assert!(view.working_memory_source().is_ok());
    let (repeat, scopes) = validation_scope::with_completed_validations(read);
    assert!(repeat.unwrap().unwrap().working_memory_source().is_ok());
    assert!(
        scopes.is_empty(),
        "healthy exact typed hit does not rerun validators"
    );
    let external = Connection::open(path).unwrap();
    external
        .execute(
            "UPDATE company_entities SET payload_digest='broken' WHERE entity_kind='project'",
            [],
        )
        .unwrap();
    let view = read().unwrap().unwrap();
    assert_eq!(view.session(), &session);
    assert_eq!(
        view.project().unwrap_err().code,
        WorkflowErrorCode::CorruptStore
    );
    assert!(view.working_memory_source().is_ok());
    assert_eq!(view.recovery_feedback().unwrap(), None);
    assert!(store.connection.lock().unwrap().is_autocommit());
}

#[test]
fn customer_final_batch_preserves_index_integrity_on_a_typed_hit() {
    use std::io::{Seek, SeekFrom, Write};
    let (_root, path, store, _principal, project) = accepted_project_fixture();
    store.connection.lock().unwrap().execute_batch(
        "CREATE INDEX customer_batch_payload_index ON company_entities(payload_digest); PRAGMA wal_checkpoint(TRUNCATE)",
    ).unwrap();
    let read = || store.company_projects_match_snapshot(std::slice::from_ref(&project));
    assert!(read().unwrap());
    let connection = store.connection.lock().unwrap();
    let size: u32 = connection
        .query_row("PRAGMA page_size", [], |row| row.get(0))
        .unwrap();
    let page: u32 = connection
        .query_row(
            "SELECT rootpage FROM sqlite_schema WHERE name='customer_batch_payload_index'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    connection.execute_batch("PRAGMA shrink_memory").unwrap();
    drop(connection);
    let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(u64::from(page - 1) * u64::from(size)))
        .unwrap();
    file.write_all(&vec![0; size as usize]).unwrap();
    file.sync_all().unwrap();
    assert_eq!(read().unwrap_err().code, WorkflowErrorCode::CorruptStore);
}

#[test]
fn customer_overview_cache_separates_exact_tenant_customer_and_request_keys() {
    let (_root, _path, store, principal, _project) = accepted_project_fixture();
    let read = |tenant: &TenantId, customer: &str, request: Option<&str>| {
        validation_scope::with_completed_validations(|| {
            store.customer_overview_read_snapshot(tenant, customer, request)
        })
    };
    let (all, first_scopes) = read(&principal.tenant_id, "customer-a", None);
    let all = all.unwrap();
    assert!(!first_scopes.is_empty());
    let (selected, selected_scopes) = read(&principal.tenant_id, "customer-a", Some("request-a"));
    assert_eq!(
        serde_json::to_vec(&selected.unwrap()).unwrap(),
        serde_json::to_vec(&all).unwrap()
    );
    assert!(
        !selected_scopes.is_empty(),
        "None and Some are distinct keys"
    );
    let (missing, missing_scopes) =
        read(&principal.tenant_id, "customer-a", Some("missing-request"));
    let missing = missing.unwrap();
    assert!(missing.requests.is_empty() && missing.proposals.is_empty());
    assert_eq!(missing.projects.len(), all.projects.len());
    assert!(!missing_scopes.is_empty());
    let (other_customer, _) = read(&principal.tenant_id, "customer-b", None);
    let other_customer = other_customer.unwrap();
    assert!(other_customer.requests.is_empty() && other_customer.projects.is_empty());
    let other_tenant = TenantId::parse("tenant-other").unwrap();
    let (other, _) = read(&other_tenant, "customer-a", Some("request-a"));
    let other = other.unwrap();
    assert!(other.requests.is_empty() && other.proposals.is_empty() && other.projects.is_empty());
    let (foreign, _) = read(&principal.tenant_id, "customer-b", Some("request-a"));
    assert_eq!(
        foreign.unwrap_err().code,
        WorkflowErrorCode::AuthorityConflict
    );
    // A rejected key cannot replace a successful scope's owned facts.
    read(&principal.tenant_id, "customer-a", None).0.unwrap();
    let (repeat, repeat_scopes) = read(&principal.tenant_id, "customer-a", None);
    assert_eq!(
        serde_json::to_vec(&repeat.unwrap()).unwrap(),
        serde_json::to_vec(&all).unwrap()
    );
    assert!(repeat_scopes.is_empty());
}

#[test]
fn customer_overview_cache_rechecks_durable_external_commit_and_direct_damage() {
    let (_root, path, store, principal, _project) = accepted_project_fixture();
    let read = || {
        validation_scope::with_completed_validations(|| {
            store.customer_overview_read_snapshot(&principal.tenant_id, "customer-a", None)
        })
    };
    let initial = read().0.unwrap();
    assert!(read().1.is_empty());
    let external = WorkflowStore::open(&path).unwrap();
    external
        .apply_company_command(
            &principal,
            Uuid::from_u128(810),
            &CompanyWorkflowCommandV1::SubmitCustomerRequest {
                summary_ref: "new customer request".into(),
                desired_outcome: "bounded follow-up".into(),
                constraints: Vec::new(),
            },
            3,
        )
        .unwrap();
    let (changed, scopes) = read();
    let changed = changed.unwrap();
    assert_eq!(changed.requests.len(), initial.requests.len() + 1);
    assert!(!scopes.is_empty());
    assert!(read().1.is_empty());
    let connection = Connection::open(&path).unwrap();
    let original_digest: String = connection
        .query_row(
            "SELECT payload_digest FROM company_entities WHERE entity_kind='project'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    connection
        .execute(
            "UPDATE company_entities SET payload_digest='broken' WHERE entity_kind='project'",
            [],
        )
        .unwrap();
    for _ in 0..2 {
        let (error, scopes) = read();
        assert_eq!(error.unwrap_err().code, WorkflowErrorCode::CorruptStore);
        assert!(
            !scopes.is_empty(),
            "failed reads never retain a result proof"
        );
    }
    connection
        .execute(
            "UPDATE company_entities SET payload_digest=?1 WHERE entity_kind='project'",
            [original_digest],
        )
        .unwrap();
    let (repaired, scopes) = read();
    assert_eq!(
        serde_json::to_vec(&repaired.unwrap()).unwrap(),
        serde_json::to_vec(&changed).unwrap()
    );
    assert!(!scopes.is_empty());
    assert!(read().1.is_empty());
    assert!(store.connection.lock().unwrap().is_autocommit());
}

#[test]
fn customer_overview_cache_outer_transaction_and_rollback_never_reuse_dirty_proof() {
    let (_root, _path, store, principal, _project) = accepted_project_fixture();
    let read = || {
        validation_scope::with_completed_validations(|| {
            store.customer_overview_read_snapshot(&principal.tenant_id, "customer-a", None)
        })
    };
    let initial = read().0.unwrap();
    assert!(read().1.is_empty());
    store
        .connection
        .lock()
        .unwrap()
        .execute_batch("BEGIN")
        .unwrap();
    for _ in 0..2 {
        let (value, scopes) = read();
        assert_eq!(
            serde_json::to_vec(&value.unwrap()).unwrap(),
            serde_json::to_vec(&initial).unwrap()
        );
        assert!(
            !scopes.is_empty(),
            "caller-owned snapshots always replay independently"
        );
        assert!(!store.connection.lock().unwrap().is_autocommit());
    }
    store
        .connection
        .lock()
        .unwrap()
        .execute(
            "UPDATE company_entities SET payload_digest='broken' WHERE entity_kind='project'",
            [],
        )
        .unwrap();
    assert_eq!(read().0.unwrap_err().code, WorkflowErrorCode::CorruptStore);
    assert!(!store.connection.lock().unwrap().is_autocommit());
    store
        .connection
        .lock()
        .unwrap()
        .execute_batch("ROLLBACK")
        .unwrap();
    let (restored, scopes) = read();
    assert_eq!(
        serde_json::to_vec(&restored.unwrap()).unwrap(),
        serde_json::to_vec(&initial).unwrap()
    );
    assert!(!scopes.is_empty());
    assert!(read().1.is_empty());
    assert!(store.connection.lock().unwrap().is_autocommit());
}

#[test]
fn customer_overview_cache_validates_later_owned_project_before_request_filter() {
    let (_root, _path, store, principal, mut inconsistent) = accepted_project_fixture();
    let read = || {
        store.customer_overview_read_snapshot(&principal.tenant_id, "customer-a", Some("request-a"))
    };
    let original = read().unwrap();
    assert_eq!(original.projects.len(), 1);
    inconsistent.project_id = ProjectId::parse("zz-inconsistent-owned-project").unwrap();
    inconsistent.agreement_digest = "b".repeat(64);
    {
        let mut connection = store.connection.lock().unwrap();
        let transaction = connection.transaction().unwrap();
        // Correct entity digest, but an incorrect binding to the owned agreement.
        put_entity(
            &transaction,
            &principal.tenant_id,
            "project",
            &inconsistent.project_id.0,
            inconsistent.version,
            &inconsistent,
        )
        .unwrap();
        transaction.commit().unwrap();
    }
    for request in [Some("request-a"), Some("missing-request"), None] {
        assert_eq!(
            store
                .customer_overview_read_snapshot(&principal.tenant_id, "customer-a", request)
                .unwrap_err()
                .code,
            WorkflowErrorCode::CorruptStore
        );
    }
    assert!(store.connection.lock().unwrap().is_autocommit());
}

#[test]
fn customer_overview_cache_validates_query_before_any_proof_lookup() {
    let (_root, _path, store, principal, _project) = accepted_project_fixture();
    store
        .customer_overview_read_snapshot(&principal.tenant_id, "customer-a", None)
        .unwrap();
    let oversized = "a".repeat(4097);
    for (customer, request) in [
        (oversized.as_str(), None),
        ("customer-a", Some(oversized.as_str())),
        ("customer-a", Some("")),
        ("customer/a", None),
    ] {
        let (result, scopes) = validation_scope::with_completed_validations(|| {
            store.customer_overview_read_snapshot(&principal.tenant_id, customer, request)
        });
        let error = result.unwrap_err();
        assert_eq!(error.code, WorkflowErrorCode::InvalidInput);
        assert_eq!(error.message, "workflow identifier is invalid");
        assert!(scopes.is_empty());
    }
}

#[test]
fn customer_overview_cache_hit_preserves_index_only_integrity_checks() {
    use std::io::{Seek, SeekFrom, Write};
    let (_root, path, store, principal, _project) = accepted_project_fixture();
    store.connection.lock().unwrap().execute_batch(
        "CREATE INDEX customer_overview_payload_index ON company_entities(payload_digest); PRAGMA wal_checkpoint(TRUNCATE)",
    ).unwrap();
    let read = || store.customer_overview_read_snapshot(&principal.tenant_id, "customer-a", None);
    read().unwrap();
    let (repeat, scopes) = validation_scope::with_completed_validations(read);
    repeat.unwrap();
    assert!(scopes.is_empty());
    let connection = store.connection.lock().unwrap();
    let size: u32 = connection
        .query_row("PRAGMA page_size", [], |row| row.get(0))
        .unwrap();
    let page: u32 = connection
        .query_row(
            "SELECT rootpage FROM sqlite_schema WHERE name='customer_overview_payload_index'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    connection.execute_batch("PRAGMA shrink_memory").unwrap();
    drop(connection);
    let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(u64::from(page - 1) * u64::from(size)))
        .unwrap();
    file.write_all(&vec![0; size as usize]).unwrap();
    file.sync_all().unwrap();
    assert_eq!(read().unwrap_err().code, WorkflowErrorCode::CorruptStore);
    assert!(store.connection.lock().unwrap().is_autocommit());
}

#[test]
fn customer_overview_cache_unsupported_input_falls_back_to_independent_validators() {
    let (_root, _path, store, principal, _project) = accepted_project_fixture();
    let expected = store
        .customer_overview_read_snapshot(&principal.tenant_id, "customer-a", None)
        .unwrap();
    store
        .connection
        .lock()
        .unwrap()
        .execute_batch("CREATE TABLE unrelated_optional_cache_input(value BLOB)")
        .unwrap();
    for _ in 0..2 {
        let (actual, scopes) = validation_scope::with_completed_validations(|| {
            store.customer_overview_read_snapshot(&principal.tenant_id, "customer-a", None)
        });
        assert_eq!(
            serde_json::to_vec(&actual.unwrap()).unwrap(),
            serde_json::to_vec(&expected).unwrap()
        );
        assert!(scopes.len() >= 4);
    }
    assert!(store.connection.lock().unwrap().is_autocommit());
}
