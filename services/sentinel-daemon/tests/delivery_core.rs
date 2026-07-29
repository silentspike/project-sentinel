use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
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

fn fixture_manifest(id: &str, generation: u64) -> ReleaseManifestV1 {
    ReleaseManifestV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        manifest_id: id.to_string(),
        generation,
        tenant_id: "tenant-a".to_string(),
        agreement: reference("agreement-1", 3),
        project: reference("project-1", 4),
        candidate: reference("candidate-fixture", 1),
        work_items_digest: digest("work-items"),
        source_digest: digest("source"),
        artifacts: vec![],
        toolchain_digest: digest("toolchain"),
        runtime_profile_digest: digest("runtime"),
        qa_gate: reference("gate-fixture", 1),
        qa_evidence_digest: digest("qa-evidence"),
        sbom_digest: digest("sbom"),
        dependency_snapshot_digest: digest("dependencies"),
        provenance_digest: digest("provenance"),
        release_actor: principal("release-manager", AuthorityRole::ReleaseManager),
        cost: cost(),
        rollback_release: None,
        manifest_digest: ContentDigest::zero(),
        created_at_ms: 1,
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
        durable_event_generation: 0,
        started_at_ms: None,
        finished_at_ms: None,
        attempts: 0,
        harness_outcome: None,
        cleanup_receipt: None,
        aggregate_outcomes: None,
        gate_receipt: None,
    }
}

fn evidence_graph(run: &VersionedRefV1, receipt: &WorkbenchEvidenceReceiptV1) -> QaEvidenceGraphV1 {
    let source = SourceTupleV1 {
        owner: "qa-fixtures".to_string(),
        source_type: "repository_fixture".to_string(),
        id: "fixture-source".to_string(),
        generation: 1,
        digest: digest("fixture-source"),
    };
    let cases: Vec<_> = ["security", "structure"]
        .into_iter()
        .map(|case_id| QaDatasetCaseV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            case_id: case_id.to_string(),
            generation: 1,
            split: DatasetSplit::HiddenHoldout,
            required: true,
            required_class: "deterministic".to_string(),
            slices: BTreeMap::from([("surface".to_string(), case_id.to_string())]),
            input_digest: digest(&format!("{case_id}-input")),
            oracle_digest: digest(&format!("{case_id}-oracle")),
            provenance: vec![source.clone()],
            license: "internal-test-fixture".to_string(),
            access_policy_digest: digest("fixture-access"),
            contamination_policy_digest: digest("fixture-contamination"),
            retired_at_ms: None,
            superseded_by: None,
            data_control: data_control(),
        })
        .collect();
    let results = cases
        .iter()
        .enumerate()
        .map(|(index, case)| {
            let outcome = if index == 0 {
                match receipt.harness_outcome {
                    QaHarnessOutcome::Pass => QaCaseOutcome::Pass,
                    QaHarnessOutcome::Fail => QaCaseOutcome::Fail,
                    QaHarnessOutcome::Error => QaCaseOutcome::Error,
                }
            } else {
                QaCaseOutcome::Pass
            };
            QaCaseResultV1 {
                schema_version: DELIVERY_SCHEMA_V1,
                result_id: format!("result-{}", case.case_id),
                generation: 1,
                run: run.clone(),
                case_ref: VersionedRefV1 {
                    id: case.case_id.clone(),
                    generation: case.generation,
                    digest: ContentDigest::of_domain("qa-dataset-case", DELIVERY_SCHEMA_V1, case)
                        .unwrap(),
                },
                outcome,
                required: true,
                reason_code: match outcome {
                    QaCaseOutcome::Pass => QaCaseReasonCode::Verified,
                    QaCaseOutcome::Fail => QaCaseReasonCode::AssertionFailed,
                    QaCaseOutcome::Error => QaCaseReasonCode::HarnessError,
                    _ => unreachable!("fixture outcome is closed"),
                },
                sources: vec![source.clone()],
                assertion_refs: vec![],
                grader_refs: vec![],
                slices: case.slices.clone(),
                attempts: 1,
                disposition: None,
            }
        })
        .collect();
    QaEvidenceGraphV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        run: run.clone(),
        workbench_receipt: VersionedRefV1 {
            id: receipt.invocation.id.clone(),
            generation: receipt.invocation.generation,
            digest: receipt.receipt_digest.clone(),
        },
        dataset_cases: cases,
        case_results: results,
        deterministic_results: vec![],
        model_results: vec![],
        flake_dispositions: vec![],
        graph_digest: ContentDigest::zero(),
    }
    .seal()
    .unwrap()
}

struct FakeIntegration {
    qa_calls: Arc<AtomicUsize>,
    controls: Arc<FakeControls>,
}

struct FakeControls {
    authority_generation: AtomicUsize,
    contract_generation: AtomicUsize,
    receipt_contract_generation_delta: AtomicUsize,
    stale_contract_digest: AtomicUsize,
    authorize_calls: AtomicUsize,
    flip_authority_on_call: AtomicUsize,
    replacement_principal: Mutex<Option<String>>,
    harness_outcome: Mutex<QaHarnessOutcome>,
    replay_workbench_receipt: Mutex<Option<WorkbenchEvidenceReceiptV1>>,
}

impl Default for FakeControls {
    fn default() -> Self {
        Self {
            authority_generation: AtomicUsize::new(7),
            contract_generation: AtomicUsize::new(7),
            receipt_contract_generation_delta: AtomicUsize::new(0),
            stale_contract_digest: AtomicUsize::new(0),
            authorize_calls: AtomicUsize::new(0),
            flip_authority_on_call: AtomicUsize::new(0),
            replacement_principal: Mutex::new(None),
            harness_outcome: Mutex::new(QaHarnessOutcome::Pass),
            replay_workbench_receipt: Mutex::new(None),
        }
    }
}

impl FakeIntegration {
    fn new() -> (Self, Arc<AtomicUsize>) {
        let qa_calls = Arc::new(AtomicUsize::new(0));
        let controls = Arc::new(FakeControls::default());
        (
            Self {
                qa_calls: Arc::clone(&qa_calls),
                controls,
            },
            qa_calls,
        )
    }

    fn controlled() -> (Self, Arc<FakeControls>) {
        let qa_calls = Arc::new(AtomicUsize::new(0));
        let controls = Arc::new(FakeControls::default());
        (
            Self {
                qa_calls,
                controls: Arc::clone(&controls),
            },
            controls,
        )
    }
}

impl DeliveryIntegrationPort for FakeIntegration {
    fn readiness(&self) -> AdapterReadiness {
        AdapterReadiness::Ready {
            contract_version: 1,
            authority_generation: self.controls.contract_generation.load(Ordering::SeqCst) as u64,
            contract_digest: if self.controls.stale_contract_digest.load(Ordering::SeqCst) == 0 {
                expected_integration_contract_digest()
            } else {
                digest("stale-integration-contract")
            },
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

    fn authorize(
        &self,
        request: &AuthorityValidationRequestV1,
    ) -> Result<AuthorityReceiptV1, DeliveryError> {
        let call = self.controls.authorize_calls.fetch_add(1, Ordering::SeqCst) + 1;
        let flip_on = self.controls.flip_authority_on_call.load(Ordering::SeqCst);
        let authority_generation = self.controls.authority_generation.load(Ordering::SeqCst)
            + usize::from(flip_on != 0 && call >= flip_on);
        let principal_id = self
            .controls
            .replacement_principal
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| request.principal_id.clone());
        AuthorityReceiptV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            request_digest: request.request_digest.clone(),
            principal: PrincipalV1 {
                tenant_id: request.tenant_id.clone(),
                principal_id,
                authority_generation: authority_generation as u64,
                roles: BTreeSet::from([request.required_role.clone()]),
            },
            contract_version: request.contract_version,
            contract_authority_generation: self.controls.contract_generation.load(Ordering::SeqCst)
                as u64
                + self
                    .controls
                    .receipt_contract_generation_delta
                    .load(Ordering::SeqCst) as u64,
            contract_digest: request.contract_digest.clone(),
            issued_at_ms: 1,
            expires_at_ms: 10_000,
            issuer: "test-authority".to_string(),
            receipt_digest: ContentDigest::zero(),
        }
        .seal()
    }

    fn execute_qa(
        &self,
        request: &WorkbenchEvidenceRequestV1,
    ) -> Result<WorkbenchEvidenceReceiptV1, DeliveryError> {
        self.qa_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(receipt) = self
            .controls
            .replay_workbench_receipt
            .lock()
            .unwrap()
            .clone()
        {
            return Ok(receipt);
        }
        let harness_outcome = *self.controls.harness_outcome.lock().unwrap();
        let mut receipt = WorkbenchEvidenceReceiptV1 {
            schema_version: 1,
            invocation: request.invocation.clone(),
            assignment: request.qa_run.clone(),
            qa_run: request.qa_run.clone(),
            assigned_qa: request.assigned_qa.clone(),
            authority_receipt_digest: request.authority_receipt_digest.clone(),
            input_digest: request.request_digest.clone(),
            output_digest: digest("qa-output"),
            artifact_ownership_digest: digest("ownership"),
            result_inventory_digest: ContentDigest::zero(),
            logs_digest: digest("logs"),
            screenshots_digest: Some(digest("screenshots")),
            failure_classification_digest: digest("failure-classes"),
            harness_outcome,
            required_cases_complete: true,
            contaminated: false,
            needs_human_review: false,
            flaky_unresolved: false,
            cleanup_receipt: reference("cleanup-1", 1),
            receipt_digest: ContentDigest::zero(),
        };
        receipt.result_inventory_digest =
            qa_evidence_inventory_digest(&evidence_graph(&request.qa_run, &receipt)).unwrap();
        receipt.seal()
    }
}

fn store(temp: &TempDir) -> DeliveryStore {
    DeliveryStore::open_test_only(&temp.path().join("delivery.redb")).unwrap()
}

#[derive(Clone, Copy)]
struct FakeEffects;

impl DeliveryEffectPort for FakeEffects {
    fn apply(
        &self,
        request: &DeliveryEffectRequestV1,
    ) -> Result<DeliveryEffectReceiptV1, DeliveryError> {
        DeliveryEffectReceiptV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            kind: request.kind,
            tenant_id: request.tenant_id.clone(),
            project: request.project.clone(),
            candidate: request.candidate.clone(),
            request_digest: request.request_digest.clone(),
            actor_authority_receipt_digest: request.actor_authority_receipt_digest.clone(),
            effect_ref: reference(
                &format!("effect-{:?}", request.kind).to_ascii_lowercase(),
                1,
            ),
            issuer: "test-effects".to_string(),
            issued_at_ms: 1,
            receipt_digest: ContentDigest::zero(),
        }
        .seal()
    }
}

fn completed_qa_core(
    temp: &TempDir,
) -> (
    DeliveryCore<FakeIntegration, DeliveryStore, FakeEffects>,
    ReleaseCandidateV1,
    QaEvaluationPlanV1,
    PrincipalV1,
) {
    let core = DeliveryCore::new_test_only(store(temp), FakeIntegration::new().0, FakeEffects);
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
    let (_, workbench) = core
        .execute_qa(
            &context("qa-1", AuthorityRole::Qa, "execute-1", 140),
            "tenant-a",
            "project-1",
            "run-1",
        )
        .unwrap();
    let run_ref = VersionedRefV1 {
        id: "run-1".to_string(),
        generation: 1,
        digest: digest("run-request"),
    };
    core.import_evidence_graph(
        &context("qa-1", AuthorityRole::Qa, "evidence-1", 145),
        "tenant-a",
        "project-1",
        "run-1",
        evidence_graph(&run_ref, &workbench),
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

fn running_qa_core(
    temp: &TempDir,
    integration: FakeIntegration,
) -> (
    DeliveryCore<FakeIntegration, DeliveryStore, FakeEffects>,
    ReleaseCandidateV1,
    QaEvaluationPlanV1,
    PrincipalV1,
) {
    let core = DeliveryCore::new_test_only(store(temp), integration, FakeEffects);
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
    (core, candidate, plan, qa)
}

fn core_with_seeded_aggregate(
    temp: &TempDir,
    aggregate: DeliveryAggregateV1,
) -> DeliveryCore<FakeIntegration, DeliveryStore, FakeEffects> {
    DeliveryCore::new_test_only(
        seeded_store(temp, aggregate),
        FakeIntegration::new().0,
        FakeEffects,
    )
}

fn seeded_store(temp: &TempDir, mut aggregate: DeliveryAggregateV1) -> DeliveryStore {
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
    store
}

#[test]
fn unavailable_integration_fails_closed_without_preventing_store_startup() {
    let temp = TempDir::new().unwrap();
    let core = DeliveryCore::new_test_only(
        store(&temp),
        UnavailableDeliveryIntegration,
        UnavailableDeliveryEffects,
    );
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
        let core = DeliveryCore::new_test_only(store(&temp), FakeIntegration::new().0, FakeEffects);
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
    let core = DeliveryCore::new_test_only(store(&temp), FakeIntegration::new().0, FakeEffects);
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
    assert!(matches!(
        result,
        Err(DeliveryError::AuthorityDenied(_) | DeliveryError::StaleEvidence(_))
    ));
}

#[test]
fn assignment_rejects_prefilled_execution_evidence_without_mutation() {
    let temp = TempDir::new().unwrap();
    let core = DeliveryCore::new_test_only(store(&temp), FakeIntegration::new().0, FakeEffects);
    let candidate = candidate(5, &["developer"]);
    core.register_candidate(
        &context("developer", AuthorityRole::Developer, "register-1", 100),
        candidate.clone(),
    )
    .unwrap();
    let plan = plan(&candidate);
    let mut forged = run(&plan, principal("qa-1", AuthorityRole::Qa));
    forged.harness_outcome = Some(QaHarnessOutcome::Pass);
    forged.cleanup_receipt = Some(reference("forged-cleanup", 1));
    forged.aggregate_outcomes = Some(QaAggregateOutcomesV1 {
        required_cases_complete: true,
        contaminated: false,
        needs_human_review: false,
        flaky_unresolved: false,
    });
    assert!(matches!(
        core.assign_qa(
            &context(
                "release-manager",
                AuthorityRole::ReleaseManager,
                "assign-forged",
                110,
            ),
            "tenant-a",
            "project-1",
            &candidate.candidate_id,
            plan,
            forged,
        ),
        Err(DeliveryError::Validation(_))
    ));
    let aggregate = core.load("tenant-a", "project-1").unwrap().unwrap();
    assert_eq!(
        aggregate.candidates[&candidate.candidate_id].state,
        CandidateState::Draft
    );
    assert!(aggregate.qa_runs.is_empty());
}

#[test]
fn stale_adapter_contract_and_replaced_actor_fail_closed() {
    let stale_temp = TempDir::new().unwrap();
    let (stale_integration, stale_controls) = FakeIntegration::controlled();
    stale_controls
        .stale_contract_digest
        .store(1, Ordering::SeqCst);
    let stale_core =
        DeliveryCore::new_test_only(store(&stale_temp), stale_integration, FakeEffects);
    assert!(matches!(
        stale_core.register_candidate(
            &context("developer", AuthorityRole::Developer, "register-stale", 100),
            candidate(5, &["developer"]),
        ),
        Err(DeliveryError::StaleEvidence(_))
    ));

    let stale_generation_temp = TempDir::new().unwrap();
    let (stale_generation_integration, stale_generation_controls) = FakeIntegration::controlled();
    stale_generation_controls
        .receipt_contract_generation_delta
        .store(1, Ordering::SeqCst);
    let stale_generation_core = DeliveryCore::new_test_only(
        store(&stale_generation_temp),
        stale_generation_integration,
        FakeEffects,
    );
    assert!(matches!(
        stale_generation_core.register_candidate(
            &context(
                "developer",
                AuthorityRole::Developer,
                "register-stale-generation",
                100,
            ),
            candidate(5, &["developer"]),
        ),
        Err(DeliveryError::StaleEvidence(_))
    ));

    let revoked_generation_temp = TempDir::new().unwrap();
    let (revoked_generation_integration, revoked_generation_controls) =
        FakeIntegration::controlled();
    revoked_generation_controls
        .authority_generation
        .store(8, Ordering::SeqCst);
    let revoked_generation_core = DeliveryCore::new_test_only(
        store(&revoked_generation_temp),
        revoked_generation_integration,
        FakeEffects,
    );
    assert!(matches!(
        revoked_generation_core.register_candidate(
            &context(
                "developer",
                AuthorityRole::Developer,
                "register-revoked-generation",
                100,
            ),
            candidate(5, &["developer"]),
        ),
        Err(DeliveryError::StaleEvidence(_))
    ));

    let replaced_temp = TempDir::new().unwrap();
    let (integration, controls) = FakeIntegration::controlled();
    let (core, _, _, _) = running_qa_core(&replaced_temp, integration);
    *controls.replacement_principal.lock().unwrap() = Some("qa-replacement".to_string());
    assert!(matches!(
        core.transition_qa(
            &context("qa-1", AuthorityRole::Qa, "replaced", 140),
            "tenant-a",
            "project-1",
            "run-1",
            QaRunState::CompletedPass,
        ),
        Err(DeliveryError::StaleEvidence(_))
    ));
    assert_eq!(
        core.load("tenant-a", "project-1").unwrap().unwrap().qa_runs["run-1"].state,
        QaRunState::Running
    );
}

#[test]
fn authority_toctou_after_workbench_effect_never_adopts_evidence() {
    let temp = TempDir::new().unwrap();
    let (integration, controls) = FakeIntegration::controlled();
    let qa_calls = Arc::clone(&integration.qa_calls);
    let (core, _, _, _) = running_qa_core(&temp, integration);
    let next_authorize = controls.authorize_calls.load(Ordering::SeqCst) + 2;
    controls
        .flip_authority_on_call
        .store(next_authorize, Ordering::SeqCst);
    assert!(matches!(
        core.execute_qa(
            &context("qa-1", AuthorityRole::Qa, "execute-toctou", 140),
            "tenant-a",
            "project-1",
            "run-1",
        ),
        Err(DeliveryError::StaleEvidence(_))
    ));
    assert_eq!(qa_calls.load(Ordering::SeqCst), 1);
    let aggregate = core.load("tenant-a", "project-1").unwrap().unwrap();
    assert!(aggregate.workbench_receipts.is_empty());
    assert_eq!(aggregate.qa_runs["run-1"].attempts, 0);
}

#[test]
fn qa_effect_uses_stable_request_and_is_not_repeated_on_retry() {
    let temp = TempDir::new().unwrap();
    let (integration, qa_calls) = FakeIntegration::new();
    let core = DeliveryCore::new_test_only(store(&temp), integration, FakeEffects);
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
fn workbench_receipt_cannot_be_replayed_across_runs() {
    let temp = TempDir::new().unwrap();
    let (integration, controls) = FakeIntegration::controlled();
    let (core, _, _, _) = running_qa_core(&temp, integration);
    let (_, first_receipt) = core
        .execute_qa(
            &context("qa-1", AuthorityRole::Qa, "execute-1", 140),
            "tenant-a",
            "project-1",
            "run-1",
        )
        .unwrap();
    *controls.replay_workbench_receipt.lock().unwrap() = Some(first_receipt);

    let mut second_candidate = candidate(6, &["developer"]);
    second_candidate.project.generation = 5;
    second_candidate = second_candidate.seal().unwrap();
    core.register_candidate(
        &context("developer", AuthorityRole::Developer, "register-2", 150),
        second_candidate.clone(),
    )
    .unwrap();
    let mut second_plan = plan(&second_candidate);
    second_plan.plan_id = "plan-2".to_string();
    second_plan.request = reference("request-2", 1);
    second_plan = second_plan.seal().unwrap();
    let mut second_run = run(&second_plan, principal("qa-1", AuthorityRole::Qa));
    second_run.run_id = "run-2".to_string();
    second_run.request_digest = digest("run-request-2");
    core.assign_qa(
        &context(
            "release-manager",
            AuthorityRole::ReleaseManager,
            "assign-2",
            160,
        ),
        "tenant-a",
        "project-1",
        &second_candidate.candidate_id,
        second_plan,
        second_run,
    )
    .unwrap();
    for (key, next, now) in [
        ("admit-2", QaRunState::Admitted, 170),
        ("running-2", QaRunState::Running, 180),
    ] {
        core.transition_qa(
            &context("qa-1", AuthorityRole::Qa, key, now),
            "tenant-a",
            "project-1",
            "run-2",
            next,
        )
        .unwrap();
    }
    assert!(matches!(
        core.execute_qa(
            &context("qa-1", AuthorityRole::Qa, "execute-2", 190),
            "tenant-a",
            "project-1",
            "run-2",
        ),
        Err(DeliveryError::StaleEvidence(_))
    ));
    let aggregate = core.load("tenant-a", "project-1").unwrap().unwrap();
    assert_eq!(aggregate.qa_runs["run-2"].attempts, 0);
}

#[test]
fn completed_pass_requires_clean_structured_workbench_evidence() {
    let temp = TempDir::new().unwrap();
    let core = DeliveryCore::new_test_only(store(&temp), FakeIntegration::new().0, FakeEffects);
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
fn failed_qa_records_durable_non_promotable_gate() {
    let temp = TempDir::new().unwrap();
    let (integration, controls) = FakeIntegration::controlled();
    *controls.harness_outcome.lock().unwrap() = QaHarnessOutcome::Fail;
    let (core, candidate, plan, qa) = running_qa_core(&temp, integration);
    let (_, receipt) = core
        .execute_qa(
            &context("qa-1", AuthorityRole::Qa, "execute-fail", 140),
            "tenant-a",
            "project-1",
            "run-1",
        )
        .unwrap();
    let run_ref = VersionedRefV1 {
        id: "run-1".to_string(),
        generation: 1,
        digest: digest("run-request"),
    };
    let graph = evidence_graph(&run_ref, &receipt);
    core.import_evidence_graph(
        &context("qa-1", AuthorityRole::Qa, "evidence-fail", 145),
        "tenant-a",
        "project-1",
        "run-1",
        graph.clone(),
    )
    .unwrap();
    core.transition_qa(
        &context("qa-1", AuthorityRole::Qa, "complete-fail", 150),
        "tenant-a",
        "project-1",
        "run-1",
        QaRunState::CompletedFail,
    )
    .unwrap();
    let gate = QaReleaseGateReceiptV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        gate_id: "gate-fail".to_string(),
        generation: 1,
        candidate: VersionedRefV1 {
            id: candidate.candidate_id.clone(),
            generation: candidate.generation,
            digest: candidate.candidate_digest,
        },
        plan: VersionedRefV1 {
            id: plan.plan_id,
            generation: plan.generation,
            digest: plan.plan_digest,
        },
        case_inventory_digest: qa_case_inventory_digest(&graph).unwrap(),
        deterministic_evidence_digest: qa_deterministic_evidence_digest(&graph).unwrap(),
        model_evidence_digest: qa_model_evidence_digest(&graph).unwrap(),
        calibration_digest: plan.aggregation_policy_digest,
        source_evidence_digest: qa_source_evidence_digest(&graph).unwrap(),
        flake_disposition_digest: qa_flake_disposition_digest(&graph).unwrap(),
        policy_digest: plan.release_policy_digest,
        release_manifest_digest: digest("non-promotable-manifest"),
        actor: qa,
        passed: false,
        issued_at_ms: 160,
        expires_at_ms: 1_000,
    };
    core.record_gate(
        &context("qa-1", AuthorityRole::Qa, "gate-fail", 170),
        "tenant-a",
        "project-1",
        "run-1",
        gate,
    )
    .unwrap();
    let aggregate = core.load("tenant-a", "project-1").unwrap().unwrap();
    assert_eq!(
        aggregate.candidates[&candidate.candidate_id].state,
        CandidateState::GateFailed
    );
    assert_eq!(aggregate.gates["gate-fail"].passed, false);
    assert_eq!(aggregate.qa_runs["run-1"].state, QaRunState::CompletedFail);
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
        7
    );
}

#[test]
fn cross_tenant_and_cross_role_commands_fail_before_mutation() {
    let temp = TempDir::new().unwrap();
    let core = DeliveryCore::new_test_only(store(&temp), FakeIntegration::new().0, FakeEffects);
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
    let core = DeliveryCore::new_test_only(store(&temp), FakeIntegration::new().0, FakeEffects);
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
    let (_, workbench) = core
        .execute_qa(
            &context("qa-1", AuthorityRole::Qa, "execute", 140),
            "tenant-a",
            "project-1",
            "run-1",
        )
        .unwrap();
    let run_ref = VersionedRefV1 {
        id: "run-1".to_string(),
        generation: 1,
        digest: digest("run-request"),
    };
    let graph = evidence_graph(&run_ref, &workbench);
    core.import_evidence_graph(
        &context("qa-1", AuthorityRole::Qa, "evidence", 145),
        "tenant-a",
        "project-1",
        "run-1",
        graph.clone(),
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
        qa_evidence_digest: graph.graph_digest.clone(),
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
        case_inventory_digest: qa_case_inventory_digest(&graph).unwrap(),
        deterministic_evidence_digest: qa_deterministic_evidence_digest(&graph).unwrap(),
        model_evidence_digest: qa_model_evidence_digest(&graph).unwrap(),
        calibration_digest: plan.aggregation_policy_digest.clone(),
        source_evidence_digest: qa_source_evidence_digest(&graph).unwrap(),
        flake_disposition_digest: qa_flake_disposition_digest(&graph).unwrap(),
        policy_digest: plan.release_policy_digest.clone(),
        release_manifest_digest: manifest.gate_input_digest().unwrap(),
        actor: qa,
        passed: true,
        issued_at_ms: 165,
        expires_at_ms: 1_000,
    };
    manifest.qa_gate.digest =
        ContentDigest::of_domain("qa-release-gate", DELIVERY_SCHEMA_V1, &gate).unwrap();
    manifest = manifest.seal().unwrap();
    let gate_ref = VersionedRefV1 {
        id: gate.gate_id.clone(),
        generation: gate.generation,
        digest: ContentDigest::of_domain("qa-release-gate", DELIVERY_SCHEMA_V1, &gate).unwrap(),
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
            findings_digest: ContentDigest::of_domain(
                "qa-findings",
                DELIVERY_SCHEMA_V1,
                &Vec::<FindingV1>::new(),
            )
            .unwrap(),
            approved: true,
            created_at_ms: 165,
        },
        TestRunV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            test_run_id: "test-run-1".to_string(),
            generation: 1,
            candidate: gate.candidate.clone(),
            qa_plan: gate.plan.clone(),
            runner_receipt: VersionedRefV1 {
                id: workbench.invocation.id.clone(),
                generation: workbench.invocation.generation,
                digest: workbench.receipt_digest.clone(),
            },
            result_inventory_digest: workbench.result_inventory_digest.clone(),
            logs_digest: workbench.logs_digest.clone(),
            screenshots_digest: workbench.screenshots_digest.clone(),
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
        rollout_receipt: None,
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
        digest: ContentDigest::of_domain("release", DELIVERY_SCHEMA_V1, &active).unwrap(),
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
    assert_eq!(aggregate.revision, 12);
    assert_eq!(
        aggregate.deliveries["delivery-1"].state,
        DeliveryState::Accepted
    );
    assert_eq!(aggregate.acceptances.len(), 1);
    assert_eq!(aggregate.approvals.len(), 1);
    assert_eq!(aggregate.manifests.len(), 1);
    assert_eq!(aggregate.active_release_id.as_deref(), Some("release-1"));
    drop(core);

    let reopened = DeliveryCore::new_test_only(store(&temp), FakeIntegration::new().0, FakeEffects);
    let recovered = reopened.load("tenant-a", "project-1").unwrap().unwrap();
    assert_eq!(recovered.revision, 12);
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
        12
    );
    assert_eq!(reopened.store().pending_publications().unwrap().len(), 12);
}

#[test]
fn manifest_and_release_ids_are_immutable_before_rollout_effect() {
    let temp = TempDir::new().unwrap();
    let mut candidate = candidate(5, &["developer"]);
    candidate.state = CandidateState::GatePassed;
    let candidate_ref = VersionedRefV1 {
        id: candidate.candidate_id.clone(),
        generation: candidate.generation,
        digest: candidate.candidate_digest.clone(),
    };
    let qa = principal("qa-1", AuthorityRole::Qa);
    let release_manager = principal("release-manager", AuthorityRole::ReleaseManager);
    let mut manifest = ReleaseManifestV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        manifest_id: "manifest-immutable".to_string(),
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
        qa_gate: VersionedRefV1 {
            id: "gate-immutable".to_string(),
            generation: 1,
            digest: ContentDigest::zero(),
        },
        qa_evidence_digest: digest("qa-evidence"),
        sbom_digest: digest("sbom"),
        dependency_snapshot_digest: digest("dependencies"),
        provenance_digest: digest("provenance"),
        release_actor: release_manager.clone(),
        cost: candidate.cost.clone(),
        rollback_release: None,
        manifest_digest: ContentDigest::zero(),
        created_at_ms: 100,
    };
    let gate = QaReleaseGateReceiptV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        gate_id: "gate-immutable".to_string(),
        generation: 1,
        candidate: candidate_ref,
        plan: reference("plan-immutable", 1),
        case_inventory_digest: digest("cases"),
        deterministic_evidence_digest: digest("deterministic"),
        model_evidence_digest: None,
        calibration_digest: digest("calibration"),
        source_evidence_digest: digest("sources"),
        flake_disposition_digest: None,
        policy_digest: digest("policy"),
        release_manifest_digest: manifest.gate_input_digest().unwrap(),
        actor: qa,
        passed: true,
        issued_at_ms: 100,
        expires_at_ms: 1_000,
    };
    manifest.qa_gate.digest =
        ContentDigest::of_domain("qa-release-gate", DELIVERY_SCHEMA_V1, &gate).unwrap();
    manifest = manifest.seal().unwrap();
    let release = ReleaseV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        release_id: "release-immutable".to_string(),
        generation: 1,
        manifest: VersionedRefV1 {
            id: manifest.manifest_id.clone(),
            generation: manifest.generation,
            digest: manifest.manifest_digest.clone(),
        },
        state: ReleaseState::Approved,
        activated_at_ms: None,
        rollout_receipt: None,
    };
    let mut aggregate = DeliveryAggregateV1::new("tenant-a", "project-1");
    aggregate
        .candidates
        .insert(candidate.candidate_id.clone(), candidate.clone());
    aggregate.gates.insert(gate.gate_id.clone(), gate);
    aggregate
        .manifests
        .insert(manifest.manifest_id.clone(), manifest.clone());
    aggregate
        .releases
        .insert(release.release_id.clone(), release.clone());
    let core = core_with_seeded_aggregate(&temp, aggregate);
    assert!(matches!(
        core.promote(
            &context(
                "release-manager",
                AuthorityRole::ReleaseManager,
                "immutable-reuse",
                200,
            ),
            "tenant-a",
            "project-1",
            &candidate.candidate_id,
            manifest,
            release,
        ),
        Err(DeliveryError::Conflict(_))
    ));
    assert_eq!(
        core.load("tenant-a", "project-1")
            .unwrap()
            .unwrap()
            .revision,
        1
    );
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
        rollout_receipt: Some(reference("rollout-previous", 1)),
    };
    let failed = ReleaseV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        release_id: "release-failed".to_string(),
        generation: 4,
        manifest: reference("manifest-failed", 4),
        state: ReleaseState::Active,
        activated_at_ms: Some(200),
        rollout_receipt: Some(reference("rollout-failed", 1)),
    };
    aggregate
        .releases
        .insert(previous.release_id.clone(), previous.clone());
    aggregate
        .releases
        .insert(failed.release_id.clone(), failed.clone());
    aggregate.manifests.insert(
        "manifest-previous".to_string(),
        fixture_manifest("manifest-previous", 3),
    );
    aggregate.manifests.insert(
        "manifest-failed".to_string(),
        fixture_manifest("manifest-failed", 4),
    );
    aggregate.active_release_id = Some(failed.release_id.clone());
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
            digest: ContentDigest::of_domain("release", DELIVERY_SCHEMA_V1, &failed).unwrap(),
        },
        to_release: VersionedRefV1 {
            id: previous.release_id.clone(),
            generation: previous.generation,
            digest: ContentDigest::of_domain("release", DELIVERY_SCHEMA_V1, &previous).unwrap(),
        },
        actor: command.principal.clone(),
        reason_digest: digest("rollback-reason"),
        effect_receipt: None,
        created_at_ms: 300,
    };

    let unavailable_temp = TempDir::new().unwrap();
    let unavailable_core = DeliveryCore::new_test_only(
        seeded_store(&unavailable_temp, aggregate.clone()),
        FakeIntegration::new().0,
        UnavailableDeliveryEffects,
    );
    assert!(matches!(
        unavailable_core.rollback(&command, "tenant-a", "project-1", rollback.clone()),
        Err(DeliveryError::AdapterUnavailable { .. })
    ));
    let unchanged = unavailable_core
        .load("tenant-a", "project-1")
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.revision, 1);
    assert_eq!(
        unchanged.active_release_id.as_deref(),
        Some("release-failed")
    );
    assert!(unchanged.rollbacks.is_empty());

    let core = core_with_seeded_aggregate(&temp, aggregate);
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
    aggregate
        .manifests
        .insert("manifest-1".to_string(), fixture_manifest("manifest-1", 1));
    aggregate.releases.insert(
        "release-1".to_string(),
        ReleaseV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            release_id: "release-1".to_string(),
            generation: 1,
            manifest: reference("manifest-1", 1),
            state: ReleaseState::Active,
            activated_at_ms: Some(100),
            rollout_receipt: Some(reference("rollout-1", 1)),
        },
    );
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
        memory_publication: Some(reference("forged-memory-publication", 1)),
        closed_by: command.principal.clone(),
        created_at_ms: 300,
    };
    assert!(matches!(
        core.closeout(&command, "tenant-a", "project-1", closeout.clone()),
        Err(DeliveryError::Validation(_))
    ));
    assert_eq!(
        core.load("tenant-a", "project-1")
            .unwrap()
            .unwrap()
            .revision,
        1
    );

    closeout.memory_publication = None;
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
            schema_version: DELIVERY_SCHEMA_V1,
            operation_id: request.operation_id.clone(),
            event_id: format!("event:{}", request.operation_id),
            aggregate_id: request.aggregate_id.clone(),
            row_identity: request.row_identity.clone(),
            payload_digest: request.payload_digest.clone(),
            request_digest: request.request_digest.clone(),
        })
    }
}

#[test]
fn outbox_publication_has_binding_receipts_and_restart_readback() {
    let temp = TempDir::new().unwrap();
    let core = DeliveryCore::new_test_only(store(&temp), FakeIntegration::new().0, FakeEffects);
    core.register_candidate(
        &context("developer", AuthorityRole::Developer, "register-1", 100),
        candidate(5, &["developer"]),
    )
    .unwrap();
    let pending = core.store().pending_publications().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].request.payload_digest,
        ContentDigest::of_bytes_domain(
            "delivery-event-envelope",
            DELIVERY_SCHEMA_V1,
            &pending[0].request.payload,
        )
        .unwrap()
    );
    assert_eq!(
        pending[0].request.request_digest,
        pending[0].request.computed_digest().unwrap()
    );
    drop(core);

    // Crash-before-publish recovery: the canonical request survives unchanged.
    let core = DeliveryCore::new_test_only(store(&temp), FakeIntegration::new().0, FakeEffects);
    let recovered = core.store().pending_publications().unwrap();
    assert_eq!(recovered, pending);
    let publisher = CorrectPublisher {
        calls: AtomicUsize::new(0),
    };
    assert_eq!(core.publish_pending(&publisher).unwrap(), 1);
    assert_eq!(core.publish_pending(&publisher).unwrap(), 0);
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 1);
    let request = &recovered[0].request;
    let same_receipt = PublicationReceiptV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        operation_id: request.operation_id.clone(),
        event_id: format!("event:{}", request.operation_id),
        aggregate_id: request.aggregate_id.clone(),
        row_identity: request.row_identity.clone(),
        payload_digest: request.payload_digest.clone(),
        request_digest: request.request_digest.clone(),
    };
    core.store()
        .mark_published(&request.request_digest, same_receipt)
        .unwrap();
    drop(core);
    let reopened = store(&temp);
    assert!(reopened.pending_publications().unwrap().is_empty());
    reopened.health().unwrap();
}

#[test]
fn wrong_publication_receipt_never_completes_outbox() {
    let temp = TempDir::new().unwrap();
    let core = DeliveryCore::new_test_only(store(&temp), FakeIntegration::new().0, FakeEffects);
    core.register_candidate(
        &context("developer", AuthorityRole::Developer, "register-1", 100),
        candidate(5, &["developer"]),
    )
    .unwrap();
    let pending = core.store().pending_publications().unwrap();
    let request = &pending[0].request;
    let wrong = PublicationReceiptV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        operation_id: request.operation_id.clone(),
        event_id: "event-wrong".to_string(),
        aggregate_id: request.aggregate_id.clone(),
        row_identity: "row-wrong".to_string(),
        payload_digest: digest("wrong"),
        request_digest: request.request_digest.clone(),
    };
    assert!(matches!(
        core.store().mark_published(&request.request_digest, wrong),
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

#[test]
fn wire_records_reject_unknown_fields_and_unknown_closed_values() {
    let mut principal_value = serde_json::to_value(principal("qa-1", AuthorityRole::Qa)).unwrap();
    principal_value
        .as_object_mut()
        .unwrap()
        .insert("forged_role".to_string(), json!("release_manager"));
    assert!(serde_json::from_value::<PrincipalV1>(principal_value).is_err());
    assert!(serde_json::from_value::<QaHarnessOutcome>(json!("caller_defined")).is_err());
    assert!(serde_json::from_value::<FindingClassification>(json!("caller_defined")).is_err());
    assert!(serde_json::from_value::<QaCaseReasonCode>(json!("caller_defined")).is_err());
}
