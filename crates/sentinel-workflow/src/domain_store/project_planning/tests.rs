use super::*;
use sentinel_common::AgentId;

const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn fixture() -> (
    tempfile::TempDir,
    WorkflowStore,
    AuthenticatedCompanyPrincipalV1,
    ProjectV1,
) {
    let (temp, _path, store, _customer, project) =
        crate::domain_store::tests::accepted_project_fixture();
    let planner = AuthenticatedCompanyPrincipalV1 {
        schema_version: crate::COMPANY_DOMAIN_SCHEMA_VERSION,
        tenant_id: project.tenant_id.clone(),
        principal_id: "pm-a".to_owned(),
        kind: CompanyPrincipalKindV1::Agent,
        role: CompanyRoleV1::ProjectManager,
        customer_id: None,
        agent_id: Some(AgentId(1)),
        authority_generation: 1,
        authority_digest: DIGEST.to_owned(),
    };
    (temp, store, planner, project)
}

fn grant(
    planner: &AuthenticatedCompanyPrincipalV1,
    project: &ProjectV1,
    expires_at_unix_ms: u64,
) -> ProjectPlanningGrantV1 {
    ProjectPlanningGrantV1 {
        schema_version: 1,
        project_id: project.project_id.clone(),
        expected_version: project.version,
        planner_principal: planner.clone(),
        provider: "codex-cli".to_owned(),
        model: "model-test".to_owned(),
        catalog_digest: DIGEST.to_owned(),
        max_duration_ms: 120_000,
        token_policy: crate::SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,
        expires_at_unix_ms,
    }
}

fn claim(call: &ProjectPlanningCallV1) -> ClaimProjectPlanningCallV1 {
    ClaimProjectPlanningCallV1 {
        allowance_id: call.allowance_id.clone(),
        project_id: call.grant.project_id.clone(),
        request_id: call.request_id(),
        request_digest: DIGEST.to_owned(),
        context_digest: "b".repeat(64),
    }
}

#[test]
fn planning_grant_claim_is_exact_durable_and_single_use() {
    let (temp, store, planner, project) = fixture();
    let call = store
        .authorize_project_planning_call(
            &planner,
            Uuid::from_u128(41),
            "planning-a",
            &grant(&planner, &project, 200_000),
            3,
        )
        .unwrap();
    assert_eq!(call.source_project, project);
    assert_eq!(call.version, 1);

    let mut wrong = claim(&call);
    wrong.allowance_id = "planning-foreign".to_owned();
    assert!(store
        .claim_project_planning_call(&planner, &wrong, 4)
        .is_err());
    let expected = claim(&call);
    let dispatched = store
        .claim_project_planning_call(&planner, &expected, 4)
        .unwrap();
    assert_eq!(dispatched.version, 2);
    assert_eq!(
        dispatched.dispatch.as_ref().unwrap().context_digest,
        "b".repeat(64)
    );
    assert!(store
        .claim_project_planning_call(&planner, &claim(&call), 5)
        .is_err());

    let reopened = WorkflowStore::open(temp.path().join("workflow.sqlite")).unwrap();
    assert_eq!(
        reopened
            .project_planning_call(&planner.tenant_id, &project.project_id)
            .unwrap(),
        Some(dispatched)
    );
}

#[test]
fn only_an_expired_undispatched_identical_grant_can_renew() {
    let (_temp, store, planner, project) = fixture();
    let operation = Uuid::from_u128(42);
    let original = grant(&planner, &project, 10);
    let call = store
        .authorize_project_planning_call(&planner, operation, "planning-b", &original, 3)
        .unwrap();
    assert_eq!(
        store
            .authorize_project_planning_call(&planner, operation, "planning-b", &original, 4)
            .unwrap(),
        call
    );

    let renewed_grant = grant(&planner, &project, 200_000);
    let renewed = store
        .authorize_project_planning_call(&planner, operation, "planning-b", &renewed_grant, 11)
        .unwrap();
    assert_eq!(renewed.version, 1);
    assert_eq!(renewed.grant, renewed_grant);
    assert_eq!(renewed.grant_issued_at_unix_ms, 11);

    let mut changed = renewed_grant;
    changed.model = "different-model".to_owned();
    assert!(store
        .authorize_project_planning_call(&planner, operation, "planning-b", &changed, 12)
        .is_err());
}
