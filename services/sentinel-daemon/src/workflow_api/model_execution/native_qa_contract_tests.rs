#![cfg(test)]

// Store-backed native fixtures and real request serialization; no provider execution.
use super::*;
use crate::workflow_api::model_work::{native_qa_package_contract, AcceptedCustomerContract};

const LEGACY_QA_PARAGRAPH: &str = " Native source packages must include sentinel-qa.json with schema_version=1 and 1 to 64 concrete behavioral cases derived from the accepted requirements. Each case has id, script (a declared relative Python/Node script), args (string array), stdin, expected_stdout, expected_stderr, expected_exit (0 to 255). The independent evaluator executes each case against read-only source and compares actual output and exit status; candidate-supplied test counts are never authority. Include ordinary unittest/node:test tests as appropriate. Neither a test summary nor an empty test file substitutes for behavioral assertions.";

const FAMILIES: [(&str, &str, &str); 2] = [
    ("python-project-v1", "python-coding-v1", "app.py"),
    ("node-project-v1", "node-coding-v1", "app.mjs"),
];

#[derive(Serialize)]
struct LegacyContext {
    binding: AdaptiveProviderAuthority,
    task: sentinel_workflow::CompanyWorkItemSpecV1,
    accepted_customer_contract: AcceptedCustomerContract,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    artifact_inputs: Vec<crate::workflow_api::model_work::ModelArtifactInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    correction: Option<crate::workflow_api::model_work::ModelWorkCorrection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    observation: Option<WorkbenchPrivateObservation>,
    #[serde(skip_serializing_if = "is_false")]
    fresh_observation_required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_catalog: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    schema_retry_feedback: Option<sentinel_workflow::AdaptiveRecoveryFeedbackV1>,
    agent_context: AdaptiveAgentContextV1,
    #[serde(skip_serializing_if = "Option::is_none")]
    working_memory: Option<working_memory::AdaptiveWorkingMemoryV1>,
}

fn legacy_bytes(context: &AdaptiveModelContext) -> Vec<u8> {
    serde_json::to_vec(&LegacyContext {
        binding: context.binding.clone(),
        task: context.task.clone(),
        accepted_customer_contract: context.accepted_customer_contract.clone(),
        artifact_inputs: context.artifact_inputs.clone(),
        correction: context.correction.clone(),
        observation: context.observation.clone(),
        fresh_observation_required: context.fresh_observation_required,
        tool_catalog: context.tool_catalog.clone(),
        schema_retry_feedback: context.schema_retry_feedback.clone(),
        agent_context: context.agent_context.clone(),
        working_memory: context.working_memory.clone(),
    })
    .unwrap()
}

fn gateway_identity(
    context: &AdaptiveModelContext,
    state: &sentinel_redb::StateStore,
) -> (Vec<u8>, String) {
    let perception = sentinel_common::Perception {
        agent_id: context.binding.grant.authority.agent_id,
        circadian_text: String::new(),
        body_text: String::new(),
        environment_text: String::new(),
        acoustic_text: String::new(),
        heard_text: String::new(),
        presence_text: String::new(),
        impulse_text: String::new(),
        is_directly_addressed: false,
        timestamp: sentinel_common::Timestamp(100),
        tick: sentinel_common::Tick(100),
        room_id: "buero-dev-1".into(),
        max_priority: "NONE".into(),
        synth_fingerprint: "native-qa-contract-fixture".into(),
        personality_type: "E".into(),
        has_operator_impulse: false,
    };
    let envelope = ModelExecutionContext::Adaptive(Box::new(context.clone()));
    let identity = crate::llm_bridge::bridge::serialized_model_gateway_request_for_test(
        &envelope,
        &perception,
        state,
    )
    .unwrap();
    assert_eq!(identity.1, sentinel_common::sha256_hex(&identity.0));
    let request: serde_json::Value = serde_json::from_slice(&identity.0).unwrap();
    assert_eq!(
        request["metadata"]["company_execution_context_digest"],
        sentinel_common::sha256_hex(&serde_json::to_vec(&envelope).unwrap())
    );
    identity
}

fn reserve(fixture: &FamilyFixture, context: &AdaptiveModelContext, request_digest: &str) {
    let id = context.binding.request_id();
    let events = fixture.api.event_store.as_ref().unwrap();
    events
        .reserve_llm_request(
            &id,
            request_digest,
            &context.binding.grant.authority.agent_id.to_string(),
        )
        .unwrap();
    let reservation = crate::llm_bridge::bridge::model_reservation(
        &ModelExecutionContext::Adaptive(Box::new(context.clone())),
        &id,
        request_digest,
    )
    .unwrap()
    .unwrap();
    events.bind_llm_model_reservation(&reservation).unwrap();
}

fn prepare(
    fixture: &FamilyFixture,
    binding: &AdaptiveProviderAuthority,
) -> Result<AdaptiveModelContext, &'static str> {
    let (profile, digest) = fixture
        .api
        .authority
        .as_ref()
        .unwrap()
        .profile_for_binding(&binding.grant.authority.profile_id)
        .unwrap();
    with_private_observation_store_for_test(
        Arc::clone(&fixture.tools),
        profile.clone(),
        digest.into(),
        || fixture.api.prepare_adaptive_model(binding),
    )
}

fn failed_fixture(family: &str, profile: &str, path: &str) -> FamilyFixture {
    let mut fixture = FamilyFixture::new(family, profile);
    fixture.inspect(path, "scripted original source\n");
    let tool = if profile == "python-coding-v1" {
        WorkbenchTool::RunTests {
            suite_id: "python-unittest".into(),
            program: "python3".into(),
            args: ["-I", "-m", "unittest", "discover"]
                .map(str::to_owned)
                .to_vec(),
        }
    } else {
        WorkbenchTool::RunTests {
            suite_id: "node-tests".into(),
            program: "node".into(),
            args: ["--test", "--", "app.test.mjs"].map(str::to_owned).to_vec(),
        }
    };
    fixture.run_tests(tool, 1, "scripted relevant test failure\n");
    fixture.inspect(path, "scripted current source\n");
    fixture
}

fn reopen(fixture: FamilyFixture) -> FamilyFixture {
    let FamilyFixture {
        api,
        session,
        tools,
        _temp,
    } = fixture;
    let allowance = api.subscription_allowance_id.clone();
    drop(api);
    drop(tools);
    let path = _temp.path().join("company.sqlite");
    let mut api = crate::workflow_api::model_work::configured_test_api(&path);
    api.request_sales_total_limit = 10;
    api.subscription_allowance_id = allowance;
    api.event_store = Some(
        sentinel_limbo::EventStore::open(path.with_extension("events.sqlite").to_str().unwrap())
            .unwrap(),
    );
    api.workbench = Some(Arc::new(WorkbenchExecutionAdapter {
        store: Arc::clone(&api.store),
        authority: Arc::clone(api.authority.as_ref().unwrap()),
    }));
    let tools =
        Arc::new(WorkbenchInvocationStore::open(_temp.path().join("workbench.redb")).unwrap());
    FamilyFixture {
        api,
        session,
        tools,
        _temp,
    }
}

#[test]
fn shared_native_paragraph_is_exact_one_shot_legacy_text_and_native_only() {
    for (_, profile, _) in FAMILIES {
        assert_eq!(
            native_qa_package_contract(profile),
            Some(LEGACY_QA_PARAGRAPH)
        );
        let mut one_shot = crate::workflow_api::model_work::test_context();
        one_shot.authority.profile_id = profile.into();
        let prompt = one_shot.prompt().unwrap();
        assert_eq!(prompt.matches(LEGACY_QA_PARAGRAPH).count(), 1);
        assert!(prompt.ends_with(LEGACY_QA_PARAGRAPH));
    }
    for profile in [
        "web-authoring-v1",
        "coding-qa-v1",
        "python-project-v1",
        "unknown",
    ] {
        assert_eq!(native_qa_package_contract(profile), None);
    }
}

#[test]
fn fresh_native_contract_is_bound_presentation_not_task_or_execution_authority() {
    for (family, profile, _) in FAMILIES {
        let fixture = FamilyFixture::new(family, profile);
        let source = fixture.session.clone();
        let context = fixture.context();
        assert_eq!(context.native_qa_contract_version, Some(1));
        context.validate_dispatch(now_unix_ms()).unwrap();
        assert_eq!(
            context
                .prompt()
                .unwrap()
                .matches(LEGACY_QA_PARAGRAPH)
                .count(),
            1
        );
        let mut legacy = context.clone();
        legacy.native_qa_contract_version = None;
        let bytes = legacy_bytes(&legacy);
        assert_eq!(serde_json::to_vec(&legacy).unwrap(), bytes);
        let decoded: AdaptiveModelContext = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded, legacy);
        assert!(!legacy.prompt().unwrap().contains("sentinel-qa.json"));
        assert_eq!(context.binding, legacy.binding);
        assert_eq!(context.task, legacy.task);
        assert_eq!(
            context.accepted_customer_contract,
            legacy.accepted_customer_contract
        );
        assert_eq!(context.working_memory, legacy.working_memory);
        assert_eq!(
            fixture
                .api
                .store
                .adaptive_session(source.grant.session_id, &source.grant.authority)
                .unwrap()
                .unwrap(),
            source
        );
        let temp = tempfile::tempdir().unwrap();
        let state =
            sentinel_redb::StateStore::open(temp.path().join("state.redb").to_str().unwrap())
                .unwrap();
        assert_eq!(
            gateway_identity(&decoded, &state),
            gateway_identity(&legacy, &state)
        );
        assert_ne!(
            gateway_identity(&context, &state),
            gateway_identity(&legacy, &state)
        );
    }
}

#[test]
fn every_legacy_and_marked_memory_variant_keeps_exact_reserved_request_after_claim_and_reopen() {
    for (family, profile, path) in FAMILIES {
        for variant in [
            "absent",
            "numeric",
            "enriched",
            "omitted",
            "marked-numeric",
            "marked-enriched",
            "marked-omitted",
        ] {
            let mut fixture = failed_fixture(family, profile, path);
            let mut sealed = fixture.context();
            assert!(sealed
                .working_memory
                .as_ref()
                .unwrap()
                .retained_failure_output
                .is_some());
            sealed.native_qa_contract_version = variant.starts_with("marked-").then_some(1);
            if variant == "absent" {
                sealed.working_memory = None;
            } else if variant.contains("numeric") || variant.contains("omitted") {
                // Optional-output omission has exactly the numeric history bytes.
                sealed
                    .working_memory
                    .as_mut()
                    .unwrap()
                    .retained_failure_output = None;
            }
            if sealed.native_qa_contract_version.is_none() {
                assert_eq!(serde_json::to_vec(&sealed).unwrap(), legacy_bytes(&sealed));
            }
            let temp = tempfile::tempdir().unwrap();
            let state =
                sentinel_redb::StateStore::open(temp.path().join("state.redb").to_str().unwrap())
                    .unwrap();
            let identity = gateway_identity(&sealed, &state);
            let bytes = serde_json::to_vec(&sealed).unwrap();
            let prompt = sealed.prompt().unwrap();
            reserve(&fixture, &sealed, &identity.1);
            assert_eq!(prepare(&fixture, &sealed.binding).unwrap(), sealed);
            fixture.advance(
                AdaptiveTransitionV1::ClaimModel {
                    effect: AdaptiveEffectV1 {
                        id: sealed.binding.effect_id,
                        request_digest: identity.1.clone(),
                    },
                    previous_observation_digest: sealed
                        .binding
                        .previous_observation
                        .as_ref()
                        .map(|reference| reference.observation_digest.clone()),
                },
                now_unix_ms(),
            );
            let fixture = reopen(fixture);
            let source = fixture.session.clone();
            for _ in 0..2 {
                let restored = prepare(&fixture, &sealed.binding).unwrap();
                assert_eq!(restored, sealed, "{family}/{variant}");
                assert_eq!(serde_json::to_vec(&restored).unwrap(), bytes);
                assert_eq!(restored.prompt().unwrap(), prompt);
                assert_eq!(gateway_identity(&restored, &state), identity);
                let (profile, digest) = fixture
                    .api
                    .authority
                    .as_ref()
                    .unwrap()
                    .profile_for_binding(&sealed.binding.grant.authority.profile_id)
                    .unwrap();
                with_private_observation_store_for_test(
                    Arc::clone(&fixture.tools),
                    profile.clone(),
                    digest.into(),
                    || {
                        fixture
                            .api
                            .validate_prepared_adaptive_model_context(&sealed.binding, &restored)
                            .unwrap()
                    },
                );
                assert_eq!(
                    fixture
                        .api
                        .store
                        .adaptive_session(source.grant.session_id, &source.grant.authority)
                        .unwrap()
                        .unwrap(),
                    source
                );
            }
        }
    }
}

#[test]
fn unknown_versions_and_non_native_markers_fail_prompt_and_dispatch() {
    let fixture = FamilyFixture::new("python-project-v1", "python-coding-v1");
    let context = fixture.context();
    for version in [0, 2, u16::MAX] {
        let mut invalid = context.clone();
        invalid.native_qa_contract_version = Some(version);
        assert_eq!(
            invalid.prompt(),
            Err("native QA contract version is unsupported")
        );
        assert_eq!(
            invalid.validate_dispatch(now_unix_ms()),
            Err("native QA contract version is unsupported")
        );
    }
    for profile in ["web-authoring-v1", "coding-qa-v1", "unknown"] {
        let mut invalid = context.clone();
        invalid.binding.grant.authority.profile_id = profile.into();
        assert_eq!(
            invalid.prompt(),
            Err("native QA contract requires a native coding profile")
        );
        assert_eq!(
            invalid.validate_dispatch(now_unix_ms()),
            Err("native QA contract requires a native coding profile")
        );
    }
}

#[test]
fn reserved_unknown_marker_is_rejected_not_downgraded_or_upgraded() {
    for (family, profile, path) in FAMILIES {
        let fixture = failed_fixture(family, profile, path);
        let mut context = fixture.context();
        context.native_qa_contract_version = Some(2);
        reserve(&fixture, &context, &"d".repeat(64));
        assert_eq!(
            prepare(&fixture, &context.binding).unwrap_err(),
            "adaptive retained context changed"
        );
        assert_eq!(
            fixture
                .api
                .store
                .adaptive_session(
                    fixture.session.grant.session_id,
                    &fixture.session.grant.authority
                )
                .unwrap()
                .unwrap(),
            fixture.session
        );
    }
}

#[test]
fn web_presentation_remains_unmarked_and_legacy_encoded() {
    let fixture = FamilyFixture::new("web-project-v1", "web-authoring-v1");
    let context = fixture.context();
    assert_eq!(context.native_qa_contract_version, None);
    assert_eq!(
        serde_json::to_vec(&context).unwrap(),
        legacy_bytes(&context)
    );
    assert!(!context.prompt().unwrap().contains("sentinel-qa.json"));
}

#[test]
fn prompt_cap_can_omit_only_optional_failure_not_native_contract_or_numeric_history() {
    // Synthetic padding tests the real prompt fitter, not a large persisted task.
    let fixture = failed_fixture("python-project-v1", "python-coding-v1", "app.py");
    let mut enriched = fixture.context();
    let mut numeric = enriched.clone();
    numeric
        .working_memory
        .as_mut()
        .unwrap()
        .retained_failure_output = None;
    let limit = crate::workflow_api::model_work::MAX_MODEL_WORK_BYTES;
    let padding = "x".repeat(limit - numeric.prompt().unwrap().len());
    numeric.task.objective.push_str(&padding);
    enriched.task.objective.push_str(&padding);
    assert_eq!(numeric.prompt().unwrap().len(), limit);
    assert_eq!(
        enriched.prompt(),
        Err("adaptive model context exceeds its bound")
    );
    assert!(enriched.fit_optional_failure_output_to_prompt().unwrap());
    assert_eq!(enriched, numeric);
    assert_eq!(enriched.native_qa_contract_version, Some(1));
    assert!(enriched.prompt().unwrap().contains(LEGACY_QA_PARAGRAPH));
    let mut legacy_at_limit = numeric.clone();
    legacy_at_limit.native_qa_contract_version = None;
    legacy_at_limit
        .task
        .objective
        .push_str(&"x".repeat(LEGACY_QA_PARAGRAPH.len()));
    assert_eq!(legacy_at_limit.prompt().unwrap().len(), limit);
    legacy_at_limit.native_qa_contract_version = Some(1);
    assert!(!legacy_at_limit
        .fit_optional_failure_output_to_prompt()
        .unwrap());
    assert_eq!(
        legacy_at_limit.prompt(),
        Err("adaptive model context exceeds its bound")
    );
    assert_eq!(legacy_at_limit.native_qa_contract_version, Some(1));
}

#[test]
fn model_response_cannot_supply_native_contract_marker_as_authority() {
    for value in [
        serde_json::json!({"schema_version":1, "native_qa_contract_version":1,
            "decision":{"kind":"blocked","reason_code":"fixture"}}),
        serde_json::json!({"schema_version":1,
            "decision":{"kind":"propose_completion","artifact_digest":"a".repeat(64),
                "native_qa_contract_version":1}}),
    ] {
        assert!(parse_adaptive_decision(&value.to_string()).is_err());
    }
}
