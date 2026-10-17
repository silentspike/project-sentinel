//! Store-backed synthetic decisions; no provider or runtime execution.
use super::*;
use crate::workbench::WorkbenchAuthoritySource;

fn counted<T>(run: impl FnOnce() -> T) -> (T, usize) {
    struct Restore(usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            super::super::super::adaptive_work_funding::SOURCE_DISCOVERIES
                .with(|count| count.set(self.0));
        }
    }
    let previous = super::super::super::adaptive_work_funding::SOURCE_DISCOVERIES
        .with(|count| count.replace(0));
    let _restore = Restore(previous);
    let result = run();
    let count =
        super::super::super::adaptive_work_funding::SOURCE_DISCOVERIES.with(std::cell::Cell::get);
    (result, count)
}

#[test]
fn standing_issues_with_one_discovery_and_retains_exact_read_only_preflight_behavior() {
    with_deferred_fixture(|temp, mut api, source| {
        enable(&temp, &mut api);
        let project = project(&api, &source);
        let operator = api.principals.principal("operator").unwrap();
        let before = state(&temp);
        let (legacy, legacy_count) = counted(|| {
            api.work_funding_current_source(
                &operator,
                &project.project_id,
                source.grant.session_id,
                now_unix_ms(),
            )
        });
        legacy.unwrap();
        assert_eq!(legacy_count, 1);
        let (observed, observed_count) = counted(|| {
            api.work_funding_current_source_from_observed(&project, &source, now_unix_ms())
        });
        observed.unwrap();
        assert_eq!(observed_count, 0);
        assert_eq!(state(&temp), before);
        let (issued, discoveries) = counted(|| api.reconcile_standing_work_funding(&project));
        assert!(issued.unwrap());
        assert_eq!(discoveries, 1);
        let receipt = proposal(&api, &source);
        let after = state(&temp);
        let (replayed, discoveries) = counted(|| api.reconcile_standing_work_funding(&project));
        assert!(!replayed.unwrap());
        assert_eq!(discoveries, 1);
        assert_eq!(proposal(&api, &source), receipt);
        assert_eq!(state(&temp), after);
    });
}

#[test]
fn stale_project_hint_is_not_promoted_to_issuance_authority() {
    with_deferred_fixture(|temp, mut api, source| {
        enable(&temp, &mut api);
        let mut observed = project(&api, &source);
        // This unrelated field is deliberately outside the runtime binding.
        observed.cost_ceiling_micros += 1;
        let before = state(&temp);
        assert_eq!(
            api.reconcile_standing_work_funding(&observed)
                .unwrap_err()
                .code,
            WorkflowErrorCode::AuthorityConflict
        );
        assert_eq!(state(&temp), before);
        assert_eq!(session(&api, &source), source);
    });
}

#[test]
fn ordinary_and_observed_preflight_agree_on_external_revocations() {
    for gate in ["employee", "leader", "profile", "catalog", "observation"] {
        with_deferred_fixture(|temp, mut api, source| {
            let project = project(&api, &source);
            let operator = api.principals.principal("operator").unwrap();
            match gate {
                "employee" => stop_review_agent(&api, source.grant.authority.agent_id, true),
                "leader" => {
                    for participant in &project.governance.participants {
                        if matches!(
                            participant.role,
                            CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
                        ) {
                            stop_review_agent(&api, participant.agent_id, true);
                        }
                    }
                }
                "profile" => {
                    Arc::make_mut(api.authority.as_mut().unwrap()).workbench_profile_digest =
                        "0".repeat(64)
                }
                "catalog" => {
                    Arc::make_mut(api.authority.as_mut().unwrap())
                        .workbench_profile
                        .network = "allow".into()
                }
                "observation" => api.workbench = None,
                _ => unreachable!(),
            }
            let before = state(&temp);
            let legacy = api
                .work_funding_current_source(
                    &operator,
                    &project.project_id,
                    source.grant.session_id,
                    now_unix_ms(),
                )
                .unwrap_err();
            let observed = api
                .work_funding_current_source_from_observed(&project, &source, now_unix_ms())
                .unwrap_err();
            assert_eq!(observed.code, legacy.code, "{gate}");
            assert_eq!(observed.retryable, legacy.retryable, "{gate}");
            assert_eq!(
                observed.message, "work funding source evidence unavailable or changed",
                "{gate}"
            );
            // Ordinary discovery can reject the session before funding preflight.
            assert_eq!(
                legacy.message,
                if gate == "profile" {
                    "resume policy source evidence unavailable or changed"
                } else {
                    "work funding source evidence unavailable or changed"
                },
                "{gate}"
            );
            assert_eq!(state(&temp), before);
        });
    }
}

#[test]
fn production_rechecks_reject_policy_health_profile_catalog_and_observation_changes_at_both_phases()
{
    for gate in [
        "removed",
        "changed",
        "operator",
        "employee",
        "leader",
        "profile",
        "catalog",
        "observation",
    ] {
        for fail_at in [1, 2] {
            with_deferred_fixture(|temp, mut api, source| {
                enable(&temp, &mut api);
                let binding = StandingFundingPolicyBinding {
                    path: temp.path().join(FILE_NAME),
                    policy: policy(&api),
                };
                let operator = binding.current_operator(&api.principals).unwrap().unwrap();
                let project = project(&api, &source);
                let store = Arc::clone(&api.store);
                let before = state(&temp);
                let mut checks = 0;
                let result = store.propose_standing_work_funding_observed_with_clock(
                    &operator.principal,
                    &binding.policy,
                    (&project, &source),
                    now_unix_ms,
                    || {
                        checks += 1;
                        if checks == fail_at {
                            match gate {
                                "removed" => fs::remove_file(&binding.path).unwrap(),
                                "changed" => {
                                    let mut changed = binding.policy.clone();
                                    changed.generation += 1;
                                    write_policy(&binding.path, &changed);
                                }
                                "operator" => {
                                    let mut principals = PrincipalAuthenticator {
                                        by_principal_id: api.principals.by_principal_id.clone(),
                                        by_credential_digest: api
                                            .principals
                                            .by_credential_digest
                                            .clone(),
                                    };
                                    principals
                                        .by_principal_id
                                        .remove(&operator.principal.principal_id);
                                    principals.by_credential_digest.retain(|_, bound| {
                                        bound.principal.principal_id
                                            != operator.principal.principal_id
                                    });
                                    api.principals = Arc::new(principals);
                                }
                                "employee" => {
                                    stop_review_agent(&api, source.grant.authority.agent_id, true)
                                }
                                "leader" => {
                                    for participant in &project.governance.participants {
                                        if matches!(
                                            participant.role,
                                            CompanyRoleV1::ProjectManager
                                                | CompanyRoleV1::TechnicalLead
                                        ) {
                                            stop_review_agent(&api, participant.agent_id, true);
                                        }
                                    }
                                }
                                "profile" => {
                                    Arc::make_mut(api.authority.as_mut().unwrap())
                                        .workbench_profile_digest = "0".repeat(64)
                                }
                                "catalog" => {
                                    Arc::make_mut(api.authority.as_mut().unwrap())
                                        .workbench_profile
                                        .network = "allow".into()
                                }
                                "observation" => api.workbench = None,
                                _ => unreachable!(),
                            }
                        }
                        api.recheck_standing_work_funding_source(
                            &binding, &operator, &project, &source,
                        )
                    },
                );
                assert!(result.is_err(), "{gate}, check={fail_at}");
                assert_eq!(checks, fail_at, "{gate}");
                assert_eq!(state(&temp), before, "{gate}, check={fail_at}");
                assert_eq!(session(&api, &source), source);
            });
        }
    }
}

#[test]
fn pinned_observation_projection_matches_existing_authority_and_rejects_foreign_records() {
    with_deferred_fixture_and_tools(|_temp, api, source, tools| {
        let project = project(&api, &source);
        let authority = api.authority.as_ref().unwrap();
        let pinned = |record: &crate::workbench::WorkbenchInvocationRecord| {
            super::super::super::adaptive_work_funding::funding_observation_projection_for_test(
                authority, &project, record,
            )
        };
        let id = source
            .last_observation
            .as_ref()
            .unwrap()
            .effect
            .id
            .to_string();
        let record = tools.load(&id).unwrap().unwrap();
        let ordinary = authority.current_for_record(&record).unwrap();
        let observed = pinned(&record).unwrap();
        assert_eq!(observed, ordinary);
        for change in [
            "project",
            "work",
            "caller",
            "agent",
            "assignment",
            "profile",
        ] {
            let mut changed = record.clone();
            match change {
                "project" => changed.project_id = "other-project".into(),
                "work" => changed.work_item_id = "other-work".into(),
                "caller" => changed.caller_id = "unregistered".into(),
                "agent" => changed.agent_id = AgentId(u16::MAX),
                "assignment" => changed.assignment_version += 1,
                "profile" => changed.tool_profile_digest = "0".repeat(64),
                _ => unreachable!(),
            }
            let denied = pinned(&changed).and_then(|snapshot| {
                crate::workbench::authorize_workbench_record(&changed, &snapshot).map(|_| ())
            });
            assert!(denied.is_err(), "{change}");
        }
    });
}

#[test]
fn pinned_reader_checks_actual_missing_observation_and_digest() {
    with_deferred_fixture_and_tools(|temp, mut api, source, _tools| {
        enable(&temp, &mut api);
        let project = project(&api, &source);
        let mut wrong_digest = source.clone();
        wrong_digest
            .last_observation
            .as_mut()
            .unwrap()
            .observation_digest = "0".repeat(64);
        assert!(api
            .work_funding_current_source_in_snapshot(&project, &wrong_digest, now_unix_ms())
            .is_err());
        let empty =
            Arc::new(WorkbenchInvocationStore::open(temp.path().join("empty.redb")).unwrap());
        let authority = api.authority.as_ref().unwrap();
        let (profile, digest) = authority
            .profile_for_binding(&source.grant.authority.profile_id)
            .unwrap();
        with_private_observation_store_for_test(empty, profile.clone(), digest.to_owned(), || {
            assert!(api
                .work_funding_current_source_from_observed(&project, &source, now_unix_ms())
                .is_err());
            assert!(api
                .work_funding_current_source_in_snapshot(&project, &source, now_unix_ms())
                .is_err());
        });
    });
}

#[test]
fn actual_observation_read_failure_under_sql_fence_rolls_back_before_and_after_insert() {
    for fail_at in [1, 2] {
        with_deferred_fixture_and_tools(|temp, mut api, source, _tools| {
            enable(&temp, &mut api);
            let binding = api.standing_work_funding_policy.as_ref().unwrap();
            let operator = binding.current_operator(&api.principals).unwrap().unwrap();
            let project = project(&api, &source);
            let empty =
                Arc::new(WorkbenchInvocationStore::open(temp.path().join("empty.redb")).unwrap());
            let authority = api.authority.as_ref().unwrap();
            let (profile, digest) = authority
                .profile_for_binding(&source.grant.authority.profile_id)
                .unwrap();
            let before = state(&temp);
            let mut checks = 0;
            assert!(api
                .store
                .propose_standing_work_funding_observed_with_clock(
                    &operator.principal,
                    &binding.policy,
                    (&project, &source),
                    now_unix_ms,
                    || {
                        checks += 1;
                        let check = || {
                            api.recheck_standing_work_funding_source(
                                binding, &operator, &project, &source,
                            )
                        };
                        if checks == fail_at {
                            with_private_observation_store_for_test(
                                Arc::clone(&empty),
                                profile.clone(),
                                digest.to_owned(),
                                check,
                            )
                        } else {
                            check()
                        }
                    },
                )
                .is_err());
            assert_eq!(checks, fail_at);
            assert_eq!(state(&temp), before);
        });
    }
}
