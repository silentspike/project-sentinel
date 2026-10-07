use super::*;
use crate::*;
use rusqlite::types::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

const NOW: u64 = 10_000;
const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CLAIM_OPERATION: u128 = 1_000;

type Snapshot = Vec<Vec<Vec<Value>>>;
type BeforeSnapshotReturn = Box<dyn FnOnce() + Send>;
type CompanyCore = WorkflowCore<
    SnapshotOrganization,
    UnavailableWorkExecutionPort,
    UnavailableCompletionEvidencePort,
    UnavailableGateEvidencePort,
>;

struct SnapshotOrganization {
    authority: RuntimeAuthoritySnapshotV1,
    before_return: Mutex<Option<BeforeSnapshotReturn>>,
}

impl OrganizationRuntimePort for SnapshotOrganization {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Ready
    }

    fn authority_snapshot(
        &self,
        tenant: &TenantId,
        project: &ProjectId,
        work: &WorkItemId,
        agent: AgentId,
    ) -> Result<RuntimeAuthoritySnapshotV1, WorkflowPortError> {
        let captured = self.authority.clone();
        if captured.tenant_id != *tenant
            || captured.project_id != *project
            || captured.work_item_id != *work
            || captured.agent_id != agent
        {
            return Err(WorkflowPortError::AuthorityConflict);
        }
        // Deterministically interleave a supported write after snapshot capture,
        // but before WorkflowCore can acquire the adaptive claim transaction.
        if let Some(before_return) = self.before_return.lock().unwrap().take() {
            before_return();
        }
        Ok(captured)
    }
}

fn core(
    store: &Arc<WorkflowStore>,
    authority: &RuntimeAuthoritySnapshotV1,
    before_return: Option<BeforeSnapshotReturn>,
) -> CompanyCore {
    WorkflowCore::new(
        Arc::clone(store),
        SnapshotOrganization {
            authority: authority.clone(),
            before_return: Mutex::new(before_return),
        },
        UnavailableWorkExecutionPort,
        UnavailableCompletionEvidencePort,
        UnavailableGateEvidencePort,
    )
}

struct Fixture {
    _temp: tempfile::TempDir,
    path: std::path::PathBuf,
    store: Arc<WorkflowStore>,
    pm: AuthenticatedCompanyPrincipalV1,
    project: ProjectV1,
    root: AdaptiveSessionGrantV1,
}

fn principal(
    id: &str,
    role: CompanyRoleV1,
    agent: Option<AgentId>,
) -> AuthenticatedCompanyPrincipalV1 {
    AuthenticatedCompanyPrincipalV1 {
        schema_version: COMPANY_DOMAIN_SCHEMA_VERSION,
        tenant_id: TenantId::parse("tenant-company-claim").unwrap(),
        principal_id: id.into(),
        kind: if role == CompanyRoleV1::Customer {
            CompanyPrincipalKindV1::Customer
        } else {
            CompanyPrincipalKindV1::Agent
        },
        role,
        customer_id: (role == CompanyRoleV1::Customer).then(|| id.to_owned()),
        agent_id: agent,
        authority_generation: 1,
        authority_digest: DIGEST.into(),
    }
}

fn fixture() -> Fixture {
    fixture_with_grant(|_| {})
}

fn fixture_with_grant(change_grant: impl FnOnce(&mut AdaptiveSessionGrantV1)) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("company.sqlite");
    let store = Arc::new(WorkflowStore::open(&path).unwrap());
    let customer = principal("customer", CompanyRoleV1::Customer, None);
    let sales = principal("sales", CompanyRoleV1::Sales, Some(AgentId(3)));
    let mut pm = principal("pm", CompanyRoleV1::ProjectManager, Some(AgentId(1)));
    pm.authority_digest = PrincipalAuthorityV1::derive("pm", 1, &[1; 32])
        .unwrap()
        .authority_digest;
    let mut operation = 0;
    let mut apply = |actor: &AuthenticatedCompanyPrincipalV1, command| {
        operation += 1;
        store
            .apply_company_command(actor, Uuid::from_u128(operation), &command, NOW)
            .unwrap()
            .response
    };
    let CompanyWorkflowResponseV1::CustomerRequest(request) = apply(
        &customer,
        CompanyWorkflowCommandV1::SubmitCustomerRequest {
            summary_ref: "Accepted regression project".into(),
            desired_outcome: "Bounded source artifact".into(),
            constraints: vec![],
        },
    ) else {
        panic!("customer request")
    };
    apply(
        &customer,
        CompanyWorkflowCommandV1::ClarifyCustomerRequest {
            request_id: request.request_id.clone(),
            expected_version: 1,
            question_ref: "artifact format".into(),
            answer_ref: "source tree".into(),
        },
    );
    apply(
        &sales,
        CompanyWorkflowCommandV1::QualifyCustomerRequest {
            request_id: request.request_id.clone(),
            expected_version: 2,
            reason_ref: "Scope is qualified".into(),
        },
    );
    let profile = WorkProfileBindingV1 {
        profile_id: "developer-v1".into(),
        generation: 1,
        digest: DIGEST.into(),
    };
    let CompanyWorkflowResponseV1::Proposal(proposal) = apply(
        &sales,
        CompanyWorkflowCommandV1::CreateProposal {
            request_id: request.request_id.clone(),
            expected_version: 3,
            binding: ProposalBindingV1 {
                scope: "bounded-source".into(),
                deliverables: vec!["source".into()],
                exclusions: vec!["deployment".into()],
                acceptance_criteria: vec!["independent-qa".into()],
                assumptions: vec![],
                cost_ceiling_micros: 100,
                provider_cost_ceilings_micros: BTreeMap::from([("local".into(), 100)]),
                governance: ProposalGovernanceV1 {
                    owner: AgentId(1),
                    participants: vec![
                        ParticipantBindingV1 {
                            agent_id: AgentId(1),
                            principal_id: "pm".into(),
                            role: CompanyRoleV1::ProjectManager,
                            reports_to: None,
                            specialties: BTreeSet::from(["coordination".into()]),
                            profile: WorkProfileBindingV1 {
                                profile_id: "pm-v1".into(),
                                ..profile.clone()
                            },
                        },
                        ParticipantBindingV1 {
                            agent_id: AgentId(2),
                            principal_id: "developer".into(),
                            role: CompanyRoleV1::Developer,
                            reports_to: Some(AgentId(1)),
                            specialties: BTreeSet::from(["rust".into()]),
                            profile: profile.clone(),
                        },
                    ],
                    project_profile: WorkProfileBindingV1 {
                        profile_id: "project-v1".into(),
                        ..profile
                    },
                },
                expires_at_unix_ms: NOW + 60_000,
            },
        },
    ) else {
        panic!("proposal")
    };
    let CompanyWorkflowResponseV1::AgreementProject { project, .. } = apply(
        &customer,
        CompanyWorkflowCommandV1::AcceptProposal {
            request_id: request.request_id,
            expected_version: 4,
            proposal_id: proposal.proposal_id,
            proposal_digest: proposal.proposal_digest,
        },
    ) else {
        panic!("accepted project")
    };
    let mut project = *project;
    let planning = store
        .authorize_project_planning_call(
            &pm,
            Uuid::from_u128(10_000),
            "company-claim-planning",
            &ProjectPlanningGrantV1 {
                schema_version: 1,
                project_id: project.project_id.clone(),
                expected_version: project.version,
                planner_principal: pm.clone(),
                provider: "codex-cli".into(),
                model: "model-test".into(),
                catalog_digest: DIGEST.into(),
                max_duration_ms: 120_000,
                token_policy: SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,
                expires_at_unix_ms: NOW + 120_000,
            },
            NOW,
        )
        .unwrap();
    store
        .claim_project_planning_call(
            &pm,
            &ClaimProjectPlanningCallV1 {
                allowance_id: planning.allowance_id.clone(),
                project_id: project.project_id.clone(),
                request_id: planning.request_id(),
                request_digest: DIGEST.into(),
                context_digest: DIGEST.into(),
            },
            NOW,
        )
        .unwrap();
    let work_id = WorkItemId::parse("build-source").unwrap();
    for stage in 0..3 {
        let command = match stage {
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
                reason_ref: "Accepted scope".into(),
            },
            _ => CompanyWorkflowCommandV1::AssignWork {
                project_id: project.project_id.clone(),
                expected_version: project.version,
                work_item_id: work_id.clone(),
                agent_id: AgentId(2),
                organization_generation: 1,
                organization_digest: DIGEST.into(),
                reason_ref: "Implementation owner".into(),
            },
        };
        let CompanyWorkflowResponseV1::Project(next) = apply(&pm, command) else {
            panic!("work graph/activation/assignment")
        };
        project = *next;
    }
    store
        .complete_project_planning_call(
            &pm,
            &project.project_id,
            &planning.allowance_id,
            DIGEST,
            DIGEST,
            &project,
            NOW,
        )
        .unwrap();
    let assignment = &project.work_items[&work_id].assignments[0];
    let authority = RuntimeAuthoritySnapshotV1 {
        schema_version: WORKFLOW_SCHEMA_VERSION,
        tenant_id: project.tenant_id.clone(),
        project_id: project.project_id.clone(),
        work_item_id: work_id.clone(),
        agent_id: assignment.agent_id,
        assignment_version: assignment.assignment_version,
        assignment_digest: assignment.canonical_digest().unwrap(),
        organization_generation: assignment.organization_generation,
        organization_digest: assignment.organization_digest.clone(),
        principal: PrincipalAuthorityV1::derive("developer", 1, &[2; 32]).unwrap(),
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
    let CompanyWorkflowResponseV1::Project(next) = apply(
        &pm,
        CompanyWorkflowCommandV1::GrantSubscriptionCall {
            project_id: project.project_id.clone(),
            expected_version: project.version,
            grant: SubscriptionCallGrantV1 {
                schema_version: 1,
                work_item_id: work_id,
                assignment_id: assignment.assignment_id.clone(),
                assignment_version: assignment.assignment_version,
                agent_id: assignment.agent_id,
                provider: "codex-cli".into(),
                model: "model-test".into(),
                catalog_digest: DIGEST.into(),
                max_calls: 8,
                max_concurrent: 1,
                max_duration_ms: 120_000,
                token_policy: SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,
                expires_at_unix_ms: NOW + 300_000,
            },
        },
    ) else {
        panic!("subscription allowance")
    };
    project = *next;
    let allowance = project.subscription_call.as_ref().unwrap();
    let mut root = AdaptiveSessionGrantV1 {
        schema_version: 1,
        session_id: Uuid::from_u128(100),
        provider_allowance_id: allowance.allowance_id.clone(),
        provider_authority_digest: adaptive_continuation_provider_digest(allowance, &authority)
            .unwrap(),
        authority,
        provider: allowance.grant.provider.clone(),
        model: allowance.grant.model.clone(),
        catalog_digest: allowance.grant.catalog_digest.clone(),
        max_output_tokens: 4_096,
        max_call_duration_ms: allowance.grant.max_duration_ms,
        max_model_calls: allowance.grant.max_calls,
        max_tool_calls: allowance.grant.max_calls,
        created_at_ms: allowance.created_at_unix_ms,
        deadline_ms: allowance.grant.expires_at_unix_ms,
    };
    change_grant(&mut root);
    store
        .begin_adaptive_session(&root, &root.authority, NOW)
        .unwrap();
    Fixture {
        _temp: temp,
        path,
        store,
        pm,
        project,
        root,
    }
}

fn snapshot(store: &WorkflowStore) -> Snapshot {
    let connection = store.lock().unwrap();
    [
        "SELECT * FROM workflow_operations ORDER BY operation_namespace,operation_id",
        "SELECT * FROM workflow_adaptive_heads ORDER BY session_id",
        "SELECT * FROM company_entities ORDER BY tenant_id,entity_kind,entity_id",
        "SELECT * FROM company_operations ORDER BY authority_namespace,operation_id",
        "SELECT * FROM company_events ORDER BY sequence",
    ]
    .into_iter()
    .map(|sql| {
        let mut statement = connection.prepare(sql).unwrap();
        let columns = statement.column_count();
        let rows = statement
            .query_map([], |row| {
                (0..columns)
                    .map(|column| row.get::<_, Value>(column))
                    .collect::<Result<Vec<_>, _>>()
            })
            .unwrap();
        rows.collect::<Result<Vec<_>, _>>().unwrap()
    })
    .collect()
}

fn claim(id: u128, previous: Option<String>) -> AdaptiveTransitionV1 {
    AdaptiveTransitionV1::ClaimModel {
        effect: AdaptiveEffectV1 {
            id: Uuid::from_u128(id),
            request_digest: DIGEST.into(),
        },
        previous_observation_digest: previous,
    }
}

fn reassign(fixture: &Fixture) -> CompanyWorkflowCommandV1 {
    CompanyWorkflowCommandV1::ReassignWork {
        project_id: fixture.project.project_id.clone(),
        expected_version: fixture.project.version,
        work_item_id: fixture.root.authority.work_item_id.clone(),
        expected_assignment_version: fixture.root.authority.assignment_version,
        agent_id: fixture.root.authority.agent_id,
        organization_generation: fixture.root.authority.organization_generation,
        organization_digest: fixture.root.authority.organization_digest.clone(),
        reason_ref: "Same employee, new assignment authority".into(),
    }
}

#[test]
fn company_model_claim_rejects_reassignment_after_organization_snapshot_without_writes() {
    let fixture = fixture();
    let before_session = fixture
        .store
        .adaptive_session(fixture.root.session_id, &fixture.root.authority)
        .unwrap()
        .unwrap();
    let peer = WorkflowStore::open(&fixture.path).unwrap();
    let pm = fixture.pm.clone();
    let command = reassign(&fixture);
    let after_reassignment = Arc::new(Mutex::new(None));
    let captured = Arc::clone(&after_reassignment);
    let core = core(
        &fixture.store,
        &fixture.root.authority,
        Some(Box::new(move || {
            peer.apply_company_command(&pm, Uuid::from_u128(500), &command, NOW + 1)
                .unwrap();
            *captured.lock().unwrap() = Some(snapshot(&peer));
        })),
    );
    let error = core
        .advance_company_adaptive_model_with_clock(
            fixture.root.session_id,
            1,
            Uuid::from_u128(CLAIM_OPERATION),
            &claim(200, None),
            &fixture.root.authority,
            || NOW + 2,
        )
        .unwrap_err();
    assert_eq!(error.code, WorkflowErrorCode::AuthorityConflict);
    let after_reassignment = after_reassignment.lock().unwrap().clone().unwrap();
    assert_eq!(snapshot(&fixture.store), after_reassignment);
    assert_eq!(
        fixture
            .store
            .adaptive_session(fixture.root.session_id, &fixture.root.authority)
            .unwrap()
            .unwrap(),
        before_session
    );
    let project = fixture
        .store
        .company_project(&fixture.project.tenant_id, &fixture.project.project_id)
        .unwrap()
        .unwrap();
    let work = &project.work_items[&fixture.root.authority.work_item_id];
    assert!(!work.assignments[0].active);
    assert!(work.assignments[1].active);
    assert_eq!(
        work.assignments[1].agent_id,
        fixture.root.authority.agent_id
    );
    assert_eq!(
        work.assignments[1].assignment_version,
        fixture.root.authority.assignment_version + 1
    );
    assert_ne!(
        work.assignments[1].assignment_id,
        work.assignments[0].assignment_id
    );
    assert_eq!(project.subscription_call, fixture.project.subscription_call);
}

#[test]
fn adaptive_project_snapshot_reuses_one_bound_project_and_head_without_writes() {
    let fixture = fixture();
    let authority = &fixture.root.authority;
    let before = snapshot(&fixture.store);
    for _ in 0..2 {
        let view = fixture
            .store
            .adaptive_project_snapshot(
                &authority.tenant_id,
                &authority.project_id,
                &authority.work_item_id,
                authority.agent_id,
            )
            .unwrap()
            .unwrap();
        assert_eq!(view.project(), &fixture.project);
        assert_eq!(
            view.session_for_authority(authority)
                .unwrap()
                .unwrap()
                .grant,
            fixture.root
        );
        let mut drift = authority.clone();
        drift.runtime_generation += 1;
        assert_eq!(
            view.session_for_authority(&drift).unwrap_err().code,
            WorkflowErrorCode::AuthorityConflict
        );
        let mut foreign = authority.clone();
        foreign.work_item_id = WorkItemId::parse("foreign-work").unwrap();
        assert_eq!(
            view.session_for_authority(&foreign).unwrap_err().code,
            WorkflowErrorCode::AuthorityConflict
        );
    }
    assert_eq!(snapshot(&fixture.store), before);
}

#[test]
fn adaptive_project_snapshot_refreshes_head_without_inventing_company_dispatch() {
    let fixture = fixture();
    let authority = &fixture.root.authority;
    let before = fixture
        .store
        .adaptive_project_snapshot(
            &authority.tenant_id,
            &authority.project_id,
            &authority.work_item_id,
            authority.agent_id,
        )
        .unwrap()
        .unwrap();
    assert!(before
        .project()
        .subscription_call
        .as_ref()
        .unwrap()
        .dispatch
        .is_none());
    core(&fixture.store, authority, None)
        .advance_company_adaptive_model_with_clock(
            fixture.root.session_id,
            1,
            Uuid::from_u128(CLAIM_OPERATION),
            &claim(200, None),
            authority,
            || NOW + 1,
        )
        .unwrap();
    let after = fixture
        .store
        .adaptive_project_snapshot(
            &authority.tenant_id,
            &authority.project_id,
            &authority.work_item_id,
            authority.agent_id,
        )
        .unwrap()
        .unwrap();
    // An adaptive reservation changes the head, not the separate provider claim.
    assert_eq!(after.project(), before.project());
    assert!(after
        .project()
        .subscription_call
        .as_ref()
        .unwrap()
        .dispatch
        .is_none());
    assert_eq!(
        after
            .session_for_authority(authority)
            .unwrap()
            .unwrap()
            .version,
        2
    );
    assert_eq!(
        before
            .session_for_authority(authority)
            .unwrap()
            .unwrap()
            .version,
        1
    );
}

#[test]
fn adaptive_project_snapshot_preserves_outer_transaction_and_head_error_stage() {
    let fixture = fixture();
    let authority = &fixture.root.authority;
    let before = snapshot(&fixture.store);
    fixture
        .store
        .lock()
        .unwrap()
        .execute_batch("BEGIN")
        .unwrap();
    let view = fixture
        .store
        .adaptive_project_snapshot(
            &authority.tenant_id,
            &authority.project_id,
            &authority.work_item_id,
            authority.agent_id,
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        view.session_for_authority(authority)
            .unwrap()
            .unwrap()
            .version,
        1
    );
    assert!(!fixture.store.lock().unwrap().is_autocommit());
    fixture
        .store
        .lock()
        .unwrap()
        .execute_batch("ROLLBACK")
        .unwrap();
    assert_eq!(snapshot(&fixture.store), before);
    // An external commit invalidates the cached combined proof.
    let peer = rusqlite::Connection::open(&fixture.path).unwrap();
    peer.execute("UPDATE workflow_adaptive_heads SET version=version+1", [])
        .unwrap();
    let rejected = fixture
        .store
        .adaptive_project_snapshot(
            &authority.tenant_id,
            &authority.project_id,
            &authority.work_item_id,
            authority.agent_id,
        )
        .unwrap()
        .unwrap();
    assert_eq!(rejected.project(), &fixture.project);
    assert_eq!(
        rejected.session_for_authority(authority).unwrap_err().code,
        WorkflowErrorCode::CorruptStore
    );
    peer.execute("UPDATE workflow_adaptive_heads SET version=version-1", [])
        .unwrap();
    let repaired = fixture
        .store
        .adaptive_project_snapshot(
            &authority.tenant_id,
            &authority.project_id,
            &authority.work_item_id,
            authority.agent_id,
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        repaired
            .session_for_authority(authority)
            .unwrap()
            .unwrap()
            .version,
        1
    );
}

#[test]
fn adaptive_project_snapshot_keeps_project_corruption_classification() {
    let fixture = fixture();
    let authority = &fixture.root.authority;
    fixture
        .store
        .adaptive_project_snapshot(
            &authority.tenant_id,
            &authority.project_id,
            &authority.work_item_id,
            authority.agent_id,
        )
        .unwrap();
    fixture
        .store
        .lock()
        .unwrap()
        .execute(
            "UPDATE company_entities SET payload='{}' WHERE entity_kind='project'",
            [],
        )
        .unwrap();
    assert!(fixture
        .store
        .adaptive_project_snapshot(
            &authority.tenant_id,
            &authority.project_id,
            &authority.work_item_id,
            authority.agent_id,
        )
        .is_err());
}

#[test]
fn company_model_claim_commits_once_and_preserves_exact_replay() {
    let fixture = fixture();
    let core = core(&fixture.store, &fixture.root.authority, None);
    let command = claim(200, None);
    let (replayed, claimed) = core
        .advance_company_adaptive_model_with_clock(
            fixture.root.session_id,
            1,
            Uuid::from_u128(CLAIM_OPERATION),
            &command,
            &fixture.root.authority,
            || NOW + 1,
        )
        .unwrap();
    assert!(!replayed);
    assert_eq!(claimed.version, 2);
    assert_eq!(claimed.model_calls, 1);
    assert_eq!(claimed.tool_calls, 0);
    assert_eq!(claimed.effect_ids, BTreeSet::from([Uuid::from_u128(200)]));
    assert_eq!(claimed.grant, fixture.root);
    assert_eq!(
        claimed.cursor,
        AdaptiveCursorV1::ModelPending {
            effect: AdaptiveEffectV1 {
                id: Uuid::from_u128(200),
                request_digest: DIGEST.into()
            },
        }
    );
    let committed = snapshot(&fixture.store);
    assert_eq!(
        core.advance_company_adaptive_model_with_clock(
            fixture.root.session_id,
            1,
            Uuid::from_u128(CLAIM_OPERATION),
            &command,
            &fixture.root.authority,
            || panic!("replay must not sample a new claim clock"),
        )
        .unwrap(),
        (true, claimed.clone())
    );
    assert_eq!(
        core.advance_company_adaptive_model_with_clock(
            fixture.root.session_id,
            1,
            Uuid::from_u128(CLAIM_OPERATION + 1),
            &command,
            &fixture.root.authority,
            || NOW + 2,
        )
        .unwrap_err()
        .code,
        WorkflowErrorCode::VersionConflict
    );
    assert_eq!(
        core.advance_company_adaptive_model_with_clock(
            fixture.root.session_id,
            2,
            Uuid::from_u128(CLAIM_OPERATION + 1),
            &claim(201, None),
            &fixture.root.authority,
            || NOW + 2,
        )
        .unwrap_err()
        .code,
        WorkflowErrorCode::InvalidTransition
    );
    assert_eq!(snapshot(&fixture.store), committed);
    assert_eq!(
        fixture
            .store
            .adaptive_session(fixture.root.session_id, &fixture.root.authority)
            .unwrap()
            .unwrap(),
        claimed
    );
}

#[test]
fn company_model_claim_replay_survives_revocation_but_cannot_authorize_a_new_effect() {
    let fixture = fixture();
    let core = core(&fixture.store, &fixture.root.authority, None);
    let command = claim(200, None);
    let (_, claimed) = core
        .advance_company_adaptive_model_with_clock(
            fixture.root.session_id,
            1,
            Uuid::from_u128(CLAIM_OPERATION),
            &command,
            &fixture.root.authority,
            || NOW + 1,
        )
        .unwrap();
    let model_effect = AdaptiveEffectV1 {
        id: Uuid::from_u128(200),
        request_digest: DIGEST.into(),
    };
    let tool_effect = AdaptiveEffectV1 {
        id: Uuid::from_u128(201),
        request_digest: DIGEST.into(),
    };
    let tool = sentinel_common::WorkbenchTool::InspectFile {
        path: "main.rs".into(),
        max_bytes: 16,
    };
    let tool_digest = adaptive_tool_digest(&tool).unwrap();
    let mut session = claimed.clone();
    for transition in [
        AdaptiveTransitionV1::ResolveModel {
            effect: model_effect,
            result_digest: DIGEST.into(),
            decision: AdaptiveModelDecisionV1::Tool {
                tool,
                tool_digest: tool_digest.clone(),
            },
        },
        AdaptiveTransitionV1::ClaimTool {
            effect: tool_effect.clone(),
            tool_digest,
        },
        AdaptiveTransitionV1::ObserveTool {
            observation: AdaptiveObservationRefV1 {
                effect: tool_effect,
                observation_digest: DIGEST.into(),
            },
        },
    ] {
        session = fixture
            .store
            .advance_adaptive_session(
                fixture.root.session_id,
                session.version,
                Uuid::from_u128(2_000 + u128::from(session.version)),
                &transition,
                &fixture.root.authority,
                NOW + 2,
            )
            .unwrap()
            .1;
    }
    assert_eq!(session.cursor, AdaptiveCursorV1::ReadyForModel);
    fixture
        .store
        .apply_company_command(
            &fixture.pm,
            Uuid::from_u128(500),
            &reassign(&fixture),
            NOW + 3,
        )
        .unwrap();
    let revoked = snapshot(&fixture.store);
    // The retained operation is historical evidence only, even after later rounds.
    assert_eq!(
        core.advance_company_adaptive_model_with_clock(
            fixture.root.session_id,
            1,
            Uuid::from_u128(CLAIM_OPERATION),
            &command,
            &fixture.root.authority,
            || panic!("historical replay must not claim"),
        )
        .unwrap(),
        (true, claimed)
    );
    let new_effect = claim(
        202,
        session
            .last_observation
            .as_ref()
            .map(|value| value.observation_digest.clone()),
    );
    assert_eq!(
        core.advance_company_adaptive_model_with_clock(
            fixture.root.session_id,
            session.version,
            Uuid::from_u128(3_000),
            &new_effect,
            &fixture.root.authority,
            || NOW + 4,
        )
        .unwrap_err()
        .code,
        WorkflowErrorCode::AuthorityConflict
    );
    assert_eq!(snapshot(&fixture.store), revoked);
    assert_eq!(
        fixture
            .store
            .adaptive_session(fixture.root.session_id, &fixture.root.authority)
            .unwrap()
            .unwrap(),
        session
    );
}

#[test]
fn company_model_claim_is_opt_in_and_rejects_non_claim_transitions() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(WorkflowStore::open(temp.path().join("generic.sqlite")).unwrap());
    let root = crate::adaptive::continuation_tests::grant();
    let now = root.created_at_ms;
    store
        .begin_adaptive_session(&root, &root.authority, now)
        .unwrap();
    let core = core(&store, &root.authority, None);
    let before = snapshot(&store);
    assert_eq!(
        core.advance_company_adaptive_model_with_clock(
            root.session_id,
            1,
            Uuid::from_u128(CLAIM_OPERATION),
            &AdaptiveTransitionV1::Cancel,
            &root.authority,
            || panic!("not a company ClaimModel"),
        )
        .unwrap_err()
        .code,
        WorkflowErrorCode::AuthorityConflict
    );
    assert_eq!(
        store
            .advance_company_adaptive_model_with_clock(
                root.session_id,
                1,
                Uuid::from_u128(CLAIM_OPERATION),
                &AdaptiveTransitionV1::Cancel,
                &root.authority,
                || panic!("not a company ClaimModel"),
            )
            .unwrap_err()
            .code,
        WorkflowErrorCode::AuthorityConflict
    );
    assert_eq!(
        core.advance_company_adaptive_model_with_clock(
            root.session_id,
            1,
            Uuid::from_u128(CLAIM_OPERATION),
            &claim(200, None),
            &root.authority,
            || now + 1,
        )
        .unwrap_err()
        .code,
        WorkflowErrorCode::AuthorityConflict
    );
    assert_eq!(snapshot(&store), before);
    let (replayed, claimed) = core
        .advance_adaptive_session_with_clock(
            root.session_id,
            1,
            Uuid::from_u128(CLAIM_OPERATION),
            &claim(200, None),
            &root.authority,
            || now + 1,
        )
        .unwrap();
    assert!(!replayed);
    assert_eq!(claimed.model_calls, 1);
    assert_eq!(claimed.version, 2);
}
