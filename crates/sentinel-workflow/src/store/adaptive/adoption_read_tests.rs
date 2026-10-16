use super::*;
use crate::adaptive::continuation_tests::{authorization, effect, grant, NOW};
use crate::domain_store::validation_scope::{self, ValidationFailureReason};
use crate::{AdaptiveCollaborationActionV1, AdaptiveCursorV1};

type Outcome = Result<
    (&'static str, Option<AdaptiveSessionV1>, bool),
    (&'static str, WorkflowError),
>;

struct Fixture {
    root: tempfile::TempDir,
    store: WorkflowStore,
    session: AdaptiveSessionV1,
    model: AdaptiveEffectV1,
    digest: String,
    decision: AdaptiveModelDecisionV1,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let store = WorkflowStore::open(root.path().join("adoption.sqlite")).unwrap();
        let grant = grant();
        let session = store
            .begin_adaptive_session(&grant, &grant.authority, NOW)
            .unwrap()
            .1;
        Self {
            root,
            store,
            session,
            model: effect(102),
            digest: "a".repeat(64),
            decision: AdaptiveModelDecisionV1::Blocked {
                reason_code: "needs_review".into(),
            },
        }
    }

    fn advance(&mut self, command: AdaptiveTransitionV1) {
        self.session = self
            .store
            .advance_adaptive_session(
                self.session.grant.session_id,
                self.session.version,
                Uuid::from_u128(10_000 + u128::from(self.session.version)),
                &command,
                &self.session.grant.authority,
                self.session.updated_at_ms + 1,
            )
            .unwrap()
            .1;
    }

    fn claim(&mut self) {
        self.advance(AdaptiveTransitionV1::ClaimModel {
            effect: self.model.clone(),
            previous_observation_digest: None,
        });
    }

    fn resolution(&self) -> AdaptiveTransitionV1 {
        AdaptiveTransitionV1::ResolveModel {
            effect: self.model.clone(),
            result_digest: self.digest.clone(),
            decision: self.decision.clone(),
        }
    }

    fn abandon(&mut self) {
        self.advance(AdaptiveTransitionV1::MarkUnknown {
            effect: self.model.clone(),
        });
        let auth = authorization(&self.session);
        let command = AdaptiveTransitionV1::ContinueGoverned {
            authorization: auth.clone(),
        };
        let next = self.session.transition(&command, auth.issued_at_ms).unwrap();
        // The existing continuation tests seed this validated historical transition directly.
        {
            let mut connection = self.store.lock().unwrap();
            let tx = immediate(&mut connection).unwrap();
            let (_, previous_digest) = load(&tx, self.session.grant.session_id).unwrap().unwrap();
            append(&tx, &namespace(self.session.grant.session_id), &Entry {
                previous_digest: Some(previous_digest), command: Some(command),
                session: next.clone(), recovery_feedback: None,
            }).unwrap();
            update_head(&tx, &self.session, &next).unwrap();
            tx.commit().unwrap();
        }
        self.session = next;
    }

    fn read(&self) -> Outcome {
        combined(
            &self.store,
            &self.session.effective_grant(),
            &self.session.grant.authority,
            &self.model,
            &self.digest,
            &self.decision,
        )
    }

    fn assert_current(&self, adopted: bool) {
        let grant = self.session.effective_grant();
        let expected = legacy(
            &self.store,
            &grant,
            &grant.authority,
            &self.model,
            &self.digest,
            &self.decision,
        );
        assert_eq!(expected, Ok(("current", Some(self.session.clone()), adopted)));
        assert_eq!(self.read(), expected);
        assert!(self.store.lock().unwrap().is_autocommit());
    }
}

fn legacy(
    store: &WorkflowStore,
    grant: &AdaptiveSessionGrantV1,
    current: &RuntimeAuthoritySnapshotV1,
    effect: &AdaptiveEffectV1,
    digest: &str,
    decision: &AdaptiveModelDecisionV1,
) -> Outcome {
    let Some(session) = store
        .adaptive_session(grant.session_id, current)
        .map_err(|error| ("session", error))?
    else {
        return Ok(("missing", None, false));
    };
    if session.effective_grant() != *grant {
        return Ok(("grant_changed", None, false));
    }
    let adopted = store
        .adaptive_model_result_is_adopted(grant, effect, digest, decision)
        .map_err(|error| ("replay", error))?;
    Ok(("current", Some(session), adopted))
}

fn combined(
    store: &WorkflowStore,
    grant: &AdaptiveSessionGrantV1,
    current: &RuntimeAuthoritySnapshotV1,
    effect: &AdaptiveEffectV1,
    digest: &str,
    decision: &AdaptiveModelDecisionV1,
) -> Outcome {
    match store.adaptive_model_adoption_read(grant, current, effect, digest, decision) {
        Ok(AdaptiveModelAdoptionRead::Missing) => Ok(("missing", None, false)),
        Ok(AdaptiveModelAdoptionRead::GrantChanged) => Ok(("grant_changed", None, false)),
        Ok(AdaptiveModelAdoptionRead::Current {
            session,
            already_adopted,
        }) => Ok(("current", Some(*session), already_adopted)),
        Err(AdaptiveModelAdoptionReadError::Session(error)) => Err(("session", error)),
        Err(AdaptiveModelAdoptionReadError::Replay(error)) => Err(("replay", error)),
    }
}

fn rows(store: &WorkflowStore) -> Vec<u8> {
    let connection = store.lock().unwrap();
    let mut statement = connection
        .prepare("SELECT operation_namespace,operation_id,request_digest,response,created_at_ms FROM workflow_operations ORDER BY operation_namespace,operation_id")
        .unwrap();
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
    let mut statement = connection
        .prepare("SELECT tenant_id,project_id,work_item_id,agent_id,authority_digest,session_id,version,updated_at_ms FROM workflow_adaptive_heads ORDER BY tenant_id,project_id,work_item_id,agent_id,authority_digest")
        .unwrap();
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
    encode(&(operations, heads)).unwrap()
}

fn changes(store: &WorkflowStore) -> i64 {
    store
        .lock()
        .unwrap()
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn adoption_read_matches_legacy_for_ready_pending_and_resolved_model() {
    let mut fixture = Fixture::new();
    fixture.assert_current(false);
    fixture.claim();
    fixture.assert_current(false);
    fixture.advance(fixture.resolution());
    fixture.assert_current(true);
    let before = rows(&fixture.store);
    let count = changes(&fixture.store);
    for _ in 0..3 {
        fixture.assert_current(true);
    }
    assert_eq!(rows(&fixture.store), before);
    assert_eq!(changes(&fixture.store), count);
    let reopened = WorkflowStore::open(fixture.root.path().join("adoption.sqlite")).unwrap();
    assert_eq!(
        combined(
            &reopened,
            &fixture.session.effective_grant(),
            &fixture.session.grant.authority,
            &fixture.model,
            &fixture.digest,
            &fixture.decision,
        ),
        fixture.read()
    );
    assert_eq!(rows(&reopened), before);
    assert_eq!(changes(&reopened), 0);
}

#[test]
fn adoption_read_preserves_missing_grant_and_authority_precedence() {
    let fixture = Fixture::new();
    let original = fixture.session.effective_grant();
    for case in ["missing", "grant", "authority", "inactive"] {
        let mut requested = original.clone();
        let mut current = original.authority.clone();
        match case {
            "missing" => requested.session_id = Uuid::from_u128(999),
            "grant" => requested.max_model_calls -= 1,
            "authority" => current.policy_generation += 1,
            "inactive" => current.active = false,
            _ => unreachable!(),
        }
        let expected = legacy(
            &fixture.store,
            &requested,
            &current,
            &fixture.model,
            &fixture.digest,
            &fixture.decision,
        );
        match case {
            "missing" => assert_eq!(expected, Ok(("missing", None, false))),
            "grant" => assert_eq!(expected, Ok(("grant_changed", None, false))),
            _ => assert_eq!(expected.as_ref().unwrap_err().0, "session"),
        }
        assert_eq!(
            combined(
                &fixture.store,
                &requested,
                &current,
                &fixture.model,
                &fixture.digest,
                &fixture.decision,
            ),
            expected,
            "{case}"
        );
    }
}

#[test]
fn adoption_read_checks_full_session_and_head_before_reporting_grant_change() {
    for damage in ["head", "journal"] {
        let mut fixture = Fixture::new();
        fixture.claim();
        fixture.assert_current(false);
        let mut requested = fixture.session.effective_grant();
        requested.max_model_calls -= 1;
        {
            let connection = fixture.store.lock().unwrap();
            if damage == "head" {
                connection.execute(
                    "UPDATE workflow_adaptive_heads SET version=version+1 WHERE session_id=?1",
                    [fixture.session.grant.session_id.to_string()],
                ).unwrap();
            } else {
                connection.execute(
                    "UPDATE workflow_operations SET response=X'00' WHERE operation_namespace=?1 AND operation_id=?2",
                    params![namespace(fixture.session.grant.session_id), format!("{:020}", 1)],
                ).unwrap();
            }
        }
        let standalone = fixture.store.adaptive_model_result_is_adopted(
            &requested, &fixture.model, &fixture.digest, &fixture.decision,
        ).unwrap_err();
        // Standalone replay keeps its old grant-before-head check; the combined
        // acceptance path must validate the authorized head before GrantChanged.
        assert_eq!(standalone.code, if damage == "head" {
            WorkflowErrorCode::IdempotencyConflict
        } else {
            WorkflowErrorCode::CorruptStore
        });
        let expected = legacy(&fixture.store, &requested, &requested.authority, &fixture.model, &fixture.digest, &fixture.decision);
        assert_eq!(expected.as_ref().unwrap_err().0, "session");
        assert_eq!(expected.as_ref().unwrap_err().1.code, WorkflowErrorCode::CorruptStore);
        assert_eq!(combined(&fixture.store, &requested, &requested.authority, &fixture.model, &fixture.digest, &fixture.decision), expected);
    }
}

#[test]
fn adoption_read_keys_bind_complete_grant_authority_effect_digest_and_decision() {
    let mut fixture = Fixture::new();
    fixture.claim();
    fixture.advance(fixture.resolution());
    fixture.assert_current(true);
    let original = fixture.session.effective_grant();
    for case in ["effect-id", "effect-digest", "result", "decision", "grant", "authority"] {
        let mut requested = original.clone();
        let mut current = original.authority.clone();
        let mut model = fixture.model.clone();
        let mut digest = fixture.digest.clone();
        let mut decision = fixture.decision.clone();
        match case {
            "effect-id" => model.id = Uuid::from_u128(998),
            "effect-digest" => model.request_digest = "b".repeat(64),
            "result" => digest = "b".repeat(64),
            "decision" => decision = AdaptiveModelDecisionV1::Blocked { reason_code: "different_result".into() },
            "grant" => requested.max_tool_calls -= 1,
            "authority" => current.organization_generation += 1,
            _ => unreachable!(),
        }
        let expected = legacy(&fixture.store, &requested, &current, &model, &digest, &decision);
        if matches!(case, "effect-id" | "effect-digest" | "result" | "decision") {
            assert_eq!(expected, Ok(("current", Some(fixture.session.clone()), false)));
        }
        assert_eq!(combined(&fixture.store, &requested, &current, &model, &digest, &decision), expected, "{case}");
        fixture.assert_current(true);
    }
}

#[test]
fn adoption_read_collaboration_needs_exact_resolution_and_matching_commit() {
    let mut fixture = Fixture::new();
    fixture.decision = AdaptiveModelDecisionV1::Collaborate {
        action: AdaptiveCollaborationActionV1::AskQuestion { question_ref: "question:implementation-choice".into() },
    };
    fixture.claim();
    fixture.assert_current(false);
    fixture.advance(fixture.resolution());
    assert!(matches!(fixture.session.cursor, AdaptiveCursorV1::CollaborationProposed { .. }));
    fixture.assert_current(false);
    let before = rows(&fixture.store);
    let rejected = fixture.store.advance_adaptive_session(
        fixture.session.grant.session_id,
        fixture.session.version,
        Uuid::from_u128(990),
        &AdaptiveTransitionV1::CommitCollaboration {
            effect: fixture.model.clone(),
            action_digest: "f".repeat(64),
        },
        &fixture.session.grant.authority,
        fixture.session.updated_at_ms + 1,
    );
    assert!(rejected.is_err());
    assert_eq!(rows(&fixture.store), before);
    fixture.assert_current(false);
    let reopened = WorkflowStore::open(fixture.root.path().join("adoption.sqlite")).unwrap();
    let grant = fixture.session.effective_grant();
    assert_eq!(combined(&reopened, &grant, &grant.authority, &fixture.model, &fixture.digest, &fixture.decision), Ok(("current", Some(fixture.session.clone()), false)));
    drop(reopened);
    let AdaptiveModelDecisionV1::Collaborate { action } = &fixture.decision else { unreachable!() };
    fixture.advance(AdaptiveTransitionV1::CommitCollaboration {
        effect: fixture.model.clone(),
        action_digest: adaptive_collaboration_digest(action).unwrap(),
    });
    fixture.assert_current(true);
    let wrong = AdaptiveModelDecisionV1::Collaborate {
        action: AdaptiveCollaborationActionV1::AskQuestion { question_ref: "question:other-choice".into() },
    };
    let grant = fixture.session.effective_grant();
    assert_eq!(
        combined(&fixture.store, &grant, &grant.authority, &fixture.model, &fixture.digest, &wrong),
        Ok(("current", Some(fixture.session.clone()), false))
    );
}

#[test]
fn adoption_read_abandoned_effect_is_replay_error_not_new_authority() {
    let mut fixture = Fixture::new();
    fixture.claim();
    fixture.abandon();
    let grant = fixture.session.effective_grant();
    let expected = legacy(&fixture.store, &grant, &grant.authority, &fixture.model, &fixture.digest, &fixture.decision);
    assert_eq!(expected.as_ref().unwrap_err().0, "replay");
    assert_eq!(expected.as_ref().unwrap_err().1.code, WorkflowErrorCode::AuthorityConflict);
    let reopened = WorkflowStore::open(fixture.root.path().join("adoption.sqlite")).unwrap();
    for store in [&fixture.store, &reopened] {
        for request_digest in [fixture.model.request_digest.clone(), "e".repeat(64)] {
            let mut abandoned = fixture.model.clone();
            abandoned.request_digest = request_digest;
            for _ in 0..2 {
                WORKING_MEMORY_READS.with(|reads| reads.set((0, 0, 0, 0)));
                let result = combined(store, &grant, &grant.authority, &abandoned, &fixture.digest, &fixture.decision);
                assert_eq!(result, expected);
                assert_eq!(WORKING_MEMORY_READS.with(|reads| reads.get().0), 1, "failed replay cannot become a successful cached enum");
            }
            assert_eq!(legacy(store, &grant, &grant.authority, &abandoned, &fixture.digest, &fixture.decision), expected);
        }
        let mut prior = grant.clone();
        prior.provider_allowance_id = "another-allowance".into();
        assert_eq!(combined(store, &prior, &grant.authority, &fixture.model, &fixture.digest, &fixture.decision), Ok(("grant_changed", None, false)));
    }
}

#[test]
fn adoption_read_external_commit_and_rollback_invalidate_only_committed_image() {
    for commit in [false, true] {
        let mut fixture = Fixture::new();
        fixture.claim();
        fixture.assert_current(false);
        let command = fixture.resolution();
        let next = fixture.session.transition(&command, fixture.session.updated_at_ms + 1).unwrap();
        let mut peer = Connection::open(fixture.root.path().join("adoption.sqlite")).unwrap();
        let tx = peer.transaction_with_behavior(TransactionBehavior::Immediate).unwrap();
        let (_, previous_digest) = load(&tx, fixture.session.grant.session_id).unwrap().unwrap();
        append(&tx, &namespace(fixture.session.grant.session_id), &Entry {
            previous_digest: Some(previous_digest), command: Some(command),
            session: next.clone(), recovery_feedback: None,
        }).unwrap();
        update_head(&tx, &fixture.session, &next).unwrap();
        fixture.assert_current(false);
        if commit {
            tx.commit().unwrap();
            fixture.session = next;
        } else {
            tx.rollback().unwrap();
        }
        fixture.assert_current(commit);
        let before = rows(&fixture.store);
        let count = changes(&fixture.store);
        fixture.assert_current(commit);
        assert_eq!(rows(&fixture.store), before);
        assert_eq!(changes(&fixture.store), count);
    }
}

#[test]
fn adoption_read_rejects_malformed_late_chain_head_and_schema_after_warm_proof() {
    for damage in ["malformed", "late", "chain", "head", "schema"] {
        let mut fixture = Fixture::new();
        fixture.claim();
        fixture.advance(fixture.resolution());
        fixture.assert_current(true);
        {
            let connection = fixture.store.lock().unwrap();
            let ns = namespace(fixture.session.grant.session_id);
            match damage {
                "malformed" => { connection.execute("UPDATE workflow_operations SET response=X'00' WHERE operation_namespace=?1 AND operation_id=?2", params![ns, format!("{:020}", 1)]).unwrap(); }
                "late" => { connection.execute("INSERT INTO workflow_operations(operation_namespace,operation_id,request_digest,response,created_at_ms) VALUES(?1,?2,?3,X'00',?4)", params![ns, format!("{:020}", fixture.session.version + 1), "a".repeat(64), i64::try_from(NOW + 5).unwrap()]).unwrap(); }
                "chain" => {
                    let bytes: Vec<u8> = connection.query_row("SELECT response FROM workflow_operations WHERE operation_namespace=?1 AND operation_id=?2", params![ns, format!("{:020}", fixture.session.version)], |row| row.get(0)).unwrap();
                    let mut entry: Entry = decode(&bytes).unwrap();
                    entry.previous_digest = Some("b".repeat(64));
                    connection.execute("UPDATE workflow_operations SET response=?1,request_digest=?2 WHERE operation_namespace=?3 AND operation_id=?4", params![encode(&entry).unwrap(), canonical_sha256("sentinel.workflow.adaptive-entry.v1", &entry).unwrap(), ns, format!("{:020}", fixture.session.version)]).unwrap();
                }
                "head" => { connection.execute("UPDATE workflow_adaptive_heads SET version=version+1 WHERE session_id=?1", [fixture.session.grant.session_id.to_string()]).unwrap(); }
                "schema" => { connection.execute_batch("ALTER TABLE workflow_operations RENAME COLUMN response TO removed_response").unwrap(); }
                _ => unreachable!(),
            }
        }
        let grant = fixture.session.effective_grant();
        let expected = legacy(&fixture.store, &grant, &grant.authority, &fixture.model, &fixture.digest, &fixture.decision);
        assert_eq!(expected.as_ref().unwrap_err().0, "session", "{damage}");
        for digest in [&fixture.digest, &"c".repeat(64)] {
            assert_eq!(combined(&fixture.store, &grant, &grant.authority, &fixture.model, digest, &fixture.decision), expected, "{damage}");
        }
        assert!(fixture.store.lock().unwrap().is_autocommit());
    }
}

#[test]
fn adoption_read_rejects_index_only_corruption_for_hit_and_new_key() {
    use std::io::{Seek, SeekFrom, Write};
    for case in ["hit", "new-key", "replay-error"] {
        let mut fixture = Fixture::new();
        fixture.claim();
        if case == "replay-error" {
            fixture.abandon();
        } else {
            fixture.advance(fixture.resolution());
        }
        fixture.store.lock().unwrap().execute_batch("CREATE INDEX adoption_payload_index ON workflow_operations(created_at_ms); PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
        if case == "replay-error" {
            let grant = fixture.session.effective_grant();
            assert_eq!(combined(&fixture.store, &grant, &grant.authority, &effect(999), &fixture.digest, &fixture.decision), Ok(("current", Some(fixture.session.clone()), false)));
        } else {
            fixture.assert_current(true);
        }
        let (page_size, page) = {
            let connection = fixture.store.lock().unwrap();
            let page_size: u32 = connection.query_row("PRAGMA page_size", [], |row| row.get(0)).unwrap();
            let page: u32 = connection.query_row("SELECT rootpage FROM sqlite_schema WHERE name='adoption_payload_index'", [], |row| row.get(0)).unwrap();
            connection.execute_batch("PRAGMA shrink_memory").unwrap();
            (page_size, page)
        };
        let before = rows(&fixture.store);
        let mut file = std::fs::OpenOptions::new().write(true).open(fixture.root.path().join("adoption.sqlite")).unwrap();
        file.seek(SeekFrom::Start(u64::from(page - 1) * u64::from(page_size))).unwrap();
        file.write_all(&vec![0; page_size as usize]).unwrap();
        file.sync_all().unwrap();
        assert_eq!(rows(&fixture.store), before);
        let digest = if case == "new-key" { "d".repeat(64) } else { fixture.digest.clone() };
        let grant = fixture.session.effective_grant();
        let failure = combined(&fixture.store, &grant, &grant.authority, &fixture.model, &digest, &fixture.decision).unwrap_err();
        assert_eq!(failure.0, "session", "{case}: integrity failure supersedes the validator's replay failure");
        assert_eq!(failure.1.code, WorkflowErrorCode::CorruptStore);
        assert!(fixture.store.lock().unwrap().is_autocommit());
    }
}

#[test]
fn adoption_read_snapshot_cleanup_failure_discards_successful_enum() {
    let fixture = Fixture::new();
    let before = rows(&fixture.store);
    {
        let mut connection = fixture.store.lock().unwrap();
        let result = fixture.store.validated_read_snapshot(
            &mut connection,
            "adoption-cleanup-fixture",
            &[],
            |snapshot| {
                snapshot.execute_batch("ROLLBACK")?;
                Ok(AdaptiveModelAdoptionRead::Current {
                    session: Box::new(fixture.session.clone()), already_adopted: false,
                })
            },
        );
        assert!(result.is_err());
        assert!(connection.is_autocommit());
        assert!(format!("{:?}", fixture.store.validated_reads.lock().unwrap()).contains("views: 0"));
    }
    fixture.assert_current(false);
    assert_eq!(rows(&fixture.store), before);
}

#[test]
fn adoption_read_preserves_64mib_predecode_body_overflow() {
    const LIMIT: usize = 64 * 1024 * 1024;
    let fixture = Fixture::new();
    fixture.assert_current(false);
    {
        let connection = fixture.store.lock().unwrap();
        let ns = namespace(fixture.session.grant.session_id);
        let original: Vec<u8> = connection.query_row("SELECT response FROM workflow_operations WHERE operation_namespace=?1", [&ns], |row| row.get(0)).unwrap();
        let mut padded = vec![b' '; LIMIT + 1 - original.len()];
        padded.extend_from_slice(&original);
        assert_eq!(padded.len(), LIMIT + 1);
        connection.execute("UPDATE workflow_operations SET response=?1 WHERE operation_namespace=?2", params![padded, ns]).unwrap();
    }
    let grant = fixture.session.effective_grant();
    for paired in [false, true] {
        WORKING_MEMORY_READS.with(|reads| reads.set((0, 0, 0, 0)));
        let (result, failures) = validation_scope::with_validation_failures(|| {
            if paired {
                fixture.read()
            } else {
                legacy(&fixture.store, &grant, &grant.authority, &fixture.model, &fixture.digest, &fixture.decision)
            }
        });
        let (phase, error) = result.unwrap_err();
        assert_eq!(phase, "session");
        assert_eq!(error.code, WorkflowErrorCode::CorruptStore);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].reason, ValidationFailureReason::BytesLimit);
        assert_eq!(failures[0].attempted_bytes, Some(LIMIT + 1));
        assert_eq!(failures[0].charge_phase, Some(validation_scope::ValidationChargePhase::Body));
        assert_eq!(WORKING_MEMORY_READS.with(|reads| reads.get().1), 0);
        assert!(fixture.store.lock().unwrap().is_autocommit());
    }
}

#[test]
fn adoption_read_owns_one_complete_journal_replay_without_writes() {
    let mut fixture = Fixture::new();
    fixture.claim();
    fixture.advance(fixture.resolution());
    let before = rows(&fixture.store);
    let count = changes(&fixture.store);
    let grant = fixture.session.effective_grant();
    WORKING_MEMORY_READS.with(|reads| reads.set((0, 0, 0, 0)));
    let expected = legacy(&fixture.store, &grant, &grant.authority, &fixture.model, &fixture.digest, &fixture.decision);
    assert_eq!(WORKING_MEMORY_READS.with(|reads| reads.get().0), 2);
    WORKING_MEMORY_READS.with(|reads| reads.set((0, 0, 0, 0)));
    let (result, scopes) = validation_scope::with_completed_validations(|| fixture.read());
    assert_eq!(result, Ok(("current", Some(fixture.session.clone()), true)));
    assert_eq!(result, expected);
    let count_domain = |domain| scopes.iter().map(|scope| scope.get(domain).copied().unwrap_or(0)).sum::<usize>();
    assert_eq!(count_domain("adaptive-journal"), 1);
    assert_eq!(count_domain("adaptive-journal-entry"), fixture.session.version as usize);
    assert_eq!(count_domain("adaptive-journal-step"), fixture.session.version as usize);
    assert_eq!(WORKING_MEMORY_READS.with(|reads| reads.get()), (1, fixture.session.version as usize, 0, 0));
    assert_eq!(rows(&fixture.store), before);
    assert_eq!(changes(&fixture.store), count);
}
