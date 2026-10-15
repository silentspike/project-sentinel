#![cfg(test)]

use super::*;

fn database() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch("CREATE TABLE records(value INTEGER); INSERT INTO records VALUES(1)")
        .unwrap();
    connection
}

fn retained_body(connection: &Connection, key: u64) -> Result<String, WorkflowError> {
    memoize_precharged(connection, "owned-body", &key, || {
        let token = precharge_body::<String>(connection, 4)?;
        Ok(("body".to_owned(), Some(token)))
    })
}

fn charged() -> usize {
    ACTIVE.with(|active| active.borrow().as_ref().unwrap().bytes)
}

#[test]
fn issue856_owned_body_handoff_charges_full_result_once_and_hits_are_free() {
    let connection = database();
    with_scope(&connection, || {
        let result = memoize_precharged(&connection, "owned-body", &0, || {
            let token = precharge_body::<String>(&connection, 4)?;
            assert_eq!(charged(), 257 + 4);
            charge_bytes(&connection, 11)?;
            memoize(&connection, "separate-proof", &0, || {
                charge_bytes(&connection, 7)?;
                Ok(42_u64)
            })?;
            assert_eq!(charged(), 257 + 4 + 11 + 257 + 7 + 2);
            Ok(("body".to_owned(), Some(token)))
        })?;
        assert_eq!(result, "body");
        let total = 257 + 6 + 11 + 257 + 7 + 2;
        assert_eq!(charged(), total);
        assert_eq!(
            memoize_precharged::<String>(&connection, "owned-body", &0, || {
                panic!("hit cannot copy or issue another token")
            })?,
            result
        );
        assert_eq!(charged(), total);
        ACTIVE.with(|active| {
            let active = active.borrow();
            let state = active.as_ref().unwrap();
            assert_eq!(state.charge_totals.key, 514);
            assert_eq!(state.charge_totals.body, 4 + 11 + 7);
            assert_eq!(state.charge_totals.result, 2 + 2);
            assert_eq!(state.charge_totals.bytes(), total);
            assert!(state.body_frames.is_empty());
        });
        Ok(())
    })
    .unwrap();
    // A new snapshot must validate and charge again, not reuse a prior token.
    assert_eq!(retained_body(&connection, 0).unwrap(), "body");
    assert!(ACTIVE.with(|active| active.borrow().is_none()));
}

#[test]
fn issue856_owned_body_handoff_exact_limit_and_pre_copy_one_byte_over() {
    let connection = database();
    with_scope(&connection, || {
        charge_bytes(&connection, MAX_BYTES - 257 - 6)?;
        assert_eq!(retained_body(&connection, 0)?, "body");
        assert_eq!(charged(), MAX_BYTES);
        assert_eq!(
            memoize_precharged::<String>(&connection, "owned-body", &0, || {
                panic!("exact-limit hit must not recharge")
            })?,
            "body"
        );
        assert_eq!(charged(), MAX_BYTES);
        Ok(())
    })
    .unwrap();

    let copied = Cell::new(false);
    let (result, failures) = with_validation_failures(|| {
        with_scope(&connection, || {
            charge_bytes(&connection, MAX_BYTES - 257 - 3)?;
            memoize_precharged(&connection, "owned-body", &0, || {
                let token = precharge_body::<String>(&connection, 4)?;
                copied.set(true);
                Ok(("body".to_owned(), Some(token)))
            })
        })
    });
    assert!(result.is_err());
    assert!(!copied.get());
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].reason, ValidationFailureReason::BytesLimit);
    assert_eq!(failures[0].charge_phase, Some(ValidationChargePhase::Body));
    assert_eq!(failures[0].bytes, MAX_BYTES - 3);
    assert_eq!(failures[0].attempted_bytes, Some(4));
}

#[test]
fn issue856_owned_body_handoff_result_delta_still_enforces_fixed_limit() {
    let connection = database();
    let (result, failures) = with_validation_failures(|| {
        with_scope(&connection, || {
            charge_bytes(&connection, MAX_BYTES - 257 - 4)?;
            retained_body(&connection, 0)
        })
    });
    assert!(result.is_err());
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].reason, ValidationFailureReason::BytesLimit);
    assert_eq!(
        failures[0].charge_phase,
        Some(ValidationChargePhase::Result)
    );
    assert_eq!(failures[0].bytes, MAX_BYTES);
    assert_eq!(failures[0].attempted_bytes, Some(2));
    assert_eq!(failures[0].charge_totals.bytes(), MAX_BYTES);
    assert!(ACTIVE.with(|active| active.borrow().is_none()));
    assert!(connection.is_autocommit());
}

#[test]
fn issue856_owned_body_handoff_no_token_and_larger_precharge_never_refund() {
    #[derive(Clone, Serialize)]
    struct CompactOwned {
        #[serde(skip)]
        body: String,
    }

    let connection = database();
    with_scope(&connection, || {
        let missing =
            memoize_precharged::<Option<String>>(&connection, "missing", &0, || Ok((None, None)))?;
        assert!(missing.is_none());
        assert_eq!(charged(), 257 + 4);
        let value = memoize_precharged(&connection, "compact-owned", &0, || {
            let token = precharge_body::<CompactOwned>(&connection, 16)?;
            Ok((
                CompactOwned {
                    body: "x".repeat(16),
                },
                Some(token),
            ))
        })?;
        assert_eq!(value.body.len(), 16);
        assert_eq!(charged(), 257 + 4 + 257 + 16);
        Ok(())
    })
    .unwrap();
}

#[test]
fn issue856_owned_body_handoff_missing_duplicate_type_and_nested_tokens_reject() {
    let connection = database();
    for case in ["missing", "duplicate", "wrong-type", "nested"] {
        let (result, failures) = with_validation_failures(|| {
            memoize_precharged::<String>(&connection, "owned-body", &0, || {
                if case == "wrong-type" {
                    let _ = precharge_body::<u64>(&connection, 4)?;
                    unreachable!("wrong result type must reject")
                }
                let token = precharge_body::<String>(&connection, 4)?;
                match case {
                    "missing" => {
                        drop(token);
                        Ok(("body".to_owned(), None))
                    }
                    "duplicate" => {
                        let _ = precharge_body::<String>(&connection, 4)?;
                        unreachable!("second precharge must reject")
                    }
                    "nested" => {
                        memoize_precharged(&connection, "nested-owner", &1, || {
                            Ok(("body".to_owned(), Some(token)))
                        })?;
                        unreachable!("another memo cannot consume this token")
                    }
                    _ => unreachable!(),
                }
            })
        });
        assert!(result.is_err(), "{case}");
        assert_eq!(
            failures[0].reason,
            ValidationFailureReason::InvalidBodyHandoff
        );
        assert!(ACTIVE.with(|active| active.borrow().is_none()));
        assert_eq!(retained_body(&connection, 0).unwrap(), "body");
    }
}

#[test]
fn issue856_owned_body_handoff_error_drop_and_retry_or_scope_exit_cannot_reuse_token() {
    let connection = database();
    for exit in [false, true] {
        let mut saved = None;
        let mut scope = Some(enter(&connection).unwrap());
        let result = memoize_precharged::<String>(&connection, "owned-body", &0, || {
            saved = Some(precharge_body::<String>(&connection, 4)?);
            Err(corrupt())
        });
        assert!(result.is_err());
        assert_eq!(charged(), 257 + 4);
        ACTIVE.with(|active| {
            let active = active.borrow();
            let state = active.as_ref().unwrap();
            assert!(state.entries.is_empty());
            assert!(state.body_frames.is_empty());
        });
        if exit {
            scope.take().unwrap().finish().unwrap();
        }
        let (result, failures) = with_validation_failures(|| {
            memoize_precharged::<String>(&connection, "owned-body", &0, || {
                Ok(("body".to_owned(), saved.take()))
            })
        });
        assert!(result.is_err());
        assert_eq!(
            failures[0].reason,
            ValidationFailureReason::InvalidBodyHandoff
        );
        drop(saved);
        if !exit {
            assert!(ACTIVE.with(|active| active.borrow().as_ref().unwrap().poisoned));
        }
        drop(scope);
        assert!(ACTIVE.with(|active| active.borrow().is_none()));
    }
    assert!(ACTIVE.with(|active| active.borrow().is_none()));
    assert_eq!(retained_body(&connection, 0).unwrap(), "body");
}

#[test]
fn issue856_owned_body_handoff_writes_connections_cycles_and_unwind_remain_fail_closed() {
    let connection = database();
    let other = database();
    for case in ["write", "announced-write", "connection", "cycle"] {
        let (result, failures) = with_validation_failures(|| {
            memoize_precharged::<String>(&connection, "owned-body", &0, || {
                let token = precharge_body::<String>(&connection, 4)?;
                match case {
                    "write" => {
                        connection.execute("UPDATE records SET value=2", [])?;
                    }
                    "announced-write" => before_write(&connection)?,
                    "connection" => {
                        let _ = precharge_body::<String>(&other, 4)?;
                    }
                    "cycle" => {
                        retained_body(&connection, 0)?;
                    }
                    _ => unreachable!(),
                }
                Ok(("body".to_owned(), Some(token)))
            })
        });
        assert!(result.is_err(), "{case}");
        assert_eq!(
            failures[0].reason,
            match case {
                "connection" => ValidationFailureReason::ConnectionMismatch,
                "cycle" => ValidationFailureReason::Visiting,
                _ => ValidationFailureReason::WriteDuringValidation,
            }
        );
        let value: i64 = connection
            .query_row("SELECT value FROM records", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, 1);
        assert!(ACTIVE.with(|active| active.borrow().is_none()));
    }

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = memoize_precharged::<String>(&connection, "owned-body", &0, || {
            let _token = precharge_body::<String>(&connection, 4)?;
            panic!("handoff unwind cleanup")
        });
    }));
    assert!(panic.is_err());
    assert!(ACTIVE.with(|active| active.borrow().is_none()));
    assert!(connection.is_autocommit());
    assert_eq!(retained_body(&connection, 0).unwrap(), "body");
}
