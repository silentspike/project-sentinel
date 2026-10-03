use super::*;
use crate::{
    adaptive_leadership_evidence_fingerprint, adaptive_leadership_review_id,
    AdaptiveBudgetWindowAuthorityV1, AdaptiveEffectV1, AdaptiveLeadershipReviewContextV1,
    AdaptiveLeadershipReviewSubjectV2, AdaptiveSessionGrantV1, AdaptiveSessionV1,
    AdaptiveTransitionV1, CompanyWorkItemSpecV1, PrincipalAuthorityV1, QualityGateBindingV1,
    WorkOutputContractV1,
};
use sentinel_common::AgentId;
use std::collections::BTreeSet;
use std::sync::{Arc, Barrier};

const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const NOW: u64 = 300_010;
const EXPIRY: u64 = NOW + 3_600_000;

mod work_funding;

struct Fixture {
    _temp: tempfile::TempDir,
    path: std::path::PathBuf,
    store: WorkflowStore,
    operator: AuthenticatedCompanyPrincipalV1,
    leader: AuthenticatedCompanyPrincipalV1,
    project: ProjectV1,
    session: AdaptiveSessionV1,
}

fn fixture(unknown: bool) -> Fixture {
    let (temp, path, store, _, mut project) =
        crate::domain_store::tests::accepted_project_fixture();
    let leader = AuthenticatedCompanyPrincipalV1 {
        schema_version: 1,
        tenant_id: project.tenant_id.clone(),
        principal_id: "pm-a".into(),
        kind: CompanyPrincipalKindV1::Agent,
        role: CompanyRoleV1::ProjectManager,
        customer_id: None,
        agent_id: Some(AgentId(1)),
        authority_generation: 1,
        authority_digest: PrincipalAuthorityV1::derive("pm-a", 1, &[1; 32])
            .unwrap()
            .authority_digest,
    };
    let mut operator = leader.clone();
    operator.kind = CompanyPrincipalKindV1::Operator;
    operator.agent_id = None;
    operator.principal_id = "operator-a".into();
    let planning = store
        .authorize_project_planning_call(
            &leader,
            Uuid::new_v4(),
            "resume-planning",
            &crate::ProjectPlanningGrantV1 {
                schema_version: 1,
                project_id: project.project_id.clone(),
                expected_version: project.version,
                planner_principal: leader.clone(),
                provider: "codex-cli".into(),
                model: "model-test".into(),
                catalog_digest: DIGEST.into(),
                max_duration_ms: crate::ADAPTIVE_LEADERSHIP_MAX_DURATION_MS,
                token_policy: SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,
                expires_at_unix_ms: 10,
            },
            2,
        )
        .unwrap();
    store
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
    let work_id = WorkItemId::parse("resume-build").unwrap();
    for step in 0..3 {
        let command = match step {
            0 => CompanyWorkflowCommandV1::PlanWorkGraph {
                project_id: project.project_id.clone(),
                expected_version: project.version,
                items: vec![CompanyWorkItemSpecV1 {
                    work_item_id: work_id.clone(),
                    title: "Build source".into(),
                    objective: "Implement accepted scope".into(),
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
                reason_ref: "resume-activation".into(),
            },
            _ => CompanyWorkflowCommandV1::AssignWork {
                project_id: project.project_id.clone(),
                expected_version: project.version,
                work_item_id: work_id.clone(),
                agent_id: AgentId(2),
                organization_generation: 1,
                organization_digest: DIGEST.into(),
                reason_ref: "resume-assignment".into(),
            },
        };
        let result = store
            .apply_company_command(&leader, Uuid::new_v4(), &command, 3 + step)
            .unwrap();
        let CompanyWorkflowResponseV1::Project(updated) = result.response else {
            panic!("project");
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
    let assignment = project.work_items[&work_id].assignments[0].clone();
    let authority = RuntimeAuthoritySnapshotV1 {
        schema_version: 1,
        tenant_id: project.tenant_id.clone(),
        project_id: project.project_id.clone(),
        work_item_id: work_id.clone(),
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
        policy_generation: project.governance.project_profile.generation,
        policy_digest: project.governance.project_profile.digest.clone(),
        active: true,
        capabilities: BTreeSet::from(["file.inspect".into(), "observation.retain_private".into()]),
    };
    let result = store
        .apply_company_command(
            &leader,
            Uuid::new_v4(),
            &CompanyWorkflowCommandV1::GrantSubscriptionCall {
                project_id: project.project_id.clone(),
                expected_version: project.version,
                grant: SubscriptionCallGrantV1 {
                    schema_version: 1,
                    work_item_id: work_id,
                    assignment_id: assignment.assignment_id,
                    assignment_version: assignment.assignment_version,
                    agent_id: assignment.agent_id,
                    provider: "codex-cli".into(),
                    model: "model-test".into(),
                    catalog_digest: DIGEST.into(),
                    max_calls: 4,
                    max_concurrent: 1,
                    max_duration_ms: crate::ADAPTIVE_LEADERSHIP_MAX_DURATION_MS,
                    token_policy: SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,
                    expires_at_unix_ms: NOW,
                },
            },
            10,
        )
        .unwrap();
    let CompanyWorkflowResponseV1::Project(updated) = result.response else {
        panic!("project");
    };
    project = *updated;
    let allowance = project.subscription_call.as_ref().unwrap();
    let grant = AdaptiveSessionGrantV1 {
        schema_version: 1,
        session_id: Uuid::new_v4(),
        authority: authority.clone(),
        provider_allowance_id: allowance.allowance_id.clone(),
        provider_authority_digest:
            crate::adaptive_leadership_continuation_provider_authority_digest(allowance, &authority)
                .unwrap(),
        provider: allowance.grant.provider.clone(),
        model: allowance.grant.model.clone(),
        catalog_digest: allowance.grant.catalog_digest.clone(),
        max_output_tokens: 4096,
        max_call_duration_ms: allowance.grant.max_duration_ms,
        max_model_calls: allowance.grant.max_calls,
        max_tool_calls: allowance.grant.max_calls,
        created_at_ms: 10,
        deadline_ms: NOW,
    };
    let (_, mut session) = store
        .begin_adaptive_session(&grant, &authority, 10)
        .unwrap();
    if unknown {
        let effect = AdaptiveEffectV1 {
            id: Uuid::new_v4(),
            request_digest: DIGEST.into(),
        };
        for (now, command) in [
            (
                11,
                AdaptiveTransitionV1::ClaimModel {
                    effect: effect.clone(),
                    previous_observation_digest: None,
                },
            ),
            (12, AdaptiveTransitionV1::MarkUnknown { effect }),
        ] {
            session = store
                .advance_adaptive_session(
                    grant.session_id,
                    session.version,
                    Uuid::new_v4(),
                    &command,
                    &authority,
                    now,
                )
                .unwrap()
                .1;
        }
    }
    Fixture {
        _temp: temp,
        path,
        store,
        operator,
        leader,
        project,
        session,
    }
}

fn draft(f: &Fixture) -> AdaptiveResumePolicyRequestV1 {
    f.store
        .adaptive_resume_policy_draft(
            &f.operator,
            &f.project.project_id,
            f.session.grant.session_id,
            Uuid::new_v4(),
            "issue-856.resume",
            EXPIRY,
            NOW,
        )
        .unwrap()
}

fn issue(f: &Fixture) -> AdaptiveResumePolicyReceiptV1 {
    f.store
        .authorize_adaptive_resume_policy(&f.operator, &draft(f), NOW)
        .unwrap()
        .1
}

fn review(
    f: &Fixture,
    binding: Option<AdaptiveResumePolicyBindingV1>,
) -> (
    AdaptiveLeadershipReviewGrantV1,
    AdaptiveLeadershipReviewContextV1,
) {
    let allowance = f.project.subscription_call.clone().unwrap();
    let budget = AdaptiveBudgetWindowAuthorityV1 {
        schema_version: 1,
        active_allowance_digest: crate::adaptive_budget_allowance_digest(&allowance).unwrap(),
        root_allowance: allowance.clone(),
        continuation_history_digest: crate::adaptive_budget_history_digest(&f.session.continuation)
            .unwrap(),
        observed_at_ms: NOW,
        model_calls_exhausted: false,
        deadline_expired: true,
        dispatch_slack_insufficient: false,
    };
    let context = AdaptiveLeadershipReviewContextV1 {
        source_project: f.project.clone(),
        source_session: f.session.clone(),
        tool_catalog: serde_json::json!({"tools": [{"name": "file.inspect"}]}),
        evidence_refs: vec![
            format!(
                "adaptive-budget-root:{}:{}",
                allowance.allowance_id, f.session.grant.provider_authority_digest
            ),
            format!("adaptive-budget-current:{}", budget.active_allowance_digest),
            format!(
                "adaptive-budget-history:{}",
                budget.continuation_history_digest
            ),
        ],
    };
    let fingerprint =
        adaptive_leadership_evidence_fingerprint(&context.tool_catalog, &context.evidence_refs)
            .unwrap();
    let grant = AdaptiveLeadershipReviewGrantV1 {
        schema_version: 3,
        review_id: adaptive_leadership_review_id(
            f.session.grant.session_id,
            f.session.version,
            &fingerprint,
        )
        .unwrap(),
        project_id: f.project.project_id.clone(),
        expected_project_version: f.project.version,
        work_item_id: f.session.grant.authority.work_item_id.clone(),
        session_id: f.session.grant.session_id,
        expected_session_version: f.session.version,
        expected_reason_code: String::new(),
        evidence_fingerprint: fingerprint,
        leadership_principal: f.leader.clone(),
        leadership_authority: PrincipalAuthorityV1::derive("pm-a", 1, &[1; 32]).unwrap(),
        assignment_id: allowance.grant.assignment_id,
        assignee_authority: f.session.grant.authority.clone(),
        provider: "codex-cli".into(),
        model: "model-test".into(),
        catalog_digest: DIGEST.into(),
        max_duration_ms: crate::ADAPTIVE_LEADERSHIP_MAX_DURATION_MS,
        token_policy: SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,
        expires_at_unix_ms: NOW + crate::ADAPTIVE_LEADERSHIP_MAX_DURATION_MS,
        subject: Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted {
            budget: Box::new(budget),
        }),
        recovery_epoch: None,
        resume_policy: binding.map(Box::new),
        work_funding: None,
    };
    (grant, context)
}

#[test]
fn issuance_preserves_every_counter_and_reopens_as_an_immutable_leaf() {
    let f = fixture(false);
    let request = draft(&f);
    assert_eq!(request.source.base_model_calls, 0);
    assert_eq!(request.source.base_tool_calls, 0);
    assert_eq!(request.source.base_review_count, 0);
    assert_eq!(request.source.base_window_count, 0);
    assert_eq!(request.limits.total_review_ceiling, 4);
    assert_eq!(request.limits.total_window_ceiling, 4);
    let (replayed, receipt) = f
        .store
        .authorize_adaptive_resume_policy(&f.operator, &request, NOW)
        .unwrap();
    assert!(!replayed);
    assert_eq!(
        f.store
            .adaptive_session(f.session.grant.session_id, &f.session.grant.authority)
            .unwrap()
            .unwrap(),
        f.session
    );
    assert_eq!(
        f.store
            .company_project(&f.project.tenant_id, &f.project.project_id)
            .unwrap()
            .unwrap(),
        f.project
    );
    let reopened = WorkflowStore::open(&f.path).unwrap();
    assert_eq!(
        reopened
            .adaptive_resume_policy(&f.project.tenant_id, f.session.grant.session_id)
            .unwrap(),
        Some(receipt)
    );
}

#[test]
fn fresh_issuance_rejects_stale_source_role_tenant_and_reduced_duration() {
    let f = fixture(false);
    let request = draft(&f);
    for index in 0..7 {
        let mut changed = request.clone();
        match index {
            0 => changed.source.expected_project_version += 1,
            1 => changed.source.expected_session_version += 1,
            2 => changed.source.project_payload_digest = "b".repeat(64),
            3 => changed.source.root_entry_digest = "b".repeat(64),
            4 => changed.source.head_entry_digest = "b".repeat(64),
            5 => changed.source.review_history_digest = "b".repeat(64),
            _ => changed.limits.max_call_duration_ms -= 1,
        }
        assert!(f
            .store
            .authorize_adaptive_resume_policy(&f.operator, &changed, NOW)
            .is_err());
    }
    let mut wrong = f.operator.clone();
    wrong.role = CompanyRoleV1::Developer;
    assert!(f
        .store
        .authorize_adaptive_resume_policy(&wrong, &request, NOW)
        .is_err());
    wrong = f.operator.clone();
    wrong.tenant_id = TenantId::parse("other-tenant").unwrap();
    assert!(f
        .store
        .authorize_adaptive_resume_policy(&wrong, &request, NOW)
        .is_err());
    assert!(f
        .store
        .authorize_adaptive_resume_policy(&f.leader, &request, NOW)
        .is_err());
    f.store
        .advance_adaptive_session(
            f.session.grant.session_id,
            f.session.version,
            Uuid::new_v4(),
            &AdaptiveTransitionV1::Cancel,
            &f.session.grant.authority,
            NOW,
        )
        .unwrap();
    assert!(f
        .store
        .authorize_adaptive_resume_policy(&f.operator, &request, NOW)
        .is_err());
    assert!(f
        .store
        .adaptive_resume_policy(&f.project.tenant_id, f.session.grant.session_id)
        .unwrap()
        .is_none());
}

#[test]
fn issuance_requires_productive_expiry_and_rejects_pending_prior_review() {
    let f = fixture(false);
    let request = draft(&f);
    for expiry in [NOW + 120_999, NOW + ADAPTIVE_RESUME_MAX_POLICY_MS + 1] {
        let mut short = request.clone();
        short.limits.expires_at_unix_ms = expiry;
        assert!(f
            .store
            .authorize_adaptive_resume_policy(&f.operator, &short, NOW)
            .is_err());
    }
    let (grant, context) = review(&f, None);
    let pending = f
        .store
        .authorize_adaptive_leadership_review_call(
            &f.leader,
            Uuid::new_v4(),
            "prior-pending-review",
            &grant,
            &context,
            NOW,
        )
        .unwrap();
    assert!(f
        .store
        .authorize_adaptive_resume_policy(&f.operator, &request, NOW)
        .is_err());
    assert!(f
        .store
        .adaptive_resume_policy_draft(
            &f.operator,
            &f.project.project_id,
            f.session.grant.session_id,
            Uuid::new_v4(),
            "issue-856.resume",
            EXPIRY,
            NOW,
        )
        .is_err());
    let retired_at = pending.grant.expires_at_unix_ms;
    f.store
        .expire_adaptive_leadership_review_call(
            &f.leader,
            pending.grant.review_id,
            pending.version,
            retired_at,
        )
        .unwrap();
    let after = f
        .store
        .adaptive_resume_policy_draft(
            &f.operator,
            &f.project.project_id,
            f.session.grant.session_id,
            Uuid::new_v4(),
            "issue-856.resume",
            EXPIRY,
            retired_at,
        )
        .unwrap();
    assert_eq!(after.source.base_review_count, 1);
    assert_eq!(after.source.base_window_count, 0);
    assert_eq!(after.limits.total_review_ceiling, 5);
    assert_eq!(after.limits.total_window_ceiling, 4);
}

#[test]
fn lower_ceilings_and_shorter_productive_window_are_allowed() {
    let f = fixture(false);
    let mut request = draft(&f);
    request.limits.total_review_ceiling = 1;
    request.limits.total_window_ceiling = 1;
    request.limits.max_window_ms =
        request.limits.max_call_duration_ms + request.limits.dispatch_margin_ms;
    request.limits.expires_at_unix_ms = NOW + request.limits.max_window_ms;
    f.store
        .authorize_adaptive_resume_policy(&f.operator, &request, NOW)
        .unwrap();
}

#[test]
fn replay_precedes_currentness_expiry_and_does_not_refill_policy() {
    let f = fixture(false);
    let receipt = issue(&f);
    f.store
        .advance_adaptive_session(
            f.session.grant.session_id,
            f.session.version,
            Uuid::new_v4(),
            &AdaptiveTransitionV1::Cancel,
            &f.session.grant.authority,
            NOW + 1,
        )
        .unwrap();
    assert_eq!(
        f.store
            .authorize_adaptive_resume_policy(&f.operator, &receipt.request, EXPIRY + 1)
            .unwrap(),
        (true, receipt.clone())
    );
    assert_eq!(
        f.store
            .adaptive_resume_policy_draft(
                &f.operator,
                &f.project.project_id,
                f.session.grant.session_id,
                receipt.request.operation_id,
                &receipt.request.reason_ref,
                EXPIRY,
                EXPIRY + 1,
            )
            .unwrap(),
        receipt.request
    );
    let mut successor = receipt.request.clone();
    successor.operation_id = Uuid::new_v4();
    assert!(f
        .store
        .authorize_adaptive_resume_policy(&f.operator, &successor, EXPIRY + 1)
        .is_err());
}

#[test]
fn unknown_requires_transaction_current_external_proof_and_replay_skips_callback() {
    let f = fixture(true);
    assert!(f
        .store
        .adaptive_resume_policy_draft(
            &f.operator,
            &f.project.project_id,
            f.session.grant.session_id,
            Uuid::new_v4(),
            "unknown-resume",
            EXPIRY,
            NOW,
        )
        .is_err());
    let request = f
        .store
        .adaptive_resume_policy_draft_with_unknown_proof(
            &f.operator,
            &f.project.project_id,
            f.session.grant.session_id,
            Uuid::new_v4(),
            "unknown-resume",
            EXPIRY,
            NOW,
            DIGEST,
        )
        .unwrap();
    assert_eq!(request.source.base_model_calls, 1);
    assert_eq!(request.limits.total_review_ceiling, 3);
    assert!(f
        .store
        .authorize_adaptive_resume_policy(&f.operator, &request, NOW)
        .is_err());
    assert!(f
        .store
        .authorize_adaptive_resume_policy_with_unknown_proof(&f.operator, &request, NOW, |_, _| Ok(
            "b".repeat(64)
        ),)
        .is_err());
    let expected = request.source.clone();
    // Test verifier models the service's independent sealed EventStore proof, not a caller digest.
    let (_, receipt) = f
        .store
        .authorize_adaptive_resume_policy_with_unknown_proof(
            &f.operator,
            &request,
            NOW,
            |_, source| {
                assert_eq!(*source, expected);
                let AdaptiveResumeSubjectV1::ModelUnknown { effect, .. } = &source.subject else {
                    panic!("unknown");
                };
                assert_eq!(
                    f.session.cursor,
                    AdaptiveCursorV1::ModelUnknown {
                        effect: effect.clone()
                    }
                );
                Ok(DIGEST.into())
            },
        )
        .unwrap();
    assert_eq!(
        f.store
            .adaptive_session(f.session.grant.session_id, &f.session.grant.authority)
            .unwrap()
            .unwrap(),
        f.session
    );
    assert_eq!(
        f.store
            .authorize_adaptive_resume_policy_with_unknown_proof(
                &f.operator,
                &request,
                EXPIRY + 1,
                |_, _| panic!("replay must not verify fresh source"),
            )
            .unwrap(),
        (true, receipt)
    );
}

#[test]
fn malformed_policy_row_and_sealed_event_fail_on_fresh_read_and_reopen() {
    for mutation in 0..5 {
        let f = fixture(false);
        let receipt = issue(&f);
        {
            let connection = f.store.connection.lock().unwrap();
            match mutation {
                0 => connection
                    .execute(
                        "UPDATE company_entities SET version=2 WHERE entity_kind=?1",
                        [POLICY_KIND],
                    )
                    .unwrap(),
                1 => connection
                    .execute(
                        "UPDATE company_entities SET entity_id='wrong-key' WHERE entity_kind=?1",
                        [POLICY_KIND],
                    )
                    .unwrap(),
                2 => connection
                    .execute(
                        "UPDATE company_events SET project_id='wrong-project' WHERE event_type=?1",
                        [POLICY_EVENT],
                    )
                    .unwrap(),
                3 => connection
                    .execute(
                        "UPDATE company_events SET principal_id='wrong-issuer' WHERE event_type=?1",
                        [POLICY_EVENT],
                    )
                    .unwrap(),
                _ => connection
                    .execute(
                        "UPDATE company_events SET payload_digest=?1 WHERE event_type=?2",
                        params!["b".repeat(64), POLICY_EVENT],
                    )
                    .unwrap(),
            };
            if mutation == 1 {
                assert!(get_entity::<AdaptiveResumePolicyReceiptV1>(
                    &connection,
                    &f.project.tenant_id,
                    POLICY_KIND,
                    "wrong-key",
                )
                .is_err());
            }
        }
        assert!(f
            .store
            .adaptive_resume_policy(&f.project.tenant_id, f.session.grant.session_id)
            .is_err());
        let reopened = WorkflowStore::open(&f.path).unwrap();
        assert!(reopened
            .adaptive_resume_policy(&f.project.tenant_id, f.session.grant.session_id)
            .is_err());
        assert!(reopened
            .authorize_adaptive_resume_policy(&f.operator, &receipt.request, NOW)
            .is_err());
    }
}

#[test]
fn leaf_checks_do_not_load_project_journal_or_review_history() {
    let f = fixture(false);
    let receipt = issue(&f);
    let (grant, _) = review(&f, Some(receipt.binding(1).unwrap()));
    let operation_id = Uuid::new_v4();
    let authorization = AdaptiveContinuationAuthorizationV1 {
        schema_version: 1,
        operation_id,
        review_id: grant.review_id,
        resolution_event_id: Uuid::new_v4(),
        session_id: grant.session_id,
        source_session_version: grant.expected_session_version,
        source: crate::AdaptiveContinuationSourceV1::BudgetWindowExhausted {
            active_allowance_digest: crate::adaptive_budget_allowance_digest(
                f.project.subscription_call.as_ref().unwrap(),
            )
            .unwrap(),
            continuation_history_digest: crate::adaptive_budget_history_digest(
                &f.session.continuation,
            )
            .unwrap(),
        },
        abandoned_model_effect: None,
        provider_allowance_id: "next-allowance".into(),
        provider_authority_digest: DIGEST.into(),
        issued_at_ms: NOW,
        deadline_ms: NOW + 121_000,
        additional_model_calls: 1,
        local_adoption: None,
        resume_policy: grant.resume_policy.clone(),
        work_funding: None,
    };
    {
        let mut connection = f.store.connection.lock().unwrap();
        let transaction = connection.transaction().unwrap();
        insert_resume_review_membership(&transaction, &grant, operation_id, DIGEST, NOW).unwrap();
        transaction.commit().unwrap();
        connection
            .execute(
                "UPDATE company_entities SET payload_digest='corrupt' WHERE entity_kind='project'",
                [],
            )
            .unwrap();
        connection
            .execute("DELETE FROM workflow_operations", [])
            .unwrap();
        require_resume_review_membership(&connection, &grant, DIGEST, operation_id).unwrap();
        assert!(
            require_resume_review_membership(&connection, &grant, DIGEST, Uuid::new_v4()).is_err()
        );
        require_resume_authorization_membership(
            &connection,
            &authorization,
            &f.session.grant.authority,
        )
        .unwrap();
        assert!(require_resume_review_membership(
            &connection,
            &grant,
            &"b".repeat(64),
            operation_id
        )
        .is_err());
    }
    let reopened = WorkflowStore::open(&f.path).unwrap();
    assert_eq!(
        reopened
            .adaptive_resume_policy(&f.project.tenant_id, f.session.grant.session_id)
            .unwrap(),
        Some(receipt.clone())
    );
    assert_eq!(
        reopened
            .authorize_adaptive_resume_policy(&f.operator, &receipt.request, EXPIRY + 1)
            .unwrap(),
        (true, receipt)
    );
}

#[test]
fn membership_is_transactional_immutable_sequential_and_sealed() {
    let f = fixture(false);
    let receipt = issue(&f);
    let (grant, _) = review(&f, Some(receipt.binding(1).unwrap()));
    let operation = Uuid::new_v4();
    let mut connection = f.store.connection.lock().unwrap();
    {
        let transaction = connection.transaction().unwrap();
        insert_resume_review_membership(&transaction, &grant, operation, DIGEST, NOW).unwrap();
        // Dropping the outer review transaction rolls back both leaf and event.
    }
    assert!(require_resume_review_membership(&connection, &grant, DIGEST, operation).is_err());
    let transaction = connection.transaction().unwrap();
    let membership =
        insert_resume_review_membership(&transaction, &grant, operation, DIGEST, NOW).unwrap();
    assert_eq!(
        insert_resume_review_membership(&transaction, &grant, operation, DIGEST, EXPIRY + 1)
            .unwrap(),
        membership
    );
    assert!(
        insert_resume_review_membership(&transaction, &grant, Uuid::new_v4(), DIGEST, NOW).is_err()
    );
    let mut gap = grant.clone();
    gap.resume_policy = Some(Box::new(receipt.binding(3).unwrap()));
    gap.review_id = Uuid::new_v4();
    assert!(
        insert_resume_review_membership(&transaction, &gap, Uuid::new_v4(), DIGEST, NOW).is_err()
    );
    gap.resume_policy = Some(Box::new(receipt.binding(2).unwrap()));
    assert!(insert_resume_review_membership(&transaction, &gap, operation, DIGEST, NOW).is_err());
    transaction.commit().unwrap();
    require_resume_review_membership(&connection, &grant, DIGEST, operation).unwrap();
    assert!(require_resume_review_membership(&connection, &grant, DIGEST, Uuid::new_v4()).is_err());
    connection
        .execute(
            "UPDATE company_events SET project_id='wrong-project' WHERE event_type=?1",
            [MEMBERSHIP_EVENT],
        )
        .unwrap();
    assert!(require_resume_review_membership(&connection, &grant, DIGEST, operation).is_err());
}

#[test]
fn policy_event_failure_rolls_back_entity_and_preserves_source() {
    let f = fixture(false);
    let request = draft(&f);
    f.store
        .connection
        .lock()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER reject_resume_policy_event BEFORE INSERT ON company_events
         WHEN NEW.event_type='adaptive_resume_policy_authorized'
         BEGIN SELECT RAISE(ABORT, 'test rollback'); END;",
        )
        .unwrap();
    assert!(f
        .store
        .authorize_adaptive_resume_policy(&f.operator, &request, NOW)
        .is_err());
    assert!(f
        .store
        .adaptive_resume_policy(&f.project.tenant_id, f.session.grant.session_id)
        .unwrap()
        .is_none());
    assert_eq!(
        f.store
            .adaptive_session(f.session.grant.session_id, &f.session.grant.authority)
            .unwrap()
            .unwrap(),
        f.session
    );
}

#[test]
fn two_connections_issue_one_policy_and_replay_exactly() {
    let f = fixture(false);
    let request = draft(&f);
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let store = WorkflowStore::open(&f.path).unwrap();
            let request = request.clone();
            let operator = f.operator.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store
                    .authorize_adaptive_resume_policy(&operator, &request, NOW)
                    .unwrap()
            })
        })
        .collect();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_ne!(results[0].0, results[1].0);
    assert_eq!(results[0].1, results[1].1);
    let count: i64 = f
        .store
        .connection
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM company_events WHERE event_type=?1",
            [POLICY_EVENT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}
