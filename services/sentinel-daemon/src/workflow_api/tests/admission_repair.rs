//! Fixture-only API regressions; constructed evidence is not live NoIO or model evidence.

use super::super::adaptive_leadership_review::{
    tests::discovery_state, LeadershipAuthority, LeadershipContext,
};
use super::super::budget_window_tests::exhausted_budget_review_fixture;
use super::tests::{admission_repair_fixture, exhausted_epoch_fixture};
use super::*;
use sentinel_workflow::{
    AdaptiveLeadershipAdmissionRepairSourceV1, AdaptiveLeadershipRecoveryBindingV1,
    AdaptiveLeadershipReviewCallV1, AdaptiveRecoveryReleaseV1,
};

fn headers(index: usize) -> HashMap<String, String> {
    HashMap::from([(
        "authorization".into(),
        format!("Bearer test-credential-{index}-{}", "x".repeat(32)),
    )])
}

fn query(context: &LeadershipContext) -> String {
    format!(
        "{ADAPTIVE_REVIEW_EPOCH_PATH}?project_id={}&session_id={}",
        context.binding.grant.project_id, context.binding.grant.session_id
    )
}

fn repair_request_shape(legacy: &serde_json::Value) -> serde_json::Value {
    // Negative wire input only; these digests do not assert positive NoIO evidence.
    let mut request = legacy.clone();
    let object = request.as_object_mut().unwrap();
    object.remove("unknown_effect");
    object.remove("sealed_unknown_proof_digest");
    object.remove("blocked_subject");
    object.insert("schema_version".into(), serde_json::json!(3));
    object.insert(
        "admission_repair".into(),
        serde_json::json!({
            "schema_version": 1,
            "source_digest": "d".repeat(64),
            "disposition_digest": "e".repeat(64),
            "failed_release": legacy["release"],
        }),
    );
    request
}

fn repair_query(source: &AdaptiveLeadershipAdmissionRepairSourceV1) -> String {
    format!(
        "{ADAPTIVE_REVIEW_EPOCH_PATH}?mode=admission_repair&project_id={}&session_id={}",
        source.project.project_id, source.session.grant.session_id,
    )
}

fn assert_repair_source_retained(
    api: &WorkflowApi,
    source: &AdaptiveLeadershipAdmissionRepairSourceV1,
) {
    let tenant = &source.project.tenant_id;
    let session_id = source.session.grant.session_id;
    assert_eq!(
        api.store
            .adaptive_session_for_authority(&source.session.grant.authority)
            .unwrap(),
        Some(source.session.clone()),
    );
    assert_eq!(
        api.store
            .company_project(tenant, &source.project.project_id)
            .unwrap(),
        Some(source.project.clone()),
    );
    assert_eq!(
        api.store
            .adaptive_leadership_recovery_epoch(tenant, session_id)
            .unwrap(),
        Some(source.legacy_epoch.as_ref().clone()),
    );
    let calls = api
        .store
        .adaptive_leadership_review_calls(tenant, session_id)
        .unwrap();
    for prior in &source.calls {
        assert_eq!(
            calls
                .iter()
                .find(|call| call.grant.review_id == prior.grant.review_id),
            Some(prior),
        );
    }
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.grant.schema_version == 3)
            .count(),
        9,
    );
}

fn assert_repair_api_issue_and_prepare(
    temp: &tempfile::TempDir,
    api: &WorkflowApi,
    source: &AdaptiveLeadershipAdmissionRepairSourceV1,
) -> (serde_json::Value, LeadershipContext) {
    let windows = source
        .session
        .continuation
        .as_ref()
        .unwrap()
        .authorizations
        .len();
    assert!((1..=2).contains(&windows));
    assert!(!source.continuation_reviews.is_empty());
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    let response = http(api, "GET", &repair_query(source), &headers(8), &[]);
    assert_eq!(
        response.status,
        200,
        "{}",
        String::from_utf8_lossy(&response.body),
    );
    let draft: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(draft["requires_explicit_submission"], true);
    assert_eq!(draft["model_decision_recorded"], false);
    let request = draft["request"].clone();
    assert_eq!(request["schema_version"], 3);
    assert_eq!(
        request["admission_repair"]["source_digest"],
        source.canonical_digest().unwrap(),
    );
    assert_eq!(
        request["prior_review_history_digest"],
        adaptive_leadership_admission_repair_history_digest(&source.calls).unwrap(),
    );
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
        ),
        before,
    );
    assert_repair_source_retained(api, source);
    let response = http(
        api,
        "POST",
        ADAPTIVE_REVIEW_EPOCH_PATH,
        &headers(8),
        &serde_json::to_vec(&request).unwrap(),
    );
    assert_eq!(
        response.status,
        200,
        "{}",
        String::from_utf8_lossy(&response.body),
    );
    let receipt: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(receipt["replayed"], false);
    assert_eq!(
        receipt["authority"],
        "one_exceptional_admission_repair_review"
    );
    assert_eq!(receipt["review_cap_exception"], true);
    assert_eq!(receipt["exceptional_review_count"], 1);
    assert_eq!(receipt["counters_and_windows_unchanged_at_issuance"], true);
    assert_eq!(receipt["issuance_snapshot"]["ordinary_review_count"], 9);
    assert_eq!(
        receipt["issuance_snapshot"]["root_max_model_calls"],
        source.session.grant.max_model_calls,
    );
    assert_eq!(
        receipt["issuance_snapshot"]["spent_model_calls"],
        source.session.model_calls
    );
    let tenant = &source.project.tenant_id;
    let session_id = source.session.grant.session_id;
    let epoch = api
        .store
        .adaptive_leadership_admission_repair_epoch(tenant, session_id)
        .unwrap()
        .unwrap();
    assert_eq!(epoch.source.as_ref(), source);
    assert_eq!(serde_json::to_value(&epoch.epoch.request).unwrap(), request);
    assert_eq!(epoch.epoch.review_grant.schema_version, 4);
    assert!(!epoch.evidence.attested_review_ids.is_empty());
    assert!(epoch
        .evidence
        .attested_review_ids
        .iter()
        .all(|id| source.qualifying_review_ids.contains(id)));
    assert!(source.continuation_reviews.iter().all(|call| {
        !epoch
            .evidence
            .attested_review_ids
            .contains(&call.grant.review_id)
    }));
    let calls = api
        .store
        .adaptive_leadership_review_calls(tenant, session_id)
        .unwrap();
    assert_eq!(calls.len(), source.calls.len() + 1);
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.grant.schema_version == 4)
            .count(),
        1,
    );
    let call = calls
        .iter()
        .find(|call| call.grant.review_id == epoch.epoch.review_id)
        .unwrap();
    assert!(call.dispatch.is_none());
    assert!(call.decision.is_none());
    assert!(call.continuation.is_none());
    assert!(call.model_response_digest.is_none());
    assert_repair_source_retained(api, source);
    let after = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    assert_eq!(
        after[4], before[4],
        "issuance must not move the adaptive head"
    );
    assert_eq!(
        after[5], before[5],
        "issuance must not append provider events"
    );
    assert_eq!(
        after[6], before[6],
        "issuance must not reserve provider work"
    );
    let context = api
        .prepare_leadership_review(&LeadershipAuthority::from_call(call))
        .unwrap();
    assert_eq!(context.source.source_session, source.session);
    assert_eq!(context.source.source_project, source.project);
    assert_eq!(context.binding.grant.schema_version, 4);
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
        ),
        after,
    );
    (request, context)
}

fn assert_repair_api_replay_does_not_renew(
    temp: &tempfile::TempDir,
    api: &WorkflowApi,
    source: &AdaptiveLeadershipAdmissionRepairSourceV1,
    request: &serde_json::Value,
) {
    let tenant = &source.project.tenant_id;
    let session_id = source.session.grant.session_id;
    let prior = api
        .store
        .adaptive_leadership_admission_repair_epoch(tenant, session_id)
        .unwrap()
        .unwrap();
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    for method in ["GET", "POST", "POST"] {
        let response = http(
            api,
            method,
            &repair_query(source),
            &headers(8),
            &serde_json::to_vec(request).unwrap(),
        );
        assert_eq!(
            response.status,
            200,
            "{}",
            String::from_utf8_lossy(&response.body),
        );
        let receipt: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(receipt["replayed"], true);
        assert_eq!(
            receipt["review_id"],
            serde_json::json!(prior.epoch.review_id)
        );
        assert_eq!(
            receipt["expires_at_unix_ms"],
            prior.epoch.expires_at_unix_ms
        );
    }
    for (field, value) in [
        ("operation_id", serde_json::json!(Uuid::new_v4())),
        ("repair_digest", serde_json::json!("f".repeat(64))),
        ("reason_ref", serde_json::json!("another-repair-request")),
        (
            "expires_at_unix_ms",
            serde_json::json!(prior.epoch.expires_at_unix_ms + 1),
        ),
        (
            "max_additional_model_calls",
            serde_json::json!(prior.epoch.request.max_additional_model_calls + 1),
        ),
        (
            "max_window_ms",
            serde_json::json!(prior.epoch.request.max_window_ms + 1),
        ),
    ] {
        let mut changed = request.clone();
        changed[field] = value;
        let response = http(
            api,
            "POST",
            ADAPTIVE_REVIEW_EPOCH_PATH,
            &headers(8),
            &serde_json::to_vec(&changed).unwrap(),
        );
        assert_eq!(response.status, 409, "changed field={field}");
    }
    assert_eq!(
        api.store
            .adaptive_leadership_admission_repair_epoch(tenant, session_id)
            .unwrap(),
        Some(prior),
    );
    assert_repair_source_retained(api, source);
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
        ),
        before,
    );
}

#[test]
fn admission_repair_http_issues_prepares_and_replays_one_separate_review() {
    let (temp, api, source, guard) = admission_repair_fixture();
    let (request, context) = assert_repair_api_issue_and_prepare(&temp, &api, &source);
    let epoch = api
        .store
        .adaptive_leadership_admission_repair_epoch(
            &source.project.tenant_id,
            source.session.grant.session_id,
        )
        .unwrap()
        .unwrap();
    assert_eq!(epoch.evidence.attested_review_ids.len(), 1);
    assert!(epoch.evidence.attested_review_ids.len() < source.qualifying_review_ids.len());
    assert_repair_api_replay_does_not_renew(&temp, &api, &source, &request);
    assert_claim_selector_bound_to_grant(&temp, &api, &context);
    guard.reject();
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    assert!(api.prepare_leadership_review(&context.binding).is_err());
    assert_repair_api_replay_does_not_renew(&temp, &api, &source, &request);
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite")
        ),
        before,
    );
}

#[test]
fn admission_repair_http_rejects_changed_proof_before_issuance_without_writes() {
    for change in ["invalid-proof", "replaced-serving-binaries"] {
        let (temp, api, source, guard) = admission_repair_fixture();
        let path = repair_query(&source);
        let response = http(&api, "GET", &path, &headers(8), &[]);
        assert_eq!(response.status, 200);
        let draft: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        match change {
            "invalid-proof" => guard.reject(),
            "replaced-serving-binaries" => guard.replace_serving_binaries(),
            _ => unreachable!(),
        }
        let before = discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
        );
        for method in ["GET", "POST"] {
            let response = http(
                &api,
                method,
                &path,
                &headers(8),
                &serde_json::to_vec(&draft["request"]).unwrap(),
            );
            assert_eq!(response.status, 409, "change={change} method={method}");
            let error: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
            assert_eq!(error["code"], "adaptive_recovery_conflict");
            assert_eq!(
                discovery_state(
                    &temp.path().join("company.sqlite"),
                    &temp.path().join("events.sqlite")
                ),
                before,
            );
        }
        assert!(api
            .store
            .adaptive_leadership_admission_repair_epoch(
                &source.project.tenant_id,
                source.session.grant.session_id,
            )
            .unwrap()
            .is_none());
        assert_repair_source_retained(&api, &source);
    }
}

#[test]
fn admission_repair_replaced_proof_blocks_prepare_but_not_immutable_replay() {
    for change in ["invalid-proof", "replaced-serving-binaries"] {
        let (temp, api, source, guard) = admission_repair_fixture();
        let (request, context) = assert_repair_api_issue_and_prepare(&temp, &api, &source);
        let call = api
            .store
            .adaptive_leadership_review_call(
                &source.project.tenant_id,
                context.binding.grant.review_id,
            )
            .unwrap()
            .unwrap();
        let request_id = call.request_id();
        let digest = "c".repeat(64);
        let agent = call.grant.leadership_principal.agent_id.unwrap();
        let events = api.event_store.as_ref().unwrap();
        // Synthetic reservation only; no provider or Gateway is invoked.
        assert!(events
            .reserve_llm_request(&request_id, &digest, &agent.to_string())
            .unwrap());
        let pending = events.get_llm_completion(&request_id).unwrap();
        let dispatch = dispatch_request(&call, &context, &digest);
        match change {
            "invalid-proof" => guard.reject(),
            "replaced-serving-binaries" => guard.replace_serving_binaries(),
            _ => unreachable!(),
        }
        let before = discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
        );
        assert!(
            api.prepare_leadership_review(&context.binding).is_err(),
            "{change}"
        );
        assert_eq!(
            api.subscription_dispatch(&serde_json::to_vec(&dispatch).unwrap())
                .status,
            403
        );
        assert_repair_api_replay_does_not_renew(&temp, &api, &source, &request);
        let call = api
            .store
            .adaptive_leadership_review_call(
                &source.project.tenant_id,
                context.binding.grant.review_id,
            )
            .unwrap()
            .unwrap();
        assert!(call.dispatch.is_none());
        assert!(call.decision.is_none());
        assert!(call.continuation.is_none());
        assert_eq!(events.get_llm_completion(&request_id).unwrap(), pending);
        assert_eq!(
            discovery_state(
                &temp.path().join("company.sqlite"),
                &temp.path().join("events.sqlite")
            ),
            before,
        );
    }
}

#[test]
fn admission_repair_only_subsequent_fixture_decision_creates_bounded_continuation() {
    use super::super::adaptive_leadership_review::tests::persist;
    use super::super::model_execution::{ModelExecutionCompletion, ModelExecutionContext};

    let (temp, api, source, _guard) = admission_repair_fixture();
    let (request, context) = assert_repair_api_issue_and_prepare(&temp, &api, &source);
    let (request_id, digest) = assert_claim_selector_bound_to_grant(&temp, &api, &context);
    assert_repair_source_retained(&api, &source);
    // Synthetic content through the real receipt/decision handler, not a live model result.
    let completion = ModelExecutionCompletion {
        context: ModelExecutionContext::AdaptiveLeadershipReview(Box::new(context.clone())),
        content: serde_json::json!({
            "schema_version": 4,
            "decision": {
                "kind": "continue", "additional_model_calls": 1, "window_ms": 1_000,
                "rationale": "Fixture-only bounded continuation after explicit repair.",
                "evidence_refs": context.source.evidence_refs,
            },
        })
        .to_string(),
        admissible: true,
    };
    persist(&api, &completion, &context, &request_id, &digest, true);
    api.accept_leadership_review(&completion, &context, &request_id, &digest)
        .unwrap();
    let next = api
        .store
        .adaptive_session_for_authority(&source.session.grant.authority)
        .unwrap()
        .unwrap();
    assert_eq!(next.grant, source.session.grant);
    assert_eq!(next.model_calls, source.session.model_calls);
    assert_eq!(next.tool_calls, source.session.tool_calls);
    let old_windows = &source.session.continuation.as_ref().unwrap().authorizations;
    let new_windows = &next.continuation.as_ref().unwrap().authorizations;
    assert_eq!(new_windows.len(), old_windows.len() + 1);
    assert_eq!(&new_windows[..old_windows.len()], old_windows);
    let added = new_windows.last().unwrap();
    assert_eq!(added.review_id, context.binding.grant.review_id);
    assert_eq!(added.additional_model_calls, 1);
    assert_eq!(added.deadline_ms - added.issued_at_ms, 1_000);
    let call = api
        .store
        .adaptive_leadership_review_call(&source.project.tenant_id, context.binding.grant.review_id)
        .unwrap()
        .unwrap();
    assert!(call.decision.is_some());
    assert_eq!(call.continuation.as_ref(), Some(added));
    let after = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    let replay = http(
        &api,
        "POST",
        ADAPTIVE_REVIEW_EPOCH_PATH,
        &headers(8),
        &serde_json::to_vec(&request).unwrap(),
    );
    assert_eq!(replay.status, 200);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&replay.body).unwrap()["replayed"],
        true
    );
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite")
        ),
        after,
    );
}

#[test]
fn admission_repair_source_bootstrap_needs_no_proof_and_exposes_only_bounded_metadata() {
    let (temp, mut api, source, guard) = admission_repair_fixture();
    guard.reject();
    let path = format!(
        "{ADAPTIVE_REVIEW_EPOCH_PATH}?mode=admission_repair_source&project_id={}&session_id={}",
        source.project.project_id, source.session.grant.session_id,
    );
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    let response = http(&api, "GET", &path, &headers(8), &[]);
    assert_eq!(
        response.status,
        200,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    let value: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(value["authority_issued"], false);
    assert_eq!(value["model_decision_recorded"], false);
    assert_eq!(value["session_head_digest"], source.session_head_digest);
    assert_eq!(value["source_digest"], source.canonical_digest().unwrap());
    assert_eq!(
        value["inventory_digest"],
        adaptive_leadership_admission_repair_history_digest(&source.calls).unwrap()
    );
    assert_eq!(
        value["qualifying_review_ids"],
        serde_json::json!(source.qualifying_review_ids)
    );
    assert_eq!(value["accounting"]["ordinary_review_count"], 9);
    assert_eq!(
        value["accounting"]["root_max_model_calls"],
        source.session.grant.max_model_calls
    );
    assert_eq!(
        value["accounting"]["spent_model_calls"],
        source.session.model_calls
    );
    assert_eq!(
        value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "schema_version",
            "purpose",
            "tenant_id",
            "project_id",
            "work_item_id",
            "session_id",
            "project_version",
            "session_version",
            "session_head_digest",
            "source_digest",
            "inventory_digest",
            "qualifying_review_ids",
            "candidates",
            "history",
            "accounting",
            "authority_issued",
            "model_decision_recorded",
        ]),
    );
    let history = value["history"].as_array().unwrap();
    assert_eq!(history.len(), source.calls.len());
    for row in history {
        assert_eq!(row["no_io"], "not_asserted");
        assert_eq!(
            row.as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "review_id",
                "grant_schema_version",
                "session_version",
                "classification",
                "no_io",
                "local_adoption",
            ]),
        );
    }
    for call in &source.continuation_reviews {
        let row = history
            .iter()
            .find(|row| row["review_id"] == serde_json::json!(call.grant.review_id))
            .unwrap();
        assert_eq!(row["classification"], "verified_continuation");
        assert_eq!(row["no_io"], "not_asserted");
    }
    let candidates = value["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), source.qualifying_review_ids.len());
    for candidate in candidates {
        assert_eq!(
            candidate
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "schema_version",
                "purpose",
                "review_id",
                "request_id",
                "authority_digest",
                "context_digest",
                "retired_call_digest",
                "issued_at_unix_ms",
                "expires_at_unix_ms",
                "retired_at_unix_ms",
            ]),
        );
    }
    drop(guard);
    let no_proof = http(&api, "GET", &path, &headers(8), &[]);
    assert_eq!(no_proof.status, 200);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&no_proof.body).unwrap(),
        value
    );
    for actor in 0..8 {
        assert_eq!(http(&api, "GET", &path, &headers(actor), &[]).status, 403);
    }
    assert_eq!(http(&api, "GET", &path, &HashMap::new(), &[]).status, 401);
    for changed_path in [
        path.replace(&source.project.project_id.to_string(), "project-foreign"),
        path.replace(
            &source.session.grant.session_id.to_string(),
            &Uuid::new_v4().to_string(),
        ),
    ] {
        assert!((400..500).contains(&http(&api, "GET", &changed_path, &headers(8), &[]).status));
    }
    bind_foreign_operator(&mut api);
    assert!((400..500).contains(&http(&api, "GET", &path, &headers(8), &[]).status));
    assert_repair_source_retained(&api, &source);
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite")
        ),
        before,
    );
}

fn bind_foreign_operator(api: &mut WorkflowApi) {
    let mut principals = PrincipalAuthenticator {
        by_principal_id: api.principals.by_principal_id.clone(),
        by_credential_digest: api.principals.by_credential_digest.clone(),
    };
    for bound in principals
        .by_principal_id
        .values_mut()
        .chain(principals.by_credential_digest.values_mut())
    {
        if bound.principal.principal_id == "operator" {
            bound.principal.tenant_id = TenantId::parse("tenant-foreign").unwrap();
        }
    }
    api.principals = Arc::new(principals);
    Arc::make_mut(api.authority.as_mut().unwrap()).principals = Arc::clone(&api.principals);
}

fn dispatch_request(
    call: &AdaptiveLeadershipReviewCallV1,
    context: &LeadershipContext,
    digest: &str,
) -> serde_json::Value {
    let grant = &call.grant;
    serde_json::json!({
        "schema_version": 5,
        "allowance_id": call.allowance_id,
        "agent_id": grant.leadership_principal.agent_id.unwrap().0,
        "request_id": call.request_id(),
        "request_digest": digest,
        "context_digest": context.context_digest,
        "provider": grant.provider,
        "model": grant.model,
        "catalog_digest": grant.catalog_digest,
        "subject": {"kind": "adaptive_leadership_review", "review_id": grant.review_id,
            "review_kind": if grant.schema_version == 4 { "admission_repair" } else { "budget_window_exhausted" }},
    })
}

fn assert_claim_selector_bound_to_grant(
    temp: &tempfile::TempDir,
    api: &WorkflowApi,
    context: &LeadershipContext,
) -> (String, String) {
    let grant = &context.binding.grant;
    let expected_selector = match grant.schema_version {
        3 => "budget_window_exhausted",
        4 => "admission_repair",
        schema => panic!("expected actual normal or admission-repair grant, got {schema}"),
    };
    let agent = grant.leadership_principal.agent_id.unwrap();
    let (authoritative, prepared) = api
        .leadership_review_for_dispatch_with_context(agent, grant.review_id)
        .expect("selector test must have an admissible authoritative call");
    assert_eq!(authoritative.grant, *grant);
    assert_eq!(prepared.context_digest, context.context_digest);
    assert!(authoritative.dispatch.is_none());
    let events = api.event_store.as_ref().unwrap();
    let request_id = authoritative.request_id();
    let request_digest = "c".repeat(64);
    // A fixture reservation is not a provider invocation or live NoIO evidence.
    assert!(events
        .reserve_llm_request(&request_id, &request_digest, &agent.to_string())
        .unwrap());
    let pending = events.get_llm_completion(&request_id).unwrap().unwrap();
    assert_eq!(pending.status, "provider_in_flight");
    assert!(pending.payload.is_empty());
    let request = dispatch_request(&authoritative, context, &request_digest);
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    for selector in [
        None,
        Some(serde_json::Value::Null),
        Some(serde_json::json!("budget_window_exhausted")),
        Some(serde_json::json!("admission_repair")),
        Some(serde_json::json!("blocked_continuation")),
        Some(serde_json::json!("unknown_model")),
    ] {
        if selector.as_ref().and_then(serde_json::Value::as_str) == Some(expected_selector) {
            continue;
        }
        let mut changed = request.clone();
        match selector {
            Some(value) => changed["subject"]["review_kind"] = value,
            None => {
                changed["subject"]
                    .as_object_mut()
                    .unwrap()
                    .remove("review_kind");
            }
        }
        let response = api.subscription_dispatch(&serde_json::to_vec(&changed).unwrap());
        assert_eq!(
            response.status, 403,
            "grant={}: {changed}",
            grant.schema_version
        );
        let error: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(error["code"], "subscription_dispatch_denied");
        assert_eq!(
            discovery_state(
                &temp.path().join("company.sqlite"),
                &temp.path().join("events.sqlite"),
            ),
            before
        );
        assert_eq!(
            events.get_llm_completion(&request_id).unwrap(),
            Some(pending.clone())
        );
    }
    let body = serde_json::to_vec(&request).unwrap();
    let accepted = api.subscription_dispatch(&body);
    assert_eq!(
        accepted.status,
        200,
        "{}",
        String::from_utf8_lossy(&accepted.body)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&accepted.body).unwrap();
    assert_eq!(receipt["schema_version"], 5);
    assert_eq!(receipt["allowance_id"], request["allowance_id"]);
    assert_eq!(receipt["request_id"], request["request_id"]);
    assert_eq!(receipt["request_digest"], request["request_digest"]);
    assert_eq!(
        receipt
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "schema_version",
            "allowance_id",
            "request_id",
            "request_digest",
            "deadline_unix_ms"
        ])
    );
    let claimed = api
        .store
        .adaptive_leadership_review_call(&grant.leadership_principal.tenant_id, grant.review_id)
        .unwrap()
        .unwrap();
    assert!(claimed.dispatch.is_some());
    assert!(claimed.decision.is_none());
    assert!(claimed.continuation.is_none());
    let after = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    assert_eq!(api.subscription_dispatch(&body).status, 403);
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
        ),
        after
    );
    assert_eq!(
        events.get_llm_completion(&request_id).unwrap(),
        Some(pending)
    );
    (request_id, request_digest)
}

#[test]
fn admission_repair_selector_cannot_claim_normal_schema3_authority_or_create_gateway_effect() {
    let (temp, api, session) = exhausted_budget_review_fixture();
    let extension_path = format!(
        "{ADAPTIVE_BUDGET_REVIEW_EXTENSION_PATH}?project_id={}&session_id={}",
        session.grant.authority.project_id, session.grant.session_id,
    );
    let draft = http(&api, "GET", &extension_path, &headers(8), &[]);
    assert_eq!(
        draft.status,
        200,
        "{}",
        String::from_utf8_lossy(&draft.body)
    );
    let value: serde_json::Value = serde_json::from_slice(&draft.body).unwrap();
    let issued = http(
        &api,
        "POST",
        ADAPTIVE_BUDGET_REVIEW_EXTENSION_PATH,
        &headers(8),
        &serde_json::to_vec(&value["request"]).unwrap(),
    );
    assert_eq!(
        issued.status,
        200,
        "{}",
        String::from_utf8_lossy(&issued.body)
    );
    let project = api
        .store
        .company_project(
            &session.grant.authority.tenant_id,
            &session.grant.authority.project_id,
        )
        .unwrap()
        .unwrap();
    {
        let _fence = api.mutation_fence.write().unwrap();
        assert!(api.reconcile_adaptive_leadership_reviews(&project).unwrap());
    }
    let call = api
        .store
        .adaptive_leadership_review_calls(
            &session.grant.authority.tenant_id,
            session.grant.session_id,
        )
        .unwrap()
        .into_iter()
        .find(|call| {
            call.grant.schema_version == 3
                && call.retired_at_unix_ms.is_none()
                && call.decision.is_none()
                && call.dispatch.is_none()
        })
        .expect("normal extension must produce an undispatched schema-3 review");
    let context = api
        .prepare_leadership_review(&LeadershipAuthority::from_call(&call))
        .unwrap();
    assert_claim_selector_bound_to_grant(&temp, &api, &context);
}

fn http(
    api: &WorkflowApi,
    method: &str,
    path: &str,
    credentials: &HashMap<String, String>,
    body: &[u8],
) -> WorkflowHttpResponse {
    api.handle(method, path, credentials, body)
        .expect("existing recovery epoch route must remain registered")
}

#[test]
fn admission_repair_legacy_release_and_binding_are_byte_compatible() {
    let digest = "d".repeat(64);
    let release = format!(
        r#"{{"schema_version":1,"source_git_sha":"{}","release_manifest_digest":"{digest}","gateway_binary_digest":"{digest}"}}"#,
        "a".repeat(40),
    );
    let decoded: AdaptiveRecoveryReleaseV1 = serde_json::from_str(&release).unwrap();
    assert_eq!(serde_json::to_vec(&decoded).unwrap(), release.as_bytes());
    let binding = format!(
        r#"{{"schema_version":1,"epoch_key":"recovery-{digest}","epoch_digest":"{digest}","review_id":"01991c34-e03c-70c2-b97e-0591f4be2311","max_window_ms":120000,"max_additional_model_calls":2}}"#,
    );
    let decoded: AdaptiveLeadershipRecoveryBindingV1 = serde_json::from_str(&binding).unwrap();
    assert_eq!(serde_json::to_vec(&decoded).unwrap(), binding.as_bytes());
}

#[test]
fn admission_repair_legacy_request_schemas_are_byte_compatible() {
    let digest = "d".repeat(64);
    let release = format!(
        r#"{{"schema_version":1,"source_git_sha":"{}","release_manifest_digest":"{digest}","gateway_binary_digest":"{digest}"}}"#,
        "a".repeat(40),
    );
    for schema in [1, 2] {
        let subject = if schema == 1 {
            format!(
                r#""unknown_effect":{{"id":"01991c34-e03c-70c2-b97e-0591f4be2312","request_digest":"{digest}"}},"sealed_unknown_proof_digest":"{digest}","#,
            )
        } else {
            format!(
                r#""blocked_subject":{{"reason_code":"dependency_unavailable","model_response_digest":"{digest}"}},"#,
            )
        };
        let bytes = format!(
            concat!(
                r#"{{"schema_version":{schema},"operation_id":"01991c34-e03c-70c2-b97e-0591f4be2313","tenant_id":"tenant-m0","project_id":"project-m0","work_item_id":"work-m0","session_id":"01991c34-e03c-70c2-b97e-0591f4be2314","expected_project_version":1,"expected_session_version":3,"session_head_digest":"{digest}","session_digest":"{digest}","project_digest":"{digest}","#,
                r#"{subject}"prior_review_history_digest":"{digest}","repair_digest":"{digest}","release":{release},"reason_ref":"legacy-wire-fixture","expires_at_unix_ms":2000000,"max_additional_model_calls":2,"max_window_ms":120000}}"#,
            ),
            schema = schema,
            digest = digest,
            subject = subject,
            release = release,
        );
        let decoded: AdaptiveLeadershipRecoveryRequestV1 = serde_json::from_str(&bytes).unwrap();
        assert_eq!(decoded.schema_version, schema);
        assert_eq!(serde_json::to_vec(&decoded).unwrap(), bytes.as_bytes());
    }
}

#[test]
fn admission_repair_route_rejects_customer_and_employee_credentials_without_writes() {
    for unknown in [false, true] {
        let (temp, _release, api, context) = exhausted_epoch_fixture(unknown);
        let path = query(&context);
        let response = http(&api, "GET", &path, &headers(8), &[]);
        assert_eq!(
            response.status,
            200,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        let draft: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        let before = discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
        );
        for (selected_path, request) in [
            (path.clone(), draft["request"].clone()),
            (
                format!("{path}&mode=admission_repair"),
                repair_request_shape(&draft["request"]),
            ),
        ] {
            let body = serde_json::to_vec(&request).unwrap();
            for actor in 0..8 {
                assert_eq!(
                    http(&api, "GET", &selected_path, &headers(actor), &[]).status,
                    403,
                    "actor={actor} path={selected_path}"
                );
                assert_eq!(
                    http(
                        &api,
                        "POST",
                        ADAPTIVE_REVIEW_EPOCH_PATH,
                        &headers(actor),
                        &body,
                    )
                    .status,
                    403,
                    "actor={actor} path={selected_path}",
                );
            }
            for method in ["GET", "POST"] {
                assert_eq!(
                    http(&api, method, &selected_path, &HashMap::new(), &body).status,
                    401
                );
            }
        }
        assert_eq!(
            discovery_state(
                &temp.path().join("company.sqlite"),
                &temp.path().join("events.sqlite"),
            ),
            before,
        );
        assert!(api
            .store
            .adaptive_leadership_recovery_epoch(
                &context.binding.grant.assignee_authority.tenant_id,
                context.binding.grant.session_id,
            )
            .unwrap()
            .is_none());
    }
}

#[test]
fn admission_repair_route_rejects_registered_foreign_operator_without_writes() {
    let (temp, _release, mut api, context) = exhausted_epoch_fixture(false);
    let path = query(&context);
    let response = http(&api, "GET", &path, &headers(8), &[]);
    assert_eq!(
        response.status,
        200,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    let draft: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    bind_foreign_operator(&mut api);
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    for (selected_path, request) in [
        (path.clone(), draft["request"].clone()),
        (
            format!("{path}&mode=admission_repair"),
            repair_request_shape(&draft["request"]),
        ),
    ] {
        let body = serde_json::to_vec(&request).unwrap();
        for method in ["GET", "POST"] {
            let response = http(&api, method, &selected_path, &headers(8), &body);
            assert!(
                (400..500).contains(&response.status),
                "{method} path={selected_path}: {}",
                response.status
            );
        }
    }
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
        ),
        before,
    );
}

#[test]
fn admission_repair_route_rejects_legacy_schema_selection_without_writes() {
    let (temp, _release, api, context) = exhausted_epoch_fixture(false);
    let path = query(&context);
    let response = http(&api, "GET", &path, &headers(8), &[]);
    assert_eq!(response.status, 200);
    let draft: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    let before = discovery_state(
        &temp.path().join("company.sqlite"),
        &temp.path().join("events.sqlite"),
    );
    for schema in [1, 2] {
        let mut request = repair_request_shape(&draft["request"]);
        request["schema_version"] = serde_json::json!(schema);
        let response = http(
            &api,
            "POST",
            ADAPTIVE_REVIEW_EPOCH_PATH,
            &headers(8),
            &serde_json::to_vec(&request).unwrap(),
        );
        assert_eq!(response.status, 400, "schema={schema}");
        let error: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(error["code"], "invalid_input");
    }
    assert_eq!(
        discovery_state(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
        ),
        before,
    );
    assert!(api
        .store
        .adaptive_leadership_admission_repair_epoch(
            &context.binding.grant.assignee_authority.tenant_id,
            context.binding.grant.session_id,
        )
        .unwrap()
        .is_none());
}
