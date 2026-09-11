use super::*;
use crate::llm_bridge::bridge::ProviderUsageAuthorityResolver;

pub(crate) fn fixture(path: &Path) -> (WorkflowApi, RequestSalesContext) {
    let mut api = super::super::model_work::tests::configured_test_api(path);
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
        .push(crate::runtime_health::RuntimeHealthAgentSnapshot {
            agent_id: 3,
            aggregate_id: AgentId(3).to_string(),
            name: "Sales".into(),
            runtime_present: true,
            projection_present: true,
            security_runtime_present: true,
            adapter_handle_present: true,
            adapter_instance_matches: true,
            runtime_resources_healthy: true,
            adapter_health_state: Some(sentinel_common::NanoHealthState::Healthy),
            logical_status: Some(sentinel_runtime::AgentStatus::Active),
            ..Default::default()
        });
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
    assert!(api.resolve_provider_usage_authority(AgentId(6)).is_err());
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
        content: r#"{"schema_version":1,"decision":{"kind":"ask_question","content":"Which three pages should the website contain?"}}"#.into(), admissible: true };
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
fn sales_parser_does_not_accept_answers_approvals_or_legacy_tools() {
    for content in [
        r#"{"schema_version":1,"decision":{"kind":"accept_proposal"}}"#,
        r#"{"schema_version":1,"decision":{"kind":"ask_question","content":"Question?","approved":true}}"#,
        r#"{"schema_version":1,"tools":[]}"#,
    ] {
        assert!(serde_json::from_str::<SalesDecision>(content).is_err());
    }
}
