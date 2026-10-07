use super::*;
use crate::{adaptive_leadership_continuation_provider_authority_digest, AdaptiveSessionGrantV1};

const MAX_HISTORICAL_PROJECT_EVENTS: usize = 1_000;
const MAX_HISTORICAL_SNAPSHOT_BLOB_BYTES: usize = 2 * 1024 * 1024;
const MAX_HISTORICAL_SNAPSHOT_TOTAL_BYTES: usize = 16 * 1024 * 1024;

pub(super) struct SnapshotByteBudget {
    remaining: usize,
}

impl SnapshotByteBudget {
    fn new() -> Self {
        Self {
            remaining: MAX_HISTORICAL_SNAPSHOT_TOTAL_BYTES,
        }
    }

    pub(super) fn charge(&mut self, bytes: i64) -> Result<(), WorkflowError> {
        let bytes = usize::try_from(bytes).map_err(|_| corrupt())?;
        if bytes > MAX_HISTORICAL_SNAPSHOT_BLOB_BYTES || bytes > self.remaining {
            return Err(corrupt());
        }
        self.remaining -= bytes;
        Ok(())
    }
}

impl WorkflowStore {
    /// Verified original usage attribution only, never current execution authority.
    /// The caller must obtain `root` and the claim time from verified journal evidence.
    /// Incomplete, corrupt or oversized history fails closed rather than granting a retry.
    /// Event payloads and operation responses are capped at 2 MiB each, 16 MiB combined.
    pub fn historical_adaptive_provider_project(
        &self,
        root: &AdaptiveSessionGrantV1,
        claimed_at_ms: u64,
    ) -> Result<Option<ProjectV1>, WorkflowError> {
        root.validate()?;
        if claimed_at_ms < root.created_at_ms || claimed_at_ms >= root.deadline_ms {
            return Err(invalid("historical adaptive claim time is invalid"));
        }
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(WorkflowError::from)?;
        historical_adaptive_provider_project_in_connection(&transaction, root, claimed_at_ms)
    }
}

// Reuses the caller's transaction; callers must verify the root against the journal.
pub(super) fn historical_adaptive_provider_project_in_connection(
    connection: &Connection,
    root: &AdaptiveSessionGrantV1,
    claimed_at_ms: u64,
) -> Result<Option<ProjectV1>, WorkflowError> {
    validation_scope::memoize(
        connection,
        "historical-project",
        &(root, claimed_at_ms),
        || historical_adaptive_provider_project_uncached(connection, root, claimed_at_ms),
    )
}

fn historical_adaptive_provider_project_uncached(
    connection: &Connection,
    root: &AdaptiveSessionGrantV1,
    claimed_at_ms: u64,
) -> Result<Option<ProjectV1>, WorkflowError> {
    root.validate()?;
    if claimed_at_ms < root.created_at_ms || claimed_at_ms >= root.deadline_ms {
        return Err(invalid("historical adaptive claim time is invalid"));
    }
    let claimed_at = sql_u64(claimed_at_ms)?;
    let sequences = {
        let mut statement = connection.prepare(
                "SELECT sequence FROM company_events WHERE tenant_id=?1 AND project_id=?2 AND created_at_ms<=?3 AND event_type GLOB 'project_*' AND event_type NOT IN ('project_planning_call_authorized','project_planning_call_renewed','project_planning_call_dispatched','project_planning_call_completed') ORDER BY sequence DESC LIMIT ?4",
            )?;
        let rows = statement.query_map(
            params![
                root.authority.tenant_id.0,
                root.authority.project_id.0,
                claimed_at,
                (MAX_HISTORICAL_PROJECT_EVENTS + 1) as i64,
            ],
            |row| row.get::<_, i64>(0),
        )?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    if sequences.len() > MAX_HISTORICAL_PROJECT_EVENTS {
        return Err(invalid("historical adaptive project event limit exceeded"));
    }
    let mut selected = None;
    let mut byte_budget = SnapshotByteBudget::new();
    for sequence in sequences {
        let sequence = stored_u64(sequence)?;
        let payload_bytes = connection
                .query_row(
                    "SELECT CASE WHEN typeof(payload)='blob' THEN length(payload) ELSE -1 END FROM company_events WHERE sequence=?1",
                    [sql_u64(sequence)?],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .ok_or_else(corrupt)?;
        byte_budget.charge(payload_bytes)?;
        let row = read_company_event_row(connection, sequence)?.ok_or_else(corrupt)?;
        let (_, project) = validate_project_snapshot_event_with_byte_budget(
            connection,
            &row,
            Some(&mut byte_budget),
        )?;
        if project.tenant_id != root.authority.tenant_id
            || project.project_id != root.authority.project_id
            || project.updated_at_unix_ms > claimed_at_ms
        {
            return Err(corrupt());
        }
        // All snapshots are validated above; matching stops after the newest hit.
        if selected.is_none() && historical_project_matches(&project, root, claimed_at_ms)? {
            selected = Some(project);
        }
    }
    Ok(selected)
}

fn historical_project_matches(
    project: &ProjectV1,
    root: &AdaptiveSessionGrantV1,
    claimed_at_ms: u64,
) -> Result<bool, WorkflowError> {
    if project.lifecycle_state != ProjectLifecycleStateV1::Active {
        return Ok(false);
    }
    let Some(allowance) = project.subscription_call.as_ref() else {
        return Ok(false);
    };
    let authority = &root.authority;
    let grant = &allowance.grant;
    if allowance.allowance_id != root.provider_allowance_id
        || !constant_time_eq(
            &adaptive_leadership_continuation_provider_authority_digest(allowance, authority)?,
            &root.provider_authority_digest,
        )
        || grant.work_item_id != authority.work_item_id
        || grant.agent_id != authority.agent_id
        || grant.assignment_version != authority.assignment_version
        || grant.provider != root.provider
        || grant.model != root.model
        || grant.catalog_digest != root.catalog_digest
        || grant.max_calls < root.max_model_calls
        || grant.max_duration_ms < root.max_call_duration_ms
        || grant.expires_at_unix_ms < root.deadline_ms
        || allowance.created_at_unix_ms > root.created_at_ms
        || claimed_at_ms < allowance.created_at_unix_ms
        || claimed_at_ms >= grant.expires_at_unix_ms
        || project.governance.project_profile.generation != authority.policy_generation
        || !constant_time_eq(
            &project.governance.project_profile.digest,
            &authority.policy_digest,
        )
    {
        return Ok(false);
    }
    let Some(work) = project.work_items.get(&authority.work_item_id) else {
        return Ok(false);
    };
    let assignments = work
        .assignments
        .iter()
        .filter(|assignment| assignment.active)
        .collect::<Vec<_>>();
    let [assignment] = assignments.as_slice() else {
        return Ok(false);
    };
    Ok(matches!(
        work.state,
        CompanyWorkStateV1::Assigned
            | CompanyWorkStateV1::InProgress
            | CompanyWorkStateV1::InReview
    ) && work.spec.work_item_id == authority.work_item_id
        && work.spec.owner == authority.agent_id
        && assignment.assignment_id == grant.assignment_id
        && assignment.agent_id == authority.agent_id
        && assignment.role == work.spec.required_role
        && assignment.assignment_version == authority.assignment_version
        && constant_time_eq(
            &assignment.canonical_digest()?,
            &authority.assignment_digest,
        )
        && assignment.profile.profile_id == authority.profile_id
        && assignment.profile.generation == authority.profile_generation
        && constant_time_eq(&assignment.profile.digest, &authority.profile_digest)
        && assignment.organization_generation == authority.organization_generation
        && constant_time_eq(
            &assignment.organization_digest,
            &authority.organization_digest,
        )
        && project.governance.participants.iter().any(|participant| {
            participant.agent_id == authority.agent_id
                && participant.principal_id == authority.principal.principal_id
                && participant.role == work.spec.required_role
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentId, CompanyWorkItemSpecV1, PrincipalAuthorityV1, QualityGateBindingV1,
        RuntimeAuthoritySnapshotV1, WorkOutputContractV1,
    };

    const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const CLAIMED_AT: u64 = 7;

    struct Fixture {
        _temp: tempfile::TempDir,
        store: WorkflowStore,
        leader: AuthenticatedCompanyPrincipalV1,
        project: ProjectV1,
        root: AdaptiveSessionGrantV1,
        sequence: u64,
    }

    fn fixture() -> Fixture {
        let (temp, _path, store, _customer, mut project) =
            crate::domain_store::tests::accepted_project_fixture();
        let leader = AuthenticatedCompanyPrincipalV1 {
            schema_version: 1,
            tenant_id: project.tenant_id.clone(),
            principal_id: "pm-a".into(),
            kind: CompanyPrincipalKindV1::Agent,
            role: CompanyRoleV1::ProjectManager,
            customer_id: None,
            agent_id: Some(AgentId(1)),
            authority_generation: 1,
            authority_digest: DIGEST.into(),
        };
        let work_item_id = WorkItemId::parse("historical-work").unwrap();
        for step in 0..3_u64 {
            let command = match step {
                0 => CompanyWorkflowCommandV1::PlanWorkGraph {
                    project_id: project.project_id.clone(),
                    expected_version: project.version,
                    items: vec![CompanyWorkItemSpecV1 {
                        work_item_id: work_item_id.clone(),
                        title: "Build source artifact".into(),
                        objective: "Implement the accepted customer scope".into(),
                        required_role: CompanyRoleV1::Developer,
                        required_specialties: BTreeSet::from(["rust".into()]),
                        dependency_ids: BTreeSet::new(),
                        owner: AgentId(2),
                        inputs: vec![],
                        outputs: vec![WorkOutputContractV1 {
                            name: "source".into(),
                            media_type: "application/octet-stream".into(),
                            digest_algorithm: "sha256".into(),
                            contract_generation: 1,
                            contract_digest: DIGEST.into(),
                        }],
                        quality_gate: QualityGateBindingV1 {
                            gate_id: "web-work-item-qa-v1".into(),
                            generation: 1,
                            digest: DIGEST.into(),
                        },
                        budget_micros: 100,
                        rework: None,
                    }],
                },
                1 => CompanyWorkflowCommandV1::ActivateProject {
                    project_id: project.project_id.clone(),
                    expected_version: project.version,
                    reason_ref: "Accepted scope ready for execution".into(),
                },
                _ => CompanyWorkflowCommandV1::AssignWork {
                    project_id: project.project_id.clone(),
                    expected_version: project.version,
                    work_item_id: work_item_id.clone(),
                    agent_id: AgentId(2),
                    organization_generation: 1,
                    organization_digest: DIGEST.into(),
                    reason_ref: "Developer owns implementation".into(),
                },
            };
            project = apply_project(&store, &leader, command, 3 + step);
        }
        let assignment = &project.work_items[&work_item_id].assignments[0];
        let authority = RuntimeAuthoritySnapshotV1 {
            schema_version: 1,
            tenant_id: project.tenant_id.clone(),
            project_id: project.project_id.clone(),
            work_item_id: work_item_id.clone(),
            agent_id: assignment.agent_id,
            assignment_version: assignment.assignment_version,
            assignment_digest: assignment.canonical_digest().unwrap(),
            organization_generation: assignment.organization_generation,
            organization_digest: assignment.organization_digest.clone(),
            principal: PrincipalAuthorityV1::derive("developer-a", 1, &[2; 32]).unwrap(),
            profile_id: assignment.profile.profile_id.clone(),
            profile_generation: assignment.profile.generation,
            profile_digest: assignment.profile.digest.clone(),
            runtime_key: "bwrap-coding-v1".into(),
            runtime_generation: 1,
            runtime_digest: DIGEST.into(),
            policy_generation: project.governance.project_profile.generation,
            policy_digest: project.governance.project_profile.digest.clone(),
            active: true,
            capabilities: BTreeSet::from(["observation.retain_private".into()]),
        };
        let grant = SubscriptionCallGrantV1 {
            schema_version: 1,
            work_item_id,
            assignment_id: assignment.assignment_id.clone(),
            assignment_version: assignment.assignment_version,
            agent_id: assignment.agent_id,
            provider: "codex-cli".into(),
            model: "model-test".into(),
            catalog_digest: DIGEST.into(),
            max_calls: 2,
            max_concurrent: 1,
            max_duration_ms: 120_000,
            token_policy: SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,
            expires_at_unix_ms: 200_006,
        };
        let command = CompanyWorkflowCommandV1::GrantSubscriptionCall {
            project_id: project.project_id.clone(),
            expected_version: project.version,
            grant,
        };
        project = apply_project(&store, &leader, command, 6);
        let allowance = project.subscription_call.as_ref().unwrap();
        let root = AdaptiveSessionGrantV1 {
            schema_version: 1,
            session_id: Uuid::new_v4(),
            provider_allowance_id: allowance.allowance_id.clone(),
            provider_authority_digest: adaptive_leadership_continuation_provider_authority_digest(
                allowance, &authority,
            )
            .unwrap(),
            authority,
            provider: allowance.grant.provider.clone(),
            model: allowance.grant.model.clone(),
            catalog_digest: allowance.grant.catalog_digest.clone(),
            max_output_tokens: 4096,
            max_call_duration_ms: allowance.grant.max_duration_ms,
            max_model_calls: allowance.grant.max_calls,
            max_tool_calls: 2,
            created_at_ms: 6,
            deadline_ms: allowance.grant.expires_at_unix_ms,
        };
        let sequence = store.company_event_cursor().unwrap();
        Fixture {
            _temp: temp,
            store,
            leader,
            project,
            root,
            sequence,
        }
    }

    fn apply_project(
        store: &WorkflowStore,
        leader: &AuthenticatedCompanyPrincipalV1,
        command: CompanyWorkflowCommandV1,
        now: u64,
    ) -> ProjectV1 {
        let result = store
            .apply_company_command(leader, Uuid::new_v4(), &command, now)
            .unwrap();
        let CompanyWorkflowResponseV1::Project(project) = result.response else {
            panic!("expected persisted project");
        };
        *project
    }

    fn total_changes(store: &WorkflowStore) -> i64 {
        store
            .connection
            .lock()
            .unwrap()
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn original_project_survives_current_allowance_replacement_without_writes() {
        let f = fixture();
        let mut next = f.project.subscription_call.as_ref().unwrap().grant.clone();
        next.expires_at_unix_ms = 400_006;
        let current = apply_project(
            &f.store,
            &f.leader,
            CompanyWorkflowCommandV1::GrantSubscriptionCall {
                project_id: f.project.project_id.clone(),
                expected_version: f.project.version,
                grant: next,
            },
            f.root.deadline_ms,
        );
        assert_ne!(
            current.subscription_call.as_ref().unwrap().allowance_id,
            f.root.provider_allowance_id
        );
        let before = total_changes(&f.store);
        assert_eq!(
            f.store
                .historical_adaptive_provider_project(&f.root, CLAIMED_AT)
                .unwrap(),
            Some(f.project.clone())
        );
        assert_eq!(total_changes(&f.store), before);
        assert_eq!(
            f.store
                .company_project(&current.tenant_id, &current.project_id)
                .unwrap(),
            Some(current)
        );
    }

    #[test]
    fn newest_matching_snapshot_is_selected_at_the_inclusive_claim_boundary() {
        let f = fixture();
        let latest = apply_project(
            &f.store,
            &f.leader,
            CompanyWorkflowCommandV1::RecordDecision {
                project_id: f.project.project_id.clone(),
                expected_version: f.project.version,
                work_item_id: None,
                choice_ref: "historical-choice".into(),
                rationale_ref: "historical-rationale".into(),
            },
            CLAIMED_AT,
        );
        assert_eq!(
            f.store
                .historical_adaptive_provider_project(&f.root, CLAIMED_AT)
                .unwrap(),
            Some(latest)
        );
        assert_eq!(
            f.store
                .historical_adaptive_provider_project(&f.root, CLAIMED_AT - 1)
                .unwrap(),
            Some(f.project)
        );
    }

    #[test]
    fn newer_blocked_snapshot_does_not_mask_the_original_active_project() {
        let f = fixture();
        let blocked = apply_project(
            &f.store,
            &f.leader,
            CompanyWorkflowCommandV1::RaiseBlocker {
                project_id: f.project.project_id.clone(),
                expected_version: f.project.version,
                work_item_id: None,
                cause_ref: "historical operational blocker".into(),
                owner: AgentId(1),
            },
            CLAIMED_AT,
        );
        assert_eq!(blocked.lifecycle_state, ProjectLifecycleStateV1::Blocked);
        assert!(!historical_project_matches(&blocked, &f.root, CLAIMED_AT).unwrap());
        let before = total_changes(&f.store);
        assert_eq!(
            f.store
                .historical_adaptive_provider_project(&f.root, CLAIMED_AT)
                .unwrap(),
            Some(f.project)
        );
        assert_eq!(total_changes(&f.store), before);
        assert_eq!(
            f.store
                .company_project(&blocked.tenant_id, &blocked.project_id)
                .unwrap(),
            Some(blocked)
        );
    }

    #[test]
    fn original_allowance_window_cannot_be_extended_by_a_supplied_root() {
        let f = fixture();
        let mut root = f.root.clone();
        root.deadline_ms += 1;
        let before = total_changes(&f.store);
        assert_eq!(
            f.store
                .historical_adaptive_provider_project(&root, f.root.deadline_ms)
                .unwrap(),
            None
        );
        assert_eq!(total_changes(&f.store), before);
    }

    #[test]
    fn allowance_dispatch_state_must_match_the_original_provider_digest() {
        let f = fixture();
        let allowance = f.project.subscription_call.as_ref().unwrap();
        let worker = AuthenticatedCompanyPrincipalV1 {
            schema_version: 1,
            tenant_id: f.root.authority.tenant_id.clone(),
            principal_id: f.root.authority.principal.principal_id.clone(),
            kind: CompanyPrincipalKindV1::Agent,
            role: CompanyRoleV1::Developer,
            customer_id: None,
            agent_id: Some(f.root.authority.agent_id),
            authority_generation: f.root.authority.principal.principal_generation,
            authority_digest: f.root.authority.principal.authority_digest.clone(),
        };
        let dispatched = apply_project(
            &f.store,
            &worker,
            CompanyWorkflowCommandV1::ClaimSubscriptionCall {
                project_id: f.project.project_id.clone(),
                expected_version: f.project.version,
                allowance_id: allowance.allowance_id.clone(),
                request_id: format!("company-provider-{}", allowance.allowance_id),
                request_digest: DIGEST.into(),
            },
            CLAIMED_AT,
        );
        assert_eq!(
            f.store
                .historical_adaptive_provider_project(&f.root, CLAIMED_AT)
                .unwrap(),
            Some(f.project)
        );
        let mut root = f.root;
        root.provider_authority_digest =
            adaptive_leadership_continuation_provider_authority_digest(
                dispatched.subscription_call.as_ref().unwrap(),
                &root.authority,
            )
            .unwrap();
        assert_eq!(
            f.store
                .historical_adaptive_provider_project(&root, CLAIMED_AT)
                .unwrap(),
            Some(dispatched)
        );
    }

    #[test]
    fn root_identity_assignment_and_provider_mismatches_never_return_a_project() {
        let f = fixture();
        for case in 0..14 {
            let mut root = f.root.clone();
            match case {
                0 => root.authority.tenant_id = TenantId::parse("other-tenant").unwrap(),
                1 => root.authority.project_id = ProjectId::parse("other-project").unwrap(),
                2 => root.authority.work_item_id = WorkItemId::parse("other-work").unwrap(),
                3 => root.authority.agent_id = AgentId(1),
                4 => root.authority.assignment_version += 1,
                5 => root.authority.assignment_digest = "b".repeat(64),
                6 => root.authority.profile_digest = "b".repeat(64),
                7 => root.authority.organization_digest = "b".repeat(64),
                8 => root.authority.principal.principal_id = "pm-a".into(),
                9 => root.model = "other-model".into(),
                10 => root.catalog_digest = "b".repeat(64),
                11 => root.max_model_calls += 1,
                12 => root.authority.policy_generation += 1,
                _ => root.authority.policy_digest = "b".repeat(64),
            }
            // A digest supplied for an inconsistent root must not bypass work bindings.
            root.provider_authority_digest =
                adaptive_leadership_continuation_provider_authority_digest(
                    f.project.subscription_call.as_ref().unwrap(),
                    &root.authority,
                )
                .unwrap();
            assert_eq!(
                f.store
                    .historical_adaptive_provider_project(&root, CLAIMED_AT)
                    .unwrap(),
                None,
                "case {case}"
            );
        }
        for case in 0..2 {
            let mut root = f.root.clone();
            if case == 0 {
                root.provider_allowance_id = "other-allowance".into();
            } else {
                root.provider_authority_digest = "b".repeat(64);
            }
            assert_eq!(
                f.store
                    .historical_adaptive_provider_project(&root, CLAIMED_AT)
                    .unwrap(),
                None
            );
        }
    }

    #[test]
    fn absent_history_future_snapshot_and_invalid_claim_times_fail_closed() {
        let f = fixture();
        let mut root = f.root.clone();
        root.created_at_ms = 5;
        assert_eq!(
            f.store
                .historical_adaptive_provider_project(&root, 5)
                .unwrap(),
            None
        );
        for claimed_at in [0, f.root.created_at_ms - 1, f.root.deadline_ms] {
            assert!(f
                .store
                .historical_adaptive_provider_project(&f.root, claimed_at)
                .is_err());
        }
        root.deadline_ms = u64::MAX;
        assert!(f
            .store
            .historical_adaptive_provider_project(&root, u64::MAX - 1)
            .is_err());
        f.store
            .connection
            .lock()
            .unwrap()
            .execute("DELETE FROM company_events", [])
            .unwrap();
        assert_eq!(
            f.store
                .historical_adaptive_provider_project(&f.root, CLAIMED_AT)
                .unwrap(),
            None
        );
    }

    #[test]
    fn event_and_operation_tampering_is_rejected_without_writes() {
        for sql in [
            "UPDATE company_events SET payload=CAST('{}' AS BLOB) WHERE sequence=?1",
            "UPDATE company_events SET payload_digest=?2 WHERE sequence=?1",
            "UPDATE company_events SET event_id=?2 WHERE sequence=?1",
            "UPDATE company_events SET authority_binding_digest=?2 WHERE sequence=?1",
            "UPDATE company_events SET created_at_ms=5 WHERE sequence=?1",
            "UPDATE company_events SET event_type='project_unknown' WHERE sequence=?1",
            "UPDATE company_operations SET response=CAST('{}' AS BLOB) WHERE operation_id=(SELECT operation_id FROM company_events WHERE sequence=?1)",
            "UPDATE company_operations SET response_digest=?2 WHERE operation_id=(SELECT operation_id FROM company_events WHERE sequence=?1)",
            "UPDATE company_operations SET request_digest=?2 WHERE operation_id=(SELECT operation_id FROM company_events WHERE sequence=?1)",
            "DELETE FROM company_operations WHERE operation_id=(SELECT operation_id FROM company_events WHERE sequence=?1)",
        ] {
            let f = fixture();
            {
                let connection = f.store.connection.lock().unwrap();
                if sql.contains("?2") {
                    connection
                        .execute(sql, params![sql_u64(f.sequence).unwrap(), "b".repeat(64)])
                        .unwrap();
                } else {
                    connection.execute(sql, [sql_u64(f.sequence).unwrap()]).unwrap();
                }
            }
            let before = total_changes(&f.store);
            assert_eq!(
                f.store
                    .historical_adaptive_provider_project(&f.root, CLAIMED_AT)
                    .unwrap_err()
                    .code,
                WorkflowErrorCode::CorruptStore,
                "{sql}"
            );
            assert_eq!(total_changes(&f.store), before, "{sql}");
        }
    }

    #[test]
    fn corrupt_older_snapshot_is_not_hidden_by_a_newer_match() {
        let f = fixture();
        f.store
            .connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE company_events SET payload_digest=?1 WHERE event_type='project_created'",
                ["b".repeat(64)],
            )
            .unwrap();
        assert_eq!(
            f.store
                .historical_adaptive_provider_project(&f.root, CLAIMED_AT)
                .unwrap_err()
                .code,
            WorkflowErrorCode::CorruptStore
        );
    }

    #[test]
    fn oversized_event_and_exact_operation_blobs_fail_without_writes_even_after_match() {
        for operation_blob in [false, true] {
            for after_match in [false, true] {
                let f = fixture();
                {
                    let connection = f.store.connection.lock().unwrap();
                    let sequence = if after_match {
                        connection
                            .query_row(
                                "SELECT sequence FROM company_events WHERE tenant_id=?1 AND project_id=?2 AND event_type='project_work_assigned'",
                                params![f.project.tenant_id.0, f.project.project_id.0],
                                |row| row.get::<_, i64>(0),
                            )
                            .unwrap()
                    } else {
                        sql_u64(f.sequence).unwrap()
                    };
                    let bytes = (MAX_HISTORICAL_SNAPSHOT_BLOB_BYTES + 1) as i64;
                    if operation_blob {
                        let operation_id: String = connection
                            .query_row(
                                "SELECT operation_id FROM company_events WHERE sequence=?1",
                                [sequence],
                                |row| row.get(0),
                            )
                            .unwrap();
                        assert_eq!(
                            connection
                                .execute(
                                    "UPDATE company_operations SET response=zeroblob(?1) WHERE authority_namespace=?2 AND operation_id=?3",
                                    params![bytes, f.leader.namespace(), operation_id],
                                )
                                .unwrap(),
                            1
                        );
                    } else {
                        connection
                            .execute(
                                "UPDATE company_events SET payload=zeroblob(?1) WHERE sequence=?2",
                                params![bytes, sequence],
                            )
                            .unwrap();
                    }
                }
                let before = total_changes(&f.store);
                assert_eq!(
                    f.store
                        .historical_adaptive_provider_project(&f.root, CLAIMED_AT)
                        .unwrap_err()
                        .code,
                    WorkflowErrorCode::CorruptStore,
                    "operation_blob={operation_blob}, after_match={after_match}"
                );
                assert_eq!(total_changes(&f.store), before);
            }
        }
    }

    #[test]
    fn oversized_same_operation_id_in_another_namespace_is_not_selected() {
        let f = fixture();
        {
            let connection = f.store.connection.lock().unwrap();
            let operation_id: String = connection
                .query_row(
                    "SELECT operation_id FROM company_events WHERE sequence=?1",
                    [sql_u64(f.sequence).unwrap()],
                    |row| row.get(0),
                )
                .unwrap();
            let mut other = f.leader.clone();
            other.tenant_id = TenantId::parse("other-tenant").unwrap();
            connection
                .execute(
                    "INSERT INTO company_operations(authority_namespace,operation_id,request_digest,authority_binding_digest,target_predecessor_digest,response,response_digest,created_at_ms) SELECT ?1,operation_id,request_digest,authority_binding_digest,target_predecessor_digest,zeroblob(?2),response_digest,created_at_ms FROM company_operations WHERE authority_namespace=?3 AND operation_id=?4",
                    params![
                        other.namespace(),
                        (MAX_HISTORICAL_SNAPSHOT_BLOB_BYTES + 1) as i64,
                        f.leader.namespace(),
                        operation_id,
                    ],
                )
                .unwrap();
        }
        let before = total_changes(&f.store);
        assert_eq!(
            f.store
                .historical_adaptive_provider_project(&f.root, CLAIMED_AT)
                .unwrap(),
            Some(f.project)
        );
        assert_eq!(total_changes(&f.store), before);
    }

    #[test]
    fn snapshot_byte_budget_enforces_inclusive_blob_and_total_limits() {
        let mut budget = SnapshotByteBudget::new();
        assert!(budget.charge(-1).is_err());
        assert!(budget
            .charge((MAX_HISTORICAL_SNAPSHOT_BLOB_BYTES + 1) as i64)
            .is_err());
        assert_eq!(budget.remaining, MAX_HISTORICAL_SNAPSHOT_TOTAL_BYTES);
        for _ in 0..8 {
            budget
                .charge(MAX_HISTORICAL_SNAPSHOT_BLOB_BYTES as i64)
                .unwrap();
        }
        assert_eq!(budget.remaining, 0);
        assert!(budget.charge(1).is_err());
        assert_eq!(budget.remaining, 0);
    }

    #[test]
    fn cumulative_verified_history_byte_overflow_rejects_an_earlier_match_without_writes() {
        let f = fixture();
        let mut project = f.project.clone();
        for index in 0..80_u64 {
            project = apply_project(
                &f.store,
                &f.leader,
                CompanyWorkflowCommandV1::RecordDecision {
                    project_id: project.project_id.clone(),
                    expected_version: project.version,
                    work_item_id: None,
                    choice_ref: format!("historical-choice-{index}"),
                    rationale_ref: "x".repeat(4_000),
                },
                CLAIMED_AT + index,
            );
        }
        {
            let connection = f.store.connection.lock().unwrap();
            let (event_max, event_total): (i64, i64) = connection
                .query_row(
                    "SELECT MAX(length(payload)),SUM(length(payload)) FROM company_events",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            let (response_max, response_total): (i64, i64) = connection
                .query_row(
                    "SELECT MAX(length(response)),SUM(length(response)) FROM company_operations",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert!(event_max <= MAX_HISTORICAL_SNAPSHOT_BLOB_BYTES as i64);
            assert!(response_max <= MAX_HISTORICAL_SNAPSHOT_BLOB_BYTES as i64);
            assert!(event_total + response_total > MAX_HISTORICAL_SNAPSHOT_TOTAL_BYTES as i64);
        }
        let before = total_changes(&f.store);
        assert_eq!(
            f.store
                .historical_adaptive_provider_project(&f.root, 100)
                .unwrap_err()
                .code,
            WorkflowErrorCode::CorruptStore
        );
        assert_eq!(total_changes(&f.store), before);
        assert_eq!(
            f.store
                .company_project(&project.tenant_id, &project.project_id)
                .unwrap(),
            Some(project)
        );
    }

    #[test]
    fn history_overflow_is_an_error_not_a_truncated_match() {
        let f = fixture();
        let connection = f.store.connection.lock().unwrap();
        for index in 0..MAX_HISTORICAL_PROJECT_EVENTS {
            connection.execute(
                "INSERT INTO company_events(event_id,tenant_id,project_id,event_type,operation_id,operation_digest,principal_id,principal_kind,principal_role,agent_id,customer_id,authority_generation,authority_digest,authority_binding_digest,payload,payload_digest,created_at_ms) SELECT ?1,tenant_id,project_id,event_type,operation_id,operation_digest,principal_id,principal_kind,principal_role,agent_id,customer_id,authority_generation,authority_digest,authority_binding_digest,payload,payload_digest,created_at_ms FROM company_events WHERE sequence=?2",
                params![format!("overflow-{index}"), sql_u64(f.sequence).unwrap()],
            ).unwrap();
        }
        drop(connection);
        let before = total_changes(&f.store);
        assert_eq!(
            f.store
                .historical_adaptive_provider_project(&f.root, CLAIMED_AT)
                .unwrap_err()
                .code,
            WorkflowErrorCode::InvalidInput
        );
        assert_eq!(total_changes(&f.store), before);
    }
}
