use super::issue856_recovery_continuation_tests::recovered_then_followup_fixture;
use super::*;
use crate::AdaptiveFundedReviewRecoveryReceiptV1;

const RECOVERY_EVENT: &str = "adaptive_funded_review_recovery_issued";
const DISPATCH_EVENT: &str = "adaptive_leadership_review_dispatched";
const ADOPTION_EVENT: &str = "adaptive_work_funding_adopted";

fn event_sequence(f: &Fixture, event: &str, operation: Uuid) -> i64 {
    f.store
        .connection
        .lock()
        .unwrap()
        .query_row(
            "SELECT sequence FROM company_events
             WHERE tenant_id=?1 AND event_type=?2 AND operation_id=?3",
            params![f.leader.tenant_id.0, event, operation.to_string()],
            |row| row.get(0),
        )
        .unwrap()
}

fn assert_readable(f: &Fixture, receipt: &AdaptiveFundedReviewRecoveryReceiptV1) {
    let before = rows(&f.store);
    assert_eq!(
        f.store
            .adaptive_funded_review_recovery(&f.leader.tenant_id, &receipt.receipt_key)
            .unwrap(),
        Some(receipt.clone())
    );
    let historical = f
        .store
        .adaptive_funded_review_recovery_historical_verification_source(
            &f.leader.tenant_id,
            &receipt.receipt_key,
        )
        .unwrap()
        .unwrap();
    assert_eq!(historical.source, *receipt.request.source);
    assert_eq!(historical.calls.len(), receipt.review_members.len());
    assert_eq!(rows(&f.store), before);
}

fn assert_corrupt(f: &Fixture, receipt: &AdaptiveFundedReviewRecoveryReceiptV1) {
    let before = rows(&f.store);
    for _ in 0..2 {
        assert_eq!(
            f.store
                .adaptive_funded_review_recovery(&f.leader.tenant_id, &receipt.receipt_key)
                .unwrap_err()
                .code,
            WorkflowErrorCode::CorruptStore
        );
        assert_eq!(
            f.store
                .adaptive_funded_review_recovery_historical_verification_source(
                    &f.leader.tenant_id,
                    &receipt.receipt_key,
                )
                .unwrap_err()
                .code,
            WorkflowErrorCode::CorruptStore
        );
    }
    assert_eq!(rows(&f.store), before);
    assert!(f.store.connection.lock().unwrap().is_autocommit());
}

fn assert_no_io_rejects(f: &Fixture, receipt: &AdaptiveFundedReviewRecoveryReceiptV1) {
    let cut = event_sequence(f, RECOVERY_EVENT, receipt.request.operation_id);
    let grant = receipt.bound_grant().unwrap();
    let connection = f.store.connection.lock().unwrap();
    let error = validation_scope::with_scope(&connection, || {
        crate::domain_store::adaptive_work_funding::require_pre_recovery_funding_history(
            &connection,
            grant.work_funding.as_ref().unwrap(),
            Some((
                &grant,
                &receipt.context,
                receipt.request.operation_id,
                receipt.issued_at_unix_ms,
                stored_u64(cut)?,
            )),
        )
    })
    .unwrap_err();
    // A semantic NoIO veto, not a broken seal, cycle, or proof-byte limit.
    assert_eq!(error.code, WorkflowErrorCode::InvalidTransition);
}

fn swap_event_positions(f: &Fixture, first: i64, second: i64) {
    assert!(first > 0 && second > 0 && first != second);
    let mut connection = f.store.connection.lock().unwrap();
    let tx = connection.transaction().unwrap();
    assert_eq!(
        tx.execute("UPDATE company_events SET sequence=-1 WHERE sequence=?1", [first])
            .unwrap(),
        1
    );
    assert_eq!(
        tx.execute(
            "UPDATE company_events SET sequence=?1 WHERE sequence=?2",
            params![first, second],
        )
        .unwrap(),
        1
    );
    assert_eq!(
        tx.execute("UPDATE company_events SET sequence=?1 WHERE sequence=-1", [second])
            .unwrap(),
        1
    );
    tx.commit().unwrap();
}

fn event_contents_without_sequence(f: &Fixture, sequence: i64) -> Vec<rusqlite::types::Value> {
    let connection = f.store.connection.lock().unwrap();
    let mut statement = connection
        .prepare("SELECT * FROM company_events WHERE sequence=?1")
        .unwrap();
    let columns: Vec<_> = (0..statement.column_count())
        .filter(|index| statement.column_name(*index).unwrap() != "sequence")
        .collect();
    statement
        .query_row([sequence], |row| {
            columns.iter().map(|index| row.get(*index)).collect()
        })
        .unwrap()
}

fn original_authorized_trace(
    f: &Fixture,
    receipt: &AdaptiveFundedReviewRecoveryReceiptV1,
) -> AdaptiveLeadershipReviewCallV1 {
    let payload: Vec<u8> = f
        .store
        .connection
        .lock()
        .unwrap()
        .query_row(
            "SELECT payload FROM company_events WHERE tenant_id=?1
             AND event_type='adaptive_leadership_review_authorized'
             AND json_extract(payload,'$.grant.review_id')=?2",
            params![f.leader.tenant_id.0, receipt.request.source.retired_review_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let call: AdaptiveLeadershipReviewCallV1 = decode(&payload).unwrap();
    call.validate_entity().unwrap();
    assert_eq!(call.grant.schema_version, 5);
    assert!(call.grant.work_funding.as_ref().unwrap().same_epoch(
        receipt.grant_template.work_funding.as_ref().unwrap(),
    ));
    call
}

fn append_adversarial_trace(f: &Fixture, call: &AdaptiveLeadershipReviewCallV1) -> u64 {
    call.validate_entity().unwrap();
    let digest = canonical_sha256("sentinel.workflow.adaptive-leadership-call.v1", call).unwrap();
    let principal = &call.grant.leadership_principal;
    let event = "adaptive_leadership_review_renewed";
    let mut connection = f.store.connection.lock().unwrap();
    let tx = connection.transaction().unwrap();
    // An adversarial sealed observation, never a new executable call or membership.
    let sequence = append_event(
        &tx,
        principal,
        call.operation_id,
        &digest,
        Some(&call.grant.project_id),
        event,
        call,
        call.updated_at_unix_ms,
    )
    .unwrap();
    tx.commit().unwrap();
    crate::domain_store::adaptive_resume_policy::require_leaf_event(
        &connection,
        &principal.tenant_id,
        &call.grant.project_id,
        event,
        call.operation_id,
        &digest,
        principal,
        call.updated_at_unix_ms,
        call,
    )
    .unwrap();
    sequence
}

#[test]
fn recovery_history_cut_requires_the_original_authenticated_issuance_event() {
    for mutation in ["missing", "payload", "issuer", "clock", "operation"] {
        let (f, receipt, _, _) = recovered_then_followup_fixture();
        assert_readable(&f, &receipt);
        let cut = event_sequence(&f, RECOVERY_EVENT, receipt.request.operation_id);
        let sql = match mutation {
            "missing" => "DELETE FROM company_events WHERE sequence=?1",
            "payload" => "UPDATE company_events SET payload_digest='corrupt' WHERE sequence=?1",
            "issuer" => "UPDATE company_events SET principal_id='wrong-issuer' WHERE sequence=?1",
            "clock" => "UPDATE company_events SET created_at_ms=created_at_ms+1 WHERE sequence=?1",
            _ => "UPDATE company_events SET operation_id='wrong-operation' WHERE sequence=?1",
        };
        assert_eq!(Connection::open(&f.path).unwrap().execute(sql, [cut]).unwrap(), 1);
        assert_corrupt(&f, &receipt);
    }
}

#[test]
fn recovery_history_cut_rejects_real_dispatch_and_adoption_moved_before_cut() {
    for event in [DISPATCH_EVENT, ADOPTION_EVENT] {
        let (f, receipt, followup, dispatch_at) = recovered_then_followup_fixture();
        let dispatched = f
            .store
            .claim_adaptive_leadership_review_call(&f.leader, &claim(&followup), dispatch_at)
            .unwrap();
        if event == ADOPTION_EVENT {
            f.store
                .complete_adaptive_leadership_review_call(
                    &f.leader,
                    &continued(&dispatched, dispatch_at + 1, 5),
                    dispatch_at + 1,
                )
                .unwrap();
        }
        assert_readable(&f, &receipt);
        let cut = event_sequence(&f, RECOVERY_EVENT, receipt.request.operation_id);
        let later = event_sequence(&f, event, followup.operation_id);
        let membership = event_sequence(
            &f,
            "adaptive_work_funding_review_issued",
            receipt.request.operation_id,
        );
        assert!(membership < cut && later > cut);
        assert!(followup.grant_issued_at_unix_ms >= receipt.issued_at_unix_ms);
        assert!(followup.grant.expected_session_version > receipt.context.source_session.version);
        // Preserve every authentic payload, seal, operation and timestamp. Only
        // move the real effect across the original cut; no inventory is dropped.
        swap_event_positions(&f, membership, later);
        assert_eq!(event_sequence(&f, RECOVERY_EVENT, receipt.request.operation_id), cut);
        assert!(event_sequence(&f, event, followup.operation_id) < cut);
        assert_no_io_rejects(&f, &receipt);
        assert_corrupt(&f, &receipt);
    }
}

#[test]
fn recovery_history_cut_rejects_designated_recovery_dispatch_and_adoption_moved_before_cut() {
    for event in [DISPATCH_EVENT, ADOPTION_EVENT] {
        let (f, receipt, _, _) = recovered_then_followup_fixture();
        assert_readable(&f, &receipt);
        let designated = f
            .store
            .adaptive_leadership_review_call(&f.leader.tenant_id, receipt.grant_template.review_id)
            .unwrap()
            .unwrap();
        assert_eq!(designated.grant.schema_version, 6);
        assert_eq!(designated.grant, receipt.bound_grant().unwrap());
        assert_eq!(designated.operation_id, receipt.request.operation_id);
        assert_eq!(designated.grant_issued_at_unix_ms, receipt.issued_at_unix_ms);
        assert_eq!(
            designated.grant.expected_session_version,
            receipt.context.source_session.version
        );
        assert!(designated.dispatch.is_some());
        assert!(designated.continuation.is_some());

        let cut = event_sequence(&f, RECOVERY_EVENT, receipt.request.operation_id);
        let effect = event_sequence(&f, event, designated.operation_id);
        let membership = event_sequence(
            &f,
            "adaptive_work_funding_review_issued",
            receipt.request.operation_id,
        );
        assert!(membership < cut && effect > cut);
        let original_effect = event_contents_without_sequence(&f, effect);
        let original_membership = event_contents_without_sequence(&f, membership);
        let original_issuance = event_contents_without_sequence(&f, cut);

        // The designated call/adoption must obey the same cut as a later review.
        // Only sequence moves: every payload, seal, issuer and clock stays exact.
        swap_event_positions(&f, membership, effect);
        assert_eq!(event_sequence(&f, event, designated.operation_id), membership);
        assert_eq!(
            event_sequence(&f, "adaptive_work_funding_review_issued", receipt.request.operation_id),
            effect
        );
        assert_eq!(event_sequence(&f, RECOVERY_EVENT, receipt.request.operation_id), cut);
        assert_eq!(event_contents_without_sequence(&f, membership), original_effect);
        assert_eq!(event_contents_without_sequence(&f, effect), original_membership);
        assert_eq!(event_contents_without_sequence(&f, cut), original_issuance);
        assert_no_io_rejects(&f, &receipt);
        assert_corrupt(&f, &receipt);
    }
}

#[test]
fn recovery_history_cut_rejects_backdated_original_trace_appended_after_cut() {
    let (f, receipt, _, _) = recovered_then_followup_fixture();
    assert_readable(&f, &receipt);
    let old = original_authorized_trace(&f, &receipt);
    assert!(old.grant_issued_at_unix_ms < receipt.issued_at_unix_ms);
    assert_eq!(old.grant.expected_session_version, receipt.context.source_session.version);
    let cut = event_sequence(&f, RECOVERY_EVENT, receipt.request.operation_id);
    let sequence = append_adversarial_trace(&f, &old);
    assert!(sequence > stored_u64(cut).unwrap());
    assert_no_io_rejects(&f, &receipt);
    assert_corrupt(&f, &receipt);
}

#[test]
fn recovery_history_cut_rejects_postcut_trace_at_original_session_version_even_with_new_clock() {
    let (f, receipt, _, _) = recovered_then_followup_fixture();
    assert_readable(&f, &receipt);
    let mut old = original_authorized_trace(&f, &receipt);
    // Deliberately reseal just the observation with a non-backdated clock. Its
    // genuine old session is unchanged, so time alone cannot make it later work.
    old.grant.expires_at_unix_ms = receipt.grant_template.expires_at_unix_ms;
    old.grant_issued_at_unix_ms = receipt.issued_at_unix_ms;
    old.updated_at_unix_ms = receipt.issued_at_unix_ms;
    assert_eq!(old.grant.expected_session_version, receipt.context.source_session.version);
    assert!(old.grant_issued_at_unix_ms >= receipt.issued_at_unix_ms);
    let cut = event_sequence(&f, RECOVERY_EVENT, receipt.request.operation_id);
    let sequence = append_adversarial_trace(&f, &old);
    assert!(sequence > stored_u64(cut).unwrap());
    assert_no_io_rejects(&f, &receipt);
    assert_corrupt(&f, &receipt);
}

#[test]
fn recovery_history_cut_still_authenticates_later_rows_memberships_and_events() {
    for mutation in [
        "call-row",
        "membership",
        "event-payload",
        "earlier-later-event",
        "event-issuer",
        "event-storage-class",
    ] {
        let (f, receipt, followup, dispatch_at) = recovered_then_followup_fixture();
        f.store
            .claim_adaptive_leadership_review_call(&f.leader, &claim(&followup), dispatch_at)
            .unwrap();
        assert_readable(&f, &receipt);
        let external = Connection::open(&f.path).unwrap();
        let changed = match mutation {
            "call-row" => external.execute(
                "UPDATE company_entities SET payload_digest='corrupt'
                 WHERE entity_kind='adaptive_leadership_review_call' AND entity_id=?1",
                [followup.review_key.clone()],
            ),
            "membership" => external.execute(
                "UPDATE company_entities SET payload_digest='corrupt'
                 WHERE entity_kind='adaptive_work_funding_review'
                 AND json_extract(payload,'$.operation_id')=?1",
                [followup.operation_id.to_string()],
            ),
            "event-payload" => external.execute(
                "UPDATE company_events SET payload_digest='corrupt'
                 WHERE event_type=?1 AND operation_id=?2",
                params![DISPATCH_EVENT, followup.operation_id.to_string()],
            ),
            "earlier-later-event" => external.execute(
                "UPDATE company_events SET payload_digest='corrupt'
                 WHERE event_type='adaptive_leadership_review_authorized' AND operation_id=?1",
                [followup.operation_id.to_string()],
            ),
            "event-issuer" => external.execute(
                "UPDATE company_events SET principal_id='wrong-issuer'
                 WHERE event_type=?1 AND operation_id=?2",
                params![DISPATCH_EVENT, followup.operation_id.to_string()],
            ),
            _ => external.execute(
                "UPDATE company_events SET payload=CAST(payload AS TEXT)
                 WHERE event_type=?1 AND operation_id=?2",
                params![DISPATCH_EVENT, followup.operation_id.to_string()],
            ),
        };
        assert_eq!(changed.unwrap(), 1, "mutation={mutation}");
        assert_corrupt(&f, &receipt);
    }
}
