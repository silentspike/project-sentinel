#![cfg(test)]

//! Component fixtures only. Descriptive DTOs here do not prove a completed
//! workflow journal; the store proof and full handoff are tested independently.
use super::super::model_execution::{AdaptiveProviderAuthority, ProviderExecutionAuthority};
use super::*;

struct Fixture {
    temp: tempfile::TempDir,
    api: WorkflowApi,
    proof: sentinel_workflow::AdaptiveSourceCompletionV1,
    binding: AdaptiveProviderAuthority,
    observation: sentinel_common::WorkbenchPrivateObservation,
    completion: ModelExecutionCompletion,
    usage: DomainEvent,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let (api, binding, _) = super::super::model_work::configured_adaptive_test_api(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
        );
        let provider_source = api
            .store
            .adaptive_session_for_authority(&binding.grant.authority)
            .unwrap()
            .unwrap();
        let artifact_digest = "a".repeat(64);
        let observation = sentinel_common::WorkbenchPrivateObservation::from_result(
            &sentinel_common::WorkbenchMessage::Result {
                schema_version: sentinel_common::WORKBENCH_SCHEMA_VERSION,
                invocation_id: Uuid::new_v4().to_string(),
                input_digest: "b".repeat(64),
                outcome: sentinel_common::WorkbenchOutcome::Succeeded,
                resources: sentinel_common::WorkbenchResourceUsage::default(),
                artifacts: vec![sentinel_common::WorkbenchArtifactRef {
                    artifact_id: format!("sha256:{artifact_digest}"),
                    sha256: artifact_digest.clone(),
                    artifact_kind: "source_tree".into(),
                    media_type: "application/vnd.sentinel.source-tree+json".into(),
                    size_bytes: 42,
                    manifest_path: format!("{artifact_digest}.manifest.json"),
                }],
                output: BTreeMap::new(),
                error: None,
            },
        )
        .unwrap();
        let mut context = api.prepare_adaptive_model(&binding).unwrap();
        // Isolate the provider boundary's retained-observation equality check.
        // This descriptor is never submitted as a real package/journal proof.
        context.observation = Some(observation.clone());
        let completion = ModelExecutionCompletion {
            context: ModelExecutionContext::Adaptive(Box::new(context)),
            content: serde_json::json!({"schema_version":1,"decision":{
                "kind":"propose_completion","artifact_digest":artifact_digest,
            }})
            .to_string(),
            admissible: true,
        };
        let request_id = binding.request_id();
        let payload = serde_json::json!({
            "type":"AgentLlmUsage", "agent_id":binding.grant.authority.agent_id,
            "tenant_id":binding.grant.authority.tenant_id,
            "project_id":binding.grant.authority.project_id,
            "work_item_id":binding.grant.authority.work_item_id,
            "reservation_id":binding.grant.provider_allowance_id,
            "assignment_id":binding.assignment_id,
            "assignment_version":binding.grant.authority.assignment_version,
            "provider":binding.grant.provider,"requested_model":binding.grant.model,
            "effective_model":binding.grant.model,"caller_role":"agent_runtime",
            "tier":"mid","hierarchy_tier":2,"cost_source":"provider_reported",
            "input_tokens":5,"output_tokens":5,"cache_read":0,"cache_creation":0,"cost_usd":0.0,
        });
        let usage = DomainEvent::new(
            "agent_llm_usage",
            &binding.grant.authority.agent_id.to_string(),
            &payload.to_string(),
            &request_id,
            1,
        )
        .with_operation_id(&format!("llm_usage_{request_id}"))
        .with_schema_version(3);
        completion.validate_usage(&usage).unwrap();
        Self {
            temp,
            api,
            proof: sentinel_workflow::AdaptiveSourceCompletionV1 {
                provider_source,
                effect: sentinel_workflow::AdaptiveEffectV1 {
                    id: binding.effect_id,
                    request_digest: "c".repeat(64),
                },
                model_response_digest: hex_sha256(completion.content.as_bytes()),
                artifact_digest,
            },
            binding,
            observation,
            completion,
            usage,
        }
    }

    fn retain(&self, bind_reservation: bool, claim: bool) {
        let events = self.api.event_store.as_ref().unwrap();
        let request_id = self.binding.request_id();
        let digest = &self.proof.effect.request_digest;
        events
            .reserve_llm_request(
                &request_id,
                digest,
                &self.binding.grant.authority.agent_id.to_string(),
            )
            .unwrap();
        if bind_reservation {
            let grant = &self.binding.grant;
            events
                .bind_llm_model_reservation(&sentinel_limbo::LlmModelReservationV1 {
                    schema_version: 1,
                    request_id: request_id.clone(),
                    request_digest: digest.clone(),
                    owner_scope: sentinel_common::StateTransferScope::for_agent(
                        grant.authority.agent_id.to_string(),
                    ),
                    subject: sentinel_limbo::LlmModelSubjectV1::Adaptive {
                        session_id: grant.session_id,
                        effect_id: self.binding.effect_id,
                        session_version: self.binding.session_version,
                    },
                    allowance_id: grant.provider_allowance_id.clone(),
                    context_digest: hex_sha256(
                        &serde_json::to_vec(&self.completion.context).unwrap(),
                    ),
                    authority_digest: hex_sha256(
                        &serde_json::to_vec(&ProviderExecutionAuthority::Adaptive(Box::new(
                            self.binding.clone(),
                        )))
                        .unwrap(),
                    ),
                    usage_binding: sentinel_limbo::LlmModelUsageBindingV1 {
                        agent_id: grant.authority.agent_id,
                        tenant_id: grant.authority.tenant_id.0.clone(),
                        project_id: grant.authority.project_id.0.clone(),
                        work_item_id: grant.authority.work_item_id.0.clone(),
                        reservation_id: grant.provider_allowance_id.clone(),
                        assignment_id: self.binding.assignment_id.clone(),
                        assignment_version: grant.authority.assignment_version,
                        provider: grant.provider.clone(),
                        model: grant.model.clone(),
                    },
                })
                .unwrap();
        }
        events.enqueue_llm_completion(&request_id, digest, &serde_json::json!({
            "version":2,"request_id":request_id,"request_digest":digest,
            "usage_event":self.usage,"model_work":self.completion,"actions":[],"tokens_used":10,
            "model_response_digest":self.proof.model_response_digest,
        }).to_string()).unwrap();
        events
            .persist_llm_completion_usage(&request_id, digest, &self.usage)
            .unwrap();
        if claim {
            assert!(events
                .claim_llm_completion_actions(&request_id, digest)
                .unwrap());
        }
    }

    fn validate(&self) -> Result<(), &'static str> {
        self.api
            .validate_adaptive_source_provider(&self.proof, &self.binding, &self.observation)
    }
}

#[test]
fn undispatched_done_is_discovery_only_without_completed_execution_proof() {
    let fixture = Fixture::new();
    let tenant = &fixture.binding.grant.authority.tenant_id;
    let project_id = &fixture.binding.grant.authority.project_id;
    let mut project = fixture
        .api
        .store
        .company_project(tenant, project_id)
        .unwrap()
        .unwrap();
    let original = project.clone();
    assert!(project
        .subscription_call
        .as_ref()
        .unwrap()
        .dispatch
        .is_none());
    assert!(!super::super::model_review::setup_due(&project));
    project.lifecycle_state = sentinel_workflow::ProjectLifecycleStateV1::DeliveryCandidate;
    project
        .work_items
        .get_mut(&fixture.binding.grant.authority.work_item_id)
        .unwrap()
        .state = sentinel_workflow::CompanyWorkStateV1::Done;
    assert!(super::super::model_review::setup_due(&project));
    // A descriptive Done value cannot manufacture a completed producer in SQL.
    assert!(fixture
        .api
        .store
        .adaptive_source_completion(
            tenant,
            project_id,
            project.subscription_call.as_ref().unwrap(),
            Uuid::new_v4(),
        )
        .unwrap()
        .is_none());
    assert_eq!(
        fixture
            .api
            .store
            .company_project(tenant, project_id)
            .unwrap(),
        Some(original)
    );
}

#[test]
fn provider_component_requires_usage_and_rejects_real_operator_resolution() {
    let fixture = Fixture::new();
    assert_eq!(fixture.validate(), Err("adaptive source usage missing"));
    fixture.retain(true, true);
    fixture.validate().unwrap();
    assert!(fixture
        .api
        .event_store
        .as_ref()
        .unwrap()
        .resolve_llm_completion_terminal(
            &fixture.binding.request_id(),
            &fixture.proof.effect.request_digest,
            "Synthetic operator-resolution refusal regression",
        )
        .unwrap());
    assert_eq!(
        fixture.validate(),
        Err("adaptive source provider was operator-resolved")
    );
}

#[test]
fn provider_component_retained_and_cleaned_results_survive_event_store_reopen() {
    let mut fixture = Fixture::new();
    fixture.retain(true, true);
    fixture.validate().unwrap();
    fixture.api.event_store = Some(
        sentinel_limbo::EventStore::open(
            fixture.temp.path().join("events.sqlite").to_str().unwrap(),
        )
        .unwrap(),
    );
    fixture.validate().unwrap();
    assert!(fixture
        .api
        .event_store
        .as_ref()
        .unwrap()
        .complete_llm_completion_actions(
            &fixture.binding.request_id(),
            &fixture.proof.effect.request_digest,
        )
        .unwrap());
    fixture.api.event_store = Some(
        sentinel_limbo::EventStore::open(
            fixture.temp.path().join("events.sqlite").to_str().unwrap(),
        )
        .unwrap(),
    );
    fixture.validate().unwrap();
}

#[test]
fn provider_component_rejects_unclaimed_or_missing_reservation_and_changed_provenance() {
    for mutation in [
        "unclaimed",
        "no-reservation",
        "response",
        "artifact",
        "usage",
        "reservation",
    ] {
        let mut fixture = Fixture::new();
        fixture.retain(mutation != "no-reservation", mutation != "unclaimed");
        match mutation {
            "response" => fixture.proof.model_response_digest = "d".repeat(64),
            "artifact" => fixture.proof.artifact_digest = "e".repeat(64),
            "usage" | "reservation" => {
                let db = sentinel_limbo::rusqlite::Connection::open(
                    fixture.temp.path().join("events.sqlite"),
                )
                .unwrap();
                let sql = if mutation == "usage" {
                    "UPDATE events SET payload=json_set(payload,'$.output_tokens',0) WHERE operation_id=?1"
                } else {
                    "UPDATE llm_completion_outbox SET model_binding=json_set(model_binding,'$.authority_digest',?2) WHERE request_id=?1"
                };
                if mutation == "usage" {
                    assert_eq!(
                        db.execute(sql, [format!("llm_usage_{}", fixture.binding.request_id())])
                            .unwrap(),
                        1
                    );
                } else {
                    assert_eq!(
                        db.execute(
                            sql,
                            sentinel_limbo::rusqlite::params![
                                fixture.binding.request_id(),
                                "f".repeat(64)
                            ]
                        )
                        .unwrap(),
                        1
                    );
                }
            }
            _ => {}
        }
        assert!(fixture.validate().is_err(), "{mutation}");
    }
}
