//! HTTP issuance regressions using real stores, not live model-decision evidence.

use super::adaptive_leadership_review::tests::discovery_state;
use super::budget_window_tests::exhausted_budget_review_fixture;
use super::*;
use sentinel_workflow::AdaptiveBudgetReviewExtensionRequestV1;
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

fn successor_draft(api: &WorkflowApi, session: &AdaptiveSessionV1) -> serde_json::Value {
    let response = call(
        api,
        "GET",
        &format!("{}&successor=true", query(session)),
        &credentials(8),
        &[],
    );
    assert_eq!(
        response.status,
        200,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    let value: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(value["requires_explicit_submission"], true);
    assert_eq!(value["model_decision_recorded"], false);
    assert_eq!(value["request"]["schema_version"], 2);
    value["request"].clone()
}

fn operator(api: &WorkflowApi) -> AuthenticatedCompanyPrincipalV1 {
    api.principals
        .by_principal_id
        .get("operator")
        .unwrap()
        .principal
        .clone()
}

fn consume_reviews(
    api: &WorkflowApi,
    session: &AdaptiveSessionV1,
    count: usize,
    retire_last: bool,
) {
    let tenant = &session.grant.authority.tenant_id;
    let mut last_updated = 0;
    for index in 0..count {
        let calls = api
            .store
            .adaptive_leadership_review_calls(tenant, session.grant.session_id)
            .unwrap();
        let template = calls
            .iter()
            .find(|call| call.grant.schema_version == 3)
            .unwrap();
        let issued = now_unix_ms().max(
            calls
                .iter()
                .map(|call| call.updated_at_unix_ms)
                .max()
                .unwrap()
                + 1,
        );
        let mut grant = template.grant.clone();
        let mut context = template.context.clone();
        context
            .evidence_refs
            .push(format!("budget-successor-test:{}", calls.len()));
        grant.evidence_fingerprint = sentinel_workflow::adaptive_leadership_evidence_fingerprint(
            &context.tool_catalog,
            &context.evidence_refs,
        )
        .unwrap();
        grant.review_id = sentinel_workflow::adaptive_leadership_review_id(
            grant.session_id,
            grant.expected_session_version,
            &grant.evidence_fingerprint,
        )
        .unwrap();
        let Some(sentinel_workflow::AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted {
            budget,
        }) = &mut grant.subject
        else {
            panic!("expected normal review source");
        };
        budget.observed_at_ms = issued;
        budget.deadline_expired = issued >= session.active_deadline_ms();
        budget.model_calls_exhausted = session.model_calls >= session.active_model_ceiling();
        grant.expires_at_unix_ms = issued + 1;
        let call = api
            .store
            .authorize_adaptive_leadership_review_call(
                &grant.leadership_principal,
                Uuid::new_v4(),
                &format!("successor-review-{}", calls.len()),
                &grant,
                &context,
                issued,
            )
            .unwrap();
        if retire_last || index + 1 < count {
            api.store
                .expire_adaptive_leadership_review_call(
                    &grant.leadership_principal,
                    grant.review_id,
                    call.version,
                    grant.expires_at_unix_ms,
                )
                .unwrap();
        }
        last_updated = grant.expires_at_unix_ms;
    }
    let wait = last_updated.saturating_sub(now_unix_ms());
    assert!(
        wait <= 1_000,
        "fixture must not wait for a normal review window"
    );
    if wait > 0 {
        std::thread::sleep(std::time::Duration::from_millis(wait));
    }
}

#[test]
fn budget_successor_http_is_append_only_reopens_and_replays_each_original_without_refill() {
    let (temp, api, session) = exhausted_budget_review_fixture();
    let path = temp.path().join("company.sqlite");
    let events = temp.path().join("events.sqlite");
    let tenant = &session.grant.authority.tenant_id;
    let sid = session.grant.session_id;
    let original = draft(&api, &session);
    assert!(original.get("prior_operation_id").is_none());
    assert_eq!(submit(&api, &original).status, 200);
    let original_receipt = api
        .store
        .budget_review_extension(tenant, sid, session.version)
        .unwrap()
        .unwrap();
    let original_value = serde_json::to_value(&original_receipt).unwrap();
    assert!(original_value.get("prior_receipt_digest").is_none());
    consume_reviews(&api, &session, 3, true);
    let before = discovery_state(&path, &events);
    let parent_expiry = original["expires_at_unix_ms"].as_u64().unwrap();
    let after_expiry = api
        .store
        .budget_review_extension_successor_draft(
            &operator(&api),
            &session.grant.authority.project_id,
            sid,
            Uuid::new_v4(),
            3,
            "consumed-expired-parent",
            parent_expiry + 3_600_000,
            parent_expiry,
        )
        .unwrap();
    assert_eq!(after_expiry.base_global_review_count, 6);
    assert_eq!(
        after_expiry.prior_operation_id,
        Some(original_receipt.request.operation_id)
    );
    let successor = successor_draft(&api, &session);
    assert_eq!(successor["prior_operation_id"], original["operation_id"]);
    assert_eq!(successor["base_global_review_count"], 6);
    assert_eq!(successor["base_head_review_count"], 6);
    assert_eq!(discovery_state(&path, &events), before);
    let response = submit(&api, &successor);
    assert_eq!(
        response.status,
        200,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    let response: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(response["schema_version"], 2);
    assert_eq!(response["prior_operation_id"], original["operation_id"]);
    assert_eq!(response["replayed"], false);
    assert_eq!(response["model_decision_recorded"], false);
    let successor_receipt = api
        .store
        .budget_review_extension(tenant, sid, session.version)
        .unwrap()
        .unwrap();
    let successor_value = serde_json::to_value(&successor_receipt).unwrap();
    assert_eq!(
        successor_value["prior_receipt_digest"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    let issued = discovery_state(&path, &events);
    for (old_table, new_table) in before.iter().zip(&issued) {
        assert!(old_table.iter().all(|row| new_table.contains(row)));
    }
    assert_eq!(&issued[3..], &before[3..]);
    let reopened = sentinel_workflow::WorkflowStore::open(&path).unwrap();
    let original_request: AdaptiveBudgetReviewExtensionRequestV1 =
        serde_json::from_value(original.clone()).unwrap();
    let successor_request: AdaptiveBudgetReviewExtensionRequestV1 =
        serde_json::from_value(successor.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(
            reopened
                .budget_review_extension(tenant, sid, session.version)
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        successor_value
    );
    let expiry = successor_request.expires_at_unix_ms;
    for now in [0, expiry + 1] {
        let (replayed, receipt) = reopened
            .authorize_budget_review_extension(&operator(&api), &original_request, now)
            .unwrap();
        assert!(replayed);
        assert_eq!(serde_json::to_value(receipt).unwrap(), original_value);
        let (replayed, receipt) = reopened
            .authorize_budget_review_extension(&operator(&api), &successor_request, now)
            .unwrap();
        assert!(replayed);
        assert_eq!(serde_json::to_value(receipt).unwrap(), successor_value);
    }
    assert_eq!(
        reopened
            .budget_review_limits(tenant, sid, session.version, now_unix_ms())
            .unwrap(),
        (9, 9, Some(expiry))
    );
    assert_eq!(
        reopened
            .budget_review_limits(tenant, sid, session.version, expiry)
            .unwrap(),
        (3, 3, None)
    );
    assert_eq!(discovery_state(&path, &events), issued);
    let replay = submit(&api, &original);
    assert_eq!(replay.status, 200);
    let replay: serde_json::Value = serde_json::from_slice(&replay.body).unwrap();
    assert_eq!(replay["operation_id"], original["operation_id"]);
    assert_eq!(replay["schema_version"], 1);
    assert_eq!(replay["replayed"], true);
    for selection in [
        query(&session),
        format!("{}&successor=true", query(&session)),
    ] {
        let get = call(&api, "GET", &selection, &credentials(8), &[]);
        assert_eq!(get.status, 200);
        let get: serde_json::Value = serde_json::from_slice(&get.body).unwrap();
        assert_eq!(get["operation_id"], successor["operation_id"]);
        assert!(get.get("request").is_none());
    }
    consume_reviews(&api, &session, 3, true);
    let consumed = discovery_state(&path, &events);
    let mut refill = successor.clone();
    refill["operation_id"] = serde_json::json!(Uuid::new_v4());
    refill["prior_operation_id"] = successor["operation_id"].clone();
    assert_eq!(submit(&api, &refill).status, 409);
    let mut replace_original = original.clone();
    replace_original["operation_id"] = serde_json::json!(Uuid::new_v4());
    assert_eq!(submit(&api, &replace_original).status, 409);
    assert!(reopened
        .budget_review_extension_successor_draft(
            &operator(&api),
            &session.grant.authority.project_id,
            sid,
            Uuid::new_v4(),
            3,
            "no-third-issuance",
            now_unix_ms() + 3_600_000,
            now_unix_ms()
        )
        .is_err());
    assert_eq!(
        api.store
            .adaptive_session(sid, &session.grant.authority)
            .unwrap(),
        Some(session.clone())
    );
    let calls = api
        .store
        .adaptive_leadership_review_calls(tenant, sid)
        .unwrap();
    assert_eq!(calls.len(), 9);
    assert!(calls
        .iter()
        .all(|call| call.retired_at_unix_ms.is_some() && call.decision.is_none()));
    assert!(api
        .store
        .adaptive_budget_window_limit_recorded(tenant, sid, session.version)
        .unwrap());
    assert_eq!(discovery_state(&path, &events), consumed);
    let project = api
        .store
        .company_project(tenant, &session.grant.authority.project_id)
        .unwrap()
        .unwrap();
    api.store
        .apply_company_command(
            &calls[0].grant.leadership_principal,
            Uuid::new_v4(),
            &CompanyWorkflowCommandV1::RecordDecision {
                project_id: project.project_id.clone(),
                expected_version: project.version,
                work_item_id: None,
                choice_ref: "changed-project-source-head".into(),
                rationale_ref: "immutable-replay-is-not-fresh-authority".into(),
            },
            now_unix_ms(),
        )
        .unwrap();
    assert!(
        api.store
            .company_project(tenant, &project.project_id)
            .unwrap()
            .unwrap()
            .version
            > project.version
    );
    let changed_source = discovery_state(&path, &events);
    for request in [&original, &successor] {
        let replay = submit(&api, request);
        assert_eq!(replay.status, 200);
        let replay: serde_json::Value = serde_json::from_slice(&replay.body).unwrap();
        assert_eq!(replay["operation_id"], request["operation_id"]);
        assert_eq!(replay["schema_version"], request["schema_version"]);
        assert_eq!(replay["replayed"], true);
        assert_eq!(replay["model_decision_recorded"], false);
    }
    assert_eq!(
        api.store
            .budget_review_limits(tenant, sid, session.version, now_unix_ms())
            .unwrap(),
        (3, 3, None)
    );
    assert_eq!(
        api.store
            .adaptive_session(sid, &session.grant.authority)
            .unwrap(),
        Some(session.clone())
    );
    assert_eq!(
        api.store
            .adaptive_leadership_review_calls(tenant, sid)
            .unwrap(),
        calls
    );
    assert_eq!(discovery_state(&path, &events), changed_source);
}

#[test]
fn budget_successor_event_append_failure_rolls_back_entity_and_event_together() {
    let (temp, api, session) = exhausted_budget_review_fixture();
    let original = draft(&api, &session);
    assert_eq!(submit(&api, &original).status, 200);
    consume_reviews(&api, &session, 3, true);
    let successor = successor_draft(&api, &session);
    let path = temp.path().join("company.sqlite");
    let events = temp.path().join("events.sqlite");
    let connection = sentinel_limbo::rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER reject_successor_audit BEFORE INSERT ON company_events
         WHEN NEW.event_type='adaptive_budget_review_extension_authorized'
         BEGIN SELECT RAISE(ABORT, 'successor audit fixture failure'); END;",
        )
        .unwrap();
    let before = discovery_state(&path, &events);
    assert!(submit(&api, &successor).status >= 400);
    assert_eq!(discovery_state(&path, &events), before);
    let retained = api
        .store
        .budget_review_extension(
            &session.grant.authority.tenant_id,
            session.grant.session_id,
            session.version,
        )
        .unwrap()
        .unwrap();
    assert_eq!(serde_json::to_value(retained.request).unwrap(), original);
    connection
        .execute_batch("DROP TRIGGER reject_successor_audit")
        .unwrap();
    assert_eq!(submit(&api, &successor).status, 200);
    assert_eq!(
        api.store
            .budget_review_extension(
                &session.grant.authority.tenant_id,
                session.grant.session_id,
                session.version
            )
            .unwrap()
            .unwrap()
            .request
            .schema_version,
        2
    );
}

#[test]
fn budget_successor_missing_wrong_parent_nonexhausted_and_nonterminal_are_read_only() {
    let (temp, api, session) = exhausted_budget_review_fixture();
    let path = temp.path().join("company.sqlite");
    let events = temp.path().join("events.sqlite");
    let successor_query = format!("{}&successor=true", query(&session));
    let original = draft(&api, &session);
    let before = discovery_state(&path, &events);
    assert_eq!(
        call(&api, "GET", &successor_query, &credentials(8), &[]).status,
        404
    );
    let mut missing = original.clone();
    missing["schema_version"] = serde_json::json!(2);
    missing["operation_id"] = serde_json::json!(Uuid::new_v4());
    missing["prior_operation_id"] = serde_json::json!(Uuid::new_v4());
    assert_eq!(submit(&api, &missing).status, 404);
    assert_eq!(
        call(
            &api,
            "GET",
            &format!("{}&successor=invalid", query(&session)),
            &credentials(8),
            &[]
        )
        .status,
        400
    );
    assert_eq!(discovery_state(&path, &events), before);
    assert_eq!(submit(&api, &original).status, 200);
    let issued = discovery_state(&path, &events);
    assert_eq!(
        call(&api, "GET", &successor_query, &credentials(8), &[]).status,
        422
    );
    missing["prior_operation_id"] = original["operation_id"].clone();
    assert_eq!(submit(&api, &missing).status, 422);
    let expiry = original["expires_at_unix_ms"].as_u64().unwrap();
    assert!(api
        .store
        .budget_review_extension_successor_draft(
            &operator(&api),
            &session.grant.authority.project_id,
            session.grant.session_id,
            Uuid::new_v4(),
            3,
            "expired-unused-parent",
            expiry + 3_600_000,
            expiry
        )
        .is_err());
    assert_eq!(discovery_state(&path, &events), issued);
    consume_reviews(&api, &session, 3, false);
    let active = discovery_state(&path, &events);
    assert_eq!(
        call(&api, "GET", &successor_query, &credentials(8), &[]).status,
        422
    );
    assert_eq!(submit(&api, &missing).status, 422);
    assert_eq!(discovery_state(&path, &events), active);
    let active_call = api
        .store
        .adaptive_leadership_review_calls(
            &session.grant.authority.tenant_id,
            session.grant.session_id,
        )
        .unwrap()
        .into_iter()
        .find(|call| call.retired_at_unix_ms.is_none())
        .unwrap();
    api.store
        .expire_adaptive_leadership_review_call(
            &active_call.grant.leadership_principal,
            active_call.grant.review_id,
            active_call.version,
            now_unix_ms(),
        )
        .unwrap();
    let request = successor_draft(&api, &session);
    let before = discovery_state(&path, &events);
    for bad_parent in [
        serde_json::Value::Null,
        serde_json::json!(Uuid::nil()),
        serde_json::json!(Uuid::new_v4()),
    ] {
        let mut changed = request.clone();
        changed["prior_operation_id"] = bad_parent;
        assert!((400..500).contains(&submit(&api, &changed).status));
    }
    for field in ["base_global_review_count", "base_head_review_count"] {
        let mut changed = request.clone();
        changed[field] = serde_json::json!(request[field].as_u64().unwrap() + 1);
        assert!((400..500).contains(&submit(&api, &changed).status));
    }
    let mut stale = request.clone();
    stale["expected_session_version"] = serde_json::json!(session.version + 1);
    assert!((400..500).contains(&submit(&api, &stale).status));
    assert_eq!(discovery_state(&path, &events), before);
}

#[test]
fn budget_successor_competing_connections_create_one_slot_only() {
    let (temp, api, session) = exhausted_budget_review_fixture();
    assert_eq!(submit(&api, &draft(&api, &session)).status, 200);
    consume_reviews(&api, &session, 3, true);
    let request: AdaptiveBudgetReviewExtensionRequestV1 =
        serde_json::from_value(successor_draft(&api, &session)).unwrap();
    let path = temp.path().join("company.sqlite");
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let mut threads = Vec::new();
    for _ in 0..2 {
        let store = sentinel_workflow::WorkflowStore::open(&path).unwrap();
        let mut request = request.clone();
        request.operation_id = Uuid::new_v4();
        let operator = operator(&api);
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            store
                .authorize_budget_review_extension(&operator, &request, now_unix_ms())
                .map(|(replayed, _)| replayed)
        }));
    }
    let outcomes: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| matches!(result, Ok(false)))
            .count(),
        1
    );
    assert_eq!(outcomes.iter().filter(|result| matches!(result, Err(error) if error.code == WorkflowErrorCode::IdempotencyConflict)).count(), 1);
    assert_eq!(
        api.store
            .budget_review_limits(
                &session.grant.authority.tenant_id,
                session.grant.session_id,
                session.version,
                now_unix_ms()
            )
            .unwrap()
            .0,
        9
    );
    let connection = sentinel_limbo::rusqlite::Connection::open(path).unwrap();
    let count: i64 = connection.query_row("SELECT COUNT(*) FROM company_entities WHERE entity_kind='adaptive_budget_review_extension'", [], |row| row.get(0)).unwrap();
    assert_eq!(count, 2);
}

#[test]
fn budget_successor_changed_source_denies_fresh_issuance_without_mutating_receipts() {
    let (temp, api, session) = exhausted_budget_review_fixture();
    assert_eq!(submit(&api, &draft(&api, &session)).status, 200);
    consume_reviews(&api, &session, 3, true);
    let request = successor_draft(&api, &session);
    let project = api
        .store
        .company_project(
            &session.grant.authority.tenant_id,
            &session.grant.authority.project_id,
        )
        .unwrap()
        .unwrap();
    let calls = api
        .store
        .adaptive_leadership_review_calls(
            &session.grant.authority.tenant_id,
            session.grant.session_id,
        )
        .unwrap();
    api.store
        .apply_company_command(
            &calls[0].grant.leadership_principal,
            Uuid::new_v4(),
            &CompanyWorkflowCommandV1::RecordDecision {
                project_id: project.project_id,
                expected_version: project.version,
                work_item_id: None,
                choice_ref: "changed-budget-source".into(),
                rationale_ref: "verify-exact-source-fence".into(),
            },
            now_unix_ms(),
        )
        .unwrap();
    let path = temp.path().join("company.sqlite");
    let events = temp.path().join("events.sqlite");
    let before = discovery_state(&path, &events);
    assert_eq!(submit(&api, &request).status, 422);
    assert!((400..500).contains(
        &call(
            &api,
            "GET",
            &format!("{}&successor=true", query(&session)),
            &credentials(8),
            &[]
        )
        .status
    ));
    for head in [session.version, session.version + 1] {
        assert_eq!(
            api.store
                .budget_review_limits(
                    &session.grant.authority.tenant_id,
                    session.grant.session_id,
                    head,
                    now_unix_ms()
                )
                .unwrap(),
            (3, 3, None)
        );
    }
    assert_eq!(discovery_state(&path, &events), before);
}

#[test]
fn budget_successor_latest_read_does_not_hide_corrupt_original_proof() {
    let (temp, api, session) = exhausted_budget_review_fixture();
    let original = draft(&api, &session);
    assert_eq!(submit(&api, &original).status, 200);
    consume_reviews(&api, &session, 3, true);
    let successor = successor_draft(&api, &session);
    assert_eq!(submit(&api, &successor).status, 200);
    let connection =
        sentinel_limbo::rusqlite::Connection::open(temp.path().join("company.sqlite")).unwrap();
    connection
        .execute(
            "UPDATE company_entities SET payload_digest='corrupt-parent'
        WHERE entity_kind='adaptive_budget_review_extension' AND entity_id NOT LIKE '%-successor'",
            [],
        )
        .unwrap();
    assert_eq!(
        api.store
            .budget_review_extension(
                &session.grant.authority.tenant_id,
                session.grant.session_id,
                session.version
            )
            .err()
            .unwrap()
            .code,
        WorkflowErrorCode::CorruptStore
    );
    assert_eq!(
        api.store
            .budget_review_limits(
                &session.grant.authority.tenant_id,
                session.grant.session_id,
                session.version,
                now_unix_ms()
            )
            .err()
            .unwrap()
            .code,
        WorkflowErrorCode::CorruptStore
    );
    assert!(submit(&api, &original).status >= 400);
    assert!(submit(&api, &successor).status >= 400);
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
