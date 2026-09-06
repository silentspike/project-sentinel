use super::*;
use sentinel_workflow::{ProjectV1, SubscriptionCallGrantV1, SubscriptionTokenPolicyV1};

fn assigned() -> (Journey, ProjectV1, SubscriptionCallGrantV1) {
    let state = journey();
    let project = project_command(
        &state.store,
        &state.pm,
        40,
        CompanyWorkflowCommandV1::PlanWorkGraph {
            project_id: state.project_id.clone(),
            expected_version: 1,
            items: vec![work(
                "build-work",
                CompanyRoleV1::Developer,
                &["rust"],
                &[],
                100,
            )],
        },
        40,
    );
    let project = project_command(
        &state.store,
        &state.pm,
        41,
        CompanyWorkflowCommandV1::ActivateProject {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            reason_ref: "approved".into(),
        },
        41,
    );
    let project = project_command(
        &state.store,
        &state.pm,
        42,
        CompanyWorkflowCommandV1::AssignWork {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: WorkItemId::parse("build-work").unwrap(),
            agent_id: AgentId(2),
            organization_generation: 1,
            organization_digest: DIGEST.into(),
            reason_ref: "assigned".into(),
        },
        42,
    );
    let assignment = &project.work_items[&WorkItemId::parse("build-work").unwrap()].assignments[0];
    let grant = SubscriptionCallGrantV1 {
        schema_version: 1,
        work_item_id: WorkItemId::parse("build-work").unwrap(),
        assignment_id: assignment.assignment_id.clone(),
        assignment_version: assignment.assignment_version,
        agent_id: AgentId(2),
        provider: "codex-cli".into(),
        model: "gpt-5.4".into(),
        catalog_digest: DIGEST.into(),
        max_calls: 1,
        max_concurrent: 1,
        max_duration_ms: 120_000,
        token_policy: SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,
        expires_at_unix_ms: 300_000,
    };
    (state, project, grant)
}

fn grant_command(project: &ProjectV1, grant: SubscriptionCallGrantV1) -> CompanyWorkflowCommandV1 {
    CompanyWorkflowCommandV1::GrantSubscriptionCall {
        project_id: project.project_id.clone(),
        expected_version: project.version,
        grant,
    }
}

fn claim_command(project: &ProjectV1) -> CompanyWorkflowCommandV1 {
    let allowance = project.subscription_call.as_ref().unwrap();
    CompanyWorkflowCommandV1::ClaimSubscriptionCall {
        project_id: project.project_id.clone(),
        expected_version: project.version,
        allowance_id: allowance.allowance_id.clone(),
        request_id: format!("company-provider-{}", allowance.allowance_id),
        request_digest: DIGEST.into(),
    }
}

#[test]
fn subscription_claim_is_durable_once_and_separate_from_money() {
    let (state, project, grant) = assigned();
    let project = project_command(
        &state.store,
        &state.pm,
        43,
        grant_command(&project, grant.clone()),
        43,
    );
    assert!(project.reservations.is_empty());
    let claimed = project_command(
        &state.store,
        &state.developer,
        44,
        claim_command(&project),
        44,
    );
    assert!(claimed
        .subscription_call
        .as_ref()
        .unwrap()
        .dispatch
        .is_some());
    assert!(claimed.reservations.is_empty());
    let reopened = WorkflowStore::open(state._temp.path().join("workflow.sqlite")).unwrap();
    let reloaded = reopened
        .company_project(&state.pm.tenant_id, &state.project_id)
        .unwrap()
        .unwrap();
    assert_eq!(claimed, reloaded);
    assert!(reopened
        .apply_company_command(
            &state.developer,
            Uuid::from_u128(45),
            &claim_command(&reloaded),
            45
        )
        .is_err());
    assert!(reopened
        .apply_company_command(
            &state.pm,
            Uuid::from_u128(46),
            &grant_command(&reloaded, grant),
            46
        )
        .is_err());
    assert!(reopened
        .apply_company_command(
            &state.pm,
            Uuid::from_u128(47),
            &CompanyWorkflowCommandV1::ReserveCost {
                project_id: state.project_id.clone(),
                expected_version: reloaded.version,
                work_item_id: Some(WorkItemId::parse("build-work").unwrap()),
                provider: "local-loop".into(),
                amount_micros: 0,
            },
            47
        )
        .is_err());
}

#[test]
fn subscription_grant_rejects_wrong_limits_identity_role_and_expiry() {
    let (state, project, grant) = assigned();
    let mutations: [fn(&mut SubscriptionCallGrantV1); 10] = [
        |g| g.max_calls = 2,
        |g| g.max_concurrent = 2,
        |g| g.max_duration_ms = 120_001,
        |g| g.provider = "local-loop".into(),
        |g| g.agent_id = AgentId(4),
        |g| g.assignment_version += 1,
        |g| g.assignment_id = "foreign".into(),
        |g| g.expires_at_unix_ms = 43,
        |g| g.expires_at_unix_ms = 300_044,
        |g| g.catalog_digest = "not-a-digest".into(),
    ];
    for (index, mutate) in mutations.into_iter().enumerate() {
        let mut invalid = grant.clone();
        mutate(&mut invalid);
        assert!(state
            .store
            .apply_company_command(
                &state.pm,
                Uuid::from_u128(100 + index as u128),
                &grant_command(&project, invalid),
                43
            )
            .is_err());
    }
    for principal in [&state.customer, &state.developer, &state.qa] {
        assert!(state
            .store
            .apply_company_command(
                principal,
                Uuid::from_u128(120),
                &grant_command(&project, grant.clone()),
                43
            )
            .is_err());
    }
    assert_eq!(
        state
            .store
            .company_project(&state.pm.tenant_id, &state.project_id)
            .unwrap()
            .unwrap(),
        project
    );
}

#[test]
fn subscription_claim_rejects_foreign_agent_expiry_and_changed_request() {
    let (state, project, grant) = assigned();
    let project = project_command(
        &state.store,
        &state.pm,
        43,
        grant_command(&project, grant),
        43,
    );
    for principal in [&state.pm, &state.junior_developer, &state.qa] {
        assert!(state
            .store
            .apply_company_command(principal, Uuid::from_u128(44), &claim_command(&project), 44)
            .is_err());
    }
    assert!(state
        .store
        .apply_company_command(
            &state.developer,
            Uuid::from_u128(45),
            &claim_command(&project),
            300_000
        )
        .is_err());
    let mut wrong = claim_command(&project);
    if let CompanyWorkflowCommandV1::ClaimSubscriptionCall { request_id, .. } = &mut wrong {
        *request_id = "foreign".into();
    }
    assert!(state
        .store
        .apply_company_command(&state.developer, Uuid::from_u128(46), &wrong, 46)
        .is_err());
    assert!(state
        .store
        .company_project(&state.pm.tenant_id, &state.project_id)
        .unwrap()
        .unwrap()
        .subscription_call
        .unwrap()
        .dispatch
        .is_none());
}

#[test]
fn legacy_project_encoding_does_not_add_subscription_field() {
    let (_, project, _) = assigned();
    let encoded = serde_json::to_string(&project).unwrap();
    assert!(!encoded.contains("subscription_call"));
    let decoded: ProjectV1 = serde_json::from_str(&encoded).unwrap();
    assert_eq!(serde_json::to_string(&decoded).unwrap(), encoded);
}

#[test]
fn subscription_concurrent_claims_have_one_winner() {
    let (state, project, grant) = assigned();
    let project = project_command(
        &state.store,
        &state.pm,
        43,
        grant_command(&project, grant),
        43,
    );
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let winners = std::thread::scope(|scope| {
        let handles = (0..2)
            .map(|index| {
                let path = state._temp.path().join("workflow.sqlite");
                let principal = state.developer.clone();
                let command = claim_command(&project);
                let barrier = barrier.clone();
                scope.spawn(move || {
                    let store = WorkflowStore::open(path).unwrap();
                    barrier.wait();
                    store
                        .apply_company_command(
                            &principal,
                            Uuid::from_u128(44 + index),
                            &command,
                            44,
                        )
                        .is_ok()
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| usize::from(handle.join().unwrap()))
            .sum::<usize>()
    });
    assert_eq!(winners, 1);
    assert!(state
        .store
        .company_project(&state.pm.tenant_id, &state.project_id)
        .unwrap()
        .unwrap()
        .subscription_call
        .unwrap()
        .dispatch
        .is_some());
}
