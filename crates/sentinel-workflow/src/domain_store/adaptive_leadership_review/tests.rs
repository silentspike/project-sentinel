use super::*;
use crate::adaptive_leadership_review::*;
use crate::{
    AdaptiveEffectV1, AdaptiveModelDecisionV1, AdaptiveSessionGrantV1, AdaptiveSessionV1,
    AdaptiveTransitionV1, CompanyWorkItemSpecV1, PrincipalAuthorityV1, QualityGateBindingV1,
    RuntimeAuthoritySnapshotV1, TenantId, WorkOutputContractV1,
};
use sentinel_common::AgentId;
use std::collections::BTreeSet;
use std::sync::{Arc, Barrier};

const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const REASON: &str = "no_private_observation";
const AUTHORIZED_AT: u64 = 20;

struct Fixture {
    _temp: tempfile::TempDir,
    path: std::path::PathBuf,
    store: WorkflowStore,
    leader: AuthenticatedCompanyPrincipalV1,
    grant: AdaptiveLeadershipReviewGrantV1,
    context: AdaptiveLeadershipReviewContextV1,
    pending_planning: crate::ProjectPlanningCallV1,
}

fn fixture() -> Fixture {
    let (temp, path, store, _customer, mut project) =
        crate::domain_store::tests::accepted_project_fixture();
    let leadership_authority = PrincipalAuthorityV1::derive("pm-a", 1, &[1; 32]).unwrap();
    let leader = AuthenticatedCompanyPrincipalV1 {
        schema_version: 1,
        tenant_id: project.tenant_id.clone(),
        principal_id: "pm-a".into(),
        kind: CompanyPrincipalKindV1::Agent,
        role: CompanyRoleV1::ProjectManager,
        customer_id: None,
        agent_id: Some(AgentId(1)),
        authority_generation: 1,
        authority_digest: leadership_authority.authority_digest.clone(),
    };
    let work_item_id = WorkItemId::parse("leadership-build-work").unwrap();
    let planning = store
        .authorize_project_planning_call(
            &leader,
            Uuid::new_v4(),
            "leadership-planning",
            &crate::ProjectPlanningGrantV1 {
                schema_version: 1,
                project_id: project.project_id.clone(),
                expected_version: project.version,
                planner_principal: leader.clone(),
                provider: "codex-cli".into(),
                model: "model-test".into(),
                catalog_digest: DIGEST.into(),
                max_duration_ms: 120_000,
                token_policy: SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,
                expires_at_unix_ms: 10,
            },
            2,
        )
        .unwrap();
    let pending_planning = store
        .claim_project_planning_call(
            &leader,
            &crate::ClaimProjectPlanningCallV1 {
                allowance_id: planning.allowance_id.clone(),
                project_id: project.project_id.clone(),
                request_id: planning.request_id(),
                request_digest: DIGEST.into(),
                context_digest: DIGEST.into(),
            },
            2,
        )
        .unwrap();
    for step in 0..3 {
        let command = match step {
            0 => CompanyWorkflowCommandV1::PlanWorkGraph {
                project_id: project.project_id.clone(),
                expected_version: project.version,
                items: vec![CompanyWorkItemSpecV1 {
                    work_item_id: work_item_id.clone(),
                    title: "Build source artifact".into(),
                    objective: "Implement the accepted customer scope".into(),
                    required_role: CompanyRoleV1::Developer,
                    required_specialties: BTreeSet::from(["rust".into()]),
                    dependency_ids: BTreeSet::new(),
                    owner: AgentId(2),
                    inputs: vec![],
                    outputs: vec![WorkOutputContractV1 {
                        name: "source".into(),
                        media_type: "application/octet-stream".into(),
                        digest_algorithm: "sha256".into(),
                        contract_generation: 1,
                        contract_digest: DIGEST.into(),
                    }],
                    quality_gate: QualityGateBindingV1 {
                        gate_id: "web-work-item-qa-v1".into(),
                        generation: 1,
                        digest: DIGEST.into(),
                    },
                    budget_micros: 100,
                    rework: None,
                }],
            },
            1 => CompanyWorkflowCommandV1::ActivateProject {
                project_id: project.project_id.clone(),
                expected_version: project.version,
                reason_ref: "Accepted scope ready for execution".into(),
            },
            _ => CompanyWorkflowCommandV1::AssignWork {
                project_id: project.project_id.clone(),
                expected_version: project.version,
                work_item_id: work_item_id.clone(),
                agent_id: AgentId(2),
                organization_generation: 1,
                organization_digest: DIGEST.into(),
                reason_ref: "Developer owns implementation".into(),
            },
        };
        let result = store
            .apply_company_command(
                &leader,
                Uuid::from_u128(40 + step),
                &command,
                3 + step as u64,
            )
            .unwrap();
        let CompanyWorkflowResponseV1::Project(updated) = result.response else {
            panic!("expected persisted project");
        };
        project = *updated;
    }
    store
        .complete_project_planning_call(
            &leader,
            &project.project_id,
            &planning.allowance_id,
            DIGEST,
            DIGEST,
            &project,
            6,
        )
        .unwrap();
    let assignment = &project.work_items[&work_item_id].assignments[0];
    let assignee_authority = RuntimeAuthoritySnapshotV1 {
        schema_version: 1,
        tenant_id: project.tenant_id.clone(),
        project_id: project.project_id.clone(),
        work_item_id: work_item_id.clone(),
        agent_id: assignment.agent_id,
        assignment_version: assignment.assignment_version,
        assignment_digest: assignment.canonical_digest().unwrap(),
        organization_generation: assignment.organization_generation,
        organization_digest: assignment.organization_digest.clone(),
        principal: PrincipalAuthorityV1::derive("developer-a", 1, &[2; 32]).unwrap(),
        profile_id: assignment.profile.profile_id.clone(),
        profile_generation: assignment.profile.generation,
        profile_digest: assignment.profile.digest.clone(),
        runtime_key: "bwrap-coding-v1".into(),
        runtime_generation: 1,
        runtime_digest: DIGEST.into(),
        policy_generation: 1,
        policy_digest: DIGEST.into(),
        active: true,
        capabilities: BTreeSet::from(["file.inspect".into(), "observation.retain_private".into()]),
    };
    let session_grant = AdaptiveSessionGrantV1 {
        schema_version: 1,
        session_id: Uuid::new_v4(),
        authority: assignee_authority.clone(),
        provider_allowance_id: "developer-adaptive".into(),
        provider_authority_digest: DIGEST.into(),
        provider: "codex-cli".into(),
        model: "model-test".into(),
        catalog_digest: DIGEST.into(),
        max_output_tokens: 4096,
        max_call_duration_ms: 120_000,
        max_model_calls: 2,
        max_tool_calls: 2,
        created_at_ms: 10,
        deadline_ms: 600_000,
    };
    let (_, initial) = store
        .begin_adaptive_session(&session_grant, &assignee_authority, 10)
        .unwrap();
    let effect = AdaptiveEffectV1 {
        id: Uuid::new_v4(),
        request_digest: crate::digest::canonical_sha256(
            "leadership-test.model-request.v1",
            &session_grant,
        )
        .unwrap(),
    };
    let (_, claimed) = store
        .advance_adaptive_session(
            session_grant.session_id,
            initial.version,
            Uuid::new_v4(),
            &AdaptiveTransitionV1::ClaimModel {
                effect: effect.clone(),
                previous_observation_digest: None,
            },
            &assignee_authority,
            11,
        )
        .unwrap();
    let decision = AdaptiveModelDecisionV1::Blocked {
        reason_code: REASON.into(),
    };
    let result_digest =
        crate::digest::canonical_sha256("leadership-test.model-result.v1", &decision).unwrap();
    let (_, blocked) = store
        .advance_adaptive_session(
            session_grant.session_id,
            claimed.version,
            Uuid::new_v4(),
            &AdaptiveTransitionV1::ResolveModel {
                effect,
                result_digest: result_digest.clone(),
                decision,
            },
            &assignee_authority,
            12,
        )
        .unwrap();
    assert_eq!(
        blocked.last_model_result_digest,
        Some(result_digest.clone())
    );
    assert_eq!(
        blocked.cursor,
        AdaptiveCursorV1::Blocked {
            reason_code: REASON.into()
        }
    );
    let context = AdaptiveLeadershipReviewContextV1 {
        source_project: project.clone(),
        source_session: blocked.clone(),
        tool_catalog: serde_json::json!({"tools": [{"name": "file.inspect"}]}),
        evidence_refs: vec![
            format!("adaptive-model-result:{result_digest}"),
            "catalog:file.inspect".into(),
        ],
    };
    let fingerprint =
        adaptive_leadership_evidence_fingerprint(&context.tool_catalog, &context.evidence_refs)
            .unwrap();
    let grant = AdaptiveLeadershipReviewGrantV1 {
        schema_version: 1,
        review_id: adaptive_leadership_review_id(
            blocked.grant.session_id,
            blocked.version,
            &fingerprint,
        )
        .unwrap(),
        project_id: project.project_id.clone(),
        expected_project_version: project.version,
        work_item_id,
        session_id: blocked.grant.session_id,
        expected_session_version: blocked.version,
        expected_reason_code: REASON.into(),
        evidence_fingerprint: fingerprint,
        leadership_principal: leader.clone(),
        leadership_authority,
        assignment_id: assignment.assignment_id.clone(),
        assignee_authority,
        provider: "codex-cli".into(),
        model: "model-test".into(),
        catalog_digest: DIGEST.into(),
        max_duration_ms: 120_000,
        token_policy: crate::SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,
        expires_at_unix_ms: 200_000,
        subject: None,
        recovery_epoch: None,
        resume_policy: None,
        work_funding: None,
    };
    grant.validate(AUTHORIZED_AT).unwrap();
    context.validate(&grant).unwrap();
    assert_eq!(
        store
            .company_project(&leader.tenant_id, &grant.project_id)
            .unwrap(),
        Some(project)
    );
    assert_eq!(
        store
            .adaptive_session(grant.session_id, &grant.assignee_authority)
            .unwrap(),
        Some(blocked)
    );
    Fixture {
        _temp: temp,
        path,
        store,
        leader,
        grant,
        context,
        pending_planning,
    }
}

// Use the production row writer so tampering retains a legitimate entity checksum.
fn persist_entity<T: serde::Serialize + CompanyEntity>(store: &WorkflowStore, value: &T) {
    value.validate_entity().unwrap();
    let (tenant, kind, id, version) = value.row_binding();
    let mut connection = store.connection.lock().unwrap();
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    put_entity(&transaction, tenant, kind, id, version, value).unwrap();
    transaction.commit().unwrap();
}

fn planning(f: &Fixture) -> crate::ProjectPlanningCallV1 {
    f.store
        .project_planning_call(&f.leader.tenant_id, &f.grant.project_id)
        .unwrap()
        .unwrap()
}

fn tamper_planning_policy(f: &Fixture, change_model: bool) -> crate::ProjectPlanningCallV1 {
    let mut changed = planning(f);
    assert_eq!(changed.version, 3);
    assert!(changed.planned_project.is_some());
    assert!(changed.model_response_digest.is_some());
    if change_model {
        changed.grant.model = "different-model".into();
        assert_ne!(changed.grant.model, f.grant.model);
    } else {
        changed.grant.catalog_digest = "b".repeat(64);
        assert_ne!(changed.grant.catalog_digest, f.grant.catalog_digest);
    }
    persist_entity(&f.store, &changed);
    // A verified read must succeed: rejection below must come from lineage, not corruption.
    assert_eq!(planning(f), changed);
    changed
}

fn authorize(f: &Fixture) -> AdaptiveLeadershipReviewCallV1 {
    f.store
        .authorize_adaptive_leadership_review_call(
            &f.leader,
            Uuid::from_u128(100),
            "leadership-a",
            &f.grant,
            &f.context,
            AUTHORIZED_AT,
        )
        .unwrap()
}

#[test]
fn retirement_requires_actual_source_drift_and_exact_leader() {
    let f = fixture();
    let call = authorize(&f);
    let before = rows(&f.store);
    assert!(f
        .store
        .retire_stale_adaptive_leadership_review_call(
            &f.leader,
            call.grant.review_id,
            call.version,
            21,
        )
        .is_err());
    assert_eq!(rows(&f.store), before);
    change_project(&f, 21);
    let mut wrong = f.leader.clone();
    wrong.authority_generation += 1;
    let before = rows(&f.store);
    assert!(f
        .store
        .retire_stale_adaptive_leadership_review_call(
            &wrong,
            call.grant.review_id,
            call.version,
            22,
        )
        .is_err());
    assert!(f
        .store
        .retire_stale_adaptive_leadership_review_call(&f.leader, call.grant.review_id, 2, 22,)
        .is_err());
    assert_eq!(rows(&f.store), before);
}

#[test]
fn stale_dispatched_retirement_is_durable_without_fabricated_decision() {
    let f = fixture();
    let call = authorize(&f);
    let dispatched = f
        .store
        .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), 21)
        .unwrap();
    let original = session(&f);
    change_project(&f, 22);
    let retired = f
        .store
        .retire_stale_adaptive_leadership_review_call(
            &f.leader,
            call.grant.review_id,
            dispatched.version,
            23,
        )
        .unwrap();
    assert_eq!(retired.version, 4);
    assert_eq!(retired.retired_at_unix_ms, Some(23));
    assert_eq!(retired.dispatch, dispatched.dispatch);
    assert_eq!(retired.allowance_id, dispatched.allowance_id);
    assert!(retired.decision.is_none());
    assert!(retired.model_response_digest.is_none());
    assert!(retired.resolution_event_id.is_none());
    assert_eq!(session(&f), original);
    let reopened = WorkflowStore::open(&f.path).unwrap();
    let before = rows(&reopened);
    assert_eq!(
        reopened
            .retire_stale_adaptive_leadership_review_call(
                &f.leader,
                call.grant.review_id,
                dispatched.version,
                24,
            )
            .unwrap(),
        retired
    );
    assert!(reopened
        .complete_adaptive_leadership_review_call(&f.leader, &completion(&call, false), 24,)
        .is_err());
    assert!(reopened
        .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), 24,)
        .is_err());
    assert_eq!(rows(&reopened), before);
}

#[test]
fn committed_resolution_cannot_be_retired_after_project_drift() {
    let f = fixture();
    let call = authorize(&f);
    let dispatched = f
        .store
        .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), 21)
        .unwrap();
    let resolution = Uuid::new_v4();
    resolve(&f, resolution, 22);
    change_project(&f, 23);
    let before = rows(&f.store);
    assert!(f
        .store
        .retire_stale_adaptive_leadership_review_call(
            &f.leader,
            call.grant.review_id,
            dispatched.version,
            24,
        )
        .is_err());
    assert_eq!(rows(&f.store), before);
    let mut result = completion(&call, true);
    result.resolution_event_id = Some(resolution);
    let completed = f
        .store
        .complete_adaptive_leadership_review_call(&f.leader, &result, 24)
        .unwrap();
    assert_eq!(completed.resolution_event_id, Some(resolution));
    assert!(completed.retired_at_unix_ms.is_none());
}

#[test]
fn resolution_receipt_rechecks_relevant_project_lineage_in_its_transaction() {
    let f = fixture();
    let call = authorize(&f);
    f.store
        .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), 21)
        .unwrap();
    let resolution = Uuid::new_v4();
    resolve(&f, resolution, 22);
    let mut project = f.context.source_project.clone();
    project
        .work_items
        .get_mut(&f.grant.work_item_id)
        .unwrap()
        .spec
        .objective
        .push_str(" with changed scope");
    project.version += 1;
    project.updated_at_unix_ms = 23;
    persist_entity(&f.store, &project);
    let before = rows(&f.store);
    let mut result = completion(&call, true);
    result.resolution_event_id = Some(resolution);
    assert!(f
        .store
        .complete_adaptive_leadership_review_call(&f.leader, &result, 24)
        .is_err());
    assert_eq!(rows(&f.store), before);
}

#[test]
fn retired_reviews_free_pending_barrier_but_count_towards_head_limit() {
    let f = fixture();
    let original = session(&f);
    for index in 0..ADAPTIVE_LEADERSHIP_MAX_REVIEWS {
        let project = f
            .store
            .company_project(&f.leader.tenant_id, &f.grant.project_id)
            .unwrap()
            .unwrap();
        let mut context = f.context.clone();
        context.source_project = project.clone();
        context
            .evidence_refs
            .push(format!("project-version:{}", project.version));
        let mut grant = f.grant.clone();
        grant.expected_project_version = project.version;
        grant.evidence_fingerprint =
            adaptive_leadership_evidence_fingerprint(&context.tool_catalog, &context.evidence_refs)
                .unwrap();
        grant.review_id = adaptive_leadership_review_id(
            grant.session_id,
            grant.expected_session_version,
            &grant.evidence_fingerprint,
        )
        .unwrap();
        let now = 30 + index as u64 * 3;
        let call = f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                Uuid::new_v4(),
                &format!("retirement-{index}"),
                &grant,
                &context,
                now,
            )
            .unwrap();
        change_project(&f, now + 1);
        f.store
            .retire_stale_adaptive_leadership_review_call(
                &f.leader,
                call.grant.review_id,
                call.version,
                now + 2,
            )
            .unwrap();
    }
    let project = f
        .store
        .company_project(&f.leader.tenant_id, &f.grant.project_id)
        .unwrap()
        .unwrap();
    let mut context = f.context.clone();
    context.source_project = project.clone();
    context
        .evidence_refs
        .push(format!("project-version:{}", project.version));
    let mut grant = f.grant.clone();
    grant.expected_project_version = project.version;
    grant.evidence_fingerprint =
        adaptive_leadership_evidence_fingerprint(&context.tool_catalog, &context.evidence_refs)
            .unwrap();
    grant.review_id = adaptive_leadership_review_id(
        grant.session_id,
        grant.expected_session_version,
        &grant.evidence_fingerprint,
    )
    .unwrap();
    let before = rows(&f.store);
    assert!(f
        .store
        .authorize_adaptive_leadership_review_call(
            &f.leader,
            Uuid::new_v4(),
            "retirement-fourth",
            &grant,
            &context,
            50,
        )
        .is_err());
    assert_eq!(rows(&f.store), before);
    assert_eq!(session(&f), original);
}

fn claim(call: &AdaptiveLeadershipReviewCallV1) -> ClaimAdaptiveLeadershipReviewCallV1 {
    ClaimAdaptiveLeadershipReviewCallV1 {
        review_id: call.grant.review_id,
        allowance_id: call.allowance_id.clone(),
        request_id: call.request_id(),
        request_digest: DIGEST.into(),
        context_digest: call.context_digest().unwrap(),
    }
}

fn completion(
    call: &AdaptiveLeadershipReviewCallV1,
    resolve: bool,
) -> CompleteAdaptiveLeadershipReviewCallV1 {
    let rationale = "Reviewed the supplied blocked-model result and tool catalog".into();
    let evidence_refs = vec![call.context.evidence_refs[0].clone()];
    CompleteAdaptiveLeadershipReviewCallV1 {
        review_id: call.grant.review_id,
        allowance_id: call.allowance_id.clone(),
        request_digest: claim(call).request_digest,
        model_response_digest: "c".repeat(64),
        decision: AdaptiveLeadershipReviewDecisionV1 {
            schema_version: 1,
            decision: if resolve {
                AdaptiveLeadershipReviewDecisionKindV1::ResolveBlocked {
                    rationale,
                    evidence_refs,
                }
            } else {
                AdaptiveLeadershipReviewDecisionKindV1::KeepBlocked {
                    rationale,
                    evidence_refs,
                }
            },
        },
        resolution_event_id: None,
        continuation: None,
    }
}

fn session(f: &Fixture) -> AdaptiveSessionV1 {
    f.store
        .adaptive_session(f.grant.session_id, &f.grant.assignee_authority)
        .unwrap()
        .unwrap()
}

fn resolve(f: &Fixture, event: Uuid, now: u64) -> AdaptiveSessionV1 {
    f.store
        .advance_adaptive_session(
            f.grant.session_id,
            session(f).version,
            Uuid::new_v4(),
            &AdaptiveTransitionV1::ResolveBlocked {
                expected_reason_code: REASON.into(),
                resolution_event_id: event.to_string(),
            },
            &f.grant.assignee_authority,
            now,
        )
        .unwrap()
        .1
}

fn change_project(f: &Fixture, now: u64) {
    let current = f
        .store
        .company_project(&f.leader.tenant_id, &f.grant.project_id)
        .unwrap()
        .unwrap();
    f.store
        .apply_company_command(
            &f.leader,
            Uuid::new_v4(),
            &CompanyWorkflowCommandV1::RecordDecision {
                project_id: current.project_id,
                expected_version: current.version,
                work_item_id: Some(f.grant.work_item_id.clone()),
                choice_ref: "New project decision".into(),
                rationale_ref: "Current project differs from authorized context".into(),
            },
            now,
        )
        .unwrap();
}

// Compare durable rows, not just the returned error: rejection must not consume a call.
fn rows(store: &WorkflowStore) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    let connection = store.connection.lock().unwrap();
    [
        "company_entities",
        "company_operations",
        "company_events",
        "company_project_projections",
        "workflow_operations",
        "workflow_adaptive_heads",
    ]
    .iter()
    .map(|table| {
        let mut statement = connection
            .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
            .unwrap();
        let columns = statement.column_count();
        let mapped = statement
            .query_map([], |row| {
                (0..columns)
                    .map(|column| row.get(column))
                    .collect::<Result<Vec<_>, _>>()
            })
            .unwrap();
        mapped.collect::<Result<Vec<_>, _>>().unwrap()
    })
    .collect()
}

#[test]
fn durable_reopen_claim_is_single_use_and_exactly_bound() {
    let f = fixture();
    let call = authorize(&f);
    assert_eq!(call.review_key, f.grant.review_id.to_string());
    assert_eq!(
        call.context_digest().unwrap(),
        crate::digest::canonical_sha256(
            "sentinel.workflow.adaptive-leadership-context.v1",
            &(&call.grant, &call.context),
        )
        .unwrap()
    );
    let reopened = WorkflowStore::open(&f.path).unwrap();
    assert_eq!(
        reopened
            .adaptive_leadership_review_call(&f.leader.tenant_id, f.grant.review_id)
            .unwrap(),
        Some(call.clone())
    );
    assert_eq!(
        reopened
            .adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id)
            .unwrap(),
        vec![call.clone()]
    );
    assert!(reopened
        .adaptive_leadership_review_calls(&f.leader.tenant_id, Uuid::new_v4())
        .unwrap()
        .is_empty());
    let foreign_tenant = TenantId::parse("tenant-foreign").unwrap();
    assert_eq!(
        reopened
            .adaptive_leadership_review_call(&foreign_tenant, f.grant.review_id)
            .unwrap(),
        None
    );
    assert!(reopened
        .adaptive_leadership_review_calls(&foreign_tenant, f.grant.session_id)
        .unwrap()
        .is_empty());
    let mut bad_claims = vec![];
    let mut wrong = claim(&call);
    wrong.allowance_id = "foreign-allowance".into();
    bad_claims.push(wrong);
    let mut wrong = claim(&call);
    wrong.review_id = Uuid::new_v4();
    bad_claims.push(wrong);
    let mut wrong = claim(&call);
    wrong.request_id = "foreign-request".into();
    bad_claims.push(wrong);
    let mut wrong = claim(&call);
    wrong.request_digest = "not-a-digest".into();
    bad_claims.push(wrong);
    let mut wrong = claim(&call);
    wrong.context_digest = "not-a-digest".into();
    bad_claims.push(wrong);
    let mut wrong = claim(&call);
    // A syntactically valid digest is still not the authorized context binding.
    wrong.context_digest = if wrong.context_digest == "b".repeat(64) {
        "d".repeat(64)
    } else {
        "b".repeat(64)
    };
    bad_claims.push(wrong);
    for wrong in bad_claims {
        let before = rows(&reopened);
        assert!(reopened
            .claim_adaptive_leadership_review_call(&f.leader, &wrong, 21)
            .is_err());
        assert_eq!(rows(&reopened), before);
    }
    let mut stale_leader = f.leader.clone();
    stale_leader.authority_generation += 1;
    assert!(reopened
        .claim_adaptive_leadership_review_call(&stale_leader, &claim(&call), 21)
        .is_err());
    let mut wrong_authority = f.leader.clone();
    wrong_authority.authority_digest = "d".repeat(64);
    let before = rows(&reopened);
    assert!(reopened
        .claim_adaptive_leadership_review_call(&wrong_authority, &claim(&call), 21)
        .is_err());
    assert_eq!(rows(&reopened), before);
    let dispatched = reopened
        .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), 21)
        .unwrap();
    assert_eq!(dispatched.version, call.version + 1);
    assert_eq!(
        dispatched.dispatch.as_ref().unwrap().context_digest,
        claim(&call).context_digest
    );
    drop(reopened);
    let reopened = WorkflowStore::open(&f.path).unwrap();
    let before = rows(&reopened);
    assert!(reopened
        .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), 22)
        .is_err());
    assert_eq!(rows(&reopened), before);
    assert_eq!(
        reopened
            .adaptive_leadership_review_call(&f.leader.tenant_id, f.grant.review_id)
            .unwrap(),
        Some(dispatched)
    );
}

#[test]
fn renewal_requires_expired_undispatched_identical_context_and_grant() {
    for dispatched in [false, true] {
        let mut f = fixture();
        f.grant.expires_at_unix_ms = 30;
        let call = authorize(&f);
        if dispatched {
            f.store
                .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), 21)
                .unwrap();
        }
        let mut renewed_grant = f.grant.clone();
        renewed_grant.expires_at_unix_ms = 200_000;
        let before = rows(&f.store);
        assert!(f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                call.operation_id,
                &call.allowance_id,
                &renewed_grant,
                &f.context,
                29,
            )
            .is_err());
        assert_eq!(rows(&f.store), before);
        assert!(f
            .store
            .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), 30)
            .is_err());
        let reopened = WorkflowStore::open(&f.path).unwrap();
        let before = rows(&reopened);
        let result = reopened.authorize_adaptive_leadership_review_call(
            &f.leader,
            call.operation_id,
            &call.allowance_id,
            &renewed_grant,
            &f.context,
            31,
        );
        if dispatched {
            assert!(result.is_err());
            assert_eq!(rows(&reopened), before);
        } else {
            let renewed = result.unwrap();
            assert_eq!(renewed.version, call.version);
            assert_eq!(renewed.created_at_unix_ms, call.created_at_unix_ms);
            assert_eq!(renewed.grant_issued_at_unix_ms, 31);
            assert_eq!(renewed.grant, renewed_grant);
            assert_eq!(renewed.context, call.context);
            assert_ne!(
                renewed.context_digest().unwrap(),
                call.context_digest().unwrap()
            );
            let before = rows(&reopened);
            assert!(reopened
                .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), 32)
                .is_err());
            assert_eq!(rows(&reopened), before);
            reopened
                .claim_adaptive_leadership_review_call(&f.leader, &claim(&renewed), 32)
                .unwrap();
        }
    }
}

#[test]
fn fresh_grant_rejects_leadership_authority_digest_mismatch_without_reservation() {
    let f = fixture();
    let mut grant = f.grant.clone();
    grant.leadership_authority.authority_digest = "d".repeat(64);
    grant.leadership_authority.validate().unwrap();
    assert_eq!(
        grant.leadership_authority.principal_id,
        f.leader.principal_id
    );
    assert_eq!(
        grant.leadership_authority.principal_generation,
        f.leader.authority_generation
    );
    assert_ne!(
        grant.leadership_authority.authority_digest,
        f.leader.authority_digest
    );
    let before = rows(&f.store);
    assert_eq!(
        f.store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                Uuid::from_u128(100),
                "leadership-a",
                &grant,
                &f.context,
                AUTHORIZED_AT,
            )
            .unwrap_err()
            .code,
        WorkflowErrorCode::InvalidInput
    );
    assert_eq!(rows(&f.store), before);
    assert!(f
        .store
        .adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id)
        .unwrap()
        .is_empty());
    // Rejection did not consume the operation, allowance, or blocked-head slot.
    let accepted = authorize(&f);
    assert_eq!(accepted.grant, f.grant);
}

#[test]
fn leadership_policy_must_match_completed_project_planning() {
    for change in 0..3 {
        let f = fixture();
        let mut grant = f.grant.clone();
        match change {
            0 => grant.model = "different-model".into(),
            1 => grant.catalog_digest = "b".repeat(64),
            _ => grant.provider = "different-provider".into(),
        }
        assert!(f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                Uuid::new_v4(),
                "review-policy-negative",
                &grant,
                &f.context,
                AUTHORIZED_AT,
            )
            .is_err());
        assert!(f
            .store
            .adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id,)
            .unwrap()
            .is_empty());
    }
}

#[test]
fn renewal_rechecks_completed_planning_policy_after_checksum_valid_tamper() {
    for change_model in [false, true] {
        let mut f = fixture();
        f.grant.expires_at_unix_ms = 30;
        let call = authorize(&f);
        let original_planning = planning(&f);
        let changed = tamper_planning_policy(&f, change_model);
        let reopened = WorkflowStore::open(&f.path).unwrap();
        assert_eq!(
            reopened
                .project_planning_call(&f.leader.tenant_id, &f.grant.project_id)
                .unwrap(),
            Some(changed)
        );
        let mut renewed_grant = call.grant.clone();
        renewed_grant.expires_at_unix_ms = 200_000;
        renewed_grant.validate(31).unwrap();
        let before = rows(&reopened);
        assert_eq!(
            reopened
                .authorize_adaptive_leadership_review_call(
                    &f.leader,
                    call.operation_id,
                    &call.allowance_id,
                    &renewed_grant,
                    &call.context,
                    31,
                )
                .unwrap_err()
                .code,
            WorkflowErrorCode::AuthorityConflict
        );
        assert_eq!(rows(&reopened), before);
        assert_eq!(
            reopened
                .adaptive_leadership_review_call(&f.leader.tenant_id, call.grant.review_id)
                .unwrap(),
            Some(call.clone())
        );
        assert_eq!(session(&f), f.context.source_session);
        persist_entity(&reopened, &original_planning);
        let renewed = reopened
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                call.operation_id,
                &call.allowance_id,
                &renewed_grant,
                &call.context,
                31,
            )
            .unwrap();
        assert_eq!(renewed.version, call.version);
        assert_eq!(renewed.grant_issued_at_unix_ms, 31);
        assert_eq!(renewed.grant, renewed_grant);
        assert!(renewed.dispatch.is_none());
    }
}

#[test]
fn claim_rechecks_completed_planning_policy_after_checksum_valid_tamper() {
    for change_model in [false, true] {
        let f = fixture();
        let call = authorize(&f);
        let original_planning = planning(&f);
        let changed = tamper_planning_policy(&f, change_model);
        let reopened = WorkflowStore::open(&f.path).unwrap();
        assert_eq!(
            reopened
                .project_planning_call(&f.leader.tenant_id, &f.grant.project_id)
                .unwrap(),
            Some(changed)
        );
        let before = rows(&reopened);
        assert_eq!(
            reopened
                .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), 21)
                .unwrap_err()
                .code,
            WorkflowErrorCode::AuthorityConflict
        );
        assert_eq!(rows(&reopened), before);
        assert_eq!(
            reopened
                .adaptive_leadership_review_call(&f.leader.tenant_id, call.grant.review_id)
                .unwrap(),
            Some(call.clone())
        );
        assert_eq!(session(&f), f.context.source_session);
        persist_entity(&reopened, &original_planning);
        let dispatched = reopened
            .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), 21)
            .unwrap();
        assert_eq!(dispatched.version, 2);
        assert_eq!(dispatched.dispatch.unwrap().request_id, call.request_id());
    }
}

#[test]
fn review_duration_above_canonical_planning_bound_is_invalid_without_reservation() {
    let mut f = fixture();
    assert_eq!(planning(&f).grant.max_duration_ms, 120_000);
    assert_eq!(f.grant.max_duration_ms, 120_000);
    f.grant.validate(AUTHORIZED_AT).unwrap();
    f.grant.max_duration_ms = 120_001;
    // Canonical planning cannot have a smaller bound; this exceeds review validation too.
    assert_eq!(
        f.grant.validate(AUTHORIZED_AT).unwrap_err().code,
        WorkflowErrorCode::InvalidInput
    );
    let before = rows(&f.store);
    assert_eq!(
        f.store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                Uuid::from_u128(100),
                "leadership-a",
                &f.grant,
                &f.context,
                AUTHORIZED_AT,
            )
            .unwrap_err()
            .code,
        WorkflowErrorCode::InvalidInput
    );
    assert_eq!(rows(&f.store), before);
    assert!(f
        .store
        .adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id)
        .unwrap()
        .is_empty());
    assert_eq!(session(&f), f.context.source_session);
    f.grant.max_duration_ms = 120_000;
    let call = authorize(&f);
    assert_eq!(call.grant.max_duration_ms, 120_000);
}

#[test]
fn absent_or_incomplete_planning_rejects_authorization_renewal_and_claim_without_effects() {
    for absent in [false, true] {
        for stage in 0..3 {
            let mut f = fixture();
            f.grant.expires_at_unix_ms = 30;
            let call = (stage != 0).then(|| authorize(&f));
            let original_planning = planning(&f);
            if absent {
                assert_eq!(
                    f.store.connection.lock().unwrap().execute(
                        "DELETE FROM company_entities WHERE tenant_id=?1 AND entity_kind='project_planning_call' AND entity_id=?2",
                        rusqlite::params![f.leader.tenant_id.0, f.grant.project_id.0],
                    ).unwrap(),
                    1
                );
            } else {
                // This is the real dispatched-but-incomplete row captured by the fixture.
                assert_eq!(f.pending_planning.version, 2);
                assert!(f.pending_planning.dispatch.is_some());
                assert!(f.pending_planning.planned_project.is_none());
                assert!(f.pending_planning.model_response_digest.is_none());
                persist_entity(&f.store, &f.pending_planning);
            }
            let reopened = WorkflowStore::open(&f.path).unwrap();
            assert_eq!(
                reopened
                    .project_planning_call(&f.leader.tenant_id, &f.grant.project_id)
                    .unwrap(),
                if absent {
                    None
                } else {
                    Some(f.pending_planning.clone())
                }
            );
            let mut renewed_grant = f.grant.clone();
            renewed_grant.expires_at_unix_ms = 200_000;
            let before = rows(&reopened);
            let result = match &call {
                None => reopened.authorize_adaptive_leadership_review_call(
                    &f.leader,
                    Uuid::from_u128(100),
                    "leadership-a",
                    &f.grant,
                    &f.context,
                    AUTHORIZED_AT,
                ),
                Some(call) if stage == 1 => reopened.authorize_adaptive_leadership_review_call(
                    &f.leader,
                    call.operation_id,
                    &call.allowance_id,
                    &renewed_grant,
                    &call.context,
                    31,
                ),
                Some(call) => {
                    reopened.claim_adaptive_leadership_review_call(&f.leader, &claim(call), 21)
                }
            };
            assert_eq!(
                result.unwrap_err().code,
                if absent {
                    WorkflowErrorCode::NotFound
                } else {
                    WorkflowErrorCode::InvalidTransition
                }
            );
            assert_eq!(rows(&reopened), before);
            assert_eq!(
                reopened
                    .adaptive_leadership_review_call(&f.leader.tenant_id, f.grant.review_id)
                    .unwrap(),
                call
            );
            assert_eq!(session(&f), f.context.source_session);
            persist_entity(&reopened, &original_planning);
            let accepted = match stage {
                0 => authorize(&f),
                1 => reopened
                    .authorize_adaptive_leadership_review_call(
                        &f.leader,
                        Uuid::from_u128(100),
                        "leadership-a",
                        &renewed_grant,
                        &f.context,
                        31,
                    )
                    .unwrap(),
                _ => reopened
                    .claim_adaptive_leadership_review_call(&f.leader, &claim(&call.unwrap()), 21)
                    .unwrap(),
            };
            assert_eq!(accepted.version, if stage == 2 { 2 } else { 1 });
        }
    }
}

#[test]
fn corrupt_real_different_session_review_fails_closed_before_authorization() {
    let f = fixture();
    let other = fixture();
    let other_call = authorize(&other);
    assert_eq!(f.leader.tenant_id, other.leader.tenant_id);
    assert_ne!(f.grant.session_id, other_call.grant.session_id);
    // Seed a valid review produced by the production helpers for another real session.
    persist_entity(&f.store, &other_call);
    assert!(f
        .store
        .adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id)
        .unwrap()
        .is_empty());
    assert_eq!(
        f.store
            .adaptive_leadership_review_calls(&f.leader.tenant_id, other_call.grant.session_id)
            .unwrap(),
        vec![other_call.clone()]
    );
    let mut tampered = other_call.clone();
    tampered.allowance_id = "tampered-other-session".into();
    tampered.validate_entity().unwrap();
    let payload = serde_json::to_vec(&tampered).unwrap();
    // Keep a readable, different-session payload but preserve its original checksum.
    assert_eq!(f.store.connection.lock().unwrap().execute(
        "UPDATE company_entities SET payload=?1 WHERE tenant_id=?2 AND entity_kind=?3 AND entity_id=?4",
        rusqlite::params![payload, f.leader.tenant_id.0, KIND, other_call.review_key],
    ).unwrap(), 1);
    let reopened = WorkflowStore::open(&f.path).unwrap();
    let before = rows(&reopened);
    for session_id in [f.grant.session_id, other_call.grant.session_id] {
        assert_eq!(
            reopened
                .adaptive_leadership_review_calls(&f.leader.tenant_id, session_id)
                .unwrap_err()
                .code,
            WorkflowErrorCode::CorruptStore
        );
    }
    assert_eq!(
        reopened
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                Uuid::from_u128(100),
                "leadership-a",
                &f.grant,
                &f.context,
                AUTHORIZED_AT,
            )
            .unwrap_err()
            .code,
        WorkflowErrorCode::CorruptStore
    );
    assert_eq!(rows(&reopened), before);
    assert_eq!(
        reopened
            .adaptive_leadership_review_call(&f.leader.tenant_id, f.grant.review_id)
            .unwrap(),
        None
    );
    assert_eq!(session(&f), f.context.source_session);
    persist_entity(&reopened, &other_call);
    assert_eq!(authorize(&f).grant, f.grant);
}

#[test]
fn foreign_tenant_cannot_claim_or_renew_and_its_corruption_is_scan_isolated() {
    let mut f = fixture();
    f.grant.expires_at_unix_ms = 30;
    let call = authorize(&f);
    let mut foreign = f.leader.clone();
    foreign.tenant_id = TenantId::parse("tenant-foreign").unwrap();
    let mut renewed_grant = f.grant.clone();
    renewed_grant.expires_at_unix_ms = 200_000;
    let before = rows(&f.store);
    assert_eq!(
        f.store
            .adaptive_leadership_review_call(&foreign.tenant_id, call.grant.review_id)
            .unwrap(),
        None
    );
    assert!(f
        .store
        .adaptive_leadership_review_calls(&foreign.tenant_id, f.grant.session_id)
        .unwrap()
        .is_empty());
    assert_eq!(
        f.store
            .claim_adaptive_leadership_review_call(&foreign, &claim(&call), 21)
            .unwrap_err()
            .code,
        WorkflowErrorCode::NotFound
    );
    assert_eq!(
        f.store
            .authorize_adaptive_leadership_review_call(
                &foreign,
                call.operation_id,
                &call.allowance_id,
                &renewed_grant,
                &call.context,
                31,
            )
            .unwrap_err()
            .code,
        WorkflowErrorCode::AuthorityConflict
    );
    assert_eq!(rows(&f.store), before);
    // Deliberately corrupt row binding in another tenant must not poison this tenant's scan.
    assert_eq!(f.store.connection.lock().unwrap().execute(
        "INSERT INTO company_entities(tenant_id,entity_kind,entity_id,version,payload,payload_digest)
         SELECT ?1,entity_kind,entity_id,version,payload,payload_digest FROM company_entities
         WHERE tenant_id=?2 AND entity_kind=?3 AND entity_id=?4",
        rusqlite::params![foreign.tenant_id.0, f.leader.tenant_id.0, KIND, call.review_key],
    ).unwrap(), 1);
    let reopened = WorkflowStore::open(&f.path).unwrap();
    let before = rows(&reopened);
    assert_eq!(
        reopened
            .adaptive_leadership_review_calls(&foreign.tenant_id, f.grant.session_id)
            .unwrap_err()
            .code,
        WorkflowErrorCode::CorruptStore
    );
    assert_eq!(
        reopened
            .adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id)
            .unwrap(),
        vec![call.clone()]
    );
    assert_eq!(rows(&reopened), before);
    let renewed = reopened
        .authorize_adaptive_leadership_review_call(
            &f.leader,
            call.operation_id,
            &call.allowance_id,
            &renewed_grant,
            &call.context,
            31,
        )
        .unwrap();
    let dispatched = reopened
        .claim_adaptive_leadership_review_call(&f.leader, &claim(&renewed), 32)
        .unwrap();
    assert_eq!(dispatched.version, 2);
    assert_eq!(session(&f), f.context.source_session);
}

#[test]
fn tampered_payload_session_id_cannot_hide_prior_review_from_verified_scan() {
    let f = fixture();
    let call = authorize(&f);
    let hidden_session_id = Uuid::new_v4();
    let mut payload = serde_json::to_value(&call).unwrap();
    payload["grant"]["session_id"] = serde_json::json!(hidden_session_id);
    let payload = serde_json::to_vec(&payload).unwrap();
    // Preserve the original row digest: filtering unverified JSON first used
    // to hide this corrupt row from the genuine source session's review budget.
    assert_eq!(f.store.connection.lock().unwrap().execute(
        "UPDATE company_entities SET payload=?1 WHERE tenant_id=?2 AND entity_kind=?3 AND entity_id=?4",
        rusqlite::params![payload, f.leader.tenant_id.0, KIND, call.review_key],
    ).unwrap(), 1);
    let reopened = WorkflowStore::open(&f.path).unwrap();
    let before = rows(&reopened);
    assert_eq!(
        reopened
            .adaptive_leadership_review_call(&f.leader.tenant_id, call.grant.review_id,)
            .unwrap_err()
            .code,
        WorkflowErrorCode::CorruptStore
    );
    for session_id in [f.grant.session_id, hidden_session_id, Uuid::new_v4()] {
        assert_eq!(
            reopened
                .adaptive_leadership_review_calls(&f.leader.tenant_id, session_id,)
                .unwrap_err()
                .code,
            WorkflowErrorCode::CorruptStore
        );
    }
    let mut context = f.context.clone();
    context.tool_catalog =
        serde_json::json!({"tools": [{"name": "file.inspect", "catalog_version": 2}]});
    let mut grant = f.grant.clone();
    grant.evidence_fingerprint =
        adaptive_leadership_evidence_fingerprint(&context.tool_catalog, &context.evidence_refs)
            .unwrap();
    grant.review_id = adaptive_leadership_review_id(
        grant.session_id,
        grant.expected_session_version,
        &grant.evidence_fingerprint,
    )
    .unwrap();
    assert_ne!(grant.review_id, call.grant.review_id);
    grant.validate(21).unwrap();
    context.validate(&grant).unwrap();
    assert_eq!(
        reopened
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                Uuid::from_u128(101),
                "leadership-hidden-prior",
                &grant,
                &context,
                21,
            )
            .unwrap_err()
            .code,
        WorkflowErrorCode::CorruptStore
    );
    assert_eq!(rows(&reopened), before);
    assert_eq!(
        reopened
            .adaptive_leadership_review_call(&f.leader.tenant_id, grant.review_id,)
            .unwrap(),
        None
    );
    assert_eq!(session(&f), f.context.source_session);
}

#[test]
fn changed_grant_context_operation_or_allowance_cannot_rebind_review() {
    let f = fixture();
    let call = authorize(&f);
    assert_eq!(
        f.store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                call.operation_id,
                &call.allowance_id,
                &f.grant,
                &f.context,
                21,
            )
            .unwrap(),
        call
    );
    let mut changed_grants = vec![];
    let mut grant = f.grant.clone();
    grant.model = "changed-model".into();
    changed_grants.push(grant);
    let mut grant = f.grant.clone();
    grant.assignee_authority.policy_generation += 1;
    changed_grants.push(grant);
    let mut grant = f.grant.clone();
    grant.leadership_authority.authority_digest = "d".repeat(64);
    changed_grants.push(grant);
    for grant in changed_grants {
        let before = rows(&f.store);
        assert!(f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                call.operation_id,
                &call.allowance_id,
                &grant,
                &f.context,
                200_001,
            )
            .is_err());
        assert_eq!(rows(&f.store), before);
    }
    let mut context = f.context.clone();
    context.source_project.version += 1;
    let mut changed_evidence = f.context.clone();
    changed_evidence.evidence_refs.push("new-evidence".into());
    let mut changed_catalog = f.context.clone();
    changed_catalog.tool_catalog = serde_json::json!({"tools": ["changed-tool"]});
    for context in [context, changed_evidence, changed_catalog] {
        let before = rows(&f.store);
        assert!(f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                call.operation_id,
                &call.allowance_id,
                &f.grant,
                &context,
                200_001,
            )
            .is_err());
        assert_eq!(rows(&f.store), before);
    }
    for (operation, allowance) in [
        (Uuid::new_v4(), call.allowance_id.as_str()),
        (call.operation_id, "changed-allowance"),
    ] {
        let before = rows(&f.store);
        assert!(f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader, operation, allowance, &f.grant, &f.context, 21,
            )
            .is_err());
        assert_eq!(rows(&f.store), before);
    }
}

#[test]
fn concurrent_connections_serialize_the_same_claim() {
    let f = fixture();
    let call = authorize(&f);
    let stores = [
        WorkflowStore::open(&f.path).unwrap(),
        WorkflowStore::open(&f.path).unwrap(),
    ];
    let barrier = Arc::new(Barrier::new(2));
    let results = std::thread::scope(|scope| {
        let handles: Vec<_> = stores
            .into_iter()
            .map(|store| {
                let barrier = barrier.clone();
                let leader = f.leader.clone();
                let claim = claim(&call);
                scope.spawn(move || {
                    barrier.wait();
                    store.claim_adaptive_leadership_review_call(&leader, &claim, 21)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let winner = results.into_iter().find_map(Result::ok).unwrap();
    assert_eq!(
        f.store
            .adaptive_leadership_review_call(&f.leader.tenant_id, f.grant.review_id)
            .unwrap(),
        Some(winner)
    );
    assert_eq!(
        f.store
            .adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn new_evidence_cannot_dispatch_while_prior_review_is_unresolved_or_renew_completed_call() {
    let mut f = fixture();
    f.grant.expires_at_unix_ms = 30;
    let call = authorize(&f);
    let mut context = f.context.clone();
    context.tool_catalog =
        serde_json::json!({"tools": [{"name": "file.inspect", "catalog_version": 2}]});
    let mut grant = f.grant.clone();
    grant.expires_at_unix_ms = 200_000;
    grant.evidence_fingerprint =
        adaptive_leadership_evidence_fingerprint(&context.tool_catalog, &context.evidence_refs)
            .unwrap();
    grant.review_id = adaptive_leadership_review_id(
        grant.session_id,
        grant.expected_session_version,
        &grant.evidence_fingerprint,
    )
    .unwrap();
    for dispatched in [false, true] {
        if dispatched {
            f.store
                .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), 21)
                .unwrap();
        }
        let before = rows(&f.store);
        assert!(f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                Uuid::from_u128(101),
                "leadership-new-evidence",
                &grant,
                &context,
                22,
            )
            .is_err());
        assert_eq!(rows(&f.store), before);
    }
    f.store
        .complete_adaptive_leadership_review_call(&f.leader, &completion(&call, false), 31)
        .unwrap();
    let mut renewed = f.grant.clone();
    renewed.expires_at_unix_ms = 200_000;
    let before = rows(&f.store);
    assert!(f
        .store
        .authorize_adaptive_leadership_review_call(
            &f.leader,
            call.operation_id,
            &call.allowance_id,
            &renewed,
            &f.context,
            32,
        )
        .is_err());
    assert_eq!(rows(&f.store), before);
    let next = f
        .store
        .authorize_adaptive_leadership_review_call(
            &f.leader,
            Uuid::from_u128(101),
            "leadership-new-evidence",
            &grant,
            &context,
            33,
        )
        .unwrap();
    assert!(next.dispatch.is_none());
    assert_eq!(session(&f), f.context.source_session);
}

#[test]
fn missing_or_changed_current_sources_reject_authorization_claim_and_completion_without_writes() {
    for source in [
        "missing-project",
        "changed-project",
        "missing-session",
        "missing-head",
        "broken-journal",
        "changed-session",
    ] {
        for stage in ["authorize", "claim", "complete"] {
            let f = fixture();
            let call = if stage == "authorize" {
                None
            } else {
                Some(authorize(&f))
            };
            if stage == "complete" {
                f.store
                    .claim_adaptive_leadership_review_call(
                        &f.leader,
                        &claim(call.as_ref().unwrap()),
                        21,
                    )
                    .unwrap();
            }
            match source {
                "missing-project" => {
                    f.store.connection.lock().unwrap().execute(
                        "DELETE FROM company_entities WHERE tenant_id=?1 AND entity_kind='project' AND entity_id=?2",
                        rusqlite::params![f.leader.tenant_id.0, f.grant.project_id.0],
                    ).unwrap();
                }
                "changed-project" => change_project(&f, 22),
                "missing-session" => {
                    let connection = f.store.connection.lock().unwrap();
                    connection
                        .execute(
                            "DELETE FROM workflow_adaptive_heads WHERE session_id=?1",
                            [f.grant.session_id.to_string()],
                        )
                        .unwrap();
                    connection
                        .execute(
                            "DELETE FROM workflow_operations WHERE operation_namespace=?1",
                            [format!("adaptive-session-v1:{}", f.grant.session_id)],
                        )
                        .unwrap();
                }
                "missing-head" => {
                    f.store
                        .connection
                        .lock()
                        .unwrap()
                        .execute(
                            "DELETE FROM workflow_adaptive_heads WHERE session_id=?1",
                            [f.grant.session_id.to_string()],
                        )
                        .unwrap();
                }
                "broken-journal" => {
                    // Keep the apparently valid blocked tail; full journal validation
                    // must still reject a missing initial entry before dispatch.
                    f.store.connection.lock().unwrap().execute(
                        "DELETE FROM workflow_operations WHERE operation_namespace=?1 AND operation_id=?2",
                        rusqlite::params![format!("adaptive-session-v1:{}", f.grant.session_id), format!("{:020}", 1)],
                    ).unwrap();
                }
                _ => {
                    resolve(&f, Uuid::new_v4(), 22);
                }
            }
            let before = rows(&f.store);
            let rejected = match stage {
                "authorize" => f
                    .store
                    .authorize_adaptive_leadership_review_call(
                        &f.leader,
                        Uuid::from_u128(100),
                        "leadership-a",
                        &f.grant,
                        &f.context,
                        23,
                    )
                    .is_err(),
                "claim" => f
                    .store
                    .claim_adaptive_leadership_review_call(
                        &f.leader,
                        &claim(call.as_ref().unwrap()),
                        23,
                    )
                    .is_err(),
                _ => f
                    .store
                    .complete_adaptive_leadership_review_call(
                        &f.leader,
                        &completion(call.as_ref().unwrap(), false),
                        23,
                    )
                    .is_err(),
            };
            assert!(rejected, "{source} must reject {stage}");
            assert_eq!(
                rows(&f.store),
                before,
                "{source} rejection wrote rows at {stage}"
            );
        }
    }
}

#[test]
fn keep_blocked_persists_receipt_without_mutating_session_and_replays_before_freshness() {
    let f = fixture();
    let call = authorize(&f);
    let complete = completion(&call, false);
    let before = rows(&f.store);
    assert!(f
        .store
        .complete_adaptive_leadership_review_call(&f.leader, &complete, 21)
        .is_err());
    assert_eq!(rows(&f.store), before);
    f.store
        .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), 21)
        .unwrap();
    let blocked = session(&f);
    let project = f
        .store
        .company_project(&f.leader.tenant_id, &f.grant.project_id)
        .unwrap();
    let cursor = f.store.company_event_cursor().unwrap();
    let completed = f
        .store
        .complete_adaptive_leadership_review_call(&f.leader, &complete, 200_001)
        .unwrap();
    assert_eq!(completed.decision, Some(complete.decision.clone()));
    assert_eq!(
        completed.model_response_digest,
        Some(complete.model_response_digest.clone())
    );
    assert_eq!(completed.resolution_event_id, None);
    assert_eq!(session(&f), blocked);
    assert_eq!(
        f.store
            .company_project(&f.leader.tenant_id, &f.grant.project_id)
            .unwrap(),
        project
    );
    assert_eq!(f.store.company_event_cursor().unwrap(), cursor + 1);
    change_project(&f, 200_002);
    resolve(&f, Uuid::new_v4(), 200_003);
    let reopened = WorkflowStore::open(&f.path).unwrap();
    let before = rows(&reopened);
    assert_eq!(
        reopened
            .complete_adaptive_leadership_review_call(&f.leader, &complete, 700_000)
            .unwrap(),
        completed
    );
    assert_eq!(rows(&reopened), before);
    let mut changed = complete.clone();
    changed.model_response_digest = "d".repeat(64);
    assert_eq!(
        reopened
            .complete_adaptive_leadership_review_call(&f.leader, &changed, 700_001)
            .unwrap_err()
            .code,
        WorkflowErrorCode::IdempotencyConflict
    );
    assert_eq!(rows(&reopened), before);
    assert!(reopened
        .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), 700_001)
        .is_err());
}

#[test]
fn resolve_completion_requires_actual_blocked_resolved_with_exact_event() {
    let f = fixture();
    let call = authorize(&f);
    f.store
        .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), 21)
        .unwrap();
    let mut complete = completion(&call, true);
    let event = Uuid::new_v4();
    for resolution in [None, Some(event)] {
        complete.resolution_event_id = resolution;
        let before = rows(&f.store);
        assert!(f
            .store
            .complete_adaptive_leadership_review_call(&f.leader, &complete, 22)
            .is_err());
        assert_eq!(rows(&f.store), before);
        assert_eq!(session(&f), f.context.source_session);
    }
    let resolved = resolve(&f, event, 23);
    assert_eq!(
        resolved.cursor,
        AdaptiveCursorV1::BlockedResolved {
            reason_code: REASON.into(),
            resolution_event_id: event.to_string(),
        }
    );
    for resolution in [None, Some(Uuid::new_v4())] {
        complete.resolution_event_id = resolution;
        let before = rows(&f.store);
        assert!(f
            .store
            .complete_adaptive_leadership_review_call(&f.leader, &complete, 24)
            .is_err());
        assert_eq!(rows(&f.store), before);
    }
    let mut keep = completion(&call, false);
    keep.resolution_event_id = Some(event);
    assert!(f
        .store
        .complete_adaptive_leadership_review_call(&f.leader, &keep, 24)
        .is_err());
    complete.resolution_event_id = Some(event);
    let reopened = WorkflowStore::open(&f.path).unwrap();
    let completed = reopened
        .complete_adaptive_leadership_review_call(&f.leader, &complete, 24)
        .unwrap();
    assert_eq!(completed.resolution_event_id, Some(event));
    assert_eq!(completed.decision, Some(complete.decision.clone()));
    assert_eq!(session(&f), resolved);
    change_project(&f, 25);
    let before = rows(&reopened);
    assert_eq!(
        reopened
            .complete_adaptive_leadership_review_call(&f.leader, &complete, 700_000)
            .unwrap(),
        completed
    );
    assert_eq!(rows(&reopened), before);
}

#[test]
fn completion_rejects_unbound_response_refs_and_keep_resolution_without_receipt() {
    let f = fixture();
    let call = authorize(&f);
    f.store
        .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), 21)
        .unwrap();
    let good = completion(&call, false);
    let mut bad = vec![];
    let mut candidate = good.clone();
    candidate.request_digest = "d".repeat(64);
    bad.push(candidate);
    let mut candidate = good.clone();
    candidate.allowance_id = "foreign-allowance".into();
    bad.push(candidate);
    let mut candidate = good.clone();
    candidate.model_response_digest = "not-a-digest".into();
    bad.push(candidate);
    let mut candidate = good.clone();
    candidate.resolution_event_id = Some(Uuid::new_v4());
    bad.push(candidate);
    let mut candidate = good.clone();
    candidate.decision.decision = AdaptiveLeadershipReviewDecisionKindV1::KeepBlocked {
        rationale: "Invented model reference must not confer evidence".into(),
        evidence_refs: vec!["model-authored:not-supplied".into()],
    };
    bad.push(candidate);
    for candidate in bad {
        let before = rows(&f.store);
        assert!(f
            .store
            .complete_adaptive_leadership_review_call(&f.leader, &candidate, 22)
            .is_err());
        assert_eq!(rows(&f.store), before);
    }
    assert_eq!(session(&f), f.context.source_session);
    assert!(f
        .store
        .adaptive_leadership_review_call(&f.leader.tenant_id, f.grant.review_id)
        .unwrap()
        .unwrap()
        .decision
        .is_none());
    f.store
        .complete_adaptive_leadership_review_call(&f.leader, &good, 23)
        .unwrap();
}

#[test]
fn same_blocked_head_allows_only_three_distinct_evidence_sets_not_reordered_retries() {
    let f = fixture();
    let original = session(&f);
    for index in 0..3 {
        let mut context = f.context.clone();
        // Each catalog version is new supplied evidence; the adaptive head stays fixed.
        context.tool_catalog =
            serde_json::json!({"tools": [{"name": "file.inspect", "catalog_version": index} ]});
        let mut grant = f.grant.clone();
        grant.evidence_fingerprint =
            adaptive_leadership_evidence_fingerprint(&context.tool_catalog, &context.evidence_refs)
                .unwrap();
        grant.review_id = adaptive_leadership_review_id(
            grant.session_id,
            grant.expected_session_version,
            &grant.evidence_fingerprint,
        )
        .unwrap();
        let operation = Uuid::from_u128(200 + index);
        let allowance = format!("leadership-evidence-{index}");
        let call = f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                operation,
                &allowance,
                &grant,
                &context,
                30 + index as u64 * 3,
            )
            .unwrap();
        f.store
            .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), 31 + index as u64 * 3)
            .unwrap();
        let completed = f
            .store
            .complete_adaptive_leadership_review_call(
                &f.leader,
                &completion(&call, false),
                32 + index as u64 * 3,
            )
            .unwrap();
        let before = rows(&f.store);
        assert_eq!(
            f.store
                .authorize_adaptive_leadership_review_call(
                    &f.leader, operation, &allowance, &grant, &context, 200_001,
                )
                .unwrap(),
            completed
        );
        assert!(f
            .store
            .claim_adaptive_leadership_review_call(&f.leader, &claim(&call), 40)
            .is_err());
        assert!(f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                Uuid::new_v4(),
                "fresh-same-evidence",
                &grant,
                &context,
                40
            )
            .is_err());
        context.evidence_refs.reverse();
        assert_eq!(
            adaptive_leadership_evidence_fingerprint(&context.tool_catalog, &context.evidence_refs)
                .unwrap(),
            grant.evidence_fingerprint
        );
        assert!(f
            .store
            .authorize_adaptive_leadership_review_call(
                &f.leader,
                Uuid::new_v4(),
                "fresh-reordered-evidence",
                &grant,
                &context,
                40
            )
            .is_err());
        assert_eq!(rows(&f.store), before);
        assert_eq!(session(&f), original);
    }
    let reopened = WorkflowStore::open(&f.path).unwrap();
    let mut context = f.context.clone();
    context.tool_catalog =
        serde_json::json!({"tools": [{"name": "file.inspect", "catalog_version": 3}]});
    let mut grant = f.grant.clone();
    grant.evidence_fingerprint =
        adaptive_leadership_evidence_fingerprint(&context.tool_catalog, &context.evidence_refs)
            .unwrap();
    grant.review_id = adaptive_leadership_review_id(
        grant.session_id,
        grant.expected_session_version,
        &grant.evidence_fingerprint,
    )
    .unwrap();
    let before = rows(&reopened);
    assert!(reopened
        .authorize_adaptive_leadership_review_call(
            &f.leader,
            Uuid::new_v4(),
            "leadership-fourth",
            &grant,
            &context,
            41
        )
        .is_err());
    assert_eq!(rows(&reopened), before);
    let calls = reopened
        .adaptive_leadership_review_calls(&f.leader.tenant_id, f.grant.session_id)
        .unwrap();
    assert_eq!(calls.len(), 3);
    assert!(calls
        .iter()
        .all(|call| call.dispatch.is_some() && call.decision.is_some()));
    assert_eq!(
        calls
            .iter()
            .map(|call| &call.grant.evidence_fingerprint)
            .collect::<BTreeSet<_>>()
            .len(),
        3
    );
}
