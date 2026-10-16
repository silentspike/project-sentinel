//! Sealed-history fixtures and operation counts only, not execution or latency evidence.
use super::*;
use crate::AdaptiveLeadershipReviewSubjectV2;
use std::cell::RefCell;

type Hook = Box<dyn FnOnce(&Connection) -> Result<(), WorkflowError>>;

#[derive(Default)]
struct Controls {
    active: bool,
    legacy: bool,
    authentications: usize,
    consumers: usize,
    after: Option<Hook>,
}

thread_local! {
    static CONTROLS: RefCell<Controls> = const { RefCell::new(Controls {
        active: false,
        legacy: false,
        authentications: 0,
        consumers: 0,
        after: None,
    }) };
}

fn observe<T>(legacy: bool, after: Option<Hook>, action: impl FnOnce() -> T) -> (T, usize, usize) {
    struct Restore(Option<Controls>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CONTROLS.with(|slot| *slot.borrow_mut() = self.0.take().unwrap());
        }
    }
    let previous = CONTROLS.with(|slot| {
        slot.replace(Controls {
            active: true,
            legacy,
            after,
            ..Controls::default()
        })
    });
    let _restore = Restore(Some(previous));
    let result = action();
    let (authentications, consumers) = CONTROLS.with(|slot| {
        let state = slot.borrow();
        (state.authentications, state.consumers)
    });
    (result, authentications, consumers)
}

pub(super) fn record_authentication() {
    CONTROLS.with(|slot| {
        let mut state = slot.borrow_mut();
        if state.active {
            state.authentications += 1;
        }
    });
}

pub(super) fn after_prologue(connection: &Connection) -> Result<(), WorkflowError> {
    let after = CONTROLS.with(|slot| slot.borrow_mut().after.take());
    after.map_or(Ok(()), |after| after(connection))
}

pub(super) fn observe_consumer(
    tenant: &TenantId,
    event_type: &str,
    owner: &CompanyEventShape,
    call: &AdaptiveLeadershipReviewCallV1,
) -> Result<(), WorkflowError> {
    let legacy = CONTROLS.with(|slot| {
        let mut state = slot.borrow_mut();
        if state.active {
            state.consumers += 1;
        }
        state.legacy
    });
    if !legacy {
        return Ok(());
    }
    record_authentication();
    // The removed block, pinned from 155aa73a, is the differential oracle.
    call.validate_entity()?;
    let digest = canonical_sha256("sentinel.workflow.adaptive-leadership-call.v1", call)?;
    let principal = &call.grant.leadership_principal;
    let payload_digest =
        bytes_digest("sentinel.workflow.company-event-payload.v1", &owner.payload)?;
    let authority_digest = principal.binding_digest()?;
    let event_id = canonical_sha256(
        "sentinel.workflow.company-event-id.v1",
        &(
            tenant,
            Some(&call.grant.project_id),
            event_type,
            call.operation_id,
            &digest,
            &authority_digest,
            &payload_digest,
            call.updated_at_unix_ms,
        ),
    )?;
    if company_event_principal(owner)? != *principal
        || owner.tenant_id != tenant.0
        || owner.project_id.as_deref() != Some(call.grant.project_id.0.as_str())
        || owner.event_type != event_type
        || owner.operation_id != call.operation_id.to_string()
        || owner.operation_digest != digest
        || owner.authority_binding_digest != authority_digest
        || owner.payload_digest != payload_digest
        || owner.event_id != event_id
        || stored_u64(owner.created_at_ms)? != call.updated_at_unix_ms
    {
        return Err(corrupt());
    }
    Ok(())
}

const NOW: u64 = crate::adaptive::continuation_tests::NOW;
const ARENA_BYTES: usize = 64 * 1024 * 1024;
const TRACE_EVENT: &str = "adaptive_leadership_review_authorized";

fn call_fixture(seed: u128) -> AdaptiveLeadershipReviewCallV1 {
    let assignment: crate::AssignmentV1 = serde_json::from_value(serde_json::json!({
        "assignment_id": "assignment-01", "agent_id": 7, "role": "developer",
        "specialties": ["rust"],
        "profile": {"profile_id": "coding-agent-v1", "generation": 2, "digest": "3".repeat(64)},
        "organization_generation": 9, "organization_digest": "2".repeat(64),
        "assignment_version": 1, "delegated_by": null, "reason_ref": "Inspect source",
        "active": true, "assigned_by": "pm-01", "created_at_unix_ms": NOW,
        "ended_at_unix_ms": null,
    }))
    .unwrap();
    let mut root_grant = crate::adaptive::continuation_tests::grant();
    root_grant.session_id = Uuid::from_u128(seed);
    root_grant.max_call_duration_ms = 2_000;
    root_grant.deadline_ms = NOW + 2_000;
    root_grant.authority.assignment_version = 1;
    root_grant.authority.assignment_digest = assignment.canonical_digest().unwrap();
    let root = crate::SubscriptionCallAllowanceV1 {
        allowance_id: root_grant.provider_allowance_id.clone(),
        grant: crate::SubscriptionCallGrantV1 {
            schema_version: 1,
            work_item_id: root_grant.authority.work_item_id.clone(),
            assignment_id: assignment.assignment_id.clone(),
            assignment_version: 1,
            agent_id: root_grant.authority.agent_id,
            provider: root_grant.provider.clone(),
            model: root_grant.model.clone(),
            catalog_digest: root_grant.catalog_digest.clone(),
            max_calls: root_grant.max_model_calls,
            max_concurrent: 1,
            max_duration_ms: root_grant.max_call_duration_ms,
            token_policy: SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,
            expires_at_unix_ms: root_grant.deadline_ms,
        },
        created_by: "pm-01".into(),
        created_at_unix_ms: NOW,
        dispatch: None,
    };
    root_grant.provider_authority_digest =
        crate::adaptive_continuation_provider_digest(&root, &root_grant.authority).unwrap();
    let session = crate::AdaptiveSessionV1::initial(root_grant).unwrap();
    let epoch = crate::adaptive::work_funding_tests::epoch_for(&session, seed + 10_000);
    let budget = crate::AdaptiveBudgetWindowAuthorityV1 {
        schema_version: 1,
        root_allowance: root.clone(),
        active_allowance_digest: crate::adaptive_budget_allowance_digest(&root).unwrap(),
        continuation_history_digest: crate::adaptive_budget_history_digest(&None).unwrap(),
        observed_at_ms: NOW + 2_001,
        model_calls_exhausted: false,
        deadline_expired: true,
        dispatch_slack_insufficient: false,
    };
    let project: ProjectV1 = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "tenant_id": "tenant-01", "project_id": "project-01",
        "agreement_id": "agreement-01", "agreement_digest": "a".repeat(64),
        "governance": {"owner": 1, "participants": [{
            "agent_id": 1, "principal_id": "pm-01", "role": "project_manager",
            "specialties": ["planning"], "reports_to": null,
            "profile": {"profile_id": "pm-v1", "generation": 1, "digest": "a".repeat(64)}
        }, {
            "agent_id": 7, "principal_id": "agent-07", "role": "developer",
            "specialties": ["rust"], "reports_to": 1,
            "profile": {"profile_id": "coding-agent-v1", "generation": 2,
                "digest": "3".repeat(64)}
        }], "project_profile": {"profile_id": "project-v1", "generation": 6,
            "digest": "5".repeat(64)}},
        "cost_ceiling_micros": 100, "provider_cost_ceilings_micros": {"codex-cli": 100},
        "lifecycle_state": "active", "reserved_cost_micros": 0, "committed_cost_micros": 0,
        "work_items": {"work-01": {
            "spec": {"work_item_id": "work-01", "title": "Inspect source",
                "objective": "Inspect accepted source", "required_role": "developer",
                "required_specialties": ["rust"], "dependency_ids": [], "owner": 7,
                "inputs": [], "outputs": [{"name": "source",
                    "media_type": "application/octet-stream", "digest_algorithm": "sha256",
                    "contract_generation": 1, "contract_digest": "a".repeat(64)}],
                "quality_gate": {"gate_id": "source-qa-v1", "generation": 1,
                    "digest": "a".repeat(64)}, "budget_micros": 100, "rework": null},
            "state": "assigned", "version": 1, "assignments": [assignment],
            "output_receipts": [], "gate_receipt": null, "transition_history": [{
                "before": "Ready", "after": "Assigned", "actor_id": "pm-01",
                "actor_agent_id": 1, "reason_ref": "Inspect source",
                "occurred_at_unix_ms": NOW
            }]
        }},
        "decisions": [], "handoffs": [], "blockers": [], "approvals": [], "reservations": [],
        "subscription_call": root, "rooms": [], "questions": [], "actions": [],
        "version": 1, "created_at_unix_ms": NOW, "updated_at_unix_ms": NOW,
    }))
    .unwrap();
    let evidence_refs = vec![
        format!(
            "adaptive-budget-root:{}:{}",
            budget.root_allowance.allowance_id, session.grant.provider_authority_digest
        ),
        format!("adaptive-budget-current:{}", budget.active_allowance_digest),
        format!(
            "adaptive-budget-history:{}",
            budget.continuation_history_digest
        ),
        epoch.evidence_ref().unwrap(),
    ];
    let context: AdaptiveLeadershipReviewContextV1 = serde_json::from_value(serde_json::json!({
        "source_project": project,
        "source_session": session,
        "tool_catalog": {"tools": ["list_directory"]},
        "evidence_refs": evidence_refs,
    }))
    .unwrap();
    let fingerprint = crate::adaptive_leadership_evidence_fingerprint(
        &context.tool_catalog,
        &context.evidence_refs,
    )
    .unwrap();
    let leadership_authority = crate::PrincipalAuthorityV1::derive("pm-01", 1, &[1; 32]).unwrap();
    let grant = AdaptiveLeadershipReviewGrantV1 {
        schema_version: 5,
        review_id: crate::adaptive_leadership_review_id(
            context.source_session.grant.session_id,
            1,
            &fingerprint,
        )
        .unwrap(),
        project_id: context.source_project.project_id.clone(),
        expected_project_version: 1,
        work_item_id: context.source_session.grant.authority.work_item_id.clone(),
        session_id: context.source_session.grant.session_id,
        expected_session_version: 1,
        expected_reason_code: String::new(),
        evidence_fingerprint: fingerprint,
        leadership_principal: AuthenticatedCompanyPrincipalV1 {
            schema_version: 1,
            tenant_id: context.source_project.tenant_id.clone(),
            principal_id: "pm-01".into(),
            kind: CompanyPrincipalKindV1::Agent,
            role: CompanyRoleV1::ProjectManager,
            customer_id: None,
            agent_id: Some(crate::AgentId(1)),
            authority_generation: 1,
            authority_digest: leadership_authority.authority_digest.clone(),
        },
        leadership_authority,
        assignment_id: "assignment-01".into(),
        assignee_authority: context.source_session.grant.authority.clone(),
        provider: context.source_session.grant.provider.clone(),
        model: context.source_session.grant.model.clone(),
        catalog_digest: context.source_session.grant.catalog_digest.clone(),
        max_duration_ms: 2_000,
        token_policy: SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,
        expires_at_unix_ms: NOW + 50_000,
        subject: Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted {
            budget: Box::new(budget),
        }),
        recovery_epoch: None,
        funded_review_recovery: None,
        resume_policy: None,
        work_funding: Some(Box::new(epoch)),
    };
    let call = AdaptiveLeadershipReviewCallV1 {
        schema_version: 5,
        review_key: grant.review_id.to_string(),
        allowance_id: format!("history-only-review-{seed}"),
        operation_id: Uuid::from_u128(seed + 20_000),
        grant,
        context,
        version: 1,
        created_at_unix_ms: NOW + 2_001,
        grant_issued_at_unix_ms: NOW + 2_001,
        updated_at_unix_ms: NOW + 2_001,
        dispatch: None,
        decision: None,
        model_response_digest: None,
        resolution_event_id: None,
        retired_at_unix_ms: None,
        continuation: None,
    };
    call.validate_entity().unwrap();
    call
}

fn append_trace(connection: &mut Connection, call: &AdaptiveLeadershipReviewCallV1) -> u64 {
    call.validate_entity().unwrap();
    append_raw_trace(connection, call)
}

fn append_raw_trace(connection: &mut Connection, call: &AdaptiveLeadershipReviewCallV1) -> u64 {
    let transaction = connection.transaction().unwrap();
    let sequence = append_event(
        &transaction,
        &call.grant.leadership_principal,
        call.operation_id,
        &canonical_sha256("sentinel.workflow.adaptive-leadership-call.v1", call).unwrap(),
        Some(&call.grant.project_id),
        TRACE_EVENT,
        call,
        call.updated_at_unix_ms,
    )
    .unwrap();
    transaction.commit().unwrap();
    sequence
}

fn history(
    connection: &Connection,
    call: &AdaptiveLeadershipReviewCallV1,
    cut: u64,
) -> Result<(), WorkflowError> {
    require_pre_recovery_funding_history(
        connection,
        call.grant.work_funding.as_ref().unwrap(),
        Some((
            &call.grant,
            &call.context,
            call.operation_id,
            call.grant_issued_at_unix_ms,
            cut,
        )),
    )
}

fn store() -> (tempfile::TempDir, std::path::PathBuf, WorkflowStore) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("event-auth.sqlite");
    let store = WorkflowStore::open(&path).unwrap();
    (temp, path, store)
}

#[test]
fn mandatory_prologue_removes_only_h_times_d_event_authentications() {
    let (_temp, _path, store) = store();
    let mut connection = store.connection.lock().unwrap();
    let selected = call_fixture(1);
    for seed in 1..=3 {
        append_trace(&mut connection, &call_fixture(seed));
    }
    let mut maps = Vec::new();
    for legacy in [false, true] {
        let (((result, scopes), auth, consumers), digests) =
            crate::digest::with_digest_diagnostics(|| {
                observe(legacy, None, || {
                    validation_scope::with_completed_validations(|| {
                        validation_scope::with_scope(&connection, || {
                            for cut in 0..4 {
                                history(&connection, &selected, cut)?;
                            }
                            Ok(())
                        })
                    })
                })
            });
        result.unwrap();
        assert_eq!(consumers, 3 * 4);
        assert_eq!(auth, if legacy { 3 * (4 + 1) } else { 3 });
        let calls = digests
            .iter()
            .find(|entry| entry.domain == Some("sentinel.workflow.adaptive-leadership-call.v1"))
            .unwrap();
        assert_eq!(calls.canonical.calls, auth as u64);
        assert!(calls.canonical.serialized_bytes > 0);
        assert_eq!(scopes.len(), 1);
        assert_eq!(scopes[0].get("unused-funding-review-traces"), Some(&1));
        assert_eq!(scopes[0].get("event-shape"), Some(&3));
        assert_eq!(scopes[0].get("funded-recovery-history"), Some(&4));
        maps.push(scopes);
    }
    assert_eq!(maps[0], maps[1], "no new proof slots or validation nodes");
    assert!(connection.is_autocommit());
}

#[test]
fn exact_full_context_and_sequence_cut_still_decide_history_not_counters() {
    let (_temp, _path, store) = store();
    let mut connection = store.connection.lock().unwrap();
    let mut call = call_fixture(1);
    call.version = 2;
    call.updated_at_unix_ms += 1;
    call.dispatch = Some(crate::RequestProviderDispatchV1 {
        request_id: call.request_id(),
        request_digest: "a".repeat(64),
        context_digest: call.context_digest().unwrap(),
        dispatched_at_unix_ms: call.updated_at_unix_ms,
    });
    let sequence = append_trace(&mut connection, &call);
    let mut different = call.clone();
    different
        .context
        .source_project
        .work_items
        .get_mut(&different.grant.work_item_id)
        .unwrap()
        .spec
        .title = "Different exact source bytes".into();
    different.context.validate(&different.grant).unwrap();
    assert_eq!(different.grant, call.grant);
    for (candidate, cut, accepted) in [
        (&call, 0, true),
        (&call, sequence, false),
        (&call, sequence + 1, false),
        (&different, 0, false),
    ] {
        let current = observe(false, None, || history(&connection, candidate, cut)).0;
        let old = observe(true, None, || history(&connection, candidate, cut)).0;
        assert_eq!(current, old);
        assert_eq!(current.is_ok(), accepted);
        if !accepted {
            assert_eq!(
                current.unwrap_err().code,
                WorkflowErrorCode::InvalidTransition
            );
        }
    }
}

#[test]
fn unrelated_corrupt_event_fields_are_rejected_before_history_filtering() {
    for sql in [
        "UPDATE company_events SET payload=x'00' WHERE sequence=?1",
        "UPDATE company_events SET payload_digest='invalid' WHERE sequence=?1",
        "UPDATE company_events SET principal_id='other' WHERE sequence=?1",
        "UPDATE company_events SET project_id='other' WHERE sequence=?1",
        "UPDATE company_events SET operation_id='other' WHERE sequence=?1",
        "UPDATE company_events SET operation_digest='invalid' WHERE sequence=?1",
        "UPDATE company_events SET authority_binding_digest='invalid' WHERE sequence=?1",
        "UPDATE company_events SET event_id='other' WHERE sequence=?1",
        "UPDATE company_events SET event_type='adaptive_leadership_review_other' WHERE sequence=?1",
        "UPDATE company_events SET created_at_ms=created_at_ms+1 WHERE sequence=?1",
    ] {
        let (_temp, _path, store) = store();
        let mut connection = store.connection.lock().unwrap();
        let selected = call_fixture(1);
        append_trace(&mut connection, &selected);
        let unrelated = call_fixture(2);
        assert_ne!(unrelated.grant.session_id, selected.grant.session_id);
        let sequence = append_trace(&mut connection, &unrelated);
        connection.execute(sql, [sequence as i64]).unwrap();
        let (current, _, consumed) = observe(false, None, || history(&connection, &selected, 0));
        let old = observe(true, None, || history(&connection, &selected, 0)).0;
        assert_eq!(current, old, "{sql}");
        assert!(current.is_err(), "{sql}");
        assert_eq!(
            consumed, 0,
            "mandatory full authentication precedes filtering"
        );
        assert!(connection.is_autocommit());
    }
}

#[test]
fn unrelated_intrinsic_corruption_is_rejected_even_with_a_valid_event_seal() {
    let (_temp, _path, store) = store();
    let mut connection = store.connection.lock().unwrap();
    let selected = call_fixture(1);
    append_trace(&mut connection, &selected);
    let mut unrelated = call_fixture(2);
    unrelated.schema_version = 4;
    assert!(unrelated.validate_entity().is_err());
    append_raw_trace(&mut connection, &unrelated);
    let (current, auth, consumers) = observe(false, None, || history(&connection, &selected, 0));
    let old = observe(true, None, || history(&connection, &selected, 0)).0;
    assert_eq!(current, old);
    assert_eq!(current.unwrap_err().code, WorkflowErrorCode::CorruptStore);
    assert_eq!(auth, 2, "the unrelated decoded call must be validated too");
    assert_eq!(consumers, 0);
    assert!(connection.is_autocommit());
}

#[test]
fn leadership_event_inventory_overflow_still_precedes_payload_authentication() {
    let (_temp, _path, store) = store();
    let mut connection = store.connection.lock().unwrap();
    let call = call_fixture(1);
    append_trace(&mut connection, &call);
    // Deliberately invalid overflow rows prove the count gate runs before decoding.
    connection
        .execute(
            "WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<4096)
         INSERT INTO company_events(event_id,tenant_id,event_type,operation_id,
             operation_digest,principal_id,principal_kind,principal_role,
             authority_generation,authority_digest,authority_binding_digest,
             payload,payload_digest,created_at_ms)
         SELECT 'overflow-'||i,?1,?2,'invalid','invalid','invalid','invalid','invalid',
             1,'invalid','invalid',X'00','invalid',1 FROM n",
            params![call.grant.leadership_principal.tenant_id.0, TRACE_EVENT],
        )
        .unwrap();
    for legacy in [false, true] {
        let (result, auth, consumers) = observe(legacy, None, || history(&connection, &call, 0));
        assert_eq!(result.unwrap_err().code, WorkflowErrorCode::CorruptStore);
        assert_eq!((auth, consumers), (0, 0));
        assert!(connection.is_autocommit());
    }
}

#[test]
fn local_mutation_after_prologue_poisoned_scope_cannot_reuse_authentication() {
    for legacy in [false, true] {
        let (_temp, _path, store) = store();
        let mut connection = store.connection.lock().unwrap();
        let call = call_fixture(1);
        let sequence = append_trace(&mut connection, &call);
        let hook: Hook = Box::new(move |connection| {
            connection.execute(
                "UPDATE company_events SET payload_digest='invalid' WHERE sequence=?1",
                [sequence as i64],
            )?;
            Ok(())
        });
        let (result, _, consumers) = observe(legacy, Some(hook), || history(&connection, &call, 0));
        assert_eq!(result.unwrap_err().code, WorkflowErrorCode::CorruptStore);
        assert_eq!(consumers, 0);
        assert!(connection.is_autocommit());
        // Error cleanup rolled back the injected local write, not any business history.
        observe(legacy, None, || history(&connection, &call, 0))
            .0
            .unwrap();
    }
}

#[test]
fn external_mutation_is_pinned_until_scope_end_then_rejected_after_reopen() {
    for legacy in [false, true] {
        let (_temp, path, store) = store();
        let mut connection = store.connection.lock().unwrap();
        let call = call_fixture(1);
        let sequence = append_trace(&mut connection, &call);
        let writer = Connection::open(&path).unwrap();
        let hook: Hook = Box::new(move |_| {
            writer.execute(
                "UPDATE company_events SET payload_digest='invalid' WHERE sequence=?1",
                [sequence as i64],
            )?;
            Ok(())
        });
        observe(legacy, Some(hook), || history(&connection, &call, 0))
            .0
            .unwrap();
        assert_eq!(
            observe(legacy, None, || history(&connection, &call, 0))
                .0
                .unwrap_err()
                .code,
            WorkflowErrorCode::CorruptStore
        );
        drop(connection);
        let reopened = WorkflowStore::open(&path).unwrap();
        let connection = reopened.connection.lock().unwrap();
        assert_eq!(
            observe(legacy, None, || history(&connection, &call, 0))
                .0
                .unwrap_err()
                .code,
            WorkflowErrorCode::CorruptStore
        );
    }
}

fn budget_result(
    connection: &Connection,
    call: &AdaptiveLeadershipReviewCallV1,
    legacy: bool,
    available: usize,
) -> Result<(), WorkflowError> {
    observe(legacy, None, || {
        validation_scope::with_scope(connection, || {
            validation_scope::charge_bytes(connection, ARENA_BYTES - available)?;
            history(connection, call, 0)
        })
    })
    .0
}

#[test]
fn legacy_and_reused_auth_have_the_same_exact_validation_budget_boundary() {
    let (_temp, _path, store) = store();
    let mut connection = store.connection.lock().unwrap();
    let call = call_fixture(1);
    append_trace(&mut connection, &call);
    let mut thresholds = Vec::new();
    for legacy in [false, true] {
        let (mut low, mut high) = (0, ARENA_BYTES);
        budget_result(&connection, &call, legacy, high).unwrap();
        while low < high {
            let middle = low + (high - low) / 2;
            if budget_result(&connection, &call, legacy, middle).is_ok() {
                high = middle;
            } else {
                low = middle + 1;
            }
        }
        assert!(low > 0);
        budget_result(&connection, &call, legacy, low).unwrap();
        assert_eq!(
            budget_result(&connection, &call, legacy, low - 1)
                .unwrap_err()
                .code,
            WorkflowErrorCode::CorruptStore
        );
        thresholds.push(low);
    }
    assert_eq!(thresholds[0], thresholds[1]);
    assert!(connection.is_autocommit());
}
