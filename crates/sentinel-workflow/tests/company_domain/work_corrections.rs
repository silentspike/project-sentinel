use super::*;
use sentinel_workflow::{
    CompletionEvidencePort, DependencyReadiness, ExecutionPlanV1, ExecutionResourceBoundsV1,
    ExecutionRevisionV1, ExecutionStepV1, ExecutionToolV1, GateEvidencePort, GateExpectationV1,
    IndependentGateEvidence, OrganizationRuntimePort, PendingCompletionEvidenceV1,
    PendingExecutionV1, PendingGateEvidenceV1, PrincipalAuthorityV1, ProjectV1,
    RuntimeAuthoritySnapshotV1, SealedArtifactEvidenceV1, SealedOutputEvidenceV1,
    TerminalExecutionEvidence, WorkCorrectionFeedbackV1, WorkExecutionObservation,
    WorkExecutionPort, WorkflowCore, WorkflowPortError,
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
    correction_fixture_with_subscription(observation, false)
}

fn correction_fixture_with_subscription(
    observation: WorkExecutionObservation,
    subscription: bool,
) -> (Journey, ProjectV1, CompanyWorkflowCommandV1) {
    let (state, mut project, grant) = super::subscription::assigned();
    if subscription {
        let command = CompanyWorkflowCommandV1::GrantSubscriptionCall {
            project_id: project.project_id.clone(),
            expected_version: project.version,
            grant,
        };
        project = project_command(&state.store, &state.pm, 840, command, 42);
        let allowance = project.subscription_call.as_ref().unwrap();
        let command = CompanyWorkflowCommandV1::ClaimSubscriptionCall {
            project_id: project.project_id.clone(),
            expected_version: project.version,
            allowance_id: allowance.allowance_id.clone(),
            request_id: format!("company-provider-{}", allowance.allowance_id),
            request_digest: DIGEST.into(),
        };
        project = project_command(&state.store, &state.developer, 841, command, 42);
    }
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
        feedback: None,
        next_subscription_grant: None,
    };
    (state, project, command)
}

#[test]
fn source_review_append_preserves_completed_work_and_replays_after_restart() {
    let (state, before, _) =
        correction_fixture_with_subscription(WorkExecutionObservation::Succeeded, true);
    let mut review = work(
        "source-review",
        CompanyRoleV1::Qa,
        &["qa"],
        &["build-work"],
        100,
    );
    review.outputs[0].media_type = "application/vnd.sentinel.qa-report+json".into();
    // OAuth call admission is separately bounded; report work cannot reserve money.
    review.budget_micros = 0;
    let command = CompanyWorkflowCommandV1::AppendSourceReview {
        project_id: before.project_id.clone(),
        expected_version: before.version,
        item: review.clone(),
    };
    for actor in [&state.developer, &state.qa, &state.customer] {
        assert!(state
            .store
            .apply_company_command(actor, Uuid::from_u128(701), &command, 60)
            .is_err());
    }
    for variant in 0..6 {
        let mut bad = review.clone();
        match variant {
            0 => bad.inputs.clear(),
            1 => bad.required_role = CompanyRoleV1::Developer,
            2 => bad.owner = AgentId(2),
            3 => bad.work_item_id = WorkItemId::parse("build-work").unwrap(),
            4 => bad.inputs[0].expected_contract_digest = OTHER_DIGEST.into(),
            _ => bad.budget_micros = 1001,
        }
        let command = CompanyWorkflowCommandV1::AppendSourceReview {
            project_id: before.project_id.clone(),
            expected_version: before.version,
            item: bad,
        };
        assert!(state
            .store
            .apply_company_command(&state.pm, Uuid::from_u128(702 + variant), &command, 60)
            .is_err());
        assert_eq!(
            state
                .store
                .company_project(&state.pm.tenant_id, &before.project_id)
                .unwrap()
                .unwrap(),
            before
        );
    }
    let after = project_command(&state.store, &state.pm, 710, command.clone(), 60);
    assert_eq!(after.lifecycle_state, ProjectLifecycleStateV1::Active);
    assert_eq!(after.work_items.len(), before.work_items.len() + 1);
    for (id, work) in &before.work_items {
        assert_eq!(&after.work_items[id], work);
    }
    assert_eq!(after.subscription_call, before.subscription_call);
    assert_eq!(after.work_corrections, before.work_corrections);
    assert_eq!(after.cost_ceiling_micros, before.cost_ceiling_micros);
    assert_eq!(
        after
            .work_items
            .values()
            .map(|work| work.spec.budget_micros)
            .sum::<u64>(),
        before
            .work_items
            .values()
            .map(|work| work.spec.budget_micros)
            .sum::<u64>()
    );
    assert_eq!(
        after.work_items[&review.work_item_id].state,
        CompanyWorkStateV1::Ready
    );
    let reopened = WorkflowStore::open(state._temp.path().join("workflow.sqlite")).unwrap();
    assert!(
        reopened
            .apply_company_command(&state.pm, Uuid::from_u128(710), &command, 61)
            .unwrap()
            .replayed
    );
    assert_eq!(
        reopened
            .company_project(&state.pm.tenant_id, &before.project_id)
            .unwrap()
            .unwrap(),
        after
    );
    let review_profile = profile("web-review-v1");
    let assign = CompanyWorkflowCommandV1::AssignSourceReview {
        project_id: after.project_id.clone(),
        expected_version: after.version,
        work_item_id: review.work_item_id.clone(),
        agent_id: AgentId(3),
        organization_generation: 1,
        organization_digest: DIGEST.into(),
        reason_ref: "source-review-profile".into(),
        profile: review_profile.clone(),
    };
    for variant in 0..4 {
        let mut bad = assign.clone();
        if let CompanyWorkflowCommandV1::AssignSourceReview {
            profile,
            reason_ref,
            agent_id,
            ..
        } = &mut bad
        {
            match variant {
                0 => profile.profile_id = "web-authoring-v1".into(),
                1 => profile.generation = 2,
                2 => *reason_ref = "ordinary-assignment".into(),
                _ => *agent_id = AgentId(2),
            }
        }
        assert!(reopened
            .apply_company_command(&state.pm, Uuid::from_u128(720 + variant), &bad, 62)
            .is_err());
    }
    let assigned = project_command(&reopened, &state.pm, 730, assign.clone(), 62);
    assert_eq!(assigned.governance, before.governance);
    assert_eq!(
        assigned.work_items[&WorkItemId::parse("build-work").unwrap()],
        before.work_items[&WorkItemId::parse("build-work").unwrap()]
    );
    assert_eq!(
        assigned.work_items[&review.work_item_id].assignments[0].profile,
        review_profile
    );
    assert!(
        reopened
            .apply_company_command(&state.pm, Uuid::from_u128(730), &assign, 63)
            .unwrap()
            .replayed
    );
    let restarted = WorkflowStore::open(state._temp.path().join("workflow.sqlite")).unwrap();
    assert_eq!(
        restarted
            .company_project(&state.pm.tenant_id, &before.project_id)
            .unwrap()
            .unwrap(),
        assigned
    );
    let previous = assigned.subscription_call.as_ref().unwrap();
    let binding = &assigned.work_items[&review.work_item_id].assignments[0];
    let mut next = previous.grant.clone();
    next.work_item_id = review.work_item_id.clone();
    next.assignment_id = binding.assignment_id.clone();
    next.assignment_version = binding.assignment_version;
    next.agent_id = binding.agent_id;
    let handoff = CompanyWorkflowCommandV1::GrantSourceReviewCall {
        project_id: assigned.project_id.clone(),
        expected_version: assigned.version,
        previous_allowance_id: previous.allowance_id.clone(),
        grant: next,
    };
    for variant in 0..4 {
        let mut bad = handoff.clone();
        if let CompanyWorkflowCommandV1::GrantSourceReviewCall {
            previous_allowance_id,
            grant,
            ..
        } = &mut bad
        {
            match variant {
                0 => *previous_allowance_id = "subscription-foreign".into(),
                1 => grant.agent_id = AgentId(2),
                2 => grant.max_calls = 65,
                _ => grant.expires_at_unix_ms = 1,
            }
        }
        assert!(restarted
            .apply_company_command(&state.pm, Uuid::from_u128(850 + variant), &bad, 64)
            .is_err());
        assert_eq!(
            restarted
                .company_project(&state.pm.tenant_id, &assigned.project_id)
                .unwrap()
                .unwrap(),
            assigned
        );
    }
    let handed = project_command(&restarted, &state.pm, 860, handoff.clone(), 64);
    assert_eq!(handed.source_review_previous_call.as_ref(), Some(previous));
    assert_ne!(
        handed.subscription_call.as_ref().unwrap().allowance_id,
        previous.allowance_id
    );
    assert_eq!(handed.work_items, assigned.work_items);
    assert_eq!(handed.work_corrections, assigned.work_corrections);
    assert!(
        restarted
            .apply_company_command(&state.pm, Uuid::from_u128(860), &handoff, 65)
            .unwrap()
            .replayed
    );
    let reopened = WorkflowStore::open(state._temp.path().join("workflow.sqlite")).unwrap();
    assert_eq!(
        reopened
            .company_project(&state.pm.tenant_id, &assigned.project_id)
            .unwrap()
            .unwrap(),
        handed
    );
    let reserve = CompanyWorkflowCommandV1::ReserveCost {
        project_id: handed.project_id.clone(),
        expected_version: handed.version,
        work_item_id: Some(previous.grant.work_item_id.clone()),
        provider: "codex-cli".into(),
        amount_micros: 1,
    };
    assert!(reopened
        .apply_company_command(&state.pm, Uuid::from_u128(861), &reserve, 65)
        .is_err());
    let mut repeated = handoff;
    if let CompanyWorkflowCommandV1::GrantSourceReviewCall {
        expected_version, ..
    } = &mut repeated
    {
        *expected_version = handed.version;
    }
    assert!(reopened
        .apply_company_command(&state.pm, Uuid::from_u128(862), &repeated, 65)
        .is_err());
    for index in 0..9_u128 {
        let CompanyWorkflowResponseV1::CustomerRequest(request) = super::command(
            &reopened,
            &state.customer,
            900 + index,
            CompanyWorkflowCommandV1::SubmitCustomerRequest {
                summary_ref: "Another website".into(),
                desired_outcome: "Landing page".into(),
                constraints: vec![],
            },
            66,
        ) else {
            panic!()
        };
        let operator = AuthenticatedCompanyPrincipalV1 {
            principal_id: "operator-test".into(),
            kind: CompanyPrincipalKindV1::Operator,
            agent_id: None,
            ..state.pm.clone()
        };
        let request_grant = sentinel_workflow::RequestProviderGrantV1 {
            schema_version: 1,
            request_id: request.request_id,
            expected_version: 1,
            sales_principal: principal(
                "tenant-a",
                "sales-test",
                CompanyPrincipalKindV1::Agent,
                CompanyRoleV1::Sales,
                None,
                Some(10),
            ),
            provider: "codex-cli".into(),
            model: "model-test".into(),
            catalog_digest: DIGEST.into(),
            total_call_limit: 10,
            concurrent_call_limit: 1,
            max_duration_ms: 120_000,
            token_policy:
                sentinel_workflow::SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,
            expires_at_unix_ms: 300_000,
        };
        // Both current QA and archived developer grants consume the campaign budget.
        let result = reopened.authorize_request_provider_call(
            &operator,
            Uuid::from_u128(920 + index),
            &request_grant,
            67,
        );
        if index < 8 {
            result.unwrap();
        } else {
            assert_eq!(
                result.unwrap_err().message,
                "request provider allowance exhausted or already reserved"
            );
        }
    }
}

#[test]
fn company_correction_feedback_is_bound_to_artifact_and_survives_restart() {
    let (state, previous, mut command) = correction_fixture(WorkExecutionObservation::Succeeded);
    let work_id = WorkItemId::parse("build-work").unwrap();
    let report = WorkCorrectionFeedbackV1 {
        summary: "Browser rejected two invalid CSS dimensions; preserve working timer controls."
            .into(),
        artifact_digest: Some(
            previous.work_items[&work_id].output_receipts[0]
                .content_digest
                .clone(),
        ),
    };
    if let CompanyWorkflowCommandV1::RequestWorkCorrection {
        feedback,
        execution_revision,
        ..
    } = &mut command
    {
        *feedback = Some(report.clone());
        execution_revision.feedback_digest = report.canonical_digest().unwrap();
    }
    for variant in 0..6 {
        let mut changed = command.clone();
        if let CompanyWorkflowCommandV1::RequestWorkCorrection {
            feedback: Some(feedback),
            execution_revision,
            ..
        } = &mut changed
        {
            match variant {
                0 => feedback.summary.push_str(" changed"),
                1 => feedback.artifact_digest = Some("f".repeat(64)),
                2 => feedback.artifact_digest = None,
                3 => feedback.summary = " ".into(),
                4 => feedback.summary = "x".repeat(4097),
                _ => feedback.summary = "control\u{0000}text".into(),
            }
            if variant == 1 || variant == 2 {
                execution_revision.feedback_digest = feedback.canonical_digest().unwrap();
            }
        }
        assert!(state
            .store
            .apply_company_command(&state.pm, Uuid::from_u128(51), &changed, 51)
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
    let corrected = project_command(&state.store, &state.pm, 51, command.clone(), 51);
    assert_eq!(
        corrected.work_corrections[0].feedback.as_ref(),
        Some(&report)
    );
    let reopened = WorkflowStore::open(state._temp.path().join("workflow.sqlite")).unwrap();
    assert_eq!(
        reopened
            .company_project(&state.pm.tenant_id, &state.project_id)
            .unwrap()
            .unwrap(),
        corrected
    );
    assert!(
        reopened
            .apply_company_command(&state.pm, Uuid::from_u128(51), &command, 52)
            .unwrap()
            .replayed
    );
    let mut changed = command.clone();
    if let CompanyWorkflowCommandV1::RequestWorkCorrection {
        feedback: Some(report),
        ..
    } = &mut changed
    {
        report.summary.push_str(" altered");
    }
    assert_eq!(
        reopened
            .apply_company_command(&state.pm, Uuid::from_u128(51), &changed, 52)
            .unwrap_err()
            .code,
        WorkflowErrorCode::IdempotencyConflict
    );
    let mut corrupted = corrected;
    corrupted.work_corrections[0]
        .feedback
        .as_mut()
        .unwrap()
        .summary
        .push_str(" altered");
    let db = rusqlite::Connection::open(state._temp.path().join("workflow.sqlite")).unwrap();
    assert_eq!(
        db.execute(
            "UPDATE company_entities SET payload=?1 WHERE entity_kind='project' AND entity_id=?2",
            rusqlite::params![serde_json::to_vec(&corrupted).unwrap(), state.project_id.0]
        )
        .unwrap(),
        1
    );
    assert!(reopened
        .company_project(&state.pm.tenant_id, &state.project_id)
        .is_err());
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
