use super::*;
use crate::AdaptiveCursorV1;

pub(super) fn validate_recovery_lineage(
    connection: &Connection,
    current_root: &Entry,
) -> Result<(), WorkflowError> {
    let mut child = current_root.clone();
    let mut visited = std::collections::BTreeSet::from([child.session.grant.session_id]);
    let mut depth = 0;
    while let Some(feedback) = child.recovery_feedback.clone() {
        feedback.validate().map_err(|_| corrupt_store())?;
        if depth >= ADAPTIVE_SCHEMA_MAX_CORRECTIONS
            || visited.len() >= MAX_SCOPED_ADAPTIVE_HEADS
            || feedback.count == 0
            || visited.contains(&feedback.previous_session_id)
        {
            return Err(corrupt_store());
        }
        let (source, _, inherited) = load_with_feedback(connection, feedback.previous_session_id)?
            .ok_or_else(corrupt_store)?;
        let ns = namespace(feedback.previous_session_id);
        let (_, root) = evidence_entry(connection, &ns, 1)?;
        let (_, claim) = evidence_entry(connection, &ns, 2)?;
        if !matches!(source.version, 4 | 5) {
            return Err(corrupt_store());
        }
        let rejection_version = source.version - 1;
        let (_, rejection) = evidence_entry(connection, &ns, rejection_version)?;
        let (_, cancellation) = evidence_entry(connection, &ns, source.version)?;
        // Rollover cancellation can lack an operation row and is no longer a head.
        if source.grant.session_id != feedback.previous_session_id
            || source != cancellation.session
            || source.grant.authority != current_root.session.grant.authority
            || root.recovery_feedback != inherited
            || inherited
                .as_ref()
                .map_or(0, |prior| prior.count)
                .checked_add(1)
                != Some(feedback.count)
            || source.grant.deadline_ms > child.session.updated_at_ms
            || source.updated_at_ms < source.grant.deadline_ms
            || source.updated_at_ms > child.session.updated_at_ms
            || !matches!(&source.cursor, AdaptiveCursorV1::Cancelled)
            || !matches!(&cancellation.command, Some(AdaptiveTransitionV1::Cancel))
        {
            return Err(corrupt_store());
        }
        let effect = match (&claim.command, &rejection.command) {
            (
                Some(AdaptiveTransitionV1::ClaimModel {
                    effect,
                    previous_observation_digest: None,
                }),
                Some(AdaptiveTransitionV1::RejectModel {
                    effect: rejected,
                    reason_code,
                    resolution_event_id,
                }),
            ) if effect == rejected
                && reason_code == &feedback.reason_code
                && resolution_event_id == &feedback.resolution_event_id =>
            {
                effect
            }
            _ => return Err(corrupt_store()),
        };
        if source.version == 5 {
            let (_, unknown) = evidence_entry(connection, &ns, 3)?;
            if !matches!(&unknown.command, Some(AdaptiveTransitionV1::MarkUnknown { effect: sealed }) if sealed == effect)
                || !matches!(&unknown.session.cursor, AdaptiveCursorV1::ModelUnknown { effect: sealed } if sealed == effect)
                || unknown.session.model_calls != 1
                || unknown.session.tool_calls != 0
                || unknown.session.effect_ids != std::collections::BTreeSet::from([effect.id])
                || unknown.session.last_observation.is_some()
                || unknown.session.last_model_result_digest.is_some()
                || unknown.session.continuation.is_some()
            {
                return Err(corrupt_store());
            }
        }
        if !matches!(&claim.session.cursor, AdaptiveCursorV1::ModelPending { effect: pending } if pending == effect)
            || !matches!(&rejection.session.cursor, AdaptiveCursorV1::ModelRejected { reason_code, resolution_event_id }
                if reason_code == &feedback.reason_code && resolution_event_id == &feedback.resolution_event_id)
            || root.session.model_calls != 0
            || !root.session.effect_ids.is_empty()
            || [&claim.session, &rejection.session, &source]
                .iter()
                .any(|session| {
                    session.model_calls != 1
                        || session.effect_ids != std::collections::BTreeSet::from([effect.id])
                })
            || [&root.session, &claim.session, &rejection.session, &source]
                .iter()
                .any(|session| {
                    session.tool_calls != 0
                        || session.last_observation.is_some()
                        || session.last_model_result_digest.is_some()
                        || session.continuation.is_some()
                })
        {
            return Err(corrupt_store());
        }
        let operations = validated_journal_operations(connection, &source)?;
        let versions = operations
            .iter()
            .map(|(record, _)| record.session_version)
            .collect::<std::collections::BTreeSet<_>>();
        if (2..=rejection_version).any(|version| !versions.contains(&version)) {
            return Err(corrupt_store());
        }
        if source.updated_at_ms == child.session.updated_at_ms {
            if source.grant.provider_allowance_id == child.session.grant.provider_allowance_id {
                return Err(corrupt_store());
            }
            visited.insert(source.grant.session_id);
            child = root;
            depth += 1;
        } else {
            let idle = idle_bridge(connection, &child, &feedback, source.updated_at_ms)?;
            if !visited.insert(idle.session.grant.session_id) {
                return Err(corrupt_store());
            }
            child = idle;
        }
    }
    Ok(())
}

fn idle_bridge(
    connection: &Connection,
    child: &Entry,
    feedback: &AdaptiveRecoveryFeedbackV1,
    earliest_root_ms: u64,
) -> Result<Entry, WorkflowError> {
    let authority = &child.session.grant.authority;
    // JSON is a bounded candidate locator only. Every selected bridge is decoded,
    // replayed and operation-bound below; locator fields never establish proof.
    let namespaces = {
        let mut statement = connection.prepare(
            "SELECT operation_namespace FROM workflow_operations WHERE operation_id=?1 AND created_at_ms=?2 AND operation_namespace GLOB 'adaptive-session-v1:*' AND json_valid(response) AND json_extract(response,'$.session.grant.authority.tenant_id')=?3 AND json_extract(response,'$.session.grant.authority.project_id')=?4 AND json_extract(response,'$.session.grant.authority.work_item_id')=?5 AND json_extract(response,'$.session.grant.authority.agent_id')=?6 ORDER BY operation_namespace LIMIT ?7",
        ).map_err(map_sqlite_error)?;
        let rows = statement
            .query_map(
                params![
                    format!("{:020}", 2),
                    sql_u64(child.session.updated_at_ms)?,
                    authority.tenant_id.0,
                    authority.project_id.0,
                    authority.work_item_id.0,
                    i64::from(authority.agent_id.0),
                    (MAX_SCOPED_ADAPTIVE_HEADS + 1) as i64,
                ],
                |row| row.get::<_, String>(0),
            )
            .map_err(map_sqlite_error)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(map_sqlite_error)?
    };
    if namespaces.len() > MAX_SCOPED_ADAPTIVE_HEADS {
        return Err(corrupt_store());
    }
    let mut selected = None;
    for ns in namespaces {
        let id = ns
            .strip_prefix("adaptive-session-v1:")
            .and_then(|id| Uuid::parse_str(id).ok())
            .ok_or_else(corrupt_store)?;
        if id.is_nil() || namespace(id) != ns {
            return Err(corrupt_store());
        }
        let (session, _, inherited) =
            load_with_feedback(connection, id)?.ok_or_else(corrupt_store)?;
        if session.version != 2 || !matches!(&session.cursor, AdaptiveCursorV1::Cancelled) {
            continue;
        }
        let (_, root) = evidence_entry(connection, &ns, 1)?;
        let (_, cancellation) = evidence_entry(connection, &ns, 2)?;
        if session.grant.authority != *authority || inherited.as_ref() != Some(feedback) {
            continue;
        }
        if root.recovery_feedback != inherited
            || session != cancellation.session
            || !matches!(&root.session.cursor, AdaptiveCursorV1::ReadyForModel)
            || !matches!(&cancellation.command, Some(AdaptiveTransitionV1::Cancel))
            || session.updated_at_ms != child.session.updated_at_ms
            || session.updated_at_ms < session.grant.deadline_ms
            || root.session.updated_at_ms < earliest_root_ms
            || root.session.updated_at_ms >= child.session.updated_at_ms
            || session.grant.provider_allowance_id == child.session.grant.provider_allowance_id
            || [&root.session, &session].iter().any(|state| {
                state.model_calls != 0
                    || state.tool_calls != 0
                    || !state.effect_ids.is_empty()
                    || state.last_observation.is_some()
                    || state.last_model_result_digest.is_some()
                    || state.continuation.is_some()
            })
        {
            return Err(corrupt_store());
        }
        validated_journal_operations(connection, &session)?;
        if selected.replace(root).is_some() {
            return Err(corrupt_store());
        }
    }
    selected.ok_or_else(corrupt_store)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adaptive::continuation_tests::{effect, grant, NOW};

    struct Fixture {
        _temp: tempfile::TempDir,
        path: std::path::PathBuf,
        store: WorkflowStore,
        current: AdaptiveSessionV1,
        ancestors: Vec<Uuid>,
        intervening: Vec<Uuid>,
    }

    fn next_grant(prior: &AdaptiveSessionGrantV1) -> AdaptiveSessionGrantV1 {
        let mut next = prior.clone();
        next.session_id = Uuid::new_v4();
        next.provider_allowance_id = format!("allowance-{}", next.session_id);
        next.created_at_ms = prior.deadline_ms;
        next.deadline_ms = next.created_at_ms + 1_000;
        next
    }

    fn fixture(corrections: u16, intervening_rollovers: usize) -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("workflow.sqlite");
        let store = WorkflowStore::open(&path).unwrap();
        let mut root = grant();
        let mut session = store
            .begin_adaptive_session(&root, &root.authority, NOW)
            .unwrap()
            .1;
        let mut ancestors = Vec::new();
        for index in 0..corrections {
            let model = effect(1_000 + u128::from(index));
            session = store
                .advance_adaptive_session(
                    root.session_id,
                    session.version,
                    Uuid::new_v4(),
                    &AdaptiveTransitionV1::ClaimModel {
                        effect: model.clone(),
                        previous_observation_digest: None,
                    },
                    &root.authority,
                    session.updated_at_ms + 1,
                )
                .unwrap()
                .1;
            store
                .advance_adaptive_session(
                    root.session_id,
                    session.version,
                    Uuid::new_v4(),
                    &AdaptiveTransitionV1::RejectModel {
                        effect: model,
                        resolution_event_id: Uuid::new_v4().to_string(),
                        reason_code: "adaptive_tool_schema".into(),
                    },
                    &root.authority,
                    session.updated_at_ms + 1,
                )
                .unwrap();
            ancestors.push(root.session_id);
            root = next_grant(&root);
            session = store
                .begin_adaptive_session(&root, &root.authority, root.created_at_ms)
                .unwrap()
                .1;
        }
        let mut intervening = Vec::new();
        for _ in 0..intervening_rollovers {
            intervening.push(root.session_id);
            root = next_grant(&root);
            session = store
                .begin_adaptive_session(&root, &root.authority, root.created_at_ms)
                .unwrap()
                .1;
        }
        let model = effect(2_000);
        session = store
            .advance_adaptive_session(
                root.session_id,
                session.version,
                Uuid::new_v4(),
                &AdaptiveTransitionV1::ClaimModel {
                    effect: model.clone(),
                    previous_observation_digest: None,
                },
                &root.authority,
                session.updated_at_ms + 1,
            )
            .unwrap()
            .1;
        session = store
            .advance_adaptive_session(
                root.session_id,
                session.version,
                Uuid::new_v4(),
                &AdaptiveTransitionV1::MarkUnknown { effect: model },
                &root.authority,
                session.updated_at_ms + 1,
            )
            .unwrap()
            .1;
        Fixture {
            _temp: temp,
            path,
            store,
            current: session,
            ancestors,
            intervening,
        }
    }

    fn total_changes(store: &WorkflowStore) -> i64 {
        store
            .lock()
            .unwrap()
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap()
    }

    fn assert_rejected_without_writes(f: &Fixture) {
        let before = total_changes(&f.store);
        assert_eq!(
            f.store
                .first_unknown_model_journal_evidence(
                    f.current.grant.session_id,
                    &f.current.grant.authority,
                )
                .unwrap_err()
                .code,
            WorkflowErrorCode::CorruptStore
        );
        assert_eq!(total_changes(&f.store), before);
    }

    // Rebind fixture entries and command operations canonically so lineage tests
    // cannot pass merely because a stale row digest fails ordinary replay first.
    fn rewrite_history(store: &WorkflowStore, id: Uuid, mutate: impl FnOnce(&mut Vec<Entry>)) {
        let mut connection = store.lock().unwrap();
        let ns = namespace(id);
        let mut entries: Vec<Entry> = {
            let mut statement = connection.prepare(
                "SELECT response FROM workflow_operations WHERE operation_namespace=?1 ORDER BY operation_id",
            ).unwrap();
            let rows = statement
                .query_map([&ns], |row| row.get::<_, Vec<u8>>(0))
                .unwrap();
            rows.map(|row| decode(&row.unwrap()).unwrap()).collect()
        };
        mutate(&mut entries);
        let root_grant = entries[0].session.grant.clone();
        let tx = immediate(&mut connection).unwrap();
        tx.execute(
            "DELETE FROM workflow_operations WHERE operation_namespace IN (?1,?2)",
            params![ns, format!("{ns}:operations")],
        )
        .unwrap();
        let mut previous: Option<(AdaptiveSessionV1, String)> = None;
        for entry in &mut entries {
            let now = entry.session.updated_at_ms;
            entry.session = if let Some((prior, _)) = &previous {
                prior
                    .transition(entry.command.as_ref().unwrap(), now)
                    .unwrap()
            } else {
                let mut initial = AdaptiveSessionV1::initial(root_grant.clone()).unwrap();
                initial.updated_at_ms = now;
                initial
            };
            entry.previous_digest = previous.as_ref().map(|(_, digest)| digest.clone());
            let digest = canonical_sha256("sentinel.workflow.adaptive-entry.v1", entry).unwrap();
            append(&tx, &ns, entry).unwrap();
            if let Some(command) = entry.command.as_ref() {
                if !matches!(command, AdaptiveTransitionV1::Cancel) {
                    let command_digest = canonical_sha256(
                        "sentinel.workflow.adaptive-command.v1",
                        &(id, entry.session.version - 1, command),
                    )
                    .unwrap();
                    insert_operation(
                        &tx,
                        &format!("{ns}:operations"),
                        &Uuid::new_v4().to_string(),
                        &command_digest,
                        &entry.session,
                        now,
                    )
                    .unwrap();
                }
            }
            previous = Some((entry.session.clone(), digest));
        }
        tx.commit().unwrap();
    }

    #[test]
    fn inherited_roots_preserve_exact_unknown_evidence_and_do_not_require_ancestor_heads() {
        for corrections in 1..=ADAPTIVE_SCHEMA_MAX_CORRECTIONS {
            let f = fixture(corrections, 0);
            let before = total_changes(&f.store);
            let evidence = f
                .store
                .first_unknown_model_journal_evidence(
                    f.current.grant.session_id,
                    &f.current.grant.authority,
                )
                .unwrap()
                .unwrap();
            assert_eq!(evidence.root_grant, f.current.grant);
            assert_eq!(evidence.effect, effect(2_000));
            assert_eq!(
                (
                    evidence.claim.session_version,
                    evidence.seal.session_version
                ),
                (2, 3)
            );
            assert_eq!(
                (evidence.sealed_model_calls, evidence.sealed_tool_calls),
                (1, 0)
            );
            assert_eq!(evidence.observed_head_version, f.current.version);
            {
                let connection = f.store.lock().unwrap();
                let ns = namespace(f.current.grant.session_id);
                assert_eq!(
                    evidence.root_entry_digest,
                    evidence_entry(&connection, &ns, 1).unwrap().0
                );
                assert_eq!(
                    evidence.claim.entry_digest,
                    evidence_entry(&connection, &ns, 2).unwrap().0
                );
                assert_eq!(
                    evidence.seal.entry_digest,
                    evidence_entry(&connection, &ns, 3).unwrap().0
                );
                let operations = validated_journal_operations(&connection, &f.current).unwrap();
                assert!(operations
                    .iter()
                    .any(|(record, _)| record == &evidence.claim));
                assert!(operations
                    .iter()
                    .any(|(record, _)| record == &evidence.seal));
                for id in &f.ancestors {
                    let ancestor = load(&connection, *id).unwrap().unwrap().0;
                    assert_eq!(ancestor.version, 4);
                    assert!(matches!(ancestor.cursor, AdaptiveCursorV1::Cancelled));
                    assert!(require_head(&connection, &ancestor).is_err());
                }
            }
            assert_eq!(total_changes(&f.store), before);
            let reopened = WorkflowStore::open(&f.path).unwrap();
            let before = total_changes(&reopened);
            assert_eq!(
                reopened
                    .first_unknown_model_journal_evidence(
                        f.current.grant.session_id,
                        &f.current.grant.authority,
                    )
                    .unwrap(),
                Some(evidence)
            );
            assert_eq!(total_changes(&reopened), before);
        }
    }

    #[test]
    fn unchanged_feedback_survives_never_claimed_intervening_rollovers() {
        let f = fixture(2, 3);
        let before = total_changes(&f.store);
        let evidence = f
            .store
            .first_unknown_model_journal_evidence(
                f.current.grant.session_id,
                &f.current.grant.authority,
            )
            .unwrap()
            .unwrap();
        assert_eq!(evidence.root_grant, f.current.grant);
        let connection = f.store.lock().unwrap();
        for id in &f.intervening {
            let session = load(&connection, *id).unwrap().unwrap().0;
            assert_eq!(
                (session.version, session.model_calls, session.tool_calls),
                (2, 0, 0)
            );
            assert!(matches!(session.cursor, AdaptiveCursorV1::Cancelled));
        }
        drop(connection);
        assert_eq!(total_changes(&f.store), before);
    }

    #[test]
    fn feedback_reason_resolution_count_missing_source_and_self_cycle_fail_closed() {
        for case in 0..5 {
            let f = fixture(1, 0);
            rewrite_history(&f.store, f.current.grant.session_id, |entries| {
                let feedback = entries[0].recovery_feedback.as_mut().unwrap();
                match case {
                    0 => feedback.reason_code = "other_schema_reason".into(),
                    1 => feedback.resolution_event_id = Uuid::new_v4().to_string(),
                    2 => feedback.count = 2,
                    3 => feedback.previous_session_id = Uuid::new_v4(),
                    _ => feedback.previous_session_id = f.current.grant.session_id,
                }
            });
            assert_rejected_without_writes(&f);
        }
    }

    #[test]
    fn foreign_authority_source_identity_and_reused_allowance_are_rejected() {
        for case in 0..4 {
            let f = fixture(1, 0);
            rewrite_history(&f.store, f.ancestors[0], |entries| {
                let root = &mut entries[0].session.grant;
                match case {
                    0 => root.authority.policy_generation += 1,
                    1 => root.authority.assignment_version += 1,
                    2 => root.session_id = Uuid::new_v4(),
                    _ => root.provider_allowance_id = f.current.grant.provider_allowance_id.clone(),
                }
            });
            assert_rejected_without_writes(&f);
        }
    }

    #[test]
    fn cyclic_and_nonmonotone_multisession_feedback_is_rejected() {
        for cycle in [false, true] {
            let f = fixture(2, 0);
            let id = if cycle {
                f.ancestors[0]
            } else {
                f.ancestors[1]
            };
            let feedback = evidence_entry(&f.store.lock().unwrap(), &namespace(f.ancestors[1]), 1)
                .unwrap()
                .1
                .recovery_feedback;
            rewrite_history(&f.store, id, |entries| {
                if cycle {
                    entries[0].recovery_feedback = feedback;
                    entries[0]
                        .recovery_feedback
                        .as_mut()
                        .unwrap()
                        .previous_session_id = f.ancestors[1];
                } else {
                    entries[0].recovery_feedback.as_mut().unwrap().count = 2;
                }
            });
            assert_rejected_without_writes(&f);
        }
    }

    #[test]
    fn ancestor_cancellation_must_follow_expiry_and_precede_the_child_root() {
        for too_late in [false, true] {
            let f = fixture(1, 0);
            rewrite_history(&f.store, f.ancestors[0], |entries| {
                entries[3].session.updated_at_ms = if too_late {
                    f.current.grant.created_at_ms + 1
                } else {
                    NOW + 3
                };
            });
            assert_rejected_without_writes(&f);
        }
    }

    #[test]
    fn adjacent_cancellation_binds_root_recorded_time_not_grant_creation() {
        let f = fixture(1, 0);
        let recorded = f.current.grant.created_at_ms + 1;
        rewrite_history(&f.store, f.ancestors[0], |entries| {
            entries[3].session.updated_at_ms = recorded;
        });
        rewrite_history(&f.store, f.current.grant.session_id, |entries| {
            entries[0].session.updated_at_ms = recorded;
        });
        let before = total_changes(&f.store);
        let evidence = f
            .store
            .first_unknown_model_journal_evidence(
                f.current.grant.session_id,
                &f.current.grant.authority,
            )
            .unwrap()
            .unwrap();
        assert_eq!(evidence.root_grant, f.current.grant);
        assert_eq!(evidence.root_recorded_at_ms, recorded);
        assert_eq!(total_changes(&f.store), before);
    }

    #[test]
    fn a_recorded_time_gap_without_a_verified_idle_bridge_is_rejected() {
        let f = fixture(1, 0);
        rewrite_history(&f.store, f.current.grant.session_id, |entries| {
            entries[0].session.updated_at_ms += 1;
        });
        assert_rejected_without_writes(&f);
    }

    fn clone_idle_history(f: &Fixture, id: Uuid) {
        let ns = namespace(f.intervening[0]);
        let original = {
            let connection = f.store.lock().unwrap();
            (1..=2)
                .map(|version| evidence_entry(&connection, &ns, version).unwrap().1)
                .collect::<Vec<_>>()
        };
        rewrite_history(&f.store, id, |entries| {
            *entries = original;
            entries[0].session.grant.session_id = id;
            entries[0].session.grant.provider_allowance_id = format!("idle-allowance-{id}");
        });
    }

    #[test]
    fn missing_corrupt_foreign_and_changed_feedback_idle_bridges_are_rejected() {
        for case in 0..5 {
            let f = fixture(1, 1);
            let id = f.intervening[0];
            match case {
                0 => {
                    f.store
                        .lock()
                        .unwrap()
                        .execute(
                            "DELETE FROM workflow_operations WHERE operation_namespace=?1",
                            [namespace(id)],
                        )
                        .unwrap();
                }
                1 => {
                    f.store.lock().unwrap().execute(
                        "UPDATE workflow_operations SET request_digest='invalid' WHERE operation_namespace=?1 AND operation_id=?2",
                        params![namespace(id), format!("{:020}", 1)],
                    ).unwrap();
                }
                _ => rewrite_history(&f.store, id, |entries| {
                    if case == 2 {
                        entries[0].session.grant.authority.policy_generation += 1;
                    } else if case == 3 {
                        entries[0].recovery_feedback.as_mut().unwrap().reason_code =
                            "other_reason".into();
                    } else {
                        entries[1].session.updated_at_ms += 1;
                    }
                }),
            }
            assert_rejected_without_writes(&f);
        }
    }

    #[test]
    fn ambiguous_idle_bridge_and_bounded_idle_candidate_overflow_fail_closed() {
        for overflow in [false, true] {
            let f = fixture(1, 1);
            let clones = if overflow {
                MAX_SCOPED_ADAPTIVE_HEADS
            } else {
                1
            };
            for _ in 0..clones {
                clone_idle_history(&f, Uuid::new_v4());
            }
            assert_rejected_without_writes(&f);
        }
    }

    #[test]
    fn idle_lineage_walk_is_bounded_without_resetting_the_correction_count() {
        let f = fixture(1, MAX_SCOPED_ADAPTIVE_HEADS);
        assert_rejected_without_writes(&f);
    }

    #[test]
    fn nonadjacent_allowance_reuse_is_not_an_invented_lineage_restriction() {
        let f = fixture(2, 0);
        rewrite_history(&f.store, f.ancestors[0], |entries| {
            entries[0].session.grant.provider_allowance_id =
                f.current.grant.provider_allowance_id.clone();
        });
        let before = total_changes(&f.store);
        assert!(f
            .store
            .first_unknown_model_journal_evidence(
                f.current.grant.session_id,
                &f.current.grant.authority,
            )
            .unwrap()
            .is_some());
        assert_eq!(total_changes(&f.store), before);
    }

    #[test]
    fn missing_corrupt_and_ambiguous_ancestor_command_operations_are_rejected() {
        for sql in [
            "DELETE FROM workflow_operations WHERE operation_namespace=?1 AND operation_id=?2",
            "UPDATE workflow_operations SET request_digest='invalid' WHERE operation_namespace=?1 AND operation_id=?2",
            "UPDATE workflow_operations SET created_at_ms=created_at_ms+1 WHERE operation_namespace=?1 AND operation_id=?2",
            "UPDATE workflow_operations SET response=x'7b7d' WHERE operation_namespace=?1 AND operation_id=?2",
            "INSERT INTO workflow_operations SELECT operation_namespace,'00000000-0000-0000-0000-00000000ffff',request_digest,response,created_at_ms FROM workflow_operations WHERE operation_namespace=?1 AND operation_id=?2",
        ] {
            let f = fixture(1, 0);
            let connection = f.store.lock().unwrap();
            let ancestor = load(&connection, f.ancestors[0]).unwrap().unwrap().0;
            let operation = validated_journal_operations(&connection, &ancestor).unwrap()
                .into_iter().find(|(record, _)| record.session_version == 3).unwrap().0;
            connection.execute(sql, params![
                format!("{}:operations", namespace(f.ancestors[0])), operation.operation_id.to_string(),
            ]).unwrap();
            drop(connection);
            assert_rejected_without_writes(&f);
        }
    }

    #[test]
    fn corrupt_or_missing_ancestor_journal_and_uncancelled_source_are_rejected() {
        for case in 0..3 {
            let f = fixture(1, 0);
            let connection = f.store.lock().unwrap();
            let ns = namespace(f.ancestors[0]);
            match case {
                0 => connection.execute("DELETE FROM workflow_operations WHERE operation_namespace=?1",
                    [&ns]).unwrap(),
                1 => connection.execute("UPDATE workflow_operations SET request_digest='invalid' WHERE operation_namespace=?1 AND operation_id=?2",
                    params![ns, format!("{:020}", 2)]).unwrap(),
                _ => connection.execute("DELETE FROM workflow_operations WHERE operation_namespace=?1 AND operation_id=?2",
                    params![ns, format!("{:020}", 4)]).unwrap(),
            };
            drop(connection);
            assert_rejected_without_writes(&f);
        }
    }

    #[test]
    fn canonical_historical_unknown_then_rejection_and_cancellation_is_supported() {
        let f = fixture(1, 0);
        rewrite_history(&f.store, f.ancestors[0], |entries| {
            let mut unknown = entries[2].clone();
            unknown.command = Some(AdaptiveTransitionV1::MarkUnknown {
                effect: effect(1_000),
            });
            entries.insert(2, unknown);
        });
        let before = total_changes(&f.store);
        let evidence = f
            .store
            .first_unknown_model_journal_evidence(
                f.current.grant.session_id,
                &f.current.grant.authority,
            )
            .unwrap()
            .unwrap();
        assert_eq!(evidence.root_grant, f.current.grant);
        assert_eq!(evidence.effect, effect(2_000));
        assert_eq!(total_changes(&f.store), before);
    }

    #[test]
    fn canonical_rejection_reasons_and_optional_cancellation_operations_are_supported() {
        let f = fixture(1, 1);
        let reason = "schema_rejection";
        rewrite_history(&f.store, f.ancestors[0], |entries| {
            let Some(AdaptiveTransitionV1::RejectModel { reason_code, .. }) =
                entries[2].command.as_mut()
            else {
                panic!("expected rejection")
            };
            *reason_code = reason.into();
        });
        for id in [f.intervening[0], f.current.grant.session_id] {
            rewrite_history(&f.store, id, |entries| {
                entries[0].recovery_feedback.as_mut().unwrap().reason_code = reason.into();
            });
        }
        {
            let mut connection = f.store.lock().unwrap();
            let tx = immediate(&mut connection).unwrap();
            for id in [f.ancestors[0], f.intervening[0]] {
                let session = load(&tx, id).unwrap().unwrap().0;
                let command = AdaptiveTransitionV1::Cancel;
                let digest = canonical_sha256(
                    "sentinel.workflow.adaptive-command.v1",
                    &(id, session.version - 1, &command),
                )
                .unwrap();
                insert_operation(
                    &tx,
                    &format!("{}:operations", namespace(id)),
                    &Uuid::new_v4().to_string(),
                    &digest,
                    &session,
                    session.updated_at_ms,
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }
        let before = total_changes(&f.store);
        assert!(f
            .store
            .first_unknown_model_journal_evidence(
                f.current.grant.session_id,
                &f.current.grant.authority,
            )
            .unwrap()
            .is_some());
        assert_eq!(total_changes(&f.store), before);
    }

    #[test]
    fn missing_unknown_operation_and_corrupt_optional_idle_cancellation_are_rejected() {
        for missing_unknown in [false, true] {
            let f = fixture(1, 1);
            if missing_unknown {
                rewrite_history(&f.store, f.ancestors[0], |entries| {
                    let mut unknown = entries[2].clone();
                    unknown.command = Some(AdaptiveTransitionV1::MarkUnknown {
                        effect: effect(1_000),
                    });
                    entries.insert(2, unknown);
                });
                let connection = f.store.lock().unwrap();
                let session = load(&connection, f.ancestors[0]).unwrap().unwrap().0;
                let operation = validated_journal_operations(&connection, &session)
                    .unwrap()
                    .into_iter()
                    .find(|(record, _)| record.session_version == 3)
                    .unwrap()
                    .0;
                connection.execute(
                    "DELETE FROM workflow_operations WHERE operation_namespace=?1 AND operation_id=?2",
                    params![format!("{}:operations", namespace(f.ancestors[0])), operation.operation_id.to_string()],
                ).unwrap();
            } else {
                let mut connection = f.store.lock().unwrap();
                let tx = immediate(&mut connection).unwrap();
                let id = f.intervening[0];
                let session = load(&tx, id).unwrap().unwrap().0;
                insert_operation(
                    &tx,
                    &format!("{}:operations", namespace(id)),
                    &Uuid::new_v4().to_string(),
                    &"a".repeat(64),
                    &session,
                    session.updated_at_ms,
                )
                .unwrap();
                tx.commit().unwrap();
            }
            assert_rejected_without_writes(&f);
        }
    }

    #[test]
    fn adopted_model_results_and_tool_claims_cannot_be_feedback_ancestors() {
        for claimed_tool in [false, true] {
            let f = fixture(1, 0);
            rewrite_history(&f.store, f.ancestors[0], |entries| {
                let tool = sentinel_common::WorkbenchTool::InspectFile {
                    path: "source.rs".into(),
                    max_bytes: 1_024,
                };
                let tool_digest = crate::adaptive_tool_digest(&tool).unwrap();
                entries[2].command = Some(AdaptiveTransitionV1::ResolveModel {
                    effect: effect(1_000),
                    result_digest: "a".repeat(64),
                    decision: AdaptiveModelDecisionV1::Tool {
                        tool,
                        tool_digest: tool_digest.clone(),
                    },
                });
                if claimed_tool {
                    let mut claim = entries[2].clone();
                    claim.session.updated_at_ms = NOW + 3;
                    claim.command = Some(AdaptiveTransitionV1::ClaimTool {
                        effect: effect(3_000),
                        tool_digest,
                    });
                    let mut observation = claim.clone();
                    observation.session.updated_at_ms = NOW + 4;
                    observation.command = Some(AdaptiveTransitionV1::ObserveTool {
                        observation: crate::AdaptiveObservationRefV1 {
                            effect: effect(3_000),
                            observation_digest: "b".repeat(64),
                        },
                    });
                    entries.insert(3, claim);
                    entries.insert(4, observation);
                }
            });
            assert_rejected_without_writes(&f);
        }
    }
}
