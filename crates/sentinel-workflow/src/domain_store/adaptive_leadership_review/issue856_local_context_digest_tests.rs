use super::*;
use std::cell::{Cell, RefCell};

fn funded_review(dispatched: bool) -> (Fixture, AdaptiveLeadershipReviewCallV1) {
    let mut f = budget_fixture(2);
    let now = CONTINUATION_AT + 7;
    let receipt = work_funding_tests::fund(&f, now);
    work_funding_tests::bind(
        &mut f,
        &receipt,
        receipt.request.source.resume_source.base_review_count + 1,
    );
    let call = if dispatched {
        work_funding_tests::dispatch(&f, now)
    } else {
        f.store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                Uuid::new_v4(),
                "local-digest-review",
                &f.grant,
                &f.context,
                now,
            )
            .unwrap()
    };
    (f, call)
}

fn counted_validation(
    connection: &Connection,
    call: &AdaptiveLeadershipReviewCallV1,
    combined: bool,
) -> (Result<(), WorkflowError>, usize) {
    let computations = Cell::new(0);
    let mut compute = || {
        computations.set(computations.get() + 1);
        call.context_digest()
    };
    let result = validation_scope::with_scope(connection, || {
        if combined {
            call.validate_full_with_context_digest(connection, &mut compute)
        } else {
            call.validate_entity_with_context_digest(&mut compute)?;
            call.validate_persisted_with_context_digest(connection, &mut compute)
        }
    });
    (result, computations.get())
}

#[test]
fn issue856_local_context_digest_full_funded_validation_hashes_once_without_wire_changes() {
    for dispatched in [false, true] {
        let (f, call) = funded_review(dispatched);
        let original = encode(&call).unwrap();
        let original_digest = call.context_digest().unwrap();
        let context_bytes =
            crate::digest::serialized_json_size(&(&call.grant, &call.context)).unwrap();
        assert!(context_bytes > 1024);
        let connection = f.store.connection.lock().unwrap();
        let (legacy, legacy_count) = counted_validation(&connection, &call, false);
        let (combined, combined_count) = counted_validation(&connection, &call, true);
        assert_eq!(combined, legacy);
        combined.unwrap();
        assert_eq!(legacy_count, if dispatched { 2 } else { 1 });
        assert_eq!(combined_count, 1);
        assert_eq!(counted_validation(&connection, &call, true), (Ok(()), 1));
        call.validate_full(&connection).unwrap();
        // The independent entrypoints remain complete without shared local state.
        call.validate_entity().unwrap();
        call.validate_persisted(&connection).unwrap();
        assert_eq!(encode(&call).unwrap(), original);
        assert_eq!(call.context_digest().unwrap(), original_digest);
        eprintln!(
            "LOCAL_CONTEXT_DIGEST dispatched={dispatched} legacy_calls={legacy_count} combined_calls={combined_count} canonical_tuple_bytes={context_bytes} saved_json_bytes={}",
            (legacy_count - combined_count) * context_bytes,
        );
        drop(connection);
        assert_eq!(
            f.store
                .adaptive_leadership_review_call(&f.leader.tenant_id, call.grant.review_id)
                .unwrap(),
            Some(call)
        );
    }
}

#[test]
fn issue856_local_context_digest_preserves_early_rejections_and_exact_context_binding() {
    let (f, call) = funded_review(true);
    let connection = f.store.connection.lock().unwrap();
    for mutation in 0..7 {
        let mut changed = call.clone();
        match mutation {
            0 => changed.schema_version = 0,
            1 => changed.allowance_id.clear(),
            2 => changed
                .dispatch
                .as_mut()
                .unwrap()
                .request_id
                .push_str("-wrong"),
            3 => changed.dispatch.as_mut().unwrap().context_digest = "invalid".into(),
            4 => {
                changed.dispatch.as_mut().unwrap().dispatched_at_unix_ms =
                    changed.grant.expires_at_unix_ms;
            }
            5 => {
                assert!(changed.context.evidence_refs.len() > 1);
                changed.context.evidence_refs.reverse();
                changed.context.validate(&changed.grant).unwrap();
                assert_ne!(
                    changed.context_digest().unwrap(),
                    call.context_digest().unwrap()
                );
            }
            _ => changed.grant.leadership_principal.authority_generation += 1,
        }
        let (legacy, legacy_count) = counted_validation(&connection, &changed, false);
        let (combined, combined_count) = counted_validation(&connection, &changed, true);
        assert_eq!(combined, legacy, "mutation {mutation}");
        assert!(combined.is_err());
        assert_eq!(combined_count, legacy_count);
        if mutation < 4 {
            assert_eq!(combined_count, 0, "digest must stay lazy");
        }
    }
    // Reordered evidence is valid shape and the same set, but not the saved exact context.
    let mut changed = call.clone();
    changed.context.evidence_refs.reverse();
    let changed_digest = changed.context_digest().unwrap();
    changed.dispatch.as_mut().unwrap().context_digest = changed_digest;
    changed.validate_entity().unwrap();
    let (legacy, legacy_count) = counted_validation(&connection, &changed, false);
    let (combined, combined_count) = counted_validation(&connection, &changed, true);
    assert_eq!(combined, legacy);
    assert!(
        combined.is_err(),
        "persisted membership must still bind the original context"
    );
    assert_eq!((legacy_count, combined_count), (2, 1));
    assert_eq!(counted_validation(&connection, &call, true), (Ok(()), 1));
}

#[test]
fn issue856_local_context_digest_does_not_hide_changed_membership_or_journal() {
    let (f, call) = funded_review(true);
    let mut connection = f.store.connection.lock().unwrap();
    for target in ["membership", "journal"] {
        assert_eq!(counted_validation(&connection, &call, true), (Ok(()), 1));
        let transaction = connection.transaction().unwrap();
        let changed = if target == "membership" {
            transaction
                .execute(
                    "DELETE FROM company_entities WHERE entity_kind='adaptive_work_funding_review'
                 AND json_extract(payload,'$.grant.review_id')=?1",
                    [call.grant.review_id.to_string()],
                )
                .unwrap()
        } else {
            transaction
                .execute(
                    "UPDATE workflow_operations SET request_digest='invalid'
                 WHERE operation_namespace=?1 AND operation_id='00000000000000000001'",
                    [format!("adaptive-session-v1:{}", call.grant.session_id)],
                )
                .unwrap()
        };
        assert!(changed > 0);
        let (legacy, _) = counted_validation(&transaction, &call, false);
        let (combined, count) = counted_validation(&transaction, &call, true);
        assert_eq!(combined, legacy, "{target}");
        assert!(combined.is_err(), "{target}");
        assert_eq!(count, 1);
        transaction.rollback().unwrap();
        assert_eq!(counted_validation(&connection, &call, true), (Ok(()), 1));
    }
    assert!(connection.is_autocommit());
}

#[test]
fn issue856_local_context_digest_preserves_full_proof_counts_and_byte_accounting() {
    let (f, call) = funded_review(true);
    let connection = f.store.connection.lock().unwrap();
    let mut observations = Vec::new();
    let mut charges = Vec::new();
    for combined in [false, true] {
        let (result, proofs) = validation_scope::with_completed_validations(|| {
            counted_validation(&connection, &call, combined).0
        });
        result.unwrap();
        assert_eq!(proofs.len(), 1);
        observations.push(proofs);
        let (result, failures) = validation_scope::with_validation_failures(|| {
            validation_scope::with_scope(&connection, || {
                counted_validation(&connection, &call, combined).0?;
                // Snapshot successful charges via the existing bounded failure metadata.
                validation_scope::charge_bytes(&connection, 64 * 1024 * 1024)
            })
        });
        assert!(result.is_err());
        assert_eq!(failures.len(), 1);
        assert_eq!(
            failures[0].reason,
            validation_scope::ValidationFailureReason::BytesLimit
        );
        assert_eq!(failures[0].attempted_bytes, Some(64 * 1024 * 1024));
        charges.push(failures);
    }
    assert_eq!(observations[0], observations[1]);
    assert_eq!(charges[0], charges[1]);
    assert!(connection.is_autocommit());
}

#[test]
fn issue856_local_context_digest_legacy_non_funded_validation_is_unchanged() {
    let f = fixture();
    let call = authorize(&f);
    let call = f
        .store
        .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), AUTHORIZED_AT + 1)
        .unwrap();
    let connection = f.store.connection.lock().unwrap();
    let (legacy, legacy_count) = counted_validation(&connection, &call, false);
    let (combined, combined_count) = counted_validation(&connection, &call, true);
    assert_eq!(combined, legacy);
    combined.unwrap();
    assert_eq!((legacy_count, combined_count), (1, 1));
}

#[test]
fn issue856_local_context_digest_default_full_validation_preserves_phase_order() {
    struct Ordered {
        phases: RefCell<Vec<&'static str>>,
        shape_error: bool,
    }
    impl CompanyEntity for Ordered {
        fn row_binding(&self) -> (&TenantId, &'static str, &str, u64) {
            panic!("not used by direct phase-order test")
        }
        fn validate_entity(&self) -> Result<(), WorkflowError> {
            self.phases.borrow_mut().push("shape");
            if self.shape_error {
                Err(invalid("shape first"))
            } else {
                Ok(())
            }
        }
        fn validate_persisted(&self, _: &Connection) -> Result<(), WorkflowError> {
            self.phases.borrow_mut().push("persisted");
            Err(not_found())
        }
    }
    let connection = Connection::open_in_memory().unwrap();
    for shape_error in [false, true] {
        let value = Ordered {
            phases: RefCell::new(Vec::new()),
            shape_error,
        };
        let error = value.validate_full(&connection).unwrap_err();
        assert_eq!(
            value.phases.into_inner(),
            if shape_error {
                vec!["shape"]
            } else {
                vec!["shape", "persisted"]
            }
        );
        assert_eq!(
            error,
            if shape_error {
                invalid("shape first")
            } else {
                not_found()
            }
        );
    }
}
