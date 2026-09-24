use super::*;
use crate::llm_bridge::bridge::ProviderUsageAuthorityResolver;
use sentinel_workflow::CustomerRequestStateV1;

pub(crate) fn fixture(path: &Path) -> (WorkflowApi, RequestSalesContext) {
    let mut api = super::super::model_work::configured_test_api(path);
    api.event_store = Some(
        sentinel_limbo::EventStore::open(path.with_extension("events.sqlite").to_str().unwrap())
            .unwrap(),
    );
    api.authority
        .as_ref()
        .unwrap()
        .runtime_health
        .write()
        .unwrap()
        .agents
        .extend([3, 4, 5, 6, 7, 8, 9].map(|agent_id| {
            crate::runtime_health::RuntimeHealthAgentSnapshot {
                agent_id,
                aggregate_id: AgentId(agent_id).to_string(),
                name: format!("Company agent {agent_id}"),
                runtime_present: true,
                projection_present: true,
                security_runtime_present: true,
                adapter_handle_present: true,
                adapter_instance_matches: true,
                runtime_resources_healthy: true,
                adapter_health_state: Some(sentinel_common::NanoHealthState::Healthy),
                logical_status: Some(sentinel_runtime::AgentStatus::Active),
                ..Default::default()
            }
        }));
    let customer = api.principals.principal("customer").unwrap();
    let response = api
        .store
        .apply_company_command(
            &customer.principal,
            Uuid::from_u128(11),
            &CompanyWorkflowCommandV1::SubmitCustomerRequest {
                summary_ref: "Studio website".into(),
                desired_outcome: "Three accessible pages".into(),
                constraints: vec![],
            },
            now_unix_ms(),
        )
        .unwrap();
    let CompanyWorkflowResponseV1::CustomerRequest(request) = response.response else {
        panic!("request");
    };
    let operator = api.principals.principal("operator").unwrap();
    let allowance = sentinel_workflow::request_provider_allowance_id(
        &operator.principal.tenant_id,
        Uuid::from_u128(12),
    )
    .unwrap();
    api.request_sales_tenant = Some(operator.principal.tenant_id.clone());
    api.subscription_allowance_id = Some(allowance.clone());
    let authorization = serde_json::json!({"operation_id": Uuid::from_u128(12), "request_id": request.request_id,
        "expected_version": request.version, "sales_principal_id": "sales", "model": "model-test",
        "catalog_digest": "a".repeat(64), "concurrent_call_limit": 1, "expires_at_unix_ms": now_unix_ms() + 240_000 });
    assert_eq!(
        api.authorize_sales_request(&customer, &serde_json::to_vec(&authorization).unwrap())
            .status,
        403
    );
    assert_eq!(
        api.authorize_sales_request(&operator, &serde_json::to_vec(&authorization).unwrap())
            .status,
        200
    );
    let call = api.request_sales_call().unwrap().unwrap();
    assert_eq!(call.allowance_id, allowance);
    let binding = RequestSalesAuthority {
        schema_version: 2,
        allowance_id: allowance,
        grant: call.grant,
    };
    let context = api.prepare_request_sales(&binding).unwrap();
    assert!(api.store.company_projects().unwrap().is_empty());
    (api, context)
}

fn dispatch(api: &WorkflowApi, context: &RequestSalesContext) -> serde_json::Value {
    let id = format!("company-provider-{}", context.binding.allowance_id);
    let model_context = ModelExecutionContext::RequestSales(Box::new(context.clone()));
    let request = serde_json::json!({"schema_version": 2, "allowance_id": context.binding.allowance_id,
        "agent_id": 3, "request_id": id, "request_digest": "b".repeat(64),
        "context_digest": format!("{:x}", Sha256::digest(serde_json::to_vec(&model_context).unwrap())),
        "provider": "codex-cli", "model": "model-test", "catalog_digest": "a".repeat(64),
        "subject": {"kind": "customer_request", "request_id": context.source_request.request_id,
            "request_version": context.source_request.version}});
    api.event_store
        .as_ref()
        .unwrap()
        .reserve_llm_request(&id, &"b".repeat(64), &AgentId(3).to_string())
        .unwrap();
    request
}

fn planning_fixture(path: &Path) -> (WorkflowApi, ProjectPlanningContext) {
    let (api, sales_context) = fixture(path);
    let request = dispatch(&api, &sales_context);
    assert_eq!(
        api.subscription_dispatch(&serde_json::to_vec(&request).unwrap())
            .status,
        200
    );
    let id = request["request_id"].as_str().unwrap();
    let digest = request["request_digest"].as_str().unwrap();
    let completion = ModelExecutionCompletion {
        context: ModelExecutionContext::RequestSales(Box::new(sales_context.clone())),
        content: r#"{"schema_version":1,"kind":"propose_offer","scope":"Design and implement the requested three-page website.","deliverables":["design specification","validated source tree","delivery preview"],"exclusions":["external hosting","production DNS"],"acceptance_criteria":["keyboard-accessible pages","independent QA pass"],"assumptions":["no external media is required"]}"#.into(),
        admissible: true,
    };
    let usage = serde_json::json!({"type":"AgentLlmUsage", "agent_id":3,
        "tenant_id":"tenant-m0", "reservation_id":sales_context.binding.allowance_id,
        "project_id":null,"work_item_id":null,"assignment_id":null,"assignment_version":null,
        "provider":"codex-cli","caller_role":"agent_runtime","effective_model":"model-test",
        "requested_model":"model-test","tier":"mid","hierarchy_tier":2,"cost_source":"provider_reported",
        "input_tokens":5,"output_tokens":5,"cache_read":0,"cache_creation":0,"cost_usd":0.0});
    let event = DomainEvent::new(
        "agent_llm_usage",
        &AgentId(3).to_string(),
        &usage.to_string(),
        id,
        1,
    )
    .with_operation_id(&format!("llm_usage_{id}"))
    .with_schema_version(4);
    let event_store = api.event_store.as_ref().unwrap();
    event_store
        .enqueue_llm_completion(
            id,
            digest,
            &serde_json::to_string(&serde_json::json!({
                "version": 2, "request_id": id, "request_digest": digest,
                "usage_event": event, "actions": [], "tokens_used": 10, "model_work": completion
            }))
            .unwrap(),
        )
        .unwrap();
    event_store
        .persist_llm_completion_usage(id, digest, &event)
        .unwrap();
    api.accept_request_sales(&completion, &sales_context, id, digest)
        .unwrap();
    let proposal = api
        .request_sales_call()
        .unwrap()
        .unwrap()
        .proposal_response
        .unwrap()
        .proposal;
    let customer = api.principals.principal("customer").unwrap();
    let outcome = api
        .store
        .apply_company_command(
            &customer.principal,
            Uuid::from_u128(91),
            &CompanyWorkflowCommandV1::AcceptProposal {
                request_id: proposal.request_id.clone(),
                expected_version: 3,
                proposal_id: proposal.proposal_id,
                proposal_digest: proposal.proposal_digest,
            },
            now_unix_ms(),
        )
        .unwrap();
    let CompanyWorkflowResponseV1::AgreementProject { project, .. } = outcome.response else {
        panic!("agreement")
    };
    let call = api.ensure_project_planning_call(&project).unwrap();
    let context = api
        .prepare_project_planning(&ProjectPlanningAuthority {
            schema_version: 4,
            allowance_id: call.allowance_id,
            grant: call.grant,
        })
        .unwrap();
    (api, context)
}

#[test]
fn sales_abandonment_requires_exact_persisted_resolution_before_new_authority() {
    let temp = tempfile::tempdir().unwrap();
    let (api, context) = fixture(&temp.path().join("workflow.sqlite"));
    let request = dispatch(&api, &context);
    assert_eq!(
        api.subscription_dispatch(&serde_json::to_vec(&request).unwrap())
            .status,
        200
    );
    let operator = api.principals.principal("operator").unwrap();
    let customer = api.principals.principal("customer").unwrap();
    assert!(
        <WorkflowApi as crate::llm_bridge::bridge::ProviderUsageAuthorityResolver>::is_provider_usage_candidate(
            &api,
            AgentId(3)
        )
        .unwrap()
    );
    assert!(
        !<WorkflowApi as crate::llm_bridge::bridge::ProviderUsageAuthorityResolver>::is_provider_usage_candidate(
            &api,
            AgentId(2)
        )
        .unwrap()
    );
    let body =
        serde_json::to_vec(&serde_json::json!({"allowance_id": context.binding.allowance_id}))
            .unwrap();
    assert_eq!(api.abandon_sales_request(&operator, &body).status, 403);
    let store = api.event_store.as_ref().unwrap();
    let id = request["request_id"].as_str().unwrap();
    store
        .resolve_llm_completion_terminal(
            id,
            &"b".repeat(64),
            "test operator abandoned terminal inference",
        )
        .unwrap();
    assert_eq!(api.abandon_sales_request(&customer, &body).status, 403);
    assert_eq!(api.abandon_sales_request(&operator, &body).status, 200);
    assert_eq!(api.abandon_sales_request(&operator, &body).status, 200);
    let call = api.request_sales_call().unwrap().unwrap();
    assert!(call.abandonment_event_id.is_some());
    assert!(call.dispatch.is_some());
    assert!(call.question_response.is_none());
    assert!(
        !<WorkflowApi as crate::llm_bridge::bridge::ProviderUsageAuthorityResolver>::is_provider_usage_candidate(
            &api,
            AgentId(3)
        )
        .unwrap()
    );
    assert!(api.prepare_request_sales(&context.binding).is_err());
    assert_eq!(
        api.store
            .company_customer_request(
                &context.source_request.tenant_id,
                &context.source_request.request_id
            )
            .unwrap(),
        Some(context.source_request)
    );
}

#[test]
fn sales_dispatch_requires_exact_subject_current_roster_and_one_durable_claim() {
    let temp = tempfile::tempdir().unwrap();
    let (api, context) = fixture(&temp.path().join("company.sqlite"));
    let request = dispatch(&api, &context);
    for field in [
        "schema_version",
        "agent_id",
        "subject",
        "context_digest",
        "model",
        "request_digest",
    ] {
        let mut wrong = request.clone();
        wrong[field] = serde_json::Value::Null;
        assert_ne!(
            api.subscription_dispatch(&serde_json::to_vec(&wrong).unwrap())
                .status,
            200,
            "{field}"
        );
    }
    let health = &api.authority.as_ref().unwrap().runtime_health;
    health.write().unwrap().agents[0].projection_present = false;
    assert_eq!(
        api.subscription_dispatch(&serde_json::to_vec(&request).unwrap())
            .status,
        403
    );
    health.write().unwrap().agents[0].projection_present = true;
    assert!(api
        .resolve_provider_usage_authority(AgentId(6))
        .unwrap()
        .is_none());
    assert_eq!(
        api.subscription_dispatch(&serde_json::to_vec(&request).unwrap())
            .status,
        200
    );
    assert_eq!(
        api.prepare_request_sales(&context.binding).unwrap(),
        context
    );
    assert_eq!(
        api.subscription_dispatch(&serde_json::to_vec(&request).unwrap())
            .status,
        403
    );
    assert!(api.store.company_projects().unwrap().is_empty());
}

#[test]
fn legacy_project_context_keeps_exact_json_and_rejects_mixed_subjects() {
    let project = super::super::model_work::test_context();
    let old = serde_json::to_vec(&project).unwrap();
    let context: ModelExecutionContext = serde_json::from_slice(&old).unwrap();
    assert_eq!(serde_json::to_vec(&context).unwrap(), old);
    assert_eq!(
        serde_json::to_vec(&context.binding()).unwrap(),
        serde_json::to_vec(&project.binding).unwrap()
    );
    let mut mixed = serde_json::to_value(&context).unwrap();
    mixed["source_request"] = serde_json::json!({});
    assert!(serde_json::from_value::<ModelExecutionContext>(mixed).is_err());
    let mut binding = serde_json::to_value(&project.binding).unwrap();
    binding["grant"] = serde_json::json!({});
    assert!(serde_json::from_value::<ProviderExecutionAuthority>(binding).is_err());
}

#[test]
fn sales_question_requires_the_durable_exact_completion_and_never_creates_a_project() {
    let temp = tempfile::tempdir().unwrap();
    let (api, context) = fixture(&temp.path().join("company.sqlite"));
    let request = dispatch(&api, &context);
    assert_eq!(
        api.subscription_dispatch(&serde_json::to_vec(&request).unwrap())
            .status,
        200
    );
    let id = request["request_id"].as_str().unwrap();
    let digest = request["request_digest"].as_str().unwrap();
    let completion = ModelExecutionCompletion { context: ModelExecutionContext::RequestSales(Box::new(context.clone())),
        content: r#"{"schema_version":1,"kind":"ask_question","content":"Which three pages should the website contain?"}"#.into(), admissible: true };
    assert!(
        api.accept_request_sales(&completion, &context, id, digest)
            .is_err(),
        "no durable model response"
    );
    let usage = serde_json::json!({"type":"AgentLlmUsage", "agent_id":3,
        "tenant_id":"tenant-m0", "reservation_id":context.binding.allowance_id,
        "project_id":null,"work_item_id":null,"assignment_id":null,"assignment_version":null,
        "provider":"codex-cli","caller_role":"agent_runtime","effective_model":"model-test",
        "requested_model":"model-test","tier":"mid","hierarchy_tier":2,"cost_source":"provider_reported",
        "input_tokens":5,"output_tokens":5,"cache_read":0,"cache_creation":0,"cost_usd":0.0});
    let event = DomainEvent::new(
        "agent_llm_usage",
        &AgentId(3).to_string(),
        &usage.to_string(),
        id,
        1,
    )
    .with_operation_id(&format!("llm_usage_{id}"))
    .with_schema_version(4);
    let store = api.event_store.as_ref().unwrap();
    store
        .enqueue_llm_completion(
            id,
            digest,
            &serde_json::to_string(&serde_json::json!({
                "version": 2, "request_id": id, "request_digest": digest,
                "usage_event": event, "actions": [], "tokens_used": 10, "model_work": completion
            }))
            .unwrap(),
        )
        .unwrap();
    assert!(
        api.accept_request_sales(&completion, &context, id, digest)
            .is_err(),
        "usage not persisted"
    );
    store
        .persist_llm_completion_usage(id, digest, &event)
        .unwrap();
    let mut forged = completion.clone();
    forged.content = forged.content.replace("three", "five");
    assert!(api
        .accept_request_sales(&forged, &context, id, digest)
        .is_err());
    api.accept_request_sales(&completion, &context, id, digest)
        .unwrap();
    api.accept_request_sales(&completion, &context, id, digest)
        .unwrap();
    let call = api.request_sales_call().unwrap().unwrap();
    assert_eq!(call.question_response.unwrap().consultation.len(), 1);
    assert!(api.store.company_projects().unwrap().is_empty());
    assert!(
        api.resolve_provider_usage_authority(AgentId(3)).is_err(),
        "answered request cannot trigger another call"
    );
}

#[test]
fn sales_model_qualifies_and_authors_one_policy_bound_offer_without_customer_acceptance() {
    let temp = tempfile::tempdir().unwrap();
    let (api, context) = fixture(&temp.path().join("company.sqlite"));
    let request = dispatch(&api, &context);
    assert_eq!(
        api.subscription_dispatch(&serde_json::to_vec(&request).unwrap())
            .status,
        200
    );
    let id = request["request_id"].as_str().unwrap();
    let digest = request["request_digest"].as_str().unwrap();
    let completion = ModelExecutionCompletion {
        context: ModelExecutionContext::RequestSales(Box::new(context.clone())),
        content: r#"{"schema_version":1,"kind":"propose_offer","scope":"Design and implement the requested three-page website.","deliverables":["design specification","validated source tree","delivery preview"],"exclusions":["external hosting","production DNS"],"acceptance_criteria":["keyboard-accessible pages","independent QA pass"],"assumptions":["no external media is required"]}"#.into(),
        admissible: true,
    };
    let usage = serde_json::json!({"type":"AgentLlmUsage", "agent_id":3,
        "tenant_id":"tenant-m0", "reservation_id":context.binding.allowance_id,
        "project_id":null,"work_item_id":null,"assignment_id":null,"assignment_version":null,
        "provider":"codex-cli","caller_role":"agent_runtime","effective_model":"model-test",
        "requested_model":"model-test","tier":"mid","hierarchy_tier":2,"cost_source":"provider_reported",
        "input_tokens":5,"output_tokens":5,"cache_read":0,"cache_creation":0,"cost_usd":0.0});
    let event = DomainEvent::new(
        "agent_llm_usage",
        &AgentId(3).to_string(),
        &usage.to_string(),
        id,
        1,
    )
    .with_operation_id(&format!("llm_usage_{id}"))
    .with_schema_version(4);
    let store = api.event_store.as_ref().unwrap();
    store
        .enqueue_llm_completion(
            id,
            digest,
            &serde_json::to_string(&serde_json::json!({
                "version": 2, "request_id": id, "request_digest": digest,
                "usage_event": event, "actions": [], "tokens_used": 10, "model_work": completion
            }))
            .unwrap(),
        )
        .unwrap();
    store
        .persist_llm_completion_usage(id, digest, &event)
        .unwrap();
    api.accept_request_sales(&completion, &context, id, digest)
        .unwrap();
    api.accept_request_sales(&completion, &context, id, digest)
        .unwrap();
    let call = api.request_sales_call().unwrap().unwrap();
    assert!(call.question_response.is_none());
    let outcome = call.proposal_response.unwrap();
    assert_eq!(outcome.request.state, CustomerRequestStateV1::Proposed);
    assert_eq!(
        outcome.request.proposal_ids,
        vec![outcome.proposal.proposal_id.clone()]
    );
    assert_eq!(outcome.proposal.binding.governance.owner, AgentId(5));
    assert_eq!(outcome.proposal.binding.governance.participants.len(), 7);
    assert_eq!(outcome.proposal.binding.cost_ceiling_micros, 2_000_000);
    assert_eq!(
        outcome.proposal.binding.provider_cost_ceilings_micros,
        BTreeMap::from([("local-loop".to_owned(), 1_000_000)])
    );
    assert!(api.store.company_projects().unwrap().is_empty());
}

#[test]
fn sales_parser_does_not_accept_answers_approvals_or_legacy_tools() {
    for content in [
        r#"{"schema_version":1,"kind":"accept_proposal"}"#,
        r#"{"schema_version":1,"kind":"ask_question","content":"Question?","approved":true}"#,
        r#"{"schema_version":1,"tools":[]}"#,
        r#"{"schema_version":1,"kind":"propose_offer","scope":"site","deliverables":["source"],"exclusions":[],"acceptance_criteria":["qa"],"assumptions":[],"cost_ceiling_micros":1}"#,
    ] {
        assert!(serde_json::from_str::<SalesDecision>(content).is_err());
    }
    assert!(matches!(
        serde_json::from_str::<SalesDecision>(
            r#"{"schema_version":1,"kind":"propose_offer","scope":"site","deliverables":["source"],"exclusions":["hosting"],"acceptance_criteria":["qa"],"assumptions":["no external media"]}"#
        )
        .unwrap()
        .into_action(),
        SalesAction::ProposeOffer { .. }
    ));
}

#[test]
fn project_plan_is_strict_acyclic_and_server_binds_authority() {
    let temp = tempfile::tempdir().unwrap();
    let (api, context) = planning_fixture(&temp.path().join("company.sqlite"));
    let decision: ProjectPlanningDecision = serde_json::from_str(
        r#"{"schema_version":1,"rationale":"Design precedes implementation.","tasks":[{"key":"design","title":"Create design","objective":"Specify the accessible interface.","role":"designer","depends_on":[]},{"key":"implement","title":"Implement site","objective":"Build and validate the accepted site.","role":"developer","depends_on":["design"]}]}"#,
    )
    .unwrap();
    let items = api.bind_project_plan(&context, &decision).unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].owner, AgentId(4));
    assert_eq!(items[1].owner, AgentId(6));
    assert_eq!(items[0].budget_micros, 1_000_000);
    assert_eq!(items[1].budget_micros, 1_000_000);
    assert_eq!(items[1].dependency_ids.len(), 1);
    assert_eq!(items[1].inputs.len(), 1);
    assert_eq!(
        items[1].inputs[0].expected_contract_digest,
        items[0].outputs[0].contract_digest
    );
    assert_ne!(items[0].work_item_id.0, "design");

    for invalid in [
        r#"{"schema_version":1,"rationale":"No implementation.","tasks":[{"key":"design","title":"Design","objective":"Design only.","role":"designer","depends_on":[]}]}"#,
        r#"{"schema_version":1,"rationale":"Duplicate.","tasks":[{"key":"build","title":"One","objective":"One.","role":"developer","depends_on":[]},{"key":"build","title":"Two","objective":"Two.","role":"developer","depends_on":[]}]}"#,
        r#"{"schema_version":1,"rationale":"Unknown dependency.","tasks":[{"key":"build","title":"Build","objective":"Build.","role":"developer","depends_on":["missing"]}]}"#,
        r#"{"schema_version":1,"rationale":"Forward dependency.","tasks":[{"key":"build","title":"Build","objective":"Build.","role":"developer","depends_on":["later"]},{"key":"later","title":"Later","objective":"Later.","role":"developer","depends_on":[]}]}"#,
    ] {
        let decision: ProjectPlanningDecision = serde_json::from_str(invalid).unwrap();
        assert!(api.bind_project_plan(&context, &decision).is_err());
    }
    assert!(serde_json::from_str::<ProjectPlanningDecision>(
        r#"{"schema_version":1,"rationale":"Extra authority.","tasks":[{"key":"build","title":"Build","objective":"Build.","role":"developer","depends_on":[],"agent_id":99}]}"#
    )
    .is_err());
}

#[test]
fn project_planning_dispatch_claims_the_exact_durable_subject_once() {
    let temp = tempfile::tempdir().unwrap();
    let (api, context) = planning_fixture(&temp.path().join("company.sqlite"));
    let binding = ProviderExecutionAuthority::ProjectPlanning(Box::new(context.binding.clone()));
    let request_id = binding.request_id();
    let request_digest = "d".repeat(64);
    let context_digest = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&ModelExecutionContext::ProjectPlanning(Box::new(
                context.clone()
            )))
            .unwrap()
        )
    );
    api.event_store
        .as_ref()
        .unwrap()
        .reserve_llm_request(
            &request_id,
            &request_digest,
            &context
                .binding
                .grant
                .planner_principal
                .agent_id
                .unwrap()
                .to_string(),
        )
        .unwrap();
    let request = serde_json::json!({
        "schema_version": 4,
        "allowance_id": context.binding.allowance_id,
        "agent_id": context.binding.grant.planner_principal.agent_id.unwrap().0,
        "request_id": request_id,
        "request_digest": request_digest,
        "context_digest": context_digest,
        "provider": context.binding.grant.provider,
        "model": context.binding.grant.model,
        "catalog_digest": context.binding.grant.catalog_digest,
        "subject": {"kind":"project_planning","project_id":context.source_project.project_id,
            "project_version":context.source_project.version}
    });
    assert_eq!(
        api.subscription_dispatch(&serde_json::to_vec(&request).unwrap())
            .status,
        200
    );
    assert_eq!(
        api.subscription_dispatch(&serde_json::to_vec(&request).unwrap())
            .status,
        403
    );
}

#[test]
fn project_planning_adoption_resumes_after_the_first_durable_workflow_step() {
    let temp = tempfile::tempdir().unwrap();
    let (api, context) = planning_fixture(&temp.path().join("company.sqlite"));
    let binding = ProviderExecutionAuthority::ProjectPlanning(Box::new(context.binding.clone()));
    let request_id = binding.request_id();
    let request_digest = "d".repeat(64);
    let context_digest = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&ModelExecutionContext::ProjectPlanning(Box::new(
                context.clone()
            )))
            .unwrap()
        )
    );
    let event_store = api.event_store.as_ref().unwrap();
    event_store
        .reserve_llm_request(
            &request_id,
            &request_digest,
            &context
                .binding
                .grant
                .planner_principal
                .agent_id
                .unwrap()
                .to_string(),
        )
        .unwrap();
    let request = serde_json::json!({
        "schema_version": 4,
        "allowance_id": context.binding.allowance_id,
        "agent_id": context.binding.grant.planner_principal.agent_id.unwrap().0,
        "request_id": request_id,
        "request_digest": request_digest,
        "context_digest": context_digest,
        "provider": context.binding.grant.provider,
        "model": context.binding.grant.model,
        "catalog_digest": context.binding.grant.catalog_digest,
        "subject": {"kind":"project_planning","project_id":context.source_project.project_id,
            "project_version":context.source_project.version}
    });
    assert_eq!(
        api.subscription_dispatch(&serde_json::to_vec(&request).unwrap())
            .status,
        200
    );

    let content = r#"{"schema_version":1,"rationale":"Implement the accepted site directly.","tasks":[{"key":"implement","title":"Implement site","objective":"Build and validate the accepted site.","role":"developer","depends_on":[]}]}"#;
    let completion = ModelExecutionCompletion {
        context: ModelExecutionContext::ProjectPlanning(Box::new(context.clone())),
        content: content.to_owned(),
        admissible: true,
    };
    let planner_agent = context.binding.grant.planner_principal.agent_id.unwrap();
    let usage = serde_json::json!({"type":"AgentLlmUsage", "agent_id":planner_agent.0,
        "tenant_id":context.binding.grant.planner_principal.tenant_id.0,
        "reservation_id":context.binding.allowance_id,
        "project_id":context.binding.grant.project_id.0,"work_item_id":null,
        "assignment_id":null,"assignment_version":null,"provider":context.binding.grant.provider,
        "caller_role":"agent_runtime","effective_model":context.binding.grant.model,
        "requested_model":context.binding.grant.model,"tier":"mid","hierarchy_tier":2,
        "cost_source":"provider_reported","input_tokens":5,"output_tokens":5,
        "cache_read":0,"cache_creation":0,"cost_usd":0.0});
    let event = DomainEvent::new(
        "agent_llm_usage",
        &planner_agent.to_string(),
        &usage.to_string(),
        &request_id,
        1,
    )
    .with_operation_id(&format!("llm_usage_{request_id}"))
    .with_schema_version(5);
    event_store
        .enqueue_llm_completion(
            &request_id,
            &request_digest,
            &serde_json::to_string(&serde_json::json!({
                "version":2,"request_id":request_id,"request_digest":request_digest,
                "usage_event":event,"actions":[],"tokens_used":10,"model_work":completion
            }))
            .unwrap(),
        )
        .unwrap();
    event_store
        .persist_llm_completion_usage(&request_id, &request_digest, &event)
        .unwrap();

    let decision: ProjectPlanningDecision = serde_json::from_str(content).unwrap();
    let items = api.bind_project_plan(&context, &decision).unwrap();
    let plan_operation =
        stable_operation_id("sentinel.workflow.adopt-project-plan.v1", &request_id, 1);
    api.core
        .apply_company_command(
            &context.binding.grant.planner_principal,
            plan_operation,
            &CompanyWorkflowCommandV1::PlanWorkGraph {
                project_id: context.source_project.project_id.clone(),
                expected_version: context.source_project.version,
                items,
            },
            now_unix_ms(),
        )
        .unwrap();

    api.accept_project_planning(&completion, &context, &request_id, &request_digest)
        .unwrap();
    api.accept_project_planning(&completion, &context, &request_id, &request_digest)
        .unwrap();
    let call = api
        .store
        .project_planning_call(
            &context.binding.grant.planner_principal.tenant_id,
            &context.binding.grant.project_id,
        )
        .unwrap()
        .unwrap();
    let project = call.planned_project.unwrap();
    assert_eq!(
        project.lifecycle_state,
        sentinel_workflow::ProjectLifecycleStateV1::Active
    );
    assert_eq!(project.work_items.len(), 1);
    assert_eq!(
        project
            .work_items
            .values()
            .next()
            .unwrap()
            .assignments
            .len(),
        1
    );
    let mut substituted = project;
    substituted.version += 1;
    assert!(api
        .store
        .complete_project_planning_call(
            &context.binding.grant.planner_principal,
            &context.binding.grant.project_id,
            &context.binding.allowance_id,
            &request_digest,
            &format!("{:x}", Sha256::digest(content.as_bytes())),
            &substituted,
            now_unix_ms(),
        )
        .is_err());
}

#[test]
fn corrected_sales_schema_requeues_only_the_exact_failed_completion_without_provider_io() {
    let temp = tempfile::tempdir().unwrap();
    let (api, context) = fixture(&temp.path().join("company.sqlite"));
    let request = dispatch(&api, &context);
    assert_eq!(
        api.subscription_dispatch(&serde_json::to_vec(&request).unwrap())
            .status,
        200
    );
    let id = request["request_id"].as_str().unwrap();
    let digest = request["request_digest"].as_str().unwrap();
    let completion = ModelExecutionCompletion {
        context: ModelExecutionContext::RequestSales(Box::new(context.clone())),
        content: r#"{"schema_version":1,"kind":"propose_offer","scope":"site","deliverables":["source"],"exclusions":["hosting"],"acceptance_criteria":["qa"],"assumptions":["no external media"]}"#.into(),
        admissible: true,
    };
    let usage = serde_json::json!({"type":"AgentLlmUsage", "agent_id":3,
        "tenant_id":"tenant-m0", "reservation_id":context.binding.allowance_id,
        "project_id":null,"work_item_id":null,"assignment_id":null,"assignment_version":null,
        "provider":"codex-cli","caller_role":"agent_runtime","effective_model":"model-test",
        "requested_model":"model-test","tier":"mid","hierarchy_tier":2,"cost_source":"provider_reported",
        "input_tokens":5,"output_tokens":5,"cache_read":0,"cache_creation":0,"cost_usd":0.0});
    let event = DomainEvent::new(
        "agent_llm_usage",
        &AgentId(3).to_string(),
        &usage.to_string(),
        id,
        1,
    )
    .with_operation_id(&format!("llm_usage_{id}"))
    .with_schema_version(4);
    let store = api.event_store.as_ref().unwrap();
    store
        .enqueue_llm_completion(
            id,
            digest,
            &serde_json::to_string(&serde_json::json!({
                "version": 2, "request_id": id, "request_digest": digest,
                "usage_event": event, "actions": [], "tokens_used": 10,
                "model_work": completion
            }))
            .unwrap(),
        )
        .unwrap();
    store
        .persist_llm_completion_usage(id, digest, &event)
        .unwrap();
    assert_eq!(
        store
            .record_llm_completion_failure(id, digest, "Sales decision is not strict JSON", 1,)
            .unwrap(),
        (1, true)
    );
    assert!(!store
        .requeue_failed_llm_completion(id, digest, "different failure")
        .unwrap());
    assert!(api.requeue_request_sales_schema_mismatch().unwrap());
    let requeued = store.get_llm_completion(id).unwrap().unwrap();
    assert_eq!(requeued.status, "ready_for_action");
    assert_eq!(requeued.attempt_count, 0);
    assert!(requeued.last_error.is_none());
    assert!(!api.requeue_request_sales_schema_mismatch().unwrap());
    api.authority
        .as_ref()
        .unwrap()
        .runtime_health
        .write()
        .unwrap()
        .agents
        .clear();
    api.accept_request_sales(&completion, &context, id, digest)
        .unwrap();
    assert!(api
        .request_sales_call()
        .unwrap()
        .unwrap()
        .proposal_response
        .is_some());
}

#[test]
fn adaptive_parser_accepts_one_typed_decision_and_rejects_ambiguous_output() {
    let tool = parse_adaptive_decision(
        r#"{"schema_version":1,"decision":{"kind":"tool","tool":{"tool":"inspect_file","path":"src/main.rs","max_bytes":4096}}}"#,
    )
    .unwrap();
    assert!(matches!(tool, AdaptiveModelDecisionV1::Tool { .. }));
    assert!(matches!(
        parse_adaptive_decision(&format!(
            r#"{{"schema_version":1,"decision":{{"kind":"propose_completion","artifact_digest":"{}"}}}}"#,
            "a".repeat(64)
        ))
        .unwrap(),
        AdaptiveModelDecisionV1::ProposeCompletion { .. }
    ));
    assert!(matches!(
        parse_adaptive_decision(
            r#"{"schema_version":1,"decision":{"kind":"blocked","reason_code":"dependency_unavailable"}}"#
        )
        .unwrap(),
        AdaptiveModelDecisionV1::Blocked { .. }
    ));
    for invalid in [
        r#"{"schema_version":2,"decision":{"kind":"blocked","reason_code":"blocked"}}"#,
        r#"{"schema_version":1,"decision":{"kind":"blocked","reason_code":"../blocked"}}"#,
        r#"{"schema_version":1,"decision":{"kind":"blocked","reason_code":"Dependency.Unavailable"}}"#,
        r#"{"schema_version":1,"decision":{"kind":"tool","tool":{"tool":"inspect_file","path":"../secret","max_bytes":4096}}}"#,
        r#"{"schema_version":1,"decision":{"kind":"blocked","reason_code":"blocked"},"extra":true}"#,
        r#"{"schema_version":1,"decision":{"kind":"propose_completion","artifact_digest":"ABC"}}"#,
    ] {
        assert!(parse_adaptive_decision(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn adaptive_completion_requires_an_observed_successful_artifact() {
    let digest = "a".repeat(64);
    let result = sentinel_common::WorkbenchMessage::Result {
        schema_version: WORKBENCH_SCHEMA_VERSION,
        invocation_id: "01991c34-e03c-70c2-b97e-0591f4be2501".into(),
        input_digest: "b".repeat(64),
        outcome: sentinel_common::WorkbenchOutcome::Succeeded,
        resources: sentinel_common::WorkbenchResourceUsage::default(),
        artifacts: vec![WorkbenchArtifactRef {
            artifact_id: format!("sha256:{digest}"),
            sha256: digest.clone(),
            artifact_kind: "source_tree".into(),
            media_type: "application/vnd.sentinel.source-tree+json".into(),
            size_bytes: 42,
            manifest_path: format!("{digest}.manifest.json"),
        }],
        output: std::collections::BTreeMap::new(),
        error: None,
    };
    let observation = WorkbenchPrivateObservation::from_result(&result).unwrap();
    let accepted = AdaptiveModelDecisionV1::ProposeCompletion {
        artifact_digest: digest,
    };
    validate_adaptive_decision_evidence(Some(&observation), &accepted).unwrap();
    assert!(validate_adaptive_decision_evidence(None, &accepted).is_err());
    assert!(validate_adaptive_decision_evidence(
        Some(&observation),
        &AdaptiveModelDecisionV1::ProposeCompletion {
            artifact_digest: "c".repeat(64),
        },
    )
    .is_err());

    let failed = sentinel_common::WorkbenchMessage::Result {
        schema_version: WORKBENCH_SCHEMA_VERSION,
        invocation_id: "01991c34-e03c-70c2-b97e-0591f4be2501".into(),
        input_digest: "b".repeat(64),
        outcome: sentinel_common::WorkbenchOutcome::Failed,
        resources: sentinel_common::WorkbenchResourceUsage::default(),
        artifacts: Vec::new(),
        output: std::collections::BTreeMap::new(),
        error: Some(sentinel_common::WorkbenchErrorInfo {
            class: sentinel_common::WorkbenchErrorClass::Tool,
            code: "package_failed".into(),
            safe_message: "package failed".into(),
            retryable: false,
        }),
    };
    let failed = WorkbenchPrivateObservation::from_result(&failed).unwrap();
    assert!(validate_adaptive_decision_evidence(Some(&failed), &accepted).is_err());
}

#[test]
fn adaptive_usage_requires_exact_schema_aggregate_and_cost_classification() {
    let temp = tempfile::tempdir().unwrap();
    let (api, authority, _) = super::super::model_work::configured_adaptive_test_api(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    let binding = ProviderExecutionAuthority::Adaptive(Box::new(authority.clone()));
    let context = api.model_work_context(&binding).unwrap().unwrap();
    let completion = ModelExecutionCompletion {
        context,
        content: r#"{"schema_version":1,"decision":{"kind":"blocked","reason_code":"dependency_unavailable"}}"#.into(),
        admissible: true,
    };
    let request_id = authority.request_id();
    let payload = serde_json::json!({
        "type": "AgentLlmUsage",
        "agent_id": authority.grant.authority.agent_id,
        "tenant_id": authority.grant.authority.tenant_id,
        "project_id": authority.grant.authority.project_id,
        "work_item_id": authority.grant.authority.work_item_id,
        "reservation_id": authority.grant.provider_allowance_id,
        "assignment_id": authority.assignment_id,
        "assignment_version": authority.grant.authority.assignment_version,
        "provider": authority.grant.provider,
        "caller_role": "agent_runtime",
        "effective_model": authority.grant.model,
        "requested_model": authority.grant.model,
        "tier": "mid",
        "hierarchy_tier": 2,
        "cost_source": "provider_reported",
        "input_tokens": 5,
        "output_tokens": 5,
        "cache_read": 0,
        "cache_creation": 0,
        "cost_usd": 0.0
    });
    let event = DomainEvent::new(
        "agent_llm_usage",
        &authority.grant.authority.agent_id.to_string(),
        &payload.to_string(),
        &request_id,
        1,
    )
    .with_operation_id(&format!("llm_usage_{request_id}"))
    .with_schema_version(3);
    completion.validate_usage(&event).unwrap();

    let mut wrong_schema = event.clone();
    wrong_schema.schema_version = 2;
    assert!(completion.validate_usage(&wrong_schema).is_err());
    let mut wrong_aggregate = event.clone();
    wrong_aggregate.aggregate_id = AgentId(99).to_string();
    assert!(completion.validate_usage(&wrong_aggregate).is_err());
    for (field, value) in [
        ("tier", serde_json::json!("")),
        ("hierarchy_tier", serde_json::Value::Null),
        ("cost_source", serde_json::Value::Null),
    ] {
        let mut changed = payload.clone();
        changed[field] = value;
        let mut invalid = event.clone();
        invalid.payload = changed.to_string();
        assert!(completion.validate_usage(&invalid).is_err(), "{field}");
    }
}
