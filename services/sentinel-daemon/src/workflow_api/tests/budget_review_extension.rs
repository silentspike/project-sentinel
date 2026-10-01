//! HTTP issuance regressions using real stores, not live model-decision evidence.

use super::adaptive_leadership_review::tests::discovery_state;
use super::budget_window_tests::exhausted_budget_review_fixture;
use super::*;
use std::collections::BTreeSet;

const PATH: &str = ADAPTIVE_BUDGET_REVIEW_EXTENSION_PATH;

fn credentials(index: usize) -> HashMap<String, String> {
    HashMap::from([(
        "authorization".into(),
        format!("Bearer test-credential-{index}-{}", "x".repeat(32)),
    )])
}

fn query(session: &AdaptiveSessionV1) -> String {
    format!(
        "{PATH}?project_id={}&session_id={}",
        session.grant.authority.project_id, session.grant.session_id
    )
}

fn call(
    api: &WorkflowApi,
    method: &str,
    path: &str,
    headers: &HashMap<String, String>,
    body: &[u8],
) -> WorkflowHttpResponse {
    api.handle(method, path, headers, body)
        .expect("budget extension route must be registered")
}

fn draft(api: &WorkflowApi, session: &AdaptiveSessionV1) -> serde_json::Value {
    let response = call(api, "GET", &query(session), &credentials(8), &[]);
    assert_eq!(
        response.status,
        200,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    let value: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(value["requires_explicit_submission"], true);
    assert_eq!(value["model_decision_recorded"], false);
    assert!(value["request"].is_object());
    value["request"].clone()
}

fn submit(api: &WorkflowApi, request: &serde_json::Value) -> WorkflowHttpResponse {
    call(
        api,
        "POST",
        PATH,
        &credentials(8),
        &serde_json::to_vec(request).unwrap(),
    )
}

#[test]
fn budget_extension_http_authentication_draft_and_tenant_binding_are_read_only() {
    let (temp, mut api, session) = exhausted_budget_review_fixture();
    let path = temp.path().join("company.sqlite");
    let events = temp.path().join("events.sqlite");
    let before = discovery_state(&path, &events);
    for method in ["GET", "POST"] {
        assert_eq!(
            call(&api, method, &query(&session), &HashMap::new(), b"{}").status,
            401
        );
        // Includes both employee leadership roles, neither of which is an Operator.
        for index in [0, 1, 2, 3, 5, 7] {
            assert_eq!(
                call(&api, method, &query(&session), &credentials(index), b"{}").status,
                403
            );
        }
    }
    assert_eq!(call(&api, "DELETE", PATH, &credentials(8), &[]).status, 405);
    for missing in [
        PATH.to_owned(),
        format!("{PATH}?project_id={}", session.grant.authority.project_id),
        format!("{PATH}?session_id={}", session.grant.session_id),
        format!(
            "{PATH}?project_id={}&session_id={}",
            session.grant.authority.project_id,
            Uuid::nil()
        ),
        format!("{}&expected_session_version=0", query(&session)),
    ] {
        assert_eq!(
            call(&api, "GET", &missing, &credentials(8), &[]).status,
            400
        );
    }
    assert_eq!(
        call(
            &api,
            "GET",
            &format!(
                "{}&expected_session_version={}",
                query(&session),
                session.version + 1
            ),
            &credentials(8),
            &[],
        )
        .status,
        409
    );
    let request = draft(&api, &session);
    let fields: BTreeSet<_> = request
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        fields,
        BTreeSet::from([
            "schema_version",
            "operation_id",
            "tenant_id",
            "project_id",
            "session_id",
            "expected_session_version",
            "budget_limit_receipt_digest",
            "source_digest",
            "base_global_review_count",
            "base_head_review_count",
            "additional_reviews",
            "reason_ref",
            "expires_at_unix_ms",
        ])
    );
    assert_eq!(request["schema_version"], 1);
    assert_eq!(
        request["tenant_id"],
        serde_json::json!(session.grant.authority.tenant_id)
    );
    assert_eq!(
        request["project_id"],
        serde_json::json!(session.grant.authority.project_id)
    );
    assert_eq!(
        request["session_id"],
        serde_json::json!(session.grant.session_id)
    );
    assert_eq!(request["expected_session_version"], session.version);
    assert_eq!(request["base_global_review_count"], 3);
    assert_eq!(request["base_head_review_count"], 3);
    assert!((1..=3).contains(&request["additional_reviews"].as_u64().unwrap()));
    let expires = request["expires_at_unix_ms"].as_u64().unwrap();
    assert!(expires > now_unix_ms());
    assert!(expires <= now_unix_ms() + 86_400_000);
    for field in ["budget_limit_receipt_digest", "source_digest"] {
        assert_eq!(request[field].as_str().unwrap().len(), 64);
    }
    let mut changed = request.clone();
    changed["tenant_id"] = serde_json::json!("tenant-other");
    assert_eq!(submit(&api, &changed).status, 403);
    for field in [
        "source_digest",
        "expected_session_version",
        "budget_limit_receipt_digest",
        "project_id",
    ] {
        let mut changed = request.clone();
        changed[field] = match field {
            "expected_session_version" => serde_json::json!(session.version + 1),
            "project_id" => serde_json::json!("project-other"),
            _ => serde_json::json!("f".repeat(64)),
        };
        let response = submit(&api, &changed);
        assert!(
            (400..600).contains(&response.status),
            "{field}: {}",
            response.status
        );
    }
    assert_eq!(call(&api, "POST", PATH, &credentials(8), b"{}").status, 400);
    // Change the server registration, not a caller-supplied principal DTO.
    for role in [CompanyRoleV1::Developer, CompanyRoleV1::ProjectManager] {
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
                bound.principal.role = role;
            }
        }
        api.principals = Arc::new(principals);
        Arc::make_mut(api.authority.as_mut().unwrap()).principals = Arc::clone(&api.principals);
        if role == CompanyRoleV1::ProjectManager {
            draft(&api, &session);
        } else {
            for method in ["GET", "POST"] {
                assert_eq!(
                    call(&api, method, &query(&session), &credentials(8), b"{}").status,
                    403
                );
            }
        }
    }
    assert_eq!(discovery_state(&path, &events), before);
    assert!(api
        .store
        .budget_review_extension(
            &session.grant.authority.tenant_id,
            session.grant.session_id,
            session.version,
        )
        .unwrap()
        .is_none());
}

#[test]
fn budget_extension_http_issuance_and_exact_replay_never_record_a_model_decision_or_refill() {
    let (temp, api, session) = exhausted_budget_review_fixture();
    let path = temp.path().join("company.sqlite");
    let events = temp.path().join("events.sqlite");
    let tenant = &session.grant.authority.tenant_id;
    let session_id = session.grant.session_id;
    let project = api
        .store
        .company_project(tenant, &session.grant.authority.project_id)
        .unwrap()
        .unwrap();
    let reviews = api
        .store
        .adaptive_leadership_review_calls(tenant, session_id)
        .unwrap();
    assert_eq!(reviews.len(), 3);
    assert!(reviews.iter().all(|review| {
        review.grant.schema_version == 3
            && review.retired_at_unix_ms.is_some()
            && review.decision.is_none()
    }));
    assert!(api
        .store
        .adaptive_budget_window_limit_recorded(tenant, session_id, session.version)
        .unwrap());
    let before = discovery_state(&path, &events);
    let request = draft(&api, &session);
    assert_eq!(discovery_state(&path, &events), before);
    let body = serde_json::to_vec(&request).unwrap();
    let response = submit(&api, &request);
    assert_eq!(
        response.status,
        200,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    let mut response: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(response["receipt_kind"], "immutable_issuance");
    assert_eq!(response["authority"], "bounded_normal_leadership_reviews");
    assert_eq!(response["model_decision_recorded"], false);
    assert_eq!(response["replayed"], false);
    let receipt = api
        .store
        .budget_review_extension(tenant, session_id, session.version)
        .unwrap()
        .unwrap();
    let receipt_value = serde_json::to_value(&receipt).unwrap();
    let additional = usize::try_from(request["additional_reviews"].as_u64().unwrap()).unwrap();
    let limits = api
        .store
        .budget_review_limits(tenant, session_id, session.version, now_unix_ms())
        .unwrap();
    assert_eq!(
        limits,
        (
            3 + additional,
            3 + additional,
            Some(request["expires_at_unix_ms"].as_u64().unwrap())
        )
    );
    assert_eq!(
        api.store
            .adaptive_session(session_id, &session.grant.authority)
            .unwrap(),
        Some(session.clone())
    );
    assert_eq!(
        api.store
            .company_project(tenant, &session.grant.authority.project_id)
            .unwrap(),
        Some(project)
    );
    assert_eq!(
        api.store
            .adaptive_leadership_review_calls(tenant, session_id)
            .unwrap(),
        reviews
    );
    assert!(api
        .store
        .adaptive_budget_window_limit_recorded(tenant, session_id, session.version)
        .unwrap());
    let issued = discovery_state(&path, &events);
    // Issuance may append its receipt/audit, but may not rewrite any prior row.
    for (old_table, new_table) in before.iter().zip(&issued) {
        assert!(old_table.iter().all(|row| new_table.contains(row)));
    }
    assert_eq!(&issued[3..], &before[3..]);
    let replay = call(&api, "POST", PATH, &credentials(8), &body);
    assert_eq!(replay.status, 200);
    response["replayed"] = serde_json::json!(true);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&replay.body).unwrap(),
        response
    );
    let get = call(&api, "GET", &query(&session), &credentials(8), &[]);
    assert_eq!(get.status, 200);
    let get: serde_json::Value = serde_json::from_slice(&get.body).unwrap();
    assert_eq!(get, response);
    let explicit = call(
        &api,
        "GET",
        &format!(
            "{}&expected_session_version={}",
            query(&session),
            session.version
        ),
        &credentials(8),
        &[],
    );
    assert_eq!(explicit.status, 200);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&explicit.body).unwrap(),
        response
    );
    let mut refill = request.clone();
    refill["operation_id"] = serde_json::json!(Uuid::new_v4());
    let denied = submit(&api, &refill);
    assert!((400..500).contains(&denied.status));
    let retained = api
        .store
        .budget_review_extension(tenant, session_id, session.version)
        .unwrap()
        .unwrap();
    assert_eq!(serde_json::to_value(&retained).unwrap(), receipt_value);
    assert_eq!(
        api.store
            .budget_review_limits(tenant, session_id, session.version, now_unix_ms())
            .unwrap(),
        limits
    );
    assert_eq!(discovery_state(&path, &events), issued);
}

#[test]
fn budget_extension_http_feature_and_poisoned_fence_deny_without_issuance() {
    let (temp, mut api, session) = exhausted_budget_review_fixture();
    let path = temp.path().join("company.sqlite");
    let events = temp.path().join("events.sqlite");
    let request = serde_json::to_vec(&draft(&api, &session)).unwrap();
    let before = discovery_state(&path, &events);
    api.model_work_enabled = false;
    for (method, body) in [("GET", &[][..]), ("POST", request.as_slice())] {
        assert_eq!(
            call(&api, method, &query(&session), &credentials(8), body).status,
            503
        );
    }
    api.model_work_enabled = true;
    let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _fence = api.mutation_fence.write().unwrap();
        panic!("fixture recovery fence poison");
    }));
    assert!(poisoned.is_err());
    for (method, body) in [("GET", &[][..]), ("POST", request.as_slice())] {
        assert_eq!(
            call(&api, method, &query(&session), &credentials(8), body).status,
            503
        );
    }
    assert_eq!(discovery_state(&path, &events), before);
}
