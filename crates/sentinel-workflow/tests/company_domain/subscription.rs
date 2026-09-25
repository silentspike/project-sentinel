use super::*;
use sentinel_workflow::{ProjectV1, SubscriptionCallGrantV1, SubscriptionTokenPolicyV1};

#[test]
fn request_provider_accounting_preserves_legacy_grants_and_unknown_dispatches() {
    for dispatched in [false, true] {
        let (state, project, grant) = assigned();
        let mut project = project_command(
            &state.store,
            &state.pm,
            43,
            grant_command(&project, grant),
            43,
        );
        if dispatched {
            project = project_command(
                &state.store,
                &state.developer,
                44,
                claim_command(&project),
                44,
            );
        }
        let operator = AuthenticatedCompanyPrincipalV1 {
            principal_id: "operator-test".into(),
            kind: CompanyPrincipalKindV1::Operator,
            agent_id: None,
            ..state.pm.clone()
        };
        let sales = principal(
            "tenant-a",
            "sales-test",
            CompanyPrincipalKindV1::Agent,
            CompanyRoleV1::Sales,
            None,
            Some(10),
        );
        let CompanyWorkflowResponseV1::CustomerRequest(request) = command(
            &state.store,
            &state.customer,
            50,
            CompanyWorkflowCommandV1::SubmitCustomerRequest {
                summary_ref: "New website".into(),
                desired_outcome: "Three pages".into(),
                constraints: vec![],
            },
            50,
        ) else {
            panic!()
        };
        let mut request_grant = sentinel_workflow::RequestProviderGrantV1 {
            schema_version: 1,
            request_id: request.request_id,
            expected_version: 1,
            sales_principal: sales.clone(),
            provider: "codex-cli".into(),
            model: "model-test".into(),
            catalog_digest: DIGEST.into(),
            total_call_limit: 1,
            concurrent_call_limit: 1,
            max_duration_ms: 120_000,
            token_policy: SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,
            expires_at_unix_ms: 300_000,
        };
        assert!(state
            .store
            .authorize_request_provider_call(&operator, Uuid::from_u128(51), &request_grant, 51)
            .is_err());
        request_grant.total_call_limit = 10;
        let call = state
            .store
            .authorize_request_provider_call(&operator, Uuid::from_u128(51), &request_grant, 51)
            .unwrap();
        let claim = sentinel_workflow::ClaimRequestProviderCallV1 {
            allowance_id: call.allowance_id.clone(),
            request_id: format!("company-provider-{}", call.allowance_id),
            request_digest: DIGEST.into(),
            context_digest: OTHER_DIGEST.into(),
        };
        let result = state.store.claim_request_provider_call(&sales, &claim, 52);
        assert_eq!(result.is_err(), dispatched);
        if !dispatched {
            // A legacy claim cannot race around the new request capacity gate.
            assert!(state
                .store
                .apply_company_command(
                    &state.developer,
                    Uuid::from_u128(53),
                    &claim_command(&project),
                    53
                )
                .is_err());
        } else {
            let project = project_command(
                &state.store,
                &state.developer,
                53,
                transition(
                    &state.project_id,
                    project.version,
                    "build-work",
                    2,
                    1,
                    CompanyWorkStateV1::Assigned,
                    CompanyWorkStateV1::InProgress,
                    Vec::new(),
                    None,
                    53,
                ),
                53,
            );
            let project = project_command(
                &state.store,
                &state.developer,
                54,
                transition(
                    &state.project_id,
                    project.version,
                    "build-work",
                    3,
                    1,
                    CompanyWorkStateV1::InProgress,
                    CompanyWorkStateV1::InReview,
                    output_receipt(),
                    None,
                    54,
                ),
                54,
            );
            let project = project_command(
                &state.store,
                &state.qa,
                55,
                transition(
                    &state.project_id,
                    project.version,
                    "build-work",
                    4,
                    1,
                    CompanyWorkStateV1::InReview,
                    CompanyWorkStateV1::Done,
                    output_receipt(),
                    Some(QualityGateReceiptBindingV1 {
                        gate_id: "web-work-item-qa-v1".into(),
                        generation: 1,
                        gate_digest: DIGEST.into(),
                        subject_digest: OTHER_DIGEST.into(),
                        passed: true,
                    }),
                    55,
                ),
                55,
            );
            assert_eq!(
                project.work_items[&WorkItemId::parse("build-work").unwrap()].state,
                CompanyWorkStateV1::Done
            );
            state
                .store
                .claim_request_provider_call(&sales, &claim, 56)
                .unwrap();
        }
        let stored = state
            .store
            .company_project(&state.pm.tenant_id, &state.project_id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.subscription_call, project.subscription_call);
    }
}

#[test]
fn request_provider_cutover_separates_sales_and_project_call_budgets() {
    let (state, project, grant) = assigned();
    let (operator, sales) = request_provider_principals(&state);

    let first = request_provider_grant(&state, &sales, 50, 1);
    state
        .store
        .authorize_request_provider_call(&operator, Uuid::from_u128(51), &first, 51)
        .unwrap();

    // Exhausting Sales consultation authority must not block execution of an
    // already accepted project under its own assignment and project budget.
    let project = project_command(
        &state.store,
        &state.pm,
        52,
        grant_command(&project, grant),
        52,
    );
    assert!(project.subscription_call.is_some());
}

#[test]
fn post_cutover_project_calls_do_not_consume_later_sales_allowance() {
    let (state, project, grant) = assigned();
    let (operator, sales) = request_provider_principals(&state);

    for index in 0_u128..10 {
        let request_operation = 100 + index * 10;
        let request_grant = request_provider_grant(&state, &sales, request_operation, 10);
        state
            .store
            .authorize_request_provider_call(
                &operator,
                Uuid::from_u128(request_operation + 1),
                &request_grant,
                u64::try_from(request_operation + 1).unwrap(),
            )
            .unwrap();
        if index == 0 {
            project_command(
                &state.store,
                &state.pm,
                request_operation + 2,
                grant_command(&project, grant.clone()),
                u64::try_from(request_operation + 2).unwrap(),
            );
        }
    }

    let exhausted = request_provider_grant(&state, &sales, 300, 10);
    assert!(state
        .store
        .authorize_request_provider_call(&operator, Uuid::from_u128(301), &exhausted, 301)
        .is_err());
}

fn request_provider_principals(
    state: &Journey,
) -> (
    AuthenticatedCompanyPrincipalV1,
    AuthenticatedCompanyPrincipalV1,
) {
    let operator = AuthenticatedCompanyPrincipalV1 {
        principal_id: "operator-test".into(),
        kind: CompanyPrincipalKindV1::Operator,
        agent_id: None,
        ..state.pm.clone()
    };
    let sales = principal(
        "tenant-a",
        "sales-test",
        CompanyPrincipalKindV1::Agent,
        CompanyRoleV1::Sales,
        None,
        Some(10),
    );
    (operator, sales)
}

fn request_provider_grant(
    state: &Journey,
    sales: &AuthenticatedCompanyPrincipalV1,
    operation: u128,
    total_call_limit: u16,
) -> sentinel_workflow::RequestProviderGrantV1 {
    let CompanyWorkflowResponseV1::CustomerRequest(request) = command(
        &state.store,
        &state.customer,
        operation,
        CompanyWorkflowCommandV1::SubmitCustomerRequest {
            summary_ref: "New website".into(),
            desired_outcome: "Three pages".into(),
            constraints: vec![],
        },
        u64::try_from(operation).unwrap(),
    ) else {
        panic!()
    };
    sentinel_workflow::RequestProviderGrantV1 {
        schema_version: 1,
        request_id: request.request_id,
        expected_version: request.version,
        sales_principal: sales.clone(),
        provider: "codex-cli".into(),
        model: "model-test".into(),
        catalog_digest: DIGEST.into(),
        total_call_limit,
        concurrent_call_limit: 1,
        max_duration_ms: 120_000,
        token_policy: SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,
        expires_at_unix_ms: 300_000,
    }
}

pub(super) fn assigned() -> (Journey, ProjectV1, SubscriptionCallGrantV1) {
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
fn completed_dispatched_subscription_rotates_to_the_next_assigned_work_only() {
    let state = journey();
    let mut build_work = work("build-work", CompanyRoleV1::Developer, &["rust"], &[], 100);
    build_work.owner = AgentId(4);
    let project = project_command(
        &state.store,
        &state.pm,
        140,
        CompanyWorkflowCommandV1::PlanWorkGraph {
            project_id: state.project_id.clone(),
            expected_version: 1,
            items: vec![
                work("design-work", CompanyRoleV1::Developer, &["web"], &[], 100),
                build_work,
            ],
        },
        140,
    );
    let project = project_command(
        &state.store,
        &state.pm,
        141,
        CompanyWorkflowCommandV1::ActivateProject {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            reason_ref: "approved".into(),
        },
        141,
    );
    let project = project_command(
        &state.store,
        &state.pm,
        142,
        CompanyWorkflowCommandV1::AssignWork {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: WorkItemId::parse("design-work").unwrap(),
            agent_id: AgentId(2),
            organization_generation: 1,
            organization_digest: DIGEST.into(),
            reason_ref: "assigned".into(),
        },
        142,
    );
    let project = project_command(
        &state.store,
        &state.pm,
        143,
        CompanyWorkflowCommandV1::AssignWork {
            project_id: state.project_id.clone(),
            expected_version: project.version,
            work_item_id: WorkItemId::parse("build-work").unwrap(),
            agent_id: AgentId(4),
            organization_generation: 1,
            organization_digest: DIGEST.into(),
            reason_ref: "assigned".into(),
        },
        143,
    );
    let first_assignment =
        &project.work_items[&WorkItemId::parse("design-work").unwrap()].assignments[0];
    let first_grant = SubscriptionCallGrantV1 {
        schema_version: 1,
        work_item_id: WorkItemId::parse("design-work").unwrap(),
        assignment_id: first_assignment.assignment_id.clone(),
        assignment_version: first_assignment.assignment_version,
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
    let project = project_command(
        &state.store,
        &state.pm,
        144,
        grant_command(&project, first_grant),
        144,
    );
    let project = project_command(
        &state.store,
        &state.developer,
        145,
        claim_command(&project),
        145,
    );
    let first_allowance = project.subscription_call.clone().unwrap();
    let next_assignment =
        &project.work_items[&WorkItemId::parse("build-work").unwrap()].assignments[0];
    let next_grant = SubscriptionCallGrantV1 {
        schema_version: 1,
        work_item_id: WorkItemId::parse("build-work").unwrap(),
        assignment_id: next_assignment.assignment_id.clone(),
        assignment_version: next_assignment.assignment_version,
        agent_id: AgentId(4),
        provider: "codex-cli".into(),
        model: "gpt-5.4".into(),
        catalog_digest: DIGEST.into(),
        max_calls: 1,
        max_concurrent: 1,
        max_duration_ms: 120_000,
        token_policy: SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,
        expires_at_unix_ms: 300_146,
    };
    assert!(state
        .store
        .apply_company_command(
            &state.pm,
            Uuid::from_u128(146),
            &grant_command(&project, next_grant.clone()),
            146,
        )
        .is_err());

    let project = project_command(
        &state.store,
        &state.developer,
        147,
        transition(
            &state.project_id,
            project.version,
            "design-work",
            2,
            1,
            CompanyWorkStateV1::Assigned,
            CompanyWorkStateV1::InProgress,
            Vec::new(),
            None,
            147,
        ),
        147,
    );
    let project = project_command(
        &state.store,
        &state.developer,
        148,
        transition(
            &state.project_id,
            project.version,
            "design-work",
            3,
            1,
            CompanyWorkStateV1::InProgress,
            CompanyWorkStateV1::InReview,
            output_receipt(),
            None,
            148,
        ),
        148,
    );
    let project = project_command(
        &state.store,
        &state.qa,
        149,
        transition(
            &state.project_id,
            project.version,
            "design-work",
            4,
            1,
            CompanyWorkStateV1::InReview,
            CompanyWorkStateV1::Done,
            output_receipt(),
            Some(QualityGateReceiptBindingV1 {
                gate_id: "web-work-item-qa-v1".into(),
                generation: 1,
                gate_digest: DIGEST.into(),
                subject_digest: OTHER_DIGEST.into(),
                passed: true,
            }),
            149,
        ),
        149,
    );
    let advanced = project_command(
        &state.store,
        &state.pm,
        150,
        grant_command(&project, next_grant.clone()),
        150,
    );
    let next = advanced.subscription_call.as_ref().unwrap();
    assert_ne!(next.allowance_id, first_allowance.allowance_id);
    assert_eq!(next.grant, next_grant);
    assert!(next.dispatch.is_none());
}

#[test]
fn subscription_grant_accepts_a_bounded_adaptive_campaign() {
    let (state, project, mut grant) = assigned();
    grant.max_calls = 8;
    let project = project_command(
        &state.store,
        &state.pm,
        43,
        grant_command(&project, grant.clone()),
        43,
    );
    assert_eq!(
        project.subscription_call.as_ref().unwrap().grant.max_calls,
        grant.max_calls
    );
    assert!(project
        .subscription_call
        .as_ref()
        .unwrap()
        .dispatch
        .is_none());
}

#[test]
fn subscription_grant_rejects_wrong_limits_identity_role_and_expiry() {
    let (state, project, grant) = assigned();
    let mutations: [fn(&mut SubscriptionCallGrantV1); 10] = [
        |g| g.max_calls = 65,
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
