use super::*;

fn discovery(store: &WorkflowStore) -> Arc<AdaptiveLeadershipDiscoveryRead> {
    store
        .adaptive_leadership_discovery_read(AgentId(1))
        .unwrap()
}

fn assert_matches_separate_reads(store: &WorkflowStore) {
    let before = snapshot(store);
    let view = discovery(store);
    let projects = store.company_projects().unwrap();
    for source in view.projects() {
        let project = source.project();
        assert!(projects.contains(project));
        for scope in source.scopes() {
            let paired = store
                .adaptive_project_snapshot(
                    &project.tenant_id,
                    &project.project_id,
                    scope.work_item_id(),
                    scope.agent_id(),
                )
                .unwrap()
                .unwrap();
            let authority = paired
                .heads
                .as_ref()
                .unwrap()
                .first()
                .unwrap()
                .grant
                .authority
                .clone();
            let expected = paired.session_for_authority(&authority).unwrap().unwrap();
            assert_eq!(
                scope.session_for_authority(&authority).unwrap(),
                Some(expected.clone())
            );
            assert_eq!(
                scope.policy_present(expected.grant.session_id).unwrap(),
                store
                    .adaptive_resume_policy(&project.tenant_id, expected.grant.session_id)
                    .unwrap()
                    .is_some()
            );
            assert_eq!(
                scope.review_calls(expected.grant.session_id).unwrap(),
                store
                    .adaptive_leadership_review_calls(&project.tenant_id, expected.grant.session_id)
                    .unwrap()
            );
        }
    }
    assert_eq!(snapshot(store), before);
}

#[test]
fn leadership_discovery_matches_separate_reads_multitenant_multisession_and_reopen() {
    let fixture = fixture();
    populate_company(&fixture.store, "aaa-other-tenant", 200, 20_000, |_| {});
    populate_company(&fixture.store, "tenant-company-claim", 300, 40_000, |_| {});
    assert_eq!(discovery(&fixture.store).projects().len(), 3);
    for _ in 0..2 {
        assert_matches_separate_reads(&fixture.store);
    }
    assert_matches_separate_reads(&WorkflowStore::open(&fixture.path).unwrap());
    assert!(fixture
        .store
        .adaptive_leadership_discovery_read(AgentId(99))
        .unwrap()
        .projects()
        .is_empty());
}

#[test]
fn leadership_discovery_warm_batch_reuses_shared_facts_but_not_runtime_authority() {
    let fixture = fixture();
    let before = snapshot(&fixture.store);
    let first = discovery(&fixture.store);
    let (second, validations) =
        crate::domain_store::validation_scope::with_completed_validations(|| {
            discovery(&fixture.store)
        });
    assert!(validations.is_empty());
    assert!(Arc::ptr_eq(&first, &second));
    let scope = &second.projects()[0].scopes()[0];
    for change in ["tenant", "project", "work", "agent", "runtime", "principal"] {
        let mut authority = fixture.root.authority.clone();
        match change {
            "tenant" => authority.tenant_id = TenantId::parse("foreign").unwrap(),
            "project" => authority.project_id = ProjectId::parse("foreign").unwrap(),
            "work" => authority.work_item_id = WorkItemId::parse("foreign").unwrap(),
            "agent" => authority.agent_id = AgentId(99),
            "runtime" => authority.runtime_generation += 1,
            "principal" => authority.principal.principal_generation += 1,
            _ => unreachable!(),
        }
        assert_eq!(
            scope.session_for_authority(&authority).unwrap_err().code,
            WorkflowErrorCode::AuthorityConflict,
            "{change}"
        );
    }
    assert_eq!(snapshot(&fixture.store), before);
}

#[test]
fn leadership_discovery_external_head_mutation_is_deferred_not_cached_or_hidden() {
    let fixture = fixture();
    let first = discovery(&fixture.store);
    let other = Connection::open(&fixture.path).unwrap();
    other
        .execute("UPDATE workflow_adaptive_heads SET version=version+1", [])
        .unwrap();
    let before = snapshot(&fixture.store);
    for _ in 0..2 {
        let view = discovery(&fixture.store);
        assert!(!Arc::ptr_eq(&first, &view));
        assert_eq!(
            view.projects()[0].scopes()[0]
                .session_for_authority(&fixture.root.authority)
                .unwrap_err()
                .code,
            WorkflowErrorCode::CorruptStore
        );
    }
    assert_eq!(snapshot(&fixture.store), before);
    // A different leader must still validate the complete project inventory,
    // but unrelated adaptive heads are not an authority dependency.
    assert!(fixture
        .store
        .adaptive_leadership_discovery_read(AgentId(99))
        .unwrap()
        .projects()
        .is_empty());
}

#[test]
fn leadership_discovery_policy_failure_does_not_hide_heads_or_get_retained() {
    let fixture = fixture();
    discovery(&fixture.store);
    let session = fixture.root.session_id;
    let connection = fixture.store.lock().unwrap();
    // An orphan policy event is rejected by the original leaf reader.
    connection.execute(
        "INSERT INTO company_events(event_id,tenant_id,project_id,event_type,operation_id,operation_digest,principal_id,principal_kind,principal_role,agent_id,customer_id,authority_generation,authority_digest,authority_binding_digest,payload,payload_digest,created_at_ms)
         SELECT 'orphan-policy',tenant_id,project_id,'adaptive_resume_policy_authorized',operation_id,operation_digest,principal_id,principal_kind,principal_role,agent_id,customer_id,authority_generation,authority_digest,authority_binding_digest,?1,payload_digest,created_at_ms FROM company_events LIMIT 1",
        [serde_json::to_vec(&serde_json::json!({"request":{"source":{"session_id":session}}})).unwrap()],
    ).unwrap();
    drop(connection);
    let view = discovery(&fixture.store);
    let scope = &view.projects()[0].scopes()[0];
    assert!(scope
        .session_for_authority(&fixture.root.authority)
        .unwrap()
        .is_some());
    assert_eq!(
        scope.policy_present(session).unwrap_err().code,
        WorkflowErrorCode::CorruptStore
    );
    assert!(scope.review_calls(session).unwrap().is_empty());
    let next = discovery(&fixture.store);
    assert!(!Arc::ptr_eq(&view, &next));
}

#[test]
fn leadership_discovery_deleted_project_and_changed_schema_invalidate_warm_facts() {
    for change in ["delete", "schema", "corrupt"] {
        let fixture = fixture();
        discovery(&fixture.store);
        let other = Connection::open(&fixture.path).unwrap();
        match change {
            "delete" => {
                other
                    .execute(
                        "DELETE FROM company_entities WHERE entity_kind='project'",
                        [],
                    )
                    .unwrap();
            }
            "schema" => other
                .execute_batch("CREATE TABLE unexpected(value TEXT)")
                .unwrap(),
            "corrupt" => {
                other.execute("UPDATE company_entities SET payload_digest='invalid' WHERE entity_kind='project'", []).unwrap();
            }
            _ => unreachable!(),
        }
        let before = snapshot(&fixture.store);
        match change {
            "delete" => assert!(discovery(&fixture.store).projects().is_empty()),
            // Unsupported cache schemas bypass reuse; they do not invent a domain error.
            "schema" => assert_eq!(discovery(&fixture.store).projects().len(), 1),
            "corrupt" => assert!(fixture
                .store
                .adaptive_leadership_discovery_read(AgentId(1))
                .is_err()),
            _ => unreachable!(),
        }
        assert_eq!(snapshot(&fixture.store), before);
    }
}

#[test]
fn leadership_discovery_outer_transaction_rollback_does_not_reuse_transient_facts() {
    let fixture = fixture();
    let original = discovery(&fixture.store);
    fixture
        .store
        .lock()
        .unwrap()
        .execute_batch("BEGIN; DELETE FROM company_entities WHERE entity_kind='project'")
        .unwrap();
    assert!(discovery(&fixture.store).projects().is_empty());
    assert!(!fixture.store.lock().unwrap().is_autocommit());
    fixture
        .store
        .lock()
        .unwrap()
        .execute_batch("ROLLBACK")
        .unwrap();
    let current = discovery(&fixture.store);
    assert_eq!(
        current.projects()[0].project(),
        original.projects()[0].project()
    );
    assert!(!Arc::ptr_eq(&current, &original));
}

#[test]
fn leadership_discovery_head_limit_preserves_independent_scopes_and_conflict() {
    let fixture = fixture();
    let (_, _, other_root) =
        populate_company(&fixture.store, "zzz-independent", 300, 40_000, |_| {});
    for index in 1..=MAX_SCOPED_ADAPTIVE_HEADS {
        let mut grant = fixture.root.clone();
        grant.session_id = Uuid::from_u128(1_000 + index as u128);
        grant.authority.assignment_version += index as u64;
        fixture
            .store
            .begin_adaptive_session(&grant, &grant.authority, NOW)
            .unwrap();
        if index < MAX_SCOPED_ADAPTIVE_HEADS {
            let mut other = other_root.clone();
            other.session_id = Uuid::from_u128(2_000 + index as u128);
            other.authority.assignment_version += index as u64;
            fixture
                .store
                .begin_adaptive_session(&other, &other.authority, NOW)
                .unwrap();
        }
        if index == MAX_SCOPED_ADAPTIVE_HEADS - 1 {
            let view = discovery(&fixture.store);
            assert_eq!(
                view.projects()[0].scopes()[0]
                    .session_for_authority(&fixture.root.authority)
                    .unwrap()
                    .unwrap()
                    .grant,
                fixture.root
            );
            assert_eq!(
                view.projects()[1].scopes()[0]
                    .session_for_authority(&other_root.authority)
                    .unwrap()
                    .unwrap()
                    .grant,
                other_root
            );
        }
    }
    let view = discovery(&fixture.store);
    assert_eq!(
        view.projects()[0].scopes()[0]
            .session_for_authority(&fixture.root.authority)
            .unwrap_err()
            .code,
        WorkflowErrorCode::AuthorityConflict
    );
    assert_eq!(view.projects().len(), 2);
    assert_eq!(view.projects()[1].scopes().len(), 1);
    assert_eq!(
        view.projects()[1].scopes()[0]
            .session_for_authority(&other_root.authority)
            .unwrap()
            .unwrap()
            .grant,
        other_root
    );
}

#[test]
fn leadership_discovery_rejects_raw_index_damage_with_unchanged_table_rows() {
    use std::io::{Seek, SeekFrom, Write};
    let fixture = fixture();
    let other = Connection::open(&fixture.path).unwrap();
    other.execute_batch("CREATE INDEX discovery_probe ON workflow_adaptive_heads(updated_at_ms); PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    discovery(&fixture.store);
    let (root, page_size): (u32, u32) = {
        let connection = fixture.store.lock().unwrap();
        (
            connection
                .query_row(
                    "SELECT rootpage FROM sqlite_schema WHERE name='discovery_probe'",
                    [],
                    |row| row.get(0),
                )
                .unwrap(),
            connection
                .query_row("PRAGMA page_size", [], |row| row.get(0))
                .unwrap(),
        )
    };
    let before = snapshot(&fixture.store);
    fixture
        .store
        .lock()
        .unwrap()
        .execute_batch("PRAGMA shrink_memory")
        .unwrap();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&fixture.path)
        .unwrap();
    file.seek(SeekFrom::Start(u64::from(root - 1) * u64::from(page_size)))
        .unwrap();
    file.write_all(&vec![0; page_size as usize]).unwrap();
    file.sync_all().unwrap();
    assert_eq!(snapshot(&fixture.store), before);
    assert!(fixture
        .store
        .adaptive_leadership_discovery_read(AgentId(1))
        .is_err());
}
