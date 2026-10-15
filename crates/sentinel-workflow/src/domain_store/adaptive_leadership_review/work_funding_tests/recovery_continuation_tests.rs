use super::*;
use crate::AdaptiveFundedReviewRecoveryReceiptV1;

fn journal_rows(f: &Fixture) -> Vec<(String, String, Vec<u8>)> {
    let connection = f.store.connection.lock().unwrap();
    let mut statement = connection
        .prepare(
            "SELECT operation_id,request_digest,response FROM workflow_operations
             WHERE operation_namespace=?1 ORDER BY operation_id",
        )
        .unwrap();
    let rows = statement
        .query_map(
            [format!("adaptive-session-v1:{}", f.grant.session_id)],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    rows.collect::<Result<Vec<_>, _>>().unwrap()
}

fn retained_facts(
    store: &WorkflowStore,
    receipt: &AdaptiveFundedReviewRecoveryReceiptV1,
) -> (AdaptiveFundedReviewRecoveryReceiptV1, Vec<u8>) {
    let tenant = &receipt.issuer_principal.tenant_id;
    let actual = store
        .adaptive_funded_review_recovery(tenant, &receipt.receipt_key)
        .unwrap()
        .unwrap();
    assert_eq!(&actual, receipt);
    let historical = store
        .adaptive_funded_review_recovery_historical_verification_source(
            tenant,
            &receipt.receipt_key,
        )
        .unwrap()
        .unwrap();
    assert_eq!(historical.source, *receipt.request.source);
    assert_eq!(*historical.project, receipt.context.source_project);
    assert_eq!(*historical.session, receipt.context.source_session);
    assert_eq!(
        *historical.funding,
        receipt.grant_template.work_funding.as_ref().unwrap().receipt
    );
    assert_eq!(historical.calls.len(), receipt.review_members.len());
    for (call, member) in historical.calls.iter().zip(&receipt.review_members) {
        assert_eq!(call.grant.review_id, member.review_id);
        assert_eq!(call.version, member.version);
        assert_eq!(
            canonical_sha256("sentinel.workflow.adaptive-leadership-call.v1", call).unwrap(),
            member.digest
        );
    }
    let bytes = encode(&(
        historical.source,
        historical.project,
        historical.session,
        historical.funding,
        historical.calls,
    ))
    .unwrap();
    (actual, bytes)
}

pub(super) fn recovered_then_followup_fixture() -> (
    Fixture,
    AdaptiveFundedReviewRecoveryReceiptV1,
    AdaptiveLeadershipReviewCallV1,
    u64,
) {
    let (mut f, request, at) = recovery_fixture();
    let original = session(&f);
    let original_rows = journal_rows(&f);
    let funding = f.grant.work_funding.as_ref().unwrap().receipt.clone();
    let receipt = f
        .store
        .authorize_adaptive_funded_review_recovery(
            &funding_operator(&f),
            &request,
            &f.grant,
            &f.context,
            at,
            recovery_evidence,
        )
        .unwrap()
        .1;
    let recovered = f
        .store
        .adaptive_leadership_review_call(&f.leader.tenant_id, receipt.grant_template.review_id)
        .unwrap()
        .unwrap();
    assert_eq!(recovered.grant.schema_version, 6);
    let epoch = recovered.grant.work_funding.as_ref().unwrap().clone();
    assert_eq!(epoch.receipt, funding);
    let dispatched = f
        .store
        .claim_adaptive_leadership_review_call(&f.leader, &claim(&recovered), at + 1)
        .unwrap();
    let result = continued(&dispatched, at + 2, 5);
    let completed = f
        .store
        .complete_adaptive_leadership_review_call(&f.leader, &result, at + 2)
        .unwrap();
    assert_eq!(completed.continuation, result.continuation);
    let adopted = session(&f);
    assert_eq!(adopted.grant, original.grant);
    assert_eq!(adopted.version, original.version + 1);
    assert_eq!(adopted.model_calls, original.model_calls);
    assert_eq!(adopted.tool_calls, original.tool_calls);
    assert!(adopted
        .continuation
        .as_ref()
        .unwrap()
        .authorizations
        .starts_with(&original.continuation.as_ref().unwrap().authorizations));

    // An ordinary successor needs actual productive journal progress, not just adoption.
    let progressed = observe_budget_inspection(&f, adopted.clone(), at + 3);
    assert_eq!(progressed.version, adopted.version + 4);
    assert_eq!(progressed.model_calls, adopted.model_calls + 1);
    assert_eq!(progressed.tool_calls, adopted.tool_calls + 1);
    assert_eq!(progressed.grant, original.grant);
    assert_eq!(progressed.active_work_funding(), Some(epoch.as_ref()));
    let now = progressed.active_deadline_ms();
    budget_context(&mut f, now, "productive-followup-after-schema6-continuation");
    bind(&mut f, &funding, epoch.binding.ordinal + 1);
    let followup = issue(&f, now).unwrap();
    assert_eq!(followup.grant.schema_version, 5);
    assert!(followup.grant.funded_review_recovery.is_none());
    assert!(followup.dispatch.is_none());
    assert!(followup.decision.is_none());
    assert_eq!(followup.context.source_session, progressed);
    assert!(followup.grant.expected_session_version > receipt.context.source_session.version);
    assert!(followup.grant_issued_at_unix_ms >= receipt.issued_at_unix_ms);
    let followup_epoch = followup.grant.work_funding.as_ref().unwrap();
    assert!(followup_epoch.same_epoch(&epoch));
    assert_eq!(followup_epoch.receipt, funding);
    assert_eq!(followup_epoch.binding.ordinal, epoch.binding.ordinal + 1);
    assert!(journal_rows(&f).starts_with(&original_rows));
    retained_facts(&f.store, &receipt);
    (f, receipt, followup, now + 1)
}

#[test]
fn schema6_continue_then_schema5_dispatch_and_adoption_preserve_recovery_history() {
    let (f, receipt, followup, dispatch_at) = recovered_then_followup_fixture();
    let original_facts = retained_facts(&f.store, &receipt);
    let original_rows = journal_rows(&f);
    let source = session(&f);
    let dispatched = f
        .store
        .claim_adaptive_leadership_review_call(&f.leader, &claim(&followup), dispatch_at)
        .unwrap();
    assert_eq!(retained_facts(&f.store, &receipt), original_facts);
    let result = continued(&dispatched, dispatch_at + 1, 5);
    let completed = f
        .store
        .complete_adaptive_leadership_review_call(&f.leader, &result, dispatch_at + 1)
        .unwrap();
    let current = session(&f);
    assert_eq!(current.grant, receipt.context.source_session.grant);
    assert_eq!(current.version, source.version + 1);
    assert_eq!(current.model_calls, source.model_calls);
    assert_eq!(current.tool_calls, source.tool_calls);
    assert_eq!(
        current.active_work_funding(),
        followup.grant.work_funding.as_deref()
    );
    let authorizations = &current.continuation.as_ref().unwrap().authorizations;
    assert_eq!(
        authorizations.len(),
        source.continuation.as_ref().unwrap().authorizations.len() + 1
    );
    assert!(authorizations.starts_with(&source.continuation.as_ref().unwrap().authorizations));
    assert_eq!(authorizations.last(), result.continuation.as_ref());
    assert_eq!(completed.grant, followup.grant);
    assert_eq!(completed.operation_id, followup.operation_id);
    assert!(journal_rows(&f).starts_with(&original_rows));
    assert_eq!(retained_facts(&f.store, &receipt), original_facts);

    let connection = f.store.connection.lock().unwrap();
    let cut: i64 = connection
        .query_row(
            "SELECT sequence FROM company_events WHERE tenant_id=?1
             AND event_type='adaptive_funded_review_recovery_issued' AND operation_id=?2",
            params![f.leader.tenant_id.0, receipt.request.operation_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let (first, count): (i64, i64) = connection
        .query_row(
            "SELECT min(sequence),count(*) FROM company_events WHERE tenant_id=?1
             AND operation_id=?2 AND event_type IN ('adaptive_leadership_review_authorized',
             'adaptive_leadership_review_dispatched','adaptive_leadership_review_completed',
             'adaptive_work_funding_adopted')",
            params![f.leader.tenant_id.0, followup.operation_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(first > cut);
    assert_eq!(count, 4);
    let funding_rows: i64 = connection
        .query_row(
            "SELECT count(*) FROM company_entities WHERE tenant_id=?1
             AND entity_kind='adaptive_work_funding'",
            [&f.leader.tenant_id.0],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        funding_rows, 1,
        "continuation must not mint a new funding receipt"
    );
    drop(connection);

    let reopened = WorkflowStore::open(&f.path).unwrap();
    assert_eq!(retained_facts(&reopened, &receipt), original_facts);
    let before = rows(&reopened);
    let expired = followup
        .grant
        .work_funding
        .as_ref()
        .unwrap()
        .binding
        .limits
        .expires_at_unix_ms
        + 1;
    for _ in 0..2 {
        assert_eq!(
            reopened
                .complete_adaptive_leadership_review_call(&f.leader, &result, expired)
                .unwrap(),
            completed
        );
    }
    assert_eq!(rows(&reopened), before);
    assert_eq!(retained_facts(&reopened, &receipt), original_facts);
}

#[test]
fn retained_recovery_cache_revalidates_legitimate_append_without_renewing_authority() {
    for external in [false, true] {
        let (f, receipt, followup, dispatch_at) = recovered_then_followup_fixture();
        let separate = external.then(|| WorkflowStore::open(&f.path).unwrap());
        let reader = separate.as_ref().unwrap_or(&f.store);
        let before = rows(&f.store);
        let (expected, cold) =
            validation_scope::with_completed_validations(|| retained_facts(reader, &receipt));
        // Helper reads may already have warmed f.store; the second store is always cold.
        if external {
            assert!(!cold.is_empty());
        }
        let (warm, scopes) =
            validation_scope::with_completed_validations(|| retained_facts(reader, &receipt));
        assert_eq!(warm, expected);
        assert!(scopes.is_empty());
        assert_eq!(rows(&f.store), before);

        f.store
            .claim_adaptive_leadership_review_call(&f.leader, &claim(&followup), dispatch_at)
            .unwrap();
        let after_dispatch = rows(&f.store);
        let (refreshed, scopes) =
            validation_scope::with_completed_validations(|| retained_facts(reader, &receipt));
        assert_eq!(refreshed, expected);
        assert!(
            scopes
                .iter()
                .any(|scope| scope.get("funded-recovery-history").copied().unwrap_or(0) > 0),
            "legitimate append must invalidate historical proofs; external={external}"
        );
        let (warm, scopes) =
            validation_scope::with_completed_validations(|| retained_facts(reader, &receipt));
        assert_eq!(warm, expected);
        assert!(scopes.is_empty());
        assert_eq!(rows(&f.store), after_dispatch);

        let recovered = f
            .store
            .adaptive_leadership_review_call(&f.leader.tenant_id, receipt.grant_template.review_id)
            .unwrap()
            .unwrap();
        assert!(recovered.decision.is_some());
        assert!(f
            .store
            .claim_adaptive_leadership_review_call(&f.leader, &claim(&recovered), dispatch_at)
            .is_err());
        assert!(reader
            .adaptive_funded_review_recovery_verification_source(
                &funding_operator(&f),
                &followup.grant.project_id,
                followup.grant.session_id,
                receipt.request.source.retired_review_id,
                dispatch_at,
            )
            .is_err());
        assert_eq!(rows(&f.store), after_dispatch);
        assert_eq!(retained_facts(reader, &receipt), expected);
        assert_eq!(validation_scope::validations("funded-recovery-history"), 0);
    }
}
