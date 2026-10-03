//! Temporary store-backed fixtures, not live provider or customer evidence.

use super::super::super::{adaptive_leadership_review, adaptive_recovery, model_work};
use super::*;
use sentinel_workflow::{
    adaptive_tool_digest, AdaptiveModelDecisionV1, CompanyWorkStateV1, ProjectV1,
    QualityGateReceiptBindingV1, WorkOutputReceiptV1, WorkTransitionReceiptV1,
};

const METADATA_CHANGES: [&str; 7] = [
    "work-profile",
    "uninstalled-profile",
    "project-profile",
    "historical-project-profile",
    "reassignment",
    "off-duty",
    "no-authority",
];

fn project(api: &WorkflowApi, session: &AdaptiveSessionV1) -> ProjectV1 {
    api.store
        .company_project(
            &session.grant.authority.tenant_id,
            &session.grant.authority.project_id,
        )
        .unwrap()
        .unwrap()
}

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

fn with_outcome(
    api: &WorkflowApi,
    events: &Path,
    ready: &AdaptiveSessionV1,
    outcome: &str,
) -> AdaptiveSessionV1 {
    let effect = AdaptiveEffectV1 {
        id: Uuid::new_v4(),
        request_digest: "a".repeat(64),
    };
    let pending = advance(
        api,
        ready,
        AdaptiveTransitionV1::ClaimModel {
            effect: effect.clone(),
            previous_observation_digest: None,
        },
    );
    match outcome {
        "model-unknown" => advance(api, &pending, AdaptiveTransitionV1::MarkUnknown { effect }),
        "provider-unknown" => {
            let request_id = format!("company-adaptive-{}-{}", ready.grant.session_id, effect.id);
            api.event_store
                .as_ref()
                .unwrap()
                .reserve_llm_request(
                    &request_id,
                    &effect.request_digest,
                    &ready.grant.authority.agent_id.to_string(),
                )
                .unwrap();
            assert_eq!(
                rusqlite::Connection::open(events)
                    .unwrap()
                    .execute(
                        "UPDATE llm_completion_outbox SET status='failed',payload='',last_error='UnknownOutcome: provider disconnected' WHERE request_id=?1",
                        [request_id],
                    )
                    .unwrap(),
                1
            );
            pending
        }
        "tool-unknown" => {
            let tool = WorkbenchTool::InspectFile {
                path: "README.md".into(),
                max_bytes: 1_024,
            };
            let tool_digest = adaptive_tool_digest(&tool).unwrap();
            let ready_for_tool = advance(
                api,
                &pending,
                AdaptiveTransitionV1::ResolveModel {
                    effect,
                    result_digest: "b".repeat(64),
                    decision: AdaptiveModelDecisionV1::Tool {
                        tool,
                        tool_digest: tool_digest.clone(),
                    },
                },
            );
            let effect = AdaptiveEffectV1 {
                id: Uuid::new_v4(),
                request_digest: "c".repeat(64),
            };
            let pending = advance(
                api,
                &ready_for_tool,
                AdaptiveTransitionV1::ClaimTool {
                    effect: effect.clone(),
                    tool_digest,
                },
            );
            advance(api, &pending, AdaptiveTransitionV1::MarkUnknown { effect })
        }
        _ => panic!("unknown fixture outcome: {outcome}"),
    }
}

fn change_metadata(api: &mut WorkflowApi, path: &Path, session: &AdaptiveSessionV1, change: &str) {
    match change {
        "work-profile" => {
            Arc::make_mut(api.authority.as_mut().unwrap()).workbench_profile_digest =
                "d".repeat(64);
        }
        "uninstalled-profile" => {
            Arc::make_mut(api.authority.as_mut().unwrap())
                .workbench_profile
                .id = "uninstalled-profile".into();
        }
        "project-profile" => {
            Arc::make_mut(api.authority.as_mut().unwrap()).project_profiles =
                super::super::super::project_profiles::ProjectProfileCatalog::test_web_digest(
                    "d".repeat(64),
                );
        }
        "historical-project-profile" => {
            let mut project = project(api, session);
            project.governance.project_profile.digest = "d".repeat(64);
            adaptive_leadership_review::tests::persist_discovery_project(path, &project);
        }
        "reassignment" => {
            let project = project(api, session);
            let authority = &session.grant.authority;
            api.store
                .apply_company_command(
                    &api.principals.principal("pm").unwrap().principal,
                    Uuid::new_v4(),
                    &CompanyWorkflowCommandV1::ReassignWork {
                        project_id: project.project_id,
                        expected_version: project.version,
                        work_item_id: authority.work_item_id.clone(),
                        expected_assignment_version: authority.assignment_version,
                        agent_id: authority.agent_id,
                        organization_generation: authority.organization_generation,
                        organization_digest: authority.organization_digest.clone(),
                        reason_ref: "Fixture replacement assignment lineage".into(),
                    },
                    now_unix_ms(),
                )
                .unwrap();
        }
        "off-duty" => {
            api.authority
                .as_ref()
                .unwrap()
                .runtime_health
                .write()
                .unwrap()
                .agents
                .iter_mut()
                .find(|agent| agent.agent_id == session.grant.authority.agent_id.0)
                .unwrap()
                .expected_active = false;
        }
        "no-authority" => api.authority = None,
        _ => panic!("unknown fixture metadata change: {change}"),
    }
    // Never bypass canonical project integrity just because health is historical.
    assert_eq!(api.store.company_projects().unwrap().len(), 1);
}

fn reopen(path: &Path, events: &Path) -> WorkflowApi {
    let mut api = model_work::configured_test_api(path);
    api.event_store = Some(sentinel_limbo::EventStore::open(events.to_str().unwrap()).unwrap());
    api
}

fn assert_read_only_health(
    api: &WorkflowApi,
    path: &Path,
    events: &Path,
    expected: &AdaptiveSessionV1,
    unknown: bool,
) {
    let before = adaptive_leadership_review::tests::discovery_state(path, events);
    for _ in 0..3 {
        assert_eq!(
            api.store.adaptive_sessions_for_health().unwrap(),
            vec![expected.clone()]
        );
        assert_eq!(api.adaptive_models_have_unknown_outcome().unwrap(), unknown);
        assert_eq!(
            api.health().last_error.as_deref(),
            unknown.then_some("UnknownOutcome")
        );
        assert_eq!(
            adaptive_leadership_review::tests::discovery_state(path, events),
            before
        );
    }
}

#[test]
fn historical_metadata_allows_healthy_reads_but_never_renews_execution_authority() {
    for change in METADATA_CHANGES {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("company.sqlite");
        let events = temp.path().join("events.sqlite");
        let (mut api, ready) = adaptive_recovery::tests::fixture(&path, &events, false);
        let binding = api
            .adaptive_provider_authority_for_claim(ready.grant.authority.agent_id)
            .unwrap()
            .unwrap();
        change_metadata(&mut api, &path, &ready, change);
        let before = adaptive_leadership_review::tests::discovery_state(&path, &events);
        assert_read_only_health(&api, &path, &events, &ready, false);
        if let Some(authority) = &api.authority {
            let scope = &ready.grant.authority;
            if let Ok(current) = authority.snapshot_for_admission(
                &scope.tenant_id,
                &scope.project_id,
                &scope.work_item_id,
                scope.agent_id,
                true,
            ) {
                // Reassignment can admit the new lineage, never the old effect.
                assert_ne!(&current, scope, "{change}");
                let error = api
                    .store
                    .advance_adaptive_session(
                        ready.grant.session_id,
                        ready.version,
                        Uuid::new_v4(),
                        &AdaptiveTransitionV1::ClaimModel {
                            effect: AdaptiveEffectV1 {
                                id: Uuid::new_v4(),
                                request_digest: "f".repeat(64),
                            },
                            previous_observation_digest: None,
                        },
                        &current,
                        now_unix_ms(),
                    )
                    .unwrap_err();
                assert_eq!(error.code, WorkflowErrorCode::AuthorityConflict, "{change}");
            }
        } else {
            assert!(api
                .adaptive_provider_authority_for_exact_binding(&binding)
                .is_err());
        }
        assert_eq!(
            adaptive_leadership_review::tests::discovery_state(&path, &events),
            before
        );
        drop(api);
        let api = reopen(&path, &events);
        assert_read_only_health(&api, &path, &events, &ready, false);
        assert_eq!(
            adaptive_leadership_review::tests::discovery_state(&path, &events),
            before
        );
    }
}

#[test]
fn durable_unknown_outcomes_survive_metadata_drift_and_reopen_without_writes() {
    for change in METADATA_CHANGES {
        for outcome in ["model-unknown", "tool-unknown", "provider-unknown"] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("company.sqlite");
            let events = temp.path().join("events.sqlite");
            let (mut api, ready) = adaptive_recovery::tests::fixture(&path, &events, false);
            let unknown = with_outcome(&api, &events, &ready, outcome);
            change_metadata(&mut api, &path, &unknown, change);
            let before = adaptive_leadership_review::tests::discovery_state(&path, &events);
            assert_read_only_health(&api, &path, &events, &unknown, true);
            drop(api);
            let api = reopen(&path, &events);
            assert_read_only_health(&api, &path, &events, &unknown, true);
            assert_eq!(
                adaptive_leadership_review::tests::discovery_state(&path, &events),
                before,
                "{change}: {outcome}"
            );
        }
    }
}

#[test]
fn historical_pending_unknown_requires_exact_provider_outbox_identity() {
    for mismatch in [
        "none", "request", "digest", "owner", "status", "payload", "reason",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("company.sqlite");
        let events = temp.path().join("events.sqlite");
        let (mut api, ready) = adaptive_recovery::tests::fixture(&path, &events, false);
        let pending = with_outcome(&api, &events, &ready, "provider-unknown");
        let connection = rusqlite::Connection::open(&events).unwrap();
        let sql = match mismatch {
            "none" => None,
            "request" => Some("UPDATE llm_completion_outbox SET request_id='unrelated-request'"),
            "digest" => Some("UPDATE llm_completion_outbox SET request_digest=?1"),
            "owner" => Some("UPDATE llm_completion_outbox SET owner_scope=?1"),
            "status" => Some("UPDATE llm_completion_outbox SET status='provider_in_flight'"),
            "payload" => Some("UPDATE llm_completion_outbox SET payload='{}'"),
            "reason" => Some("UPDATE llm_completion_outbox SET last_error='ordinary_failure'"),
            _ => unreachable!(),
        };
        if let Some(sql) = sql {
            if mismatch == "owner" {
                connection
                    .execute(
                        sql,
                        [sentinel_common::StateTransferScope::for_agent("5").to_wire()],
                    )
                    .unwrap();
            } else if mismatch == "digest" {
                connection.execute(sql, ["d".repeat(64)]).unwrap();
            } else {
                connection.execute(sql, []).unwrap();
            }
        }
        change_metadata(&mut api, &path, &pending, "no-authority");
        assert_read_only_health(&api, &path, &events, &pending, mismatch == "none");
    }
}

fn transition_work(api: &WorkflowApi, session: &AdaptiveSessionV1, to: CompanyWorkStateV1) {
    let project = project(api, session);
    let work = &project.work_items[&session.grant.authority.work_item_id];
    let now = now_unix_ms().max(project.updated_at_unix_ms);
    let needs_outputs = matches!(to, CompanyWorkStateV1::InReview | CompanyWorkStateV1::Done);
    let outputs = if needs_outputs {
        work.spec
            .outputs
            .iter()
            .map(|output| WorkOutputReceiptV1 {
                name: output.name.clone(),
                contract_generation: output.contract_generation,
                contract_digest: output.contract_digest.clone(),
                content_digest: "e".repeat(64),
            })
            .collect()
    } else {
        Vec::new()
    };
    let done = to == CompanyWorkStateV1::Done;
    let receipt = WorkTransitionReceiptV1 {
        schema_version: 1,
        project_id: project.project_id.clone(),
        work_item_id: work.spec.work_item_id.clone(),
        expected_project_version: project.version,
        expected_work_version: work.version,
        expected_assignment_version: session.grant.authority.assignment_version,
        from_state: work.state,
        to_state: to,
        output_receipts: outputs,
        gate_receipt: done.then(|| QualityGateReceiptBindingV1 {
            gate_id: work.spec.quality_gate.gate_id.clone(),
            generation: work.spec.quality_gate.generation,
            gate_digest: work.spec.quality_gate.digest.clone(),
            subject_digest: "f".repeat(64),
            passed: true,
        }),
        phase_a_evidence_digest: "a".repeat(64),
        reason_ref: "Synthetic fixture work transition evidence".into(),
        occurred_at_unix_ms: now,
    };
    api.store
        .apply_company_command(
            &api.principals
                .principal(if done { "qa" } else { "developer-6" })
                .unwrap()
                .principal,
            Uuid::new_v4(),
            &CompanyWorkflowCommandV1::ApplyWorkTransition {
                project_id: project.project_id,
                expected_version: project.version,
                receipt,
            },
            now,
        )
        .unwrap();
}

#[test]
fn unknown_inventory_is_independent_of_assigned_in_progress_in_review_and_done() {
    for state in [
        CompanyWorkStateV1::Assigned,
        CompanyWorkStateV1::InProgress,
        CompanyWorkStateV1::InReview,
        CompanyWorkStateV1::Done,
    ] {
        for outcome in ["model-unknown", "tool-unknown", "provider-unknown"] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("company.sqlite");
            let events = temp.path().join("events.sqlite");
            let (api, ready) = adaptive_recovery::tests::fixture(&path, &events, false);
            let unknown = with_outcome(&api, &events, &ready, outcome);
            if state == CompanyWorkStateV1::Done {
                // The shared fixture governs only PM/developer. Add fixture QA
                // before using the production command and independent Done gate.
                let mut project = project(&api, &ready);
                let mut qa = project.governance.participants[0].clone();
                qa.agent_id = AgentId(8);
                qa.principal_id = "qa".into();
                qa.role = CompanyRoleV1::Qa;
                qa.reports_to = Some(AgentId(5));
                project.governance.participants.push(qa);
                adaptive_leadership_review::tests::persist_discovery_project(&path, &project);
            }
            if state != CompanyWorkStateV1::Assigned {
                transition_work(&api, &ready, CompanyWorkStateV1::InProgress);
            }
            if matches!(
                state,
                CompanyWorkStateV1::InReview | CompanyWorkStateV1::Done
            ) {
                transition_work(&api, &ready, CompanyWorkStateV1::InReview);
            }
            if state == CompanyWorkStateV1::Done {
                transition_work(&api, &ready, CompanyWorkStateV1::Done);
            }
            let project = project(&api, &ready);
            assert_eq!(api.store.company_projects().unwrap(), vec![project.clone()]);
            assert_eq!(
                project.work_items[&ready.grant.authority.work_item_id].state,
                state
            );
            if state != CompanyWorkStateV1::Assigned {
                assert!(api.review_sessions(&project).unwrap().is_empty());
            }
            let before = adaptive_leadership_review::tests::discovery_state(&path, &events);
            assert_read_only_health(&api, &path, &events, &unknown, true);
            drop(api);
            let api = reopen(&path, &events);
            assert_read_only_health(&api, &path, &events, &unknown, true);
            assert_eq!(
                adaptive_leadership_review::tests::discovery_state(&path, &events),
                before
            );
        }
    }
}

#[test]
fn durable_health_preserves_project_head_journal_and_persistence_failures() {
    for unknown in [false, true] {
        for failure in [
            "project",
            "head",
            "journal",
            "persistence",
            "missing-project",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("company.sqlite");
            let events = temp.path().join("events.sqlite");
            let (api, ready) = adaptive_recovery::tests::fixture(&path, &events, false);
            if unknown {
                with_outcome(&api, &events, &ready, "model-unknown");
            }
            let connection = rusqlite::Connection::open(&path).unwrap();
            let sql = match failure {
                "project" => "UPDATE company_entities SET payload_digest='invalid' WHERE entity_kind='project'",
                "head" => "UPDATE workflow_adaptive_heads SET version=version+1",
                "journal" => "UPDATE workflow_operations SET request_digest='invalid' WHERE operation_namespace LIKE 'adaptive-session-v1:%'",
                "persistence" => "ALTER TABLE workflow_adaptive_heads RENAME COLUMN version TO unavailable_version",
                "missing-project" => "DELETE FROM company_entities WHERE entity_kind='project'",
                _ => unreachable!(),
            };
            connection.execute_batch(sql).unwrap();
            let before = adaptive_leadership_review::tests::discovery_state(&path, &events);
            let expected = match failure {
                "project" => "adaptive projects unavailable",
                "missing-project" => "adaptive project unavailable",
                _ => "adaptive sessions unavailable",
            };
            for _ in 0..2 {
                assert_eq!(
                    api.adaptive_models_have_unknown_outcome(),
                    Err(expected),
                    "{failure}"
                );
                assert_eq!(api.health().last_error.as_deref(), Some(expected));
                assert!(!api.health().ready);
                assert_eq!(
                    adaptive_leadership_review::tests::discovery_state(&path, &events),
                    before
                );
            }
        }
    }
}

fn second_session(api: &WorkflowApi, ready: &AdaptiveSessionV1) -> AdaptiveSessionV1 {
    let binding = model_work::assign_test_work_from(api, Some(8), 1_000);
    let current = api
        .authority
        .as_ref()
        .unwrap()
        .snapshot_for_admission(
            &TenantId::parse(&binding.tenant_id).unwrap(),
            &ProjectId::parse(&binding.project_id).unwrap(),
            &WorkItemId::parse(&binding.work_item_id).unwrap(),
            binding.agent_id,
            false,
        )
        .unwrap();
    let mut grant = ready.grant.clone();
    grant.session_id = Uuid::new_v4();
    grant.authority = current.clone();
    grant.provider_allowance_id = binding.reservation_id;
    let allowance = binding.subscription_grant.unwrap();
    grant.deadline_ms = allowance.expires_at_unix_ms;
    let project = api
        .store
        .company_project(&current.tenant_id, &current.project_id)
        .unwrap()
        .unwrap();
    let allowance = project.subscription_call.as_ref().unwrap();
    let authority_digest = current.canonical_digest().unwrap();
    let provider_bytes = serde_json::to_vec(&(allowance, &authority_digest)).unwrap();
    grant.provider_authority_digest = domain_digest(
        "sentinel.workflow.adaptive-provider-authority.v1",
        &[&provider_bytes],
    );
    grant.created_at_ms = allowance.created_at_unix_ms;
    api.store
        .begin_adaptive_session(&grant, &current, grant.created_at_ms)
        .unwrap()
        .1
}

#[test]
fn an_unknown_session_never_hides_later_corruption_missing_project_or_outbox_failure() {
    for failure in ["head", "namespace-alias", "missing-project", "outbox"] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("company.sqlite");
        let events = temp.path().join("events.sqlite");
        let (api, ready) = adaptive_recovery::tests::fixture(&path, &events, false);
        second_session(&api, &ready);
        let sessions = api.store.adaptive_sessions_for_health().unwrap();
        assert_eq!(sessions.len(), 2);
        with_outcome(&api, &events, &sessions[0], "model-unknown");
        with_outcome(&api, &events, &sessions[1], "provider-unknown");
        assert!(api.adaptive_models_have_unknown_outcome().unwrap());
        let expected = if failure == "head" {
            rusqlite::Connection::open(&path)
                .unwrap()
                .execute(
                    "UPDATE workflow_adaptive_heads SET version=version+1 WHERE session_id=?1",
                    [sessions[1].grant.session_id.to_string()],
                )
                .unwrap();
            "adaptive sessions unavailable"
        } else if failure == "namespace-alias" {
            let namespace = format!("adaptive-session-v1:{}", sessions[1].grant.session_id);
            let inserted = rusqlite::Connection::open(&path)
                .unwrap()
                .execute(
                    "INSERT INTO workflow_operations (operation_namespace,operation_id,request_digest,response,created_at_ms) SELECT ?1,operation_id,request_digest,response,created_at_ms FROM workflow_operations WHERE operation_namespace=?2",
                    rusqlite::params![format!("{namespace}:"), namespace],
                )
                .unwrap();
            assert!(inserted > 0);
            "adaptive sessions unavailable"
        } else if failure == "missing-project" {
            let scope = &sessions[1].grant.authority;
            rusqlite::Connection::open(&path)
                .unwrap()
                .execute(
                    "DELETE FROM company_entities WHERE entity_kind='project' AND tenant_id=?1 AND entity_id=?2",
                    rusqlite::params![scope.tenant_id.0, scope.project_id.0],
                )
                .unwrap();
            "adaptive project unavailable"
        } else {
            rusqlite::Connection::open(&events)
                .unwrap()
                .execute_batch(
                    "ALTER TABLE llm_completion_outbox RENAME COLUMN request_digest TO unavailable_request_digest",
                )
                .unwrap();
            "adaptive provider outcome unavailable"
        };
        let before = adaptive_leadership_review::tests::discovery_state(&path, &events);
        for _ in 0..2 {
            assert_eq!(
                api.adaptive_models_have_unknown_outcome(),
                Err(expected),
                "{failure}"
            );
            assert_eq!(api.health().last_error.as_deref(), Some(expected));
            assert_eq!(
                adaptive_leadership_review::tests::discovery_state(&path, &events),
                before
            );
        }
    }
}
