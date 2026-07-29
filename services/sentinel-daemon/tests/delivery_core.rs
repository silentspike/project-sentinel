use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use sentinel_daemon::delivery::*;
use serde_json::json;
use tempfile::TempDir;

fn digest(value: &str) -> ContentDigest {
    ContentDigest::of(&value).unwrap()
}

fn reference(id: &str, generation: u64) -> VersionedRefV1 {
    VersionedRefV1 {
        id: id.to_string(),
        generation,
        digest: digest(id),
    }
}

fn principal(id: &str, role: AuthorityRole) -> PrincipalV1 {
    PrincipalV1 {
        tenant_id: "tenant-a".to_string(),
        principal_id: id.to_string(),
        authority_generation: 7,
        roles: BTreeSet::from([role]),
    }
}

fn context(id: &str, role: AuthorityRole, key: &str, now_ms: u64) -> CommandContextV1 {
    CommandContextV1 {
        principal: principal(id, role),
        idempotency_key: key.to_string(),
        now_ms,
    }
}

fn cost() -> CostRefV1 {
    CostRefV1 {
        ledger_id: "cost-1".to_string(),
        generation: 1,
        digest: digest("cost"),
        currency: "USD".to_string(),
        amount_minor: 120,
    }
}

fn candidate(generation: u64, implementers: &[&str]) -> ReleaseCandidateV1 {
    ReleaseCandidateV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        candidate_id: format!("candidate-{generation}"),
        generation,
        tenant_id: "tenant-a".to_string(),
        agreement: reference("agreement-1", 3),
        project: reference("project-1", 4),
        work_items_digest: digest("work-items"),
        source_digest: digest(&format!("source-{generation}")),
        artifacts: vec![ArtifactRefV1 {
            artifact_id: "site".to_string(),
            generation,
            digest: digest(&format!("artifact-{generation}")),
            media_type: "application/octet-stream".to_string(),
            owner_principal_id: implementers[0].to_string(),
        }],
        toolchain_digest: digest("toolchain"),
        runtime_profile_digest: digest("runtime"),
        acceptance_criteria_digest: digest("acs"),
        implementer_principal_ids: implementers
            .iter()
            .map(|value| (*value).to_string())
            .collect(),
        cost: cost(),
        state: CandidateState::Draft,
        candidate_digest: ContentDigest::zero(),
        created_at_ms: 100,
    }
    .seal()
    .unwrap()
}

fn data_control() -> DataControlV1 {
    DataControlV1 {
        classification: "internal".to_string(),
        encryption_key_owner: "security".to_string(),
        access_policy_digest: digest("access"),
        redaction_policy_digest: digest("redaction"),
        retention_frontier: reference("frontier", 2),
        audit_policy_digest: digest("audit"),
    }
}

fn plan(candidate: &ReleaseCandidateV1) -> QaEvaluationPlanV1 {
    QaEvaluationPlanV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        plan_id: "plan-1".to_string(),
        generation: 1,
        request: reference("request-1", 1),
        candidate: VersionedRefV1 {
            id: candidate.candidate_id.clone(),
            generation: candidate.generation,
            digest: candidate.candidate_digest.clone(),
        },
        agreement: candidate.agreement.clone(),
        project: candidate.project.clone(),
        work_items_digest: candidate.work_items_digest.clone(),
        acceptance_criteria_digest: candidate.acceptance_criteria_digest.clone(),
        required_case_ids: BTreeSet::from(["structure".to_string(), "security".to_string()]),
        optional_case_ids: BTreeSet::from(["visual".to_string()]),
        fixture_inventory_digest: digest("fixtures"),
        evaluator_policy_digest: digest("evaluator"),
        aggregation_policy_digest: digest("aggregation"),
        release_policy_digest: digest("release-policy"),
        runner_binary_digest: digest("runner"),
        toolchain_digest: candidate.toolchain_digest.clone(),
        sandbox_profile_digest: digest("sandbox"),
        capability_digest: digest("capability"),
        environment_digest: digest("environment"),
        credential_policy_digest: digest("credentials"),
        declared_seeds: BTreeSet::from([1, 2]),
        retry_limit: 1,
        retryable_classes: BTreeSet::from(["infrastructure".to_string()]),
        data_control: data_control(),
        plan_digest: ContentDigest::zero(),
    }
    .seal()
    .unwrap()
}

fn run(plan: &QaEvaluationPlanV1, qa: PrincipalV1) -> QaEvaluationRunReceiptV1 {
    QaEvaluationRunReceiptV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        run_id: "run-1".to_string(),
        generation: 1,
        plan: VersionedRefV1 {
            id: plan.plan_id.clone(),
            generation: plan.generation,
            digest: plan.plan_digest.clone(),
        },
        request_digest: digest("run-request"),
        state: QaRunState::Planned,
        retry_of: None,
        supersedes: None,
        actors: vec![qa],
        durable_event_generation: 1,
        started_at_ms: None,
        finished_at_ms: None,
        attempts: 0,
        harness_outcome: None,
        cleanup_receipt: None,
        aggregate_outcomes: BTreeMap::new(),
        gate_receipt: None,
    }
}

struct FakeIntegration {
    qa_calls: Arc<AtomicUsize>,
}

impl FakeIntegration {
    fn new() -> (Self, Arc<AtomicUsize>) {
        let qa_calls = Arc::new(AtomicUsize::new(0));
        (
            Self {
                qa_calls: Arc::clone(&qa_calls),
            },
            qa_calls,
        )
    }
}

impl DeliveryIntegrationPort for FakeIntegration {
    fn readiness(&self) -> AdapterReadiness {
        AdapterReadiness::Ready {
            contract_version: 1,
            authority_generation: 7,
            contract_digest: digest("integration"),
        }
    }

    fn candidate_authority(
        &self,
        query: &CandidateAuthorityQueryV1,
    ) -> Result<CandidateAuthoritySnapshotV1, DeliveryError> {
        CandidateAuthoritySnapshotV1 {
            schema_version: 1,
            authority_generation: 7,
            agreement: query.agreement.clone(),
            project: query.project.clone(),
            work_items_digest: query.work_items_digest.clone(),
            current_candidate_generation: query.project.generation + 1,
            current_candidate_digest: query.candidate_digest.clone(),
            participant_principals: vec![],
            snapshot_digest: ContentDigest::zero(),
        }
        .seal()
    }

    fn execute_qa(
        &self,
        request: &WorkbenchEvidenceRequestV1,
    ) -> Result<WorkbenchEvidenceReceiptV1, DeliveryError> {
        self.qa_calls.fetch_add(1, Ordering::SeqCst);
        WorkbenchEvidenceReceiptV1 {
            schema_version: 1,
            invocation: reference("invocation-1", 1),
            assignment: reference("assignment-1", 1),
            input_digest: request.request_digest.clone(),
            output_digest: digest("qa-output"),
            artifact_ownership_digest: digest("ownership"),
            result_inventory_digest: digest("results"),
            logs_digest: digest("logs"),
            screenshots_digest: Some(digest("screenshots")),
            failure_classification_digest: digest("failure-classes"),
            passed: true,
            required_cases_complete: true,
            contaminated: false,
            needs_human_review: false,
            flaky_unresolved: false,
            cleanup_receipt: reference("cleanup-1", 1),
            receipt_digest: ContentDigest::zero(),
        }
        .seal()
    }
}

fn store(temp: &TempDir) -> DeliveryStore {
    DeliveryStore::open(&temp.path().join("delivery.redb")).unwrap()
}

fn completed_qa_core(
    temp: &TempDir,
) -> (
    DeliveryCore<FakeIntegration>,
    ReleaseCandidateV1,
    QaEvaluationPlanV1,
    PrincipalV1,
) {
    let core = DeliveryCore::new(store(temp), FakeIntegration::new().0);
    let candidate = candidate(5, &["developer"]);
    core.register_candidate(
        &context("developer", AuthorityRole::Developer, "register-1", 100),
        candidate.clone(),
    )
    .unwrap();
    let plan = plan(&candidate);
    let qa = principal("qa-1", AuthorityRole::Qa);
    core.assign_qa(
        &context(
            "release-manager",
            AuthorityRole::ReleaseManager,
            "assign-1",
            110,
        ),
        "tenant-a",
        "project-1",
        &candidate.candidate_id,
        plan.clone(),
        run(&plan, qa.clone()),
    )
    .unwrap();
    for (key, next, now) in [
        ("admit-1", QaRunState::Admitted, 120),
        ("running-1", QaRunState::Running, 130),
    ] {
        core.transition_qa(
            &context("qa-1", AuthorityRole::Qa, key, now),
            "tenant-a",
            "project-1",
            "run-1",
            next,
        )
        .unwrap();
    }
    core.execute_qa(
        &context("qa-1", AuthorityRole::Qa, "execute-1", 140),
        "tenant-a",
        "project-1",
        "run-1",
    )
    .unwrap();
    core.transition_qa(
        &context("qa-1", AuthorityRole::Qa, "pass-1", 150),
        "tenant-a",
        "project-1",
        "run-1",
        QaRunState::CompletedPass,
    )
    .unwrap();
    (core, candidate, plan, qa)
}

fn core_with_seeded_aggregate(
    temp: &TempDir,
    mut aggregate: DeliveryAggregateV1,
) -> DeliveryCore<FakeIntegration> {
    aggregate.revision = 1;
    let store = store(temp);
    store
        .commit(&DeliveryCommitRequestV1 {
            tenant_id: aggregate.tenant_id.clone(),
            project_id: aggregate.project_id.clone(),
            expected_revision: 0,
            principal_id: "seed-authority".to_string(),
            command_kind: "seed_fixture".to_string(),
            idempotency_key: "seed-1".to_string(),
            command_digest: digest("seed-command"),
            aggregate,
            event_type: "delivery_fixture_seeded_v1".to_string(),
            event_payload: json!({"fixture": "delivery-core"}),
            committed_at_ms: 1,
        })
        .unwrap();
    DeliveryCore::new(store, FakeIntegration::new().0)
}

#[test]
fn unavailable_integration_fails_closed_without_preventing_store_startup() {
    let temp = TempDir::new().unwrap();
    let core = DeliveryCore::new(store(&temp), UnavailableDeliveryIntegration);
    assert!(matches!(
        core.readiness(),
        AdapterReadiness::Unavailable { .. }
    ));
    let result = core.register_candidate(
        &context("developer", AuthorityRole::Developer, "register-1", 100),
        candidate(5, &["developer"]),
    );
    assert!(matches!(
        result,
        Err(DeliveryError::AdapterUnavailable { .. })
    ));
    assert!(core.load("tenant-a", "project-1").unwrap().is_none());
    core.store().health().unwrap();
}

#[test]
fn candidate_registration_is_restart_safe_and_principal_namespaced() {
    let temp = TempDir::new().unwrap();
    let candidate = candidate(5, &["developer"]);
    {
        let core = DeliveryCore::new(store(&temp), FakeIntegration::new().0);
        let command = context("developer", AuthorityRole::Developer, "register-1", 100);
        let first = core
            .register_candidate(&command, candidate.clone())
            .unwrap();
        let duplicate = core
            .register_candidate(&command, candidate.clone())
            .unwrap();
        assert!(!first.duplicate);
        assert!(duplicate.duplicate);
        assert_eq!(first.operation_id, duplicate.operation_id);
        assert_eq!(
            core.load("tenant-a", "project-1")
                .unwrap()
                .unwrap()
                .revision,
            1
        );

        let mut changed = candidate.clone();
        changed.source_digest = digest("substitution");
        changed = changed.seal().unwrap();
        assert!(matches!(
            core.register_candidate(&command, changed),
            Err(DeliveryError::IdempotencyConflict { .. })
        ));
    }
    let reopened = store(&temp);
    assert_eq!(
        reopened
            .load("tenant-a", "project-1")
            .unwrap()
            .unwrap()
            .candidates
            .len(),
        1
    );
    assert_eq!(reopened.journal("tenant-a", "project-1").unwrap().len(), 1);
}

#[test]
fn separation_of_duties_rejects_implementer_as_qa() {
    let temp = TempDir::new().unwrap();
    let core = DeliveryCore::new(store(&temp), FakeIntegration::new().0);
    let candidate = candidate(5, &["developer"]);
    core.register_candidate(
        &context("developer", AuthorityRole::Developer, "register-1", 100),
        candidate.clone(),
    )
    .unwrap();
    let plan = plan(&candidate);
    let mut developer_as_qa = principal("developer", AuthorityRole::Qa);
    developer_as_qa.roles.insert(AuthorityRole::Developer);
    let result = core.assign_qa(
        &context(
            "release-manager",
            AuthorityRole::ReleaseManager,
            "assign-1",
            110,
        ),
        "tenant-a",
        "project-1",
        &candidate.candidate_id,
        plan.clone(),
        run(&plan, developer_as_qa),
    );
    assert!(matches!(result, Err(DeliveryError::AuthorityDenied(_))));
}

#[test]
fn qa_effect_uses_stable_request_and_is_not_repeated_on_retry() {
    let temp = TempDir::new().unwrap();
    let (integration, qa_calls) = FakeIntegration::new();
    let core = DeliveryCore::new(store(&temp), integration);
    let candidate = candidate(5, &["developer"]);
    core.register_candidate(
        &context("developer", AuthorityRole::Developer, "register-1", 100),
        candidate.clone(),
    )
    .unwrap();
    let plan = plan(&candidate);
    let qa = principal("qa-1", AuthorityRole::Qa);
    core.assign_qa(
        &context(
            "release-manager",
            AuthorityRole::ReleaseManager,
            "assign-1",
            110,
        ),
        "tenant-a",
        "project-1",
        &candidate.candidate_id,
        plan.clone(),
        run(&plan, qa.clone()),
    )
    .unwrap();
    core.transition_qa(
        &context("qa-1", AuthorityRole::Qa, "admit-1", 120),
        "tenant-a",
        "project-1",
        "run-1",
        QaRunState::Admitted,
    )
    .unwrap();
    core.transition_qa(
        &context("qa-1", AuthorityRole::Qa, "run-1", 130),
        "tenant-a",
        "project-1",
        "run-1",
        QaRunState::Running,
    )
    .unwrap();
    let effect_context = context("qa-1", AuthorityRole::Qa, "execute-1", 140);
    let first = core
        .execute_qa(&effect_context, "tenant-a", "project-1", "run-1")
        .unwrap();
    let duplicate = core
        .execute_qa(&effect_context, "tenant-a", "project-1", "run-1")
        .unwrap();
    assert!(!first.0.duplicate);
    assert!(duplicate.0.duplicate);
    assert_eq!(first.1.receipt_digest, duplicate.1.receipt_digest);
    assert_eq!(qa_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn completed_pass_requires_clean_structured_workbench_evidence() {
    let temp = TempDir::new().unwrap();
    let core = DeliveryCore::new(store(&temp), FakeIntegration::new().0);
    let candidate = candidate(5, &["developer"]);
    core.register_candidate(
        &context("developer", AuthorityRole::Developer, "register-1", 100),
        candidate.clone(),
    )
    .unwrap();
    let plan = plan(&candidate);
    let qa = principal("qa-1", AuthorityRole::Qa);
    core.assign_qa(
        &context(
            "release-manager",
            AuthorityRole::ReleaseManager,
            "assign-1",
            110,
        ),
        "tenant-a",
        "project-1",
        &candidate.candidate_id,
        plan.clone(),
        run(&plan, qa),
    )
    .unwrap();
    core.transition_qa(
        &context("qa-1", AuthorityRole::Qa, "admit-1", 120),
        "tenant-a",
        "project-1",
        "run-1",
        QaRunState::Admitted,
    )
    .unwrap();
    core.transition_qa(
        &context("qa-1", AuthorityRole::Qa, "running-1", 130),
        "tenant-a",
        "project-1",
        "run-1",
        QaRunState::Running,
    )
    .unwrap();

    assert!(matches!(
        core.transition_qa(
            &context("qa-1", AuthorityRole::Qa, "pass-too-early", 135),
            "tenant-a",
            "project-1",
            "run-1",
            QaRunState::CompletedPass,
        ),
        Err(DeliveryError::MissingEvidence(_))
    ));

    core.execute_qa(
        &context("qa-1", AuthorityRole::Qa, "execute-1", 140),
        "tenant-a",
        "project-1",
        "run-1",
    )
    .unwrap();
    core.transition_qa(
        &context("qa-1", AuthorityRole::Qa, "pass-1", 150),
        "tenant-a",
        "project-1",
        "run-1",
        QaRunState::CompletedPass,
    )
    .unwrap();
    let loaded = core.load("tenant-a", "project-1").unwrap().unwrap();
    assert_eq!(loaded.qa_runs["run-1"].state, QaRunState::CompletedPass);
    assert_eq!(loaded.qa_runs["run-1"].attempts, 1);
}

#[test]
fn gate_rejects_missing_expired_and_differently_bound_evidence() {
    let temp = TempDir::new().unwrap();
    let (core, candidate, plan, qa) = completed_qa_core(&temp);
    let candidate_ref = VersionedRefV1 {
        id: candidate.candidate_id.clone(),
        generation: candidate.generation,
        digest: candidate.candidate_digest,
    };
    let plan_ref = VersionedRefV1 {
        id: plan.plan_id,
        generation: plan.generation,
        digest: plan.plan_digest,
    };
    let gate = QaReleaseGateReceiptV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        gate_id: "gate-negative".to_string(),
        generation: 1,
        candidate: candidate_ref,
        plan: plan_ref,
        case_inventory_digest: digest("case-inventory"),
        deterministic_evidence_digest: digest("deterministic-evidence"),
        model_evidence_digest: None,
        calibration_digest: digest("calibration"),
        source_evidence_digest: digest("source-evidence"),
        flake_disposition_digest: None,
        policy_digest: digest("gate-policy"),
        release_manifest_digest: digest("manifest-input"),
        actor: qa,
        passed: true,
        issued_at_ms: 160,
        expires_at_ms: 1_000,
    };

    assert!(matches!(
        core.record_gate(
            &context("qa-1", AuthorityRole::Qa, "missing-approval", 170),
            "tenant-a",
            "project-1",
            "run-1",
            gate.clone(),
        ),
        Err(DeliveryError::MissingEvidence(_))
    ));

    let mut expired = gate.clone();
    expired.expires_at_ms = 170;
    assert!(matches!(
        core.record_gate(
            &context("qa-1", AuthorityRole::Qa, "expired", 170),
            "tenant-a",
            "project-1",
            "run-1",
            expired,
        ),
        Err(DeliveryError::AuthorityDenied(_))
    ));

    let mut wrong_plan = gate;
    wrong_plan.plan.digest = digest("other-plan");
    assert!(matches!(
        core.record_gate(
            &context("qa-1", AuthorityRole::Qa, "wrong-plan", 170),
            "tenant-a",
            "project-1",
            "run-1",
            wrong_plan,
        ),
        Err(DeliveryError::MissingEvidence(_))
    ));
    assert_eq!(
        core.load("tenant-a", "project-1")
            .unwrap()
            .unwrap()
            .revision,
        6
    );
}

#[test]
fn cross_tenant_and_cross_role_commands_fail_before_mutation() {
    let temp = TempDir::new().unwrap();
    let core = DeliveryCore::new(store(&temp), FakeIntegration::new().0);
    let mut foreign = candidate(5, &["developer"]);
    foreign.tenant_id = "tenant-b".to_string();
    foreign = foreign.seal().unwrap();
    assert!(matches!(
        core.register_candidate(
            &context("developer", AuthorityRole::Developer, "foreign", 100),
            foreign,
        ),
        Err(DeliveryError::AuthorityDenied(_))
    ));
    assert!(matches!(
        core.register_candidate(
            &context("customer", AuthorityRole::Customer, "wrong-role", 100),
            candidate(5, &["developer"]),
        ),
        Err(DeliveryError::AuthorityDenied(_))
    ));
    assert!(core.load("tenant-a", "project-1").unwrap().is_none());
    assert!(core.load("tenant-b", "project-1").unwrap().is_none());
}

#[test]
fn exact_gate_manifest_delivery_and_explicit_customer_acceptance_form_one_lineage() {
    let temp = TempDir::new().unwrap();
    let core = DeliveryCore::new(store(&temp), FakeIntegration::new().0);
    let candidate = candidate(5, &["developer"]);
    core.register_candidate(
        &context("developer", AuthorityRole::Developer, "register-1", 100),
        candidate.clone(),
    )
    .unwrap();
    let plan = plan(&candidate);
    let qa = principal("qa-1", AuthorityRole::Qa);
    core.assign_qa(
        &context(
            "release-manager",
            AuthorityRole::ReleaseManager,
            "assign-1",
            110,
        ),
        "tenant-a",
        "project-1",
        &candidate.candidate_id,
        plan.clone(),
        run(&plan, qa.clone()),
    )
    .unwrap();
    for (key, next, now) in [
        ("admit", QaRunState::Admitted, 120),
        ("running", QaRunState::Running, 130),
    ] {
        core.transition_qa(
            &context("qa-1", AuthorityRole::Qa, key, now),
            "tenant-a",
            "project-1",
            "run-1",
            next,
        )
        .unwrap();
    }
    core.execute_qa(
        &context("qa-1", AuthorityRole::Qa, "execute", 140),
        "tenant-a",
        "project-1",
        "run-1",
    )
    .unwrap();
    core.transition_qa(
        &context("qa-1", AuthorityRole::Qa, "pass", 150),
        "tenant-a",
        "project-1",
        "run-1",
        QaRunState::CompletedPass,
    )
    .unwrap();

    let release_manager = principal("release-manager", AuthorityRole::ReleaseManager);
    let candidate_ref = VersionedRefV1 {
        id: candidate.candidate_id.clone(),
        generation: candidate.generation,
        digest: candidate.candidate_digest.clone(),
    };
    let plan_ref = VersionedRefV1 {
        id: plan.plan_id.clone(),
        generation: plan.generation,
        digest: plan.plan_digest.clone(),
    };
    let gate_stub = VersionedRefV1 {
        id: "gate-1".to_string(),
        generation: 1,
        digest: ContentDigest::zero(),
    };
    let mut manifest = ReleaseManifestV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        manifest_id: "manifest-1".to_string(),
        generation: 1,
        tenant_id: "tenant-a".to_string(),
        agreement: candidate.agreement.clone(),
        project: candidate.project.clone(),
        candidate: candidate_ref.clone(),
        work_items_digest: candidate.work_items_digest.clone(),
        source_digest: candidate.source_digest.clone(),
        artifacts: candidate.artifacts.clone(),
        toolchain_digest: candidate.toolchain_digest.clone(),
        runtime_profile_digest: candidate.runtime_profile_digest.clone(),
        qa_gate: gate_stub,
        qa_evidence_digest: digest("qa-evidence"),
        sbom_digest: digest("sbom"),
        dependency_snapshot_digest: digest("dependencies"),
        provenance_digest: digest("provenance"),
        release_actor: release_manager.clone(),
        cost: candidate.cost.clone(),
        rollback_release: None,
        manifest_digest: ContentDigest::zero(),
        created_at_ms: 160,
    };
    let gate = QaReleaseGateReceiptV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        gate_id: "gate-1".to_string(),
        generation: 1,
        candidate: candidate_ref,
        plan: plan_ref,
        case_inventory_digest: digest("case-inventory"),
        deterministic_evidence_digest: digest("deterministic-evidence"),
        model_evidence_digest: None,
        calibration_digest: digest("calibration"),
        source_evidence_digest: digest("source-evidence"),
        flake_disposition_digest: None,
        policy_digest: digest("gate-policy"),
        release_manifest_digest: manifest.gate_input_digest().unwrap(),
        actor: qa,
        passed: true,
        issued_at_ms: 165,
        expires_at_ms: 1_000,
    };
    manifest.qa_gate.digest = ContentDigest::of(&gate).unwrap();
    manifest = manifest.seal().unwrap();
    let gate_ref = VersionedRefV1 {
        id: gate.gate_id.clone(),
        generation: gate.generation,
        digest: ContentDigest::of(&gate).unwrap(),
    };
    core.record_review_bundle(
        &context("qa-1", AuthorityRole::Qa, "review-bundle", 165),
        "tenant-a",
        "project-1",
        "run-1",
        ReviewV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            review_id: "review-1".to_string(),
            generation: 1,
            candidate: gate.candidate.clone(),
            reviewer: gate.actor.clone(),
            findings_digest: digest("findings"),
            approved: true,
            created_at_ms: 165,
        },
        TestRunV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            test_run_id: "test-run-1".to_string(),
            generation: 1,
            candidate: gate.candidate.clone(),
            qa_plan: gate.plan.clone(),
            runner_receipt: reference("runner-receipt", 1),
            result_inventory_digest: digest("result-inventory"),
            logs_digest: digest("logs"),
            screenshots_digest: Some(digest("screenshots")),
            passed: true,
        },
        vec![],
        Some(ApprovalV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            approval_id: "approval-1".to_string(),
            generation: 1,
            candidate: gate.candidate.clone(),
            gate: gate_ref,
            approver: gate.actor.clone(),
            policy_digest: gate.policy_digest.clone(),
            approved_at_ms: 165,
        }),
    )
    .unwrap();
    core.record_gate(
        &context("qa-1", AuthorityRole::Qa, "gate", 170),
        "tenant-a",
        "project-1",
        "run-1",
        gate,
    )
    .unwrap();

    let release = ReleaseV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        release_id: "release-1".to_string(),
        generation: 1,
        manifest: VersionedRefV1 {
            id: manifest.manifest_id.clone(),
            generation: manifest.generation,
            digest: manifest.manifest_digest.clone(),
        },
        state: ReleaseState::Approved,
        activated_at_ms: None,
    };
    core.promote(
        &context(
            "release-manager",
            AuthorityRole::ReleaseManager,
            "promote",
            180,
        ),
        "tenant-a",
        "project-1",
        &candidate.candidate_id,
        manifest,
        release,
    )
    .unwrap();

    let active = core
        .load("tenant-a", "project-1")
        .unwrap()
        .unwrap()
        .releases["release-1"]
        .clone();
    let release_ref = VersionedRefV1 {
        id: active.release_id.clone(),
        generation: active.generation,
        digest: ContentDigest::of(&active).unwrap(),
    };
    let delivery = DeliveryReceiptV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        delivery_id: "delivery-1".to_string(),
        generation: 1,
        tenant_id: "tenant-a".to_string(),
        release: release_ref.clone(),
        customer_principal_id: "customer-1".to_string(),
        preview_digest: digest("bounded-preview"),
        receipt_digest: ContentDigest::zero(),
        state: DeliveryState::PreviewReady,
        issued_at_ms: 190,
        expires_at_ms: 1_000,
    }
    .seal()
    .unwrap();
    core.issue_delivery(
        &context(
            "release-manager",
            AuthorityRole::ReleaseManager,
            "deliver",
            190,
        ),
        "project-1",
        delivery,
    )
    .unwrap();

    let delivered = core
        .load("tenant-a", "project-1")
        .unwrap()
        .unwrap()
        .deliveries["delivery-1"]
        .clone();
    let delivery_ref = VersionedRefV1 {
        id: delivered.delivery_id.clone(),
        generation: delivered.generation,
        digest: delivered.receipt_digest.clone(),
    };
    let customer = principal("customer-1", AuthorityRole::Customer);
    let feedback = CustomerFeedbackV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        feedback_id: "feedback-1".to_string(),
        generation: 1,
        delivery: delivery_ref.clone(),
        customer: customer.clone(),
        action: CustomerAction::Accept,
        feedback_digest: ContentDigest::zero(),
        requested_work_item_refs: vec![],
        created_at_ms: 200,
    }
    .seal()
    .unwrap();
    let acceptance = AcceptanceV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        acceptance_id: "acceptance-1".to_string(),
        generation: 1,
        delivery: delivery_ref,
        release: release_ref,
        customer,
        acceptance_digest: ContentDigest::zero(),
        accepted_at_ms: 200,
    }
    .seal()
    .unwrap();
    core.customer_action(
        &context("customer-1", AuthorityRole::Customer, "accept", 200),
        "tenant-a",
        "project-1",
        feedback,
        Some(acceptance),
    )
    .unwrap();

    let aggregate = core.load("tenant-a", "project-1").unwrap().unwrap();
    assert_eq!(aggregate.revision, 11);
    assert_eq!(
        aggregate.deliveries["delivery-1"].state,
        DeliveryState::Accepted
    );
    assert_eq!(aggregate.acceptances.len(), 1);
    assert_eq!(aggregate.approvals.len(), 1);
    assert_eq!(aggregate.manifests.len(), 1);
    assert_eq!(aggregate.active_release_id.as_deref(), Some("release-1"));
    drop(core);

    let reopened = DeliveryCore::new(store(&temp), FakeIntegration::new().0);
    let recovered = reopened.load("tenant-a", "project-1").unwrap().unwrap();
    assert_eq!(recovered.revision, 11);
    assert_eq!(
        recovered.deliveries["delivery-1"].state,
        DeliveryState::Accepted
    );
    assert_eq!(recovered.acceptances.len(), 1);
    assert_eq!(recovered.active_release_id.as_deref(), Some("release-1"));
    assert_eq!(
        reopened
            .store()
            .journal("tenant-a", "project-1")
            .unwrap()
            .len(),
        11
    );
    assert_eq!(reopened.store().pending_publications().unwrap().len(), 11);
}

#[test]
fn rollback_is_atomic_idempotent_and_restart_safe() {
    let temp = TempDir::new().unwrap();
    let mut aggregate = DeliveryAggregateV1::new("tenant-a", "project-1");
    let previous = ReleaseV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        release_id: "release-previous".to_string(),
        generation: 3,
        manifest: reference("manifest-previous", 3),
        state: ReleaseState::Superseded,
        activated_at_ms: Some(100),
    };
    let failed = ReleaseV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        release_id: "release-failed".to_string(),
        generation: 4,
        manifest: reference("manifest-failed", 4),
        state: ReleaseState::Active,
        activated_at_ms: Some(200),
    };
    aggregate
        .releases
        .insert(previous.release_id.clone(), previous.clone());
    aggregate
        .releases
        .insert(failed.release_id.clone(), failed.clone());
    aggregate.active_release_id = Some(failed.release_id.clone());
    let core = core_with_seeded_aggregate(&temp, aggregate);
    let command = context(
        "release-manager",
        AuthorityRole::ReleaseManager,
        "rollback-1",
        300,
    );
    let rollback = RollbackV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        rollback_id: "rollback-1".to_string(),
        generation: 1,
        from_release: VersionedRefV1 {
            id: failed.release_id.clone(),
            generation: failed.generation,
            digest: ContentDigest::of(&failed).unwrap(),
        },
        to_release: VersionedRefV1 {
            id: previous.release_id.clone(),
            generation: previous.generation,
            digest: ContentDigest::of(&previous).unwrap(),
        },
        actor: command.principal.clone(),
        reason_digest: digest("rollback-reason"),
        effect_receipt: reference("rollback-effect", 1),
        created_at_ms: 300,
    };
    let first = core.rollback(&command, "tenant-a", "project-1", rollback.clone());
    assert!(!first.unwrap().duplicate);
    assert!(
        core.rollback(&command, "tenant-a", "project-1", rollback)
            .unwrap()
            .duplicate
    );
    let after = core.load("tenant-a", "project-1").unwrap().unwrap();
    assert_eq!(after.active_release_id.as_deref(), Some("release-previous"));
    assert_eq!(
        after.releases["release-failed"].state,
        ReleaseState::RolledBack
    );
    assert_eq!(
        after.releases["release-previous"].state,
        ReleaseState::Active
    );
    assert_eq!(after.rollbacks.len(), 1);
    drop(core);

    let reopened = store(&temp).load("tenant-a", "project-1").unwrap().unwrap();
    assert_eq!(
        reopened.active_release_id.as_deref(),
        Some("release-previous")
    );
    assert_eq!(reopened.rollbacks.len(), 1);
}

#[test]
fn closeout_requires_bound_memory_receipt_and_survives_restart() {
    let temp = TempDir::new().unwrap();
    let customer = principal("customer-1", AuthorityRole::Customer);
    let release_ref = reference("release-1", 1);
    let delivery_ref = reference("delivery-1", 1);
    let acceptance = AcceptanceV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        acceptance_id: "acceptance-1".to_string(),
        generation: 1,
        delivery: delivery_ref,
        release: release_ref.clone(),
        customer,
        acceptance_digest: ContentDigest::zero(),
        accepted_at_ms: 200,
    }
    .seal()
    .unwrap();
    let mut aggregate = DeliveryAggregateV1::new("tenant-a", "project-1");
    aggregate
        .acceptances
        .insert(acceptance.acceptance_id.clone(), acceptance.clone());
    let core = core_with_seeded_aggregate(&temp, aggregate);
    let command = context(
        "release-manager",
        AuthorityRole::ReleaseManager,
        "closeout-1",
        300,
    );
    let mut closeout = ProjectCloseoutV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        closeout_id: "closeout-1".to_string(),
        generation: 1,
        project: reference("project-1", 4),
        accepted_release: release_ref,
        acceptance: VersionedRefV1 {
            id: acceptance.acceptance_id,
            generation: acceptance.generation,
            digest: acceptance.acceptance_digest,
        },
        decisions_digest: digest("decisions"),
        artifact_inventory_digest: digest("artifact-inventory"),
        failures_digest: digest("failures"),
        lessons_digest: digest("lessons"),
        memory_publication: None,
        closed_by: command.principal.clone(),
        created_at_ms: 300,
    };
    assert!(matches!(
        core.closeout(&command, "tenant-a", "project-1", closeout.clone()),
        Err(DeliveryError::AdapterUnavailable { .. })
    ));
    assert_eq!(
        core.load("tenant-a", "project-1")
            .unwrap()
            .unwrap()
            .revision,
        1
    );

    closeout.memory_publication = Some(reference("memory-publication-1", 1));
    let first = core
        .closeout(&command, "tenant-a", "project-1", closeout.clone())
        .unwrap();
    assert!(!first.duplicate);
    assert!(
        core.closeout(&command, "tenant-a", "project-1", closeout)
            .unwrap()
            .duplicate
    );
    drop(core);

    let reopened = store(&temp).load("tenant-a", "project-1").unwrap().unwrap();
    assert_eq!(reopened.closeouts.len(), 1);
    assert_eq!(reopened.revision, 2);
}

struct CorrectPublisher {
    calls: AtomicUsize,
}

impl DeliveryPublicationPort for CorrectPublisher {
    fn publish(
        &self,
        request: &PublicationRequestV1,
    ) -> Result<PublicationReceiptV1, DeliveryError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(PublicationReceiptV1 {
            operation_id: request.operation_id.clone(),
            event_id: format!("event:{}", request.operation_id),
            row_identity: format!("row:{}", request.operation_id),
            payload_digest: request.payload_digest.clone(),
        })
    }
}

#[test]
fn outbox_publication_has_binding_receipts_and_restart_readback() {
    let temp = TempDir::new().unwrap();
    let core = DeliveryCore::new(store(&temp), FakeIntegration::new().0);
    core.register_candidate(
        &context("developer", AuthorityRole::Developer, "register-1", 100),
        candidate(5, &["developer"]),
    )
    .unwrap();
    let publisher = CorrectPublisher {
        calls: AtomicUsize::new(0),
    };
    assert_eq!(core.publish_pending(&publisher).unwrap(), 1);
    assert_eq!(core.publish_pending(&publisher).unwrap(), 0);
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 1);
    drop(core);
    let reopened = store(&temp);
    assert!(reopened.pending_publications().unwrap().is_empty());
}

#[test]
fn wrong_publication_receipt_never_completes_outbox() {
    let temp = TempDir::new().unwrap();
    let core = DeliveryCore::new(store(&temp), FakeIntegration::new().0);
    core.register_candidate(
        &context("developer", AuthorityRole::Developer, "register-1", 100),
        candidate(5, &["developer"]),
    )
    .unwrap();
    let pending = core.store().pending_publications().unwrap();
    let request = &pending[0].request;
    let wrong = PublicationReceiptV1 {
        operation_id: request.operation_id.clone(),
        event_id: "event-wrong".to_string(),
        row_identity: "row-wrong".to_string(),
        payload_digest: digest("wrong"),
    };
    assert!(matches!(
        core.store().mark_published(&request.payload_digest, wrong),
        Err(DeliveryError::Conflict(_))
    ));
    assert_eq!(core.store().pending_publications().unwrap().len(), 1);
}

#[test]
fn legal_state_machines_reject_terminal_reopen_and_shortcuts() {
    assert!(transition_qa_run(QaRunState::CompletedPass, QaRunState::Running).is_err());
    assert!(transition_candidate(CandidateState::Draft, CandidateState::Promoted).is_err());
    assert!(transition_delivery(DeliveryState::PreviewReady, DeliveryState::Accepted).is_err());
    assert!(transition_release(ReleaseState::Approved, ReleaseState::Active).is_ok());
}
