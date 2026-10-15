//! Synthetic offline storage regressions, not live provider or latency evidence.
use super::super::adaptive_leadership_review::tests::reconcile_review_at;
use super::super::adaptive_resume_policy::tests::unreviewed_policy_root_with_limits;
use super::*;
use sentinel_limbo::rusqlite::{params, Connection};
use sentinel_workflow::{AdaptiveLeadershipReviewCallV1, WorkflowStore};

type Snapshot = (
    Option<AdaptiveLeadershipReviewCallV1>,
    Result<Option<AdaptiveWorkFundingReceiptV1>, WorkflowError>,
    Result<Option<Uuid>, WorkflowError>,
);

type Selection = (TenantId, Uuid, Uuid, Uuid, u64, bool);

fn fixture() -> (
    tempfile::TempDir,
    WorkflowApi,
    AdaptiveLeadershipReviewCallV1,
) {
    let (temp, api, source) = unreviewed_policy_root_with_limits(120_000, false, 4, 4);
    let principal = api.principals.principal("operator").unwrap();
    let now = now_unix_ms();
    let request = api
        .store
        .adaptive_work_funding_draft(
            &principal.principal,
            &source.grant.authority.project_id,
            source.grant.session_id,
            Uuid::new_v4(),
            "funding-review-snapshot-regression",
            AdaptiveWorkFundingLimitsV1 {
                additional_model_calls: 4,
                additional_tool_calls: 4,
                additional_reviews: 4,
                additional_windows: 4,
                max_window_ms: 300_000,
                max_call_duration_ms: 120_000,
                dispatch_margin_ms: sentinel_workflow::ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
                expires_at_unix_ms: now + 600_000,
            },
            now,
        )
        .unwrap();
    api.store
        .authorize_adaptive_work_funding(&principal.principal, &request, now)
        .unwrap();
    let project = api
        .store
        .company_project(
            &source.grant.authority.tenant_id,
            &source.grant.authority.project_id,
        )
        .unwrap()
        .unwrap();
    assert!(reconcile_review_at(&api, &project, now_unix_ms()));
    let mut calls = api
        .store
        .adaptive_leadership_review_calls(&project.tenant_id, source.grant.session_id)
        .unwrap();
    assert_eq!(calls.len(), 1);
    let call = calls.pop().unwrap();
    assert!(call.grant.work_funding.is_some());
    (temp, api, call)
}

fn selection(call: &AdaptiveLeadershipReviewCallV1) -> Selection {
    (
        call.grant.leadership_principal.tenant_id.clone(),
        call.grant.review_id,
        call.grant.session_id,
        call.grant
            .work_funding
            .as_ref()
            .unwrap()
            .receipt
            .request
            .operation_id,
        call.grant.expected_session_version,
        true,
    )
}

fn batch(store: &WorkflowStore, selection: &Selection) -> Result<Snapshot, WorkflowError> {
    store.adaptive_work_funding_review_snapshot(
        &selection.0,
        selection.1,
        selection.2,
        selection.3,
        selection.4,
        selection.5,
    )
}

fn legacy(store: &WorkflowStore, selection: &Selection) -> Result<Snapshot, WorkflowError> {
    let (tenant, review, session, operation, version, check_current) = selection;
    let review = store.adaptive_leadership_review_call(tenant, *review)?;
    if review.is_none() {
        return Ok((review, Ok(None), Ok(None)));
    }
    let receipt = store.adaptive_work_funding(tenant, *session, *operation);
    if !matches!(&receipt, Ok(Some(_))) || !*check_current {
        return Ok((review, receipt, Ok(None)));
    }
    let current = store
        .adaptive_leadership_review_calls(tenant, *session)
        .map(|calls| {
            let current: Vec<_> = calls
                .iter()
                .filter(|candidate| {
                    candidate.decision.is_none()
                        && candidate.retired_at_unix_ms.is_none()
                        && candidate.grant.expected_session_version == *version
                })
                .collect();
            match current.as_slice() {
                [only] => Some(only.grant.review_id),
                _ => None,
            }
        });
    Ok((review, receipt, current))
}

fn assert_equivalent(store: &WorkflowStore, selected: &Selection) {
    assert_eq!(batch(store, selected), legacy(store, selected));
}

// Retain a legitimate row checksum so semantic validation must catch the change.
fn replace_review(connection: &Connection, call: &AdaptiveLeadershipReviewCallV1) {
    let payload = serde_json::to_vec(call).unwrap();
    let mut digest = Sha256::new();
    digest.update(b"sentinel.workflow.company-entity-row.v1\0");
    digest.update(serde_json::to_vec(&payload).unwrap());
    connection
        .execute(
            "INSERT INTO company_entities
             (tenant_id,entity_kind,entity_id,version,payload,payload_digest)
             VALUES(?1,'adaptive_leadership_review_call',?2,?3,?4,?5)
             ON CONFLICT(tenant_id,entity_kind,entity_id) DO UPDATE
             SET version=excluded.version,payload=excluded.payload,payload_digest=excluded.payload_digest",
            params![
                call.grant.leadership_principal.tenant_id.0,
                call.grant.review_id.to_string(),
                i64::try_from(call.version).unwrap(),
                payload,
                format!("{:x}", digest.finalize()),
            ],
        )
        .unwrap();
}

#[test]
fn funding_review_snapshot_matches_three_reads_cold_warm_and_missing() {
    let (temp, api, call) = fixture();
    let store = WorkflowStore::open(temp.path().join("company.sqlite")).unwrap();
    let selected = selection(&call);
    for _ in 0..3 {
        assert_equivalent(&store, &selected);
        let (review, receipt, current) = batch(&store, &selected).unwrap();
        assert_eq!(review, Some(call.clone()));
        assert_eq!(
            receipt.unwrap(),
            Some(call.grant.work_funding.as_ref().unwrap().receipt.clone())
        );
        assert_eq!(current.unwrap(), Some(call.grant.review_id));
        assert_eq!(api.validate_work_funding_review(&call), Ok(()));
    }
    let mut missing = selected.clone();
    missing.1 = Uuid::new_v4();
    for _ in 0..2 {
        assert_equivalent(&store, &missing);
        assert!(batch(&store, &missing).unwrap().0.is_none());
    }
}

#[test]
fn funding_review_snapshot_key_binds_every_selection_input() {
    let (temp, _api, call) = fixture();
    let store = WorkflowStore::open(temp.path().join("company.sqlite")).unwrap();
    let selected = selection(&call);
    let mut variants = Vec::new();
    let mut other = selected.clone();
    other.0 = TenantId::parse("other-funding-tenant").unwrap();
    variants.push(other);
    let mut other = selected.clone();
    other.1 = Uuid::new_v4();
    variants.push(other);
    let mut other = selected.clone();
    other.2 = Uuid::new_v4();
    variants.push(other);
    let mut other = selected.clone();
    other.3 = Uuid::new_v4();
    variants.push(other);
    let mut other = selected.clone();
    other.4 += 1;
    variants.push(other);
    let mut other = selected.clone();
    other.5 = false;
    variants.push(other);
    for variant in variants {
        for _ in 0..2 {
            assert_equivalent(&store, &selected);
            assert_equivalent(&store, &variant);
            let (review, receipt, current) = batch(&store, &variant).unwrap();
            if variant.0 != selected.0 || variant.1 != selected.1 {
                assert!(review.is_none());
            } else if variant.2 != selected.2 {
                // The issuance exists under this operation, but not this session.
                assert_eq!(receipt.unwrap_err().code, WorkflowErrorCode::CorruptStore);
            } else if variant.3 != selected.3 {
                assert!(receipt.unwrap().is_none());
            } else {
                assert_eq!(review, Some(call.clone()));
                assert!(receipt.unwrap().is_some());
                assert_eq!(current.unwrap(), None);
            }
        }
    }
}

#[test]
fn funding_review_snapshot_rechecks_external_retirement_without_caching_freshness() {
    let (temp, api, call) = fixture();
    let selected = selection(&call);
    assert_equivalent(&api.store, &selected);
    let external = WorkflowStore::open(temp.path().join("company.sqlite")).unwrap();
    let retired = external
        .expire_adaptive_leadership_review_call(
            &call.grant.leadership_principal,
            call.grant.review_id,
            call.version,
            call.grant.expires_at_unix_ms,
        )
        .unwrap();
    for _ in 0..2 {
        assert_equivalent(&api.store, &selected);
        let (review, receipt, current) = batch(&api.store, &selected).unwrap();
        assert_eq!(review, Some(retired.clone()));
        assert!(receipt.unwrap().is_some());
        assert_eq!(current.unwrap(), None);
    }
    assert_eq!(
        api.validate_work_funding_review(&call),
        Err("work funding current review is ambiguous")
    );
    assert_eq!(api.validate_work_funding_review(&retired), Ok(()));
}

#[test]
fn funding_review_snapshot_defers_receipt_failure_and_cleans_up_its_scope() {
    let (temp, _api, call) = fixture();
    let store = WorkflowStore::open(temp.path().join("company.sqlite")).unwrap();
    let selected = selection(&call);
    assert_eq!(
        batch(&store, &selected).unwrap().2.unwrap(),
        Some(selected.1)
    );
    let mut invalid = selected.clone();
    invalid.3 = Uuid::nil();
    for _ in 0..3 {
        assert_eq!(
            store
                .adaptive_leadership_review_call(&selected.0, selected.1)
                .unwrap(),
            Some(call.clone()),
            "the exact review must validate before the independently invalid receipt selection"
        );
        let (review, receipt, current) = batch(&store, &invalid)
            .expect("receipt selection must fail inside the tuple, not the exact review read");
        assert_eq!(review, Some(call.clone()));
        assert_eq!(receipt.unwrap_err().code, WorkflowErrorCode::InvalidInput);
        assert_eq!(current.unwrap(), None);
        assert_equivalent(&store, &invalid);
    }
    // A write after the failed read gives a leaked WAL read snapshot something
    // to pin. A non-busy truncating checkpoint proves no read savepoint escaped.
    let external = Connection::open(temp.path().join("company.sqlite")).unwrap();
    external.busy_timeout(std::time::Duration::ZERO).unwrap();
    assert_eq!(
        external
            .execute(
                "UPDATE company_entities SET payload=payload WHERE tenant_id=?1
             AND entity_kind='adaptive_leadership_review_call' AND entity_id=?2",
                params![selected.0 .0, selected.1.to_string()],
            )
            .unwrap(),
        1
    );
    let checkpoint: (i64, i64, i64) = external
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap();
    assert_eq!(
        checkpoint.0, 0,
        "deferred receipt failure leaked a read snapshot"
    );
    assert_equivalent(&store, &selected);
    assert_eq!(
        batch(&store, &selected).unwrap().2.unwrap(),
        Some(selected.1)
    );
}

#[test]
fn funding_review_snapshot_schema6_warm_proof_rechecks_external_recovery_membership() {
    use sentinel_workflow::{
        AdaptiveFundedReviewNoIoV1, AdaptiveFundedReviewRecoveryEvidenceV1,
        AdaptiveFundedReviewRecoveryRequestV1, AdaptiveRecoveryReleaseV1,
    };

    let (temp, api, old) = fixture();
    let now = old.grant.expires_at_unix_ms;
    api.store
        .expire_adaptive_leadership_review_call(
            &old.grant.leadership_principal,
            old.grant.review_id,
            old.version,
            now,
        )
        .unwrap();
    let operator = api.principals.principal("operator").unwrap();
    let source = api
        .store
        .adaptive_funded_review_recovery_verification_source(
            &operator.principal,
            &old.grant.project_id,
            old.grant.session_id,
            old.grant.review_id,
            now,
        )
        .unwrap();
    let request = AdaptiveFundedReviewRecoveryRequestV1 {
        schema_version: 1,
        operation_id: Uuid::new_v4(),
        source_digest: source.source.canonical_digest().unwrap(),
        source: Box::new(source.source.clone()),
        release: AdaptiveRecoveryReleaseV1 {
            schema_version: 1,
            source_git_sha: "a".repeat(40),
            release_manifest_digest: "b".repeat(64),
            gateway_binary_digest: "c".repeat(64),
        },
        proof_digest: "d".repeat(64),
        reason_ref: "synthetic-batch-membership-regression".into(),
        expires_at_unix_ms: now + 180_000,
    };
    let mut grant = old.grant.clone();
    let mut context = old.context.clone();
    let epoch = grant.work_funding.as_mut().unwrap();
    let prior_marker = epoch.evidence_ref().unwrap();
    epoch.binding.ordinal = source.source.ordinal;
    context
        .evidence_refs
        .retain(|reference| reference != &prior_marker);
    context.evidence_refs.push(epoch.evidence_ref().unwrap());
    context.evidence_refs.push(format!(
        "funded-undispatched-recovery:{}:{}",
        request.source_digest,
        super::super::adaptive_recovery_release::json_digest(&request).unwrap(),
    ));
    context.evidence_refs.sort();
    context.evidence_refs.dedup();
    grant.evidence_fingerprint = sentinel_workflow::adaptive_leadership_evidence_fingerprint(
        &context.tool_catalog,
        &context.evidence_refs,
    )
    .unwrap();
    grant.review_id = sentinel_workflow::adaptive_leadership_review_id(
        grant.session_id,
        grant.expected_session_version,
        &grant.evidence_fingerprint,
    )
    .unwrap();
    grant.expires_at_unix_ms = request.expires_at_unix_ms;
    let (_, recovery) = api
        .store
        .authorize_adaptive_funded_review_recovery(
            &operator.principal,
            &request,
            &grant,
            &context,
            now,
            |source, request| {
                Ok(AdaptiveFundedReviewRecoveryEvidenceV1 {
                    source_digest: source.source.canonical_digest()?,
                    proof_digest: request.proof_digest.clone(),
                    release: request.release.clone(),
                    no_io: AdaptiveFundedReviewNoIoV1::NeverReserved {
                        request_id: old.request_id(),
                    },
                    verified_at_unix_ms: now,
                })
            },
        )
        .unwrap();
    let call = api
        .store
        .adaptive_leadership_review_call(
            &old.grant.leadership_principal.tenant_id,
            recovery.bound_grant().unwrap().review_id,
        )
        .unwrap()
        .unwrap();
    assert_eq!(call.grant.schema_version, 6);
    let selected = selection(&call);
    for _ in 0..3 {
        let (review, receipt, current) = batch(&api.store, &selected).unwrap();
        assert_eq!(review, Some(call.clone()));
        assert!(receipt.unwrap().is_some());
        assert_eq!(current.unwrap(), Some(call.grant.review_id));
    }
    let external = Connection::open(temp.path().join("company.sqlite")).unwrap();
    let original: (i64, Vec<u8>, String) = external
        .query_row(
            "SELECT version,payload,payload_digest FROM company_entities WHERE tenant_id=?1
         AND entity_kind='adaptive_funded_review_recovery' AND entity_id=?2",
            params![selected.0 .0, recovery.receipt_key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        external
            .execute(
                "DELETE FROM company_entities WHERE tenant_id=?1
         AND entity_kind='adaptive_funded_review_recovery' AND entity_id=?2",
                params![selected.0 .0, recovery.receipt_key],
            )
            .unwrap(),
        1
    );
    for _ in 0..2 {
        assert_equivalent(&api.store, &selected);
        assert_eq!(
            batch(&api.store, &selected).unwrap_err().code,
            WorkflowErrorCode::CorruptStore
        );
    }
    external
        .execute(
            "INSERT INTO company_entities(tenant_id,entity_kind,entity_id,version,payload,payload_digest)
         VALUES(?1,'adaptive_funded_review_recovery',?2,?3,?4,?5)",
            params![selected.0.0, recovery.receipt_key, original.0, original.1, original.2],
        )
        .unwrap();
    assert_equivalent(&api.store, &selected);
    assert_eq!(batch(&api.store, &selected).unwrap().0, Some(call));
}

#[test]
fn funding_review_snapshot_rejects_resealed_semantic_corruption_and_recovers() {
    let (temp, api, call) = fixture();
    let selected = selection(&call);
    assert_equivalent(&api.store, &selected);
    let external = Connection::open(temp.path().join("company.sqlite")).unwrap();
    let mut corrupt = call.clone();
    corrupt.context.source_session.model_calls += 1;
    replace_review(&external, &corrupt);
    for _ in 0..2 {
        assert_equivalent(&api.store, &selected);
        assert!(batch(&api.store, &selected).is_err());
        assert_eq!(
            api.validate_work_funding_review(&call),
            Err("work funding review membership unavailable")
        );
    }
    replace_review(&external, &call);
    assert_equivalent(&api.store, &selected);
    assert_eq!(api.validate_work_funding_review(&call), Ok(()));
}

#[test]
fn funding_review_snapshot_validates_unrelated_session_history_before_filtering() {
    let (temp, api, call) = fixture();
    let selected = selection(&call);
    assert_equivalent(&api.store, &selected);
    let external = Connection::open(temp.path().join("company.sqlite")).unwrap();
    let mut unrelated = call.clone();
    unrelated.grant.review_id = Uuid::new_v4();
    unrelated.review_key = unrelated.grant.review_id.to_string();
    unrelated.grant.session_id = Uuid::new_v4();
    unrelated.retired_at_unix_ms = Some(call.grant_issued_at_unix_ms);
    replace_review(&external, &unrelated);
    for _ in 0..2 {
        assert_equivalent(&api.store, &selected);
        let (_, receipt, current) = batch(&api.store, &selected).unwrap();
        assert!(receipt.unwrap().is_some());
        assert_eq!(current.unwrap_err().code, WorkflowErrorCode::CorruptStore);
        assert_eq!(
            api.validate_work_funding_review(&call),
            Err("work funding current reviews unavailable")
        );
        let mut changed = call.clone();
        changed.allowance_id.push_str("-caller-mismatch");
        assert_eq!(
            api.validate_work_funding_review(&changed),
            Err("work funding sealed review membership changed")
        );
    }
    external
        .execute(
            "DELETE FROM company_entities WHERE tenant_id=?1
             AND entity_kind='adaptive_leadership_review_call' AND entity_id=?2",
            params![selected.0 .0, unrelated.grant.review_id.to_string()],
        )
        .unwrap();
    assert_equivalent(&api.store, &selected);
    assert_eq!(api.validate_work_funding_review(&call), Ok(()));
}

#[test]
fn funding_review_snapshot_does_not_cache_deleted_historical_issuance() {
    let (temp, api, call) = fixture();
    let selected = selection(&call);
    assert_equivalent(&api.store, &selected);
    let external = Connection::open(temp.path().join("company.sqlite")).unwrap();
    assert_eq!(
        external
            .execute(
                "DELETE FROM company_events WHERE event_type='adaptive_work_funding_issued'
                 AND operation_id=?1",
                [selected.3.to_string()],
            )
            .unwrap(),
        1
    );
    for _ in 0..2 {
        assert_equivalent(&api.store, &selected);
        assert!(batch(&api.store, &selected).is_err());
        assert_eq!(
            api.validate_work_funding_review(&call),
            Err("work funding review membership unavailable")
        );
    }
}
