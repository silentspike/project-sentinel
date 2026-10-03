#![cfg(test)]

// Store fixtures only; these tests are not live provider or leadership evidence.
use super::*;
use crate::adaptive::continuation_tests::{authorization, effect, grant, NOW};
use crate::{AdaptiveCursorV1, AdaptiveModelAdmissionV1, AdaptiveObservationRefV1};
use sentinel_common::WorkbenchTool;

const OPERATION: u128 = 900;

fn decision(tool: WorkbenchTool) -> AdaptiveModelDecisionV1 {
    AdaptiveModelDecisionV1::Tool {
        tool_digest: crate::adaptive_tool_digest(&tool).unwrap(),
        tool,
    }
}

fn write_decision() -> AdaptiveModelDecisionV1 {
    decision(WorkbenchTool::WriteFile {
        path: "main.py".into(),
        content: "print('retained original completion')\n".into(),
        expected_sha256: None,
    })
}

fn advance(
    store: &WorkflowStore,
    source: &AdaptiveSessionV1,
    command: AdaptiveTransitionV1,
    now: u64,
) -> AdaptiveSessionV1 {
    store
        .advance_adaptive_session(
            source.grant.session_id,
            source.version,
            Uuid::from_u128(10_000 + u128::from(source.version)),
            &command,
            &source.grant.authority,
            now,
        )
        .unwrap()
        .1
}

fn fixture(store: &WorkflowStore, additional: Option<u16>) -> AdaptiveSessionV1 {
    let root = grant();
    let mut session = store
        .begin_adaptive_session(&root, &root.authority, NOW)
        .unwrap()
        .1;
    for index in 0..2 {
        let model = effect(300 + index * 2);
        let tool_effect = effect(301 + index * 2);
        let tool = WorkbenchTool::InspectFile {
            path: "main.py".into(),
            max_bytes: 1024,
        };
        let tool_digest = crate::adaptive_tool_digest(&tool).unwrap();
        let commands = [
            AdaptiveTransitionV1::ClaimModel {
                effect: model.clone(),
                previous_observation_digest: session
                    .last_observation
                    .as_ref()
                    .map(|observation| observation.observation_digest.clone()),
            },
            AdaptiveTransitionV1::ResolveModel {
                effect: model,
                result_digest: "a".repeat(64),
                decision: decision(tool),
            },
            AdaptiveTransitionV1::ClaimTool {
                effect: tool_effect.clone(),
                tool_digest,
            },
            AdaptiveTransitionV1::ObserveTool {
                observation: AdaptiveObservationRefV1 {
                    effect: tool_effect,
                    observation_digest: "b".repeat(64),
                },
            },
        ];
        for command in commands {
            session = advance(store, &session, command, NOW + 1);
        }
    }
    if let Some(additional) = additional {
        let previous_observation_digest = session
            .last_observation
            .as_ref()
            .map(|observation| observation.observation_digest.clone());
        session = advance(
            store,
            &session,
            AdaptiveTransitionV1::ClaimModel {
                effect: effect(102),
                previous_observation_digest,
            },
            NOW + 2,
        );
        session = advance(
            store,
            &session,
            AdaptiveTransitionV1::MarkUnknown {
                effect: effect(102),
            },
            NOW + 3,
        );
        let mut auth = authorization(&session);
        auth.additional_model_calls = additional;
        let command = AdaptiveTransitionV1::ContinueGoverned {
            authorization: auth.clone(),
        };
        let continued = session.transition(&command, auth.issued_at_ms).unwrap();
        // Seed a valid historical continuation fact, not a live leadership grant.
        let mut connection = store.lock().unwrap();
        let tx = immediate(&mut connection).unwrap();
        let (_, prior_digest) = load(&tx, root.session_id).unwrap().unwrap();
        append(
            &tx,
            &namespace(root.session_id),
            &Entry {
                previous_digest: Some(prior_digest),
                command: Some(command),
                session: continued.clone(),
                recovery_feedback: None,
            },
        )
        .unwrap();
        update_head(&tx, &session, &continued).unwrap();
        tx.commit().unwrap();
        session = continued;
    }
    let previous_observation_digest = session
        .last_observation
        .as_ref()
        .map(|observation| observation.observation_digest.clone());
    let now = session.updated_at_ms + 1;
    advance(
        store,
        &session,
        AdaptiveTransitionV1::ClaimModel {
            effect: effect(401),
            previous_observation_digest,
        },
        now,
    )
}

fn receipt(store: &WorkflowStore, source: &AdaptiveSessionV1) -> AdaptiveRejectedModelReceiptV1 {
    let (_, digest) = store
        .adaptive_pending_model_head_evidence(
            source.grant.session_id,
            source.version,
            &effect(401),
            &source.grant.authority,
        )
        .unwrap()
        .unwrap();
    let AdaptiveModelDecisionV1::Tool { tool_digest, .. } = write_decision() else {
        unreachable!()
    };
    AdaptiveRejectedModelReceiptV1 {
        schema_version: 1,
        session_id: source.grant.session_id,
        source_session_version: source.version,
        source_entry_digest: digest,
        effect: effect(401),
        resolution_event_id: Uuid::from_u128(902),
        reason_code: "fresh_observation_required".into(),
        reservation_digest: "c".repeat(64),
        authority_binding_digest: "d".repeat(64),
        completion_payload_digest: "e".repeat(64),
        model_response_digest: "f".repeat(64),
        context_digest: "1".repeat(64),
        tool_digest,
        usage_event_id: Uuid::from_u128(903).to_string(),
        usage_event_digest: "2".repeat(64),
    }
}

fn dispose(
    store: &WorkflowStore,
    source: &AdaptiveSessionV1,
    proof: &AdaptiveRejectedModelReceiptV1,
    now: u64,
) -> (bool, AdaptiveSessionV1) {
    store
        .dispose_rejected_adaptive_model(
            Uuid::from_u128(OPERATION),
            proof,
            &source.grant.authority,
            || now,
            |pending, digest, checked| {
                assert_eq!(pending, source);
                assert_eq!(digest, proof.source_entry_digest);
                assert_eq!(checked, proof);
                Ok(write_decision())
            },
        )
        .unwrap()
}

fn snapshot(store: &WorkflowStore) -> Vec<u8> {
    let connection = store.lock().unwrap();
    let mut statement = connection.prepare(
        "SELECT operation_namespace,operation_id,request_digest,response,created_at_ms FROM workflow_operations ORDER BY operation_namespace,operation_id",
    ).unwrap();
    let operations = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Vec<u8>>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let mut statement = connection.prepare(
        "SELECT tenant_id,project_id,work_item_id,agent_id,authority_digest,session_id,version,updated_at_ms FROM workflow_adaptive_heads ORDER BY tenant_id,project_id,work_item_id,agent_id,authority_digest",
    ).unwrap();
    let heads = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    serde_json::to_vec(&(operations, heads)).unwrap()
}

#[test]
fn disposition_preserves_exact_four_of_five_state_and_retained_evidence() {
    let store = WorkflowStore::open(":memory:").unwrap();
    let source = fixture(&store, Some(2));
    let proof = receipt(&store, &source);
    assert_eq!(source.model_calls, 4);
    assert_eq!(source.active_model_ceiling(), 5);
    assert!(source.last_observation.is_some());
    assert!(source.last_model_result_digest.is_some());
    assert!(source.requires_fresh_observation());
    let now = NOW + 1_002;
    let (replayed, resumed) = dispose(&store, &source, &proof, now);
    assert!(!replayed);
    let mut expected = source.clone();
    expected.version += 2;
    expected.updated_at_ms = now;
    expected.cursor = AdaptiveCursorV1::ReadyForModel;
    assert_eq!(resumed, expected);
    assert_eq!(
        resumed.model_admission_at(now),
        AdaptiveModelAdmissionV1::Admissible
    );
    assert_eq!(
        store
            .adaptive_session(source.grant.session_id, &source.grant.authority)
            .unwrap(),
        Some(resumed.clone())
    );
    let connection = store.lock().unwrap();
    let (_, rejected) = evidence_entry(
        &connection,
        &namespace(proof.session_id),
        source.version + 1,
    )
    .unwrap();
    assert!(matches!(
        rejected.command,
        Some(AdaptiveTransitionV1::RejectModel { .. })
    ));
    assert_eq!(rejected.session.model_calls, 4);
    let recorded =
        read_rejected_model_disposition(&connection, proof.session_id, Uuid::from_u128(OPERATION))
            .unwrap()
            .unwrap();
    assert_eq!(recorded.receipt, proof);
    assert_eq!(recorded.response, resumed);
    let mut statement = connection
        .prepare(
            "SELECT response FROM workflow_operations ORDER BY operation_namespace,operation_id",
        )
        .unwrap();
    let payloads = statement
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(payloads
        .iter()
        .all(|bytes| !String::from_utf8_lossy(bytes).contains("retained original completion")));
    drop(statement);
    drop(connection);
    assert!(!store
        .adaptive_model_result_is_adopted(
            &resumed.effective_grant(),
            &proof.effect,
            &proof.model_response_digest,
            &write_decision(),
        )
        .unwrap());
}

#[test]
fn expired_or_exhausted_disposition_does_not_renew_admission() {
    for (additional, now, admission) in [
        (2, NOW + 11_000, AdaptiveModelAdmissionV1::DeadlineExpired),
        (1, NOW + 1_002, AdaptiveModelAdmissionV1::CallsExhausted),
    ] {
        let store = WorkflowStore::open(":memory:").unwrap();
        let source = fixture(&store, Some(additional));
        let proof = receipt(&store, &source);
        let (_, resumed) = dispose(&store, &source, &proof, now);
        assert_eq!(resumed.grant, source.grant);
        assert_eq!(resumed.continuation, source.continuation);
        assert_eq!(resumed.model_calls, source.model_calls);
        assert_eq!(resumed.model_admission_at(now), admission);
        assert!(resumed.model_window_exhausted_at(now));
        let before = snapshot(&store);
        assert!(store
            .advance_adaptive_session(
                proof.session_id,
                resumed.version,
                Uuid::from_u128(904),
                &AdaptiveTransitionV1::ClaimModel {
                    effect: effect(402),
                    previous_observation_digest: Some("b".repeat(64)),
                },
                &source.grant.authority,
                now,
            )
            .is_err());
        assert_eq!(snapshot(&store), before);
    }
}

#[test]
fn only_new_inspection_spends_fifth_call_and_clears_fresh_flag() {
    let store = WorkflowStore::open(":memory:").unwrap();
    let source = fixture(&store, Some(2));
    let proof = receipt(&store, &source);
    let (_, ready) = dispose(&store, &source, &proof, NOW + 1_002);
    assert!(ready
        .transition(
            &AdaptiveTransitionV1::ClaimModel {
                effect: proof.effect.clone(),
                previous_observation_digest: Some("b".repeat(64)),
            },
            NOW + 1_003
        )
        .is_err());
    let pending = advance(
        &store,
        &ready,
        AdaptiveTransitionV1::ClaimModel {
            effect: effect(402),
            previous_observation_digest: Some("b".repeat(64)),
        },
        NOW + 1_003,
    );
    assert_eq!(pending.model_calls, 5);
    assert!(pending
        .transition(
            &AdaptiveTransitionV1::ResolveModel {
                effect: effect(402),
                result_digest: "a".repeat(64),
                decision: write_decision(),
            },
            NOW + 1_004
        )
        .is_err());
    let tool = WorkbenchTool::InspectFile {
        path: "main.py".into(),
        max_bytes: 1024,
    };
    let tool_digest = crate::adaptive_tool_digest(&tool).unwrap();
    let ready_tool = advance(
        &store,
        &pending,
        AdaptiveTransitionV1::ResolveModel {
            effect: effect(402),
            result_digest: "a".repeat(64),
            decision: decision(tool),
        },
        NOW + 1_004,
    );
    assert!(ready_tool.requires_fresh_observation());
    let tool_pending = advance(
        &store,
        &ready_tool,
        AdaptiveTransitionV1::ClaimTool {
            effect: effect(403),
            tool_digest,
        },
        NOW + 1_005,
    );
    assert!(tool_pending.requires_fresh_observation());
    let observed = advance(
        &store,
        &tool_pending,
        AdaptiveTransitionV1::ObserveTool {
            observation: AdaptiveObservationRefV1 {
                effect: effect(403),
                observation_digest: "c".repeat(64),
            },
        },
        NOW + 1_006,
    );
    assert!(!observed.requires_fresh_observation());
    assert_eq!(observed.model_calls, 5);
    assert_eq!(
        observed.model_admission_at(NOW + 1_006),
        AdaptiveModelAdmissionV1::CallsExhausted
    );
}

#[test]
fn direct_resume_is_denied_even_for_recorded_phase_replay() {
    let store = WorkflowStore::open(":memory:").unwrap();
    let source = fixture(&store, Some(2));
    let proof = receipt(&store, &source);
    let op = Uuid::from_u128(OPERATION);
    let (_, command) = rejected_model_commands(op, &proof).unwrap();
    for committed in [false, true] {
        if committed {
            dispose(&store, &source, &proof, NOW + 1_002);
        }
        let before = snapshot(&store);
        assert!(store
            .advance_adaptive_session_with_clock(
                proof.session_id,
                source.version + 1,
                rejected_model_resume_operation_id(op),
                &command,
                &source.grant.authority,
                || panic!("direct resume must not sample time"),
            )
            .is_err());
        assert_eq!(snapshot(&store), before);
    }
}

#[test]
fn standalone_rejection_and_malformed_resume_cannot_bypass_receipt_path() {
    let store = WorkflowStore::open(":memory:").unwrap();
    let source = fixture(&store, Some(2));
    let proof = receipt(&store, &source);
    let (reject, resume) = rejected_model_commands(Uuid::from_u128(OPERATION), &proof).unwrap();
    let rejected = advance(&store, &source, reject, NOW + 1_002);
    for mode in 0..4 {
        let mut command = resume.clone();
        let AdaptiveTransitionV1::ResumeRejectedModel {
            disposition_operation_id,
            expected_reason_code,
            resolution_event_id,
            receipt_digest,
        } = &mut command
        else {
            unreachable!()
        };
        match mode {
            0 => *disposition_operation_id = Uuid::nil(),
            1 => *expected_reason_code = "unknown".into(),
            2 => *resolution_event_id = Uuid::from_u128(999).to_string(),
            _ => *receipt_digest = "not-a-digest".into(),
        }
        assert!(rejected.transition(&command, NOW + 1_003).is_err());
    }
    let before = snapshot(&store);
    assert!(store
        .dispose_rejected_adaptive_model(
            Uuid::from_u128(OPERATION),
            &proof,
            &source.grant.authority,
            || panic!("standalone rejection must not sample time"),
            |_, _, _| panic!("standalone rejection must not verify"),
        )
        .is_err());
    assert_eq!(snapshot(&store), before);
}

#[test]
fn nil_operation_and_backwards_clock_leave_no_disposition() {
    let store = WorkflowStore::open(":memory:").unwrap();
    let source = fixture(&store, Some(2));
    let proof = receipt(&store, &source);
    let before = snapshot(&store);
    assert!(store
        .dispose_rejected_adaptive_model(
            Uuid::nil(),
            &proof,
            &source.grant.authority,
            || panic!("nil operation must not sample time"),
            |_, _, _| panic!("nil operation must not verify"),
        )
        .is_err());
    assert!(store
        .dispose_rejected_adaptive_model(
            Uuid::from_u128(OPERATION),
            &proof,
            &source.grant.authority,
            || source.updated_at_ms - 1,
            |_, _, _| Ok(write_decision()),
        )
        .is_err());
    assert_eq!(snapshot(&store), before);
}

#[test]
fn source_guard_rejects_false_flag_unknown_and_stale_receipts_before_verifier() {
    for mode in 0..7 {
        let store = WorkflowStore::open(":memory:").unwrap();
        let source = fixture(&store, if mode == 0 { None } else { Some(2) });
        let mut proof = receipt(&store, &source);
        match mode {
            1 => proof.source_session_version -= 1,
            2 => proof.source_entry_digest = "0".repeat(64),
            3 => proof.effect.id = effect(999).id,
            4 => proof.effect.request_digest = "0".repeat(64),
            5 => proof.effect = effect(102),
            6 => {
                advance(
                    &store,
                    &source,
                    AdaptiveTransitionV1::MarkUnknown {
                        effect: proof.effect.clone(),
                    },
                    NOW + 1_002,
                );
            }
            _ => {}
        }
        let before = snapshot(&store);
        assert!(store
            .dispose_rejected_adaptive_model(
                Uuid::from_u128(OPERATION),
                &proof,
                &source.grant.authority,
                || panic!("source mismatch must not sample time"),
                |_, _, _| panic!("source mismatch must not verify"),
            )
            .is_err());
        assert_eq!(snapshot(&store), before);
    }
}

#[test]
fn verifier_failure_and_nonwrite_or_wrong_digest_are_atomic() {
    for mode in 0..5 {
        let store = WorkflowStore::open(":memory:").unwrap();
        let source = fixture(&store, Some(2));
        let proof = receipt(&store, &source);
        let before = snapshot(&store);
        assert!(store
            .dispose_rejected_adaptive_model(
                Uuid::from_u128(OPERATION),
                &proof,
                &source.grant.authority,
                || panic!("invalid verified decision must not sample time"),
                |_, _, _| match mode {
                    0 => Err(authority_conflict()),
                    1 => Ok(AdaptiveModelDecisionV1::Blocked {
                        reason_code: "fixture".into()
                    }),
                    2 => Ok(decision(WorkbenchTool::InspectFile {
                        path: "main.py".into(),
                        max_bytes: 1024
                    })),
                    3 => Ok(AdaptiveModelDecisionV1::Tool {
                        tool: WorkbenchTool::WriteFile {
                            path: "main.py".into(),
                            content: "different".into(),
                            expected_sha256: None
                        },
                        tool_digest: proof.tool_digest.clone(),
                    }),
                    _ => Ok(AdaptiveModelDecisionV1::Tool {
                        tool: WorkbenchTool::WriteFile {
                            path: "main.py".into(),
                            content: "original".into(),
                            expected_sha256: None
                        },
                        tool_digest: "0".repeat(64),
                    }),
                },
            )
            .is_err());
        assert_eq!(snapshot(&store), before);
    }
}

fn mixed_receipts(proof: &AdaptiveRejectedModelReceiptV1) -> Vec<AdaptiveRejectedModelReceiptV1> {
    let mut variants = Vec::new();
    for index in 0..8 {
        let mut changed = proof.clone();
        match index {
            0 => changed.reservation_digest = "0".repeat(64),
            1 => changed.authority_binding_digest = "0".repeat(64),
            2 => changed.completion_payload_digest = "0".repeat(64),
            3 => changed.model_response_digest = "0".repeat(64),
            4 => changed.context_digest = "0".repeat(64),
            5 => changed.usage_event_digest = "0".repeat(64),
            6 => changed.usage_event_id = Uuid::from_u128(999).to_string(),
            _ => changed.resolution_event_id = Uuid::from_u128(999),
        }
        variants.push(changed);
    }
    variants
}

#[test]
fn mixed_evidence_requires_trusted_verification_and_exact_replay_receipt() {
    let store = WorkflowStore::open(":memory:").unwrap();
    let source = fixture(&store, Some(2));
    let proof = receipt(&store, &source);
    for committed in [false, true] {
        if committed {
            dispose(&store, &source, &proof, NOW + 1_002);
        }
        for mixed in mixed_receipts(&proof) {
            assert_ne!(
                mixed.canonical_digest().unwrap(),
                proof.canonical_digest().unwrap()
            );
            let before = snapshot(&store);
            assert!(store
                .dispose_rejected_adaptive_model(
                    Uuid::from_u128(OPERATION),
                    &mixed,
                    &source.grant.authority,
                    || panic!("mixed evidence must not sample time"),
                    |_, _, checked| {
                        assert!(!committed, "replay must not invoke verifier");
                        assert_ne!(checked, &proof);
                        Err(authority_conflict())
                    },
                )
                .is_err());
            assert_eq!(snapshot(&store), before);
        }
    }
}

#[test]
fn replay_precedes_freshness_but_never_authority_or_newer_head() {
    let store = WorkflowStore::open(":memory:").unwrap();
    let source = fixture(&store, Some(2));
    let proof = receipt(&store, &source);
    let (_, historical) = dispose(&store, &source, &proof, NOW + 1_002);
    let newer = advance(
        &store,
        &historical,
        AdaptiveTransitionV1::ClaimModel {
            effect: effect(402),
            previous_observation_digest: Some("b".repeat(64)),
        },
        NOW + 1_003,
    );
    let before = snapshot(&store);
    let replay = store
        .dispose_rejected_adaptive_model(
            Uuid::from_u128(OPERATION),
            &proof,
            &source.grant.authority,
            || panic!("replay must not sample time"),
            |_, _, _| panic!("replay must not verify"),
        )
        .unwrap();
    assert_eq!(replay, (true, historical));
    assert_eq!(
        store
            .adaptive_session(proof.session_id, &source.grant.authority)
            .unwrap(),
        Some(newer)
    );
    for committed in [false, true] {
        let mut stale = source.grant.authority.clone();
        stale.profile_generation += 1;
        assert!(store
            .dispose_rejected_adaptive_model(
                Uuid::from_u128(if committed { OPERATION } else { 999 }),
                &proof,
                &stale,
                || panic!("stale authority must not sample time"),
                |_, _, _| panic!("stale authority must not verify"),
            )
            .is_err());
    }
    assert_eq!(snapshot(&store), before);
}

#[test]
fn pending_head_evidence_is_exact_authorized_and_read_only() {
    let store = WorkflowStore::open(":memory:").unwrap();
    let source = fixture(&store, Some(2));
    let before = snapshot(&store);
    let proof = receipt(&store, &source);
    assert_eq!(
        store
            .adaptive_pending_model_head_evidence(
                proof.session_id,
                source.version,
                &proof.effect,
                &source.grant.authority,
            )
            .unwrap(),
        Some((source.clone(), proof.source_entry_digest))
    );
    for (version, requested, stale) in [
        (source.version - 1, effect(401), false),
        (source.version, effect(999), false),
        (source.version, effect(401), true),
    ] {
        let mut current = source.grant.authority.clone();
        if stale {
            current.organization_generation += 1;
        }
        assert!(store
            .adaptive_pending_model_head_evidence(
                source.grant.session_id,
                version,
                &requested,
                &current,
            )
            .is_err());
    }
    assert_eq!(
        store
            .adaptive_pending_model_head_evidence(
                Uuid::from_u128(999),
                source.version,
                &effect(401),
                &source.grant.authority,
            )
            .unwrap(),
        None
    );
    assert_eq!(snapshot(&store), before);
}

#[test]
fn all_transaction_write_boundaries_roll_back_and_retry() {
    for boundary in 0..6 {
        let store = WorkflowStore::open(":memory:").unwrap();
        let source = fixture(&store, Some(2));
        let proof = receipt(&store, &source);
        let ns = namespace(proof.session_id);
        let op = Uuid::from_u128(OPERATION);
        let (table, action, condition) = match boundary {
            0 => (
                "workflow_operations",
                "INSERT",
                format!(
                    "NEW.operation_namespace='{ns}' AND NEW.operation_id='{:020}'",
                    source.version + 1
                ),
            ),
            1 => (
                "workflow_operations",
                "INSERT",
                format!(
                    "NEW.operation_namespace='{ns}' AND NEW.operation_id='{:020}'",
                    source.version + 2
                ),
            ),
            2 => (
                "workflow_operations",
                "INSERT",
                format!("NEW.operation_namespace='{ns}:operations' AND NEW.operation_id='{op}'"),
            ),
            3 => (
                "workflow_operations",
                "INSERT",
                format!(
                    "NEW.operation_namespace='{ns}:operations' AND NEW.operation_id='{}'",
                    rejected_model_resume_operation_id(op)
                ),
            ),
            4 => (
                "workflow_operations",
                "INSERT",
                format!(
                    "NEW.operation_namespace='{}'",
                    rejected_model_disposition_namespace(proof.session_id)
                ),
            ),
            _ => ("workflow_adaptive_heads", "UPDATE", "1".into()),
        };
        let before = snapshot(&store);
        store.lock().unwrap().execute_batch(&format!(
            "CREATE TRIGGER disposition_fail BEFORE {action} ON {table} WHEN {condition} BEGIN SELECT RAISE(ABORT, 'fixture failpoint'); END;"
        )).unwrap();
        assert!(store
            .dispose_rejected_adaptive_model(
                op,
                &proof,
                &source.grant.authority,
                || NOW + 1_002,
                |_, _, _| Ok(write_decision()),
            )
            .is_err());
        assert_eq!(snapshot(&store), before);
        store
            .lock()
            .unwrap()
            .execute_batch("DROP TRIGGER disposition_fail")
            .unwrap();
        assert!(!dispose(&store, &source, &proof, NOW + 1_002).0);
    }
}

#[test]
fn competing_connections_commit_once_and_replay_without_reverification() {
    for same_operation in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("workflow.sqlite");
        let store = WorkflowStore::open(&path).unwrap();
        let source = fixture(&store, Some(2));
        let proof = receipt(&store, &source);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let verifies = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut threads = Vec::new();
        for index in 0..2 {
            let connection = WorkflowStore::open(&path).unwrap();
            let source = source.clone();
            let proof = proof.clone();
            let barrier = barrier.clone();
            let verifies = verifies.clone();
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                connection.dispose_rejected_adaptive_model(
                    Uuid::from_u128(OPERATION + if same_operation { 0 } else { index }),
                    &proof,
                    &source.grant.authority,
                    || NOW + 1_002,
                    |_, _, _| {
                        verifies.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        Ok(write_decision())
                    },
                )
            }));
        }
        let results: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(verifies.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Ok((false, _))))
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Ok((true, _))))
                .count(),
            usize::from(same_operation)
        );
        assert_eq!(
            results.iter().filter(|result| result.is_err()).count(),
            usize::from(!same_operation)
        );
        let head = store
            .adaptive_session(proof.session_id, &source.grant.authority)
            .unwrap()
            .unwrap();
        assert_eq!(head.version, source.version + 2);
        assert_eq!(head.model_calls, 4);
    }
}

#[test]
fn reopen_requires_disposition_proof_and_both_phase_operations() {
    for corruption in 0..4 {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("workflow.sqlite");
        let store = WorkflowStore::open(&path).unwrap();
        let source = fixture(&store, Some(2));
        let proof = receipt(&store, &source);
        let (_, resumed) = dispose(&store, &source, &proof, NOW + 1_002);
        drop(store);
        let reopened = WorkflowStore::open(&path).unwrap();
        assert_eq!(
            reopened
                .adaptive_session(proof.session_id, &source.grant.authority)
                .unwrap(),
            Some(resumed)
        );
        if corruption == 0 {
            continue;
        }
        let (ns, operation) = match corruption {
            1 => (
                rejected_model_disposition_namespace(proof.session_id),
                Uuid::from_u128(OPERATION),
            ),
            2 => (
                format!("{}:operations", namespace(proof.session_id)),
                Uuid::from_u128(OPERATION),
            ),
            _ => (
                format!("{}:operations", namespace(proof.session_id)),
                rejected_model_resume_operation_id(Uuid::from_u128(OPERATION)),
            ),
        };
        reopened
            .lock()
            .unwrap()
            .execute(
                "DELETE FROM workflow_operations WHERE operation_namespace=?1 AND operation_id=?2",
                params![ns, operation.to_string()],
            )
            .unwrap();
        drop(reopened);
        let corrupted = WorkflowStore::open(&path).unwrap();
        assert!(corrupted
            .adaptive_session(proof.session_id, &source.grant.authority)
            .is_err());
    }
}

#[test]
fn receipt_is_bounded_strict_and_legacy_rejection_encoding_is_unchanged() {
    let store = WorkflowStore::open(":memory:").unwrap();
    let source = fixture(&store, Some(2));
    let proof = receipt(&store, &source);
    proof.validate().unwrap();
    let bytes = serde_json::to_vec(&proof).unwrap();
    assert_eq!(
        serde_json::from_slice::<AdaptiveRejectedModelReceiptV1>(&bytes).unwrap(),
        proof
    );
    let mut extra = serde_json::to_value(&proof).unwrap();
    extra["forged"] = serde_json::json!(true);
    assert!(serde_json::from_value::<AdaptiveRejectedModelReceiptV1>(extra).is_err());
    for mode in 0..7 {
        let mut invalid = proof.clone();
        match mode {
            0 => invalid.schema_version = 2,
            1 => invalid.session_id = Uuid::nil(),
            2 => invalid.source_session_version = u64::MAX,
            3 => invalid.reason_code = "unknown".into(),
            4 => invalid.usage_event_id = Uuid::nil().to_string(),
            5 => invalid.context_digest = "a".repeat(65),
            _ => invalid.resolution_event_id = Uuid::nil(),
        }
        assert!(invalid.validate().is_err());
        assert!(invalid.canonical_digest().is_err());
    }
    let reject = AdaptiveTransitionV1::RejectModel {
        effect: proof.effect.clone(),
        resolution_event_id: proof.resolution_event_id.to_string(),
        reason_code: proof.reason_code.clone(),
    };
    assert_eq!(
        serde_json::to_value(&reject).unwrap(),
        serde_json::json!({
            "kind": "reject_model", "effect": proof.effect,
            "resolution_event_id": proof.resolution_event_id.to_string(),
            "reason_code": "fresh_observation_required",
        })
    );
}
