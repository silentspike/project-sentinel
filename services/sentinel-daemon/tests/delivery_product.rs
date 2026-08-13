use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use sentinel_daemon::delivery::*;
use tempfile::TempDir;

fn digest(value: &str) -> ContentDigest {
    ContentDigest::of(&value).expect("test digest")
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

fn candidate() -> ReleaseCandidateV1 {
    ReleaseCandidateV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        candidate_id: "candidate-private-1".to_string(),
        generation: 1,
        tenant_id: "tenant-a".to_string(),
        agreement: reference("agreement-private-1", 2),
        project: reference("project-private-1", 3),
        work_items_digest: digest("work-items"),
        source_digest: digest("source"),
        artifacts: vec![ArtifactRefV1 {
            artifact_id: "artifact-private-1".to_string(),
            generation: 1,
            digest: digest("artifact"),
            media_type: "application/octet-stream".to_string(),
            owner_principal_id: "developer-private".to_string(),
        }],
        toolchain_digest: digest("toolchain"),
        runtime_profile_digest: digest("runtime-profile"),
        acceptance_criteria_digest: digest("acceptance-criteria"),
        implementer_principal_ids: BTreeSet::from(["developer-private".to_string()]),
        cost: CostRefV1 {
            ledger_id: "ledger-private-1".to_string(),
            generation: 1,
            digest: digest("cost"),
            currency: "USD".to_string(),
            amount_minor: 125,
        },
        state: CandidateState::Draft,
        candidate_digest: ContentDigest::zero(),
        created_at_ms: 100,
    }
    .seal()
    .expect("candidate seal")
}

fn plan_for(candidate: &ReleaseCandidateV1) -> QaEvaluationPlanV1 {
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
        required_case_ids: BTreeSet::from(["case-1".to_string()]),
        optional_case_ids: BTreeSet::new(),
        fixture_inventory_digest: digest("fixtures"),
        evaluator_policy_digest: digest("evaluator"),
        aggregation_policy_digest: digest("aggregation"),
        release_policy_digest: digest("release-policy"),
        runner_binary_digest: digest("runner"),
        toolchain_digest: candidate.toolchain_digest.clone(),
        sandbox_profile_digest: digest("sandbox"),
        capability_digest: digest("capability"),
        environment_digest: digest("environment"),
        credential_policy_digest: digest("credential-policy"),
        declared_seeds: BTreeSet::new(),
        retry_limit: 0,
        retryable_classes: BTreeSet::new(),
        data_control: DataControlV1 {
            classification: "internal".to_string(),
            encryption_key_owner: "security".to_string(),
            access_policy_digest: digest("access"),
            redaction_policy_digest: digest("redaction"),
            retention_frontier: reference("frontier", 1),
            audit_policy_digest: digest("audit"),
        },
        plan_digest: ContentDigest::zero(),
    }
    .seal()
    .expect("plan seal")
}

fn run_for(plan: &QaEvaluationPlanV1) -> QaEvaluationRunReceiptV1 {
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
        state: QaRunState::Running,
        retry_of: None,
        supersedes: None,
        actors: vec![principal("qa", AuthorityRole::Qa)],
        durable_event_generation: 1,
        started_at_ms: Some(1),
        finished_at_ms: None,
        attempts: 0,
        case_attempt_history_digest: None,
        harness_outcome: None,
        cleanup_receipt: None,
        aggregate_outcomes: None,
        gate_receipt: None,
    }
}

fn release(id: &str) -> ReleaseV1 {
    ReleaseV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        release_id: id.to_string(),
        generation: 1,
        manifest: reference("manifest", 1),
        state: ReleaseState::Active,
        activated_at_ms: Some(1),
        rollout_receipt: Some(reference("rollout", 1)),
    }
}

fn manifest_for(candidate: &ReleaseCandidateV1) -> ReleaseManifestV1 {
    ReleaseManifestV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        manifest_id: "manifest-1".to_string(),
        generation: 1,
        tenant_id: candidate.tenant_id.clone(),
        agreement: candidate.agreement.clone(),
        project: candidate.project.clone(),
        candidate: VersionedRefV1 {
            id: candidate.candidate_id.clone(),
            generation: candidate.generation,
            digest: candidate.candidate_digest.clone(),
        },
        work_items_digest: candidate.work_items_digest.clone(),
        source_digest: candidate.source_digest.clone(),
        artifacts: candidate.artifacts.clone(),
        toolchain_digest: candidate.toolchain_digest.clone(),
        runtime_profile_digest: candidate.runtime_profile_digest.clone(),
        qa_gate: reference("gate-1", 1),
        qa_evidence_digest: digest("qa-evidence"),
        sbom_digest: digest("sbom"),
        dependency_snapshot_digest: digest("dependency-snapshot"),
        provenance_digest: digest("provenance"),
        release_actor: principal("release-manager", AuthorityRole::ReleaseManager),
        cost: candidate.cost.clone(),
        rollback_release: None,
        manifest_digest: ContentDigest::zero(),
        created_at_ms: 1,
    }
    .seal()
    .expect("manifest seal")
}

#[derive(Clone)]
struct DeterministicIntegration {
    principals: Vec<PrincipalV1>,
    execution_ready: bool,
    workflow_fault: WorkflowFault,
    lineage_phase: Arc<AtomicUsize>,
}

#[derive(Clone, Copy, Default)]
enum WorkflowFault {
    #[default]
    None,
    OmitClass,
    SubstituteDigest,
    PrivateBinding,
    GenerationMismatch,
    AuthorityRevoked,
    CandidateSwap,
    OmitEdge,
    Disconnected,
    DuplicateEdge,
    Cycle,
    DuplicateRoot,
    IllegalState,
    StaleIntegration,
}

impl DeliveryIntegrationPort for DeterministicIntegration {
    fn readiness(&self) -> AdapterReadiness {
        AdapterReadiness::Ready {
            contract_version: DELIVERY_SCHEMA_V1,
            authority_generation: 11,
            contract_digest: if matches!(self.workflow_fault, WorkflowFault::StaleIntegration) {
                digest("stale-integration-contract")
            } else {
                expected_integration_contract_digest()
            },
        }
    }

    fn execution_saga_readiness(&self) -> AdapterReadiness {
        if self.execution_ready {
            AdapterReadiness::Ready {
                contract_version: DELIVERY_SCHEMA_V1,
                authority_generation: 11,
                contract_digest: expected_workbench_execution_saga_contract_digest(),
            }
        } else {
            AdapterReadiness::Unavailable {
                reason: "deterministic workbench unavailable".to_string(),
            }
        }
    }

    fn candidate_authority(
        &self,
        query: &CandidateAuthorityQueryV1,
    ) -> Result<CandidateAuthoritySnapshotV1, DeliveryError> {
        let phase = self.lineage_phase.fetch_add(1, Ordering::SeqCst);
        let generation = if matches!(self.workflow_fault, WorkflowFault::GenerationMismatch)
            && phase >= 1
        {
            12
        } else {
            11
        };
        let candidate_digest =
            if matches!(self.workflow_fault, WorkflowFault::CandidateSwap) && phase >= 1 {
                digest("candidate-swap")
            } else {
                query.candidate_digest.clone()
            };
        CandidateAuthoritySnapshotV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            authority_generation: generation,
            agreement: query.agreement.clone(),
            project: query.project.clone(),
            work_items_digest: query.work_items_digest.clone(),
            current_candidate_generation: 1,
            current_candidate_digest: candidate_digest,
            participant_principals: self.principals.clone(),
            snapshot_digest: ContentDigest::zero(),
        }
        .seal()
    }

    fn workflow_lineage(
        &self,
        query: &WorkflowLineageQueryV1,
    ) -> Result<WorkflowLineageSnapshotV1, DeliveryError> {
        self.lineage_phase.store(2, Ordering::SeqCst);
        let kinds = [
            (
                WorkflowLineageKindV1::CustomerRequest,
                WorkflowLineageStateV1::Requested,
                None,
            ),
            (
                WorkflowLineageKindV1::Agreement,
                WorkflowLineageStateV1::Approved,
                None,
            ),
            (
                WorkflowLineageKindV1::Project,
                WorkflowLineageStateV1::Active,
                None,
            ),
            (
                WorkflowLineageKindV1::WorkItem,
                WorkflowLineageStateV1::Completed,
                None,
            ),
            (
                WorkflowLineageKindV1::Participant,
                WorkflowLineageStateV1::Active,
                Some(AuthorityRole::Developer),
            ),
            (
                WorkflowLineageKindV1::Decision,
                WorkflowLineageStateV1::Approved,
                None,
            ),
            (
                WorkflowLineageKindV1::Handoff,
                WorkflowLineageStateV1::HandedOff,
                None,
            ),
            (
                WorkflowLineageKindV1::Blocker,
                WorkflowLineageStateV1::Clear,
                None,
            ),
        ];
        let nodes = kinds
            .into_iter()
            .enumerate()
            .map(
                |(index, (kind, state, participant_role))| WorkflowLineageNodeV1 {
                    node_ordinal: (index + 1) as u32,
                    kind,
                    state,
                    generation: 1,
                    digest: digest(&format!("workflow-node-{}", index + 1)),
                    participant_role,
                },
            )
            .collect::<Vec<_>>();
        let edges = nodes
            .windows(2)
            .map(|pair| WorkflowLineageEdgeV1 {
                from_ordinal: pair[0].node_ordinal,
                to_ordinal: pair[1].node_ordinal,
            })
            .collect();
        let mut snapshot = WorkflowLineageSnapshotV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            server_redacted: true,
            tenant_id: query.tenant_id.clone(),
            project: query.project.clone(),
            candidate: query.candidate.clone(),
            authority_generation: query.authority_generation,
            authority_identity_digest: query.authority_identity_digest.clone(),
            query_digest: query.query_digest.clone(),
            snapshot_generation: 1,
            nodes,
            edges,
            snapshot_digest: ContentDigest::zero(),
        };
        match self.workflow_fault {
            WorkflowFault::None
            | WorkflowFault::StaleIntegration
            | WorkflowFault::GenerationMismatch
            | WorkflowFault::AuthorityRevoked
            | WorkflowFault::CandidateSwap => {}
            WorkflowFault::OmitClass => {
                snapshot.nodes.pop();
                snapshot.edges.pop();
            }
            WorkflowFault::SubstituteDigest => snapshot.candidate.digest = digest("substitute"),
            WorkflowFault::PrivateBinding => snapshot.tenant_id = "secret=private".to_string(),
            WorkflowFault::OmitEdge => {
                snapshot.edges.remove(0);
            }
            WorkflowFault::Disconnected => {
                snapshot.edges.remove(2);
            }
            WorkflowFault::DuplicateEdge => snapshot.edges.push(snapshot.edges[0].clone()),
            WorkflowFault::Cycle => snapshot.edges.push(WorkflowLineageEdgeV1 {
                from_ordinal: 8,
                to_ordinal: 3,
            }),
            WorkflowFault::DuplicateRoot => {
                let mut duplicate = snapshot.nodes[0].clone();
                duplicate.node_ordinal = 9;
                duplicate.digest = digest("duplicate-request-root");
                snapshot.nodes.push(duplicate);
                snapshot.edges.push(WorkflowLineageEdgeV1 {
                    from_ordinal: 1,
                    to_ordinal: 9,
                });
            }
            WorkflowFault::IllegalState => {
                snapshot.nodes[1].state = WorkflowLineageStateV1::Requested;
            }
        }
        snapshot.seal()
    }

    fn authorize(
        &self,
        request: &AuthorityValidationRequestV1,
    ) -> Result<AuthorityReceiptV1, DeliveryError> {
        let mut principal = self
            .principals
            .iter()
            .find(|principal| principal.principal_id == request.principal_id)
            .cloned()
            .ok_or_else(|| DeliveryError::AuthorityDenied("unknown principal".to_string()))?;
        if matches!(self.workflow_fault, WorkflowFault::AuthorityRevoked)
            && self.lineage_phase.load(Ordering::SeqCst) >= 2
        {
            principal.authority_generation += 1;
        }
        AuthorityReceiptV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            request_digest: request.request_digest.clone(),
            principal,
            contract_version: request.contract_version,
            contract_authority_generation: 11,
            contract_digest: request.contract_digest.clone(),
            issued_at_ms: 1,
            expires_at_ms: 10_000,
            issuer: "deterministic-authority".to_string(),
            receipt_digest: ContentDigest::zero(),
        }
        .seal()
    }

    fn execute_qa(
        &self,
        _request: &WorkbenchEvidenceRequestV1,
    ) -> Result<WorkbenchEvidenceReceiptV1, DeliveryError> {
        Err(DeliveryError::AdapterUnavailable {
            dependency: "workbench",
            reason: "deterministic product-shape test does not execute QA".to_string(),
        })
    }
}

#[derive(Clone, Copy)]
struct DeterministicEffects;

impl DeliveryEffectPort for DeterministicEffects {
    fn readiness(&self) -> AdapterReadiness {
        AdapterReadiness::Ready {
            contract_version: DELIVERY_SCHEMA_V1,
            authority_generation: 11,
            contract_digest: expected_effect_saga_contract_digest(),
        }
    }

    fn apply(
        &self,
        _request: &DeliveryEffectRequestV1,
    ) -> Result<DeliveryEffectReceiptV1, DeliveryError> {
        Err(DeliveryError::AdapterUnavailable {
            dependency: "delivery_effect",
            reason: "deterministic product-shape test performs no external effect".to_string(),
        })
    }
}

#[derive(Default)]
struct PublisherState {
    receipts: BTreeMap<(String, ContentDigest), PublicationReceiptV1>,
    invocations: usize,
    effective_events: usize,
}

#[derive(Clone, Default)]
struct DeterministicPublisher(Arc<Mutex<PublisherState>>);

impl DeterministicPublisher {
    fn counts(&self) -> (usize, usize) {
        let state = self.0.lock().expect("publisher state");
        (state.invocations, state.effective_events)
    }
}

impl DeliveryPublicationPort for DeterministicPublisher {
    fn readiness(&self) -> AdapterReadiness {
        AdapterReadiness::Ready {
            contract_version: DELIVERY_SCHEMA_V1,
            authority_generation: 11,
            contract_digest: expected_publication_contract_digest(),
        }
    }

    fn publish(
        &self,
        request: &PublicationRequestV1,
    ) -> Result<PublicationReceiptV1, DeliveryError> {
        let mut state = self.0.lock().expect("publisher state");
        state.invocations += 1;
        let key = (request.operation_id.clone(), request.request_digest.clone());
        if let Some(receipt) = state.receipts.get(&key) {
            return Ok(receipt.clone());
        }
        let receipt = PublicationReceiptV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            operation_id: request.operation_id.clone(),
            event_id: format!("event:{}", request.operation_id),
            aggregate_id: request.aggregate_id.clone(),
            row_identity: request.row_identity.clone(),
            payload_digest: request.payload_digest.clone(),
            request_digest: request.request_digest.clone(),
        };
        state.receipts.insert(key, receipt.clone());
        state.effective_events += 1;
        Ok(receipt)
    }
}

fn config(temp: &TempDir) -> DeliveryStoreConfigV1 {
    #[cfg(unix)]
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))
        .expect("protect temp store root");
    DeliveryStoreConfigV1::new(temp.path(), "delivery.redb").expect("store config")
}

#[test]
fn configured_core_starts_with_typed_unavailable_ports_and_no_fake_readiness() {
    let temp = TempDir::new().expect("tempdir");
    assert!(matches!(
        DeliveryStoreConfigV1::new("relative-root", "delivery.redb"),
        Err(DeliveryError::Validation(_))
    ));
    let product = ConfiguredDeliveryCore::open(
        &config(&temp),
        UnavailableDeliveryIntegration,
        UnavailableDeliveryEffects,
        UnavailableDeliveryPublication,
    )
    .expect("local store opens independently");

    assert!(matches!(
        product.readiness(),
        Err(DeliveryError::AdapterUnavailable {
            dependency: "delivery_integration",
            ..
        })
    ));
    assert!(matches!(
        product.command_readiness(DeliveryCommandV1::RegisterCandidate),
        Err(DeliveryError::AdapterUnavailable {
            dependency: "delivery_integration",
            ..
        })
    ));
    assert!(matches!(
        product.read_public_lineage(
            &CommandContextV1 {
                principal: principal("auditor-private", AuthorityRole::Auditor),
                idempotency_key: "read-with-integration-down".to_string(),
                now_ms: 10,
            },
            "tenant-a",
            "project-private-1",
        ),
        Err(DeliveryError::AdapterUnavailable {
            dependency: "delivery_integration",
            ..
        })
    ));
    let rejected = product.register_candidate(
        &CommandContextV1 {
            principal: principal("developer-private", AuthorityRole::Developer),
            idempotency_key: "must-not-bypass".to_string(),
            now_ms: 100,
        },
        candidate(),
    );
    assert!(matches!(
        rejected,
        Err(DeliveryError::AdapterUnavailable {
            dependency: "delivery_integration",
            ..
        })
    ));
    assert_eq!(
        product
            .pending_publication_count()
            .expect("no bypass outbox"),
        0
    );
    let service_source = include_str!("../src/delivery/service.rs");
    assert!(!service_source.contains("pub fn core(&self)"));
    assert!(!service_source.contains("pub fn with_ports("));
    drop(product);

    let stale_integration_temp = TempDir::new().expect("stale integration tempdir");
    let stale_integration = ConfiguredDeliveryCore::open(
        &config(&stale_integration_temp),
        DeterministicIntegration {
            principals: vec![],
            execution_ready: true,
            workflow_fault: WorkflowFault::StaleIntegration,
            lineage_phase: Arc::default(),
        },
        DeterministicEffects,
        DeterministicPublisher::default(),
    )
    .expect("stale integration store opens");
    assert!(matches!(
        stale_integration.read_public_lineage(
            &CommandContextV1 {
                principal: principal("auditor-private", AuthorityRole::Auditor),
                idempotency_key: "read-with-stale-integration".to_string(),
                now_ms: 10,
            },
            "tenant-a",
            "project-private-1",
        ),
        Err(DeliveryError::StaleEvidence(_))
    ));
    drop(stale_integration);

    fn seed_committed_lineage(temp: &TempDir) -> (PrincipalV1, PrincipalV1) {
        let developer = principal("developer-private", AuthorityRole::Developer);
        let auditor = principal("auditor-private", AuthorityRole::Auditor);
        let seeded = ConfiguredDeliveryCore::open(
            &config(temp),
            DeterministicIntegration {
                principals: vec![developer.clone(), auditor.clone()],
                execution_ready: true,
                workflow_fault: WorkflowFault::None,
                lineage_phase: Arc::default(),
            },
            DeterministicEffects,
            DeterministicPublisher::default(),
        )
        .expect("seeded store opens");
        seeded
            .register_candidate(
                &CommandContextV1 {
                    principal: developer.clone(),
                    idempotency_key: "seed-lineage".to_string(),
                    now_ms: 100,
                },
                candidate(),
            )
            .expect("committed lineage seed");
        drop(seeded);
        (developer, auditor)
    }

    fn assert_lineage_readable<I, E, P>(
        product: &ConfiguredDeliveryCore<I, E, P>,
        auditor: PrincipalV1,
        key: &str,
    ) where
        I: DeliveryIntegrationPort,
        E: DeliveryEffectPort,
        P: DeliveryPublicationPort,
    {
        product
            .lineage_readiness()
            .expect("lineage-specific readiness");
        let lineage = product
            .read_public_lineage(
                &CommandContextV1 {
                    principal: auditor,
                    idempotency_key: key.to_string(),
                    now_ms: 101,
                },
                "tenant-a",
                "project-private-1",
            )
            .expect("unrelated adapter outage must not hide committed lineage");
        assert!(lineage.adapter_ready);
        assert!(lineage
            .nodes
            .iter()
            .any(|node| node.stage == DeliveryLineageStageV1::Candidate));
    }

    let publication_temp = TempDir::new().expect("publication tempdir");
    let (publication_developer, publication_auditor) = seed_committed_lineage(&publication_temp);
    let publication_gated = ConfiguredDeliveryCore::open(
        &config(&publication_temp),
        DeterministicIntegration {
            principals: vec![publication_developer, publication_auditor.clone()],
            execution_ready: true,
            workflow_fault: WorkflowFault::None,
            lineage_phase: Arc::default(),
        },
        DeterministicEffects,
        UnavailableDeliveryPublication,
    )
    .expect("store and earlier ports are independently ready");
    assert!(matches!(
        publication_gated.readiness(),
        Err(DeliveryError::AdapterUnavailable {
            dependency: "delivery_publication",
            ..
        })
    ));
    assert_lineage_readable(
        &publication_gated,
        publication_auditor,
        "read-with-publication-down",
    );
    drop(publication_gated);

    let execution_temp = TempDir::new().expect("execution tempdir");
    let (execution_developer, execution_auditor) = seed_committed_lineage(&execution_temp);
    let execution_gated = ConfiguredDeliveryCore::open(
        &config(&execution_temp),
        DeterministicIntegration {
            principals: vec![execution_developer, execution_auditor.clone()],
            execution_ready: false,
            workflow_fault: WorkflowFault::None,
            lineage_phase: Arc::default(),
        },
        DeterministicEffects,
        DeterministicPublisher::default(),
    )
    .expect("integration store opens");
    assert!(matches!(
        execution_gated.readiness(),
        Err(DeliveryError::AdapterUnavailable {
            dependency: "workbench_execution_saga",
            ..
        })
    ));
    assert_lineage_readable(
        &execution_gated,
        execution_auditor,
        "read-with-execution-down",
    );
    drop(execution_gated);

    let effects_temp = TempDir::new().expect("effects tempdir");
    let (effects_developer, effects_auditor) = seed_committed_lineage(&effects_temp);
    let effects_gated = ConfiguredDeliveryCore::open(
        &config(&effects_temp),
        DeterministicIntegration {
            principals: vec![effects_developer, effects_auditor.clone()],
            execution_ready: true,
            workflow_fault: WorkflowFault::None,
            lineage_phase: Arc::default(),
        },
        UnavailableDeliveryEffects,
        DeterministicPublisher::default(),
    )
    .expect("integration store opens");
    assert!(matches!(
        effects_gated.readiness(),
        Err(DeliveryError::AdapterUnavailable {
            dependency: "delivery_effect_saga",
            ..
        })
    ));
    assert_lineage_readable(&effects_gated, effects_auditor, "read-with-effects-down");
}

#[cfg(unix)]
#[test]
fn productive_store_rejects_escape_symlinks_and_permissive_files_then_reopens_0600() {
    use std::os::unix::fs::symlink;

    let parent = TempDir::new().expect("parent tempdir");
    let root = parent.path().join("delivery-root");
    std::fs::create_dir(&root).expect("root");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("root mode");
    assert!(DeliveryStoreConfigV1::new(&root, "../escape.redb").is_err());
    let current_uid = std::fs::metadata(&root).expect("root metadata").uid();
    if let Some(foreign_root) = ["/var/tmp", "/"].into_iter().find(|candidate| {
        std::fs::metadata(candidate).is_ok_and(|metadata| metadata.uid() != current_uid)
    }) {
        assert!(DeliveryStoreConfigV1::new(foreign_root, "delivery.redb").is_err());
    }

    for forbidden_mode in [0o1700, 0o2700] {
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(forbidden_mode))
            .expect("set forbidden root special bit");
        assert!(DeliveryStoreConfigV1::new(&root, "delivery.redb").is_err());
    }
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
        .expect("restore root mode");

    let linked_root = parent.path().join("linked-root");
    symlink(&root, &linked_root).expect("root symlink");
    assert!(DeliveryStoreConfigV1::new(&linked_root, "delivery.redb").is_err());

    let outside = parent.path().join("outside.redb");
    std::fs::write(&outside, []).expect("outside file");
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o600))
        .expect("outside mode");
    symlink(&outside, root.join("delivery.redb")).expect("file symlink");
    assert!(DeliveryStoreConfigV1::new(&root, "delivery.redb").is_err());
    std::fs::remove_file(root.join("delivery.redb")).expect("remove file symlink");

    let path = root.join("delivery.redb");
    std::fs::write(&path, []).expect("permissive file");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
        .expect("permissive mode");
    assert!(DeliveryStoreConfigV1::new(&root, "delivery.redb").is_err());
    std::fs::remove_file(&path).expect("remove permissive file");

    for forbidden_mode in [0o4600, 0o2600] {
        std::fs::write(&path, []).expect("special-bit file");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(forbidden_mode))
            .expect("set forbidden store special bit");
        assert!(DeliveryStoreConfigV1::new(&root, "delivery.redb").is_err());
        std::fs::remove_file(&path).expect("remove special-bit file");
    }

    let config = DeliveryStoreConfigV1::new(&root, "delivery.redb").expect("valid config");
    drop(DeliveryStore::open(&config).expect("new protected store"));
    assert_eq!(
        std::fs::metadata(&path)
            .expect("store metadata")
            .permissions()
            .mode()
            & 0o7777,
        0o600
    );
    drop(
        DeliveryStore::open(
            &DeliveryStoreConfigV1::new(&root, "delivery.redb").expect("reopen config"),
        )
        .expect("0600 reopen"),
    );

    let hardlink = root.join("delivery-copy.redb");
    std::fs::hard_link(&path, &hardlink).expect("hardlink store");
    assert!(DeliveryStoreConfigV1::new(&root, "delivery.redb").is_err());
    std::fs::remove_file(&hardlink).expect("remove hardlink");

    let replacement_config =
        DeliveryStoreConfigV1::new(&root, "delivery.redb").expect("replacement config");
    let displaced = root.join("delivery-displaced.redb");
    std::fs::rename(&path, &displaced).expect("displace configured store");
    std::fs::write(&path, []).expect("replacement store");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .expect("replacement mode");
    assert!(DeliveryStore::open(&replacement_config).is_err());
    std::fs::remove_file(&path).expect("remove replacement");
    std::fs::rename(&displaced, &path).expect("restore configured store");

    let root_replacement_config =
        DeliveryStoreConfigV1::new(&root, "delivery.redb").expect("root identity config");
    let displaced_root = parent.path().join("delivery-root-displaced");
    std::fs::rename(&root, &displaced_root).expect("displace configured root");
    std::fs::create_dir(&root).expect("replacement root");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
        .expect("replacement root mode");
    assert!(DeliveryStore::open(&root_replacement_config).is_err());
}

#[test]
fn production_shape_persists_publishes_and_returns_only_authorized_redacted_lineage() {
    let temp = TempDir::new().expect("tempdir");
    let developer = principal("developer-private", AuthorityRole::Developer);
    let auditor = principal("auditor-private", AuthorityRole::Auditor);
    let integration = DeterministicIntegration {
        principals: vec![developer.clone(), auditor.clone()],
        execution_ready: true,
        workflow_fault: WorkflowFault::None,
        lineage_phase: Arc::default(),
    };
    let candidate = candidate();

    {
        let product = ConfiguredDeliveryCore::open(
            &config(&temp),
            integration.clone(),
            DeterministicEffects,
            DeterministicPublisher::default(),
        )
        .expect("configured product");
        product.readiness().expect("all deterministic ports ready");
        product
            .register_candidate(
                &CommandContextV1 {
                    principal: developer,
                    idempotency_key: "register-1".to_string(),
                    now_ms: 100,
                },
                candidate,
            )
            .expect("candidate commit");

        let lineage = product
            .read_public_lineage(
                &CommandContextV1 {
                    principal: auditor.clone(),
                    idempotency_key: "read-1".to_string(),
                    now_ms: 101,
                },
                "tenant-a",
                "project-private-1",
            )
            .expect("authorized lineage");
        let cross_tenant = product.read_public_lineage(
            &CommandContextV1 {
                principal: auditor.clone(),
                idempotency_key: "read-cross-tenant".to_string(),
                now_ms: 101,
            },
            "tenant-b",
            "project-private-1",
        );
        assert!(matches!(
            cross_tenant,
            Err(DeliveryError::AuthorityDenied(_))
        ));
        let json = serde_json::to_string(&lineage).expect("lineage JSON");
        assert!(lineage.server_redacted);
        assert_eq!(lineage.project_label, "Project");
        assert_eq!(lineage.nodes[0].id, "node-000001");
        assert_eq!(lineage.nodes.len(), 10);
        assert!(!json.contains("tenant-a"));
        assert!(!json.contains("project-private-1"));
        assert!(!json.contains("agreement-private-1"));
        assert!(!json.contains("candidate-private-1"));
        assert!(!json.contains("developer-private"));
        assert!(!json.contains("artifact-private-1"));
        assert!(!json.contains("ledger-private-1"));
        let stages = lineage
            .nodes
            .iter()
            .map(|node| node.stage)
            .collect::<BTreeSet<_>>();
        for stage in [
            DeliveryLineageStageV1::CustomerRequest,
            DeliveryLineageStageV1::Agreement,
            DeliveryLineageStageV1::Project,
            DeliveryLineageStageV1::WorkItem,
            DeliveryLineageStageV1::Participant,
            DeliveryLineageStageV1::Decision,
            DeliveryLineageStageV1::Handoff,
            DeliveryLineageStageV1::Blocker,
            DeliveryLineageStageV1::Candidate,
            DeliveryLineageStageV1::Artifact,
        ] {
            assert!(stages.contains(&stage));
        }
        let low_entropy_hash = ContentDigest::of_domain(
            "public-delivery-project",
            DELIVERY_SCHEMA_V1,
            &("tenant-a", "project-private-1"),
        )
        .expect("low entropy hash");
        assert!(!json.contains(&low_entropy_hash.as_str()[..12]));

        assert_eq!(product.publish_pending().expect("publish"), 1);
        assert_eq!(product.publish_pending().expect("idempotent readback"), 0);
    }

    let reopened = ConfiguredDeliveryCore::open(
        &config(&temp),
        integration,
        DeterministicEffects,
        DeterministicPublisher::default(),
    )
    .expect("restart reopens durable store");
    let lineage = reopened
        .read_public_lineage(
            &CommandContextV1 {
                principal: auditor.clone(),
                idempotency_key: "read-2".to_string(),
                now_ms: 102,
            },
            "tenant-a",
            "project-private-1",
        )
        .expect("lineage after restart");
    assert_eq!(lineage.revision, 1);
    assert_eq!(lineage.nodes.len(), 10);
    assert_eq!(
        reopened
            .publish_pending()
            .expect("published receipt persisted"),
        0
    );
}

#[test]
fn workflow_lineage_fails_closed_on_omission_digest_substitution_and_private_keys() {
    for fault in [
        WorkflowFault::OmitClass,
        WorkflowFault::SubstituteDigest,
        WorkflowFault::PrivateBinding,
    ] {
        let temp = TempDir::new().expect("tempdir");
        let developer = principal("developer-private", AuthorityRole::Developer);
        let auditor = principal("auditor-private", AuthorityRole::Auditor);
        let product = ConfiguredDeliveryCore::open(
            &config(&temp),
            DeterministicIntegration {
                principals: vec![developer.clone(), auditor.clone()],
                execution_ready: true,
                workflow_fault: fault,
                lineage_phase: Arc::default(),
            },
            DeterministicEffects,
            DeterministicPublisher::default(),
        )
        .expect("configured product");
        product
            .register_candidate(
                &CommandContextV1 {
                    principal: developer,
                    idempotency_key: "register-workflow-fault".to_string(),
                    now_ms: 100,
                },
                candidate(),
            )
            .expect("candidate commit");
        let result = product.read_public_lineage(
            &CommandContextV1 {
                principal: auditor,
                idempotency_key: "read-workflow-fault".to_string(),
                now_ms: 101,
            },
            "tenant-a",
            "project-private-1",
        );
        assert!(matches!(
            result,
            Err(DeliveryError::CorruptStore(_) | DeliveryError::StaleEvidence(_))
        ));
    }
}

#[test]
fn lineage_generation_mismatch_revocation_and_candidate_swap_return_no_dto() {
    for fault in [
        WorkflowFault::GenerationMismatch,
        WorkflowFault::AuthorityRevoked,
        WorkflowFault::CandidateSwap,
    ] {
        let temp = TempDir::new().expect("tempdir");
        let developer = principal("developer-private", AuthorityRole::Developer);
        let auditor = principal("auditor-private", AuthorityRole::Auditor);
        let phase = Arc::new(AtomicUsize::new(0));
        let product = ConfiguredDeliveryCore::open(
            &config(&temp),
            DeterministicIntegration {
                principals: vec![developer.clone(), auditor.clone()],
                execution_ready: true,
                workflow_fault: fault,
                lineage_phase: phase.clone(),
            },
            DeterministicEffects,
            DeterministicPublisher::default(),
        )
        .expect("configured product");
        product
            .register_candidate(
                &CommandContextV1 {
                    principal: developer,
                    idempotency_key: "register-before-lineage-race".to_string(),
                    now_ms: 100,
                },
                candidate(),
            )
            .expect("candidate commit");
        phase.store(0, Ordering::SeqCst);
        let result = product.read_public_lineage(
            &CommandContextV1 {
                principal: auditor,
                idempotency_key: "read-lineage-race".to_string(),
                now_ms: 101,
            },
            "tenant-a",
            "project-private-1",
        );
        assert!(matches!(result, Err(DeliveryError::StaleEvidence(_))));
    }
}

#[test]
fn workflow_lineage_rejects_incomplete_disconnected_duplicate_cyclic_or_illegal_topology() {
    for fault in [
        WorkflowFault::OmitEdge,
        WorkflowFault::Disconnected,
        WorkflowFault::DuplicateEdge,
        WorkflowFault::Cycle,
        WorkflowFault::DuplicateRoot,
        WorkflowFault::IllegalState,
    ] {
        let temp = TempDir::new().expect("tempdir");
        let developer = principal("developer-private", AuthorityRole::Developer);
        let auditor = principal("auditor-private", AuthorityRole::Auditor);
        let product = ConfiguredDeliveryCore::open(
            &config(&temp),
            DeterministicIntegration {
                principals: vec![developer.clone(), auditor.clone()],
                execution_ready: true,
                workflow_fault: fault,
                lineage_phase: Arc::default(),
            },
            DeterministicEffects,
            DeterministicPublisher::default(),
        )
        .expect("configured product");
        product
            .register_candidate(
                &CommandContextV1 {
                    principal: developer,
                    idempotency_key: "register-topology-fault".to_string(),
                    now_ms: 100,
                },
                candidate(),
            )
            .expect("candidate commit");
        let result = product.read_public_lineage(
            &CommandContextV1 {
                principal: auditor,
                idempotency_key: "read-topology-fault".to_string(),
                now_ms: 101,
            },
            "tenant-a",
            "project-private-1",
        );
        assert!(matches!(result, Err(DeliveryError::CorruptStore(_))));
    }
}

#[test]
fn public_lineage_keeps_two_candidate_rework_history_reachable_from_project() {
    let temp = TempDir::new().expect("tempdir");
    let developer = principal("developer-private", AuthorityRole::Developer);
    let auditor = principal("auditor-private", AuthorityRole::Auditor);
    let product = ConfiguredDeliveryCore::open(
        &config(&temp),
        DeterministicIntegration {
            principals: vec![developer.clone(), auditor.clone()],
            execution_ready: true,
            workflow_fault: WorkflowFault::None,
            lineage_phase: Arc::default(),
        },
        DeterministicEffects,
        DeterministicPublisher::default(),
    )
    .expect("configured product");
    product
        .register_candidate(
            &CommandContextV1 {
                principal: developer.clone(),
                idempotency_key: "register-first-history-candidate".to_string(),
                now_ms: 100,
            },
            candidate(),
        )
        .expect("first candidate");
    let mut rework = candidate();
    rework.candidate_id = "candidate-private-rework".to_string();
    rework.source_digest = digest("rework-source");
    rework.candidate_digest = ContentDigest::zero();
    let rework = rework.seal().expect("rework candidate seal");
    product
        .register_candidate(
            &CommandContextV1 {
                principal: developer,
                idempotency_key: "register-rework-history-candidate".to_string(),
                now_ms: 101,
            },
            rework,
        )
        .expect("rework candidate");

    let lineage = product
        .read_public_lineage(
            &CommandContextV1 {
                principal: auditor,
                idempotency_key: "read-two-candidate-history".to_string(),
                now_ms: 102,
            },
            "tenant-a",
            "project-private-1",
        )
        .expect("complete public lineage");
    let node_ids = lineage
        .nodes
        .iter()
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    assert!(lineage
        .edges
        .iter()
        .all(|edge| node_ids.contains(&edge.from) && node_ids.contains(&edge.to)));
    let adjacency = lineage.edges.iter().fold(
        BTreeMap::<String, BTreeSet<String>>::new(),
        |mut graph, edge| {
            graph
                .entry(edge.from.clone())
                .or_default()
                .insert(edge.to.clone());
            graph
        },
    );
    let reachable = |start: &str| {
        let mut reached = BTreeSet::new();
        let mut pending = vec![start.to_string()];
        while let Some(current) = pending.pop() {
            if reached.insert(current.clone()) {
                pending.extend(adjacency.get(&current).into_iter().flatten().cloned());
            }
        }
        reached
    };
    let request = lineage
        .nodes
        .iter()
        .find(|node| node.stage == DeliveryLineageStageV1::CustomerRequest)
        .expect("request node");
    let project = lineage
        .nodes
        .iter()
        .find(|node| node.stage == DeliveryLineageStageV1::Project)
        .expect("project node");
    let candidates = lineage
        .nodes
        .iter()
        .filter(|node| node.stage == DeliveryLineageStageV1::Candidate)
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(candidates.len(), 2);
    assert_eq!(reachable(&request.id), node_ids);
    assert!(candidates.is_subset(&reachable(&project.id)));
}

#[test]
fn publisher_replay_after_crash_before_local_receipt_has_one_effective_event() {
    let temp = TempDir::new().expect("tempdir");
    let developer = principal("developer-private", AuthorityRole::Developer);
    let integration = DeterministicIntegration {
        principals: vec![developer.clone()],
        execution_ready: true,
        workflow_fault: WorkflowFault::None,
        lineage_phase: Arc::default(),
    };
    let publisher = DeterministicPublisher::default();

    let product = ConfiguredDeliveryCore::open(
        &config(&temp),
        integration.clone(),
        DeterministicEffects,
        publisher.clone(),
    )
    .expect("configured product");
    product
        .register_candidate(
            &CommandContextV1 {
                principal: developer,
                idempotency_key: "register-before-crash".to_string(),
                now_ms: 100,
            },
            candidate(),
        )
        .expect("candidate commit");
    let pending = product
        .pending_publications_test_only()
        .expect("pending outbox");
    assert_eq!(pending.len(), 1);

    let first_receipt = publisher
        .publish(&pending[0].request)
        .expect("external publish succeeds");
    assert_eq!(publisher.counts(), (1, 1));
    drop(product); // crash before mark_published commits the external receipt

    let restarted = ConfiguredDeliveryCore::open(
        &config(&temp),
        integration,
        DeterministicEffects,
        publisher.clone(),
    )
    .expect("restart");
    assert_eq!(restarted.publish_pending().expect("reconcile publish"), 1);
    assert_eq!(publisher.counts(), (2, 1));
    assert_eq!(
        publisher.0.lock().expect("publisher state").receipts.get(&(
            pending[0].request.operation_id.clone(),
            pending[0].request.request_digest.clone(),
        )),
        Some(&first_receipt),
    );
    assert_eq!(
        restarted
            .pending_publication_count()
            .expect("published outbox"),
        0
    );
}

#[test]
fn publication_fails_before_external_io_when_local_authority_is_corrupt() {
    let temp = TempDir::new().expect("tempdir");
    let developer = principal("developer-private", AuthorityRole::Developer);
    let integration = DeterministicIntegration {
        principals: vec![developer.clone()],
        execution_ready: true,
        workflow_fault: WorkflowFault::None,
        lineage_phase: Arc::default(),
    };
    let publisher = DeterministicPublisher::default();
    let product = ConfiguredDeliveryCore::open(
        &config(&temp),
        integration.clone(),
        DeterministicEffects,
        publisher.clone(),
    )
    .expect("configured product");
    product
        .register_candidate(
            &CommandContextV1 {
                principal: developer,
                idempotency_key: "register-before-publication-tamper".to_string(),
                now_ms: 100,
            },
            candidate(),
        )
        .expect("candidate commit");
    drop(product);

    let store = DeliveryStore::open(&config(&temp)).expect("open store for fault injection");
    let mut aggregate = store
        .load("tenant-a", "project-private-1")
        .expect("load aggregate")
        .expect("aggregate");
    aggregate
        .candidates
        .get_mut("candidate-private-1")
        .expect("candidate")
        .source_digest = digest("tampered-before-publication");
    store
        .replace_aggregate_test_only(&aggregate)
        .expect("inject corrupt aggregate");
    drop(store);

    let restarted = ConfiguredDeliveryCore::open(
        &config(&temp),
        integration,
        DeterministicEffects,
        publisher.clone(),
    );
    assert!(matches!(
        restarted,
        Err(DeliveryError::CorruptStore(_))
    ));
    assert_eq!(
        publisher.counts(),
        (0, 0),
        "corrupt local authority must fail before publisher I/O"
    );
}

#[test]
fn cross_record_reference_validator_rejects_every_required_missing_endpoint() {
    fn assert_corrupt(aggregate: &DeliveryAggregateV1, expected: &str) {
        let error = validate_delivery_aggregate_references(aggregate)
            .expect_err("missing required endpoint must fail closed");
        assert!(matches!(&error, DeliveryError::CorruptStore(_)));
        assert!(error.to_string().contains(expected), "{error}");
    }

    let mut missing_plan = DeliveryAggregateV1::new("tenant-a", "project-private-1");
    let candidate = candidate();
    let plan = plan_for(&candidate);
    missing_plan
        .qa_runs
        .insert("run-1".to_string(), run_for(&plan));
    assert_corrupt(&missing_plan, "QA run plan is missing");

    let mut missing_gate = DeliveryAggregateV1::new("tenant-a", "project-private-1");
    let mut run = run_for(&plan);
    run.gate_receipt = Some(reference("gate-1", 1));
    missing_gate
        .candidates
        .insert(candidate.candidate_id.clone(), candidate.clone());
    missing_gate
        .qa_plans
        .insert(plan.plan_id.clone(), plan.clone());
    missing_gate.qa_runs.insert(run.run_id.clone(), run);
    assert_corrupt(&missing_gate, "QA run gate is missing");

    let mut approval_outside_staged_review =
        DeliveryAggregateV1::new("tenant-a", "project-private-1");
    approval_outside_staged_review
        .candidates
        .insert(candidate.candidate_id.clone(), candidate.clone());
    approval_outside_staged_review.approvals.insert(
        "approval-without-gate".to_string(),
        ApprovalV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            approval_id: "approval-without-gate".to_string(),
            generation: 1,
            candidate: VersionedRefV1 {
                id: candidate.candidate_id.clone(),
                generation: candidate.generation,
                digest: candidate.candidate_digest.clone(),
            },
            gate: reference("gate-not-staged", 1),
            approver: principal("qa-private", AuthorityRole::Qa),
            policy_digest: digest("release-policy"),
            approved_at_ms: 1,
        },
    );
    assert_corrupt(
        &approval_outside_staged_review,
        "approval gate is missing outside staged QA review",
    );

    let manifest = manifest_for(&candidate);
    let mut missing_manifest_candidate = DeliveryAggregateV1::new("tenant-a", "project-private-1");
    missing_manifest_candidate
        .manifests
        .insert(manifest.manifest_id.clone(), manifest.clone());
    assert_corrupt(
        &missing_manifest_candidate,
        "release manifest candidate is missing",
    );

    let mut missing_manifest_gate = DeliveryAggregateV1::new("tenant-a", "project-private-1");
    missing_manifest_gate
        .candidates
        .insert(candidate.candidate_id.clone(), candidate.clone());
    missing_manifest_gate
        .manifests
        .insert(manifest.manifest_id.clone(), manifest);
    assert_corrupt(
        &missing_manifest_gate,
        "release manifest QA gate is missing",
    );

    let release_ref = reference("release-1", 1);
    let delivery = DeliveryReceiptV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        delivery_id: "delivery-1".to_string(),
        generation: 1,
        tenant_id: "tenant-a".to_string(),
        release: release_ref.clone(),
        customer_principal_id: "customer".to_string(),
        preview_digest: digest("preview"),
        preview_ttl_policy_version: DELIVERY_PREVIEW_TTL_POLICY_V1,
        receipt_digest: ContentDigest::zero(),
        state: DeliveryState::Delivered,
        issued_at_ms: 1,
        expires_at_ms: 100,
    }
    .seal()
    .expect("delivery seal");
    let mut missing_delivery_release = DeliveryAggregateV1::new("tenant-a", "project-private-1");
    missing_delivery_release
        .deliveries
        .insert(delivery.delivery_id.clone(), delivery.clone());
    assert_corrupt(&missing_delivery_release, "delivery release is missing");

    let acceptance = AcceptanceV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        acceptance_id: "acceptance-1".to_string(),
        generation: 1,
        delivery: VersionedRefV1 {
            id: delivery.delivery_id.clone(),
            generation: delivery.generation,
            digest: delivery.receipt_digest.clone(),
        },
        release: release_ref.clone(),
        customer: principal("customer", AuthorityRole::Customer),
        acceptance_digest: ContentDigest::zero(),
        accepted_at_ms: 2,
    }
    .seal()
    .expect("acceptance seal");
    let mut missing_acceptance_delivery = DeliveryAggregateV1::new("tenant-a", "project-private-1");
    missing_acceptance_delivery
        .acceptances
        .insert(acceptance.acceptance_id.clone(), acceptance.clone());
    assert_corrupt(
        &missing_acceptance_delivery,
        "acceptance delivery is missing",
    );

    let rollback = RollbackV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        rollback_id: "rollback-1".to_string(),
        generation: 1,
        from_release: reference("release-from", 1),
        to_release: reference("release-to", 1),
        actor: principal("release-manager", AuthorityRole::ReleaseManager),
        reason_digest: digest("rollback-reason"),
        effect_receipt: Some(reference("rollback-effect", 1)),
        created_at_ms: 3,
    };
    let mut missing_rollback_source = DeliveryAggregateV1::new("tenant-a", "project-private-1");
    missing_rollback_source
        .rollbacks
        .insert(rollback.rollback_id.clone(), rollback.clone());
    assert_corrupt(
        &missing_rollback_source,
        "rollback source release is missing",
    );
    let mut missing_rollback_target = DeliveryAggregateV1::new("tenant-a", "project-private-1");
    missing_rollback_target
        .releases
        .insert("release-from".to_string(), release("release-from"));
    missing_rollback_target
        .rollbacks
        .insert(rollback.rollback_id.clone(), rollback);
    assert_corrupt(
        &missing_rollback_target,
        "rollback target release is missing",
    );

    let closeout = ProjectCloseoutV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        closeout_id: "closeout-1".to_string(),
        generation: 1,
        project: reference("project-private-1", 1),
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
        memory_publication: Some(reference("memory", 1)),
        closed_by: principal("release-manager", AuthorityRole::ReleaseManager),
        created_at_ms: 4,
    };
    let mut missing_closeout_release = DeliveryAggregateV1::new("tenant-a", "project-private-1");
    missing_closeout_release
        .closeouts
        .insert(closeout.closeout_id.clone(), closeout);
    assert_corrupt(
        &missing_closeout_release,
        "closeout accepted release is missing",
    );
}

#[test]
fn health_and_lineage_reject_tampered_content_and_wrong_or_substituted_references() {
    let first = candidate();
    let mut aggregate = DeliveryAggregateV1::new("tenant-a", "project-private-1");
    aggregate
        .candidates
        .insert(first.candidate_id.clone(), first.clone());
    let mut wrong_digest_plan = plan_for(&first);
    wrong_digest_plan.candidate.digest = digest("wrong-candidate-digest");
    wrong_digest_plan.plan_digest = ContentDigest::zero();
    let wrong_digest_plan = wrong_digest_plan.seal().expect("reseal wrong-ref plan");
    aggregate
        .qa_plans
        .insert(wrong_digest_plan.plan_id.clone(), wrong_digest_plan);
    assert!(matches!(
        validate_delivery_aggregate_references(&aggregate),
        Err(DeliveryError::CorruptStore(_))
    ));

    let mut second = first.clone();
    second.candidate_id = "candidate-private-2".to_string();
    second.candidate_digest = ContentDigest::zero();
    let second = second.seal().expect("second candidate");
    let mut substituted = DeliveryAggregateV1::new("tenant-a", "project-private-1");
    substituted
        .candidates
        .insert(first.candidate_id.clone(), first.clone());
    substituted
        .candidates
        .insert(second.candidate_id.clone(), second.clone());
    let mut substituted_plan = plan_for(&first);
    substituted_plan.candidate.id = second.candidate_id.clone();
    substituted_plan.plan_digest = ContentDigest::zero();
    let substituted_plan = substituted_plan.seal().expect("substituted plan");
    substituted
        .qa_plans
        .insert(substituted_plan.plan_id.clone(), substituted_plan);
    assert!(matches!(
        validate_delivery_aggregate_references(&substituted),
        Err(DeliveryError::CorruptStore(_))
    ));

    let temp = TempDir::new().expect("tempdir");
    let developer = principal("developer-private", AuthorityRole::Developer);
    let auditor = principal("auditor-private", AuthorityRole::Auditor);
    let integration = DeterministicIntegration {
        principals: vec![developer.clone(), auditor.clone()],
        execution_ready: true,
        workflow_fault: WorkflowFault::None,
        lineage_phase: Arc::default(),
    };
    let product = ConfiguredDeliveryCore::open(
        &config(&temp),
        integration.clone(),
        DeterministicEffects,
        DeterministicPublisher::default(),
    )
    .expect("configured product");
    product
        .register_candidate(
            &CommandContextV1 {
                principal: developer,
                idempotency_key: "register-before-tamper".to_string(),
                now_ms: 100,
            },
            first,
        )
        .expect("candidate commit");
    drop(product);
    let store = DeliveryStore::open(&config(&temp)).expect("open store for fault injection");
    let mut persisted = store
        .load("tenant-a", "project-private-1")
        .expect("load aggregate")
        .expect("aggregate");
    let valid_persisted = persisted.clone();
    persisted
        .candidates
        .get_mut("candidate-private-1")
        .expect("candidate")
        .source_digest = digest("tampered-source");
    store
        .replace_aggregate_test_only(&persisted)
        .expect("inject corrupt aggregate");
    assert!(matches!(
        store.health(),
        Err(DeliveryError::CorruptStore(_))
    ));
    store
        .replace_aggregate_test_only(&valid_persisted)
        .expect("restore valid aggregate");
    store.health().expect("restored aggregate is healthy");
    store
        .replace_aggregate_test_only(&substituted)
        .expect("inject cross-record substitution");
    assert!(matches!(
        store.health(),
        Err(DeliveryError::CorruptStore(_))
    ));
    drop(store);
    let reopened = ConfiguredDeliveryCore::open(
        &config(&temp),
        DeterministicIntegration {
            principals: vec![auditor],
            execution_ready: true,
            workflow_fault: WorkflowFault::None,
            lineage_phase: Arc::default(),
        },
        DeterministicEffects,
        DeterministicPublisher::default(),
    );
    assert!(matches!(
        reopened,
        Err(DeliveryError::CorruptStore(_))
    ));
}

#[test]
fn customer_lineage_access_is_bound_to_its_delivery_membership() {
    let temp = TempDir::new().expect("tempdir");
    let customer = principal("customer-private", AuthorityRole::Customer);
    let developer = principal("developer-private", AuthorityRole::Developer);
    let product = ConfiguredDeliveryCore::open(
        &config(&temp),
        DeterministicIntegration {
            principals: vec![customer.clone(), developer.clone()],
            execution_ready: true,
            workflow_fault: WorkflowFault::None,
            lineage_phase: Arc::default(),
        },
        DeterministicEffects,
        DeterministicPublisher::default(),
    )
    .expect("configured product");
    product
        .register_candidate(
            &CommandContextV1 {
                principal: developer,
                idempotency_key: "register-customer-project".to_string(),
                now_ms: 100,
            },
            candidate(),
        )
        .expect("candidate commit");

    let error = product
        .read_public_lineage(
            &CommandContextV1 {
                principal: customer,
                idempotency_key: "read-customer".to_string(),
                now_ms: 101,
            },
            "tenant-a",
            "project-private-1",
        )
        .expect_err("customer without a delivery remains fail-closed");
    assert!(matches!(error, DeliveryError::AuthorityDenied(_)));
}

#[test]
fn candidate_write_rejects_unsupported_currency_without_state_or_outbox_mutation() {
    let temp = TempDir::new().expect("tempdir");
    let developer = principal("developer-private", AuthorityRole::Developer);
    let product = ConfiguredDeliveryCore::open(
        &config(&temp),
        DeterministicIntegration {
            principals: vec![developer.clone()],
            execution_ready: true,
            workflow_fault: WorkflowFault::None,
            lineage_phase: Arc::default(),
        },
        DeterministicEffects,
        DeterministicPublisher::default(),
    )
    .expect("configured product");
    let mut unsupported = candidate();
    unsupported.cost.currency = "JPY".to_string();
    unsupported.candidate_digest = ContentDigest::zero();
    let unsupported = unsupported.seal().expect("reseal candidate");
    assert_eq!(
        product.pending_publication_count().expect("empty outbox"),
        0
    );
    let error = product
        .register_candidate(
            &CommandContextV1 {
                principal: developer,
                idempotency_key: "register-jpy".to_string(),
                now_ms: 100,
            },
            unsupported,
        )
        .expect_err("unsupported currency must be rejected before commit");
    assert!(matches!(error, DeliveryError::Validation(_)));
    assert_eq!(
        product
            .pending_publication_count()
            .expect("unchanged outbox"),
        0
    );
    drop(product);
    let store = DeliveryStore::open(&config(&temp)).expect("reopen unchanged store");
    assert!(store
        .load("tenant-a", "project-private-1")
        .expect("aggregate read")
        .is_none());
}
