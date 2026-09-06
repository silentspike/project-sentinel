use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Barrier};
use std::thread;

use sentinel_workflow::{
    authorize_collaboration_gateway_result, collaboration_policy_ambiguity,
    collaboration_policy_reversibility, collaboration_policy_role_name,
    collaboration_policy_separation_requirements, collaboration_policy_task_risk,
    collaboration_policy_uncertainty, compile_collaboration_gateway_request,
    filtered_collaboration_view, AgentId, AuthenticatedCompanyPrincipalV1, BehaviorMandateV1,
    BlockerKindV1, BlockerStateV1, BlockerV1, ClaimExposureStateV1, CollaborationAdmissionBudgetV1,
    CollaborationAdmissionFenceV1, CollaborationAdmissionInputV1, CollaborationAdmissionModeV1,
    CollaborationAdmissionStateV1, CollaborationAuthorityFenceV1, CollaborationBudgetV1,
    CollaborationCandidateV1, CollaborationModeV1, CollaborationParticipantV1,
    CollaborationProgressDispositionV1, CollaborationProgressV1, CollaborationSessionStateV1,
    CompanyPrincipalKindV1, CompanyRoleV1, CompanyWorkItemSpecV1, CompanyWorkStateV1,
    CompanyWorkflowCommandV1, CompanyWorkflowResponseV1, CostReservationStateV1,
    EvidenceReferenceV1, HandoffConsumptionKindV1, HandoffGapClassV1, HandoffPacketStateV1,
    ParticipantBindingV1, ProjectId, ProjectLifecycleStateV1, ProjectQuestionV1, ProposalBindingV1,
    ProposalGovernanceV1, QualityGateBindingV1, QualityGateReceiptBindingV1,
    ReliabilityObservationV1, ReversibilityV1, TaskRiskV1, TenantId, UncertaintyClassV1,
    WorkInputContractV1, WorkItemId, WorkOutputContractV1, WorkOutputReceiptV1,
    WorkProfileBindingV1, WorkTransitionReceiptV1, WorkflowErrorCode, WorkflowStore,
    COLLABORATION_ADMISSION_SCHEMA_VERSION, COLLABORATION_POLICY_MAX_PARTICIPANTS,
    COLLABORATION_POLICY_MAX_ROUNDS, COLLABORATION_POLICY_MAX_STALLED_UPDATES,
    COLLABORATION_POLICY_MAX_TOKENS, COLLABORATION_POLICY_MINIMUM_NOVELTY_MICROS,
    COLLABORATION_POLICY_QUALITY_TOLERANCE_MICROS, COLLABORATION_POLICY_WINDOW_MS,
    COMPANY_DOMAIN_SCHEMA_VERSION,
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

#[path = "company_domain/subscription.rs"]
mod subscription;

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

fn collaboration_participants_from_admission(
    admission: &sentinel_workflow::CollaborationAdmissionDecisionV1,
) -> Vec<CollaborationParticipantV1> {
    admission
        .selected_participants
        .iter()
        .map(|participant| CollaborationParticipantV1 {
            agent_id: participant.agent_id,
            permanent_role: participant.permanent_role,
            mandate: participant.mandate,
            capability_snapshot_digest: participant.candidate_snapshot_digest.clone(),
            capabilities: participant.capabilities.clone(),
            privacy_classes: participant.privacy_classes.clone(),
        })
        .collect()
}

fn collaboration_capability_snapshot(
    project: &sentinel_workflow::ProjectV1,
    session_id: &str,
    agent_id: AgentId,
) -> String {
    project
        .collaboration_sessions
        .iter()
        .find(|session| session.session_id == session_id)
        .and_then(|session| {
            session
                .participants
                .iter()
                .find(|participant| participant.agent_id == agent_id)
        })
        .map(|participant| participant.capability_snapshot_digest.clone())
        .unwrap()
}

fn collaboration_admission_input(
    project: &sentinel_workflow::ProjectV1,
    work_item_id: &WorkItemId,
    store: &WorkflowStore,
    now_ms: u64,
) -> CollaborationAdmissionInputV1 {
    let work_item = &project.work_items[work_item_id];
    let assignment = work_item
        .assignments
        .iter()
        .find(|assignment| assignment.active)
        .unwrap();
    let remaining_cost_budget_micros = project
        .cost_ceiling_micros
        .checked_sub(project.reserved_cost_micros)
        .and_then(|value| value.checked_sub(project.committed_cost_micros))
        .and_then(|value| {
            value.checked_sub(
                store
                    .collaboration_capacity_snapshot(&project.tenant_id, &project.project_id)
                    .unwrap()
                    .project_reserved_cost_micros,
            )
        })
        .unwrap();
    let task_risk = collaboration_policy_task_risk(work_item.spec.required_role);
    let reversibility = collaboration_policy_reversibility(work_item.spec.required_role);
    let ambiguity = collaboration_policy_ambiguity(work_item.spec.required_role);
    let uncertainty = collaboration_policy_uncertainty(project, work_item_id);
    let evidence_conflict = false;
    let mut required_handoff_agents = Vec::new();
    for dependency_id in &work_item.spec.dependency_ids {
        let dependency_assignment = project.work_items[dependency_id]
            .assignments
            .iter()
            .find(|candidate| candidate.active)
            .unwrap();
        if dependency_assignment.agent_id != assignment.agent_id
            && !required_handoff_agents.contains(&dependency_assignment.agent_id)
        {
            required_handoff_agents.push(dependency_assignment.agent_id);
        }
    }
    required_handoff_agents.sort_unstable_by_key(|agent_id| agent_id.0);
    let capability_topology = project
        .governance
        .participants
        .iter()
        .map(|participant| (participant.agent_id, participant.specialties.clone()))
        .collect::<Vec<_>>();
    let (directed_handoff_required, specialist_panel_required) =
        sentinel_workflow::collaboration_policy_team_shape(
            assignment.agent_id,
            &work_item.spec.required_specialties,
            &capability_topology,
            &required_handoff_agents,
        )
        .unwrap();
    let separation_requirements = collaboration_policy_separation_requirements(
        work_item.spec.required_role,
        task_risk,
        ambiguity,
        uncertainty,
        evidence_conflict,
    );
    CollaborationAdmissionInputV1 {
        schema_version: COLLABORATION_ADMISSION_SCHEMA_VERSION,
        tenant_id: project.tenant_id.clone(),
        project_id: project.project_id.clone(),
        work_item_id: work_item_id.clone(),
        owner: assignment.agent_id,
        task_family: project.governance.project_profile.profile_id.clone(),
        input_class: collaboration_policy_role_name(work_item.spec.required_role).to_owned(),
        task_risk,
        reversibility,
        ambiguity,
        required_capabilities: work_item.spec.required_specialties.clone(),
        uncertainty,
        evidence_conflict,
        directed_handoff_required,
        required_handoff_agents,
        specialist_panel_required,
        separation_requirements,
        privacy_class: "project-internal".to_owned(),
        authority_conflict: false,
        privacy_conflict: false,
        human_approval_required: reversibility == ReversibilityV1::Irreversible,
        remaining_cost_budget_micros,
        remaining_time_budget_ms: COLLABORATION_POLICY_WINDOW_MS,
        organization_generation: assignment.organization_generation,
        organization_digest: assignment.organization_digest.clone(),
        assignment_id: assignment.assignment_id.clone(),
        assignment_version: assignment.assignment_version,
        assignment_digest: assignment.canonical_digest().unwrap(),
        behavior_policy_generation: project.governance.project_profile.generation,
        behavior_policy_digest: project.governance.project_profile.digest.clone(),
        learned_reliability_enabled: false,
        collaboration_generation: project.collaboration_generation,
        quality_tolerance_micros: COLLABORATION_POLICY_QUALITY_TOLERANCE_MICROS,
        permitted_packet_classes: BTreeSet::from([
            "decision".to_owned(),
            "evidence".to_owned(),
            "finding".to_owned(),
            "handoff".to_owned(),
        ]),
        budget: CollaborationAdmissionBudgetV1 {
            max_participants: u16::try_from(
                project
                    .governance
                    .participants
                    .len()
                    .min(usize::from(COLLABORATION_POLICY_MAX_PARTICIPANTS)),
            )
            .unwrap(),
            max_rounds: COLLABORATION_POLICY_MAX_ROUNDS,
            max_tokens: COLLABORATION_POLICY_MAX_TOKENS,
            max_cost_micros: work_item
                .spec
                .budget_micros
                .min(remaining_cost_budget_micros),
            deadline_unix_ms: now_ms + COLLABORATION_POLICY_WINDOW_MS,
            minimum_novelty_micros: COLLABORATION_POLICY_MINIMUM_NOVELTY_MICROS,
            max_stalled_updates: COLLABORATION_POLICY_MAX_STALLED_UPDATES,
        },
    }
}

fn collaboration_admission_candidates(
    project: &sentinel_workflow::ProjectV1,
    work_item_id: &WorkItemId,
    store: &WorkflowStore,
) -> Vec<CollaborationCandidateV1> {
    let capacity = store
        .collaboration_capacity_snapshot(&project.tenant_id, &project.project_id)
        .unwrap();
    let work_item = &project.work_items[work_item_id];
    let assignment = work_item
        .assignments
        .iter()
        .find(|assignment| assignment.active)
        .unwrap();
    project
        .governance
        .participants
        .iter()
        .map(|participant| CollaborationCandidateV1 {
            agent_id: participant.agent_id,
            permanent_role: participant.role,
            mandate: sentinel_workflow::collaboration_policy_mandate(participant.role),
            active: true,
            authority_scope_digest: assignment.canonical_digest().unwrap(),
            organization_generation: assignment.organization_generation,
            organization_digest: assignment.organization_digest.clone(),
            assignment_load: capacity
                .assignment_load
                .get(&participant.agent_id.0)
                .copied()
                .unwrap_or(0),
            assignment_limit: 8,
            capabilities: participant.specialties.clone(),
            privacy_classes: BTreeSet::from(["project-internal".to_owned()]),
            runtime_available: true,
            tools_available: true,
            model_family: format!("model-{}", participant.agent_id.0),
            prompt_digest: participant.profile.digest.clone(),
            tool_set_digest: format!("{:064x}", 100 + participant.agent_id.0),
            data_provenance_digest: format!("{:064x}", 200 + participant.agent_id.0),
            prior_claim_correlation_digest: None,
            queue_delay_ms: 0,
            estimated_cost_micros: 0,
        })
        .collect()
}

fn collaboration_admission_fence(
    project: &sentinel_workflow::ProjectV1,
    work_item_id: &WorkItemId,
) -> CollaborationAdmissionFenceV1 {
    let assignment = project.work_items[work_item_id]
        .assignments
        .iter()
        .find(|assignment| assignment.active)
        .unwrap();
    CollaborationAdmissionFenceV1 {
        organization_generation: assignment.organization_generation,
        organization_digest: assignment.organization_digest.clone(),
        assignment_id: assignment.assignment_id.clone(),
        assignment_version: assignment.assignment_version,
        assignment_digest: assignment.canonical_digest().unwrap(),
        behavior_policy_generation: project.governance.project_profile.generation,
        behavior_policy_digest: project.governance.project_profile.digest.clone(),
        collaboration_generation: project.collaboration_generation,
    }
}

fn admit_independent_review(
    state: &Journey,
    project: &sentinel_workflow::ProjectV1,
    work_item_id: &WorkItemId,
    operation: u128,
    now_ms: u64,
) -> sentinel_workflow::ProjectV1 {
    let owner = project.work_items[work_item_id]
        .assignments
        .iter()
        .find(|assignment| assignment.active)
        .unwrap()
        .agent_id;
    let project = project_command(
        &state.store,
        &state.pm,
        operation.checked_sub(1).unwrap(),
        CompanyWorkflowCommandV1::RecordQuestion {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: Some(work_item_id.clone()),
            owner,
            question_ref: "independent evidence is required before completion".to_owned(),
        },
        now_ms,
    );
    let input = collaboration_admission_input(&project, work_item_id, &state.store, now_ms);
    project_command(
        &state.store,
        &state.pm,
        operation,
        CompanyWorkflowCommandV1::AdmitCollaboration {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            source_request_digest: DIGEST.to_owned(),
            input,
            candidates: collaboration_admission_candidates(&project, work_item_id, &state.store),
            reliability: project.collaboration_reliability.clone(),
            expected_benefit_ref: "independent implementation review reduces defect risk"
                .to_owned(),
        },
        now_ms,
    )
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
fn capability_gap_requires_a_bound_collaboration_before_execution() {
    let state = journey();
    let work_item_id = WorkItemId::parse("capability-gap").unwrap();
    let rejected_work_item_id = WorkItemId::parse("foreign-capability").unwrap();
    let mut project = project_command(
        &state.store,
        &state.pm,
        43,
        CompanyWorkflowCommandV1::PlanWorkGraph {
            project_id: state.project_id.clone(),
            expected_version: 1,
            items: vec![
                work(
                    &work_item_id.0,
                    CompanyRoleV1::Developer,
                    &["qa", "rust"],
                    &[],
                    100,
                ),
                work(
                    &rejected_work_item_id.0,
                    CompanyRoleV1::Developer,
                    &["python"],
                    &[],
                    100,
                ),
            ],
        },
        43,
    );
    project = project_command(
        &state.store,
        &state.pm,
        44,
        CompanyWorkflowCommandV1::ActivateProject {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            reason_ref: "capability plan approved".to_owned(),
        },
        44,
    );
    project = project_command(
        &state.store,
        &state.pm,
        45,
        CompanyWorkflowCommandV1::AssignWork {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: work_item_id.clone(),
            agent_id: AgentId(2),
            organization_generation: 1,
            organization_digest: DIGEST.to_owned(),
            reason_ref: "developer owns the bounded capability gap".to_owned(),
        },
        45,
    );
    let work_item = &project.work_items[&work_item_id];
    let assignment = work_item.assignments.last().unwrap();
    assert!(
        !sentinel_workflow::execution_capability_coverage_is_admitted(
            &project, work_item, assignment,
        )
        .unwrap()
    );

    let input = collaboration_admission_input(&project, &work_item_id, &state.store, 46);
    assert!(input.directed_handoff_required);
    let candidates = collaboration_admission_candidates(&project, &work_item_id, &state.store);
    project = project_command(
        &state.store,
        &state.pm,
        46,
        CompanyWorkflowCommandV1::AdmitCollaboration {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            source_request_digest: DIGEST.to_owned(),
            input,
            candidates,
            reliability: Vec::new(),
            expected_benefit_ref: "qa closes the owner capability gap".to_owned(),
        },
        46,
    );
    let work_item = &project.work_items[&work_item_id];
    let assignment = work_item.assignments.last().unwrap();
    assert_eq!(
        project.collaboration_admissions[0].mode,
        CollaborationAdmissionModeV1::DirectedHandoff
    );
    assert!(
        sentinel_workflow::execution_capability_coverage_is_admitted(
            &project, work_item, assignment,
        )
        .unwrap()
    );

    let rejected = state
        .store
        .apply_company_command(
            &state.pm,
            Uuid::from_u128(47),
            &CompanyWorkflowCommandV1::AssignWork {
                project_id: state.project_id,
                expected_version: project.version,
                work_item_id: rejected_work_item_id,
                agent_id: AgentId(2),
                organization_generation: 1,
                organization_digest: DIGEST.to_owned(),
                reason_ref: "unrelated capability must fail".to_owned(),
            },
            47,
        )
        .unwrap_err();
    assert_eq!(rejected.code, WorkflowErrorCode::AuthorityConflict);
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
    project = admit_independent_review(&state, &project, &work_item_id, 3302, 303);
    let admission = project.collaboration_admissions[0].clone();
    let admission_contract_digest = admission.expected_session_contract_digest().unwrap();

    let authority = collaboration_authority(&project, &work_item_id);
    let unscoped = state.store.apply_company_command(
        &state.pm,
        Uuid::from_u128(3303),
        &CompanyWorkflowCommandV1::CreateCollaborationSession {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: None,
            admission_id: admission.admission_id.clone(),
            admission_contract_digest: admission_contract_digest.clone(),
            collaboration_generation: project.collaboration_generation,
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
            admission_id: admission.admission_id.clone(),
            admission_contract_digest: admission_contract_digest.clone(),
            collaboration_generation: project.collaboration_generation,
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

    let substituted_roster = state.store.apply_company_command(
        &state.pm,
        Uuid::from_u128(6303),
        &CompanyWorkflowCommandV1::CreateCollaborationSession {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: Some(work_item_id.clone()),
            admission_id: admission.admission_id.clone(),
            admission_contract_digest: admission_contract_digest.clone(),
            collaboration_generation: project.collaboration_generation,
            authority: authority.clone(),
            subject_ref: "caller-selected roster must fail".to_owned(),
            input_digest: DIGEST.to_owned(),
            mode: CollaborationModeV1::IndependentReview,
            budget: CollaborationBudgetV1 {
                max_participants: u16::try_from(admission.selected_agents.len()).unwrap(),
                max_claims: u16::try_from(admission.selected_agents.len()).unwrap(),
                max_handoffs: 1,
                max_clarification_rounds: 1,
                max_transitions: 39,
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
    assert_eq!(
        substituted_roster.unwrap_err().code,
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
            admission_id: admission.admission_id.clone(),
            admission_contract_digest,
            collaboration_generation: project.collaboration_generation,
            authority: authority.clone(),
            subject_ref: "produce and independently review the artifact".to_owned(),
            input_digest: DIGEST.to_owned(),
            mode: CollaborationModeV1::IndependentReview,
            budget: CollaborationBudgetV1 {
                max_participants: u16::try_from(admission.selected_agents.len()).unwrap(),
                max_claims: u16::try_from(admission.selected_agents.len()).unwrap(),
                max_handoffs: 3,
                max_clarification_rounds: 1,
                max_transitions: 39,
                deadline_unix_ms: 10_000,
            },
            participants: collaboration_participants_from_admission(&admission),
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

    let developer_claim = CompanyWorkflowCommandV1::RecordIndependentClaim {
        project_id: state.project_id.clone(),
        expected_version: project.version,
        session_id: session_id.clone(),
        expected_transition_sequence: 2,
        authority: authority.clone(),
        conclusion_ref: "the implementation is feasible".to_owned(),
        evidence: evidence("implementation evidence"),
        assumptions: vec!["toolchain remains pinned".to_owned()],
        uncertainty: UncertaintyClassV1::Low,
        confidence_basis: "source and contract inspection".to_owned(),
        capability_snapshot_digest: collaboration_capability_snapshot(
            &project,
            &session_id,
            AgentId(2),
        ),
        input_digest: DIGEST.to_owned(),
    };
    let first = state
        .store
        .apply_company_command(
            &state.developer,
            Uuid::from_u128(305),
            &developer_claim,
            305,
        )
        .unwrap();
    assert!(!first.replayed);
    let CompanyWorkflowResponseV1::Project(first_project) = first.response else {
        panic!("expected project response")
    };
    project = *first_project;
    let replay = state
        .store
        .apply_company_command(
            &state.developer,
            Uuid::from_u128(305),
            &developer_claim,
            306,
        )
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(project.collaboration_publications.len(), 4);

    project = project_command(
        &state.store,
        &state.qa,
        307,
        CompanyWorkflowCommandV1::RecordIndependentClaim {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            session_id: session_id.clone(),
            expected_transition_sequence: 3,
            authority: authority.clone(),
            conclusion_ref: "independent verification is required".to_owned(),
            evidence: evidence("quality evidence"),
            assumptions: Vec::new(),
            uncertainty: UncertaintyClassV1::Blocking,
            confidence_basis: "independent negative testing".to_owned(),
            capability_snapshot_digest: collaboration_capability_snapshot(
                &project,
                &session_id,
                AgentId(3),
            ),
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
                expected_transition_sequence: 4,
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
            expected_transition_sequence: 4,
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
        expected_transition_sequence: 5,
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
            expected_transition_sequence: 6,
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
            expected_transition_sequence: 7,
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
            expected_transition_sequence: 8,
            authority: authority.clone(),
            packet_id: packet_id.clone(),
            packet_digest: packet_digest.clone(),
            capability_snapshot_digest: collaboration_capability_snapshot(
                &project,
                &session_id,
                AgentId(3),
            ),
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
                expected_transition_sequence: 9,
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
            expected_transition_sequence: 9,
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
            expected_transition_sequence: 10,
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
            expected_transition_sequence: 11,
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
                expected_transition_sequence: 12,
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
            expected_transition_sequence: 12,
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
            expected_transition_sequence: 13,
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
                expected_transition_sequence: 14,
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
            expected_transition_sequence: 14,
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
                expected_transition_sequence: 15,
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
            expected_transition_sequence: 15,
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
            expected_transition_sequence: 16,
            authority,
            target: CollaborationSessionStateV1::Completed,
            reason_ref: "authorized decision is evidence-linked".to_owned(),
        },
        325,
    );

    let session = &project.collaboration_sessions[0];
    assert_eq!(session.state, CollaborationSessionStateV1::Completed);
    assert_eq!(session.transition_sequence, 17);
    assert_eq!(session.publication_revision, 17);
    assert_eq!(project.collaboration_publications.len(), 18);
    let session_publications = project
        .collaboration_publications
        .iter()
        .filter(|publication| publication.proposal.causal_context.correlation_id == session_id)
        .collect::<Vec<_>>();
    assert_eq!(session_publications.len(), 17);
    for (index, publication) in session_publications.iter().enumerate() {
        assert_eq!(publication.transition_sequence, (index + 1) as u64);
    }
}

#[test]
fn collaboration_uncertainty_uses_only_relevant_durable_questions_and_blockers() {
    let state = journey();
    let mut project = state
        .store
        .company_project(&TenantId::parse("tenant-a").unwrap(), &state.project_id)
        .unwrap()
        .unwrap();
    let target = WorkItemId::parse("target-work").unwrap();
    let unrelated = WorkItemId::parse("unrelated-work").unwrap();

    assert_eq!(
        collaboration_policy_uncertainty(&project, &target),
        UncertaintyClassV1::Low
    );
    project.questions.push(ProjectQuestionV1 {
        question_id: "question-target".to_owned(),
        work_item_id: Some(target.clone()),
        owner: AgentId(2),
        question_ref: "question-ref".to_owned(),
        resolution_ref: None,
        created_by: "pm-a".to_owned(),
        resolved_by: None,
        created_at_unix_ms: 1,
        updated_at_unix_ms: 1,
    });
    assert_eq!(
        collaboration_policy_uncertainty(&project, &target),
        UncertaintyClassV1::Material
    );
    project.questions[0].resolution_ref = Some("answer-ref".to_owned());
    project.blockers.push(BlockerV1 {
        blocker_id: "blocker-unrelated".to_owned(),
        work_item_id: Some(unrelated),
        cause_ref: "unrelated-cause".to_owned(),
        owner: AgentId(2),
        escalation_target: Some(AgentId(1)),
        state: BlockerStateV1::Escalated,
        blocker_kind: BlockerKindV1::Operational,
        blocked_from_state: None,
        resolution_ref: None,
        last_actor_id: "pm-a".to_owned(),
        created_at_unix_ms: 1,
        updated_at_unix_ms: 1,
        transition_history: Vec::new(),
    });
    assert_eq!(
        collaboration_policy_uncertainty(&project, &target),
        UncertaintyClassV1::Low
    );
    project.blockers.push(BlockerV1 {
        blocker_id: "blocker-global".to_owned(),
        work_item_id: None,
        cause_ref: "global-cause".to_owned(),
        owner: AgentId(2),
        escalation_target: Some(AgentId(1)),
        state: BlockerStateV1::Open,
        blocker_kind: BlockerKindV1::Operational,
        blocked_from_state: None,
        resolution_ref: None,
        last_actor_id: "pm-a".to_owned(),
        created_at_unix_ms: 1,
        updated_at_unix_ms: 1,
        transition_history: Vec::new(),
    });
    assert_eq!(
        collaboration_policy_uncertainty(&project, &target),
        UncertaintyClassV1::Material
    );
    project.blockers[1].state = BlockerStateV1::Escalated;
    assert_eq!(
        collaboration_policy_uncertainty(&project, &target),
        UncertaintyClassV1::Blocking
    );
    project.blockers[1].state = BlockerStateV1::Resolved;
    assert_eq!(
        collaboration_policy_uncertainty(&project, &target),
        UncertaintyClassV1::Low
    );
}

#[test]
fn collaboration_admission_is_solo_first_durable_fenced_and_exactly_replayable() {
    let state = journey();
    let work_item_id = WorkItemId::parse("admission-work").unwrap();
    let mut project = project_command(
        &state.store,
        &state.pm,
        500,
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
        500,
    );
    project = project_command(
        &state.store,
        &state.pm,
        501,
        CompanyWorkflowCommandV1::ActivateProject {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            reason_ref: "admission plan approved".to_owned(),
        },
        501,
    );
    project = project_command(
        &state.store,
        &state.pm,
        502,
        CompanyWorkflowCommandV1::AssignWork {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: work_item_id.clone(),
            agent_id: AgentId(2),
            organization_generation: 1,
            organization_digest: DIGEST.to_owned(),
            reason_ref: "single accountable owner".to_owned(),
        },
        502,
    );

    let admission_input = collaboration_admission_input(&project, &work_item_id, &state.store, 503);
    let candidates = collaboration_admission_candidates(&project, &work_item_id, &state.store);
    let mut forged_uncertainty = admission_input.clone();
    forged_uncertainty.uncertainty = UncertaintyClassV1::Material;
    forged_uncertainty.separation_requirements = collaboration_policy_separation_requirements(
        CompanyRoleV1::Developer,
        forged_uncertainty.task_risk,
        forged_uncertainty.ambiguity,
        forged_uncertainty.uncertainty,
        forged_uncertainty.evidence_conflict,
    );
    let error = state
        .store
        .apply_company_command(
            &state.pm,
            Uuid::from_u128(499),
            &CompanyWorkflowCommandV1::AdmitCollaboration {
                project_id: state.project_id.clone(),
                expected_version: project.version,
                source_request_digest: DIGEST.to_owned(),
                input: forged_uncertainty,
                candidates: candidates.clone(),
                reliability: Vec::new(),
                expected_benefit_ref: "caller cannot override derived uncertainty".to_owned(),
            },
            503,
        )
        .unwrap_err();
    assert_eq!(error.code, WorkflowErrorCode::AuthorityConflict);
    assert_eq!(
        state
            .store
            .company_project(&project.tenant_id, &project.project_id)
            .unwrap(),
        Some(project.clone())
    );
    let mut strengthened_risk = admission_input.clone();
    strengthened_risk.task_risk = TaskRiskV1::High;
    strengthened_risk.separation_requirements = collaboration_policy_separation_requirements(
        CompanyRoleV1::Developer,
        strengthened_risk.task_risk,
        strengthened_risk.ambiguity,
        strengthened_risk.uncertainty,
        strengthened_risk.evidence_conflict,
    );
    let mut injected_conflict = admission_input.clone();
    injected_conflict.evidence_conflict = true;
    injected_conflict.separation_requirements = collaboration_policy_separation_requirements(
        CompanyRoleV1::Developer,
        injected_conflict.task_risk,
        injected_conflict.ambiguity,
        injected_conflict.uncertainty,
        injected_conflict.evidence_conflict,
    );
    let mut narrowed_budget = admission_input.clone();
    narrowed_budget.remaining_cost_budget_micros -= 1;
    let mut forced_human = admission_input.clone();
    forced_human.human_approval_required = true;
    for (operation, input) in [
        (4_991, strengthened_risk),
        (4_992, injected_conflict),
        (4_993, narrowed_budget),
        (4_994, forced_human),
    ] {
        let error = state
            .store
            .apply_company_command(
                &state.pm,
                Uuid::from_u128(operation),
                &CompanyWorkflowCommandV1::AdmitCollaboration {
                    project_id: state.project_id.clone(),
                    expected_version: project.version,
                    source_request_digest: DIGEST.to_owned(),
                    input,
                    candidates: candidates.clone(),
                    reliability: Vec::new(),
                    expected_benefit_ref: "caller cannot alter derived admission policy".to_owned(),
                },
                503,
            )
            .unwrap_err();
        assert_eq!(error.code, WorkflowErrorCode::AuthorityConflict);
    }
    let mut forged_candidates = candidates.clone();
    forged_candidates[0].mandate = BehaviorMandateV1::Verify;
    let error = state
        .store
        .apply_company_command(
            &state.pm,
            Uuid::from_u128(4_995),
            &CompanyWorkflowCommandV1::AdmitCollaboration {
                project_id: state.project_id.clone(),
                expected_version: project.version,
                source_request_digest: DIGEST.to_owned(),
                input: admission_input.clone(),
                candidates: forged_candidates,
                reliability: Vec::new(),
                expected_benefit_ref: "caller cannot alter the role mandate".to_owned(),
            },
            503,
        )
        .unwrap_err();
    assert_eq!(error.code, WorkflowErrorCode::AuthorityConflict);
    assert_eq!(
        state
            .store
            .company_project(&project.tenant_id, &project.project_id)
            .unwrap(),
        Some(project.clone())
    );
    let operation_id = Uuid::from_u128(503);
    let admission_command = CompanyWorkflowCommandV1::AdmitCollaboration {
        project_id: state.project_id.clone(),
        expected_version: project.version,
        source_request_digest: OTHER_DIGEST.to_owned(),
        input: admission_input.clone(),
        candidates: candidates.clone(),
        reliability: Vec::new(),
        expected_benefit_ref: "solo baseline avoids coordination overhead".to_owned(),
    };
    let admitted = state
        .store
        .apply_company_command(&state.pm, operation_id, &admission_command, 503)
        .unwrap();
    let admitted_response = admitted.response.clone();
    let CompanyWorkflowResponseV1::Project(admitted_project) = admitted.response else {
        panic!("expected project response")
    };
    project = *admitted_project;
    assert_eq!(project.collaboration_generation, 2);
    assert_eq!(project.collaboration_admissions.len(), 1);
    let decision = &project.collaboration_admissions[0];
    assert_eq!(decision.mode, CollaborationAdmissionModeV1::Solo);
    assert_eq!(decision.state, CollaborationAdmissionStateV1::Admitted);
    assert_eq!(decision.selected_agents, vec![AgentId(2)]);
    assert_eq!(decision.publication_revision, 1);
    assert_eq!(decision.request_bindings.len(), 1);
    assert_eq!(project.collaboration_publications.len(), 1);
    assert_admission_snapshot_readback(&state, &project);
    assert_eq!(
        state
            .store
            .collaboration_capacity_snapshot(&project.tenant_id, &project.project_id)
            .unwrap()
            .reserved_load
            .get(&2),
        Some(&1)
    );

    let replay = state
        .store
        .apply_company_command(&state.pm, operation_id, &admission_command, 9_000)
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(
        replay.response,
        CompanyWorkflowResponseV1::Project(Box::new(project.clone()))
    );

    let before_rejection = project.clone();
    let mut incomplete_candidates = candidates;
    incomplete_candidates.pop();
    let omitted_candidate = state
        .store
        .apply_company_command(
            &state.pm,
            Uuid::from_u128(504),
            &CompanyWorkflowCommandV1::AdmitCollaboration {
                project_id: state.project_id.clone(),
                expected_version: project.version,
                source_request_digest: DIGEST.to_owned(),
                input: collaboration_admission_input(&project, &work_item_id, &state.store, 504),
                candidates: incomplete_candidates,
                reliability: Vec::new(),
                expected_benefit_ref: "caller-selected roster must fail".to_owned(),
            },
            504,
        )
        .unwrap_err();
    assert_eq!(omitted_candidate.code, WorkflowErrorCode::AuthorityConflict);
    assert_eq!(
        state
            .store
            .company_project(&project.tenant_id, &project.project_id)
            .unwrap(),
        Some(before_rejection)
    );

    let decision = project.collaboration_admissions[0].clone();
    let stale_progress = state
        .store
        .apply_company_command(
            &state.developer,
            Uuid::from_u128(505),
            &CompanyWorkflowCommandV1::ProgressCollaborationAdmission {
                project_id: state.project_id.clone(),
                expected_version: project.version,
                source_request_digest: DIGEST.to_owned(),
                admission_id: decision.admission_id.clone(),
                fence: CollaborationAdmissionFenceV1 {
                    organization_generation: decision.input.organization_generation,
                    organization_digest: decision.input.organization_digest.clone(),
                    assignment_id: decision.input.assignment_id.clone(),
                    assignment_version: decision.input.assignment_version,
                    assignment_digest: decision.input.assignment_digest.clone(),
                    behavior_policy_generation: decision.input.behavior_policy_generation,
                    behavior_policy_digest: decision.input.behavior_policy_digest.clone(),
                    collaboration_generation: decision.input.collaboration_generation,
                },
                progress: CollaborationProgressV1 {
                    expected_transition_sequence: decision.transition_sequence,
                    rounds_delta: 1,
                    tokens_delta: 10,
                    cost_delta_micros: 0,
                    novelty_micros: 1_000_000,
                    novelty_digest: DIGEST.to_owned(),
                    milestone_digest: None,
                    work_digest: None,
                    disposition: CollaborationProgressDispositionV1::Continue,
                    reason_ref: "stale collaboration generation".to_owned(),
                },
            },
            505,
        )
        .unwrap_err();
    assert_eq!(stale_progress.code, WorkflowErrorCode::AuthorityConflict);

    let assignment = project.work_items[&work_item_id]
        .assignments
        .iter()
        .find(|assignment| assignment.active)
        .unwrap();
    project = project_command(
        &state.store,
        &state.developer,
        506,
        CompanyWorkflowCommandV1::ProgressCollaborationAdmission {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            source_request_digest: DIGEST.to_owned(),
            admission_id: decision.admission_id,
            fence: CollaborationAdmissionFenceV1 {
                organization_generation: assignment.organization_generation,
                organization_digest: assignment.organization_digest.clone(),
                assignment_id: assignment.assignment_id.clone(),
                assignment_version: assignment.assignment_version,
                assignment_digest: assignment.canonical_digest().unwrap(),
                behavior_policy_generation: project.governance.project_profile.generation,
                behavior_policy_digest: project.governance.project_profile.digest.clone(),
                collaboration_generation: project.collaboration_generation,
            },
            progress: CollaborationProgressV1 {
                expected_transition_sequence: decision.transition_sequence,
                rounds_delta: 1,
                tokens_delta: 10,
                cost_delta_micros: 0,
                novelty_micros: 1_000_000,
                novelty_digest: OTHER_DIGEST.to_owned(),
                milestone_digest: Some(DIGEST.to_owned()),
                work_digest: Some(OTHER_DIGEST.to_owned()),
                disposition: CollaborationProgressDispositionV1::Complete,
                reason_ref: "bounded solo task completed".to_owned(),
            },
        },
        506,
    );
    let decision = &project.collaboration_admissions[0];
    assert_eq!(decision.state, CollaborationAdmissionStateV1::Completed);
    assert_eq!(decision.publication_revision, 2);
    assert!(decision.reservations.iter().all(|value| value.released));
    assert_eq!(project.collaboration_publications.len(), 2);
    assert_admission_snapshot_readback(&state, &project);
    assert!(state
        .store
        .collaboration_capacity_snapshot(&project.tenant_id, &project.project_id)
        .unwrap()
        .reserved_load
        .is_empty());
    assert_eq!(
        state
            .store
            .company_operation_response(&state.pm, operation_id)
            .unwrap(),
        Some(admitted_response)
    );
}

#[test]
fn concurrent_admissions_commit_once_and_active_followup_cannot_double_reserve() {
    let state = journey();
    let work_item_id = WorkItemId::parse("concurrent-admission-work").unwrap();
    let mut project = project_command(
        &state.store,
        &state.pm,
        600,
        CompanyWorkflowCommandV1::PlanWorkGraph {
            project_id: state.project_id.clone(),
            expected_version: 1,
            items: vec![work(
                &work_item_id.0,
                CompanyRoleV1::Developer,
                &["rust"],
                &[],
                1_000,
            )],
        },
        600,
    );
    project = project_command(
        &state.store,
        &state.pm,
        601,
        CompanyWorkflowCommandV1::ActivateProject {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            reason_ref: "concurrent admission plan approved".to_owned(),
        },
        601,
    );
    project = project_command(
        &state.store,
        &state.pm,
        602,
        CompanyWorkflowCommandV1::AssignWork {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: work_item_id.clone(),
            agent_id: AgentId(2),
            organization_generation: 1,
            organization_digest: DIGEST.to_owned(),
            reason_ref: "one accountable admission owner".to_owned(),
        },
        602,
    );
    project = project_command(
        &state.store,
        &state.pm,
        7_602,
        CompanyWorkflowCommandV1::RecordQuestion {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: Some(work_item_id.clone()),
            owner: AgentId(2),
            question_ref: "concurrent admission requires independent evidence".to_owned(),
        },
        603,
    );
    let input = collaboration_admission_input(&project, &work_item_id, &state.store, 603);
    let mut candidates = collaboration_admission_candidates(&project, &work_item_id, &state.store);
    for candidate in &mut candidates {
        candidate.estimated_cost_micros = if matches!(candidate.agent_id.0, 2 | 3) {
            400
        } else {
            0
        };
    }
    let command = CompanyWorkflowCommandV1::AdmitCollaboration {
        project_id: state.project_id.clone(),
        expected_version: project.version,
        source_request_digest: DIGEST.to_owned(),
        input,
        candidates,
        reliability: Vec::new(),
        expected_benefit_ref: "reserve the smallest eligible team once".to_owned(),
    };
    let pm = state.pm.clone();
    let project_id = state.project_id.clone();
    let tenant_id = pm.tenant_id.clone();
    let store = Arc::new(state.store);
    let barrier = Arc::new(Barrier::new(3));
    let mut handles = Vec::new();
    for operation in [603_u128, 604] {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        let pm = pm.clone();
        let command = command.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            store.apply_company_command(&pm, Uuid::from_u128(operation), &command, 603)
        }));
    }
    barrier.wait();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);

    let project = store
        .company_project(&tenant_id, &project_id)
        .unwrap()
        .unwrap();
    assert_eq!(project.collaboration_admissions.len(), 1);
    assert_eq!(project.collaboration_publications.len(), 1);
    assert_eq!(project.collaboration_generation, 2);
    assert_eq!(
        project.collaboration_admissions[0].mode,
        CollaborationAdmissionModeV1::ParallelIndependentReview
    );
    assert_eq!(
        project.collaboration_admissions[0].selected_agents,
        vec![AgentId(2), AgentId(3)]
    );
    let capacity = store
        .collaboration_capacity_snapshot(&tenant_id, &project_id)
        .unwrap();
    assert_eq!(capacity.reserved_load.get(&2), Some(&1));
    assert_eq!(capacity.reserved_load.get(&3), Some(&1));
    assert_eq!(capacity.project_reserved_cost_micros, 1_000);
    let current = store
        .company_project(&tenant_id, &project_id)
        .unwrap()
        .unwrap();
    let second = store
        .apply_company_command(
            &pm,
            Uuid::from_u128(605),
            &CompanyWorkflowCommandV1::AdmitCollaboration {
                project_id,
                expected_version: current.version,
                source_request_digest: OTHER_DIGEST.to_owned(),
                input: collaboration_admission_input(&current, &work_item_id, &store, 605),
                candidates: collaboration_admission_candidates(&current, &work_item_id, &store),
                reliability: Vec::new(),
                expected_benefit_ref: "the reserved cost ceiling cannot be consumed twice"
                    .to_owned(),
            },
            605,
        )
        .unwrap_err();
    assert_eq!(second.code, WorkflowErrorCode::InvalidInput);
    let capacity_after_fallback = store
        .collaboration_capacity_snapshot(&tenant_id, &current.project_id)
        .unwrap();
    assert_eq!(capacity_after_fallback.project_reserved_cost_micros, 1_000);
    assert_eq!(capacity_after_fallback.reserved_load.get(&2), Some(&1));
    assert_eq!(capacity_after_fallback.reserved_load.get(&3), Some(&1));
    assert_eq!(
        store
            .company_project(&tenant_id, &current.project_id)
            .unwrap()
            .unwrap(),
        current
    );
}

#[test]
fn unrelated_admission_does_not_revoke_an_exact_bound_session() {
    let state = journey();
    let first_work = WorkItemId::parse("admission-work-a").unwrap();
    let second_work = WorkItemId::parse("admission-work-b").unwrap();
    let mut project = project_command(
        &state.store,
        &state.pm,
        650,
        CompanyWorkflowCommandV1::PlanWorkGraph {
            project_id: state.project_id.clone(),
            expected_version: 1,
            items: vec![
                work(&first_work.0, CompanyRoleV1::Developer, &["rust"], &[], 100),
                work(
                    &second_work.0,
                    CompanyRoleV1::Developer,
                    &["rust"],
                    &[],
                    100,
                ),
            ],
        },
        650,
    );
    project = project_command(
        &state.store,
        &state.pm,
        651,
        CompanyWorkflowCommandV1::ActivateProject {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            reason_ref: "independent admission scopes approved".to_owned(),
        },
        651,
    );
    for (operation, work_item) in [(652, &first_work), (653, &second_work)] {
        project = project_command(
            &state.store,
            &state.pm,
            operation,
            CompanyWorkflowCommandV1::AssignWork {
                project_id: state.project_id.clone(),
                expected_version: project.version,
                work_item_id: work_item.clone(),
                agent_id: AgentId(2),
                organization_generation: 1,
                organization_digest: DIGEST.to_owned(),
                reason_ref: "one accountable owner per work item".to_owned(),
            },
            operation as u64,
        );
    }

    project = admit_independent_review(&state, &project, &first_work, 7_654, 654);
    let first_admission = project.collaboration_admissions[0].clone();
    let first_generation = project.collaboration_generation;
    assert!(first_admission.selected_agents.contains(&AgentId(3)));
    let unauthorized_terminal = state
        .store
        .apply_company_command(
            &state.qa,
            Uuid::from_u128(7_654),
            &CompanyWorkflowCommandV1::ProgressCollaborationAdmission {
                project_id: state.project_id.clone(),
                expected_version: project.version,
                source_request_digest: DIGEST.to_owned(),
                admission_id: first_admission.admission_id.clone(),
                fence: CollaborationAdmissionFenceV1 {
                    organization_generation: first_admission.input.organization_generation,
                    organization_digest: first_admission.input.organization_digest.clone(),
                    assignment_id: first_admission.input.assignment_id.clone(),
                    assignment_version: first_admission.input.assignment_version,
                    assignment_digest: first_admission.input.assignment_digest.clone(),
                    behavior_policy_generation: first_admission.input.behavior_policy_generation,
                    behavior_policy_digest: first_admission.input.behavior_policy_digest.clone(),
                    collaboration_generation: project.collaboration_generation,
                },
                progress: CollaborationProgressV1 {
                    expected_transition_sequence: first_admission.transition_sequence,
                    rounds_delta: 1,
                    tokens_delta: 0,
                    cost_delta_micros: 0,
                    novelty_micros: 1_000_000,
                    novelty_digest: OTHER_DIGEST.to_owned(),
                    milestone_digest: Some(DIGEST.to_owned()),
                    work_digest: Some(OTHER_DIGEST.to_owned()),
                    disposition: CollaborationProgressDispositionV1::Complete,
                    reason_ref: "reviewer cannot complete the owner's admission".to_owned(),
                },
            },
            654,
        )
        .unwrap_err();
    assert_eq!(
        unauthorized_terminal.code,
        WorkflowErrorCode::AuthorityConflict
    );
    assert_eq!(
        state
            .store
            .company_project(&project.tenant_id, &project.project_id)
            .unwrap(),
        Some(project.clone())
    );
    let second_input = collaboration_admission_input(&project, &second_work, &state.store, 655);
    project = project_command(
        &state.store,
        &state.pm,
        655,
        CompanyWorkflowCommandV1::AdmitCollaboration {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            source_request_digest: OTHER_DIGEST.to_owned(),
            input: second_input,
            candidates: collaboration_admission_candidates(&project, &second_work, &state.store),
            reliability: project.collaboration_reliability.clone(),
            expected_benefit_ref: "second work item remains solo".to_owned(),
        },
        655,
    );
    assert!(project.collaboration_generation > first_generation);
    let authority = collaboration_authority(&project, &first_work);
    project = project_command(
        &state.store,
        &state.pm,
        656,
        CompanyWorkflowCommandV1::CreateCollaborationSession {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: Some(first_work.clone()),
            admission_id: first_admission.admission_id.clone(),
            admission_contract_digest: first_admission.expected_session_contract_digest().unwrap(),
            collaboration_generation: first_generation,
            authority: authority.clone(),
            subject_ref: "session remains scoped to its own admission".to_owned(),
            input_digest: DIGEST.to_owned(),
            mode: CollaborationModeV1::IndependentReview,
            budget: CollaborationBudgetV1 {
                max_participants: u16::try_from(first_admission.selected_agents.len()).unwrap(),
                max_claims: u16::try_from(first_admission.selected_agents.len()).unwrap(),
                max_handoffs: 1,
                max_clarification_rounds: 1,
                max_transitions: 12,
                deadline_unix_ms: 10_000,
            },
            participants: collaboration_participants_from_admission(&first_admission),
        },
        656,
    );
    let session_id = project.collaboration_sessions[0].session_id.clone();
    let retained_admission = project
        .collaboration_admissions
        .iter()
        .find(|candidate| candidate.admission_id == first_admission.admission_id)
        .unwrap();
    let retained_session = project
        .collaboration_sessions
        .iter()
        .find(|candidate| candidate.session_id == session_id)
        .unwrap();
    let retained_assignment = project.work_items[&first_work]
        .assignments
        .iter()
        .find(|candidate| candidate.active)
        .unwrap();
    retained_admission.validate(657).unwrap();
    assert_eq!(retained_assignment.assignment_id, authority.assignment_id);
    assert_eq!(
        retained_assignment.assignment_version,
        authority.assignment_version
    );
    assert_eq!(
        retained_assignment.canonical_digest().unwrap(),
        authority.assignment_digest
    );
    assert_eq!(
        retained_session.organization_generation,
        authority.organization_generation
    );
    assert_eq!(
        retained_session.organization_digest,
        authority.organization_digest
    );
    assert_eq!(retained_session.assignment_id, authority.assignment_id);
    assert_eq!(
        retained_session.assignment_version,
        authority.assignment_version
    );
    assert_eq!(
        retained_session.assignment_digest,
        authority.assignment_digest
    );
    assert_eq!(retained_session.policy_version, authority.policy_version);
    assert_eq!(retained_session.policy_digest, authority.policy_digest);
    assert_eq!(
        retained_session.collaboration_generation,
        Some(first_generation)
    );
    assert_eq!(
        retained_admission.input.collaboration_generation + 1,
        first_generation
    );
    assert_eq!(
        retained_admission
            .expected_session_contract_digest()
            .unwrap(),
        retained_session.admission_contract_digest.clone().unwrap()
    );
    assert_eq!(
        retained_session.participants,
        collaboration_participants_from_admission(retained_admission)
    );
    assert_eq!(retained_session.admission_routes, retained_admission.routes);

    project = project_command(
        &state.store,
        &state.pm,
        657,
        CompanyWorkflowCommandV1::TransitionCollaborationSession {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            session_id: session_id.clone(),
            expected_transition_sequence: 1,
            authority,
            target: CollaborationSessionStateV1::CollectingIndependentClaims,
            reason_ref: "unrelated admission cannot revoke this session".to_owned(),
        },
        657,
    );
    assert_eq!(
        project.collaboration_sessions[0].state,
        CollaborationSessionStateV1::CollectingIndependentClaims
    );
    assert_eq!(
        project.collaboration_sessions[0].collaboration_generation,
        Some(first_generation)
    );

    let dispatched =
        compile_collaboration_gateway_request(&project, &session_id, AgentId(2), 657).unwrap();
    authorize_collaboration_gateway_result(&project, &dispatched, 657).unwrap();
    let deadline = project.collaboration_sessions[0].budget.deadline_unix_ms;
    assert_eq!(
        compile_collaboration_gateway_request(&project, &session_id, AgentId(2), deadline)
            .unwrap_err()
            .code,
        WorkflowErrorCode::AuthorityConflict
    );
    assert_eq!(
        authorize_collaboration_gateway_result(&project, &dispatched, deadline)
            .unwrap_err()
            .code,
        WorkflowErrorCode::AuthorityConflict
    );
    for stale_project in [
        {
            let mut value = project.clone();
            value.work_items.get_mut(&first_work).unwrap().assignments[0]
                .organization_generation += 1;
            value
        },
        {
            let mut value = project.clone();
            value.work_items.get_mut(&first_work).unwrap().assignments[0].assignment_version += 1;
            value
        },
        {
            let mut value = project.clone();
            value.governance.project_profile.generation += 1;
            value
        },
        {
            let mut value = project.clone();
            let admission = value
                .collaboration_admissions
                .iter_mut()
                .find(|candidate| candidate.admission_id == first_admission.admission_id)
                .unwrap();
            admission.input.collaboration_generation += 1;
            admission.refresh_digest().unwrap();
            value
        },
    ] {
        assert_eq!(
            compile_collaboration_gateway_request(&stale_project, &session_id, AgentId(2), 657)
                .unwrap_err()
                .code,
            WorkflowErrorCode::AuthorityConflict
        );
        assert_eq!(
            authorize_collaboration_gateway_result(&stale_project, &dispatched, 657)
                .unwrap_err()
                .code,
            WorkflowErrorCode::AuthorityConflict
        );
    }
    let mut transitioned = project.clone();
    transitioned.collaboration_sessions[0].transition_sequence += 1;
    transitioned.collaboration_sessions[0].publication_revision += 1;
    transitioned.collaboration_sessions[0].updated_at_unix_ms += 1;
    assert_eq!(
        authorize_collaboration_gateway_result(&transitioned, &dispatched, 658)
            .unwrap_err()
            .code,
        WorkflowErrorCode::AuthorityConflict
    );

    let active_admission = project
        .collaboration_admissions
        .iter()
        .find(|candidate| candidate.admission_id == first_admission.admission_id)
        .unwrap()
        .clone();
    let active_assignment = project.work_items[&first_work]
        .assignments
        .iter()
        .find(|candidate| candidate.active)
        .unwrap();
    project = project_command(
        &state.store,
        &state.pm,
        658,
        CompanyWorkflowCommandV1::ProgressCollaborationAdmission {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            source_request_digest: OTHER_DIGEST.to_owned(),
            admission_id: active_admission.admission_id.clone(),
            fence: CollaborationAdmissionFenceV1 {
                organization_generation: active_assignment.organization_generation,
                organization_digest: active_assignment.organization_digest.clone(),
                assignment_id: active_assignment.assignment_id.clone(),
                assignment_version: active_assignment.assignment_version,
                assignment_digest: active_assignment.canonical_digest().unwrap(),
                behavior_policy_generation: project.governance.project_profile.generation,
                behavior_policy_digest: project.governance.project_profile.digest.clone(),
                collaboration_generation: project.collaboration_generation,
            },
            progress: CollaborationProgressV1 {
                expected_transition_sequence: active_admission.transition_sequence,
                rounds_delta: 1,
                tokens_delta: 0,
                cost_delta_micros: 0,
                novelty_micros: 1_000_000,
                novelty_digest: DIGEST.to_owned(),
                milestone_digest: Some(OTHER_DIGEST.to_owned()),
                work_digest: None,
                disposition: CollaborationProgressDispositionV1::Cancel,
                reason_ref: "replace the work-scoped admission atomically".to_owned(),
            },
        },
        658,
    );
    assert!(project
        .collaboration_admissions
        .iter()
        .find(|candidate| candidate.admission_id == first_admission.admission_id)
        .unwrap()
        .reservations
        .iter()
        .all(|reservation| reservation.released));

    let next_input = collaboration_admission_input(&project, &first_work, &state.store, 659);
    project = project_command(
        &state.store,
        &state.pm,
        659,
        CompanyWorkflowCommandV1::AdmitCollaboration {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            source_request_digest: DIGEST.to_owned(),
            input: next_input,
            candidates: collaboration_admission_candidates(&project, &first_work, &state.store),
            reliability: project.collaboration_reliability.clone(),
            expected_benefit_ref: "new evidence requires a fresh work-scoped decision".to_owned(),
        },
        659,
    );
    let current_fence = collaboration_admission_fence(&project, &first_work);
    let before_stale_progress = project.clone();
    let error = state
        .store
        .apply_company_command(
            &state.developer,
            Uuid::from_u128(660),
            &CompanyWorkflowCommandV1::ProgressCollaborationAdmission {
                project_id: state.project_id.clone(),
                expected_version: project.version,
                source_request_digest: OTHER_DIGEST.to_owned(),
                admission_id: first_admission.admission_id,
                fence: current_fence,
                progress: CollaborationProgressV1 {
                    expected_transition_sequence: first_admission.transition_sequence,
                    rounds_delta: 1,
                    tokens_delta: 0,
                    cost_delta_micros: 0,
                    novelty_micros: 1_000_000,
                    novelty_digest: DIGEST.to_owned(),
                    milestone_digest: None,
                    work_digest: None,
                    disposition: CollaborationProgressDispositionV1::Continue,
                    reason_ref: "superseded work decision cannot progress".to_owned(),
                },
            },
            660,
        )
        .unwrap_err();
    assert_eq!(error.code, WorkflowErrorCode::AuthorityConflict);
    assert_eq!(
        state
            .store
            .company_project(&project.tenant_id, &project.project_id)
            .unwrap(),
        Some(before_stale_progress)
    );
}

#[test]
fn reliability_requires_attributed_claim_accepted_output_and_independent_gate() {
    let state = journey();
    let work_item_id = WorkItemId::parse("verified-reliability-work").unwrap();
    let mut project = project_command(
        &state.store,
        &state.pm,
        700,
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
        700,
    );
    project = project_command(
        &state.store,
        &state.pm,
        701,
        CompanyWorkflowCommandV1::ActivateProject {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            reason_ref: "reliability evidence plan approved".to_owned(),
        },
        701,
    );
    project = project_command(
        &state.store,
        &state.pm,
        702,
        CompanyWorkflowCommandV1::AssignWork {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: work_item_id.clone(),
            agent_id: AgentId(2),
            organization_generation: 1,
            organization_digest: DIGEST.to_owned(),
            reason_ref: "reliability attribution owner".to_owned(),
        },
        702,
    );
    project = admit_independent_review(&state, &project, &work_item_id, 7702, 703);
    let admission = project.collaboration_admissions[0].clone();
    let authority = collaboration_authority(&project, &work_item_id);
    let mut widened_budget = CollaborationBudgetV1 {
        max_participants: 3,
        max_claims: 3,
        max_handoffs: 1,
        max_clarification_rounds: 1,
        max_transitions: 12,
        deadline_unix_ms: 10_000,
    };
    let participants = collaboration_participants_from_admission(&admission);
    let participant_count = u16::try_from(participants.len()).unwrap();
    let mut altered_participants = participants.clone();
    altered_participants[0].capability_snapshot_digest = OTHER_DIGEST.to_owned();
    for (operation, budget, candidate_participants) in [
        (7_720, widened_budget.clone(), altered_participants),
        {
            widened_budget.max_participants = 4;
            widened_budget.max_claims = 4;
            (7_721, widened_budget.clone(), participants.clone())
        },
        {
            widened_budget.max_participants = 3;
            widened_budget.max_claims = 4;
            (7_722, widened_budget.clone(), participants.clone())
        },
        {
            widened_budget.max_claims = 3;
            widened_budget.max_handoffs = 5;
            (7_723, widened_budget.clone(), participants.clone())
        },
        {
            widened_budget.max_handoffs = 1;
            widened_budget.max_transitions = 41;
            (7_724, widened_budget.clone(), participants.clone())
        },
        {
            widened_budget.max_transitions = 12;
            widened_budget.deadline_unix_ms = admission.input.budget.deadline_unix_ms + 1;
            (7_725, widened_budget.clone(), participants.clone())
        },
    ] {
        let rejected = state
            .store
            .apply_company_command(
                &state.pm,
                Uuid::from_u128(operation),
                &CompanyWorkflowCommandV1::CreateCollaborationSession {
                    project_id: state.project_id.clone(),
                    expected_version: project.version,
                    work_item_id: Some(work_item_id.clone()),
                    admission_id: admission.admission_id.clone(),
                    admission_contract_digest: admission
                        .expected_session_contract_digest()
                        .unwrap(),
                    collaboration_generation: project.collaboration_generation,
                    authority: authority.clone(),
                    subject_ref: "reject a widened admission session".to_owned(),
                    input_digest: DIGEST.to_owned(),
                    mode: CollaborationModeV1::IndependentReview,
                    budget,
                    participants: candidate_participants,
                },
                703,
            )
            .unwrap_err();
        assert_eq!(rejected.code, WorkflowErrorCode::AuthorityConflict);
    }
    project = project_command(
        &state.store,
        &state.pm,
        703,
        CompanyWorkflowCommandV1::CreateCollaborationSession {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: Some(work_item_id.clone()),
            admission_id: admission.admission_id.clone(),
            admission_contract_digest: admission.expected_session_contract_digest().unwrap(),
            collaboration_generation: project.collaboration_generation,
            authority: authority.clone(),
            subject_ref: "bind one implementation claim to accepted work".to_owned(),
            input_digest: DIGEST.to_owned(),
            mode: CollaborationModeV1::IndependentReview,
            budget: CollaborationBudgetV1 {
                max_participants: participant_count,
                max_claims: participant_count,
                max_handoffs: 1,
                max_clarification_rounds: 1,
                max_transitions: 12,
                deadline_unix_ms: 10_000,
            },
            participants,
        },
        703,
    );
    let session_id = project.collaboration_sessions[0].session_id.clone();
    project = project_command(
        &state.store,
        &state.pm,
        704,
        CompanyWorkflowCommandV1::TransitionCollaborationSession {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            session_id: session_id.clone(),
            expected_transition_sequence: 1,
            authority: authority.clone(),
            target: CollaborationSessionStateV1::CollectingIndependentClaims,
            reason_ref: "collect the attributed claim privately".to_owned(),
        },
        704,
    );
    project = project_command(
        &state.store,
        &state.developer,
        705,
        CompanyWorkflowCommandV1::RecordIndependentClaim {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            session_id: session_id.clone(),
            expected_transition_sequence: 2,
            authority,
            conclusion_ref: "the implementation satisfies the accepted contract".to_owned(),
            evidence: evidence("accepted implementation evidence"),
            assumptions: vec!["the independent gate remains authoritative".to_owned()],
            uncertainty: UncertaintyClassV1::Low,
            confidence_basis: "content-addressed output and independent gate".to_owned(),
            capability_snapshot_digest: collaboration_capability_snapshot(
                &project,
                &session_id,
                AgentId(2),
            ),
            input_digest: DIGEST.to_owned(),
        },
        705,
    );
    let claim_id = project.collaboration_sessions[0].claims[0].claim_id.clone();
    project = project_command(
        &state.store,
        &state.developer,
        706,
        transition(
            &state.project_id,
            project.version,
            &work_item_id.0,
            2,
            1,
            CompanyWorkStateV1::Assigned,
            CompanyWorkStateV1::InProgress,
            Vec::new(),
            None,
            706,
        ),
        706,
    );
    project = project_command(
        &state.store,
        &state.developer,
        707,
        transition(
            &state.project_id,
            project.version,
            &work_item_id.0,
            3,
            1,
            CompanyWorkStateV1::InProgress,
            CompanyWorkStateV1::InReview,
            output_receipt(),
            None,
            707,
        ),
        707,
    );

    let mut observation = ReliabilityObservationV1 {
        observation_id: "reliability-observation-1".to_owned(),
        agent_id: AgentId(2),
        capability: "rust".to_owned(),
        task_family: "project-web-v1".to_owned(),
        input_class: "developer".to_owned(),
        claim_id,
        accepted_outcome_digest: OTHER_DIGEST.to_owned(),
        independent_verification_digest: DIGEST.to_owned(),
        verifier_principal_id: state.qa.principal_id.clone(),
        verifier_authority_digest: state.qa.authority_digest.clone(),
        accepted: true,
        calibration_bucket: 80,
        evidence_quality_micros: 900_000,
        policy_generation: project.governance.project_profile.generation,
        observation_digest: String::new(),
        recorded_at_unix_ms: 709,
    };
    observation.observation_digest = observation.expected_digest().unwrap();
    let before_unverified = project.clone();
    let unverified = state
        .store
        .apply_company_command(
            &state.qa,
            Uuid::from_u128(708),
            &CompanyWorkflowCommandV1::RecordCollaborationReliability {
                project_id: state.project_id.clone(),
                expected_version: project.version,
                work_item_id: work_item_id.clone(),
                fence: collaboration_admission_fence(&project, &work_item_id),
                observation: observation.clone(),
            },
            709,
        )
        .unwrap_err();
    assert_eq!(unverified.code, WorkflowErrorCode::AuthorityConflict);
    assert_eq!(
        state
            .store
            .company_project(&project.tenant_id, &project.project_id)
            .unwrap(),
        Some(before_unverified)
    );

    project = project_command(
        &state.store,
        &state.qa,
        709,
        transition(
            &state.project_id,
            project.version,
            &work_item_id.0,
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
            709,
        ),
        709,
    );
    assert!(state
        .store
        .collaboration_capacity_snapshot(&project.tenant_id, &project.project_id)
        .unwrap()
        .assignment_load
        .is_empty());
    assert_eq!(
        project.work_items[&work_item_id]
            .assignments
            .iter()
            .filter(|assignment| assignment.active)
            .count(),
        1
    );
    let self_report = state
        .store
        .apply_company_command(
            &state.developer,
            Uuid::from_u128(710),
            &CompanyWorkflowCommandV1::RecordCollaborationReliability {
                project_id: state.project_id.clone(),
                expected_version: project.version,
                work_item_id: work_item_id.clone(),
                fence: collaboration_admission_fence(&project, &work_item_id),
                observation: observation.clone(),
            },
            710,
        )
        .unwrap_err();
    assert_eq!(self_report.code, WorkflowErrorCode::AuthorityConflict);

    for (operation, mut invalid_observation) in [
        {
            let mut value = observation.clone();
            value.task_family = "foreign-task-family".to_owned();
            value.recorded_at_unix_ms = 711;
            (711_u128, value)
        },
        {
            let mut value = observation.clone();
            value.input_class = "qa".to_owned();
            value.recorded_at_unix_ms = 711;
            (712_u128, value)
        },
        {
            let mut value = observation.clone();
            value.recorded_at_unix_ms = 710;
            (713_u128, value)
        },
    ] {
        invalid_observation.observation_digest = invalid_observation.expected_digest().unwrap();
        let error = state
            .store
            .apply_company_command(
                &state.qa,
                Uuid::from_u128(operation),
                &CompanyWorkflowCommandV1::RecordCollaborationReliability {
                    project_id: state.project_id.clone(),
                    expected_version: project.version,
                    work_item_id: work_item_id.clone(),
                    fence: collaboration_admission_fence(&project, &work_item_id),
                    observation: invalid_observation,
                },
                711,
            )
            .unwrap_err();
        assert_eq!(error.code, WorkflowErrorCode::AuthorityConflict);
    }

    observation.recorded_at_unix_ms = 711;
    observation.observation_digest = observation.expected_digest().unwrap();
    let project = project_command(
        &state.store,
        &state.qa,
        714,
        CompanyWorkflowCommandV1::RecordCollaborationReliability {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: work_item_id.clone(),
            fence: collaboration_admission_fence(&project, &work_item_id),
            observation: observation.clone(),
        },
        711,
    );
    assert_eq!(project.collaboration_reliability, vec![observation]);
    assert_eq!(project.collaboration_generation, 3);
    assert_admission_snapshot_readback(&state, &project);
}

fn assert_admission_snapshot_readback(state: &Journey, project: &sentinel_workflow::ProjectV1) {
    let projection = state
        .store
        .company_project_projection(&project.tenant_id, &project.project_id)
        .unwrap()
        .unwrap();
    assert_eq!(&projection.project, project);
    let events = state
        .store
        .company_project_events_since(&project.tenant_id, 0, 100)
        .unwrap();
    let event = events.last().unwrap();
    assert_eq!(event.event_type, "project_collaboration_admission_recorded");
    assert_eq!(event.sequence, projection.source_sequence);
    assert_eq!(&event.project, project);
    assert_eq!(
        state.store.rebuild_company_project_projections().unwrap(),
        1
    );
    let reopened = WorkflowStore::open(state._temp.path().join("workflow.sqlite")).unwrap();
    assert_eq!(
        reopened
            .company_project_projection(&project.tenant_id, &project.project_id)
            .unwrap(),
        Some(projection)
    );
    assert_eq!(
        reopened
            .company_project_events_since(&project.tenant_id, 0, 100)
            .unwrap(),
        events
    );
}
