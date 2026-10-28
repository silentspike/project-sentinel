use super::*;

// Frozen pre-batch selection order. SQL/runtime errors are deliberately not flattened.
fn legacy_selection(
    api: &WorkflowApi,
    agent: AgentId,
    now: u64,
) -> Result<Option<AdaptiveLeadershipReviewCallV1>, &'static str> {
    if !api.has_registered_model_role(agent, None, &[CompanyRoleV1::ProjectManager, CompanyRoleV1::TechnicalLead]) {
        return Ok(None);
    }
    for project in api.store.company_projects().map_err(|_| "leadership projects unavailable")? {
        if !project.governance.participants.iter().any(|participant| participant.agent_id == agent
            && matches!(participant.role, CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead)) {
            continue;
        }
        for session in api.review_sessions(&project)? {
            let policy_present = api.store.adaptive_resume_policy(&project.tenant_id, session.grant.session_id)
                .map_err(|_| "resume policy unavailable")?.is_some();
            let mut calls = api.store.adaptive_leadership_review_calls(&project.tenant_id, session.grant.session_id)
                .map_err(|_| "leadership calls unavailable")?;
            let funded_current: Vec<_> = calls.iter().filter(|call| {
                call.grant.work_funding.is_some() && call.decision.is_none() && call.retired_at_unix_ms.is_none()
                    && call.context.source_project == project && call.context.source_session == session
            }).collect();
            if funded_current.len() > 1 {
                return Err("funded leadership current review is ambiguous");
            }
            let funded_id = funded_current.first().map(|call| call.grant.review_id);
            calls.sort_by_key(|call| call.grant.resume_policy.is_none());
            for call in calls {
                if call.grant.leadership_principal.agent_id == Some(agent)
                    && call.decision.is_none() && call.retired_at_unix_ms.is_none()
                    && call.context.source_project == project && call.context.source_session == session
                    && call.dispatch.is_none() && now >= call.grant_issued_at_unix_ms
                    && now < call.grant.expires_at_unix_ms
                    && funded_id.is_none_or(|id| call.grant.review_id == id)
                    && (!policy_present || call.grant.resume_policy.is_some() || call.grant.work_funding.is_some()) {
                    api.validate_resume_policy_review(&call)?;
                    return Ok(Some(call));
                }
            }
        }
    }
    Ok(None)
}

#[test]
fn leadership_discovery_paired_selection_preserves_role_order_expiry_and_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("company.sqlite");
    let events = temp.path().join("events.sqlite");
    let (mut api, context) = fixture(&path, &events);
    let agent = context.binding.grant.leadership_principal.agent_id.unwrap();
    let call = expiry_review(&api, &context);
    for reopen in [false, true] {
        if reopen {
            api.store = Arc::new(sentinel_workflow::WorkflowStore::open(&path).unwrap());
        }
        let before = discovery_state(&path, &events);
        for agent in [agent, AgentId(99), context.binding.grant.assignee_authority.agent_id] {
            for now in [call.grant_issued_at_unix_ms - 1, call.grant_issued_at_unix_ms,
                call.grant.expires_at_unix_ms - 1, call.grant.expires_at_unix_ms] {
                let expected = legacy_selection(&api, agent, now);
                for _ in 0..2 {
                    assert_eq!(api.leadership_review_for_agent_with_clock(agent, || now), expected, "{agent:?}/{now}/{reopen}");
                }
            }
        }
        assert_eq!(discovery_state(&path, &events), before);
    }
}

#[test]
fn leadership_discovery_paired_errors_keep_runtime_head_and_history_precedence() {
    for change in ["head", "history", "head_and_history", "missing_runtime", "runtime_conflict", "project", "assignment", "principal_generation"] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("company.sqlite");
        let events = temp.path().join("events.sqlite");
        let (mut api, context) = fixture(&path, &events);
        let agent = context.binding.grant.leadership_principal.agent_id.unwrap();
        let now = now_unix_ms();
        api.leadership_review_for_agent_with_clock(agent, || now).unwrap().unwrap();
        let connection = sentinel_limbo::rusqlite::Connection::open(&path).unwrap();
        if matches!(change, "head" | "head_and_history" | "missing_runtime" | "runtime_conflict") {
            assert_eq!(connection.execute("UPDATE workflow_adaptive_heads SET version=version+1", []).unwrap(), 1);
        }
        if matches!(change, "history" | "head_and_history" | "missing_runtime" | "runtime_conflict") {
            assert_eq!(connection.execute("UPDATE company_entities SET payload_digest='invalid' WHERE entity_kind='adaptive_leadership_review_call'", []).unwrap(), 1);
        }
        match change {
            "missing_runtime" => api.authority = None,
            "runtime_conflict" => Arc::make_mut(api.authority.as_mut().unwrap()).workbench_profile_digest = "0".repeat(64),
            "project" => { connection.execute("UPDATE company_entities SET payload_digest='invalid' WHERE entity_kind='project'", []).unwrap(); }
            "assignment" | "principal_generation" => change_review_assignee(&mut api, &context, change),
            _ => {}
        }
        let before = discovery_state(&path, &events);
        let expected = legacy_selection(&api, agent, now);
        if change == "head_and_history" {
            assert_eq!(expected, Err("leadership session unavailable"));
        } else if change == "missing_runtime" {
            assert_eq!(expected, Err("leadership runtime unavailable"));
        } else if change == "runtime_conflict" {
            assert_eq!(expected, Ok(None));
        }
        for _ in 0..2 {
            assert_eq!(api.leadership_review_for_agent_with_clock(agent, || now), expected, "{change}");
        }
        assert_eq!(discovery_state(&path, &events), before);
    }
}

#[test]
fn leadership_discovery_selected_review_is_reauthorized_after_warm_selection() {
    for change in ["project", "head", "runtime"] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("company.sqlite");
        let events = temp.path().join("events.sqlite");
        let (mut api, context) = fixture(&path, &events);
        let agent = context.binding.grant.leadership_principal.agent_id.unwrap();
        let selected = api.leadership_review_for_agent(agent).unwrap().unwrap();
        api.leadership_review_for_agent(agent).unwrap().unwrap();
        match change {
            "project" => record_independent_decision(&api, &context.source.source_project),
            "head" => {
                sentinel_limbo::rusqlite::Connection::open(&path).unwrap()
                    .execute("UPDATE workflow_adaptive_heads SET version=version+1", []).unwrap();
            }
            "runtime" => Arc::make_mut(api.authority.as_mut().unwrap()).workbench_profile_digest = "0".repeat(64),
            _ => unreachable!(),
        }
        let before = discovery_state(&path, &events);
        assert!(api.leadership_review_for_dispatch_with_context(agent, selected.grant.review_id).is_err(), "{change}");
        assert_eq!(discovery_state(&path, &events), before);
    }
}

#[test]
fn leadership_discovery_paired_funded_review_keeps_existing_selection_contract() {
    let (_temp, api, session) = super::super::super::adaptive_resume_policy::tests::unreviewed_policy_root_with_limits(
        120_000, false, 4, 4,
    );
    let now = now_unix_ms();
    let operator = api.principals.principal("operator").unwrap();
    let request = api.store.adaptive_work_funding_draft(
        &operator.principal, &session.grant.authority.project_id, session.grant.session_id,
        Uuid::new_v4(), "synthetic-discovery-accounting",
        sentinel_workflow::AdaptiveWorkFundingLimitsV1 {
            additional_model_calls: 4, additional_tool_calls: 4, additional_reviews: 4, additional_windows: 4,
            max_window_ms: 300_000, max_call_duration_ms: 120_000,
            dispatch_margin_ms: sentinel_workflow::ADAPTIVE_RESUME_DISPATCH_MARGIN_MS,
            expires_at_unix_ms: now + 600_000,
        }, now,
    ).unwrap();
    api.store.authorize_adaptive_work_funding(&operator.principal, &request, now).unwrap();
    let project = api.store.company_project(&session.grant.authority.tenant_id, &session.grant.authority.project_id).unwrap().unwrap();
    reconcile_review_at(&api, &project, now);
    let agent = api.principals.principal("pm").unwrap().principal.agent_id.unwrap();
    let expected = legacy_selection(&api, agent, now).unwrap().unwrap();
    assert!(expected.grant.work_funding.is_some());
    assert_eq!(api.leadership_review_for_agent_with_clock(agent, || now).unwrap(), Some(expected));
}

#[test]
fn leadership_discovery_source_closure_uses_one_batch_and_retains_fresh_final_checks() {
    let source = include_str!("../adaptive_leadership_review.rs");
    let selection = source.split("fn leadership_review_for_agent_with_clock(").nth(1).unwrap()
        .split("pub(super) fn leadership_review_for_dispatch(").next().unwrap();
    assert_eq!(selection.matches(".adaptive_leadership_discovery_read(").count(), 1);
    for repeated in [".company_projects(", ".adaptive_project_snapshot(", ".adaptive_resume_policy(", ".adaptive_leadership_review_calls("] {
        assert!(!selection.contains(repeated), "{repeated}");
    }
    assert_eq!(selection.matches(".validate_resume_policy_review(").count(), 1);
    assert_eq!(selection.matches(".snapshot_from_validated_project(").count(), 1);
}
