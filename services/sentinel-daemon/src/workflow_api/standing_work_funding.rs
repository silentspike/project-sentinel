//! Standing operator proposals, never automatic employee-window adoption.

use super::*;
use sentinel_workflow::StandingWorkFundingPolicyV1;

const FILE_NAME: &str = "company-standing-funding.json";
const MAX_POLICY_BYTES: usize = 4096;

fn source_conflict() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::AuthorityConflict,
        false,
        "standing funding authority changed",
    )
}

pub(super) struct StandingFundingPolicyBinding {
    path: PathBuf,
    policy: StandingWorkFundingPolicyV1,
}

fn read_policy(path: &Path) -> Result<StandingWorkFundingPolicyV1, WorkflowError> {
    let bytes = read_principal_bindings_file(path)?;
    if bytes.len() > MAX_POLICY_BYTES {
        return Err(workflow_unavailable());
    }
    serde_json::from_slice(&bytes).map_err(|_| workflow_unavailable())
}

fn registered_operator(
    policy: &StandingWorkFundingPolicyV1,
    principals: &PrincipalAuthenticator,
) -> Result<BoundPrincipal, WorkflowError> {
    let operator = principals
        .principal(&policy.issuer_principal_id)
        .ok_or_else(workflow_unavailable)?;
    policy.validate_for(&operator.principal)?;
    if operator.principal.kind != CompanyPrincipalKindV1::Operator
        || !matches!(
            operator.principal.role,
            CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
        )
    {
        return Err(workflow_unavailable());
    }
    Ok(operator)
}

impl StandingFundingPolicyBinding {
    pub(super) fn load(
        config_dir: &Path,
        principals: &PrincipalAuthenticator,
        model_work_enabled: bool,
    ) -> Result<Option<Self>, WorkflowError> {
        let path = config_dir.join(FILE_NAME);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(workflow_unavailable()),
            Ok(_) if !model_work_enabled => return Err(workflow_unavailable()),
            Ok(_) => {}
        }
        let policy = read_policy(&path)?;
        registered_operator(&policy, principals)?;
        Ok(Some(Self { path, policy }))
    }

    fn current_operator(
        &self,
        principals: &PrincipalAuthenticator,
    ) -> Result<Option<BoundPrincipal>, WorkflowError> {
        match fs::symlink_metadata(&self.path) {
            // Removal revokes future automatic proposals, not retained finite grants.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(workflow_unavailable()),
            Ok(_) => {}
        }
        if read_policy(&self.path)? != self.policy {
            // A changed standing delegation requires an explicit configuration reload.
            return Err(workflow_unavailable());
        }
        registered_operator(&self.policy, principals).map(Some)
    }
}

impl WorkflowApi {
    fn recheck_standing_work_funding_source(
        &self,
        binding: &StandingFundingPolicyBinding,
        operator: &BoundPrincipal,
        project: &sentinel_workflow::ProjectV1,
        session: &AdaptiveSessionV1,
    ) -> Result<(), WorkflowError> {
        let live_operator = binding
            .current_operator(&self.principals)?
            .ok_or_else(source_conflict)?;
        if live_operator.principal != operator.principal
            || live_operator.execution_authority != operator.execution_authority
        {
            return Err(source_conflict());
        }
        self.validate_work_funding_operator(&live_operator)?;
        self.work_funding_current_source_in_snapshot(project, session, now_unix_ms())?;
        let current = self
            .authority
            .as_ref()
            .ok_or_else(workflow_unavailable)?
            .snapshot_from_validated_project(
                project,
                &project.tenant_id,
                &project.project_id,
                &session.grant.authority.work_item_id,
                session.grant.authority.agent_id,
                true,
            )
            .map_err(execution_intent_port_error)?;
        if current != session.grant.authority {
            return Err(source_conflict());
        }
        Ok(())
    }

    // Called only by reconciliation, already holding its recovery/mutation fence.
    pub(super) fn reconcile_standing_work_funding(
        &self,
        project: &sentinel_workflow::ProjectV1,
    ) -> Result<bool, WorkflowError> {
        let Some(binding) = &self.standing_work_funding_policy else {
            return Ok(false);
        };
        if !self.enabled
            || !self.model_work_enabled
            || project.tenant_id != binding.policy.tenant_id
            || project.lifecycle_state != sentinel_workflow::ProjectLifecycleStateV1::Active
        {
            return Ok(false);
        }
        let Some(operator) = binding.current_operator(&self.principals)? else {
            return Ok(false);
        };
        self.validate_work_funding_operator(&operator)?;
        #[cfg(test)]
        super::adaptive_work_funding::SOURCE_DISCOVERIES.with(|count| count.set(count.get() + 1));
        for session in self
            .review_sessions(project)
            .map_err(|_| workflow_unavailable())?
        {
            if !matches!(session.cursor, AdaptiveCursorV1::ReadyForModel)
                || !session.model_window_exhausted_at(now_unix_ms())
            {
                continue;
            }
            match self.work_funding_current_source_from_observed(project, &session, now_unix_ms()) {
                Ok(()) => {}
                Err(error) if error.code == WorkflowErrorCode::AuthorityConflict => continue,
                Err(error) => return Err(error),
            }
            let proposal = self
                .store
                .propose_standing_work_funding_observed_with_clock(
                    &operator.principal,
                    &binding.policy,
                    (project, &session),
                    now_unix_ms,
                    || {
                        self.recheck_standing_work_funding_source(
                            binding, &operator, project, &session,
                        )
                    },
                )?;
            if proposal.is_some_and(|(replayed, _)| !replayed) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
#[path = "standing_work_funding/integration_tests.rs"]
mod integration_tests;

#[cfg(test)]
mod policy_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn policy(api: &WorkflowApi) -> StandingWorkFundingPolicyV1 {
        let issuer = api.principals.principal("operator").unwrap().principal;
        StandingWorkFundingPolicyV1 {
            schema_version: 1,
            policy_id: "standing-tests".into(),
            generation: 1,
            tenant_id: issuer.tenant_id,
            issuer_principal_id: issuer.principal_id,
            epoch_duration_ms: 6 * 60 * 60 * 1000,
            additional_reviews: 4,
            additional_windows: 4,
            max_window_ms: 300_000,
        }
    }

    #[test]
    fn policy_is_absent_by_default_and_requires_enabled_models_and_registered_issuer() {
        let temp = tempfile::tempdir().unwrap();
        let api =
            super::super::model_work::configured_test_api(&temp.path().join("company.sqlite"));
        assert!(
            StandingFundingPolicyBinding::load(temp.path(), &api.principals, true)
                .unwrap()
                .is_none()
        );
        let path = temp.path().join(FILE_NAME);
        fs::write(&path, serde_json::to_vec(&policy(&api)).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(StandingFundingPolicyBinding::load(temp.path(), &api.principals, false).is_err());
        assert!(
            StandingFundingPolicyBinding::load(temp.path(), &api.principals, true)
                .unwrap()
                .is_some()
        );
        let mut foreign = policy(&api);
        foreign.issuer_principal_id = "missing-operator".into();
        fs::write(&path, serde_json::to_vec(&foreign).unwrap()).unwrap();
        assert!(StandingFundingPolicyBinding::load(temp.path(), &api.principals, true).is_err());
    }

    #[test]
    fn policy_removal_stops_new_proposals_and_changes_require_reload() {
        let temp = tempfile::tempdir().unwrap();
        let api =
            super::super::model_work::configured_test_api(&temp.path().join("company.sqlite"));
        let path = temp.path().join(FILE_NAME);
        let initial = policy(&api);
        fs::write(&path, serde_json::to_vec(&initial).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let binding = StandingFundingPolicyBinding::load(temp.path(), &api.principals, true)
            .unwrap()
            .unwrap();
        assert!(binding.current_operator(&api.principals).unwrap().is_some());
        let mut changed = initial;
        changed.generation += 1;
        fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(binding.current_operator(&api.principals).is_err());
        fs::remove_file(&path).unwrap();
        assert!(binding.current_operator(&api.principals).unwrap().is_none());
    }

    #[test]
    fn policy_rejects_symlinks_shared_inodes_unknown_fields_and_writable_files() {
        let temp = tempfile::tempdir().unwrap();
        let api =
            super::super::model_work::configured_test_api(&temp.path().join("company.sqlite"));
        let path = temp.path().join(FILE_NAME);
        let target = temp.path().join("policy-target.json");
        fs::write(&target, serde_json::to_vec(&policy(&api)).unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(StandingFundingPolicyBinding::load(temp.path(), &api.principals, true).is_err());
        fs::remove_file(&path).unwrap();
        fs::hard_link(&target, &path).unwrap();
        assert!(StandingFundingPolicyBinding::load(temp.path(), &api.principals, true).is_err());
        fs::remove_file(&path).unwrap();
        fs::write(&path, serde_json::to_vec(&policy(&api)).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(StandingFundingPolicyBinding::load(temp.path(), &api.principals, true).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let mut value = serde_json::to_value(policy(&api)).unwrap();
        value["automatic_continue"] = serde_json::json!(true);
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(StandingFundingPolicyBinding::load(temp.path(), &api.principals, true).is_err());
    }
}
