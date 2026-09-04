use std::collections::{BTreeMap, BTreeSet};

use sentinel_workflow::{
    filtered_collaboration_view, AgentId, AuthenticatedCompanyPrincipalV1, BehaviorMandateV1,
    BlockerKindV1, BlockerStateV1, ClaimExposureStateV1, CollaborationAuthorityFenceV1,
    CollaborationBudgetV1, CollaborationModeV1, CollaborationParticipantV1,
    CollaborationSessionStateV1, CompanyPrincipalKindV1, CompanyRoleV1, CompanyWorkItemSpecV1,
    CompanyWorkStateV1, CompanyWorkflowCommandV1, CompanyWorkflowResponseV1,
    CostReservationStateV1, EvidenceReferenceV1, HandoffConsumptionKindV1, HandoffGapClassV1,
    HandoffPacketStateV1, ParticipantBindingV1, ProjectId, ProjectLifecycleStateV1,
    ProposalBindingV1, ProposalGovernanceV1, QualityGateBindingV1, QualityGateReceiptBindingV1,
    TenantId, UncertaintyClassV1, WorkInputContractV1, WorkItemId, WorkOutputContractV1,
    WorkOutputReceiptV1, WorkProfileBindingV1, WorkTransitionReceiptV1, WorkflowErrorCode,
    WorkflowStore, COMPANY_DOMAIN_SCHEMA_VERSION,
};
use tempfile::TempDir;
use uuid::Uuid;

const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OTHER_DIGEST: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

struct Journey {
    _temp: TempDir,
    store: WorkflowStore,
    customer: AuthenticatedCompanyPrincipalV1,
    pm: AuthenticatedCompanyPrincipalV1,
    developer: AuthenticatedCompanyPrincipalV1,
    junior_developer: AuthenticatedCompanyPrincipalV1,
    qa: AuthenticatedCompanyPrincipalV1,
    request_id: String,
    proposal_id: String,
    proposal_digest: String,
    project_id: ProjectId,
}

fn principal(
    tenant: &str,
    name: &str,
    kind: CompanyPrincipalKindV1,
    role: CompanyRoleV1,
    customer_id: Option<&str>,
    agent_id: Option<u16>,
) -> AuthenticatedCompanyPrincipalV1 {
    AuthenticatedCompanyPrincipalV1 {
        schema_version: COMPANY_DOMAIN_SCHEMA_VERSION,
        tenant_id: TenantId::parse(tenant).unwrap(),
        principal_id: name.to_owned(),
        kind,
        role,
        customer_id: customer_id.map(str::to_owned),
        agent_id: agent_id.map(AgentId),
        authority_generation: 1,
        authority_digest: DIGEST.to_owned(),
    }
}

fn profile(id: &str) -> WorkProfileBindingV1 {
    WorkProfileBindingV1 {
        profile_id: id.to_owned(),
        generation: 1,
        digest: DIGEST.to_owned(),
    }
}

fn participant(
    agent_id: u16,
    role: CompanyRoleV1,
    specialties: &[&str],
    reports_to: Option<u16>,
    profile_id: &str,
) -> ParticipantBindingV1 {
    ParticipantBindingV1 {
        agent_id: AgentId(agent_id),
        principal_id: match agent_id {
            1 => "pm-a",
            2 => "developer-a",
            3 => "qa-a",
            4 => "developer-b",
            value => panic!("missing principal fixture for agent {value}"),
        }
        .to_owned(),
        role,
        specialties: specialties
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        reports_to: reports_to.map(AgentId),
        profile: profile(profile_id),
    }
}

fn binding() -> ProposalBindingV1 {
    ProposalBindingV1 {
        scope: "bounded-web-delivery".to_owned(),
        deliverables: vec!["web-application".to_owned()],
        exclusions: vec!["provider-spend".to_owned()],
        acceptance_criteria: vec!["independent-qa".to_owned()],
        assumptions: vec!["single-node".to_owned()],
        cost_ceiling_micros: 1_000,
        provider_cost_ceilings_micros: BTreeMap::from([
            ("local".to_owned(), 700),
            ("local-loop".to_owned(), 700),
            ("review".to_owned(), 300),
        ]),
        governance: ProposalGovernanceV1 {
            owner: AgentId(1),
            participants: vec![
                participant(
                    1,
                    CompanyRoleV1::ProjectManager,
                    &["coordination"],
                    None,
                    "pm-v1",
                ),
                participant(
                    2,
                    CompanyRoleV1::Developer,
                    &["rust", "web"],
                    Some(1),
                    "developer-v1",
                ),
                participant(
                    4,
                    CompanyRoleV1::Developer,
                    &["rust", "web"],
                    Some(2),
                    "developer-v1",
                ),
                participant(3, CompanyRoleV1::Qa, &["qa", "web"], Some(1), "qa-v1"),
            ],
            project_profile: profile("project-web-v1"),
        },
        expires_at_unix_ms: 10_000,
    }
}

fn command(
    store: &WorkflowStore,
    actor: &AuthenticatedCompanyPrincipalV1,
    id: u128,
    command: CompanyWorkflowCommandV1,
    now: u64,
) -> CompanyWorkflowResponseV1 {
    store
        .apply_company_command(actor, Uuid::from_u128(id), &command, now)
        .unwrap()
        .response
}

fn journey() -> Journey {
    let temp = TempDir::new().unwrap();
    let store = WorkflowStore::open(temp.path().join("workflow.sqlite")).unwrap();
    let customer = principal(
        "tenant-a",
        "customer-a",
        CompanyPrincipalKindV1::Customer,
        CompanyRoleV1::Customer,
        Some("customer-a"),
        None,
    );
    let sales = principal(
        "tenant-a",
        "sales-a",
        CompanyPrincipalKindV1::Agent,
        CompanyRoleV1::Sales,
        None,
        Some(10),
    );
    let pm = principal(
        "tenant-a",
        "pm-a",
        CompanyPrincipalKindV1::Agent,
        CompanyRoleV1::ProjectManager,
        None,
        Some(1),
    );
    let developer = principal(
        "tenant-a",
        "developer-a",
        CompanyPrincipalKindV1::Agent,
        CompanyRoleV1::Developer,
        None,
        Some(2),
    );
    let junior_developer = principal(
        "tenant-a",
        "developer-b",
        CompanyPrincipalKindV1::Agent,
        CompanyRoleV1::Developer,
        None,
        Some(4),
    );
    let qa = principal(
        "tenant-a",
        "qa-a",
        CompanyPrincipalKindV1::Agent,
        CompanyRoleV1::Qa,
        None,
        Some(3),
    );
    let request = command(
        &store,
        &customer,
        1,
        CompanyWorkflowCommandV1::SubmitCustomerRequest {
            summary_ref: "request-summary".to_owned(),
            desired_outcome: "working-product".to_owned(),
            constraints: vec!["token-free".to_owned()],
        },
        1,
    );
    let CompanyWorkflowResponseV1::CustomerRequest(request) = request else {
        panic!()
    };
    let request_id = request.request_id;
    command(
        &store,
        &sales,
        2,
        CompanyWorkflowCommandV1::ClarifyCustomerRequest {
            request_id: request_id.clone(),
            expected_version: 1,
            question_ref: "supported-browser".to_owned(),
            answer_ref: "current-stable".to_owned(),
        },
        2,
    );
    command(
        &store,
        &sales,
        3,
        CompanyWorkflowCommandV1::QualifyCustomerRequest {
            request_id: request_id.clone(),
            expected_version: 2,
            reason_ref: "bounded-scope".to_owned(),
        },
        3,
    );
    let proposal = command(
        &store,
        &sales,
        4,
        CompanyWorkflowCommandV1::CreateProposal {
            request_id: request_id.clone(),
            expected_version: 3,
            binding: binding(),
        },
        4,
    );
    let CompanyWorkflowResponseV1::Proposal(proposal) = proposal else {
        panic!()
    };
    let proposal_id = proposal.proposal_id;
    let proposal_digest = proposal.proposal_digest;
    let accepted = command(
        &store,
        &customer,
        5,
        CompanyWorkflowCommandV1::AcceptProposal {
            request_id: request_id.clone(),
            expected_version: 4,
            proposal_id: proposal_id.clone(),
            proposal_digest: proposal_digest.clone(),
        },
        5,
    );
    let CompanyWorkflowResponseV1::AgreementProject { project, .. } = accepted else {
        panic!()
    };
    Journey {
        _temp: temp,
        store,
        customer,
        pm,
        developer,
        junior_developer,
        qa,
        request_id,
        proposal_id,
        proposal_digest,
        project_id: project.project_id,
    }
}

fn work(
    id: &str,
    role: CompanyRoleV1,
    specialties: &[&str],
    dependencies: &[&str],
    budget: u64,
) -> CompanyWorkItemSpecV1 {
    CompanyWorkItemSpecV1 {
        work_item_id: WorkItemId::parse(id).unwrap(),
        title: format!("title-{id}"),
        objective: format!("objective-{id}"),
        required_role: role,
        required_specialties: specialties
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        dependency_ids: dependencies
            .iter()
            .map(|value| WorkItemId::parse(*value).unwrap())
            .collect(),
        owner: if role == CompanyRoleV1::Qa {
            AgentId(3)
        } else {
            AgentId(2)
        },
        inputs: dependencies
            .iter()
            .map(|dependency| WorkInputContractV1 {
                name: format!("input-{dependency}"),
                producer_work_item_id: WorkItemId::parse(*dependency).unwrap(),
                producer_output_name: "result".to_owned(),
                expected_contract_generation: 1,
                expected_contract_digest: DIGEST.to_owned(),
            })
            .collect(),
        outputs: vec![WorkOutputContractV1 {
            name: "result".to_owned(),
            media_type: "application/octet-stream".to_owned(),
            digest_algorithm: "sha256".to_owned(),
            contract_generation: 1,
            contract_digest: DIGEST.to_owned(),
        }],
        quality_gate: QualityGateBindingV1 {
            gate_id: "web-work-item-qa-v1".to_owned(),
            generation: 1,
            digest: DIGEST.to_owned(),
        },
        budget_micros: budget,
        rework: None,
    }
}

fn output_receipt() -> Vec<WorkOutputReceiptV1> {
    vec![WorkOutputReceiptV1 {
        name: "result".to_owned(),
        contract_generation: 1,
        contract_digest: DIGEST.to_owned(),
        content_digest: OTHER_DIGEST.to_owned(),
    }]
}

fn project_command(
    store: &WorkflowStore,
    actor: &AuthenticatedCompanyPrincipalV1,
    id: u128,
    command_value: CompanyWorkflowCommandV1,
    now: u64,
) -> sentinel_workflow::ProjectV1 {
    let CompanyWorkflowResponseV1::Project(project) = command(store, actor, id, command_value, now)
    else {
        panic!("expected project response")
    };
    *project
}

fn collaboration_authority(
    project: &sentinel_workflow::ProjectV1,
    work_item_id: &WorkItemId,
) -> CollaborationAuthorityFenceV1 {
    let assignment = project.work_items[work_item_id]
        .assignments
        .iter()
        .find(|assignment| assignment.active)
        .unwrap();
    CollaborationAuthorityFenceV1 {
        organization_generation: 1,
        organization_digest: DIGEST.to_owned(),
        assignment_id: assignment.assignment_id.clone(),
        assignment_version: assignment.assignment_version,
        assignment_digest: assignment.canonical_digest().unwrap(),
        policy_version: 1,
        policy_digest: DIGEST.to_owned(),
    }
}

fn collaboration_participant(
    agent_id: u16,
    role: CompanyRoleV1,
    mandate: BehaviorMandateV1,
    capability: &str,
) -> CollaborationParticipantV1 {
    CollaborationParticipantV1 {
        agent_id: AgentId(agent_id),
        permanent_role: role,
        mandate,
        capability_snapshot_digest: match agent_id {
            1 | 3 => DIGEST,
            _ => OTHER_DIGEST,
        }
        .to_owned(),
        capabilities: BTreeSet::from([capability.to_owned()]),
        privacy_classes: BTreeSet::from(["project-internal".to_owned()]),
    }
}

fn evidence(reference: &str) -> Vec<EvidenceReferenceV1> {
    vec![EvidenceReferenceV1 {
        reference: reference.to_owned(),
        digest: DIGEST.to_owned(),
    }]
}

fn transition(
    project_id: &ProjectId,
    project_version: u64,
    work_id: &str,
    work_version: u64,
    assignment_version: u64,
    from: CompanyWorkStateV1,
    to: CompanyWorkStateV1,
    outputs: Vec<WorkOutputReceiptV1>,
    gate: Option<QualityGateReceiptBindingV1>,
    now: u64,
) -> CompanyWorkflowCommandV1 {
    CompanyWorkflowCommandV1::ApplyWorkTransition {
        project_id: project_id.clone(),
        expected_version: project_version,
        receipt: WorkTransitionReceiptV1 {
            schema_version: COMPANY_DOMAIN_SCHEMA_VERSION,
            project_id: project_id.clone(),
            work_item_id: WorkItemId::parse(work_id).unwrap(),
            expected_project_version: project_version,
            expected_work_version: work_version,
            expected_assignment_version: assignment_version,
            from_state: from,
            to_state: to,
            output_receipts: outputs,
            gate_receipt: gate,
            phase_a_evidence_digest: DIGEST.to_owned(),
            reason_ref: "verified-transition".to_owned(),
            occurred_at_unix_ms: now,
        },
    }
}

#[test]
fn customer_lifecycle_binds_tenant_version_and_exact_proposal_digest() {
    let state = journey();
    let request = state
        .store
        .company_customer_request(&TenantId::parse("tenant-a").unwrap(), &state.request_id)
        .unwrap()
        .unwrap();
    assert_eq!(request.version, 5);

    let foreign_customer = principal(
        "tenant-a",
        "customer-b",
        CompanyPrincipalKindV1::Customer,
        CompanyRoleV1::Customer,
        Some("customer-b"),
        None,
    );
    let error = state
        .store
        .apply_company_command(
            &foreign_customer,
            Uuid::from_u128(20),
            &CompanyWorkflowCommandV1::RecordCustomerFeedback {
                request_id: state.request_id.clone(),
                expected_version: 5,
                feedback_ref: "spoof".to_owned(),
            },
            20,
        )
        .unwrap_err();
    assert_eq!(error.code, WorkflowErrorCode::AuthorityConflict);

    let stale = state
        .store
        .apply_company_command(
            &state.customer,
            Uuid::from_u128(21),
            &CompanyWorkflowCommandV1::RecordCustomerFeedback {
                request_id: state.request_id.clone(),
                expected_version: 4,
                feedback_ref: "stale".to_owned(),
            },
            21,
        )
        .unwrap_err();
    assert_eq!(stale.code, WorkflowErrorCode::VersionConflict);

    let tenant_b = principal(
        "tenant-b",
        "customer-b",
        CompanyPrincipalKindV1::Customer,
        CompanyRoleV1::Customer,
        Some("customer-b"),
        None,
    );
    let missing = state
        .store
        .apply_company_command(
            &tenant_b,
            Uuid::from_u128(22),
            &CompanyWorkflowCommandV1::RecordCustomerFeedback {
                request_id: state.request_id,
                expected_version: 5,
                feedback_ref: "foreign".to_owned(),
            },
            22,
        )
        .unwrap_err();
    assert_eq!(missing.code, WorkflowErrorCode::NotFound);

    assert!(!state.proposal_id.is_empty());
    assert_eq!(state.proposal_digest.len(), 64);
}

#[test]
fn work_graph_rejects_cycles_and_budget_overflow_without_project_mutation() {
    let state = journey();
    let cycle = state
        .store
        .apply_company_command(
            &state.pm,
            Uuid::from_u128(30),
            &CompanyWorkflowCommandV1::PlanWorkGraph {
                project_id: state.project_id.clone(),
                expected_version: 1,
                items: vec![
                    work(
                        "work-a",
                        CompanyRoleV1::Developer,
                        &["rust"],
                        &["work-b"],
                        100,
                    ),
                    work(
                        "work-b",
                        CompanyRoleV1::Developer,
                        &["rust"],
                        &["work-a"],
                        100,
                    ),
                ],
            },
            30,
        )
        .unwrap_err();
    assert_eq!(cycle.code, WorkflowErrorCode::InvalidInput);

    let overflow = state
        .store
        .apply_company_command(
            &state.pm,
            Uuid::from_u128(31),
            &CompanyWorkflowCommandV1::PlanWorkGraph {
                project_id: state.project_id.clone(),
                expected_version: 1,
                items: vec![work(
                    "work-a",
                    CompanyRoleV1::Developer,
                    &["rust"],
                    &[],
                    1_001,
                )],
            },
            31,
        )
        .unwrap_err();
    assert_eq!(overflow.code, WorkflowErrorCode::InvalidInput);
    assert_eq!(
        state
            .store
            .company_project(&TenantId::parse("tenant-a").unwrap(), &state.project_id)
            .unwrap()
            .unwrap()
            .version,
        1
    );
}

#[test]
fn assignment_is_profile_bound_and_self_qa_is_rejected() {
    let state = journey();
    let project = command(
        &state.store,
        &state.pm,
        40,
        CompanyWorkflowCommandV1::PlanWorkGraph {
            project_id: state.project_id.clone(),
            expected_version: 1,
            items: vec![work("qa-work", CompanyRoleV1::Qa, &["qa"], &[], 100)],
        },
        40,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.pm,
        401,
        CompanyWorkflowCommandV1::ActivateProject {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            reason_ref: "plan-approved".to_owned(),
        },
        41,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.pm,
        41,
        CompanyWorkflowCommandV1::AssignWork {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: WorkItemId::parse("qa-work").unwrap(),
            agent_id: AgentId(3),
            organization_generation: 7,
            organization_digest: OTHER_DIGEST.to_owned(),
            reason_ref: "qa-owner".to_owned(),
        },
        41,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let assigned = &project.work_items[&WorkItemId::parse("qa-work").unwrap()];
    assert_eq!(assigned.state, CompanyWorkStateV1::Assigned);
    assert_eq!(
        assigned.assignments.last().unwrap().profile.profile_id,
        "qa-v1"
    );

    let error = state
        .store
        .apply_company_command(
            &state.qa,
            Uuid::from_u128(42),
            &CompanyWorkflowCommandV1::RecordApproval {
                project_id: state.project_id,
                expected_version: project.version,
                work_item_id: WorkItemId::parse("qa-work").unwrap(),
                subject_digest: DIGEST.to_owned(),
                approved: true,
            },
            42,
        )
        .unwrap_err();
    assert_eq!(error.code, WorkflowErrorCode::AuthorityConflict);
}

#[test]
fn budget_reservation_and_commit_are_bounded_and_idempotent() {
    let state = journey();
    let project = command(
        &state.store,
        &state.pm,
        49,
        CompanyWorkflowCommandV1::PlanWorkGraph {
            project_id: state.project_id.clone(),
            expected_version: 1,
            items: vec![work(
                "budget-work",
                CompanyRoleV1::Developer,
                &["rust"],
                &[],
                100,
            )],
        },
        49,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.pm,
        491,
        CompanyWorkflowCommandV1::ActivateProject {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            reason_ref: "approved-plan".to_owned(),
        },
        50,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.pm,
        50,
        CompanyWorkflowCommandV1::ReserveCost {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: None,
            provider: "local".to_owned(),
            amount_micros: 600,
        },
        50,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let reservation_id = project.reservations[0].reservation_id.clone();
    let overflow = state
        .store
        .apply_company_command(
            &state.pm,
            Uuid::from_u128(51),
            &CompanyWorkflowCommandV1::ReserveCost {
                project_id: state.project_id.clone(),
                expected_version: project.version,
                work_item_id: None,
                provider: "local".to_owned(),
                amount_micros: 101,
            },
            51,
        )
        .unwrap_err();
    assert_eq!(overflow.code, WorkflowErrorCode::InvalidInput);
    let committed = command(
        &state.store,
        &state.pm,
        52,
        CompanyWorkflowCommandV1::CommitCost {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            reservation_id,
            actual_micros: 550,
            usage_event_operation_id: Some("llm_usage_request-52".to_owned()),
        },
        52,
    );
    let CompanyWorkflowResponseV1::Project(committed) = committed else {
        panic!()
    };
    assert_eq!(committed.committed_cost_micros, 550);
    assert_eq!(
        committed.reservations[0]
            .usage_event_operation_id
            .as_deref(),
        Some("llm_usage_request-52")
    );

    let second = command(
        &state.store,
        &state.pm,
        53,
        CompanyWorkflowCommandV1::ReserveCost {
            project_id: state.project_id.clone(),
            expected_version: committed.version,
            work_item_id: None,
            provider: "local".to_owned(),
            amount_micros: 50,
        },
        53,
    );
    let CompanyWorkflowResponseV1::Project(second) = second else {
        panic!()
    };
    let duplicate = state
        .store
        .apply_company_command(
            &state.pm,
            Uuid::from_u128(54),
            &CompanyWorkflowCommandV1::CommitCost {
                project_id: state.project_id.clone(),
                expected_version: second.version,
                reservation_id: second.reservations[1].reservation_id.clone(),
                actual_micros: 50,
                usage_event_operation_id: Some("llm_usage_request-52".to_owned()),
            },
            54,
        )
        .unwrap_err();
    assert_eq!(duplicate.code, WorkflowErrorCode::InvalidInput);
}

#[test]
fn operation_replay_survives_reopen_and_changed_request_conflicts() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("workflow.sqlite");
    let customer = principal(
        "tenant-a",
        "customer-a",
        CompanyPrincipalKindV1::Customer,
        CompanyRoleV1::Customer,
        Some("customer-a"),
        None,
    );
    let operation_id = Uuid::from_u128(60);
    let original = CompanyWorkflowCommandV1::SubmitCustomerRequest {
        summary_ref: "summary".to_owned(),
        desired_outcome: "outcome".to_owned(),
        constraints: Vec::new(),
    };
    let first = WorkflowStore::open(&path)
        .unwrap()
        .apply_company_command(&customer, operation_id, &original, 60)
        .unwrap();
    assert!(!first.replayed);
    let reopened = WorkflowStore::open(&path).unwrap();
    let replay = reopened
        .apply_company_command(&customer, operation_id, &original, 61)
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(first.response, replay.response);
    let conflict = reopened
        .apply_company_command(
            &customer,
            operation_id,
            &CompanyWorkflowCommandV1::SubmitCustomerRequest {
                summary_ref: "changed".to_owned(),
                desired_outcome: "outcome".to_owned(),
                constraints: Vec::new(),
            },
            62,
        )
        .unwrap_err();
    assert_eq!(conflict.code, WorkflowErrorCode::IdempotencyConflict);
}

#[test]
fn operation_replay_returns_the_sealed_response_after_later_mutations() {
    let state = journey();
    let original = CompanyWorkflowCommandV1::SubmitCustomerRequest {
        summary_ref: "request-summary".to_owned(),
        desired_outcome: "working-product".to_owned(),
        constraints: vec!["token-free".to_owned()],
    };
    let replay = state
        .store
        .apply_company_command(&state.customer, Uuid::from_u128(1), &original, 100)
        .unwrap();
    assert!(replay.replayed);
    let CompanyWorkflowResponseV1::CustomerRequest(request) = replay.response else {
        panic!()
    };
    assert_eq!(request.version, 1);
    assert_eq!(
        state
            .store
            .company_customer_request(&TenantId::parse("tenant-a").unwrap(), &state.request_id)
            .unwrap()
            .unwrap()
            .version,
        5
    );
}

#[test]
fn append_only_project_snapshots_rebuild_the_independent_projection() {
    let state = journey();
    command(
        &state.store,
        &state.pm,
        70,
        CompanyWorkflowCommandV1::PlanWorkGraph {
            project_id: state.project_id.clone(),
            expected_version: 1,
            items: vec![work(
                "work-a",
                CompanyRoleV1::Developer,
                &["rust"],
                &[],
                100,
            )],
        },
        70,
    );
    command(
        &state.store,
        &state.pm,
        71,
        CompanyWorkflowCommandV1::RecordDecision {
            project_id: state.project_id.clone(),
            expected_version: 2,
            work_item_id: None,
            choice_ref: "ship-v1".to_owned(),
            rationale_ref: "bounded-m0".to_owned(),
        },
        71,
    );
    let before = state
        .store
        .company_project_projection(&TenantId::parse("tenant-a").unwrap(), &state.project_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        state.store.rebuild_company_project_projections().unwrap(),
        1
    );
    let after = state
        .store
        .company_project_projection(&TenantId::parse("tenant-a").unwrap(), &state.project_id)
        .unwrap()
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(after.project.decisions.len(), 1);
}

#[test]
fn handoffs_blockers_rooms_questions_and_actions_keep_project_authority() {
    let state = journey();
    let project = command(
        &state.store,
        &state.pm,
        80,
        CompanyWorkflowCommandV1::PlanWorkGraph {
            project_id: state.project_id.clone(),
            expected_version: 1,
            items: vec![work(
                "work-a",
                CompanyRoleV1::Developer,
                &["rust"],
                &[],
                100,
            )],
        },
        80,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.pm,
        801,
        CompanyWorkflowCommandV1::ActivateProject {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            reason_ref: "plan-approved".to_owned(),
        },
        81,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.pm,
        81,
        CompanyWorkflowCommandV1::AssignWork {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: WorkItemId::parse("work-a").unwrap(),
            agent_id: AgentId(2),
            organization_generation: 1,
            organization_digest: DIGEST.to_owned(),
            reason_ref: "developer-owner".to_owned(),
        },
        81,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.developer,
        82,
        CompanyWorkflowCommandV1::CreateHandoff {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: WorkItemId::parse("work-a").unwrap(),
            consumer: AgentId(3),
            artifact_digests: BTreeSet::from([DIGEST.to_owned()]),
            reason_ref: "qa-review".to_owned(),
        },
        82,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let handoff_id = project.handoffs[0].handoff_id.clone();
    let project = command(
        &state.store,
        &state.pm,
        83,
        CompanyWorkflowCommandV1::RaiseBlocker {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: Some(WorkItemId::parse("work-a").unwrap()),
            cause_ref: "dependency-unavailable".to_owned(),
            owner: AgentId(2),
        },
        83,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let blocker_id = project.blockers[0].blocker_id.clone();
    let project = command(
        &state.store,
        &state.pm,
        84,
        CompanyWorkflowCommandV1::CreateProjectRoom {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            kind: sentinel_workflow::ProjectRoomKindV1::Team,
            members: vec![AgentId(1), AgentId(2)],
        },
        84,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.pm,
        85,
        CompanyWorkflowCommandV1::RecordQuestion {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: None,
            owner: AgentId(1),
            question_ref: "release-window".to_owned(),
        },
        85,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let question_id = project.questions[0].question_id.clone();
    let project = command(
        &state.store,
        &state.pm,
        86,
        CompanyWorkflowCommandV1::RecordAction {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: None,
            owner: AgentId(2),
            action_ref: "prepare-artifact".to_owned(),
        },
        86,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let action_id = project.actions[0].action_id.clone();
    let project = command(
        &state.store,
        &state.qa,
        87,
        CompanyWorkflowCommandV1::AcknowledgeHandoff {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            handoff_id,
            accepted: true,
            reason_ref: "accepted-for-review".to_owned(),
        },
        87,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.developer,
        88,
        CompanyWorkflowCommandV1::ResolveBlocker {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            blocker_id,
            resolution_ref: "dependency-restored".to_owned(),
        },
        88,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.pm,
        89,
        CompanyWorkflowCommandV1::ResolveQuestion {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            question_id,
            resolution_ref: "next-window".to_owned(),
        },
        89,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.developer,
        90,
        CompanyWorkflowCommandV1::ResolveAction {
            project_id: state.project_id,
            expected_version: project.version,
            action_id,
            resolution_ref: "artifact-ready".to_owned(),
        },
        90,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    assert_eq!(
        (
            project.handoffs.len(),
            project.blockers.len(),
            project.rooms.len(),
            project.questions.len(),
            project.actions.len()
        ),
        (1, 1, 1, 1, 1)
    );
    assert_eq!(
        project.handoffs[0].state,
        sentinel_workflow::HandoffStateV1::Accepted
    );
    assert_eq!(
        project.blockers[0].state,
        sentinel_workflow::BlockerStateV1::Resolved
    );
    assert_eq!(
        project.questions[0].resolution_ref.as_deref(),
        Some("next-window")
    );
    assert!(project.actions[0].completed);
}

#[test]
fn resolving_a_pre_activation_blocker_restores_explicit_planning_authority() {
    let state = journey();
    let project = command(
        &state.store,
        &state.pm,
        800,
        CompanyWorkflowCommandV1::PlanWorkGraph {
            project_id: state.project_id.clone(),
            expected_version: 1,
            items: vec![work(
                "work-a",
                CompanyRoleV1::Developer,
                &["rust"],
                &[],
                100,
            )],
        },
        800,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.pm,
        801,
        CompanyWorkflowCommandV1::RaiseBlocker {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: None,
            cause_ref: "pre-activation-review".to_owned(),
            owner: AgentId(2),
        },
        801,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    assert_eq!(project.lifecycle_state, ProjectLifecycleStateV1::Blocked);
    let blocker_id = project.blockers[0].blocker_id.clone();
    let project = command(
        &state.store,
        &state.pm,
        802,
        CompanyWorkflowCommandV1::EscalateBlocker {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            blocker_id: blocker_id.clone(),
            escalation_target: AgentId(1),
            reason_ref: "manager-decision-required".to_owned(),
        },
        802,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.pm,
        803,
        CompanyWorkflowCommandV1::ResolveBlocker {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            blocker_id,
            resolution_ref: "review-approved".to_owned(),
        },
        803,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    assert_eq!(project.lifecycle_state, ProjectLifecycleStateV1::Planning);
    let project = command(
        &state.store,
        &state.pm,
        804,
        CompanyWorkflowCommandV1::ActivateProject {
            project_id: state.project_id,
            expected_version: project.version,
            reason_ref: "governance-complete".to_owned(),
        },
        804,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    assert_eq!(project.lifecycle_state, ProjectLifecycleStateV1::Active);
}

#[test]
fn every_project_mutation_rejects_unbound_or_spoofed_principals_before_state_change() {
    let state = journey();
    let before = state
        .store
        .company_project(&TenantId::parse("tenant-a").unwrap(), &state.project_id)
        .unwrap()
        .unwrap();
    let actors = [
        state.customer.clone(),
        principal(
            "tenant-a",
            "operator-a",
            CompanyPrincipalKindV1::Operator,
            CompanyRoleV1::ProjectManager,
            None,
            None,
        ),
        principal(
            "tenant-a",
            "unknown-agent",
            CompanyPrincipalKindV1::Agent,
            CompanyRoleV1::ProjectManager,
            None,
            Some(60),
        ),
        principal(
            "tenant-a",
            "spoofed-pm",
            CompanyPrincipalKindV1::Agent,
            CompanyRoleV1::Developer,
            None,
            Some(1),
        ),
    ];
    for (offset, actor) in actors.iter().enumerate() {
        let error = state
            .store
            .apply_company_command(
                actor,
                Uuid::from_u128(100 + offset as u128),
                &CompanyWorkflowCommandV1::RecordDecision {
                    project_id: state.project_id.clone(),
                    expected_version: before.version,
                    work_item_id: None,
                    choice_ref: "forbidden".to_owned(),
                    rationale_ref: "actor-not-bound".to_owned(),
                },
                100 + offset as u64,
            )
            .unwrap_err();
        assert_eq!(error.code, WorkflowErrorCode::AuthorityConflict);
    }
    let foreign = principal(
        "tenant-b",
        "pm-a",
        CompanyPrincipalKindV1::Agent,
        CompanyRoleV1::ProjectManager,
        None,
        Some(1),
    );
    let error = state
        .store
        .apply_company_command(
            &foreign,
            Uuid::from_u128(105),
            &CompanyWorkflowCommandV1::RecordDecision {
                project_id: state.project_id.clone(),
                expected_version: before.version,
                work_item_id: None,
                choice_ref: "foreign".to_owned(),
                rationale_ref: "cross-tenant".to_owned(),
            },
            105,
        )
        .unwrap_err();
    assert_eq!(error.code, WorkflowErrorCode::NotFound);
    assert_eq!(
        state
            .store
            .company_project(&TenantId::parse("tenant-a").unwrap(), &state.project_id)
            .unwrap()
            .unwrap(),
        before
    );
}

#[test]
fn dependency_contracts_gate_assignment_and_unlock_only_after_bound_output_and_qa() {
    let state = journey();
    let project = command(
        &state.store,
        &state.pm,
        110,
        CompanyWorkflowCommandV1::PlanWorkGraph {
            project_id: state.project_id.clone(),
            expected_version: 1,
            items: vec![
                work("producer", CompanyRoleV1::Developer, &["rust"], &[], 100),
                work(
                    "consumer",
                    CompanyRoleV1::Developer,
                    &["rust"],
                    &["producer"],
                    100,
                ),
            ],
        },
        110,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    assert_eq!(
        project.work_items[&WorkItemId::parse("consumer").unwrap()].state,
        CompanyWorkStateV1::DependencyPending
    );
    let project = command(
        &state.store,
        &state.pm,
        111,
        CompanyWorkflowCommandV1::ActivateProject {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            reason_ref: "approved-plan".to_owned(),
        },
        111,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let early = state
        .store
        .apply_company_command(
            &state.pm,
            Uuid::from_u128(112),
            &CompanyWorkflowCommandV1::AssignWork {
                project_id: state.project_id.clone(),
                expected_version: project.version,
                work_item_id: WorkItemId::parse("consumer").unwrap(),
                agent_id: AgentId(2),
                organization_generation: 1,
                organization_digest: DIGEST.to_owned(),
                reason_ref: "too-early".to_owned(),
            },
            112,
        )
        .unwrap_err();
    assert_eq!(early.code, WorkflowErrorCode::AuthorityConflict);
    let project = command(
        &state.store,
        &state.pm,
        113,
        CompanyWorkflowCommandV1::AssignWork {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: WorkItemId::parse("producer").unwrap(),
            agent_id: AgentId(2),
            organization_generation: 1,
            organization_digest: DIGEST.to_owned(),
            reason_ref: "producer-owner".to_owned(),
        },
        113,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.developer,
        114,
        transition(
            &state.project_id,
            project.version,
            "producer",
            2,
            1,
            CompanyWorkStateV1::Assigned,
            CompanyWorkStateV1::InProgress,
            Vec::new(),
            None,
            114,
        ),
        114,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.developer,
        115,
        transition(
            &state.project_id,
            project.version,
            "producer",
            3,
            1,
            CompanyWorkStateV1::InProgress,
            CompanyWorkStateV1::InReview,
            output_receipt(),
            None,
            115,
        ),
        115,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    assert_eq!(
        project.work_items[&WorkItemId::parse("consumer").unwrap()].state,
        CompanyWorkStateV1::DependencyPending
    );
    let project = command(
        &state.store,
        &state.qa,
        116,
        transition(
            &state.project_id,
            project.version,
            "producer",
            4,
            1,
            CompanyWorkStateV1::InReview,
            CompanyWorkStateV1::Done,
            output_receipt(),
            Some(QualityGateReceiptBindingV1 {
                gate_id: "web-work-item-qa-v1".to_owned(),
                generation: 1,
                gate_digest: DIGEST.to_owned(),
                subject_digest: OTHER_DIGEST.to_owned(),
                passed: true,
            }),
            116,
        ),
        116,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let producer = &project.work_items[&WorkItemId::parse("producer").unwrap()];
    assert_eq!(producer.transition_history[0].before, "Ready");
    assert_eq!(producer.transition_history[0].after, "Assigned");
    assert_eq!(
        project.work_items[&WorkItemId::parse("consumer").unwrap()].state,
        CompanyWorkStateV1::Ready
    );
    let consumer = &project.work_items[&WorkItemId::parse("consumer").unwrap()];
    assert_eq!(consumer.transition_history.len(), 1);
    assert_eq!(consumer.transition_history[0].before, "DependencyPending");
    assert_eq!(consumer.transition_history[0].after, "Ready");
    assert_eq!(
        consumer.transition_history[0].reason_ref,
        "dependency-contract-satisfied"
    );
    assert_eq!(project.lifecycle_state, ProjectLifecycleStateV1::Active);
}

#[test]
fn independent_gate_authority_can_block_review_but_the_implementer_cannot() {
    let state = journey();
    let project = command(
        &state.store,
        &state.pm,
        170,
        CompanyWorkflowCommandV1::PlanWorkGraph {
            project_id: state.project_id.clone(),
            expected_version: 1,
            items: vec![work(
                "gate-timeout-work",
                CompanyRoleV1::Developer,
                &["rust"],
                &[],
                100,
            )],
        },
        170,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.pm,
        171,
        CompanyWorkflowCommandV1::ActivateProject {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            reason_ref: "approved-plan".to_owned(),
        },
        171,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.pm,
        172,
        CompanyWorkflowCommandV1::AssignWork {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: WorkItemId::parse("gate-timeout-work").unwrap(),
            agent_id: AgentId(2),
            organization_generation: 1,
            organization_digest: DIGEST.to_owned(),
            reason_ref: "implementation-owner".to_owned(),
        },
        172,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.developer,
        173,
        transition(
            &state.project_id,
            project.version,
            "gate-timeout-work",
            2,
            1,
            CompanyWorkStateV1::Assigned,
            CompanyWorkStateV1::InProgress,
            Vec::new(),
            None,
            173,
        ),
        173,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.developer,
        174,
        transition(
            &state.project_id,
            project.version,
            "gate-timeout-work",
            3,
            1,
            CompanyWorkStateV1::InProgress,
            CompanyWorkStateV1::InReview,
            output_receipt(),
            None,
            174,
        ),
        174,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let implementer_error = state
        .store
        .apply_company_command(
            &state.developer,
            Uuid::from_u128(175),
            &transition(
                &state.project_id,
                project.version,
                "gate-timeout-work",
                4,
                1,
                CompanyWorkStateV1::InReview,
                CompanyWorkStateV1::Blocked,
                Vec::new(),
                None,
                175,
            ),
            175,
        )
        .unwrap_err();
    assert_eq!(implementer_error.code, WorkflowErrorCode::InvalidTransition);

    let project = command(
        &state.store,
        &state.qa,
        176,
        transition(
            &state.project_id,
            project.version,
            "gate-timeout-work",
            4,
            1,
            CompanyWorkStateV1::InReview,
            CompanyWorkStateV1::Blocked,
            Vec::new(),
            None,
            176,
        ),
        176,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    assert_eq!(
        project.work_items[&WorkItemId::parse("gate-timeout-work").unwrap()].state,
        CompanyWorkStateV1::Blocked
    );
}

#[test]
fn customer_rework_reopens_delivery_candidate_with_new_linked_work_and_exact_replay() {
    let state = journey();
    let project = command(
        &state.store,
        &state.pm,
        180,
        CompanyWorkflowCommandV1::PlanWorkGraph {
            project_id: state.project_id.clone(),
            expected_version: 1,
            items: vec![work(
                "original-work",
                CompanyRoleV1::Developer,
                &["rust"],
                &[],
                100,
            )],
        },
        180,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.pm,
        181,
        CompanyWorkflowCommandV1::ActivateProject {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            reason_ref: "approved-plan".to_owned(),
        },
        181,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.pm,
        182,
        CompanyWorkflowCommandV1::AssignWork {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: WorkItemId::parse("original-work").unwrap(),
            agent_id: AgentId(2),
            organization_generation: 1,
            organization_digest: DIGEST.to_owned(),
            reason_ref: "owner".to_owned(),
        },
        182,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.developer,
        183,
        transition(
            &state.project_id,
            project.version,
            "original-work",
            2,
            1,
            CompanyWorkStateV1::Assigned,
            CompanyWorkStateV1::InProgress,
            Vec::new(),
            None,
            183,
        ),
        183,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.developer,
        184,
        transition(
            &state.project_id,
            project.version,
            "original-work",
            3,
            1,
            CompanyWorkStateV1::InProgress,
            CompanyWorkStateV1::InReview,
            output_receipt(),
            None,
            184,
        ),
        184,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.qa,
        185,
        transition(
            &state.project_id,
            project.version,
            "original-work",
            4,
            1,
            CompanyWorkStateV1::InReview,
            CompanyWorkStateV1::Done,
            output_receipt(),
            Some(QualityGateReceiptBindingV1 {
                gate_id: "web-work-item-qa-v1".to_owned(),
                generation: 1,
                gate_digest: DIGEST.to_owned(),
                subject_digest: OTHER_DIGEST.to_owned(),
                passed: true,
            }),
            185,
        ),
        185,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    assert_eq!(
        project.lifecycle_state,
        ProjectLifecycleStateV1::DeliveryCandidate
    );
    let original = project.work_items[&WorkItemId::parse("original-work").unwrap()].clone();
    let rework = CompanyWorkflowCommandV1::CreateGovernedRework {
        project_id: state.project_id.clone(),
        expected_version: project.version,
        source_candidate_digest: OTHER_DIGEST.to_owned(),
        feedback_digest: DIGEST.to_owned(),
        source_delivery_id: "delivery-1".to_owned(),
    };
    let foreign_customer = principal(
        "tenant-a",
        "customer-b",
        CompanyPrincipalKindV1::Customer,
        CompanyRoleV1::Customer,
        Some("customer-b"),
        None,
    );
    assert_eq!(
        state
            .store
            .apply_company_command(&foreign_customer, Uuid::from_u128(186), &rework, 186)
            .unwrap_err()
            .code,
        WorkflowErrorCode::AuthorityConflict
    );
    let first = state
        .store
        .apply_company_command(&state.customer, Uuid::from_u128(187), &rework, 187)
        .unwrap();
    assert!(!first.replayed);
    let CompanyWorkflowResponseV1::Project(reopened) = first.response else {
        panic!()
    };
    assert_eq!(reopened.lifecycle_state, ProjectLifecycleStateV1::Active);
    assert_eq!(reopened.work_items.len(), 2);
    assert_eq!(
        reopened.work_items[&WorkItemId::parse("original-work").unwrap()],
        original
    );
    let created = reopened
        .work_items
        .values()
        .find(|work| work.spec.rework.is_some())
        .unwrap();
    let binding = created.spec.rework.as_ref().unwrap();
    assert_eq!(binding.operation_id, Uuid::from_u128(187));
    assert_eq!(binding.source_work_item_id.0, "original-work");
    assert_eq!(binding.source_delivery_id, "delivery-1");
    assert_eq!(binding.source_candidate_digest, OTHER_DIGEST);
    assert_eq!(binding.feedback_digest, DIGEST);
    assert_eq!(binding.generation, 1);
    assert_eq!(created.state, CompanyWorkStateV1::Ready);
    assert!(created.assignments.is_empty());
    let replay = state
        .store
        .apply_company_command(&state.customer, Uuid::from_u128(187), &rework, 188)
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(
        replay.response,
        CompanyWorkflowResponseV1::Project(reopened)
    );
}

#[test]
fn delegation_and_reassignment_preserve_immutable_assignment_history() {
    let state = journey();
    let project = command(
        &state.store,
        &state.pm,
        120,
        CompanyWorkflowCommandV1::PlanWorkGraph {
            project_id: state.project_id.clone(),
            expected_version: 1,
            items: vec![work(
                "delegated-work",
                CompanyRoleV1::Developer,
                &["rust"],
                &[],
                100,
            )],
        },
        120,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.pm,
        121,
        CompanyWorkflowCommandV1::ActivateProject {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            reason_ref: "approved-plan".to_owned(),
        },
        121,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.pm,
        122,
        CompanyWorkflowCommandV1::AssignWork {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: WorkItemId::parse("delegated-work").unwrap(),
            agent_id: AgentId(2),
            organization_generation: 1,
            organization_digest: DIGEST.to_owned(),
            reason_ref: "initial-owner".to_owned(),
        },
        122,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.developer,
        123,
        transition(
            &state.project_id,
            project.version,
            "delegated-work",
            2,
            1,
            CompanyWorkStateV1::Assigned,
            CompanyWorkStateV1::Blocked,
            Vec::new(),
            None,
            123,
        ),
        123,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.developer,
        124,
        CompanyWorkflowCommandV1::DelegateWork {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: WorkItemId::parse("delegated-work").unwrap(),
            expected_assignment_version: 1,
            delegate: AgentId(4),
            reason_ref: "bounded-delegation".to_owned(),
        },
        124,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    assert_eq!(project.lifecycle_state, ProjectLifecycleStateV1::Active);
    let assignments =
        &project.work_items[&WorkItemId::parse("delegated-work").unwrap()].assignments;
    assert_eq!(assignments.len(), 2);
    assert!(!assignments[0].active);
    assert_eq!(assignments[1].agent_id, AgentId(4));
    assert_eq!(assignments[1].delegated_by, Some(AgentId(2)));
    let project = command(
        &state.store,
        &state.junior_developer,
        125,
        transition(
            &state.project_id,
            project.version,
            "delegated-work",
            4,
            2,
            CompanyWorkStateV1::Assigned,
            CompanyWorkStateV1::Blocked,
            Vec::new(),
            None,
            125,
        ),
        125,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.pm,
        126,
        CompanyWorkflowCommandV1::ReassignWork {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: WorkItemId::parse("delegated-work").unwrap(),
            expected_assignment_version: 2,
            agent_id: AgentId(2),
            organization_generation: 2,
            organization_digest: OTHER_DIGEST.to_owned(),
            reason_ref: "manager-reassignment".to_owned(),
        },
        126,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    assert_eq!(project.lifecycle_state, ProjectLifecycleStateV1::Active);
    let assignments =
        &project.work_items[&WorkItemId::parse("delegated-work").unwrap()].assignments;
    assert_eq!(assignments.len(), 3);
    assert_eq!(assignments.iter().filter(|value| value.active).count(), 1);
    assert_eq!(assignments[2].assignment_version, 3);
    assert_eq!(assignments[2].agent_id, AgentId(2));
    let history =
        &project.work_items[&WorkItemId::parse("delegated-work").unwrap()].transition_history;
    assert_eq!(history.len(), 5);
    assert_eq!(
        history
            .iter()
            .map(|audit| (audit.before.as_str(), audit.after.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("Ready", "Assigned"),
            ("Assigned", "Blocked"),
            ("Blocked", "Assigned"),
            ("Assigned", "Blocked"),
            ("Blocked", "Assigned"),
        ]
    );
    assert_eq!(state.junior_developer.agent_id, Some(AgentId(4)));
}

#[test]
fn active_assignment_reassign_and_delegate_have_authority_audits() {
    let state = journey();
    let project = command(
        &state.store,
        &state.pm,
        127,
        CompanyWorkflowCommandV1::PlanWorkGraph {
            project_id: state.project_id.clone(),
            expected_version: 1,
            items: vec![work(
                "active-assignment",
                CompanyRoleV1::Developer,
                &["rust"],
                &[],
                100,
            )],
        },
        127,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.pm,
        128,
        CompanyWorkflowCommandV1::ActivateProject {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            reason_ref: "approved-plan".to_owned(),
        },
        128,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.pm,
        129,
        CompanyWorkflowCommandV1::AssignWork {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: WorkItemId::parse("active-assignment").unwrap(),
            agent_id: AgentId(2),
            organization_generation: 1,
            organization_digest: DIGEST.to_owned(),
            reason_ref: "initial-owner".to_owned(),
        },
        129,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.developer,
        130,
        CompanyWorkflowCommandV1::DelegateWork {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: WorkItemId::parse("active-assignment").unwrap(),
            expected_assignment_version: 1,
            delegate: AgentId(4),
            reason_ref: "active-delegation".to_owned(),
        },
        130,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.pm,
        131,
        CompanyWorkflowCommandV1::ReassignWork {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: WorkItemId::parse("active-assignment").unwrap(),
            expected_assignment_version: 2,
            agent_id: AgentId(2),
            organization_generation: 2,
            organization_digest: OTHER_DIGEST.to_owned(),
            reason_ref: "active-reassignment".to_owned(),
        },
        131,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let work = &project.work_items[&WorkItemId::parse("active-assignment").unwrap()];
    assert_eq!(work.assignments.len(), 3);
    assert_eq!(
        work.transition_history
            .iter()
            .map(|audit| (audit.before.as_str(), audit.after.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("Ready", "Assigned"),
            ("Assigned", "Assigned"),
            ("Assigned", "Assigned"),
        ]
    );
    assert_eq!(work.transition_history[1].actor_id, "developer-a");
    assert_eq!(work.transition_history[2].actor_id, "pm-a");
}

#[test]
fn operation_identity_binds_every_authenticated_authority_field_across_reopen() {
    let state = journey();
    let operation_id = Uuid::from_u128(130);
    let command_value = CompanyWorkflowCommandV1::RecordDecision {
        project_id: state.project_id.clone(),
        expected_version: 1,
        work_item_id: None,
        choice_ref: "bound-authority".to_owned(),
        rationale_ref: "audit-contract".to_owned(),
    };
    state
        .store
        .apply_company_command(&state.pm, operation_id, &command_value, 130)
        .unwrap();
    let reopened = WorkflowStore::open(state._temp.path().join("workflow.sqlite")).unwrap();
    assert!(
        reopened
            .apply_company_command(&state.pm, operation_id, &command_value, 131)
            .unwrap()
            .replayed
    );
    let mut variants = Vec::new();
    let mut changed_digest = state.pm.clone();
    changed_digest.authority_digest = OTHER_DIGEST.to_owned();
    variants.push(changed_digest);
    let mut changed_generation = state.pm.clone();
    changed_generation.authority_generation = 2;
    variants.push(changed_generation);
    variants.push(principal(
        "tenant-a",
        "pm-a",
        CompanyPrincipalKindV1::Agent,
        CompanyRoleV1::Developer,
        None,
        Some(2),
    ));
    variants.push(principal(
        "tenant-a",
        "pm-a",
        CompanyPrincipalKindV1::Operator,
        CompanyRoleV1::ProjectManager,
        None,
        None,
    ));
    variants.push(principal(
        "tenant-a",
        "pm-a",
        CompanyPrincipalKindV1::Customer,
        CompanyRoleV1::Customer,
        Some("customer-b"),
        None,
    ));
    for actor in variants {
        let error = reopened
            .apply_company_command(&actor, operation_id, &command_value, 132)
            .unwrap_err();
        assert_eq!(error.code, WorkflowErrorCode::IdempotencyConflict);
    }
}

#[test]
fn backdated_request_and_project_commands_fail_without_mutation_but_replay_remains_valid() {
    let state = journey();
    let request_before = state
        .store
        .company_customer_request(&TenantId::parse("tenant-a").unwrap(), &state.request_id)
        .unwrap()
        .unwrap();
    let error = state
        .store
        .apply_company_command(
            &state.customer,
            Uuid::from_u128(132),
            &CompanyWorkflowCommandV1::RecordCustomerFeedback {
                request_id: state.request_id.clone(),
                expected_version: request_before.version,
                feedback_ref: "backdated-feedback".to_owned(),
            },
            request_before.updated_at_unix_ms - 1,
        )
        .unwrap_err();
    assert_eq!(error.code, WorkflowErrorCode::InvalidInput);
    assert_eq!(
        state
            .store
            .company_customer_request(&TenantId::parse("tenant-a").unwrap(), &state.request_id)
            .unwrap()
            .unwrap(),
        request_before
    );

    let project_before = state
        .store
        .company_project(&TenantId::parse("tenant-a").unwrap(), &state.project_id)
        .unwrap()
        .unwrap();
    let project_command = CompanyWorkflowCommandV1::RecordDecision {
        project_id: state.project_id.clone(),
        expected_version: project_before.version,
        work_item_id: None,
        choice_ref: "bounded-choice".to_owned(),
        rationale_ref: "time-authority".to_owned(),
    };
    let error = state
        .store
        .apply_company_command(
            &state.pm,
            Uuid::from_u128(133),
            &project_command,
            project_before.updated_at_unix_ms - 1,
        )
        .unwrap_err();
    assert_eq!(error.code, WorkflowErrorCode::InvalidInput);
    assert_eq!(
        state
            .store
            .company_project(&TenantId::parse("tenant-a").unwrap(), &state.project_id)
            .unwrap()
            .unwrap(),
        project_before
    );

    let committed = state
        .store
        .apply_company_command(&state.pm, Uuid::from_u128(134), &project_command, 6)
        .unwrap();
    assert!(!committed.replayed);
    let replay = state
        .store
        .apply_company_command(&state.pm, Uuid::from_u128(134), &project_command, 1)
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.response, committed.response);
}

#[test]
fn proposal_acceptance_cannot_predate_the_request_or_proposal() {
    let temp = TempDir::new().unwrap();
    let store = WorkflowStore::open(temp.path().join("workflow.sqlite")).unwrap();
    let customer = principal(
        "tenant-a",
        "customer-a",
        CompanyPrincipalKindV1::Customer,
        CompanyRoleV1::Customer,
        Some("customer-a"),
        None,
    );
    let sales = principal(
        "tenant-a",
        "sales-a",
        CompanyPrincipalKindV1::Agent,
        CompanyRoleV1::Sales,
        None,
        Some(10),
    );
    let request = command(
        &store,
        &customer,
        135,
        CompanyWorkflowCommandV1::SubmitCustomerRequest {
            summary_ref: "request-summary".to_owned(),
            desired_outcome: "working-product".to_owned(),
            constraints: Vec::new(),
        },
        10,
    );
    let CompanyWorkflowResponseV1::CustomerRequest(request) = request else {
        panic!()
    };
    let request = command(
        &store,
        &sales,
        136,
        CompanyWorkflowCommandV1::QualifyCustomerRequest {
            request_id: request.request_id.clone(),
            expected_version: request.version,
            reason_ref: "bounded-scope".to_owned(),
        },
        11,
    );
    let CompanyWorkflowResponseV1::CustomerRequest(request) = request else {
        panic!()
    };
    let proposal = command(
        &store,
        &sales,
        137,
        CompanyWorkflowCommandV1::CreateProposal {
            request_id: request.request_id.clone(),
            expected_version: request.version,
            binding: binding(),
        },
        12,
    );
    let CompanyWorkflowResponseV1::Proposal(proposal) = proposal else {
        panic!()
    };
    let request_before = store
        .company_customer_request(&customer.tenant_id, &request.request_id)
        .unwrap()
        .unwrap();
    let error = store
        .apply_company_command(
            &customer,
            Uuid::from_u128(138),
            &CompanyWorkflowCommandV1::AcceptProposal {
                request_id: request.request_id.clone(),
                expected_version: request_before.version,
                proposal_id: proposal.proposal_id,
                proposal_digest: proposal.proposal_digest,
            },
            11,
        )
        .unwrap_err();
    assert_eq!(error.code, WorkflowErrorCode::InvalidInput);
    assert_eq!(
        store
            .company_customer_request(&customer.tenant_id, &request.request_id)
            .unwrap()
            .unwrap(),
        request_before
    );
}

#[test]
fn exact_budget_exhaustion_blocks_then_release_restores_active_and_zero_cost_local_is_valid() {
    let state = journey();
    let project = command(
        &state.store,
        &state.pm,
        140,
        CompanyWorkflowCommandV1::PlanWorkGraph {
            project_id: state.project_id.clone(),
            expected_version: 1,
            items: vec![work(
                "budget-work",
                CompanyRoleV1::Developer,
                &["rust"],
                &[],
                100,
            )],
        },
        140,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.pm,
        141,
        CompanyWorkflowCommandV1::ActivateProject {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            reason_ref: "approved-plan".to_owned(),
        },
        141,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.pm,
        142,
        CompanyWorkflowCommandV1::ReserveCost {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: None,
            provider: "local".to_owned(),
            amount_micros: 700,
        },
        142,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    let project = command(
        &state.store,
        &state.pm,
        143,
        CompanyWorkflowCommandV1::ReserveCost {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: None,
            provider: "review".to_owned(),
            amount_micros: 300,
        },
        143,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    assert_eq!(project.lifecycle_state, ProjectLifecycleStateV1::Blocked);
    assert!(project.blockers.iter().any(|value| {
        value.blocker_kind == BlockerKindV1::BudgetExhausted && value.state == BlockerStateV1::Open
    }));
    let losing_writer = state
        .store
        .apply_company_command(
            &state.pm,
            Uuid::from_u128(144),
            &CompanyWorkflowCommandV1::ReserveCost {
                project_id: state.project_id.clone(),
                expected_version: project.version - 1,
                work_item_id: None,
                provider: "review".to_owned(),
                amount_micros: 1,
            },
            144,
        )
        .unwrap_err();
    assert_eq!(losing_writer.code, WorkflowErrorCode::VersionConflict);
    let review_reservation = project
        .reservations
        .iter()
        .find(|value| value.provider == "review")
        .unwrap()
        .reservation_id
        .clone();
    let project = command(
        &state.store,
        &state.pm,
        145,
        CompanyWorkflowCommandV1::ReleaseCost {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            reservation_id: review_reservation,
            reason_ref: "capacity-restored".to_owned(),
        },
        145,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    assert_eq!(project.lifecycle_state, ProjectLifecycleStateV1::Active);
    assert!(project
        .blockers
        .iter()
        .all(|value| value.state == BlockerStateV1::Resolved));
    let budget_blocker = project
        .blockers
        .iter()
        .find(|value| value.blocker_kind == BlockerKindV1::BudgetExhausted)
        .unwrap();
    assert_eq!(budget_blocker.transition_history.len(), 1);
    assert_eq!(budget_blocker.transition_history[0].before, "Open");
    assert_eq!(budget_blocker.transition_history[0].after, "Resolved");
    assert!(project.reservations.iter().any(|value| {
        value.provider == "review" && value.state == CostReservationStateV1::Released
    }));
    let project = command(
        &state.store,
        &state.pm,
        146,
        CompanyWorkflowCommandV1::ReserveCost {
            project_id: state.project_id,
            expected_version: project.version,
            work_item_id: None,
            provider: "local-loop".to_owned(),
            amount_micros: 0,
        },
        146,
    );
    let CompanyWorkflowResponseV1::Project(project) = project else {
        panic!()
    };
    assert_eq!(project.reserved_cost_micros, 700);

    let paid_zero = state
        .store
        .apply_company_command(
            &state.pm,
            Uuid::from_u128(147),
            &CompanyWorkflowCommandV1::ReserveCost {
                project_id: project.project_id.clone(),
                expected_version: project.version,
                work_item_id: None,
                provider: "review".to_owned(),
                amount_micros: 0,
            },
            147,
        )
        .unwrap_err();
    assert_eq!(paid_zero.code, WorkflowErrorCode::InvalidInput);
}

#[test]
fn collaboration_journey_is_durable_fenced_bounded_and_replayable() {
    let state = journey();
    let work_item_id = WorkItemId::parse("collaboration-work").unwrap();
    let mut project = project_command(
        &state.store,
        &state.pm,
        300,
        CompanyWorkflowCommandV1::PlanWorkGraph {
            project_id: state.project_id.clone(),
            expected_version: 1,
            items: vec![work(
                &work_item_id.0,
                CompanyRoleV1::Developer,
                &["rust"],
                &[],
                100,
            )],
        },
        300,
    );
    project = project_command(
        &state.store,
        &state.pm,
        301,
        CompanyWorkflowCommandV1::ActivateProject {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            reason_ref: "approved collaboration plan".to_owned(),
        },
        301,
    );
    project = project_command(
        &state.store,
        &state.pm,
        302,
        CompanyWorkflowCommandV1::AssignWork {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: work_item_id.clone(),
            agent_id: AgentId(2),
            organization_generation: 1,
            organization_digest: DIGEST.to_owned(),
            reason_ref: "bounded implementation owner".to_owned(),
        },
        302,
    );

    let authority = collaboration_authority(&project, &work_item_id);
    let unscoped = state.store.apply_company_command(
        &state.pm,
        Uuid::from_u128(3303),
        &CompanyWorkflowCommandV1::CreateCollaborationSession {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: None,
            authority: authority.clone(),
            subject_ref: "unscoped collaboration must fail".to_owned(),
            input_digest: DIGEST.to_owned(),
            mode: CollaborationModeV1::IndependentReview,
            budget: CollaborationBudgetV1 {
                max_participants: 2,
                max_claims: 2,
                max_handoffs: 1,
                max_clarification_rounds: 1,
                max_transitions: 8,
                deadline_unix_ms: 10_000,
            },
            participants: vec![
                collaboration_participant(
                    1,
                    CompanyRoleV1::ProjectManager,
                    BehaviorMandateV1::Synthesize,
                    "coordination",
                ),
                collaboration_participant(
                    2,
                    CompanyRoleV1::Developer,
                    BehaviorMandateV1::Implement,
                    "rust",
                ),
            ],
        },
        303,
    );
    assert_eq!(
        unscoped.unwrap_err().code,
        WorkflowErrorCode::AuthorityConflict
    );

    let mut stale_policy = authority.clone();
    stale_policy.policy_digest = OTHER_DIGEST.to_owned();
    let stale = state.store.apply_company_command(
        &state.pm,
        Uuid::from_u128(4303),
        &CompanyWorkflowCommandV1::CreateCollaborationSession {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: Some(work_item_id.clone()),
            authority: stale_policy,
            subject_ref: "stale policy must fail".to_owned(),
            input_digest: DIGEST.to_owned(),
            mode: CollaborationModeV1::IndependentReview,
            budget: CollaborationBudgetV1 {
                max_participants: 2,
                max_claims: 2,
                max_handoffs: 1,
                max_clarification_rounds: 1,
                max_transitions: 8,
                deadline_unix_ms: 10_000,
            },
            participants: vec![
                collaboration_participant(
                    1,
                    CompanyRoleV1::ProjectManager,
                    BehaviorMandateV1::Synthesize,
                    "coordination",
                ),
                collaboration_participant(
                    2,
                    CompanyRoleV1::Developer,
                    BehaviorMandateV1::Implement,
                    "rust",
                ),
            ],
        },
        303,
    );
    assert_eq!(
        stale.unwrap_err().code,
        WorkflowErrorCode::AuthorityConflict
    );

    project = project_command(
        &state.store,
        &state.pm,
        303,
        CompanyWorkflowCommandV1::CreateCollaborationSession {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: Some(work_item_id.clone()),
            authority: authority.clone(),
            subject_ref: "produce and independently review the artifact".to_owned(),
            input_digest: DIGEST.to_owned(),
            mode: CollaborationModeV1::IndependentReview,
            budget: CollaborationBudgetV1 {
                max_participants: 3,
                max_claims: 3,
                max_handoffs: 3,
                max_clarification_rounds: 1,
                max_transitions: 40,
                deadline_unix_ms: 10_000,
            },
            participants: vec![
                collaboration_participant(
                    1,
                    CompanyRoleV1::ProjectManager,
                    BehaviorMandateV1::Synthesize,
                    "coordination",
                ),
                collaboration_participant(
                    2,
                    CompanyRoleV1::Developer,
                    BehaviorMandateV1::Implement,
                    "rust",
                ),
                collaboration_participant(3, CompanyRoleV1::Qa, BehaviorMandateV1::Challenge, "qa"),
            ],
        },
        303,
    );
    let session_id = project.collaboration_sessions[0].session_id.clone();
    let mut stale_assignment = authority.clone();
    stale_assignment.assignment_version += 1;
    let stale_assignment_error = state
        .store
        .apply_company_command(
            &state.pm,
            Uuid::from_u128(5304),
            &CompanyWorkflowCommandV1::TransitionCollaborationSession {
                project_id: state.project_id.clone(),
                expected_version: project.version,
                session_id: session_id.clone(),
                expected_transition_sequence: 1,
                authority: stale_assignment,
                target: CollaborationSessionStateV1::CollectingIndependentClaims,
                reason_ref: "stale assignment must fail".to_owned(),
            },
            304,
        )
        .unwrap_err();
    assert_eq!(
        stale_assignment_error.code,
        WorkflowErrorCode::AuthorityConflict
    );
    assert_eq!(
        state
            .store
            .company_project(&project.tenant_id, &project.project_id)
            .unwrap(),
        Some(project.clone())
    );
    project = project_command(
        &state.store,
        &state.pm,
        304,
        CompanyWorkflowCommandV1::TransitionCollaborationSession {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            session_id: session_id.clone(),
            expected_transition_sequence: 1,
            authority: authority.clone(),
            target: CollaborationSessionStateV1::CollectingIndependentClaims,
            reason_ref: "collect claims independently".to_owned(),
        },
        304,
    );

    let pm_claim = CompanyWorkflowCommandV1::RecordIndependentClaim {
        project_id: state.project_id.clone(),
        expected_version: project.version,
        session_id: session_id.clone(),
        expected_transition_sequence: 2,
        authority: authority.clone(),
        conclusion_ref: "the plan is bounded".to_owned(),
        evidence: evidence("plan evidence"),
        assumptions: vec!["authority remains stable".to_owned()],
        uncertainty: UncertaintyClassV1::Low,
        confidence_basis: "verified project contract".to_owned(),
        capability_snapshot_digest: DIGEST.to_owned(),
        input_digest: DIGEST.to_owned(),
    };
    let first = state
        .store
        .apply_company_command(&state.pm, Uuid::from_u128(305), &pm_claim, 305)
        .unwrap();
    assert!(!first.replayed);
    let CompanyWorkflowResponseV1::Project(first_project) = first.response else {
        panic!("expected project response")
    };
    project = *first_project;
    let replay = state
        .store
        .apply_company_command(&state.pm, Uuid::from_u128(305), &pm_claim, 306)
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(project.collaboration_publications.len(), 3);

    project = project_command(
        &state.store,
        &state.developer,
        306,
        CompanyWorkflowCommandV1::RecordIndependentClaim {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            session_id: session_id.clone(),
            expected_transition_sequence: 3,
            authority: authority.clone(),
            conclusion_ref: "the implementation is feasible".to_owned(),
            evidence: evidence("implementation evidence"),
            assumptions: vec!["toolchain remains pinned".to_owned()],
            uncertainty: UncertaintyClassV1::Material,
            confidence_basis: "source and contract inspection".to_owned(),
            capability_snapshot_digest: OTHER_DIGEST.to_owned(),
            input_digest: DIGEST.to_owned(),
        },
        307,
    );
    project = project_command(
        &state.store,
        &state.qa,
        307,
        CompanyWorkflowCommandV1::RecordIndependentClaim {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            session_id: session_id.clone(),
            expected_transition_sequence: 4,
            authority: authority.clone(),
            conclusion_ref: "independent verification is required".to_owned(),
            evidence: evidence("quality evidence"),
            assumptions: Vec::new(),
            uncertainty: UncertaintyClassV1::Blocking,
            confidence_basis: "independent negative testing".to_owned(),
            capability_snapshot_digest: DIGEST.to_owned(),
            input_digest: DIGEST.to_owned(),
        },
        308,
    );
    let private = filtered_collaboration_view(&project, &state.developer, &session_id).unwrap();
    assert_eq!(private.session.claims.len(), 1);
    assert_eq!(private.session.claims[0].contributor, AgentId(2));

    let mut stale_authority = authority.clone();
    stale_authority.policy_version += 1;
    let stale_error = state
        .store
        .apply_company_command(
            &state.pm,
            Uuid::from_u128(308),
            &CompanyWorkflowCommandV1::OpenClaimExposureBarrier {
                project_id: state.project_id.clone(),
                expected_version: project.version,
                session_id: session_id.clone(),
                expected_transition_sequence: 5,
                authority: stale_authority,
                reason_ref: "must not expose".to_owned(),
            },
            309,
        )
        .unwrap_err();
    assert_eq!(stale_error.code, WorkflowErrorCode::AuthorityConflict);
    assert_eq!(
        state
            .store
            .company_project(&project.tenant_id, &project.project_id)
            .unwrap()
            .unwrap(),
        project
    );

    project = project_command(
        &state.store,
        &state.pm,
        309,
        CompanyWorkflowCommandV1::OpenClaimExposureBarrier {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            session_id: session_id.clone(),
            expected_transition_sequence: 5,
            authority: authority.clone(),
            reason_ref: "all independent claims are durable".to_owned(),
        },
        310,
    );
    assert!(project.collaboration_sessions[0]
        .claims
        .iter()
        .all(|claim| claim.exposure_state == ClaimExposureStateV1::Exposed));

    let offer_handoff = CompanyWorkflowCommandV1::OfferHandoffPacket {
        project_id: state.project_id.clone(),
        expected_version: project.version,
        session_id: session_id.clone(),
        expected_transition_sequence: 6,
        authority: authority.clone(),
        work_item_id: work_item_id.clone(),
        consumer: AgentId(3),
        objective_ref: "independently verify the implementation".to_owned(),
        authority_scope_ref: authority.assignment_id.clone(),
        authority_scope_digest: authority.assignment_digest.clone(),
        input_digests: BTreeSet::from([DIGEST.to_owned()]),
        artifact_digests: BTreeSet::from([OTHER_DIGEST.to_owned()]),
        evidence: evidence("handoff evidence"),
        assumptions: vec!["artifact is immutable".to_owned()],
        unresolved_questions: vec!["is the failure path bounded".to_owned()],
        uncertainty: UncertaintyClassV1::Material,
        acceptance_checks: vec!["reproduce the negative path".to_owned()],
        required_capabilities: BTreeSet::from(["qa".to_owned()]),
        privacy_classes: BTreeSet::from(["project-internal".to_owned()]),
    };
    let mut wrong_authority_scope = offer_handoff.clone();
    let CompanyWorkflowCommandV1::OfferHandoffPacket {
        authority_scope_digest,
        ..
    } = &mut wrong_authority_scope
    else {
        unreachable!()
    };
    *authority_scope_digest = DIGEST.to_owned();
    let wrong_authority_error = state
        .store
        .apply_company_command(
            &state.developer,
            Uuid::from_u128(5310),
            &wrong_authority_scope,
            311,
        )
        .unwrap_err();
    assert_eq!(
        wrong_authority_error.code,
        WorkflowErrorCode::AuthorityConflict
    );
    assert_eq!(
        state
            .store
            .company_project(&project.tenant_id, &project.project_id)
            .unwrap(),
        Some(project.clone())
    );
    project = project_command(&state.store, &state.developer, 310, offer_handoff, 311);
    let packet_id = project.handoff_packets[0].packet_id.clone();
    let packet_digest = project.handoff_packets[0].packet_digest.clone();
    project = project_command(
        &state.store,
        &state.qa,
        311,
        CompanyWorkflowCommandV1::RequestHandoffClarification {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            session_id: session_id.clone(),
            expected_transition_sequence: 7,
            authority: authority.clone(),
            packet_id: packet_id.clone(),
            packet_digest: packet_digest.clone(),
            gap_class: HandoffGapClassV1::DataGap,
            question_ref: "provide the negative result".to_owned(),
            basis_digest: DIGEST.to_owned(),
        },
        312,
    );
    let clarification_id = project.handoff_packets[0].clarifications[0]
        .clarification_id
        .clone();
    project = project_command(
        &state.store,
        &state.developer,
        312,
        CompanyWorkflowCommandV1::AnswerHandoffClarification {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            session_id: session_id.clone(),
            expected_transition_sequence: 8,
            authority: authority.clone(),
            packet_id: packet_id.clone(),
            packet_digest: packet_digest.clone(),
            clarification_id,
            question_generation: 1,
            answer_ref: "negative result is attached".to_owned(),
            new_information_digest: OTHER_DIGEST.to_owned(),
        },
        313,
    );
    project = project_command(
        &state.store,
        &state.qa,
        313,
        CompanyWorkflowCommandV1::AcceptHandoffPacket {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            session_id: session_id.clone(),
            expected_transition_sequence: 9,
            authority: authority.clone(),
            packet_id: packet_id.clone(),
            packet_digest: packet_digest.clone(),
            capability_snapshot_digest: DIGEST.to_owned(),
            reason_ref: "evidence is sufficient".to_owned(),
        },
        314,
    );
    let qa_claim = project.collaboration_sessions[0]
        .claims
        .iter()
        .find(|claim| claim.contributor == AgentId(3))
        .unwrap()
        .clone();
    let wrong_consumption = state
        .store
        .apply_company_command(
            &state.qa,
            Uuid::from_u128(400),
            &CompanyWorkflowCommandV1::ConsumeHandoffPacket {
                project_id: state.project_id.clone(),
                expected_version: project.version,
                session_id: session_id.clone(),
                expected_transition_sequence: 10,
                authority: authority.clone(),
                packet_id: packet_id.clone(),
                packet_digest: packet_digest.clone(),
                kind: HandoffConsumptionKindV1::IndependentClaim,
                subject_id: qa_claim.claim_id.clone(),
                subject_digest: DIGEST.to_owned(),
            },
            315,
        )
        .unwrap_err();
    assert_eq!(wrong_consumption.code, WorkflowErrorCode::InvalidTransition);
    assert_eq!(
        state
            .store
            .company_project(&project.tenant_id, &project.project_id)
            .unwrap()
            .unwrap(),
        project
    );
    project = project_command(
        &state.store,
        &state.qa,
        314,
        CompanyWorkflowCommandV1::ConsumeHandoffPacket {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            session_id: session_id.clone(),
            expected_transition_sequence: 10,
            authority: authority.clone(),
            packet_id: packet_id.clone(),
            packet_digest: packet_digest.clone(),
            kind: HandoffConsumptionKindV1::IndependentClaim,
            subject_id: qa_claim.claim_id,
            subject_digest: qa_claim.claim_digest,
        },
        315,
    );
    assert_eq!(
        project.handoff_packets[0].state,
        HandoffPacketStateV1::Consumed
    );

    project = project_command(
        &state.store,
        &state.developer,
        401,
        CompanyWorkflowCommandV1::OfferHandoffPacket {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            session_id: session_id.clone(),
            expected_transition_sequence: 11,
            authority: authority.clone(),
            work_item_id: work_item_id.clone(),
            consumer: AgentId(3),
            objective_ref: "verify clarification novelty".to_owned(),
            authority_scope_ref: authority.assignment_id.clone(),
            authority_scope_digest: authority.assignment_digest.clone(),
            input_digests: BTreeSet::from([DIGEST.to_owned()]),
            artifact_digests: BTreeSet::new(),
            evidence: evidence("novelty evidence"),
            assumptions: Vec::new(),
            unresolved_questions: vec!["does the answer add information".to_owned()],
            uncertainty: UncertaintyClassV1::Material,
            acceptance_checks: vec!["reject repeated information".to_owned()],
            required_capabilities: BTreeSet::from(["qa".to_owned()]),
            privacy_classes: BTreeSet::from(["project-internal".to_owned()]),
        },
        316,
    );
    let novelty_packet = project.handoff_packets[1].clone();
    project = project_command(
        &state.store,
        &state.qa,
        402,
        CompanyWorkflowCommandV1::RequestHandoffClarification {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            session_id: session_id.clone(),
            expected_transition_sequence: 12,
            authority: authority.clone(),
            packet_id: novelty_packet.packet_id.clone(),
            packet_digest: novelty_packet.packet_digest.clone(),
            gap_class: HandoffGapClassV1::ReferentialDrift,
            question_ref: "provide a current reference".to_owned(),
            basis_digest: DIGEST.to_owned(),
        },
        317,
    );
    let novelty_clarification = project.handoff_packets[1].clarifications[0]
        .clarification_id
        .clone();
    let unknown_clarification = state
        .store
        .apply_company_command(
            &state.developer,
            Uuid::from_u128(404),
            &CompanyWorkflowCommandV1::AnswerHandoffClarification {
                project_id: state.project_id.clone(),
                expected_version: project.version,
                session_id: session_id.clone(),
                expected_transition_sequence: 13,
                authority: authority.clone(),
                packet_id: novelty_packet.packet_id.clone(),
                packet_digest: novelty_packet.packet_digest.clone(),
                clarification_id: "unknown-clarification".to_owned(),
                question_generation: 1,
                answer_ref: "repeated evidence".to_owned(),
                new_information_digest: DIGEST.to_owned(),
            },
            318,
        )
        .unwrap_err();
    assert_eq!(unknown_clarification.code, WorkflowErrorCode::NotFound);
    project = project_command(
        &state.store,
        &state.developer,
        403,
        CompanyWorkflowCommandV1::AnswerHandoffClarification {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            session_id: session_id.clone(),
            expected_transition_sequence: 13,
            authority: authority.clone(),
            packet_id: novelty_packet.packet_id,
            packet_digest: novelty_packet.packet_digest,
            clarification_id: novelty_clarification,
            question_generation: 1,
            answer_ref: "repeated evidence".to_owned(),
            new_information_digest: DIGEST.to_owned(),
        },
        318,
    );
    assert_eq!(
        project.handoff_packets[1].state,
        HandoffPacketStateV1::Escalated
    );

    project = project_command(
        &state.store,
        &state.pm,
        315,
        CompanyWorkflowCommandV1::TransitionCollaborationSession {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            session_id: session_id.clone(),
            expected_transition_sequence: 14,
            authority: authority.clone(),
            target: CollaborationSessionStateV1::Deciding,
            reason_ref: "evaluate exposed evidence".to_owned(),
        },
        319,
    );
    project = project_command(
        &state.store,
        &state.pm,
        316,
        CompanyWorkflowCommandV1::RecordDecision {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: Some(work_item_id.clone()),
            choice_ref: "accept implementation".to_owned(),
            rationale_ref: "independent claims and handoff evidence agree".to_owned(),
        },
        320,
    );
    let decision_id = project.decisions.last().unwrap().decision_id.clone();
    project = project_command(
        &state.store,
        &state.pm,
        320,
        CompanyWorkflowCommandV1::RecordDecision {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: None,
            choice_ref: "retain alternative".to_owned(),
            rationale_ref: "prove collaboration evidence cannot cross work scope".to_owned(),
        },
        321,
    );
    let other_decision_id = project.decisions.last().unwrap().decision_id.clone();
    let cross_work_dissent = state
        .store
        .apply_company_command(
            &state.qa,
            Uuid::from_u128(4321),
            &CompanyWorkflowCommandV1::RecordDissent {
                project_id: state.project_id.clone(),
                expected_version: project.version,
                session_id: session_id.clone(),
                expected_transition_sequence: 15,
                authority: authority.clone(),
                decision_id: other_decision_id.clone(),
                claim_id: None,
                rationale_ref: "cross-work dissent must fail".to_owned(),
                evidence: evidence("cross-work evidence"),
                residual_risk_ref: "unrelated project decision".to_owned(),
            },
            321,
        )
        .unwrap_err();
    assert_eq!(
        cross_work_dissent.code,
        WorkflowErrorCode::InvalidTransition
    );
    let claim_ids = project.collaboration_sessions[0]
        .claims
        .iter()
        .map(|claim| claim.claim_id.clone())
        .collect::<BTreeSet<_>>();
    let qa_claim_id = project.collaboration_sessions[0]
        .claims
        .iter()
        .find(|claim| claim.contributor == AgentId(3))
        .unwrap()
        .claim_id
        .clone();
    project = project_command(
        &state.store,
        &state.qa,
        317,
        CompanyWorkflowCommandV1::RecordDissent {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            session_id: session_id.clone(),
            expected_transition_sequence: 15,
            authority: authority.clone(),
            decision_id: decision_id.clone(),
            claim_id: Some(qa_claim_id),
            rationale_ref: "retain the bounded residual risk".to_owned(),
            evidence: evidence("residual risk evidence"),
            residual_risk_ref: "monitor the negative path".to_owned(),
        },
        322,
    );
    let dissent_id = project.dissent_records[0].dissent_id.clone();
    let before_wrong_decision_link = project.clone();
    let wrong_decision_link = state
        .store
        .apply_company_command(
            &state.pm,
            Uuid::from_u128(321),
            &CompanyWorkflowCommandV1::LinkDecisionEvidence {
                project_id: state.project_id.clone(),
                expected_version: project.version,
                session_id: session_id.clone(),
                expected_transition_sequence: 16,
                authority: authority.clone(),
                decision_id: other_decision_id,
                claim_ids: claim_ids.clone(),
                dissent_ids: BTreeSet::from([dissent_id.clone()]),
            },
            323,
        )
        .unwrap_err();
    assert_eq!(
        wrong_decision_link.code,
        WorkflowErrorCode::InvalidTransition
    );
    assert_eq!(
        state
            .store
            .company_project(&state.pm.tenant_id, &state.project_id)
            .unwrap(),
        Some(before_wrong_decision_link)
    );
    project = project_command(
        &state.store,
        &state.pm,
        318,
        CompanyWorkflowCommandV1::LinkDecisionEvidence {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            session_id: session_id.clone(),
            expected_transition_sequence: 16,
            authority: authority.clone(),
            decision_id,
            claim_ids,
            dissent_ids: BTreeSet::from([dissent_id]),
        },
        324,
    );
    project = project_command(
        &state.store,
        &state.pm,
        319,
        CompanyWorkflowCommandV1::TransitionCollaborationSession {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            session_id: session_id.clone(),
            expected_transition_sequence: 17,
            authority,
            target: CollaborationSessionStateV1::Completed,
            reason_ref: "authorized decision is evidence-linked".to_owned(),
        },
        325,
    );

    let session = &project.collaboration_sessions[0];
    assert_eq!(session.state, CollaborationSessionStateV1::Completed);
    assert_eq!(session.transition_sequence, 18);
    assert_eq!(session.publication_revision, 18);
    assert_eq!(project.collaboration_publications.len(), 18);
    for (index, publication) in project.collaboration_publications.iter().enumerate() {
        assert_eq!(publication.transition_sequence, (index + 1) as u64);
        assert_eq!(
            publication.proposal.causal_context.correlation_id,
            session_id
        );
    }
}
