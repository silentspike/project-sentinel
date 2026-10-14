#![cfg(feature = "llm")]

use super::*;

fn advance(
    api: &WorkflowApi,
    session: &AdaptiveSessionV1,
    transition: AdaptiveTransitionV1,
) -> AdaptiveSessionV1 {
    api.store
        .advance_adaptive_session(
            session.grant.session_id,
            session.version,
            Uuid::new_v4(),
            &transition,
            &session.grant.authority,
            now_unix_ms().max(session.updated_at_ms),
        )
        .unwrap()
        .1
}

fn overview(api: &WorkflowApi, path: &str) -> serde_json::Value {
    let customer = api.principals.principal("customer").unwrap();
    let response = api.customer_overview(&customer, path);
    assert_eq!(response.status, 200);
    serde_json::from_slice(&response.body).unwrap()
}

// Independent legacy collection reads pin the public projection, not private DTOs.
fn expected_overview(api: &WorkflowApi) -> serde_json::Value {
    let customer = api.principals.principal("customer").unwrap();
    let tenant = &customer.principal.tenant_id;
    let customer_id = customer.principal.customer_id.as_deref().unwrap();
    let requests = api
        .store
        .company_customer_requests(tenant, customer_id)
        .unwrap();
    let mut proposals = Vec::new();
    for request in &requests {
        for id in &request.proposal_ids {
            let proposal = api.store.company_proposal(tenant, id).unwrap().unwrap();
            proposals.push(serde_json::json!({
                "proposal_id": proposal.proposal_id,
                "request_id": proposal.request_id,
                "generation": proposal.generation,
                "proposal_digest": proposal.proposal_digest,
                "scope": proposal.binding.scope,
                "deliverables": proposal.binding.deliverables,
                "exclusions": proposal.binding.exclusions,
                "acceptance_criteria": proposal.binding.acceptance_criteria,
                "assumptions": proposal.binding.assumptions,
                "cost_ceiling_micros": proposal.binding.cost_ceiling_micros,
                "expires_at_unix_ms": proposal.binding.expires_at_unix_ms,
            }));
        }
    }
    let projects = api
        .store
        .company_customer_projects(tenant, customer_id)
        .unwrap()
        .into_iter()
        .map(|project| {
            let agreement = api
                .store
                .company_agreement(tenant, &project.agreement_id)
                .unwrap()
                .unwrap();
            let rows = project
                .work_items
                .iter()
                .map(|(id, work)| serde_json::json!({"work_item_id": id, "state": work.state}))
                .collect::<Vec<_>>();
            serde_json::json!({
                "project_id": project.project_id,
                "request_id": agreement.request_id,
                "state": project.lifecycle_state,
                "version": project.version,
                "work_items": rows,
                "deliveries": [],
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({"requests": requests, "proposals": proposals, "projects": projects})
}

#[test]
fn batched_customer_overview_preserves_full_wire_json_and_selected_request() {
    let temp = tempfile::tempdir().unwrap();
    let (api, _) = adaptive_recovery::tests::fixture(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
        false,
    );
    model_work::assign_test_work_from(&api, Some(1), 100);
    let expected = expected_overview(&api);
    assert_eq!(expected["requests"].as_array().unwrap().len(), 2);
    assert_eq!(expected["projects"].as_array().unwrap().len(), 2);
    let projects = api.store.company_projects().unwrap();
    let cursor = api.store.company_event_cursor().unwrap();
    for _ in 0..2 {
        assert_eq!(overview(&api, CUSTOMER_OVERVIEW_PATH), expected);
        for request in expected["requests"].as_array().unwrap() {
            let id = request["request_id"].as_str().unwrap();
            let mut selected = expected.clone();
            selected["requests"] = serde_json::json!([request]);
            for key in ["proposals", "projects"] {
                selected[key]
                    .as_array_mut()
                    .unwrap()
                    .retain(|row| row["request_id"].as_str() == Some(id));
            }
            assert_eq!(
                overview(&api, &format!("{CUSTOMER_OVERVIEW_PATH}?request_id={id}")),
                selected
            );
        }
    }
    assert_eq!(api.store.company_projects().unwrap(), projects);
    assert_eq!(api.store.company_event_cursor().unwrap(), cursor);
}

#[test]
fn batched_customer_overview_foreign_selected_request_stays_forbidden() {
    let temp = tempfile::tempdir().unwrap();
    let (api, _) = adaptive_recovery::tests::fixture(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
        false,
    );
    let expected = overview(&api, CUSTOMER_OVERVIEW_PATH);
    let request_id = expected["requests"][0]["request_id"].as_str().unwrap();
    let mut foreign = api.principals.principal("customer").unwrap();
    foreign.principal.customer_id = Some("another-customer".into());
    let cursor = api.store.company_event_cursor().unwrap();
    for _ in 0..2 {
        let response = api.customer_overview(
            &foreign,
            &format!("{CUSTOMER_OVERVIEW_PATH}?request_id={request_id}"),
        );
        assert_eq!(response.status, 403);
        let expected_error = json_error(
            403,
            "authority_conflict",
            "customer request is foreign",
            false,
        );
        assert_eq!(response.body, expected_error.body);
        assert_eq!(
            overview_for_principal(&api, &foreign),
            serde_json::json!({"requests": [], "proposals": [], "projects": []})
        );
    }
    assert_eq!(api.store.company_event_cursor().unwrap(), cursor);
}

fn overview_for_principal(api: &WorkflowApi, principal: &BoundPrincipal) -> serde_json::Value {
    let response = api.customer_overview(principal, CUSTOMER_OVERVIEW_PATH);
    assert_eq!(response.status, 200);
    serde_json::from_slice(&response.body).unwrap()
}

#[test]
fn batched_customer_overview_empty_filter_and_missing_request_keep_distinct_semantics() {
    let temp = tempfile::tempdir().unwrap();
    let (api, _) = adaptive_recovery::tests::fixture(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
        false,
    );
    let expected = expected_overview(&api);
    assert_eq!(
        overview(&api, &format!("{CUSTOMER_OVERVIEW_PATH}?request_id=")),
        expected
    );
    assert_eq!(
        overview(
            &api,
            &format!("{CUSTOMER_OVERVIEW_PATH}?request_id=request-does-not-exist")
        ),
        serde_json::json!({"requests": [], "proposals": [], "projects": []})
    );
    let empty = model_work::configured_test_api(&temp.path().join("empty.sqlite"));
    assert_eq!(
        overview(&empty, CUSTOMER_OVERVIEW_PATH),
        serde_json::json!({"requests": [], "proposals": [], "projects": []})
    );
}

#[test]
fn batched_customer_overview_unknown_outcomes_are_private_read_only_fixed_labels() {
    for tool_unknown in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let (api, ready) = adaptive_recovery::tests::fixture(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
            false,
        );
        let mut effect = AdaptiveEffectV1 {
            id: Uuid::new_v4(),
            request_digest: "a".repeat(64),
        };
        let mut pending = advance(
            &api,
            &ready,
            AdaptiveTransitionV1::ClaimModel {
                effect: effect.clone(),
                previous_observation_digest: None,
            },
        );
        if tool_unknown {
            let tool = WorkbenchTool::InspectFile {
                path: "private-batched-read-path.txt".into(),
                max_bytes: 1024,
            };
            let tool_digest = sentinel_workflow::adaptive_tool_digest(&tool).unwrap();
            let proposed = advance(
                &api,
                &pending,
                AdaptiveTransitionV1::ResolveModel {
                    effect,
                    result_digest: "b".repeat(64),
                    decision: sentinel_workflow::AdaptiveModelDecisionV1::Tool {
                        tool,
                        tool_digest: tool_digest.clone(),
                    },
                },
            );
            effect = AdaptiveEffectV1 {
                id: Uuid::new_v4(),
                request_digest: "c".repeat(64),
            };
            pending = advance(
                &api,
                &proposed,
                AdaptiveTransitionV1::ClaimTool {
                    effect: effect.clone(),
                    tool_digest,
                },
            );
        }
        let unknown = advance(&api, &pending, AdaptiveTransitionV1::MarkUnknown { effect });
        let mut expected = expected_overview(&api);
        expected["projects"][0]["work_items"][0]["adaptive_progress"] = if tool_unknown {
            serde_json::json!({"status": "tool_outcome_unknown", "label": "Tool outcome unknown."})
        } else {
            serde_json::json!({"status": "model_outcome_unknown", "label": "Model outcome unknown."})
        };
        let projects = api.store.company_projects().unwrap();
        let cursor = api.store.company_event_cursor().unwrap();
        for _ in 0..2 {
            let body = overview(&api, CUSTOMER_OVERVIEW_PATH);
            assert_eq!(body, expected);
            let bytes = body.to_string();
            for private in [
                unknown.grant.session_id.to_string(),
                "private-batched-read-path.txt".into(),
                "request_digest".into(),
                "tool_digest".into(),
                "max_bytes".into(),
            ] {
                assert!(!bytes.contains(&private));
            }
        }
        assert_eq!(api.store.company_projects().unwrap(), projects);
        assert_eq!(api.store.company_event_cursor().unwrap(), cursor);
        assert_eq!(
            api.store
                .adaptive_session(unknown.grant.session_id, &unknown.grant.authority)
                .unwrap(),
            Some(unknown)
        );
    }
}

#[test]
fn batched_customer_overview_warm_read_rejects_noncurrent_exact_head() {
    let temp = tempfile::tempdir().unwrap();
    let company = temp.path().join("company.sqlite");
    let (api, ready) =
        adaptive_recovery::tests::fixture(&company, &temp.path().join("events.sqlite"), false);
    overview(&api, CUSTOMER_OVERVIEW_PATH);
    let connection = sentinel_limbo::rusqlite::Connection::open(&company).unwrap();
    assert_eq!(
        connection
            .execute(
                "UPDATE workflow_adaptive_heads SET version=version+1 WHERE session_id=?1",
                [ready.grant.session_id.to_string()],
            )
            .unwrap(),
        1
    );
    let response = api.customer_overview(
        &api.principals.principal("customer").unwrap(),
        CUSTOMER_OVERVIEW_PATH,
    );
    assert_eq!(response.status, 503);
    let body: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(body["code"], "customer_progress_unavailable");
    assert_eq!(body["error"], "Customer progress is unavailable");
    assert!(!body.to_string().contains(company.to_str().unwrap()));
    assert!(!body
        .to_string()
        .contains(&ready.grant.session_id.to_string()));
}

#[test]
fn batched_customer_progress_final_comparison_rejects_project_changed_after_capture() {
    let temp = tempfile::tempdir().unwrap();
    let (api, ready) = adaptive_recovery::tests::fixture(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
        false,
    );
    let authority = &ready.grant.authority;
    let captured = api
        .store
        .company_project(&authority.tenant_id, &authority.project_id)
        .unwrap()
        .unwrap();
    assert!(api
        .store
        .company_projects_match_snapshot(std::slice::from_ref(&captured))
        .unwrap());
    api.store
        .apply_company_command(
            &api.principals.principal("pm").unwrap().principal,
            Uuid::new_v4(),
            &CompanyWorkflowCommandV1::RaiseBlocker {
                project_id: captured.project_id.clone(),
                expected_version: captured.version,
                work_item_id: Some(authority.work_item_id.clone()),
                cause_ref: "batched-read-dependency".into(),
                owner: AgentId(5),
            },
            now_unix_ms(),
        )
        .unwrap();
    let current = api
        .store
        .company_project(&authority.tenant_id, &authority.project_id)
        .unwrap()
        .unwrap();
    assert_ne!(captured, current);
    assert_eq!(captured.subscription_call, current.subscription_call);
    let cursor = api.store.company_event_cursor().unwrap();
    assert!(!api
        .store
        .company_projects_match_snapshot(std::slice::from_ref(&captured))
        .unwrap());
    assert_eq!(
        overview(&api, CUSTOMER_OVERVIEW_PATH),
        expected_overview(&api)
    );
    assert_eq!(api.store.company_event_cursor().unwrap(), cursor);
}

#[test]
fn batched_adaptive_preparation_preserves_ready_and_pending_legacy_context_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let (api, binding, _) = model_work::configured_adaptive_test_api(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    let ready = api.prepare_adaptive_model(&binding).unwrap();
    let bytes = serde_json::to_vec(&ready).unwrap();
    assert_eq!(
        serde_json::to_vec(&api.prepare_adaptive_model(&binding).unwrap()).unwrap(),
        bytes
    );
    let session = api
        .store
        .adaptive_session(binding.grant.session_id, &binding.grant.authority)
        .unwrap()
        .unwrap();
    let pending = advance(
        &api,
        &session,
        AdaptiveTransitionV1::ClaimModel {
            effect: AdaptiveEffectV1 {
                id: binding.effect_id,
                request_digest: "d".repeat(64),
            },
            previous_observation_digest: None,
        },
    );
    let mut expected_legacy = ready;
    expected_legacy.working_memory = None;
    let cursor = api.store.company_event_cursor().unwrap();
    for _ in 0..2 {
        assert_eq!(
            serde_json::to_vec(&api.prepare_adaptive_model(&binding).unwrap()).unwrap(),
            serde_json::to_vec(&expected_legacy).unwrap()
        );
    }
    assert_eq!(api.store.company_event_cursor().unwrap(), cursor);
    assert_eq!(
        api.store
            .adaptive_session(binding.grant.session_id, &binding.grant.authority)
            .unwrap(),
        Some(pending)
    );
}

#[test]
fn batched_adaptive_preparation_schema_missing_session_and_effect_error_order() {
    let temp = tempfile::tempdir().unwrap();
    let (api, binding, _) = model_work::configured_adaptive_test_api(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    let mut invalid = binding.clone();
    invalid.schema_version = 0;
    invalid.grant.session_id = Uuid::new_v4();
    invalid.effect_id = Uuid::new_v4();
    invalid.assignment_id = "foreign-assignment".into();
    assert_eq!(
        api.prepare_adaptive_model(&invalid).unwrap_err(),
        "adaptive provider schema is invalid"
    );
    invalid.schema_version = binding.schema_version;
    assert_eq!(
        api.prepare_adaptive_model(&invalid).unwrap_err(),
        "adaptive session missing"
    );
    invalid.grant = binding.grant;
    assert_eq!(
        api.prepare_adaptive_model(&invalid).unwrap_err(),
        "adaptive model effect changed"
    );
}

#[test]
fn batched_adaptive_preparation_project_fault_precedes_bad_grant_or_effect() {
    for missing_project in [false, true] {
        for bad_grant in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let company = temp.path().join("company.sqlite");
            let (api, binding, _) = model_work::configured_adaptive_test_api(
                &company,
                &temp.path().join("events.sqlite"),
            );
            api.prepare_adaptive_model(&binding).unwrap();
            let mut invalid = binding.clone();
            let healthy_error = if bad_grant {
                invalid.grant.model = "foreign-model".into();
                assert_ne!(invalid.grant.model, binding.grant.model);
                "adaptive session authority changed"
            } else {
                invalid.effect_id = Uuid::new_v4();
                "adaptive model effect changed"
            };
            assert_eq!(
                api.prepare_adaptive_model(&invalid).unwrap_err(),
                healthy_error
            );

            let connection = sentinel_limbo::rusqlite::Connection::open(&company).unwrap();
            let sql = if missing_project {
                "DELETE FROM company_entities WHERE tenant_id=?1 AND entity_kind='project' AND entity_id=?2"
            } else {
                "UPDATE company_entities SET payload_digest=?3 WHERE tenant_id=?1 AND entity_kind='project' AND entity_id=?2"
            };
            let authority = &binding.grant.authority;
            let changed = if missing_project {
                connection.execute(sql, [&authority.tenant_id.0, &authority.project_id.0])
            } else {
                connection.execute(
                    sql,
                    sentinel_limbo::rusqlite::params![
                        authority.tenant_id.0,
                        authority.project_id.0,
                        "not-a-canonical-project-digest",
                    ],
                )
            }
            .unwrap();
            assert_eq!(changed, 1);
            for _ in 0..2 {
                assert_eq!(
                    api.prepare_adaptive_model(&invalid).unwrap_err(),
                    "adaptive session unavailable"
                );
            }
            invalid.schema_version = 0;
            assert_eq!(
                api.prepare_adaptive_model(&invalid).unwrap_err(),
                "adaptive provider schema is invalid"
            );
        }
    }
}

#[test]
fn batched_adaptive_preparation_runtime_authority_failure_precedes_missing_session() {
    for revoked in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let (api, binding, _) = model_work::configured_adaptive_test_api(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
        );
        let mut missing = binding.clone();
        missing.grant.session_id = Uuid::new_v4();
        assert_eq!(
            api.prepare_adaptive_model(&missing).unwrap_err(),
            "adaptive session missing"
        );
        let authority = api.authority.as_ref().unwrap();
        let original = authority.runtime_health.read().unwrap().clone();
        {
            let mut health = authority.runtime_health.write().unwrap();
            if revoked {
                health
                    .agents
                    .iter_mut()
                    .find(|agent| agent.agent_id == binding.grant.authority.agent_id.0)
                    .unwrap()
                    .expected_active = false;
            } else {
                health
                    .agents
                    .retain(|agent| agent.agent_id != binding.grant.authority.agent_id.0);
            }
        }
        for _ in 0..2 {
            assert_eq!(
                api.prepare_adaptive_model(&missing).unwrap_err(),
                "adaptive session unavailable"
            );
        }
        let mut invalid_schema = missing.clone();
        invalid_schema.schema_version = 0;
        assert_eq!(
            api.prepare_adaptive_model(&invalid_schema).unwrap_err(),
            "adaptive provider schema is invalid"
        );
        *authority.runtime_health.write().unwrap() = original;
        assert_eq!(
            api.prepare_adaptive_model(&missing).unwrap_err(),
            "adaptive session missing"
        );
    }
}

#[test]
fn batched_adaptive_preparation_retains_exact_legacy_digest_and_rejects_mismatch() {
    use model_execution::ModelExecutionContext;
    use sentinel_limbo::{LlmModelReservationV1, LlmModelSubjectV1, LlmModelUsageBindingV1};

    for exact_digest in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let (api, binding, events) = model_work::configured_adaptive_test_api(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
        );
        let mut legacy = api.prepare_adaptive_model(&binding).unwrap();
        assert!(legacy.working_memory.is_some());
        legacy.working_memory = None;
        let legacy_bytes =
            serde_json::to_vec(&ModelExecutionContext::Adaptive(Box::new(legacy.clone()))).unwrap();
        let expected_digest = hex_sha256(&legacy_bytes);
        let request_id = binding.request_id();
        let request_digest = "d".repeat(64);
        let authority = &binding.grant.authority;
        let owner = sentinel_common::StateTransferScope::for_agent(authority.agent_id.to_string());
        events
            .reserve_llm_request(
                &request_id,
                &request_digest,
                &authority.agent_id.to_string(),
            )
            .unwrap();
        let reservation = LlmModelReservationV1 {
            schema_version: 1,
            request_id: request_id.clone(),
            request_digest,
            owner_scope: owner.clone(),
            subject: LlmModelSubjectV1::Adaptive {
                session_id: binding.grant.session_id,
                effect_id: binding.effect_id,
                session_version: binding.session_version,
            },
            allowance_id: binding.grant.provider_allowance_id.clone(),
            context_digest: if exact_digest {
                expected_digest
            } else {
                let modern = api.prepare_adaptive_model(&binding).unwrap();
                let modern_bytes =
                    serde_json::to_vec(&ModelExecutionContext::Adaptive(Box::new(modern))).unwrap();
                let digest = hex_sha256(&modern_bytes);
                assert_ne!(digest, expected_digest);
                // Neither the legacy nor the modern envelope is this commitment.
                let mismatch = "0".repeat(64);
                assert_ne!(mismatch, digest);
                assert_ne!(mismatch, expected_digest);
                mismatch
            },
            authority_digest: binding.grant.provider_authority_digest.clone(),
            usage_binding: LlmModelUsageBindingV1 {
                agent_id: authority.agent_id,
                tenant_id: authority.tenant_id.0.clone(),
                project_id: authority.project_id.0.clone(),
                work_item_id: authority.work_item_id.0.clone(),
                reservation_id: binding.grant.provider_allowance_id.clone(),
                assignment_id: binding.assignment_id.clone(),
                assignment_version: authority.assignment_version,
                provider: binding.grant.provider.clone(),
                model: binding.grant.model.clone(),
            },
        };
        events.bind_llm_model_reservation(&reservation).unwrap();
        let cursor = api.store.company_event_cursor().unwrap();
        for _ in 0..2 {
            let result = api.prepare_adaptive_model(&binding);
            if exact_digest {
                let prepared = result.unwrap();
                assert!(prepared.working_memory.is_none());
                assert_eq!(
                    serde_json::to_vec(&ModelExecutionContext::Adaptive(Box::new(prepared)))
                        .unwrap(),
                    legacy_bytes
                );
            } else {
                assert_eq!(result.unwrap_err(), "adaptive retained context changed");
            }
        }
        assert_eq!(api.store.company_event_cursor().unwrap(), cursor);
        assert_eq!(
            events.llm_model_reservation(&request_id, &owner).unwrap(),
            Some(reservation)
        );
    }
}
