#![cfg(test)]
//! Synthetic durable lineage, not live provider or runtime latency evidence.
use super::*;
use std::io::{Seek, SeekFrom, Write};

struct Fixture {
    _temp: tempfile::TempDir,
    path: std::path::PathBuf,
    store: WorkflowStore,
    operator: AuthenticatedCompanyPrincipalV1,
    source: AdaptiveMaterialWindowVerificationSourceV1,
    request: AdaptiveWorkFundingRequestV1,
    proof: AdaptiveMaterialWindowUsageProofV1,
    now: u64,
}

fn fixture() -> Fixture {
    let (_temp, path, store, operator, source, request, proof, now) =
        crate::domain_store::adaptive_leadership_review::material_preview_fixture_for_tests();
    Fixture {
        _temp,
        path,
        store,
        operator,
        source,
        request,
        proof,
        now,
    }
}

fn read(
    f: &Fixture,
    now: u64,
) -> Result<AdaptiveMaterialWindowVerificationSourceV1, WorkflowError> {
    f.store.adaptive_material_window_verification_source(
        &f.operator,
        &f.source.project.project_id,
        f.source.session.grant.session_id,
        &f.source.declined.receipt_digest()?,
        f.source.refused.grant.review_id,
        now,
    )
}

fn legacy(
    f: &Fixture,
    now: u64,
) -> Result<AdaptiveMaterialWindowVerificationSourceV1, WorkflowError> {
    let mut connection = f.store.connection.lock().unwrap();
    let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let scope = validation_scope::enter(&tx)?;
    let source = verification_source(
        &tx,
        &f.operator,
        &f.source.project.project_id,
        f.source.session.grant.session_id,
        &f.source.declined.receipt_digest()?,
        f.source.refused.grant.review_id,
        now,
    )?;
    scope.finish()?;
    Ok(source)
}

fn draft(
    f: &Fixture,
    source: &AdaptiveMaterialWindowVerificationSourceV1,
    now: u64,
) -> Result<AdaptiveWorkFundingRequestV1, WorkflowError> {
    f.store.adaptive_material_window_draft(
        &f.operator,
        source,
        f.request.operation_id,
        &f.request.reason_ref,
        f.request.limits.clone(),
        &f.proof,
        now,
    )
}

fn rows(store: &WorkflowStore) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    let connection = store.connection.lock().unwrap();
    [
        "company_entities",
        "company_operations",
        "company_events",
        "company_project_projections",
        "workflow_operations",
        "workflow_adaptive_heads",
    ]
    .into_iter()
    .map(|table| {
        let mut query = connection
            .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
            .unwrap();
        let columns = query.column_count();
        query
            .query_map([], |row| {
                (0..columns).map(|column| row.get(column)).collect()
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    })
    .collect()
}

#[test]
fn material_source_snapshot_reuses_one_full_proof_across_source_and_final_draft() {
    let mut f = fixture();
    f.store = WorkflowStore::open(&f.path).unwrap();
    let before = rows(&f.store);
    let expected = legacy(&f, f.now).unwrap();
    let (source, cold) = validation_scope::with_completed_validations(|| read(&f, f.now));
    assert_eq!(source.unwrap(), expected);
    assert_eq!(cold.len(), 1, "one arena for the full source proof");
    let (request, warm) =
        validation_scope::with_completed_validations(|| draft(&f, &expected, f.now));
    assert_eq!(request.unwrap(), f.request);
    assert!(
        warm.is_empty(),
        "final equality must reuse checked facts, not reconstruct their proofs"
    );
    for _ in 0..2 {
        let (source, warm) = validation_scope::with_completed_validations(|| read(&f, f.now + 1));
        assert_eq!(source.unwrap(), expected);
        assert!(warm.is_empty());
    }
    assert_eq!(rows(&f.store), before);
    assert!(f.store.connection.lock().unwrap().is_autocommit());
}

#[test]
fn material_source_snapshot_warm_clock_and_request_limits_are_always_fresh() {
    let f = fixture();
    read(&f, f.now).unwrap();
    let early = f
        .source
        .session
        .updated_at_ms
        .max(f.source.project.updated_at_unix_ms);
    assert!(!f.source.session.model_window_exhausted_at(early));
    for at in [0, early, f.now, f.now + 1] {
        assert_eq!(read(&f, at), legacy(&f, at), "clock {at}");
    }
    let before = rows(&f.store);
    let expired = f.request.limits.expires_at_unix_ms;
    assert!(
        draft(&f, &f.source, expired).is_err(),
        "proof reuse must not renew expired limits"
    );
    assert!(draft(&f, &f.source, 0).is_err());
    assert_eq!(draft(&f, &f.source, f.now).unwrap(), f.request);
    assert_eq!(rows(&f.store), before);
}

#[test]
fn material_source_snapshot_final_comparison_covers_every_source_component() {
    let f = fixture();
    read(&f, f.now).unwrap();
    let before = rows(&f.store);
    for field in [
        "project", "session", "declined", "refused", "anchor", "floor",
    ] {
        let mut source = f.source.clone();
        match field {
            "project" => source.project.cost_ceiling_micros += 1,
            "session" => source.session.updated_at_ms += 1,
            "declined" => source.declined.issued_at_unix_ms += 1,
            "refused" => source.refused.updated_at_unix_ms += 1,
            "anchor" => source.resume_source.base_review_count += 1,
            "floor" => source.floor.total_window_ceiling += 1,
            _ => unreachable!(),
        }
        assert!(draft(&f, &source, f.now).is_err(), "{field}");
    }
    assert_eq!(rows(&f.store), before);
}

#[test]
fn material_source_snapshot_external_project_commit_invalidates_final_source() {
    let f = fixture();
    read(&f, f.now).unwrap();
    let other = WorkflowStore::open(&f.path).unwrap();
    other
        .apply_company_command(
            &f.source.refused.grant.leadership_principal,
            Uuid::new_v4(),
            &CompanyWorkflowCommandV1::RecordDecision {
                project_id: f.source.project.project_id.clone(),
                expected_version: f.source.project.version,
                work_item_id: None,
                choice_ref: "synthetic-intervening-choice".into(),
                rationale_ref: "Synthetic source currentness regression".into(),
            },
            f.now + 1,
        )
        .unwrap();
    let before = rows(&f.store);
    assert!(draft(&f, &f.source, f.now + 1).is_err());
    assert_eq!(read(&f, f.now + 1), legacy(&f, f.now + 1));
    assert_eq!(rows(&f.store), before);
}

#[test]
fn material_source_snapshot_external_corruption_and_neighbor_rows_never_hit_old_proof() {
    for sql in [
        "UPDATE company_entities SET payload_digest='invalid' WHERE entity_kind='project'",
        "UPDATE company_events SET payload_digest='invalid' WHERE event_type='adaptive_leadership_review_completed'",
        "DELETE FROM company_events WHERE event_type='adaptive_work_funding_issued'",
        "UPDATE workflow_operations SET response=X'7b7d'",
        "UPDATE workflow_adaptive_heads SET version=version+1",
        "INSERT INTO company_entities(tenant_id,entity_kind,entity_id,version,payload,payload_digest)
         SELECT tenant_id,'adaptive_leadership_review_call','corrupt-neighbor',1,X'7b7d','invalid'
         FROM company_entities WHERE entity_kind='project' LIMIT 1",
    ] {
        let f = fixture();
        read(&f, f.now).unwrap();
        let writer = Connection::open(&f.path).unwrap();
        assert!(writer.execute(sql, []).unwrap() > 0, "fixture mutation must be real");
        let before = rows(&f.store);
        for _ in 0..2 {
            let result = read(&f, f.now);
            assert!(result.is_err(), "{sql}");
            assert_eq!(result, legacy(&f, f.now), "preserve legacy error classification");
        }
        assert_eq!(rows(&f.store), before);
    }
}

#[test]
fn material_source_snapshot_reopen_and_caller_transaction_rebuild_instead_of_reusing() {
    let mut f = fixture();
    read(&f, f.now).unwrap();
    f.store = WorkflowStore::open(&f.path).unwrap();
    let (source, cold) = validation_scope::with_completed_validations(|| read(&f, f.now));
    assert_eq!(source.unwrap(), f.source);
    assert_eq!(cold.len(), 1);
    let before = rows(&f.store);
    f.store
        .connection
        .lock()
        .unwrap()
        .execute_batch("BEGIN DEFERRED")
        .unwrap();
    for _ in 0..2 {
        let (source, bypass) = validation_scope::with_completed_validations(|| read(&f, f.now));
        assert_eq!(source.unwrap(), f.source);
        assert_eq!(
            bypass.len(),
            1,
            "caller transactions never create reusable proofs"
        );
        assert!(!f.store.connection.lock().unwrap().is_autocommit());
    }
    f.store
        .connection
        .lock()
        .unwrap()
        .execute_batch("ROLLBACK")
        .unwrap();
    assert_eq!(rows(&f.store), before);
}

#[test]
fn material_source_snapshot_index_only_damage_rejects_unchanged_table_facts() {
    let f = fixture();
    {
        let connection = f.store.connection.lock().unwrap();
        connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
    }
    read(&f, f.now).unwrap();
    let (page_size, root): (u32, u32) = {
        let connection = f.store.connection.lock().unwrap();
        let size = connection
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .unwrap();
        let root = connection
            .query_row(
                "SELECT rootpage FROM sqlite_schema WHERE name='idx_company_events_tenant_project'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        connection.execute_batch("PRAGMA shrink_memory").unwrap();
        (size, root)
    };
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&f.path)
        .unwrap();
    file.seek(SeekFrom::Start(u64::from(root - 1) * u64::from(page_size)))
        .unwrap();
    file.write_all(&vec![0; page_size as usize]).unwrap();
    file.sync_all().unwrap();
    assert!(
        read(&f, f.now).is_err(),
        "an unchanged table view cannot mask damaged indexes"
    );
}
