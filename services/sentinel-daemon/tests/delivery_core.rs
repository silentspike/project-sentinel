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

fn versioned_release(release: &ReleaseV1) -> VersionedRefV1 {
    VersionedRefV1 {
        id: release.release_id.clone(),
        generation: release.generation,
        digest: ContentDigest::of_domain("release", DELIVERY_SCHEMA_V1, release).unwrap(),
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

fn fixture_source() -> SourceTupleV1 {
    SourceTupleV1 {
        owner: "qa-fixtures".to_string(),
        source_type: "repository_fixture".to_string(),
        id: "fixture-source".to_string(),
        generation: 1,
        digest: digest("fixture-source"),
    }
}

fn fixture_cases() -> Vec<QaDatasetCaseV1> {
    let source = fixture_source();
    [("security", true), ("structure", true), ("visual", false)]
        .into_iter()
        .map(|(case_id, required)| QaDatasetCaseV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            case_id: case_id.to_string(),
            generation: 1,
            split: DatasetSplit::HiddenHoldout,
            required,
            required_class: if required {
                "deterministic".to_string()
            } else {
                "optional".to_string()
            },
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
        .collect()
}

fn plan(candidate: &ReleaseCandidateV1) -> QaEvaluationPlanV1 {
    let fixtures = fixture_cases();
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
        fixture_inventory_digest: qa_fixture_inventory_digest(&fixtures).unwrap(),
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

fn evidence_graph(
    run: &VersionedRefV1,
    plan_digest: &ContentDigest,
    receipt: &WorkbenchEvidenceReceiptV1,
) -> QaEvidenceGraphV1 {
    let cases = fixture_cases();
    let deterministic_results: Vec<_> = cases
        .iter()
        .filter(|case| case.required)
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
            QaDeterministicAssertionResultV1 {
                schema_version: DELIVERY_SCHEMA_V1,
                assertion_id: format!("assertion-{}", case.case_id),
                generation: 1,
                plan_digest: plan_digest.clone(),
                case_digest: ContentDigest::of_domain("qa-dataset-case", DELIVERY_SCHEMA_V1, case)
                    .unwrap(),
                assertion_digest: digest(&format!("{}-assertion", case.case_id)),
                oracle_digest: case.oracle_digest.clone(),
                input_digest: case.input_digest.clone(),
                evidence_digest: digest(&format!("{}-evidence", case.case_id)),
                actual_digest: digest(&format!("{}-actual", case.case_id)),
                passed: outcome == QaCaseOutcome::Pass,
            }
        })
        .collect();
    let results = cases
        .iter()
        .filter(|case| case.required)
        .zip(&deterministic_results)
        .enumerate()
        .map(|(index, (case, assertion))| {
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
                sources: case.provenance.clone(),
                assertion_refs: vec![VersionedRefV1 {
                    id: assertion.assertion_id.clone(),
                    generation: assertion.generation,
                    digest: ContentDigest::of_domain(
                        "qa-deterministic-result",
                        DELIVERY_SCHEMA_V1,
                        assertion,
                    )
                    .unwrap(),
                }],
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
        deterministic_results,
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
    renew_receipts: AtomicUsize,
    execution_saga_digest_mode: AtomicUsize,
    workbench_receipt_fault: AtomicUsize,
    replacement_principal: Mutex<Option<String>>,
    harness_outcome: Mutex<QaHarnessOutcome>,
    replay_workbench_receipt: Mutex<Option<WorkbenchEvidenceReceiptV1>>,
    durable_workbench_outcomes: Mutex<BTreeMap<ContentDigest, WorkbenchEvidenceReceiptV1>>,
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
            renew_receipts: AtomicUsize::new(0),
            execution_saga_digest_mode: AtomicUsize::new(0),
            workbench_receipt_fault: AtomicUsize::new(0),
            replacement_principal: Mutex::new(None),
            harness_outcome: Mutex::new(QaHarnessOutcome::Pass),
            replay_workbench_receipt: Mutex::new(None),
            durable_workbench_outcomes: Mutex::new(BTreeMap::new()),
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

    fn execution_saga_readiness(&self) -> AdapterReadiness {
        AdapterReadiness::Ready {
            contract_version: DELIVERY_SCHEMA_V1,
            authority_generation: 1,
            contract_digest: match self
                .controls
                .execution_saga_digest_mode
                .load(Ordering::SeqCst)
            {
                0 => expected_workbench_execution_saga_contract_digest(),
                1 => expected_effect_saga_contract_digest(),
                _ => digest("stale-workbench-execution-saga"),
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
            issued_at_ms: 1 + if self.controls.renew_receipts.load(Ordering::SeqCst) == 0 {
                0
            } else {
                call as u64
            },
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
        if let Some(receipt) = self
            .controls
            .replay_workbench_receipt
            .lock()
            .unwrap()
            .clone()
        {
            return Ok(receipt);
        }
        if let Some(receipt) = self
            .controls
            .durable_workbench_outcomes
            .lock()
            .unwrap()
            .get(&request.request_digest)
            .cloned()
        {
            return Ok(receipt);
        }
        self.qa_calls.fetch_add(1, Ordering::SeqCst);
        let harness_outcome = *self.controls.harness_outcome.lock().unwrap();
        let mut receipt = WorkbenchEvidenceReceiptV1 {
            schema_version: 1,
            invocation: request.invocation.clone(),
            assignment: request.qa_run.clone(),
            qa_run: request.qa_run.clone(),
            assigned_qa: request.assigned_qa.clone(),
            authority_receipt_digest: request.authority_receipt_digest.clone(),
            authority_identity_digest: request.authority_identity_digest.clone(),
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
        match self.controls.workbench_receipt_fault.load(Ordering::SeqCst) {
            1 => receipt.schema_version = DELIVERY_SCHEMA_V1 + 1,
            2 => receipt.artifact_ownership_digest = ContentDigest::zero(),
            3 => receipt.cleanup_receipt.generation = 0,
            4 => receipt.cleanup_receipt.digest = ContentDigest::zero(),
            5 => receipt.authority_receipt_digest = ContentDigest::zero(),
            _ => {}
        }
        receipt.result_inventory_digest = qa_evidence_inventory_digest(&evidence_graph(
            &request.qa_run,
            &request.qa_plan.digest,
            &receipt,
        ))
        .unwrap();
        let receipt = receipt.seal()?;
        self.controls
            .durable_workbench_outcomes
            .lock()
            .unwrap()
            .insert(request.request_digest.clone(), receipt.clone());
        Ok(receipt)
    }
}

fn store(temp: &TempDir) -> DeliveryStore {
    DeliveryStore::open_test_only(&temp.path().join("delivery.redb")).unwrap()
}

#[derive(Clone, Copy)]
struct FakeEffects;

impl DeliveryEffectPort for FakeEffects {
    fn readiness(&self) -> AdapterReadiness {
        AdapterReadiness::Ready {
            contract_version: DELIVERY_SCHEMA_V1,
            authority_generation: 1,
            contract_digest: expected_effect_saga_contract_digest(),
        }
    }

    fn apply(
        &self,
        request: &DeliveryEffectRequestV1,
    ) -> Result<DeliveryEffectReceiptV1, DeliveryError> {
        DeliveryEffectReceiptV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            operation_id: request.operation_id.clone(),
            kind: request.kind,
            tenant_id: request.tenant_id.clone(),
            project: request.project.clone(),
            candidate: request.candidate.clone(),
            subject: request.subject.clone(),
            target: request.target.clone(),
            actor: request.actor.clone(),
            request_digest: request.request_digest.clone(),
            actor_authority_receipt_digest: request.actor_authority_receipt_digest.clone(),
            actor_authority_identity_digest: request.actor_authority_identity_digest.clone(),
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

struct ReadinessOnlyEffects {
    contract_digest: ContentDigest,
}

impl DeliveryEffectPort for ReadinessOnlyEffects {
    fn readiness(&self) -> AdapterReadiness {
        AdapterReadiness::Ready {
            contract_version: DELIVERY_SCHEMA_V1,
            authority_generation: 1,
            contract_digest: self.contract_digest.clone(),
        }
    }

    fn apply(
        &self,
        _request: &DeliveryEffectRequestV1,
    ) -> Result<DeliveryEffectReceiptV1, DeliveryError> {
        panic!("invalid readiness must fail before the effect port is called")
    }
}

#[derive(Clone, Copy)]
struct WrongTargetEffects;

impl DeliveryEffectPort for WrongTargetEffects {
    fn readiness(&self) -> AdapterReadiness {
        AdapterReadiness::Ready {
            contract_version: DELIVERY_SCHEMA_V1,
            authority_generation: 1,
            contract_digest: expected_effect_saga_contract_digest(),
        }
    }

    fn apply(
        &self,
        request: &DeliveryEffectRequestV1,
    ) -> Result<DeliveryEffectReceiptV1, DeliveryError> {
        DeliveryEffectReceiptV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            operation_id: request.operation_id.clone(),
            kind: request.kind,
            tenant_id: request.tenant_id.clone(),
            project: request.project.clone(),
            candidate: request.candidate.clone(),
            subject: request.subject.clone(),
            target: Some(reference("wrong-rollback-target", 1)),
            actor: request.actor.clone(),
            request_digest: request.request_digest.clone(),
            actor_authority_receipt_digest: request.actor_authority_receipt_digest.clone(),
            actor_authority_identity_digest: request.actor_authority_identity_digest.clone(),
            effect_ref: reference("wrong-target-effect", 1),
            issuer: "test-effects".to_string(),
            issued_at_ms: 1,
            receipt_digest: ContentDigest::zero(),
        }
        .seal()
    }
}

#[derive(Clone, Copy)]
struct WrongAuthorityIdentityEffects;

impl DeliveryEffectPort for WrongAuthorityIdentityEffects {
    fn readiness(&self) -> AdapterReadiness {
        AdapterReadiness::Ready {
            contract_version: DELIVERY_SCHEMA_V1,
            authority_generation: 1,
            contract_digest: expected_effect_saga_contract_digest(),
        }
    }

    fn apply(
        &self,
        request: &DeliveryEffectRequestV1,
    ) -> Result<DeliveryEffectReceiptV1, DeliveryError> {
        DeliveryEffectReceiptV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            operation_id: request.operation_id.clone(),
            kind: request.kind,
            tenant_id: request.tenant_id.clone(),
            project: request.project.clone(),
            candidate: request.candidate.clone(),
            subject: request.subject.clone(),
            target: request.target.clone(),
            actor: request.actor.clone(),
            request_digest: request.request_digest.clone(),
            actor_authority_receipt_digest: request.actor_authority_receipt_digest.clone(),
            actor_authority_identity_digest: digest("forged-authority-identity"),
            effect_ref: reference("wrong-authority-effect", 1),
            issuer: "test-effects".to_string(),
            issued_at_ms: 1,
            receipt_digest: ContentDigest::zero(),
        }
        .seal()
    }
}

#[derive(Clone, Default)]
struct DurableFakeEffects {
    calls: Arc<AtomicUsize>,
    outcomes: Arc<Mutex<BTreeMap<String, (ContentDigest, DeliveryEffectReceiptV1)>>>,
}

impl DeliveryEffectPort for DurableFakeEffects {
    fn readiness(&self) -> AdapterReadiness {
        AdapterReadiness::Ready {
            contract_version: DELIVERY_SCHEMA_V1,
            authority_generation: 1,
            contract_digest: expected_effect_saga_contract_digest(),
        }
    }

    fn apply(
        &self,
        request: &DeliveryEffectRequestV1,
    ) -> Result<DeliveryEffectReceiptV1, DeliveryError> {
        let mut outcomes = self.outcomes.lock().unwrap();
        if let Some((digest, receipt)) = outcomes.get(&request.operation_id) {
            if digest != &request.request_digest {
                return Err(DeliveryError::IdempotencyConflict {
                    key: request.operation_id.clone(),
                });
            }
            return Ok(receipt.clone());
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        let receipt = DeliveryEffectReceiptV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            operation_id: request.operation_id.clone(),
            kind: request.kind,
            tenant_id: request.tenant_id.clone(),
            project: request.project.clone(),
            candidate: request.candidate.clone(),
            subject: request.subject.clone(),
            target: request.target.clone(),
            actor: request.actor.clone(),
            request_digest: request.request_digest.clone(),
            actor_authority_receipt_digest: request.actor_authority_receipt_digest.clone(),
            actor_authority_identity_digest: request.actor_authority_identity_digest.clone(),
            effect_ref: reference("durable-effect-1", 1),
            issuer: "test-durable-effect-saga".to_string(),
            issued_at_ms: 1,
            receipt_digest: ContentDigest::zero(),
        }
        .seal()?;
        outcomes.insert(
            request.operation_id.clone(),
            (request.request_digest.clone(), receipt.clone()),
        );
        Ok(receipt)
    }
}

struct ConflictOnceStore {
    inner: DeliveryStore,
    command_kind: &'static str,
    conflicts: AtomicUsize,
}

impl DeliveryAggregateStorePort for ConflictOnceStore {
    fn load(
        &self,
        tenant_id: &str,
        project_id: &str,
    ) -> Result<Option<DeliveryAggregateV1>, DeliveryError> {
        self.inner.load(tenant_id, project_id)
    }

    fn lookup_idempotency(
        &self,
        tenant_id: &str,
        principal_id: &str,
        command_kind: &str,
        idempotency_key: &str,
        command_digest: &ContentDigest,
    ) -> Result<Option<DeliveryCommitReceiptV1>, DeliveryError> {
        self.inner.lookup_idempotency(
            tenant_id,
            principal_id,
            command_kind,
            idempotency_key,
            command_digest,
        )
    }

    fn commit(
        &self,
        request: &DeliveryCommitRequestV1,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        if request.command_kind == self.command_kind
            && self.conflicts.fetch_add(1, Ordering::SeqCst) == 0
        {
            return Err(DeliveryError::RevisionConflict {
                expected: request.expected_revision,
                actual: request.expected_revision + 1,
            });
        }
        self.inner.commit(request)
    }
}

impl DeliveryPublicationStatePort for ConflictOnceStore {
    fn pending_publications(&self) -> Result<Vec<DeliveryOutboxEntryV1>, DeliveryError> {
        self.inner.pending_publications()
    }

    fn mark_published(
        &self,
        expected_request_digest: &ContentDigest,
        receipt: PublicationReceiptV1,
    ) -> Result<(), DeliveryError> {
        self.inner.mark_published(expected_request_digest, receipt)
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
        evidence_graph(&run_ref, &plan.plan_digest, &workbench),
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
fn public_record_headers_reject_zero_generation_before_mutation() {
    let temp = TempDir::new().unwrap();
    let core = DeliveryCore::new_test_only(store(&temp), FakeIntegration::new().0, FakeEffects);
    let mut invalid = candidate(5, &["developer"]);
    invalid.generation = 0;
    invalid = invalid.seal().unwrap();
    assert!(matches!(
        core.register_candidate(
            &context("developer", AuthorityRole::Developer, "invalid-header", 100),
            invalid,
        ),
        Err(DeliveryError::Validation(_))
    ));
    assert!(core.load("tenant-a", "project-1").unwrap().is_none());
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
fn assignment_rejects_zero_plan_field_before_mutation() {
    let temp = TempDir::new().unwrap();
    let core = DeliveryCore::new_test_only(store(&temp), FakeIntegration::new().0, FakeEffects);
    let candidate = candidate(5, &["developer"]);
    core.register_candidate(
        &context("developer", AuthorityRole::Developer, "register-1", 100),
        candidate.clone(),
    )
    .unwrap();
    let mut invalid_plan = plan(&candidate);
    invalid_plan.runner_binary_digest = ContentDigest::zero();
    invalid_plan = invalid_plan.seal().unwrap();
    let planned_run = run(&invalid_plan, principal("qa-1", AuthorityRole::Qa));
    assert!(matches!(
        core.assign_qa(
            &context(
                "release-manager",
                AuthorityRole::ReleaseManager,
                "assign-zero-plan",
                110,
            ),
            "tenant-a",
            "project-1",
            &candidate.candidate_id,
            invalid_plan,
            planned_run,
        ),
        Err(DeliveryError::Validation(_))
    ));
    let aggregate = core.load("tenant-a", "project-1").unwrap().unwrap();
    assert!(aggregate.qa_plans.is_empty());
    assert!(aggregate.qa_runs.is_empty());
}

#[test]
fn assignment_accepts_explicit_no_retry_no_seed_plan() {
    let temp = TempDir::new().unwrap();
    let core = DeliveryCore::new_test_only(store(&temp), FakeIntegration::new().0, FakeEffects);
    let candidate = candidate(5, &["developer"]);
    core.register_candidate(
        &context("developer", AuthorityRole::Developer, "register-1", 100),
        candidate.clone(),
    )
    .unwrap();
    let mut deterministic_plan = plan(&candidate);
    deterministic_plan.declared_seeds.clear();
    deterministic_plan.retry_limit = 0;
    deterministic_plan.retryable_classes.clear();
    deterministic_plan = deterministic_plan.seal().unwrap();
    let planned_run = run(&deterministic_plan, principal("qa-1", AuthorityRole::Qa));
    core.assign_qa(
        &context(
            "release-manager",
            AuthorityRole::ReleaseManager,
            "assign-no-retry",
            110,
        ),
        "tenant-a",
        "project-1",
        &candidate.candidate_id,
        deterministic_plan,
        planned_run,
    )
    .unwrap();
    let aggregate = core.load("tenant-a", "project-1").unwrap().unwrap();
    assert_eq!(aggregate.qa_plans["plan-1"].retry_limit, 0);
    assert!(aggregate.qa_plans["plan-1"].retryable_classes.is_empty());
    assert!(aggregate.qa_plans["plan-1"].declared_seeds.is_empty());
}

#[test]
fn assignment_rejects_inconsistent_retry_contracts() {
    let temp = TempDir::new().unwrap();
    let core = DeliveryCore::new_test_only(store(&temp), FakeIntegration::new().0, FakeEffects);
    let candidate = candidate(5, &["developer"]);
    core.register_candidate(
        &context("developer", AuthorityRole::Developer, "register-1", 100),
        candidate.clone(),
    )
    .unwrap();

    let mut zero_with_classes = plan(&candidate);
    zero_with_classes.retry_limit = 0;
    zero_with_classes = zero_with_classes.seal().unwrap();
    let zero_run = run(&zero_with_classes, principal("qa-1", AuthorityRole::Qa));
    assert!(matches!(
        core.assign_qa(
            &context(
                "release-manager",
                AuthorityRole::ReleaseManager,
                "assign-zero-with-classes",
                110,
            ),
            "tenant-a",
            "project-1",
            &candidate.candidate_id,
            zero_with_classes,
            zero_run,
        ),
        Err(DeliveryError::Validation(_))
    ));

    let mut positive_without_classes = plan(&candidate);
    positive_without_classes.retryable_classes.clear();
    positive_without_classes = positive_without_classes.seal().unwrap();
    let positive_run = run(
        &positive_without_classes,
        principal("qa-1", AuthorityRole::Qa),
    );
    assert!(matches!(
        core.assign_qa(
            &context(
                "release-manager",
                AuthorityRole::ReleaseManager,
                "assign-positive-without-classes",
                111,
            ),
            "tenant-a",
            "project-1",
            &candidate.candidate_id,
            positive_without_classes,
            positive_run,
        ),
        Err(DeliveryError::Validation(_))
    ));
    let aggregate = core.load("tenant-a", "project-1").unwrap().unwrap();
    assert!(aggregate.qa_plans.is_empty());
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
fn workbench_effect_reconciles_after_authority_toctou_without_reexecution() {
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

    controls.flip_authority_on_call.store(0, Ordering::SeqCst);
    controls.renew_receipts.store(1, Ordering::SeqCst);
    core.execute_qa(
        &context("qa-1", AuthorityRole::Qa, "execute-reconcile", 141),
        "tenant-a",
        "project-1",
        "run-1",
    )
    .unwrap();
    assert_eq!(qa_calls.load(Ordering::SeqCst), 1);
    let recovered = core.load("tenant-a", "project-1").unwrap().unwrap();
    assert_eq!(recovered.workbench_receipts.len(), 1);
    assert_eq!(recovered.qa_runs["run-1"].attempts, 1);
}

#[test]
fn qa_effect_uses_stable_request_and_is_not_repeated_on_retry() {
    let temp = TempDir::new().unwrap();
    let (integration, controls) = FakeIntegration::controlled();
    let qa_calls = Arc::clone(&integration.qa_calls);
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
    controls.renew_receipts.store(1, Ordering::SeqCst);
    let duplicate = core
        .execute_qa(&effect_context, "tenant-a", "project-1", "run-1")
        .unwrap();
    assert!(!first.0.duplicate);
    assert!(duplicate.0.duplicate);
    assert_eq!(first.1.receipt_digest, duplicate.1.receipt_digest);
    assert_eq!(qa_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn workbench_saga_rejects_effect_and_stale_contract_digests() {
    assert_ne!(
        expected_workbench_execution_saga_contract_digest(),
        expected_effect_saga_contract_digest()
    );
    let temp = TempDir::new().unwrap();
    let (integration, controls) = FakeIntegration::controlled();
    let qa_calls = Arc::clone(&integration.qa_calls);
    let (core, _, _, _) = running_qa_core(&temp, integration);

    controls
        .execution_saga_digest_mode
        .store(1, Ordering::SeqCst);
    assert!(matches!(
        core.execute_qa(
            &context("qa-1", AuthorityRole::Qa, "execute-swapped-saga", 140),
            "tenant-a",
            "project-1",
            "run-1",
        ),
        Err(DeliveryError::StaleEvidence(_))
    ));
    controls
        .execution_saga_digest_mode
        .store(2, Ordering::SeqCst);
    assert!(matches!(
        core.execute_qa(
            &context("qa-1", AuthorityRole::Qa, "execute-stale-saga", 141),
            "tenant-a",
            "project-1",
            "run-1",
        ),
        Err(DeliveryError::StaleEvidence(_))
    ));
    assert_eq!(qa_calls.load(Ordering::SeqCst), 0);

    controls
        .execution_saga_digest_mode
        .store(0, Ordering::SeqCst);
    core.execute_qa(
        &context("qa-1", AuthorityRole::Qa, "execute-exact-saga", 142),
        "tenant-a",
        "project-1",
        "run-1",
    )
    .unwrap();
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
fn workbench_receipt_cannot_be_replayed_across_authority_lineages() {
    let temp = TempDir::new().unwrap();
    let (integration, controls) = FakeIntegration::controlled();
    let qa_calls = Arc::clone(&integration.qa_calls);
    let (core, _, _, _) = running_qa_core(&temp, integration);
    let (_, old_receipt) = core
        .execute_qa(
            &context("qa-1", AuthorityRole::Qa, "execute-old-lineage", 140),
            "tenant-a",
            "project-1",
            "run-1",
        )
        .unwrap();
    assert_eq!(qa_calls.load(Ordering::SeqCst), 1);
    *controls.replay_workbench_receipt.lock().unwrap() = Some(old_receipt);
    controls.contract_generation.store(8, Ordering::SeqCst);

    assert!(matches!(
        core.execute_qa(
            &context("qa-1", AuthorityRole::Qa, "execute-new-lineage", 150),
            "tenant-a",
            "project-1",
            "run-1",
        ),
        Err(DeliveryError::StaleEvidence(_))
    ));
    assert_eq!(qa_calls.load(Ordering::SeqCst), 1);
    let aggregate = core.load("tenant-a", "project-1").unwrap().unwrap();
    assert_eq!(aggregate.workbench_receipts.len(), 1);
    assert_eq!(aggregate.qa_runs["run-1"].attempts, 1);
}

#[test]
fn workbench_receipt_rejects_incomplete_schema_ownership_and_cleanup_bindings() {
    for fault in 1..=5 {
        let temp = TempDir::new().unwrap();
        let (integration, controls) = FakeIntegration::controlled();
        controls
            .workbench_receipt_fault
            .store(fault, Ordering::SeqCst);
        let (core, _, _, _) = running_qa_core(&temp, integration);
        let result = core.execute_qa(
            &context(
                "qa-1",
                AuthorityRole::Qa,
                &format!("execute-incomplete-receipt-{fault}"),
                140,
            ),
            "tenant-a",
            "project-1",
            "run-1",
        );
        assert!(matches!(
            result,
            Err(DeliveryError::StaleEvidence(_) | DeliveryError::Validation(_))
        ));
        let aggregate = core.load("tenant-a", "project-1").unwrap().unwrap();
        assert!(aggregate.workbench_receipts.is_empty());
        assert_eq!(aggregate.qa_runs["run-1"].attempts, 0);
    }
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
fn unresolved_or_malformed_findings_cannot_authorize_gate_or_promotion() {
    let temp = TempDir::new().unwrap();
    let (core, candidate, plan, qa) = completed_qa_core(&temp);
    let aggregate = core.load("tenant-a", "project-1").unwrap().unwrap();
    let workbench = aggregate
        .workbench_receipts
        .values()
        .next()
        .unwrap()
        .clone();
    let graph = aggregate.evidence_graphs["run-1"].clone();
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
    let base_finding = FindingV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        finding_id: "finding-unresolved".to_string(),
        generation: 1,
        candidate: candidate_ref.clone(),
        severity: FindingSeverity::Critical,
        classification: FindingClassification::Security,
        evidence: vec![SourceTupleV1 {
            owner: "qa-review".to_string(),
            source_type: "finding".to_string(),
            id: "finding-evidence".to_string(),
            generation: 1,
            digest: digest("finding-evidence"),
        }],
        resolved_by: None,
    };
    let make_bundle = |findings: &[FindingV1], approved: bool, suffix: &str| {
        (
            ReviewV1 {
                schema_version: DELIVERY_SCHEMA_V1,
                review_id: format!("review-{suffix}"),
                generation: 1,
                candidate: candidate_ref.clone(),
                reviewer: qa.clone(),
                findings_digest: ContentDigest::of_domain(
                    "qa-findings",
                    DELIVERY_SCHEMA_V1,
                    &findings,
                )
                .unwrap(),
                approved,
                created_at_ms: 160,
            },
            TestRunV1 {
                schema_version: DELIVERY_SCHEMA_V1,
                test_run_id: format!("test-run-{suffix}"),
                generation: 1,
                candidate: candidate_ref.clone(),
                qa_plan: plan_ref.clone(),
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
        )
    };

    let mut malformed = base_finding.clone();
    malformed.evidence[0].digest = ContentDigest::zero();
    let (review, test_run) = make_bundle(&[malformed.clone()], false, "malformed");
    assert!(matches!(
        core.record_review_bundle(
            &context("qa-1", AuthorityRole::Qa, "finding-malformed", 160),
            "tenant-a",
            "project-1",
            "run-1",
            review,
            test_run,
            vec![malformed],
            None,
        ),
        Err(DeliveryError::Validation(_))
    ));

    let mut duplicate = base_finding.clone();
    duplicate.evidence.push(duplicate.evidence[0].clone());
    let (review, test_run) = make_bundle(&[duplicate.clone()], false, "duplicate");
    assert!(matches!(
        core.record_review_bundle(
            &context("qa-1", AuthorityRole::Qa, "finding-duplicate", 160),
            "tenant-a",
            "project-1",
            "run-1",
            review,
            test_run,
            vec![duplicate],
            None,
        ),
        Err(DeliveryError::Conflict(_))
    ));

    let mut locator_conflict = base_finding.clone();
    let mut conflicting_source = locator_conflict.evidence[0].clone();
    conflicting_source.digest = digest("conflicting-finding-evidence");
    locator_conflict.evidence.push(conflicting_source);
    let (review, test_run) = make_bundle(&[locator_conflict.clone()], false, "conflict");
    assert!(matches!(
        core.record_review_bundle(
            &context("qa-1", AuthorityRole::Qa, "finding-conflict", 160),
            "tenant-a",
            "project-1",
            "run-1",
            review,
            test_run,
            vec![locator_conflict],
            None,
        ),
        Err(DeliveryError::Conflict(_))
    ));

    let mut bad_resolution = base_finding.clone();
    bad_resolution.resolved_by = Some(VersionedRefV1 {
        id: "finding-resolution".to_string(),
        generation: 1,
        digest: ContentDigest::zero(),
    });
    let (review, test_run) = make_bundle(&[bad_resolution.clone()], false, "bad-resolution");
    assert!(matches!(
        core.record_review_bundle(
            &context("qa-1", AuthorityRole::Qa, "finding-bad-resolution", 160),
            "tenant-a",
            "project-1",
            "run-1",
            review,
            test_run,
            vec![bad_resolution],
            None,
        ),
        Err(DeliveryError::Validation(_))
    ));

    let unresolved = vec![base_finding];
    let (review, test_run) = make_bundle(&unresolved, true, "forged-approval");
    assert!(matches!(
        core.record_review_bundle(
            &context("qa-1", AuthorityRole::Qa, "finding-forged-approval", 160),
            "tenant-a",
            "project-1",
            "run-1",
            review,
            test_run,
            unresolved.clone(),
            Some(ApprovalV1 {
                schema_version: DELIVERY_SCHEMA_V1,
                approval_id: "approval-forged".to_string(),
                generation: 1,
                candidate: candidate_ref.clone(),
                gate: reference("gate-unresolved", 1),
                approver: qa.clone(),
                policy_digest: plan.release_policy_digest.clone(),
                approved_at_ms: 160,
            }),
        ),
        Err(DeliveryError::StaleEvidence(_) | DeliveryError::AuthorityDenied(_))
    ));

    let (review, test_run) = make_bundle(&unresolved, false, "unresolved");
    core.record_review_bundle(
        &context("qa-1", AuthorityRole::Qa, "finding-unresolved", 160),
        "tenant-a",
        "project-1",
        "run-1",
        review,
        test_run,
        unresolved,
        None,
    )
    .unwrap();

    let gate = QaReleaseGateReceiptV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        gate_id: "gate-unresolved".to_string(),
        generation: 1,
        candidate: candidate_ref,
        plan: plan_ref,
        case_inventory_digest: qa_case_inventory_digest(&graph).unwrap(),
        deterministic_evidence_digest: qa_deterministic_evidence_digest(&graph).unwrap(),
        model_evidence_digest: None,
        calibration_digest: None,
        source_evidence_digest: qa_source_evidence_digest(&graph).unwrap(),
        flake_disposition_digest: qa_flake_disposition_digest(&graph).unwrap(),
        policy_digest: plan.release_policy_digest,
        release_manifest_digest: digest("unresolved-manifest-input"),
        actor: qa,
        passed: true,
        issued_at_ms: 170,
        expires_at_ms: 1_000,
    };
    assert!(matches!(
        core.record_gate(
            &context("qa-1", AuthorityRole::Qa, "gate-unresolved", 170),
            "tenant-a",
            "project-1",
            "run-1",
            gate,
        ),
        Err(DeliveryError::MissingEvidence(_))
    ));

    let manifest = fixture_manifest("manifest-unresolved", 1);
    let release = ReleaseV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        release_id: "release-unresolved".to_string(),
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
    assert!(matches!(
        core.promote(
            &context(
                "release-manager",
                AuthorityRole::ReleaseManager,
                "promote-unresolved",
                180,
            ),
            "tenant-a",
            "project-1",
            &candidate.candidate_id,
            manifest,
            release,
        ),
        Err(DeliveryError::StaleEvidence(_) | DeliveryError::MissingEvidence(_))
    ));
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
    let graph = evidence_graph(&run_ref, &plan.plan_digest, &receipt);
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
        model_evidence_digest: None,
        calibration_digest: None,
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
        calibration_digest: None,
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

    let mut fake_model_binding = gate.clone();
    fake_model_binding.model_evidence_digest = Some(digest("uncalibrated-model-inventory"));
    assert!(matches!(
        core.record_gate(
            &context("qa-1", AuthorityRole::Qa, "fake-model-binding", 170),
            "tenant-a",
            "project-1",
            "run-1",
            fake_model_binding,
        ),
        Err(DeliveryError::MissingEvidence(_))
    ));

    let mut fake_calibration = gate.clone();
    fake_calibration.calibration_digest = Some(digest("aggregation-is-not-calibration"));
    assert!(matches!(
        core.record_gate(
            &context("qa-1", AuthorityRole::Qa, "fake-calibration", 170),
            "tenant-a",
            "project-1",
            "run-1",
            fake_calibration,
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
    let graph = evidence_graph(&run_ref, &plan.plan_digest, &workbench);
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
        model_evidence_digest: None,
        calibration_digest: None,
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
    let mut fake_model_gate = gate.clone();
    fake_model_gate.model_evidence_digest = Some(digest("uncalibrated-model-inventory"));
    assert!(matches!(
        core.record_gate(
            &context("qa-1", AuthorityRole::Qa, "gate-model-rejected", 170),
            "tenant-a",
            "project-1",
            "run-1",
            fake_model_gate,
        ),
        Err(DeliveryError::MissingEvidence(message))
            if message == "gate outcome or evidence graph does not match the exact terminal run"
    ));
    let mut fake_calibration_gate = gate.clone();
    fake_calibration_gate.calibration_digest = Some(plan.aggregation_policy_digest.clone());
    assert!(matches!(
        core.record_gate(
            &context(
                "qa-1",
                AuthorityRole::Qa,
                "gate-calibration-rejected",
                170,
            ),
            "tenant-a",
            "project-1",
            "run-1",
            fake_calibration_gate,
        ),
        Err(DeliveryError::MissingEvidence(message))
            if message == "gate outcome or evidence graph does not match the exact terminal run"
    ));
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
    let mut stale_acceptance = acceptance.clone();
    stale_acceptance.release.generation += 1;
    stale_acceptance = stale_acceptance.seal().unwrap();
    assert!(matches!(
        core.customer_action(
            &context("customer-1", AuthorityRole::Customer, "accept", 200),
            "tenant-a",
            "project-1",
            feedback.clone(),
            Some(stale_acceptance),
        ),
        Err(DeliveryError::AuthorityDenied(_))
    ));
    let mut forged_rework = feedback.clone();
    forged_rework.requested_work_item_refs = vec![reference("caller-rework", 1)];
    forged_rework = forged_rework.seal().unwrap();
    assert!(matches!(
        core.customer_action(
            &context("customer-1", AuthorityRole::Customer, "accept", 200),
            "tenant-a",
            "project-1",
            forged_rework,
            Some(acceptance.clone()),
        ),
        Err(DeliveryError::AuthorityDenied(_))
    ));
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
        calibration_digest: None,
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
fn rollback_local_adoption_is_idempotent_after_successful_commit() {
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

    for (name, contract_digest) in [
        (
            "swapped",
            expected_workbench_execution_saga_contract_digest(),
        ),
        ("stale", digest("stale-delivery-effect-saga")),
    ] {
        let readiness_temp = TempDir::new().unwrap();
        let readiness_core = DeliveryCore::new_test_only(
            seeded_store(&readiness_temp, aggregate.clone()),
            FakeIntegration::new().0,
            ReadinessOnlyEffects { contract_digest },
        );
        assert!(matches!(
            readiness_core.rollback(
                &context(
                    "release-manager",
                    AuthorityRole::ReleaseManager,
                    &format!("rollback-{name}"),
                    300,
                ),
                "tenant-a",
                "project-1",
                rollback.clone(),
            ),
            Err(DeliveryError::StaleEvidence(_))
        ));
        assert_eq!(
            readiness_core
                .load("tenant-a", "project-1")
                .unwrap()
                .unwrap()
                .revision,
            1
        );
    }

    let wrong_target_temp = TempDir::new().unwrap();
    let wrong_target_core = DeliveryCore::new_test_only(
        seeded_store(&wrong_target_temp, aggregate.clone()),
        FakeIntegration::new().0,
        WrongTargetEffects,
    );
    assert!(matches!(
        wrong_target_core.rollback(&command, "tenant-a", "project-1", rollback.clone()),
        Err(DeliveryError::StaleEvidence(_))
    ));
    let wrong_target_unchanged = wrong_target_core
        .load("tenant-a", "project-1")
        .unwrap()
        .unwrap();
    assert_eq!(
        wrong_target_unchanged.active_release_id.as_deref(),
        Some("release-failed")
    );
    assert!(wrong_target_unchanged.rollbacks.is_empty());

    let wrong_authority_temp = TempDir::new().unwrap();
    let wrong_authority_core = DeliveryCore::new_test_only(
        seeded_store(&wrong_authority_temp, aggregate.clone()),
        FakeIntegration::new().0,
        WrongAuthorityIdentityEffects,
    );
    assert!(matches!(
        wrong_authority_core.rollback(&command, "tenant-a", "project-1", rollback.clone()),
        Err(DeliveryError::StaleEvidence(_))
    ));
    let wrong_authority_unchanged = wrong_authority_core
        .load("tenant-a", "project-1")
        .unwrap()
        .unwrap();
    assert_eq!(
        wrong_authority_unchanged.active_release_id.as_deref(),
        Some("release-failed")
    );
    assert!(wrong_authority_unchanged.rollbacks.is_empty());

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
fn rollback_reconciles_durable_effect_after_local_revision_conflict_without_reexecution() {
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
        "rollback-conflict",
        300,
    );
    let rollback = RollbackV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        rollback_id: "rollback-conflict".to_string(),
        generation: 1,
        from_release: versioned_release(&failed),
        to_release: versioned_release(&previous),
        actor: command.principal.clone(),
        reason_digest: digest("rollback-reason"),
        effect_receipt: None,
        created_at_ms: 300,
    };
    let effects = DurableFakeEffects::default();
    let calls = Arc::clone(&effects.calls);
    let store = ConflictOnceStore {
        inner: seeded_store(&temp, aggregate),
        command_kind: "rollback",
        conflicts: AtomicUsize::new(0),
    };
    let (integration, controls) = FakeIntegration::controlled();
    controls.renew_receipts.store(1, Ordering::SeqCst);
    let core = DeliveryCore::new_test_only(store, integration, effects);

    assert!(matches!(
        core.rollback(&command, "tenant-a", "project-1", rollback.clone()),
        Err(DeliveryError::RevisionConflict { .. })
    ));
    let unchanged = core.load("tenant-a", "project-1").unwrap().unwrap();
    assert_eq!(
        unchanged.active_release_id.as_deref(),
        Some("release-failed")
    );
    assert!(unchanged.rollbacks.is_empty());

    core.rollback(&command, "tenant-a", "project-1", rollback)
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let recovered = core.load("tenant-a", "project-1").unwrap().unwrap();
    assert_eq!(
        recovered.active_release_id.as_deref(),
        Some("release-previous")
    );
    assert_eq!(recovered.rollbacks.len(), 1);
}

#[test]
fn closeout_requires_bound_memory_receipt_and_survives_restart() {
    let temp = TempDir::new().unwrap();
    let customer = principal("customer-1", AuthorityRole::Customer);
    let release = ReleaseV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        release_id: "release-1".to_string(),
        generation: 1,
        manifest: reference("manifest-1", 1),
        state: ReleaseState::Active,
        activated_at_ms: Some(100),
        rollout_receipt: Some(reference("rollout-1", 1)),
    };
    let release_ref = versioned_release(&release);
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
    aggregate.releases.insert("release-1".to_string(), release);
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
    let mut stale_release_closeout = closeout.clone();
    stale_release_closeout.accepted_release.generation += 1;
    assert!(matches!(
        core.closeout(&command, "tenant-a", "project-1", stale_release_closeout),
        Err(DeliveryError::StaleEvidence(_))
    ));
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
    for (schema_version, event_id) in [
        (DELIVERY_SCHEMA_V1 + 1, "event-valid".to_string()),
        (DELIVERY_SCHEMA_V1, String::new()),
    ] {
        let invalid = PublicationReceiptV1 {
            schema_version,
            operation_id: request.operation_id.clone(),
            event_id,
            aggregate_id: request.aggregate_id.clone(),
            row_identity: request.row_identity.clone(),
            payload_digest: request.payload_digest.clone(),
            request_digest: request.request_digest.clone(),
        };
        assert!(matches!(
            core.store()
                .mark_published(&request.request_digest, invalid),
            Err(DeliveryError::Conflict(_))
        ));
    }
    assert_eq!(core.store().pending_publications().unwrap().len(), 1);
}

#[test]
fn health_rejects_record_map_key_identity_mismatch() {
    let temp = TempDir::new().unwrap();
    let mut aggregate = DeliveryAggregateV1::new("tenant-a", "project-1");
    aggregate
        .candidates
        .insert("wrong-map-key".to_string(), candidate(5, &["developer"]));
    let store = seeded_store(&temp, aggregate);
    assert!(matches!(
        store.health(),
        Err(DeliveryError::CorruptStore(_))
    ));
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

#[test]
fn renewed_authority_receipt_does_not_change_external_operation_identity() {
    let qa = principal("qa-1", AuthorityRole::Qa);
    let authority = AuthorityReceiptV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        request_digest: digest("authority-request"),
        principal: qa.clone(),
        contract_version: DELIVERY_SCHEMA_V1,
        contract_authority_generation: 7,
        contract_digest: expected_integration_contract_digest(),
        issued_at_ms: 10,
        expires_at_ms: 100,
        issuer: "test-authority".to_string(),
        receipt_digest: ContentDigest::zero(),
    }
    .seal()
    .unwrap();
    let authority_identity = authority.stable_identity_digest().unwrap();
    let mut workbench = WorkbenchEvidenceRequestV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        tenant_id: "tenant-a".to_string(),
        project: reference("project-1", 4),
        candidate: reference("candidate-1", 1),
        qa_plan: reference("plan-1", 1),
        qa_run: reference("run-1", 1),
        assigned_qa: qa.clone(),
        authority_receipt_digest: authority.receipt_digest.clone(),
        authority_identity_digest: authority_identity.clone(),
        invocation: reference("qa-invocation-1", 1),
        request_digest: ContentDigest::zero(),
    }
    .seal()
    .unwrap();
    let old_workbench_digest = workbench.request_digest.clone();
    let mut renewed = authority.clone();
    renewed.issued_at_ms = 20;
    renewed.expires_at_ms = 200;
    renewed.receipt_digest = ContentDigest::zero();
    renewed = renewed.seal().unwrap();
    assert_eq!(
        authority_identity,
        renewed.stable_identity_digest().unwrap()
    );
    workbench.authority_receipt_digest = renewed.receipt_digest.clone();
    assert_eq!(
        old_workbench_digest,
        workbench.computed_digest().unwrap(),
        "renewing the same authority must retain the workbench idempotency key"
    );

    let mut effect = DeliveryEffectRequestV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        operation_id: "rollback:tenant-a:project-1:rollback-1".to_string(),
        kind: DeliveryEffectKind::Rollback,
        tenant_id: "tenant-a".to_string(),
        project: reference("project-1", 4),
        candidate: Some(reference("candidate-1", 1)),
        subject: reference("release-from", 2),
        target: Some(reference("release-to", 1)),
        actor: qa,
        actor_authority_receipt_digest: authority.receipt_digest.clone(),
        actor_authority_identity_digest: authority_identity.clone(),
        request_digest: ContentDigest::zero(),
    }
    .seal()
    .unwrap();
    let old_effect_digest = effect.request_digest.clone();
    effect.actor_authority_receipt_digest = renewed.receipt_digest;
    assert_eq!(
        old_effect_digest,
        effect.computed_digest().unwrap(),
        "renewing the same authority must retain the effect operation digest"
    );

    let mut changed_authorities = Vec::new();
    let mut changed = authority.clone();
    changed.contract_authority_generation += 1;
    changed_authorities.push(changed);
    let mut changed = authority.clone();
    changed.contract_digest = digest("replacement-contract");
    changed_authorities.push(changed);
    let mut changed = authority.clone();
    changed.issuer = "replacement-authority".to_string();
    changed_authorities.push(changed);
    let mut changed = authority;
    changed.principal = principal("replacement-qa", AuthorityRole::Qa);
    changed_authorities.push(changed);
    for changed in changed_authorities {
        let changed_identity = changed.stable_identity_digest().unwrap();
        assert_ne!(authority_identity, changed_identity);
        workbench.authority_identity_digest = changed_identity.clone();
        assert_ne!(old_workbench_digest, workbench.computed_digest().unwrap());
        effect.actor_authority_identity_digest = changed_identity;
        assert_ne!(old_effect_digest, effect.computed_digest().unwrap());
    }
}

#[test]
fn effect_saga_rejects_outcome_from_another_authority_lineage() {
    let actor = principal("release-manager", AuthorityRole::ReleaseManager);
    let old_authority = AuthorityReceiptV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        request_digest: digest("effect-authority-request"),
        principal: actor.clone(),
        contract_version: DELIVERY_SCHEMA_V1,
        contract_authority_generation: 7,
        contract_digest: expected_integration_contract_digest(),
        issued_at_ms: 10,
        expires_at_ms: 100,
        issuer: "test-authority".to_string(),
        receipt_digest: ContentDigest::zero(),
    }
    .seal()
    .unwrap();
    let old_request = DeliveryEffectRequestV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        operation_id: "rollback:tenant-a:project-1:authority-lineage".to_string(),
        kind: DeliveryEffectKind::Rollback,
        tenant_id: "tenant-a".to_string(),
        project: reference("project-1", 4),
        candidate: Some(reference("candidate-1", 1)),
        subject: reference("release-from", 2),
        target: Some(reference("release-to", 1)),
        actor,
        actor_authority_receipt_digest: old_authority.receipt_digest.clone(),
        actor_authority_identity_digest: old_authority.stable_identity_digest().unwrap(),
        request_digest: ContentDigest::zero(),
    }
    .seal()
    .unwrap();

    let mut changed_authorities = Vec::new();
    let mut changed = old_authority.clone();
    changed.principal.authority_generation += 1;
    changed.receipt_digest = ContentDigest::zero();
    changed_authorities.push(changed.seal().unwrap());
    let mut changed = old_authority.clone();
    changed.principal.principal_id = "replacement-release-manager".to_string();
    changed.receipt_digest = ContentDigest::zero();
    changed_authorities.push(changed.seal().unwrap());
    let mut changed = old_authority.clone();
    changed.principal.roles.insert(AuthorityRole::Auditor);
    changed.receipt_digest = ContentDigest::zero();
    changed_authorities.push(changed.seal().unwrap());
    let mut changed = old_authority.clone();
    changed.contract_version += 1;
    changed.receipt_digest = ContentDigest::zero();
    changed_authorities.push(changed.seal().unwrap());
    let mut changed = old_authority.clone();
    changed.contract_authority_generation += 1;
    changed.receipt_digest = ContentDigest::zero();
    changed_authorities.push(changed.seal().unwrap());
    let mut changed = old_authority.clone();
    changed.contract_digest = digest("replacement-contract");
    changed.receipt_digest = ContentDigest::zero();
    changed_authorities.push(changed.seal().unwrap());
    let mut changed = old_authority.clone();
    changed.issuer = "replacement-authority".to_string();
    changed.receipt_digest = ContentDigest::zero();
    changed_authorities.push(changed.seal().unwrap());

    for changed_authority in changed_authorities {
        let effects = DurableFakeEffects::default();
        effects.apply(&old_request).unwrap();
        assert_eq!(effects.calls.load(Ordering::SeqCst), 1);

        let mut changed_request = old_request.clone();
        changed_request.actor = changed_authority.principal.clone();
        changed_request.actor_authority_receipt_digest =
            changed_authority.receipt_digest.clone();
        changed_request.actor_authority_identity_digest =
            changed_authority.stable_identity_digest().unwrap();
        changed_request.request_digest = ContentDigest::zero();
        changed_request = changed_request.seal().unwrap();
        assert_ne!(old_request.request_digest, changed_request.request_digest);
        assert!(matches!(
            effects.apply(&changed_request),
            Err(DeliveryError::IdempotencyConflict { .. })
        ));
        assert_eq!(effects.calls.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn evidence_graph_rejects_all_model_authority_until_issue_749() {
    let temp = TempDir::new().unwrap();
    let (core, _, plan, _) = running_qa_core(&temp, FakeIntegration::new().0);
    let (_, receipt) = core
        .execute_qa(
            &context("qa-1", AuthorityRole::Qa, "execute-evidence-negative", 140),
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
    let valid = evidence_graph(&run_ref, &plan.plan_digest, &receipt);
    validate_qa_evidence_graph(
        &plan,
        &run_ref,
        &valid,
        &principal("qa-1", AuthorityRole::Qa),
        150,
    )
    .unwrap();

    let mut empty = valid.clone();
    empty.case_results[0].assertion_refs.clear();
    empty = empty.seal().unwrap();
    assert!(matches!(
        validate_qa_evidence_graph(
            &plan,
            &run_ref,
            &empty,
            &principal("qa-1", AuthorityRole::Qa),
            150
        ),
        Err(DeliveryError::MissingEvidence(_))
    ));

    let mut optional_without_assertion = valid.clone();
    let optional_case = optional_without_assertion
        .dataset_cases
        .iter()
        .find(|case| !case.required)
        .unwrap()
        .clone();
    let mut optional_result = optional_without_assertion.case_results[0].clone();
    optional_result.result_id = "result-visual".to_string();
    optional_result.case_ref = VersionedRefV1 {
        id: optional_case.case_id.clone(),
        generation: optional_case.generation,
        digest: ContentDigest::of_domain("qa-dataset-case", DELIVERY_SCHEMA_V1, &optional_case)
            .unwrap(),
    };
    optional_result.required = false;
    optional_result.slices = optional_case.slices;
    optional_result.assertion_refs.clear();
    optional_without_assertion
        .case_results
        .push(optional_result);
    optional_without_assertion = optional_without_assertion.seal().unwrap();
    assert!(matches!(
        validate_qa_evidence_graph(
            &plan,
            &run_ref,
            &optional_without_assertion,
            &principal("qa-1", AuthorityRole::Qa),
            150,
        ),
        Err(DeliveryError::MissingEvidence(_))
    ));

    let mut failed = valid.clone();
    failed.deterministic_results[0].passed = false;
    failed.case_results[0].assertion_refs[0].digest = ContentDigest::of_domain(
        "qa-deterministic-result",
        DELIVERY_SCHEMA_V1,
        &failed.deterministic_results[0],
    )
    .unwrap();
    failed = failed.seal().unwrap();
    assert!(matches!(
        validate_qa_evidence_graph(
            &plan,
            &run_ref,
            &failed,
            &principal("qa-1", AuthorityRole::Qa),
            150
        ),
        Err(DeliveryError::MissingEvidence(_))
    ));

    let mut duplicate = valid.clone();
    duplicate
        .dataset_cases
        .push(duplicate.dataset_cases[0].clone());
    duplicate = duplicate.seal().unwrap();
    assert!(matches!(
        validate_qa_evidence_graph(
            &plan,
            &run_ref,
            &duplicate,
            &principal("qa-1", AuthorityRole::Qa),
            150
        ),
        Err(DeliveryError::Conflict(_))
    ));

    let verified_model = QaModelGradeEvidenceV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        evidence_id: "model-evidence-1".to_string(),
        generation: 1,
        provider_endpoint_class: "mock".to_string(),
        api_version: "v1".to_string(),
        requested_model_id: "mock-model".to_string(),
        reported_model_id: Some("mock-model".to_string()),
        model_fingerprint: Some("test-fingerprint".to_string()),
        model_identity_status: QaModelIdentityStatus::Verified,
        model_family: "mock".to_string(),
        model_version: "1".to_string(),
        system_digest: digest("system"),
        rubric_digest: digest("rubric"),
        prompt_digest: digest("prompt"),
        response_schema_digest: digest("response-schema"),
        sampling_parameters: BTreeMap::new(),
        seed_supported: true,
        request_id: "model-request-1".to_string(),
        response_id: Some("model-response-1".to_string()),
        raw_output_digest: digest("model-output"),
        parse_outcome: QaModelParseOutcome::Valid,
        verdict: QaModelVerdict::Pass,
        attempts: 1,
        input_tokens: 1,
        output_tokens: 1,
        cost: None,
    };
    let model_ref = |model: &QaModelGradeEvidenceV1| VersionedRefV1 {
        id: model.evidence_id.clone(),
        generation: model.generation,
        digest: ContentDigest::of_domain("qa-model-evidence", DELIVERY_SCHEMA_V1, model).unwrap(),
    };

    let mut model_only = valid.clone();
    model_only.case_results[0].assertion_refs.clear();
    model_only.deterministic_results.remove(0);
    model_only.case_results[0]
        .grader_refs
        .push(model_ref(&verified_model));
    model_only.model_results.push(verified_model.clone());
    model_only = model_only.seal().unwrap();
    assert!(matches!(
        validate_qa_evidence_graph(
            &plan,
            &run_ref,
            &model_only,
            &principal("qa-1", AuthorityRole::Qa),
            150
        ),
        Err(DeliveryError::AdapterUnavailable {
            dependency: "qa_model_evidence_#749",
            ..
        })
    ));

    let mut deterministic_with_model = valid.clone();
    let mut uncalibrated_model = verified_model.clone();
    uncalibrated_model.verdict = QaModelVerdict::Fail;
    deterministic_with_model.case_results[0]
        .grader_refs
        .push(model_ref(&uncalibrated_model));
    deterministic_with_model
        .model_results
        .push(uncalibrated_model);
    deterministic_with_model = deterministic_with_model.seal().unwrap();
    assert!(matches!(
        validate_qa_evidence_graph(
            &plan,
            &run_ref,
            &deterministic_with_model,
            &principal("qa-1", AuthorityRole::Qa),
            150
        ),
        Err(DeliveryError::AdapterUnavailable {
            dependency: "qa_model_evidence_#749",
            ..
        })
    ));

    let mut mismatched_model = valid.clone();
    let mut model = verified_model;
    model.reported_model_id = Some("other-model".to_string());
    model.model_identity_status = QaModelIdentityStatus::Mismatch;
    mismatched_model.case_results[0]
        .grader_refs
        .push(model_ref(&model));
    mismatched_model.model_results.push(model);
    mismatched_model = mismatched_model.seal().unwrap();
    assert!(matches!(
        validate_qa_evidence_graph(
            &plan,
            &run_ref,
            &mismatched_model,
            &principal("qa-1", AuthorityRole::Qa),
            150
        ),
        Err(DeliveryError::AdapterUnavailable {
            dependency: "qa_model_evidence_#749",
            ..
        })
    ));

    let mut missing_flake = valid.clone();
    missing_flake.case_results[0].outcome = QaCaseOutcome::FlakyUnresolved;
    missing_flake.case_results[0].reason_code = QaCaseReasonCode::FlakyUnresolved;
    missing_flake.case_results[0].disposition = None;
    missing_flake = missing_flake.seal().unwrap();
    assert!(matches!(
        validate_qa_evidence_graph(
            &plan,
            &run_ref,
            &missing_flake,
            &principal("qa-1", AuthorityRole::Qa),
            150,
        ),
        Err(DeliveryError::MissingEvidence(_))
    ));

    let mut expired_flake = valid.clone();
    expired_flake.case_results[0].outcome = QaCaseOutcome::FlakyUnresolved;
    expired_flake.case_results[0].reason_code = QaCaseReasonCode::FlakyUnresolved;
    expired_flake.case_results[0].disposition = Some(VersionedRefV1 {
        id: "flake-1".to_string(),
        generation: 1,
        digest: ContentDigest::zero(),
    });
    let disposition = QaFlakeDispositionV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        disposition_id: "flake-1".to_string(),
        generation: 1,
        result: VersionedRefV1 {
            id: expired_flake.case_results[0].result_id.clone(),
            generation: expired_flake.case_results[0].generation,
            digest: qa_case_result_binding_digest(&expired_flake.case_results[0]).unwrap(),
        },
        owner: principal("qa-1", AuthorityRole::Qa),
        classification: QaFlakeClassification::Infrastructure,
        reason: QaFlakeReason::Unresolved,
        policy_revision: plan.generation,
        expires_at_ms: 149,
        defect_ref: reference("defect-1", 1),
        deterministic_regression_fixture: reference("regression-1", 1),
    };
    expired_flake.case_results[0]
        .disposition
        .as_mut()
        .unwrap()
        .digest =
        ContentDigest::of_domain("qa-flake-disposition", DELIVERY_SCHEMA_V1, &disposition).unwrap();
    expired_flake.flake_dispositions.push(disposition);
    expired_flake = expired_flake.seal().unwrap();
    assert!(matches!(
        validate_qa_evidence_graph(
            &plan,
            &run_ref,
            &expired_flake,
            &principal("qa-1", AuthorityRole::Qa),
            150
        ),
        Err(DeliveryError::StaleEvidence(_))
    ));

    let mut wrong_owner_flake = valid.clone();
    wrong_owner_flake.case_results[0].outcome = QaCaseOutcome::FlakyUnresolved;
    wrong_owner_flake.case_results[0].reason_code = QaCaseReasonCode::FlakyUnresolved;
    wrong_owner_flake.case_results[0].disposition = Some(reference("flake-wrong-owner", 1));
    let mut wrong_owner = QaFlakeDispositionV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        disposition_id: "flake-wrong-owner".to_string(),
        generation: 1,
        result: VersionedRefV1 {
            id: wrong_owner_flake.case_results[0].result_id.clone(),
            generation: wrong_owner_flake.case_results[0].generation,
            digest: qa_case_result_binding_digest(&wrong_owner_flake.case_results[0]).unwrap(),
        },
        owner: PrincipalV1 {
            tenant_id: "tenant-b".to_string(),
            ..principal("forged-qa", AuthorityRole::Qa)
        },
        classification: QaFlakeClassification::Infrastructure,
        reason: QaFlakeReason::Unresolved,
        policy_revision: plan.generation,
        expires_at_ms: 200,
        defect_ref: reference("defect-wrong-owner", 1),
        deterministic_regression_fixture: reference("regression-wrong-owner", 1),
    };
    wrong_owner_flake.case_results[0]
        .disposition
        .as_mut()
        .unwrap()
        .digest =
        ContentDigest::of_domain("qa-flake-disposition", DELIVERY_SCHEMA_V1, &wrong_owner).unwrap();
    wrong_owner_flake
        .flake_dispositions
        .push(wrong_owner.clone());
    wrong_owner_flake = wrong_owner_flake.seal().unwrap();
    assert!(matches!(
        validate_qa_evidence_graph(
            &plan,
            &run_ref,
            &wrong_owner_flake,
            &principal("qa-1", AuthorityRole::Qa),
            150,
        ),
        Err(DeliveryError::StaleEvidence(_))
    ));

    let mut malformed_flake = valid;
    malformed_flake.case_results[0].outcome = QaCaseOutcome::FlakyUnresolved;
    malformed_flake.case_results[0].reason_code = QaCaseReasonCode::FlakyUnresolved;
    malformed_flake.case_results[0].disposition = Some(reference("flake-malformed", 1));
    wrong_owner.disposition_id = "flake-malformed".to_string();
    wrong_owner.owner = principal("qa-1", AuthorityRole::Qa);
    wrong_owner.result = VersionedRefV1 {
        id: malformed_flake.case_results[0].result_id.clone(),
        generation: malformed_flake.case_results[0].generation,
        digest: qa_case_result_binding_digest(&malformed_flake.case_results[0]).unwrap(),
    };
    wrong_owner.defect_ref.digest = ContentDigest::zero();
    malformed_flake.case_results[0]
        .disposition
        .as_mut()
        .unwrap()
        .digest =
        ContentDigest::of_domain("qa-flake-disposition", DELIVERY_SCHEMA_V1, &wrong_owner).unwrap();
    malformed_flake.flake_dispositions.push(wrong_owner);
    malformed_flake = malformed_flake.seal().unwrap();
    assert!(matches!(
        validate_qa_evidence_graph(
            &plan,
            &run_ref,
            &malformed_flake,
            &principal("qa-1", AuthorityRole::Qa),
            150,
        ),
        Err(DeliveryError::Validation(_))
    ));
}

#[test]
fn evidence_graph_accepts_cross_inventory_reuse_and_independent_result_sources() {
    let temp = TempDir::new().unwrap();
    let (core, _, plan, _) = running_qa_core(&temp, FakeIntegration::new().0);
    let (_, receipt) = core
        .execute_qa(
            &context("qa-1", AuthorityRole::Qa, "execute-source-positive", 140),
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
    let mut graph = evidence_graph(&run_ref, &plan.plan_digest, &receipt);
    graph.case_results[0].sources = vec![
        fixture_source(),
        SourceTupleV1 {
            owner: "workbench".to_string(),
            source_type: "invocation".to_string(),
            id: "shared-evidence".to_string(),
            generation: 1,
            digest: digest("workbench-evidence"),
        },
        SourceTupleV1 {
            owner: "event-store".to_string(),
            source_type: "event".to_string(),
            id: "shared-evidence".to_string(),
            generation: 2,
            digest: digest("event-evidence"),
        },
        SourceTupleV1 {
            owner: "artifact-store".to_string(),
            source_type: "artifact".to_string(),
            id: "shared-evidence".to_string(),
            generation: 3,
            digest: digest("artifact-evidence"),
        },
    ];
    graph = graph.seal().unwrap();
    validate_qa_evidence_graph(
        &plan,
        &run_ref,
        &graph,
        &principal("qa-1", AuthorityRole::Qa),
        150,
    )
    .unwrap();
}

#[test]
fn evidence_graph_rejects_fixture_substitution_and_invalid_source_tuples() {
    let temp = TempDir::new().unwrap();
    let (core, _, plan, _) = running_qa_core(&temp, FakeIntegration::new().0);
    let (_, receipt) = core
        .execute_qa(
            &context("qa-1", AuthorityRole::Qa, "execute-fixture-negative", 140),
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
    let valid = evidence_graph(&run_ref, &plan.plan_digest, &receipt);

    let mut substituted = valid.clone();
    substituted.dataset_cases[0].input_digest = digest("substituted-input");
    substituted = substituted.seal().unwrap();
    assert!(matches!(
        validate_qa_evidence_graph(
            &plan,
            &run_ref,
            &substituted,
            &principal("qa-1", AuthorityRole::Qa),
            150
        ),
        Err(DeliveryError::MissingEvidence(_))
    ));

    let mut empty_sources = valid.clone();
    empty_sources.dataset_cases[0].provenance.clear();
    empty_sources = empty_sources.seal().unwrap();
    assert!(matches!(
        validate_qa_evidence_graph(
            &plan,
            &run_ref,
            &empty_sources,
            &principal("qa-1", AuthorityRole::Qa),
            150
        ),
        Err(DeliveryError::MissingEvidence(_))
    ));

    let mut duplicate_result_source = valid.clone();
    let repeated_source = duplicate_result_source.case_results[0].sources[0].clone();
    duplicate_result_source.case_results[0]
        .sources
        .push(repeated_source);
    duplicate_result_source = duplicate_result_source.seal().unwrap();
    assert!(matches!(
        validate_qa_evidence_graph(
            &plan,
            &run_ref,
            &duplicate_result_source,
            &principal("qa-1", AuthorityRole::Qa),
            150
        ),
        Err(DeliveryError::Conflict(_))
    ));

    let mut cross_inventory_conflict = valid.clone();
    cross_inventory_conflict.case_results[0].sources[0].digest =
        digest("cross-inventory-conflicting-content");
    cross_inventory_conflict = cross_inventory_conflict.seal().unwrap();
    assert!(matches!(
        validate_qa_evidence_graph(
            &plan,
            &run_ref,
            &cross_inventory_conflict,
            &principal("qa-1", AuthorityRole::Qa),
            150
        ),
        Err(DeliveryError::Conflict(_))
    ));

    let mut conflicting_result_source = valid.clone();
    let mut conflicting_digest = conflicting_result_source.case_results[0].sources[0].clone();
    conflicting_digest.digest = digest("conflicting-source-content");
    conflicting_result_source.case_results[0]
        .sources
        .push(conflicting_digest);
    conflicting_result_source = conflicting_result_source.seal().unwrap();
    assert!(matches!(
        validate_qa_evidence_graph(
            &plan,
            &run_ref,
            &conflicting_result_source,
            &principal("qa-1", AuthorityRole::Qa),
            150
        ),
        Err(DeliveryError::Conflict(_))
    ));

    let mut malformed_source = valid;
    malformed_source.case_results[0].sources[0].digest = ContentDigest::zero();
    malformed_source = malformed_source.seal().unwrap();
    assert!(matches!(
        validate_qa_evidence_graph(
            &plan,
            &run_ref,
            &malformed_source,
            &principal("qa-1", AuthorityRole::Qa),
            150
        ),
        Err(DeliveryError::Validation(_))
    ));
}

#[test]
fn evidence_graph_rejects_changed_or_missing_case_slices() {
    let temp = TempDir::new().unwrap();
    let (core, _, plan, _) = running_qa_core(&temp, FakeIntegration::new().0);
    let (_, receipt) = core
        .execute_qa(
            &context("qa-1", AuthorityRole::Qa, "execute-slice-negative", 140),
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
    let valid = evidence_graph(&run_ref, &plan.plan_digest, &receipt);

    let mut changed = valid.clone();
    changed.case_results[0]
        .slices
        .insert("surface".to_string(), "relabeled".to_string());
    changed = changed.seal().unwrap();
    assert!(matches!(
        validate_qa_evidence_graph(
            &plan,
            &run_ref,
            &changed,
            &principal("qa-1", AuthorityRole::Qa),
            150
        ),
        Err(DeliveryError::StaleEvidence(_))
    ));

    let mut missing = valid;
    missing.case_results[0].slices.clear();
    missing = missing.seal().unwrap();
    assert!(matches!(
        validate_qa_evidence_graph(
            &plan,
            &run_ref,
            &missing,
            &principal("qa-1", AuthorityRole::Qa),
            150
        ),
        Err(DeliveryError::StaleEvidence(_))
    ));
}

#[test]
fn evidence_graph_rejects_duplicate_result_for_same_case() {
    let temp = TempDir::new().unwrap();
    let (core, _, plan, _) = running_qa_core(&temp, FakeIntegration::new().0);
    let (_, receipt) = core
        .execute_qa(
            &context("qa-1", AuthorityRole::Qa, "execute-result-negative", 140),
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
    let mut duplicate = evidence_graph(&run_ref, &plan.plan_digest, &receipt);
    let mut second_result = duplicate.case_results[0].clone();
    second_result.result_id = "result-security-duplicate".to_string();
    duplicate.case_results.push(second_result);
    duplicate = duplicate.seal().unwrap();
    assert!(matches!(
        validate_qa_evidence_graph(
            &plan,
            &run_ref,
            &duplicate,
            &principal("qa-1", AuthorityRole::Qa),
            150
        ),
        Err(DeliveryError::Conflict(_))
    ));
}
