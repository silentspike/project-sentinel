//! Store-backed regression coverage; no live provider execution is implied.

use super::super::super::adaptive_leadership_review::tests::{
    discovery_state, persist, persist_discovery_project,
};
use super::super::super::model_work::{assign_test_work_from, configured_adaptive_test_api};
use super::*;

// These hooks count acquisition attempts, not SQL scans or cache validations.
thread_local! {
    static ACQUISITIONS: std::cell::Cell<[usize; 5]> = const {
        std::cell::Cell::new([0; 5])
    };
    static MODEL_PREPARATION_READS: std::cell::Cell<[usize; 3]> = const {
        std::cell::Cell::new([0; 3])
    };
    static AFTER_PREPARATION_CALLBACKS: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const {
        std::cell::RefCell::new(None)
    };
}

pub(in crate::workflow_api::model_execution) fn record_model_preparation_read(phase: &str) {
    let index = match phase {
        "preparation_snapshot" => 0,
        "post_callback_snapshot" => 1,
        "final_freshness_snapshot" => 2,
        _ => return,
    };
    MODEL_PREPARATION_READS.with(|counts| {
        let mut next = counts.get();
        next[index] += 1;
        counts.set(next);
    });
}

pub(in crate::workflow_api::model_execution) fn after_preparation_callbacks() {
    let callback = AFTER_PREPARATION_CALLBACKS.with(|slot| slot.borrow_mut().take());
    if let Some(callback) = callback {
        callback();
    }
}

#[test]
fn paired_adaptive_gate_removes_duplicate_snapshot_and_preserves_bytes_without_writes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("company.sqlite");
    let events = temp.path().join("events.sqlite");
    let (api, expected, _) = configured_adaptive_test_api(&path, &events);
    let context = api.prepare_adaptive_model(&expected).unwrap();
    let authority = ProviderExecutionAuthority::Adaptive(Box::new(expected));
    let envelope = ModelExecutionContext::Adaptive(Box::new(context));
    let bytes = serde_json::to_vec(&envelope).unwrap();
    let before = discovery_state(&path, &events);

    MODEL_PREPARATION_READS.with(|counts| counts.set([0; 3]));
    EXACT_ADAPTIVE_READ_COUNTS.with(|counts| counts.set((0, 0, 0)));
    assert_eq!(api.validate_provider_usage_authority(&authority), Ok(true));
    assert_eq!(
        api.model_work_context(&authority).unwrap(),
        Some(envelope.clone())
    );
    assert_eq!(
        MODEL_PREPARATION_READS.with(|counts| counts.get()),
        [1, 1, 1]
    );
    assert_eq!(
        EXACT_ADAPTIVE_READ_COUNTS.with(|counts| counts.get()),
        (1, 1, 1)
    );

    MODEL_PREPARATION_READS.with(|counts| counts.set([0; 3]));
    EXACT_ADAPTIVE_READ_COUNTS.with(|counts| counts.set((0, 0, 0)));
    for gate in 1..=2 {
        assert_eq!(
            api.validate_prepared_provider_model_context(&authority, &envelope),
            Ok(Some(())),
        );
        assert_eq!(
            MODEL_PREPARATION_READS.with(|counts| counts.get()),
            [0, gate, gate]
        );
        assert_eq!(
            EXACT_ADAPTIVE_READ_COUNTS.with(|counts| counts.get()),
            (gate, gate, gate)
        );
        assert_eq!(serde_json::to_vec(&envelope).unwrap(), bytes);
        assert_eq!(discovery_state(&path, &events), before);
    }
    let mut changed = envelope.clone();
    let ModelExecutionContext::Adaptive(context) = &mut changed else {
        panic!("adaptive fixture");
    };
    context.task.objective.push_str(" changed saved context");
    assert_eq!(
        api.validate_prepared_provider_model_context(&authority, &changed),
        Err("model work authority changed during provider I/O".into()),
    );
    assert_eq!(discovery_state(&path, &events), before);
}

#[test]
fn shared_adaptive_read_payloads_are_reused_but_complete_keys_stay_separate() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("company.sqlite");
    let events = temp.path().join("events.sqlite");
    let (api, expected, _) = configured_adaptive_test_api(&path, &events);
    let scope = &expected.grant.authority;
    let read_project = |work: &WorkItemId| {
        api.store
            .adaptive_project_snapshot_shared(
                &scope.tenant_id,
                &scope.project_id,
                work,
                scope.agent_id,
            )
            .unwrap()
    };
    let first = read_project(&scope.work_item_id);
    let second = read_project(&scope.work_item_id);
    assert!(Arc::ptr_eq(&first, &second));
    let foreign = read_project(&WorkItemId::parse("another-work").unwrap());
    assert!(!Arc::ptr_eq(&first, &foreign));
    let read_model = |effect| {
        api.store
            .adaptive_model_preparation_read_snapshot_shared(
                expected.grant.session_id,
                expected.session_version,
                effect,
                scope,
            )
            .unwrap()
    };
    let first = read_model(expected.effect_id);
    let second = read_model(expected.effect_id);
    assert!(Arc::ptr_eq(&first, &second));
    let foreign = read_model(Uuid::new_v4());
    assert!(!Arc::ptr_eq(&first, &foreign));
    assert!(foreign
        .as_ref()
        .as_ref()
        .unwrap()
        .working_memory_source()
        .is_err());
}

#[test]
fn paired_adaptive_gate_reacquires_corrupt_project_and_head_after_warm_read() {
    for project_error in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("company.sqlite");
        let events = temp.path().join("events.sqlite");
        let (api, expected, _) = configured_adaptive_test_api(&path, &events);
        let context = api.prepare_adaptive_model(&expected).unwrap();
        assert_eq!(
            api.validate_prepared_adaptive_model_context(&expected, &context),
            Ok(())
        );
        if project_error {
            rusqlite::Connection::open(&path).unwrap().execute(
                "UPDATE company_entities SET payload_digest='invalid' WHERE entity_kind='project'",
                [],
            ).unwrap();
        } else {
            corrupt_head(&path, expected.grant.session_id);
        }
        let before = discovery_state(&path, &events);
        assert_eq!(
            api.validate_prepared_adaptive_model_context(&expected, &context),
            Err(format!(
                "provider usage reauthorization failed: {}",
                if project_error {
                    "adaptive exact project unavailable"
                } else {
                    "adaptive exact session unavailable"
                }
            )),
        );
        assert_eq!(discovery_state(&path, &events), before);
    }
}

#[test]
fn paired_adaptive_gate_rechecks_runtime_and_principals_after_warm_read() {
    for change in 0..3 {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("company.sqlite");
        let events = temp.path().join("events.sqlite");
        let (mut api, expected, _) = configured_adaptive_test_api(&path, &events);
        let context = api.prepare_adaptive_model(&expected).unwrap();
        api.validate_prepared_adaptive_model_context(&expected, &context)
            .unwrap();
        let authority = Arc::make_mut(api.authority.as_mut().unwrap());
        match change {
            0 => authority.workbench_profile_digest = "0".repeat(64),
            1 => authority.principals = Arc::new(PrincipalAuthenticator::default()),
            _ => {
                for agent in &mut authority.runtime_health.write().unwrap().agents {
                    if agent.agent_id == expected.grant.authority.agent_id.0 {
                        agent.expected_active = false;
                    }
                }
            }
        }
        let before = discovery_state(&path, &events);
        assert!(api
            .validate_prepared_adaptive_model_context(&expected, &context)
            .is_err());
        assert_eq!(discovery_state(&path, &events), before);
    }
}

#[test]
fn paired_adaptive_gate_rejects_post_callback_mutation_without_holding_store_lock() {
    for external in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("company.sqlite");
        let events = temp.path().join("events.sqlite");
        let (api, expected, _) = configured_adaptive_test_api(&path, &events);
        let context = api.prepare_adaptive_model(&expected).unwrap();
        api.validate_prepared_adaptive_model_context(&expected, &context)
            .unwrap();
        let store = Arc::clone(&api.store);
        let binding = expected.clone();
        let database = path.clone();
        AFTER_PREPARATION_CALLBACKS.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                if external {
                    corrupt_head(&database, binding.grant.session_id);
                } else {
                    store
                        .advance_adaptive_session(
                            binding.grant.session_id,
                            binding.session_version,
                            Uuid::new_v4(),
                            &AdaptiveTransitionV1::ClaimModel {
                                effect: AdaptiveEffectV1 {
                                    id: binding.effect_id,
                                    request_digest: "a".repeat(64),
                                },
                                previous_observation_digest: None,
                            },
                            &binding.grant.authority,
                            now_unix_ms(),
                        )
                        .unwrap();
                }
            }));
        });
        assert_eq!(
            api.validate_prepared_adaptive_model_context(&expected, &context),
            Err("adaptive recovery feedback unavailable".into()),
        );
        assert!(AFTER_PREPARATION_CALLBACKS.with(|slot| slot.borrow().is_none()));
        let before = discovery_state(&path, &events);
        assert!(api
            .validate_prepared_adaptive_model_context(&expected, &context)
            .is_err());
        assert_eq!(discovery_state(&path, &events), before);
    }
}

#[test]
fn paired_adaptive_gate_rejects_unknown_model_outcomes_without_releasing_effect() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("company.sqlite");
    let events = temp.path().join("events.sqlite");
    let (api, expected, limbo) = configured_adaptive_test_api(&path, &events);
    let context = api.prepare_adaptive_model(&expected).unwrap();
    api.validate_prepared_adaptive_model_context(&expected, &context)
        .unwrap();
    let effect = AdaptiveEffectV1 {
        id: expected.effect_id,
        request_digest: "a".repeat(64),
    };
    let pending = api
        .store
        .advance_adaptive_session(
            expected.grant.session_id,
            expected.session_version,
            Uuid::new_v4(),
            &AdaptiveTransitionV1::ClaimModel {
                effect: effect.clone(),
                previous_observation_digest: None,
            },
            &expected.grant.authority,
            now_unix_ms(),
        )
        .unwrap()
        .1;
    let id = expected.request_id();
    limbo
        .reserve_llm_request(
            &id,
            &effect.request_digest,
            &expected.grant.authority.agent_id.to_string(),
        )
        .unwrap();
    assert!(limbo
        .mark_llm_provider_outcome_unknown(
            &id,
            &effect.request_digest,
            "UnknownOutcome: provider_transport_deadline_elapsed",
        )
        .unwrap());
    for mark_domain_unknown in [false, true] {
        if mark_domain_unknown {
            api.store
                .advance_adaptive_session(
                    expected.grant.session_id,
                    pending.version,
                    Uuid::new_v4(),
                    &AdaptiveTransitionV1::MarkUnknown {
                        effect: effect.clone(),
                    },
                    &expected.grant.authority,
                    now_unix_ms(),
                )
                .unwrap();
        }
        let before = discovery_state(&path, &events);
        assert_eq!(
            api.validate_prepared_adaptive_model_context(&expected, &context),
            Err("provider usage authority changed during provider I/O".into()),
        );
        assert_eq!(discovery_state(&path, &events), before);
        assert_eq!(
            limbo.get_llm_completion(&id).unwrap().unwrap().status,
            "failed"
        );
    }
}

pub(in crate::workflow_api::model_execution) fn record_acquisition(cohort: &str) {
    let index = match cohort {
        "queue" => 0,
        "continuation" => 1,
        "binding" => 2,
        "first_rejection" => 3,
        "known_rejection" => 4,
        _ => panic!("unknown adaptive read cohort: {cohort}"),
    };
    ACQUISITIONS.with(|counts| {
        let mut next = counts.get();
        next[index] += 1;
        counts.set(next);
    });
}

fn reset() {
    ACQUISITIONS.with(|counts| counts.set([0; 5]));
}

fn assert_counts(expected: [usize; 5]) {
    assert_eq!(ACQUISITIONS.with(|counts| counts.get()), expected);
}

fn project(
    api: &WorkflowApi,
    authority: &AdaptiveProviderAuthority,
) -> sentinel_workflow::ProjectV1 {
    api.store
        .company_project(
            &authority.grant.authority.tenant_id,
            &authority.grant.authority.project_id,
        )
        .unwrap()
        .unwrap()
}

fn binding(project: &sentinel_workflow::ProjectV1) -> ProviderUsageBinding {
    let allowance = project.subscription_call.as_ref().unwrap();
    select_provider_usage_binding(
        std::slice::from_ref(project),
        allowance.grant.agent_id,
        Some(&allowance.allowance_id),
    )
    .unwrap()
    .unwrap()
}

fn corrupt_head(path: &Path, session_id: Uuid) {
    assert!(
        rusqlite::Connection::open(path)
            .unwrap()
            .execute(
                "UPDATE workflow_operations SET request_digest='invalid' WHERE operation_namespace=?1",
                [format!("adaptive-session-v1:{session_id}")],
            )
            .unwrap()
            > 0
    );
}

#[test]
fn adaptive_read_cohorts_acquire_once_per_boundary_and_preserve_binding_bytes_without_writes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("company.sqlite");
    let events = temp.path().join("events.sqlite");
    let (api, expected, _) = configured_adaptive_test_api(&path, &events);
    let project = project(&api, &expected);
    let binding = binding(&project);
    let session = api
        .store
        .adaptive_session(expected.grant.session_id, &expected.grant.authority)
        .unwrap()
        .unwrap();
    let before = discovery_state(&path, &events);
    reset();
    for boundary in 1..=3 {
        assert_eq!(
            api.adaptive_subscription_queue_order(&binding),
            Ok(Some((2, session.updated_at_ms))),
        );
        assert_eq!(
            api.binding_has_continued_adaptive_session(&binding),
            Ok(false)
        );
        let (actual, reconciled) = api
            .adaptive_provider_authority_from_binding(binding.clone(), false)
            .unwrap();
        assert!(!reconciled);
        assert_eq!(actual, Some(expected.clone()));
        assert_eq!(
            serde_json::to_vec(&actual.unwrap()).unwrap(),
            serde_json::to_vec(&expected).unwrap(),
        );
        assert_eq!(
            api.recover_rejected_first_adaptive_model(&project),
            Ok(false)
        );
        assert_eq!(
            api.recover_known_rejected_adaptive_model(&project),
            Ok(false)
        );
        assert_counts([boundary; 5]);
        assert_eq!(discovery_state(&path, &events), before);
    }
}

#[test]
fn adaptive_read_cohorts_missing_head_is_read_only_until_binding_admission() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("company.sqlite");
    let events = temp.path().join("events.sqlite");
    let (api, _, _) = configured_adaptive_test_api(&path, &events);
    let assigned = assign_test_work_from(&api, Some(8), 100);
    let project = api
        .store
        .company_project(
            &TenantId::parse(&assigned.tenant_id).unwrap(),
            &ProjectId::parse(&assigned.project_id).unwrap(),
        )
        .unwrap()
        .unwrap();
    let binding = binding(&project);
    let before = discovery_state(&path, &events);
    reset();
    assert_eq!(
        api.adaptive_subscription_queue_order(&binding),
        Ok(Some((2, 0))),
    );
    assert_eq!(
        api.binding_has_continued_adaptive_session(&binding),
        Ok(false)
    );
    assert_eq!(
        api.recover_rejected_first_adaptive_model(&project),
        Ok(false)
    );
    assert_eq!(
        api.recover_known_rejected_adaptive_model(&project),
        Ok(false)
    );
    assert_counts([1, 1, 0, 1, 1]);
    assert_eq!(discovery_state(&path, &events), before);
    let (created, reconciled) = api
        .adaptive_provider_authority_from_binding(binding.clone(), false)
        .unwrap();
    let created = created.unwrap();
    assert!(!reconciled);
    assert_eq!(created.grant.authority.project_id, project.project_id);
    assert_eq!(created.assignment_id, binding.assignment_id);
    assert_eq!(created.session_version, 1);
    let session = api
        .store
        .adaptive_session(created.grant.session_id, &created.grant.authority)
        .unwrap()
        .unwrap();
    let admitted = discovery_state(&path, &events);
    assert_ne!(admitted, before);
    assert_eq!(
        api.adaptive_subscription_queue_order(&binding),
        Ok(Some((2, session.updated_at_ms))),
    );
    assert_eq!(
        api.adaptive_provider_authority_from_binding(binding, false),
        Ok((Some(created), false)),
    );
    assert_counts([2, 1, 2, 1, 1]);
    assert_eq!(discovery_state(&path, &events), admitted);
}

#[test]
fn adaptive_read_cohorts_reacquire_after_external_project_and_head_corruption() {
    for project_error in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("company.sqlite");
        let events = temp.path().join("events.sqlite");
        let (api, expected, _) = configured_adaptive_test_api(&path, &events);
        let project = project(&api, &expected);
        let binding = binding(&project);
        let session = api
            .store
            .adaptive_session(expected.grant.session_id, &expected.grant.authority)
            .unwrap()
            .unwrap();
        reset();
        assert_eq!(
            api.adaptive_subscription_queue_order(&binding),
            Ok(Some((2, session.updated_at_ms))),
        );
        assert_eq!(
            api.binding_has_continued_adaptive_session(&binding),
            Ok(false)
        );
        assert_eq!(
            api.adaptive_provider_authority_from_binding(binding.clone(), false),
            Ok((Some(expected.clone()), false)),
        );
        if project_error {
            assert_eq!(
                rusqlite::Connection::open(&path)
                    .unwrap()
                    .execute(
                        "UPDATE company_entities SET payload_digest='invalid' WHERE entity_kind='project'",
                        [],
                    )
                    .unwrap(),
                1,
            );
        } else {
            corrupt_head(&path, expected.grant.session_id);
        }
        let before = discovery_state(&path, &events);
        assert_eq!(
            api.adaptive_subscription_queue_order(&binding),
            Err(if project_error {
                "adaptive runtime authority unavailable"
            } else {
                "adaptive queue session unavailable"
            }),
        );
        assert_eq!(
            api.binding_has_continued_adaptive_session(&binding),
            Err(if project_error {
                "adaptive runtime authority unavailable"
            } else {
                "adaptive continuation head unavailable"
            }),
        );
        assert_eq!(
            api.adaptive_provider_authority_from_binding(binding, false),
            Err(if project_error {
                "adaptive project unavailable"
            } else {
                "adaptive continuation head unavailable"
            }),
        );
        assert_eq!(
            api.recover_rejected_first_adaptive_model(&project),
            if project_error {
                Ok(false)
            } else {
                Err("adaptive recovery session unavailable")
            },
        );
        assert_eq!(
            api.recover_known_rejected_adaptive_model(&project),
            if project_error {
                Ok(false)
            } else {
                Err("known rejection session unavailable")
            },
        );
        assert_counts([2, 2, 2, 1, 1]);
        assert_eq!(discovery_state(&path, &events), before);
    }
}

#[test]
fn adaptive_read_cohorts_preserve_allowance_then_runtime_then_head_error_precedence() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("company.sqlite");
    let events = temp.path().join("events.sqlite");
    let (mut api, expected, _) = configured_adaptive_test_api(&path, &events);
    let project = project(&api, &expected);
    let binding = binding(&project);
    corrupt_head(&path, expected.grant.session_id);
    let original_digest = api
        .authority
        .as_ref()
        .unwrap()
        .workbench_profile_digest
        .clone();
    Arc::make_mut(api.authority.as_mut().unwrap()).workbench_profile_digest = "0".repeat(64);
    let before = discovery_state(&path, &events);
    reset();
    let mut changed = binding.clone();
    changed.reservation_id.push_str("-changed");
    assert_eq!(
        api.adaptive_provider_authority_from_binding(changed, false),
        Err("adaptive allowance changed"),
    );
    let mut changed = binding.clone();
    changed.subscription_grant.as_mut().unwrap().max_calls += 1;
    assert_eq!(
        api.adaptive_provider_authority_from_binding(changed, false),
        Err("adaptive allowance is already consumed or changed"),
    );
    assert_eq!(
        api.adaptive_subscription_queue_order(&binding),
        Err("adaptive runtime authority unavailable"),
    );
    assert_eq!(
        api.binding_has_continued_adaptive_session(&binding),
        Err("adaptive runtime authority unavailable"),
    );
    assert_eq!(
        api.adaptive_provider_authority_from_binding(binding.clone(), false),
        Err("adaptive runtime authority unavailable"),
    );
    assert_eq!(
        api.recover_rejected_first_adaptive_model(&project),
        Ok(false)
    );
    assert_eq!(
        api.recover_known_rejected_adaptive_model(&project),
        Ok(false)
    );
    Arc::make_mut(api.authority.as_mut().unwrap()).workbench_profile_digest = original_digest;
    assert_eq!(
        api.adaptive_subscription_queue_order(&binding),
        Err("adaptive queue session unavailable"),
    );
    assert_eq!(
        api.binding_has_continued_adaptive_session(&binding),
        Err("adaptive continuation head unavailable"),
    );
    assert_eq!(
        api.adaptive_provider_authority_from_binding(binding, false),
        Err("adaptive continuation head unavailable"),
    );
    assert_eq!(
        api.recover_rejected_first_adaptive_model(&project),
        Err("adaptive recovery session unavailable"),
    );
    assert_eq!(
        api.recover_known_rejected_adaptive_model(&project),
        Err("known rejection session unavailable"),
    );
    assert_counts([2, 2, 4, 2, 2]);
    assert_eq!(discovery_state(&path, &events), before);
}

#[test]
fn adaptive_read_cohorts_reacquire_committed_model_cursor_without_writes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("company.sqlite");
    let events = temp.path().join("events.sqlite");
    let (api, expected, _) = configured_adaptive_test_api(&path, &events);
    let binding = binding(&project(&api, &expected));
    reset();
    let ready = api
        .store
        .adaptive_session(expected.grant.session_id, &expected.grant.authority)
        .unwrap()
        .unwrap();
    assert_eq!(
        api.adaptive_subscription_queue_order(&binding),
        Ok(Some((2, ready.updated_at_ms))),
    );
    assert_eq!(
        api.adaptive_provider_authority_from_binding(binding.clone(), false),
        Ok((Some(expected.clone()), false)),
    );
    let effect = AdaptiveEffectV1 {
        id: expected.effect_id,
        request_digest: "c".repeat(64),
    };
    let pending = api
        .core
        .advance_adaptive_session(
            expected.grant.session_id,
            ready.version,
            Uuid::new_v4(),
            &AdaptiveTransitionV1::ClaimModel {
                effect: effect.clone(),
                previous_observation_digest: None,
            },
            &expected.grant.authority,
            now_unix_ms(),
        )
        .unwrap()
        .1;
    let before = discovery_state(&path, &events);
    assert_eq!(
        api.adaptive_subscription_queue_order(&binding),
        Ok(Some((1, pending.updated_at_ms))),
    );
    assert_eq!(
        api.adaptive_provider_authority_from_binding(binding.clone(), false),
        Ok((Some(expected.clone()), false)),
    );
    assert_eq!(discovery_state(&path, &events), before);
    api.core
        .advance_adaptive_session(
            expected.grant.session_id,
            pending.version,
            Uuid::new_v4(),
            &AdaptiveTransitionV1::ResolveModel {
                effect,
                result_digest: "b".repeat(64),
                decision: AdaptiveModelDecisionV1::Blocked {
                    reason_code: "dependency_unavailable".into(),
                },
            },
            &expected.grant.authority,
            now_unix_ms(),
        )
        .unwrap();
    let before = discovery_state(&path, &events);
    assert_eq!(api.adaptive_subscription_queue_order(&binding), Ok(None));
    assert_eq!(
        api.binding_has_continued_adaptive_session(&binding),
        Ok(false)
    );
    assert_eq!(
        api.adaptive_provider_authority_from_binding(binding, false),
        Ok((None, false)),
    );
    assert_counts([3, 1, 3, 0, 0]);
    assert_eq!(discovery_state(&path, &events), before);
}

#[test]
fn adaptive_read_cohorts_continuation_uses_active_allowance_and_rejects_stale_binding() {
    use super::super::super::adaptive_continuation_tests::{
        fixture_schema2_blocked, reserve_and_claim_schema2,
    };
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("company.sqlite");
    let events = temp.path().join("events.sqlite");
    let (api, context) = fixture_schema2_blocked(&path, &events);
    let stale = binding(&context.source.source_project);
    let completion = ModelExecutionCompletion {
        context: ModelExecutionContext::AdaptiveLeadershipReview(Box::new(context.clone())),
        content: serde_json::json!({"schema_version": 2, "decision": {
            "kind": "continue", "additional_model_calls": 1, "window_ms": 300_000,
            "rationale": "Inspect the current assignment before continuing.",
            "evidence_refs": context.source.evidence_refs,
        }})
        .to_string(),
        admissible: true,
    };
    let (id, digest) = reserve_and_claim_schema2(&api, &context);
    persist(&api, &completion, &context, &id, &digest, true);
    api.accept_leadership_review(&completion, &context, &id, &digest)
        .unwrap();
    let session = api
        .store
        .adaptive_session_for_authority(&context.binding.grant.assignee_authority)
        .unwrap()
        .unwrap();
    assert!(session.continuation.is_some());
    assert!(matches!(session.cursor, AdaptiveCursorV1::ReadyForModel));
    let project = api
        .store
        .company_project(
            &context.source.source_project.tenant_id,
            &context.source.source_project.project_id,
        )
        .unwrap()
        .unwrap();
    let active = binding(&project);
    assert_ne!(active.reservation_id, stale.reservation_id);
    assert_eq!(
        active.reservation_id,
        session.active_provider_allowance_id()
    );
    let before = discovery_state(&path, &events);
    reset();
    for boundary in 1..=2 {
        assert_eq!(
            api.binding_has_continued_adaptive_session(&active),
            Ok(true)
        );
        assert_eq!(
            api.adaptive_subscription_queue_order(&active),
            Ok(Some((1, session.updated_at_ms))),
        );
        let (authority, reconciled) = api
            .adaptive_provider_authority_from_binding(active.clone(), false)
            .unwrap();
        let authority = authority.unwrap();
        assert!(!reconciled);
        assert_eq!(authority.grant, session.effective_grant());
        assert_eq!(authority.session_version, session.version);
        assert_eq!(authority.assignment_id, active.assignment_id);
        assert_eq!(
            api.binding_has_continued_adaptive_session(&stale),
            Err("adaptive continuation allowance changed"),
        );
        assert_eq!(
            api.adaptive_subscription_queue_order(&stale),
            Err("adaptive continuation allowance changed"),
        );
        assert_eq!(
            api.adaptive_provider_authority_from_binding(stale.clone(), false),
            Err("adaptive allowance changed"),
        );
        assert_counts([2 * boundary, 2 * boundary, 2 * boundary, 0, 0]);
        assert_eq!(discovery_state(&path, &events), before);
    }
}

#[test]
fn adaptive_read_cohorts_external_allowance_change_precedes_corrupt_head() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("company.sqlite");
    let events = temp.path().join("events.sqlite");
    let (api, expected, _) = configured_adaptive_test_api(&path, &events);
    let mut project = project(&api, &expected);
    let binding = binding(&project);
    reset();
    assert_eq!(
        api.adaptive_provider_authority_from_binding(binding.clone(), false),
        Ok((Some(expected.clone()), false)),
    );
    let original = project.clone();
    for boundary in 1..=2 {
        project.subscription_call.as_mut().unwrap().grant.max_calls += 1;
        persist_discovery_project(&path, &project);
        let before = discovery_state(&path, &events);
        assert_eq!(
            api.adaptive_provider_authority_from_binding(binding.clone(), false),
            Err("adaptive allowance is already consumed or changed"),
        );
        assert_eq!(discovery_state(&path, &events), before);
        persist_discovery_project(&path, &original);
        let before = discovery_state(&path, &events);
        assert_eq!(
            api.adaptive_provider_authority_from_binding(binding.clone(), false),
            Ok((Some(expected.clone()), false)),
        );
        assert_counts([0, 0, 1 + 2 * boundary, 0, 0]);
        assert_eq!(discovery_state(&path, &events), before);
    }
    persist_discovery_project(&path, &project);
    corrupt_head(&path, expected.grant.session_id);
    let before = discovery_state(&path, &events);
    assert_eq!(
        api.adaptive_provider_authority_from_binding(binding, false),
        Err("adaptive allowance is already consumed or changed"),
    );
    assert_counts([0, 0, 6, 0, 0]);
    assert_eq!(discovery_state(&path, &events), before);
}

#[test]
fn adaptive_read_cohorts_reacquire_after_repeated_external_head_corruption_and_repair() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("company.sqlite");
    let events = temp.path().join("events.sqlite");
    let (api, expected, _) = configured_adaptive_test_api(&path, &events);
    let binding = binding(&project(&api, &expected));
    let session = api
        .store
        .adaptive_session(expected.grant.session_id, &expected.grant.authority)
        .unwrap()
        .unwrap();
    let namespace = format!("adaptive-session-v1:{}", expected.grant.session_id);
    let connection = rusqlite::Connection::open(&path).unwrap();
    let digests: Vec<(String, String)> = connection
        .prepare(
            "SELECT operation_id,request_digest FROM workflow_operations WHERE operation_namespace=?1 ORDER BY operation_id",
        )
        .unwrap()
        .query_map([&namespace], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(!digests.is_empty());
    reset();
    for boundary in 1..=2 {
        corrupt_head(&path, expected.grant.session_id);
        let before = discovery_state(&path, &events);
        assert_eq!(
            api.adaptive_subscription_queue_order(&binding),
            Err("adaptive queue session unavailable"),
        );
        assert_eq!(
            api.binding_has_continued_adaptive_session(&binding),
            Err("adaptive continuation head unavailable"),
        );
        assert_eq!(
            api.adaptive_provider_authority_from_binding(binding.clone(), false),
            Err("adaptive continuation head unavailable"),
        );
        assert_eq!(discovery_state(&path, &events), before);
        for (operation, digest) in &digests {
            assert_eq!(
                connection
                    .execute(
                        "UPDATE workflow_operations SET request_digest=?3 WHERE operation_namespace=?1 AND operation_id=?2",
                        rusqlite::params![namespace, operation, digest],
                    )
                    .unwrap(),
                1,
            );
        }
        let before = discovery_state(&path, &events);
        assert_eq!(
            api.adaptive_subscription_queue_order(&binding),
            Ok(Some((2, session.updated_at_ms))),
        );
        assert_eq!(
            api.binding_has_continued_adaptive_session(&binding),
            Ok(false)
        );
        assert_eq!(
            api.adaptive_provider_authority_from_binding(binding.clone(), false),
            Ok((Some(expected.clone()), false)),
        );
        assert_counts([2 * boundary, 2 * boundary, 2 * boundary, 0, 0]);
        assert_eq!(discovery_state(&path, &events), before);
    }
}
