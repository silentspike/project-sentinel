use super::super::leadership_discovery::pending_review_calls;
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
                    .into_iter()
                    .filter(|call| call.decision.is_none() && call.retired_at_unix_ms.is_none())
                    .collect::<Vec<_>>()
            );
        }
    }
    assert_eq!(snapshot(store), before);
}

fn projected_material_calls(
    view: &AdaptiveLeadershipDiscoveryRead,
    source: &AdaptiveMaterialWindowVerificationSourceV1,
) -> Vec<AdaptiveLeadershipReviewCallV1> {
    view.projects()
        .iter()
        .find(|project| {
            project.project().tenant_id == source.project.tenant_id
                && project.project().project_id == source.project.project_id
        })
        .unwrap()
        .scopes()
        .iter()
        .find(|scope| {
            scope.work_item_id() == &source.session.grant.authority.work_item_id
                && scope.agent_id() == source.session.grant.authority.agent_id
        })
        .unwrap()
        .review_calls(source.session.grant.session_id)
        .unwrap()
}

#[test]
fn leadership_discovery_projection_does_not_decide_eligibility_or_hide_ambiguity() {
    let (_temp, _path, store, _operator, source, _request, _proof, _now) =
        crate::domain_store::adaptive_leadership_review::material_preview_fixture_for_tests();
    let before = snapshot(&store);
    let terminal = source.refused.clone();
    assert!(terminal.decision.is_some());
    assert!(terminal.dispatch.is_some());
    let mut pending = terminal.clone();
    pending.decision = None;
    pending.retired_at_unix_ms = None;
    // Pure projection inputs, not forged persisted grants or authorization proof.
    let mut candidates = Vec::new();
    for variant in 0..7 {
        let mut call = pending.clone();
        match variant {
            0 => call.dispatch = None,
            1 => call.grant.leadership_principal.agent_id = Some(AgentId(999)),
            2 => call.grant_issued_at_unix_ms = u64::MAX,
            3 => call.grant.expires_at_unix_ms = 1,
            4 => call.context.source_session.version += 1,
            5 => call.context.source_project.tenant_id = TenantId::parse("foreign").unwrap(),
            _ => {}
        }
        candidates.push(call);
    }
    assert!(pending.grant.work_funding.is_some());
    // Duplicate-sensitive: even two matching funded calls must reach ambiguity
    // detection; a projection must neither select one nor deduplicate them.
    candidates.push(pending.clone());
    candidates.push(pending.clone());
    let mut retired = pending;
    retired.retired_at_unix_ms = Some(1);
    let mut input = vec![terminal.clone(), retired];
    input.extend(candidates.clone());
    input.insert(5, terminal);
    assert_eq!(pending_review_calls(input), candidates);
    assert_eq!(snapshot(&store), before);
}

#[test]
fn leadership_discovery_projects_durable_history_without_changing_history_api_or_reopen() {
    let (_temp, path, store, _operator, source, _request, _proof, _now) =
        crate::domain_store::adaptive_leadership_review::material_preview_fixture_for_tests();
    let leader = source.refused.grant.leadership_principal.agent_id.unwrap();
    let tenant = &source.project.tenant_id;
    let session = source.session.grant.session_id;
    let history = store
        .adaptive_leadership_review_calls(tenant, session)
        .unwrap();
    let expected: Vec<_> = history
        .iter()
        .filter(|call| call.decision.is_none() && call.retired_at_unix_ms.is_none())
        .cloned()
        .collect();
    assert!(expected.len() < history.len());
    let before = snapshot(&store);
    let first = store.adaptive_leadership_discovery_read(leader).unwrap();
    assert_eq!(projected_material_calls(&first, &source), expected);
    store.company_projects().unwrap();
    store.adaptive_health_inventory().unwrap();
    let (second, validations) =
        crate::domain_store::validation_scope::with_completed_validations(|| {
            store.adaptive_leadership_discovery_read(leader).unwrap()
        });
    assert!(Arc::ptr_eq(&first, &second));
    assert!(validations.is_empty());
    assert_eq!(
        store
            .adaptive_leadership_review_calls(tenant, session)
            .unwrap(),
        history
    );
    assert!(store
        .adaptive_leadership_discovery_read(AgentId(999))
        .unwrap()
        .projects()
        .is_empty());
    assert_eq!(snapshot(&store), before);
    let reopened = WorkflowStore::open(&path).unwrap();
    let cold = reopened.adaptive_leadership_discovery_read(leader).unwrap();
    assert_eq!(projected_material_calls(&cold, &source), expected);
    assert_eq!(
        reopened
            .adaptive_leadership_review_calls(tenant, session)
            .unwrap(),
        history
    );
    assert_eq!(snapshot(&reopened), before);

    // Same retained view, with only the historical calls restored, measures the
    // deterministic serialization delta, not latency, cache eviction or readiness.
    let mut old = serde_json::to_value(first.as_ref()).unwrap();
    let mut replaced = 0;
    for project in old["projects"].as_array_mut().unwrap() {
        for scope in project["scopes"].as_array_mut().unwrap() {
            for review in scope["reviews"].as_array_mut().unwrap() {
                if review["session_id"] == serde_json::to_value(session).unwrap() {
                    review["calls"]["Ok"] = serde_json::to_value(&history).unwrap();
                    replaced += 1;
                }
            }
        }
    }
    assert_eq!(replaced, 1);
    assert!(
        serde_json::to_vec(first.as_ref()).unwrap().len() < serde_json::to_vec(&old).unwrap().len()
    );
}

#[test]
fn leadership_discovery_revalidates_excluded_history_after_external_mutation() {
    let (_temp, path, store, _operator, source, _request, _proof, _now) =
        crate::domain_store::adaptive_leadership_review::material_preview_fixture_for_tests();
    let leader = source.refused.grant.leadership_principal.agent_id.unwrap();
    let tenant = &source.project.tenant_id;
    let session = source.session.grant.session_id;
    let terminal = &source.refused;
    assert!(terminal.decision.is_some());
    let original = store.adaptive_leadership_discovery_read(leader).unwrap();
    let history = store
        .adaptive_leadership_review_calls(tenant, session)
        .unwrap();
    let other = Connection::open(&path).unwrap();
    let key = terminal.grant.review_id.to_string();
    let digest: String = other
        .query_row(
            "SELECT payload_digest FROM company_entities WHERE tenant_id=?1 AND entity_kind='adaptive_leadership_review_call' AND entity_id=?2",
            rusqlite::params![tenant.0, key],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        other
            .execute(
                "UPDATE company_entities SET payload_digest='invalid' WHERE tenant_id=?1 AND entity_kind='adaptive_leadership_review_call' AND entity_id=?2",
                rusqlite::params![tenant.0, key],
            )
            .unwrap(),
        1
    );
    let damaged = snapshot(&store);
    for _ in 0..2 {
        assert_eq!(
            store
                .adaptive_leadership_review_calls(tenant, session)
                .unwrap_err()
                .code,
            WorkflowErrorCode::CorruptStore
        );
        match store.adaptive_leadership_discovery_read(leader) {
            Err(error) => assert_eq!(error.code, WorkflowErrorCode::CorruptStore),
            Ok(view) => {
                assert!(!Arc::ptr_eq(&original, &view));
                let scope = view
                    .projects()
                    .iter()
                    .find(|project| project.project().project_id == source.project.project_id)
                    .unwrap()
                    .scopes()
                    .iter()
                    .find(|scope| {
                        scope.work_item_id() == &source.session.grant.authority.work_item_id
                    })
                    .unwrap();
                assert_eq!(
                    scope.review_calls(session).unwrap_err().code,
                    WorkflowErrorCode::CorruptStore
                );
            }
        }
    }
    assert_eq!(snapshot(&store), damaged);
    assert_eq!(
        other
            .execute(
                "UPDATE company_entities SET payload_digest=?3 WHERE tenant_id=?1 AND entity_kind='adaptive_leadership_review_call' AND entity_id=?2",
                rusqlite::params![tenant.0, key, digest],
            )
            .unwrap(),
        1
    );
    let restored = store.adaptive_leadership_discovery_read(leader).unwrap();
    assert!(!Arc::ptr_eq(&original, &restored));
    assert_eq!(
        projected_material_calls(&restored, &source),
        pending_review_calls(history.clone())
    );
    assert_eq!(
        store
            .adaptive_leadership_review_calls(tenant, session)
            .unwrap(),
        history
    );
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
