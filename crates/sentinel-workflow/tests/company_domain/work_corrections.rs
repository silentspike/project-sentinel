use super::*;
use sentinel_workflow::{
    CompletionEvidencePort, DependencyReadiness, ExecutionPlanV1, ExecutionResourceBoundsV1,
    ExecutionRevisionV1, ExecutionStepV1, ExecutionToolV1, GateEvidencePort, GateExpectationV1,
    IndependentGateEvidence, OrganizationRuntimePort, PendingCompletionEvidenceV1,
    PendingExecutionV1, PendingGateEvidenceV1, PrincipalAuthorityV1, ProjectV1,
    RuntimeAuthoritySnapshotV1, SealedArtifactEvidenceV1, SealedOutputEvidenceV1,
    TerminalExecutionEvidence, WorkExecutionObservation, WorkExecutionPort, WorkflowCore,
    WorkflowPortError,
};

struct Organization(RuntimeAuthoritySnapshotV1);
impl OrganizationRuntimePort for Organization {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Ready
    }
    fn authority_snapshot(
        &self,
        tenant: &TenantId,
        project: &ProjectId,
        work: &WorkItemId,
        agent: AgentId,
    ) -> Result<RuntimeAuthoritySnapshotV1, WorkflowPortError> {
        assert_eq!(
            (tenant, project, work, agent),
            (
                &self.0.tenant_id,
                &self.0.project_id,
                &self.0.work_item_id,
                self.0.agent_id
            )
        );
        Ok(self.0.clone())
    }
}
struct Execution(WorkExecutionObservation);
impl WorkExecutionPort for Execution {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Ready
    }
    fn reconcile(
        &self,
        _: &PendingExecutionV1,
    ) -> Result<WorkExecutionObservation, WorkflowPortError> {
        Ok(self.0)
    }
}

struct Evidence;
struct Completion {
    request: PendingCompletionEvidenceV1,
    outputs: Vec<SealedOutputEvidenceV1>,
    digest: String,
}
impl TerminalExecutionEvidence for Completion {
    fn schema_version(&self) -> u16 {
        1
    }
    fn receipt_id(&self) -> &str {
        "test-root"
    }
    fn invocation_id(&self) -> Uuid {
        self.request.invocation_id
    }
    fn plan_digest(&self) -> &str {
        &self.request.plan_digest
    }
    fn step_digest(&self) -> &str {
        &self.request.step_digest
    }
    fn output_bundle_digest(&self) -> &str {
        &self.digest
    }
    fn outputs(&self) -> &[SealedOutputEvidenceV1] {
        &self.outputs
    }
    fn artifacts(&self) -> &[SealedArtifactEvidenceV1] {
        &[]
    }
    fn completed_at_unix_ms(&self) -> u64 {
        self.request.created_at_unix_ms + 1
    }
}
impl CompletionEvidencePort for Evidence {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Ready
    }
    fn terminal_evidence(
        &self,
        request: &PendingCompletionEvidenceV1,
    ) -> Result<Box<dyn TerminalExecutionEvidence>, WorkflowPortError> {
        let outputs = vec![SealedOutputEvidenceV1 {
            name: "source".into(),
            kind: "source_tree".into(),
            digest_algorithm: "sha256".into(),
            digest: OTHER_DIGEST.into(),
        }];
        let digest = sentinel_workflow::sealed_output_bundle_digest(&outputs, &[]).unwrap();
        Ok(Box::new(Completion {
            request: request.clone(),
            outputs,
            digest,
        }))
    }
}
struct Gate(PendingGateEvidenceV1);
impl IndependentGateEvidence for Gate {
    fn schema_version(&self) -> u16 {
        1
    }
    fn receipt_id(&self) -> &str {
        "test-gate"
    }
    fn profile_id(&self) -> &str {
        &self.0.expectation.profile_id
    }
    fn profile_generation(&self) -> u64 {
        self.0.expectation.profile_generation
    }
    fn profile_digest(&self) -> &str {
        &self.0.expectation.profile_digest
    }
    fn subject_digest(&self) -> &str {
        &self.0.subject_digest
    }
    fn required_checks_digest(&self) -> &str {
        &self.0.required_checks_digest
    }
    fn passed(&self) -> bool {
        true
    }
    fn completed_at_unix_ms(&self) -> u64 {
        self.0.created_at_unix_ms + 1
    }
}
impl GateEvidencePort for Evidence {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Ready
    }
    fn gate_evidence(
        &self,
        request: &PendingGateEvidenceV1,
    ) -> Result<Box<dyn IndependentGateEvidence>, WorkflowPortError> {
        Ok(Box::new(Gate(request.clone())))
    }
}

fn correction_fixture(
    observation: WorkExecutionObservation,
) -> (Journey, ProjectV1, CompanyWorkflowCommandV1) {
    let (state, mut project, _) = super::subscription::assigned();
    let work_id = WorkItemId::parse("build-work").unwrap();
    let assignment = &project.work_items[&work_id].assignments[0];
    let authority = RuntimeAuthoritySnapshotV1 {
        schema_version: 1,
        tenant_id: state.pm.tenant_id.clone(),
        project_id: project.project_id.clone(),
        work_item_id: work_id.clone(),
        agent_id: assignment.agent_id,
        assignment_version: assignment.assignment_version,
        assignment_digest: assignment.canonical_digest().unwrap(),
        organization_generation: assignment.organization_generation,
        organization_digest: assignment.organization_digest.clone(),
        principal: PrincipalAuthorityV1::derive("developer-a", 1, &[0x5a; 32]).unwrap(),
        profile_id: assignment.profile.profile_id.clone(),
        profile_generation: assignment.profile.generation,
        profile_digest: assignment.profile.digest.clone(),
        runtime_key: "bwrap-test".into(),
        runtime_generation: 1,
        runtime_digest: DIGEST.into(),
        policy_generation: 1,
        policy_digest: DIGEST.into(),
        active: true,
        capabilities: BTreeSet::from(["file.write".to_owned()]),
    };
    let workspace = format!("{}:{}", project.project_id.0, work_id.0);
    let plan = ExecutionPlanV1 {
        schema_version: 1,
        plan_id: Uuid::from_u128(100),
        tenant_id: authority.tenant_id.clone(),
        project_id: authority.project_id.clone(),
        work_item_id: work_id.clone(),
        agent_id: authority.agent_id,
        workspace_id: workspace.clone(),
        assignment_version: authority.assignment_version,
        assignment_digest: authority.assignment_digest.clone(),
        organization_generation: authority.organization_generation,
        organization_digest: authority.organization_digest.clone(),
        principal: authority.principal.clone(),
        profile_id: authority.profile_id.clone(),
        profile_generation: authority.profile_generation,
        profile_digest: authority.profile_digest.clone(),
        runtime_key: authority.runtime_key.clone(),
        runtime_generation: 1,
        runtime_digest: DIGEST.into(),
        policy_generation: 1,
        policy_digest: DIGEST.into(),
        created_at_unix_ms: 43,
        deadline_unix_ms: 10_000,
        request_digest: String::new(),
        steps: vec![ExecutionStepV1 {
            step_id: Uuid::from_u128(101),
            invocation_id: Uuid::from_u128(102),
            ordinal: 0,
            workspace_id: workspace,
            capabilities: authority.capabilities.clone(),
            inputs: vec![],
            command_policy: vec![],
            tool: ExecutionToolV1::WriteFile {
                path: "index.html".into(),
                content: "test".into(),
                expected_sha256: None,
            },
            outputs: vec![sentinel_workflow::OutputExpectationV1 {
                name: "source".into(),
                kind: "source_tree".into(),
                required: true,
                digest_algorithm: "sha256".into(),
            }],
            artifacts: vec![],
            gate_expectation: GateExpectationV1 {
                profile_id: "web-work-item-qa-v1".into(),
                profile_generation: 1,
                profile_digest: DIGEST.into(),
                required_checks: BTreeSet::from(["check".to_owned()]),
            },
            resource_bounds: ExecutionResourceBoundsV1 {
                wall_time_ms: 1000,
                cpu_time_ms: 1000,
                memory_bytes: 1024 * 1024,
                process_count: 1,
                file_bytes: 1024,
                stdout_bytes: 1024,
                stderr_bytes: 1024,
            },
            deadline_unix_ms: 10_000,
        }],
    }
    .bind_digest()
    .unwrap();
    let core = WorkflowCore::new(
        WorkflowStore::open(state._temp.path().join("workflow.sqlite")).unwrap(),
        Organization(authority),
        Execution(observation),
        Evidence,
        Evidence,
    );
    core.admit_plan(&plan, 43).unwrap();
    let pending = core.store().pending_executions(1).unwrap().remove(0);
    let mut execution = core.reconcile_execution(&pending, 44).unwrap();
    if observation == WorkExecutionObservation::Succeeded {
        let completion = core
            .store()
            .pending_completion_evidence(1)
            .unwrap()
            .remove(0);
        core.reconcile_completion_evidence(&completion, 45).unwrap();
        let gate = core.store().pending_gate_evidence(1).unwrap().remove(0);
        execution = core.reconcile_gate_evidence(&gate, 47).unwrap();
        for (index, (from, to)) in [
            (CompanyWorkStateV1::Assigned, CompanyWorkStateV1::InProgress),
            (CompanyWorkStateV1::InProgress, CompanyWorkStateV1::InReview),
            (CompanyWorkStateV1::InReview, CompanyWorkStateV1::Done),
        ]
        .into_iter()
        .enumerate()
        {
            let timestamp = 48 + index as u64;
            let done = to == CompanyWorkStateV1::Done;
            let gate = done.then(|| QualityGateReceiptBindingV1 {
                gate_id: "web-work-item-qa-v1".into(),
                generation: 1,
                gate_digest: DIGEST.into(),
                subject_digest: execution
                    .gate_evidence
                    .as_ref()
                    .unwrap()
                    .subject_digest
                    .clone(),
                passed: true,
            });
            let receipt = if to == CompanyWorkStateV1::InProgress {
                vec![]
            } else {
                output_receipt()
            };
            project = project_command(
                &state.store,
                if done { &state.qa } else { &state.developer },
                u128::from(timestamp),
                transition(
                    &project.project_id,
                    project.version,
                    "build-work",
                    2 + index as u64,
                    1,
                    from,
                    to,
                    receipt,
                    gate,
                    timestamp,
                ),
                timestamp,
            );
        }
    } else {
        project = project_command(
            &state.store,
            &state.developer,
            45,
            transition(
                &project.project_id,
                project.version,
                "build-work",
                2,
                1,
                CompanyWorkStateV1::Assigned,
                CompanyWorkStateV1::Blocked,
                vec![],
                None,
                45,
            ),
            45,
        );
    }
    let command = CompanyWorkflowCommandV1::RequestWorkCorrection {
        project_id: project.project_id.clone(),
        expected_version: project.version,
        work_item_id: work_id,
        expected_work_version: project.work_items[&WorkItemId::parse("build-work").unwrap()]
            .version,
        execution_revision: ExecutionRevisionV1::from_completed_work(
            &execution,
            OTHER_DIGEST.into(),
        )
        .unwrap(),
        feedback_ref: "qa-result-1".into(),
        next_subscription_grant: None,
    };
    (state, project, command)
}

#[test]
fn company_correction_archives_completed_outputs_and_allows_later_reassignment() {
    let (state, previous, request) = correction_fixture(WorkExecutionObservation::Succeeded);
    let id = WorkItemId::parse("build-work").unwrap();
    let corrected = project_command(&state.store, &state.pm, 51, request.clone(), 51);
    assert_eq!(
        corrected.work_corrections[0].previous,
        previous.work_items[&id]
    );
    assert!(corrected.work_items[&id].output_receipts.is_empty());
    assert!(corrected.work_items[&id].gate_receipt.is_none());
    assert_eq!(corrected.lifecycle_state, ProjectLifecycleStateV1::Active);
    let reassigned = project_command(
        &state.store,
        &state.pm,
        52,
        CompanyWorkflowCommandV1::ReassignWork {
            project_id: corrected.project_id.clone(),
            expected_version: corrected.version,
            work_item_id: id.clone(),
            expected_assignment_version: 1,
            agent_id: AgentId(2),
            organization_generation: 1,
            organization_digest: DIGEST.into(),
            reason_ref: "new-assignment".into(),
        },
        52,
    );
    assert_eq!(reassigned.work_corrections, corrected.work_corrections);
    let reopened = WorkflowStore::open(state._temp.path().join("workflow.sqlite")).unwrap();
    assert_eq!(
        reopened
            .company_project(&state.pm.tenant_id, &state.project_id)
            .unwrap()
            .unwrap(),
        reassigned
    );
    assert!(reopened
        .apply_company_command(&state.pm, Uuid::from_u128(53), &request, 53)
        .is_err());
}

#[test]
fn company_correction_preserves_identity_history_and_old_provider_authority() {
    let (state, previous, command) = correction_fixture(WorkExecutionObservation::Failed);
    let response = state
        .store
        .apply_company_command(&state.pm, Uuid::from_u128(46), &command, 46)
        .unwrap();
    let CompanyWorkflowResponseV1::Project(project) = &response.response else {
        panic!()
    };
    let id = WorkItemId::parse("build-work").unwrap();
    assert_eq!(project.work_items.len(), 1);
    assert_eq!(project.work_items[&id].state, CompanyWorkStateV1::Assigned);
    assert_eq!(
        project.work_items[&id].version,
        previous.work_items[&id].version + 1
    );
    assert_eq!(
        project.work_corrections[0].previous,
        previous.work_items[&id]
    );
    assert_eq!(
        project.work_items[&id].assignments,
        previous.work_items[&id].assignments
    );
    assert_eq!(project.subscription_call, previous.subscription_call);
    let reopened = WorkflowStore::open(state._temp.path().join("workflow.sqlite")).unwrap();
    assert_eq!(
        reopened
            .apply_company_command(&state.pm, Uuid::from_u128(46), &command, 50)
            .unwrap()
            .response,
        response.response
    );
    assert_eq!(
        reopened
            .company_project(&state.pm.tenant_id, &state.project_id)
            .unwrap()
            .unwrap(),
        **project
    );
    let mut changed = command.clone();
    if let CompanyWorkflowCommandV1::RequestWorkCorrection { feedback_ref, .. } = &mut changed {
        *feedback_ref = "different-qa".into();
    }
    assert_eq!(
        reopened
            .apply_company_command(&state.pm, Uuid::from_u128(46), &changed, 50)
            .unwrap_err()
            .code,
        WorkflowErrorCode::IdempotencyConflict
    );
}

#[test]
fn company_correction_rejects_unresolved_execution_and_stale_or_foreign_authority() {
    for observation in [
        WorkExecutionObservation::UnknownOutcome,
        WorkExecutionObservation::TimedOut,
        WorkExecutionObservation::Executing,
    ] {
        let (state, previous, command) = correction_fixture(observation);
        assert!(state
            .store
            .apply_company_command(&state.pm, Uuid::from_u128(46), &command, 46)
            .is_err());
        assert_eq!(
            state
                .store
                .company_project(&state.pm.tenant_id, &state.project_id)
                .unwrap()
                .unwrap(),
            previous
        );
    }
    let (state, previous, command) = correction_fixture(WorkExecutionObservation::Failed);
    for principal in [&state.customer, &state.developer, &state.qa] {
        assert!(state
            .store
            .apply_company_command(principal, Uuid::from_u128(46), &command, 46)
            .is_err());
    }
    let mut stale = command.clone();
    if let CompanyWorkflowCommandV1::RequestWorkCorrection {
        execution_revision, ..
    } = &mut stale
    {
        execution_revision.previous_state_digest = DIGEST.into();
    }
    assert!(state
        .store
        .apply_company_command(&state.pm, Uuid::from_u128(46), &stale, 46)
        .is_err());
    assert_eq!(
        state
            .store
            .company_project(&state.pm.tenant_id, &state.project_id)
            .unwrap()
            .unwrap(),
        previous
    );
}
