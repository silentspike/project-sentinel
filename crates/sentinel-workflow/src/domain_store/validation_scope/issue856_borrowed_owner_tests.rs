#![cfg(test)]

use super::*;
use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Owner {
    text: String,
    #[serde(default)]
    extra: String,
}

fn database(raw: &[u8]) -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch("CREATE TABLE source(id INTEGER PRIMARY KEY, payload)")
        .unwrap();
    connection
        .execute("INSERT INTO source VALUES(1,?1)", [raw])
        .unwrap();
    connection
}

fn with_blob<T>(
    connection: &Connection,
    action: impl FnOnce(&[u8]) -> Result<T, WorkflowError>,
) -> Result<Option<T>, WorkflowError> {
    let mut statement = connection.prepare("SELECT payload FROM source WHERE id=1")?;
    let mut rows = statement.query([])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let rusqlite::types::ValueRef::Blob(raw) = row.get_ref(0)? else {
        return Err(corrupt());
    };
    action(raw).map(Some)
}

fn read_owner(
    connection: &Connection,
    decoded: &Cell<usize>,
) -> Result<Option<Owner>, WorkflowError> {
    memoize_borrowed_sqlite_decoded(connection, "borrowed-owner", &0_u64, || {
        let value = with_blob(connection, |raw| {
            let token = precharge_borrowed_sqlite_decoded::<Option<Owner>>(connection, raw)?;
            decoded.set(decoded.get() + 1);
            let owner = serde_json::from_slice::<Owner>(raw)?;
            Ok((owner, token))
        })?;
        Ok(match value {
            Some((owner, token)) => (Some(owner), Some(token)),
            None => (None, None),
        })
    })
}

#[test]
fn borrowed_owner_exact_boundary_preserves_full_raw_or_result_and_warm_hits() {
    for raw in [
        br#"{"text":"a"}"#.as_slice(),
        b" \n {\"text\":\"\\u00e4\\ud83d\\ude00\\n\",\"extra\":\"\"} \t ".as_slice(),
    ] {
        let expected: Owner = serde_json::from_slice(raw).unwrap();
        let result_bytes = serde_json::to_vec(&Some(expected.clone())).unwrap().len();
        let key_bytes = serde_json::to_vec(&0_u64).unwrap().len() + 256;
        assert_ne!(raw.len(), result_bytes);
        let connection = database(raw);
        let decoded = Cell::new(0);
        let before = changes(&connection).unwrap();
        let (result, failures) = with_validation_failures(|| {
            with_scope(&connection, || {
                charge_bytes(
                    &connection,
                    MAX_BYTES - key_bytes - raw.len().max(result_bytes),
                )?;
                assert_eq!(read_owner(&connection, &decoded)?, Some(expected.clone()));
                assert_eq!(read_owner(&connection, &decoded)?, Some(expected.clone()));
                assert_eq!(decoded.get(), 1);
                charge_bytes(&connection, 1)
            })
        });
        assert!(result.is_err());
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].reason, ValidationFailureReason::BytesLimit);
        assert_eq!(failures[0].bytes, MAX_BYTES);
        assert_eq!(failures[0].attempted_bytes, Some(1));
        assert_eq!(changes(&connection).unwrap(), before);
        assert!(connection.is_autocommit());
        assert_eq!(read_owner(&connection, &decoded).unwrap(), Some(expected));
        assert_eq!(decoded.get(), 2, "a new scope must decode again");
    }
}

#[test]
fn borrowed_owner_actual_sqlite_blob_over_limit_rejects_before_decode() {
    let connection = database(b"{}");
    connection
        .execute(
            "UPDATE source SET payload=zeroblob(?1)",
            [(MAX_BYTES + 1) as i64],
        )
        .unwrap();
    let decoded = Cell::new(0);
    let (result, failures) = with_validation_failures(|| read_owner(&connection, &decoded));
    assert!(result.is_err());
    assert_eq!(
        decoded.get(),
        0,
        "the real BLOB must be admitted before deserialization"
    );
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].reason, ValidationFailureReason::BytesLimit);
    assert_eq!(failures[0].charge_phase, Some(ValidationChargePhase::Body));
    assert_eq!(failures[0].attempted_bytes, Some(MAX_BYTES + 1));
    assert!(connection.is_autocommit());
    assert!(ACTIVE.with(|active| active.borrow().is_none()));
}

#[test]
fn borrowed_owner_complete_result_remainder_one_byte_over_still_rejects() {
    let raw = br#"{"text":"a"}"#;
    let expected: Owner = serde_json::from_slice(raw).unwrap();
    let result_bytes = serde_json::to_vec(&Some(expected)).unwrap().len();
    assert!(result_bytes > raw.len());
    let key_bytes = serde_json::to_vec(&0_u64).unwrap().len() + 256;
    let connection = database(raw);
    let decoded = Cell::new(0);
    let (result, failures) = with_validation_failures(|| {
        with_scope(&connection, || {
            charge_bytes(&connection, MAX_BYTES - key_bytes - result_bytes + 1)?;
            read_owner(&connection, &decoded)
        })
    });
    assert!(result.is_err());
    assert_eq!(decoded.get(), 1);
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].reason, ValidationFailureReason::BytesLimit);
    assert_eq!(
        failures[0].charge_phase,
        Some(ValidationChargePhase::Result)
    );
    assert_eq!(failures[0].attempted_bytes, Some(result_bytes - raw.len()));
    assert_eq!(
        failures[0].bytes + failures[0].attempted_bytes.unwrap(),
        MAX_BYTES + 1
    );
    assert!(ACTIVE.with(|active| active.borrow().is_none()));
}

#[test]
fn owned_raw_decode_keeps_ordinary_additive_accounting_and_cannot_cross_modes() {
    let raw = br#"{"text":"owned","extra":""}"#;
    let connection = database(raw);
    let expected: Owner = serde_json::from_slice(raw).unwrap();
    let result_bytes = serde_json::to_vec(&Some(expected)).unwrap().len();
    let key_bytes = serde_json::to_vec(&0_u64).unwrap().len() + 256;
    let copied = Cell::new(0);
    let (result, failures) = with_validation_failures(|| {
        with_scope(&connection, || {
            charge_bytes(
                &connection,
                MAX_BYTES - key_bytes - raw.len().max(result_bytes),
            )?;
            memoize(&connection, "owned-raw-decode", &0_u64, || {
                with_blob(&connection, |borrowed| {
                    charge_bytes(&connection, borrowed.len())?;
                    let owned_raw = borrowed.to_vec();
                    copied.set(copied.get() + 1);
                    Ok(serde_json::from_slice::<Owner>(&owned_raw)?)
                })
            })
        })
    });
    assert!(
        result.is_err(),
        "owned raw/decoded overlap cannot obtain the new discount"
    );
    assert_eq!(copied.get(), 1);
    assert_eq!(failures[0].reason, ValidationFailureReason::BytesLimit);
    assert_eq!(
        failures[0].charge_phase,
        Some(ValidationChargePhase::Result)
    );
    for old_frame in [false, true] {
        let (result, failures) = with_validation_failures(|| {
            if old_frame {
                memoize_precharged::<Option<Owner>>(&connection, "old-frame", &0, || {
                    with_blob(&connection, |blob| {
                        let _ =
                            precharge_borrowed_sqlite_decoded::<Option<Owner>>(&connection, blob)?;
                        Ok(())
                    })?;
                    unreachable!("borrowed token cannot be issued in an owned frame")
                })
            } else {
                memoize_borrowed_sqlite_decoded::<Option<Owner>>(
                    &connection,
                    "new-frame",
                    &0,
                    || {
                        let _ = precharge_body::<Option<Owner>>(&connection, raw.len())?;
                        unreachable!("owned token cannot be issued in a borrowed frame")
                    },
                )
            }
        });
        assert!(result.is_err());
        assert_eq!(
            failures[0].reason,
            ValidationFailureReason::InvalidBodyHandoff
        );
        assert!(connection.is_autocommit());
    }
}

#[test]
fn borrowed_owner_missing_non_blob_and_malformed_sources_do_not_publish_proofs() {
    for source in ["missing", "text", "malformed"] {
        let connection = database(b"not-json");
        match source {
            "missing" => {
                connection.execute("DELETE FROM source", []).unwrap();
            }
            "text" => {
                connection
                    .execute("UPDATE source SET payload='{}'", [])
                    .unwrap();
            }
            _ => {}
        }
        let decoded = Cell::new(0);
        let result = read_owner(&connection, &decoded);
        if source == "missing" {
            assert_eq!(result.unwrap(), None);
        } else {
            assert!(result.is_err());
        }
        assert_eq!(decoded.get(), usize::from(source == "malformed"));
        assert!(ACTIVE.with(|active| active.borrow().is_none()));
        connection
            .execute(
                "INSERT OR REPLACE INTO source VALUES(1,?1)",
                [br#"{"text":"repaired"}"#.as_slice()],
            )
            .unwrap();
        assert_eq!(
            read_owner(&connection, &decoded).unwrap().unwrap().text,
            "repaired"
        );
    }
}

#[test]
fn borrowed_owner_rejects_missing_duplicate_type_depth_and_frame_tokens() {
    let connection = database(br#"{"text":"a"}"#);
    for case in ["missing", "duplicate", "type", "depth", "frame"] {
        let (result, failures) = with_validation_failures(|| {
            memoize_borrowed_sqlite_decoded::<Option<Owner>>(
                &connection,
                "token-misuse",
                &0,
                || {
                    with_blob(&connection, |raw| {
                        if case == "type" {
                            let _ = precharge_borrowed_sqlite_decoded::<u64>(&connection, raw)?;
                            unreachable!("wrong result type must reject")
                        }
                        if case == "depth" {
                            memoize(&connection, "deeper-miss", &1, || {
                                let _ = precharge_borrowed_sqlite_decoded::<Option<Owner>>(
                                    &connection,
                                    raw,
                                )?;
                                Ok(())
                            })?;
                            unreachable!("a nested ordinary miss cannot borrow its parent's frame")
                        }
                        let token =
                            precharge_borrowed_sqlite_decoded::<Option<Owner>>(&connection, raw)?;
                        match case {
                            "missing" => {
                                drop(token);
                                Ok((None, None))
                            }
                            "duplicate" => {
                                let _ = precharge_borrowed_sqlite_decoded::<Option<Owner>>(
                                    &connection,
                                    raw,
                                )?;
                                unreachable!("a second precharge must reject")
                            }
                            "frame" => {
                                memoize_borrowed_sqlite_decoded(
                                    &connection,
                                    "other-frame",
                                    &1,
                                    || Ok((None::<Owner>, Some(token))),
                                )?;
                                unreachable!("another miss cannot consume this token")
                            }
                            _ => unreachable!(),
                        }
                    })?
                    .ok_or_else(corrupt)
                },
            )
        });
        assert!(result.is_err(), "{case}");
        assert_eq!(
            failures[0].reason,
            ValidationFailureReason::InvalidBodyHandoff,
            "{case}"
        );
        assert!(connection.is_autocommit());
        assert!(ACTIVE.with(|active| active.borrow().is_none()));
        assert!(read_owner(&connection, &Cell::new(0)).unwrap().is_some());
    }
}

#[test]
fn borrowed_owner_tokens_cannot_survive_error_retry_or_scope_exit() {
    let connection = database(br#"{"text":"a"}"#);
    for exit_scope in [false, true] {
        let mut saved = None;
        let mut scope = Some(enter(&connection).unwrap());
        assert!(memoize_borrowed_sqlite_decoded::<Option<Owner>>(
            &connection,
            "save-token",
            &0,
            || {
                saved = with_blob(&connection, |raw| {
                    precharge_borrowed_sqlite_decoded::<Option<Owner>>(&connection, raw)
                })?;
                Err(corrupt())
            }
        )
        .is_err());
        if exit_scope {
            scope.take().unwrap().finish().unwrap();
        }
        let (result, failures) = with_validation_failures(|| {
            memoize_borrowed_sqlite_decoded(&connection, "save-token", &0, || {
                Ok((None::<Owner>, saved.take()))
            })
        });
        assert!(result.is_err());
        assert_eq!(
            failures[0].reason,
            ValidationFailureReason::InvalidBodyHandoff
        );
        drop(scope);
        assert!(ACTIVE.with(|active| active.borrow().is_none()));
    }
}

#[test]
fn borrowed_owner_write_connection_poison_and_unwind_fail_closed_and_roll_back() {
    let connection = database(br#"{"text":"a"}"#);
    let other = database(br#"{"text":"b"}"#);
    for case in ["write", "announced-write", "connection", "poison"] {
        let (result, failures) = with_validation_failures(|| {
            memoize_borrowed_sqlite_decoded::<Option<Owner>>(
                &connection,
                "mutated-owner",
                &0,
                || {
                    let (owner, token) = with_blob(&connection, |raw| {
                        let token =
                            precharge_borrowed_sqlite_decoded::<Option<Owner>>(&connection, raw)?;
                        Ok((serde_json::from_slice::<Owner>(raw)?, token))
                    })?
                    .ok_or_else(corrupt)?;
                    match case {
                        "write" => {
                            connection.execute("UPDATE source SET payload=x'00'", [])?;
                        }
                        "announced-write" => before_write(&connection)?,
                        "connection" => {
                            let _ = with_blob(&other, |raw| {
                                precharge_borrowed_sqlite_decoded::<Option<Owner>>(&other, raw)
                            })?;
                        }
                        "poison" => ACTIVE
                            .with(|active| active.borrow_mut().as_mut().unwrap().poisoned = true),
                        _ => unreachable!(),
                    }
                    Ok((Some(owner), Some(token)))
                },
            )
        });
        assert!(result.is_err(), "{case}");
        assert_eq!(
            failures[0].reason,
            match case {
                "connection" => ValidationFailureReason::ConnectionMismatch,
                "poison" => ValidationFailureReason::Poisoned,
                _ => ValidationFailureReason::WriteDuringValidation,
            },
            "{case}"
        );
        assert!(connection.is_autocommit());
        assert!(
            read_owner(&connection, &Cell::new(0)).unwrap().is_some(),
            "write must roll back"
        );
    }
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = memoize_borrowed_sqlite_decoded::<Option<Owner>>(
            &connection,
            "unwind-owner",
            &0,
            || {
                with_blob::<()>(&connection, |raw| {
                    let _token =
                        precharge_borrowed_sqlite_decoded::<Option<Owner>>(&connection, raw)?;
                    panic!("injected borrowed owner unwind")
                })?;
                unreachable!()
            },
        );
    }));
    assert!(panic.is_err());
    assert!(connection.is_autocommit());
    assert!(ACTIVE.with(|active| active.borrow().is_none()));
    assert!(read_owner(&connection, &Cell::new(0)).unwrap().is_some());
}
