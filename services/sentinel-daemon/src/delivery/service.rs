use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    digest::ContentDigest,
    error::DeliveryError,
    ports::{
        AdapterReadiness, CandidateAuthorityQueryV1, DeliveryIntegrationPort,
        DeliveryPublicationPort, WorkbenchEvidenceReceiptV1, WorkbenchEvidenceRequestV1,
    },
    schema::{
        AcceptanceV1, ApprovalV1, AuthorityRole, CandidateState, CustomerAction,
        CustomerFeedbackV1, DeliveryReceiptV1, DeliveryState, FindingV1, PrincipalV1,
        ProjectCloseoutV1, QaEvaluationPlanV1, QaEvaluationRunReceiptV1, QaReleaseGateReceiptV1,
        QaRunState, ReleaseCandidateV1, ReleaseManifestV1, ReleaseState, ReleaseV1, ReviewV1,
        RollbackV1, TestRunV1, VersionedRefV1,
    },
    state::{
        transition_candidate, transition_delivery, transition_qa_run, transition_release,
        DeliveryAggregateV1,
    },
    store::{DeliveryCommitReceiptV1, DeliveryCommitRequestV1, DeliveryStore},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandContextV1 {
    pub principal: PrincipalV1,
    pub idempotency_key: String,
    pub now_ms: u64,
}

pub struct DeliveryCore<I> {
    store: DeliveryStore,
    integration: I,
}

impl<I: DeliveryIntegrationPort> DeliveryCore<I> {
    pub fn new(store: DeliveryStore, integration: I) -> Self {
        Self { store, integration }
    }

    pub fn readiness(&self) -> AdapterReadiness {
        self.integration.readiness()
    }

    pub fn load(
        &self,
        tenant_id: &str,
        project_id: &str,
    ) -> Result<Option<DeliveryAggregateV1>, DeliveryError> {
        self.store.load(tenant_id, project_id)
    }

    pub fn register_candidate(
        &self,
        context: &CommandContextV1,
        candidate: ReleaseCandidateV1,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        require_role(context, AuthorityRole::Developer)?;
        require_tenant(context, &candidate.tenant_id)?;
        if candidate.schema_version != super::schema::DELIVERY_SCHEMA_V1
            || candidate.state != CandidateState::Draft
            || candidate.candidate_digest != candidate.computed_digest()?
        {
            return Err(DeliveryError::Validation(
                "candidate schema, state, or canonical digest is invalid".to_string(),
            ));
        }
        let command_digest = command_digest(context, &candidate)?;
        if let Some(receipt) = self.existing(
            context,
            "register_candidate",
            &candidate.tenant_id,
            &command_digest,
        )? {
            return Ok(receipt);
        }
        self.require_integration()?;
        let authority = self
            .integration
            .candidate_authority(&CandidateAuthorityQueryV1 {
                tenant_id: candidate.tenant_id.clone(),
                agreement: candidate.agreement.clone(),
                project: candidate.project.clone(),
                work_items_digest: candidate.work_items_digest.clone(),
                candidate_digest: candidate.candidate_digest.clone(),
            })?;
        if authority.agreement != candidate.agreement
            || authority.project != candidate.project
            || authority.work_items_digest != candidate.work_items_digest
            || authority.current_candidate_generation != candidate.generation
            || authority.current_candidate_digest != candidate.candidate_digest
            || authority.snapshot_digest != authority.computed_digest()?
        {
            return Err(DeliveryError::StaleEvidence(
                "workflow authority does not match the candidate".to_string(),
            ));
        }
        let mut aggregate = self
            .store
            .load(&candidate.tenant_id, &candidate.project.id)?
            .unwrap_or_else(|| {
                DeliveryAggregateV1::new(&candidate.tenant_id, &candidate.project.id)
            });
        if aggregate.candidates.contains_key(&candidate.candidate_id) {
            return Err(DeliveryError::Conflict(format!(
                "candidate {} already exists",
                candidate.candidate_id
            )));
        }
        let expected_revision = aggregate.revision;
        aggregate
            .candidates
            .insert(candidate.candidate_id.clone(), candidate.clone());
        aggregate.revision += 1;
        self.commit(
            context,
            "register_candidate",
            command_digest,
            aggregate,
            "delivery_candidate_registered_v1",
            json!({
                "candidate_id": candidate.candidate_id,
                "generation": candidate.generation,
                "candidate_digest": candidate.candidate_digest,
                "authority_snapshot_digest": authority.snapshot_digest,
            }),
            expected_revision,
        )
    }

    pub fn assign_qa(
        &self,
        context: &CommandContextV1,
        tenant_id: &str,
        project_id: &str,
        candidate_id: &str,
        plan: QaEvaluationPlanV1,
        run: QaEvaluationRunReceiptV1,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        require_role(context, AuthorityRole::ReleaseManager)?;
        require_tenant(context, tenant_id)?;
        let command_digest =
            command_digest(context, &(tenant_id, project_id, candidate_id, &plan, &run))?;
        if let Some(receipt) = self.existing(context, "assign_qa", tenant_id, &command_digest)? {
            return Ok(receipt);
        }
        self.require_integration()?;
        if run.state != QaRunState::Planned
            || run.actors.len() != 1
            || plan.candidate.id != candidate_id
            || run.plan.id != plan.plan_id
            || run.plan.generation != plan.generation
            || run.plan.digest != plan.plan_digest
            || plan.plan_digest != plan.computed_digest()?
        {
            return Err(DeliveryError::Validation(
                "QA assignment requires one planned QA actor".to_string(),
            ));
        }
        let qa = &run.actors[0];
        require_principal_role(qa, AuthorityRole::Qa)?;
        require_same_tenant(&context.principal, qa)?;
        if qa.principal_id == context.principal.principal_id {
            return Err(DeliveryError::AuthorityDenied(
                "release manager cannot assign itself as QA".to_string(),
            ));
        }
        let mut aggregate = self.required_aggregate(tenant_id, project_id)?;
        let candidate = aggregate
            .candidates
            .get_mut(candidate_id)
            .ok_or_else(|| DeliveryError::NotFound(format!("candidate {candidate_id}")))?;
        if candidate
            .implementer_principal_ids
            .contains(&qa.principal_id)
            || candidate
                .implementer_principal_ids
                .contains(&context.principal.principal_id)
        {
            return Err(DeliveryError::AuthorityDenied(
                "developer, QA, and release authorities must be distinct".to_string(),
            ));
        }
        if plan.candidate.generation != candidate.generation
            || plan.candidate.digest != candidate.candidate_digest
            || plan.agreement != candidate.agreement
            || plan.project != candidate.project
            || plan.work_items_digest != candidate.work_items_digest
            || plan.acceptance_criteria_digest != candidate.acceptance_criteria_digest
        {
            return Err(DeliveryError::StaleEvidence(
                "QA plan is not bound to the exact candidate".to_string(),
            ));
        }
        transition_candidate(candidate.state, CandidateState::QaAssigned)?;
        candidate.state = CandidateState::QaAssigned;
        if aggregate
            .qa_plans
            .insert(plan.plan_id.clone(), plan.clone())
            .is_some()
        {
            return Err(DeliveryError::Conflict(format!(
                "QA plan {} already exists",
                plan.plan_id
            )));
        }
        if aggregate
            .qa_runs
            .insert(run.run_id.clone(), run.clone())
            .is_some()
        {
            return Err(DeliveryError::Conflict(format!(
                "QA run {} already exists",
                run.run_id
            )));
        }
        let expected_revision = aggregate.revision;
        aggregate.revision += 1;
        self.commit(
            context,
            "assign_qa",
            command_digest,
            aggregate,
            "qa_assigned_v1",
            json!({
                "candidate_id": candidate_id,
                "run_id": run.run_id,
                "plan_id": plan.plan_id,
                "plan_digest": plan.plan_digest,
                "qa_principal_id": qa.principal_id,
                "authority_generation": qa.authority_generation,
            }),
            expected_revision,
        )
    }

    pub fn transition_qa(
        &self,
        context: &CommandContextV1,
        tenant_id: &str,
        project_id: &str,
        run_id: &str,
        next: QaRunState,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        require_role(context, AuthorityRole::Qa)?;
        require_tenant(context, tenant_id)?;
        let command_digest = command_digest(context, &(tenant_id, project_id, run_id, next))?;
        if let Some(receipt) =
            self.existing(context, "transition_qa", tenant_id, &command_digest)?
        {
            return Ok(receipt);
        }
        let mut aggregate = self.required_aggregate(tenant_id, project_id)?;
        let run = aggregate
            .qa_runs
            .get_mut(run_id)
            .ok_or_else(|| DeliveryError::NotFound(format!("QA run {run_id}")))?;
        if !run
            .actors
            .iter()
            .any(|actor| actor.principal_id == context.principal.principal_id)
        {
            return Err(DeliveryError::AuthorityDenied(
                "only the assigned QA principal may transition the run".to_string(),
            ));
        }
        transition_qa_run(run.state, next)?;
        if next == QaRunState::CompletedPass
            && (run.harness_outcome.as_deref() != Some("pass")
                || run.cleanup_receipt.is_none()
                || run
                    .aggregate_outcomes
                    .get("required_cases_complete")
                    .map(String::as_str)
                    != Some("true")
                || ["contaminated", "needs_human_review", "flaky_unresolved"]
                    .iter()
                    .any(|key| {
                        run.aggregate_outcomes.get(*key).map(String::as_str) != Some("false")
                    }))
        {
            return Err(DeliveryError::MissingEvidence(
                "completed-pass requires exact clean workbench evidence".to_string(),
            ));
        }
        run.state = next;
        if next == QaRunState::Running {
            run.started_at_ms.get_or_insert(context.now_ms);
        }
        if next.is_terminal() {
            run.finished_at_ms.get_or_insert(context.now_ms);
        }
        let candidate_id = aggregate
            .qa_plans
            .get(&run.plan.id)
            .map(|plan| plan.candidate.id.clone())
            .ok_or_else(|| DeliveryError::CorruptStore("QA plan missing".to_string()))?;
        if let Some(candidate) = aggregate.candidates.get_mut(&candidate_id) {
            if next == QaRunState::Running && candidate.state == CandidateState::QaAssigned {
                transition_candidate(candidate.state, CandidateState::QaRunning)?;
                candidate.state = CandidateState::QaRunning;
            }
        }
        let expected_revision = aggregate.revision;
        aggregate.revision += 1;
        self.commit(
            context,
            "transition_qa",
            command_digest,
            aggregate,
            "qa_run_transitioned_v1",
            json!({"run_id": run_id, "state": next}),
            expected_revision,
        )
    }

    /// Executes the stable QA request outside a redb writer transaction and then
    /// atomically records the opaque workbench receipt. The productive #694 adapter
    /// must deduplicate the request digest across caller restart.
    pub fn execute_qa(
        &self,
        context: &CommandContextV1,
        tenant_id: &str,
        project_id: &str,
        run_id: &str,
    ) -> Result<(DeliveryCommitReceiptV1, WorkbenchEvidenceReceiptV1), DeliveryError> {
        require_role(context, AuthorityRole::Qa)?;
        require_tenant(context, tenant_id)?;
        let mut aggregate = self.required_aggregate(tenant_id, project_id)?;
        let run = aggregate
            .qa_runs
            .get(run_id)
            .ok_or_else(|| DeliveryError::NotFound(format!("QA run {run_id}")))?
            .clone();
        if run.state != QaRunState::Running
            || !run
                .actors
                .iter()
                .any(|actor| actor.principal_id == context.principal.principal_id)
        {
            return Err(DeliveryError::AuthorityDenied(
                "only assigned QA may execute a running plan".to_string(),
            ));
        }
        let plan = aggregate
            .qa_plans
            .get(&run.plan.id)
            .ok_or_else(|| DeliveryError::CorruptStore("QA plan missing".to_string()))?
            .clone();
        let request = WorkbenchEvidenceRequestV1 {
            tenant_id: tenant_id.to_string(),
            project: plan.project.clone(),
            candidate: plan.candidate.clone(),
            qa_plan: run.plan.clone(),
            request_digest: ContentDigest::zero(),
        }
        .seal()?;
        let command_digest = command_digest(context, &(tenant_id, project_id, run_id, &request))?;
        if let Some(existing) = self.existing(context, "execute_qa", tenant_id, &command_digest)? {
            let receipt = aggregate
                .workbench_receipts
                .values()
                .find(|receipt| receipt.input_digest == request.request_digest)
                .cloned()
                .ok_or_else(|| {
                    DeliveryError::CorruptStore(
                        "idempotent QA commit has no matching receipt".to_string(),
                    )
                })?;
            return Ok((existing, receipt));
        }
        self.require_integration()?;

        // The external effect occurs only after the durable running state exists and
        // no database writer is held.
        let receipt = self.integration.execute_qa(&request)?;
        if receipt.receipt_digest != receipt.computed_digest()?
            || receipt.input_digest != request.request_digest
            || receipt.output_digest == ContentDigest::zero()
            || receipt.result_inventory_digest == ContentDigest::zero()
            || receipt.logs_digest == ContentDigest::zero()
            || receipt.failure_classification_digest == ContentDigest::zero()
        {
            return Err(DeliveryError::StaleEvidence(
                "workbench receipt is not bound to the stable QA request".to_string(),
            ));
        }
        if aggregate
            .workbench_receipts
            .insert(receipt.invocation.id.clone(), receipt.clone())
            .is_some()
        {
            return Err(DeliveryError::Conflict(format!(
                "workbench invocation {} already exists",
                receipt.invocation.id
            )));
        }
        let run_mut = aggregate
            .qa_runs
            .get_mut(run_id)
            .ok_or_else(|| DeliveryError::CorruptStore("QA run disappeared".to_string()))?;
        run_mut.attempts = run_mut.attempts.saturating_add(1);
        run_mut.cleanup_receipt = Some(receipt.cleanup_receipt.clone());
        run_mut.harness_outcome = Some(if receipt.passed {
            "pass".to_string()
        } else {
            "fail".to_string()
        });
        run_mut.aggregate_outcomes.insert(
            "required_cases_complete".to_string(),
            receipt.required_cases_complete.to_string(),
        );
        run_mut
            .aggregate_outcomes
            .insert("contaminated".to_string(), receipt.contaminated.to_string());
        run_mut.aggregate_outcomes.insert(
            "needs_human_review".to_string(),
            receipt.needs_human_review.to_string(),
        );
        run_mut.aggregate_outcomes.insert(
            "flaky_unresolved".to_string(),
            receipt.flaky_unresolved.to_string(),
        );
        let expected_revision = aggregate.revision;
        aggregate.revision += 1;
        let commit = self.commit(
            context,
            "execute_qa",
            command_digest,
            aggregate,
            "qa_workbench_evidence_recorded_v1",
            json!({
                "run_id": run_id,
                "request_digest": request.request_digest,
                "invocation": receipt.invocation,
                "receipt_digest": receipt.receipt_digest,
            }),
            expected_revision,
        )?;
        Ok((commit, receipt))
    }

    pub fn record_gate(
        &self,
        context: &CommandContextV1,
        tenant_id: &str,
        project_id: &str,
        run_id: &str,
        gate: QaReleaseGateReceiptV1,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        require_role(context, AuthorityRole::Qa)?;
        require_tenant(context, tenant_id)?;
        let command_digest = command_digest(context, &(tenant_id, project_id, run_id, &gate))?;
        if let Some(receipt) = self.existing(context, "record_gate", tenant_id, &command_digest)? {
            return Ok(receipt);
        }
        if gate.actor.principal_id != context.principal.principal_id
            || gate.actor.authority_generation != context.principal.authority_generation
            || gate.issued_at_ms > context.now_ms
            || gate.expires_at_ms <= context.now_ms
        {
            return Err(DeliveryError::AuthorityDenied(
                "gate actor or validity window is not authoritative".to_string(),
            ));
        }
        let mut aggregate = self.required_aggregate(tenant_id, project_id)?;
        let run = aggregate
            .qa_runs
            .get(run_id)
            .ok_or_else(|| DeliveryError::NotFound(format!("QA run {run_id}")))?;
        if run.state != QaRunState::CompletedPass || run.plan != gate.plan {
            return Err(DeliveryError::MissingEvidence(
                "gate requires the exact completed-pass QA plan".to_string(),
            ));
        }
        if run.harness_outcome.as_deref() != Some("pass")
            || run.cleanup_receipt.is_none()
            || run
                .aggregate_outcomes
                .get("required_cases_complete")
                .map(String::as_str)
                != Some("true")
            || ["contaminated", "needs_human_review", "flaky_unresolved"]
                .iter()
                .any(|key| run.aggregate_outcomes.get(*key).map(String::as_str) != Some("false"))
            || gate.case_inventory_digest == ContentDigest::zero()
            || gate.deterministic_evidence_digest == ContentDigest::zero()
            || gate.calibration_digest == ContentDigest::zero()
            || gate.source_evidence_digest == ContentDigest::zero()
        {
            return Err(DeliveryError::MissingEvidence(
                "gate rejects incomplete, contaminated, review-required, flaky, or empty evidence"
                    .to_string(),
            ));
        }
        let gate_digest = ContentDigest::of(&gate)?;
        let approval = aggregate
            .approvals
            .values()
            .find(|approval| {
                approval.candidate == gate.candidate
                    && approval.gate.id == gate.gate_id
                    && approval.gate.generation == gate.generation
                    && approval.gate.digest == gate_digest
                    && approval.policy_digest == gate.policy_digest
                    && approval.approver.principal_id == context.principal.principal_id
                    && approval.approver.authority_generation
                        == context.principal.authority_generation
            })
            .ok_or_else(|| {
                DeliveryError::MissingEvidence(
                    "gate requires an exact independent approval".to_string(),
                )
            })?;
        if approval.approved_at_ms > gate.issued_at_ms {
            return Err(DeliveryError::StaleEvidence(
                "approval was issued after the release gate".to_string(),
            ));
        }
        let candidate = aggregate
            .candidates
            .get_mut(&gate.candidate.id)
            .ok_or_else(|| DeliveryError::NotFound(format!("candidate {}", gate.candidate.id)))?;
        if gate.candidate.generation != candidate.generation
            || gate.candidate.digest != candidate.candidate_digest
            || candidate.state != CandidateState::QaRunning
            || candidate
                .implementer_principal_ids
                .contains(&context.principal.principal_id)
        {
            return Err(DeliveryError::StaleEvidence(
                "gate is stale, self-approved, or bound to another candidate".to_string(),
            ));
        }
        let next = if gate.passed {
            CandidateState::GatePassed
        } else {
            CandidateState::GateFailed
        };
        transition_candidate(candidate.state, next)?;
        candidate.state = next;
        if aggregate
            .gates
            .insert(gate.gate_id.clone(), gate.clone())
            .is_some()
        {
            return Err(DeliveryError::Conflict(format!(
                "gate {} already exists",
                gate.gate_id
            )));
        }
        let expected_revision = aggregate.revision;
        aggregate.revision += 1;
        self.commit(
            context,
            "record_gate",
            command_digest,
            aggregate,
            "qa_release_gate_recorded_v1",
            json!({
                "gate_id": gate.gate_id,
                "candidate_id": gate.candidate.id,
                "passed": gate.passed,
                "policy_digest": gate.policy_digest,
            }),
            expected_revision,
        )
    }

    pub fn record_review_bundle(
        &self,
        context: &CommandContextV1,
        tenant_id: &str,
        project_id: &str,
        run_id: &str,
        review: ReviewV1,
        test_run: TestRunV1,
        findings: Vec<FindingV1>,
        approval: Option<ApprovalV1>,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        require_role(context, AuthorityRole::Qa)?;
        require_tenant(context, tenant_id)?;
        let command_digest = command_digest(
            context,
            &(
                tenant_id, project_id, run_id, &review, &test_run, &findings, &approval,
            ),
        )?;
        if let Some(receipt) =
            self.existing(context, "record_review_bundle", tenant_id, &command_digest)?
        {
            return Ok(receipt);
        }
        if review.reviewer.principal_id != context.principal.principal_id
            || review.reviewer.authority_generation != context.principal.authority_generation
        {
            return Err(DeliveryError::AuthorityDenied(
                "reviewer is not the authenticated QA authority".to_string(),
            ));
        }
        let mut aggregate = self.required_aggregate(tenant_id, project_id)?;
        let run = aggregate
            .qa_runs
            .get(run_id)
            .ok_or_else(|| DeliveryError::NotFound(format!("QA run {run_id}")))?;
        let plan = aggregate
            .qa_plans
            .get(&run.plan.id)
            .ok_or_else(|| DeliveryError::CorruptStore("QA plan missing".to_string()))?;
        let candidate = aggregate
            .candidates
            .get(&plan.candidate.id)
            .ok_or_else(|| DeliveryError::CorruptStore("candidate missing".to_string()))?;
        if candidate
            .implementer_principal_ids
            .contains(&context.principal.principal_id)
            || review.candidate != plan.candidate
            || test_run.candidate != plan.candidate
            || test_run.qa_plan != run.plan
            || findings
                .iter()
                .any(|finding| finding.candidate != plan.candidate)
            || review.approved != test_run.passed
        {
            return Err(DeliveryError::StaleEvidence(
                "review bundle is self-authored, inconsistent, or bound to another candidate"
                    .to_string(),
            ));
        }
        if let Some(approval) = &approval {
            if !review.approved
                || approval.candidate != plan.candidate
                || approval.approver.principal_id != context.principal.principal_id
                || approval.approver.authority_generation != context.principal.authority_generation
            {
                return Err(DeliveryError::AuthorityDenied(
                    "approval is not an independent exact-candidate QA approval".to_string(),
                ));
            }
        }
        if aggregate
            .reviews
            .insert(review.review_id.clone(), review.clone())
            .is_some()
            || aggregate
                .test_runs
                .insert(test_run.test_run_id.clone(), test_run.clone())
                .is_some()
        {
            return Err(DeliveryError::Conflict(
                "review or test-run ID already exists".to_string(),
            ));
        }
        for finding in &findings {
            if aggregate
                .findings
                .insert(finding.finding_id.clone(), finding.clone())
                .is_some()
            {
                return Err(DeliveryError::Conflict(format!(
                    "finding {} already exists",
                    finding.finding_id
                )));
            }
        }
        if let Some(approval) = &approval {
            if aggregate
                .approvals
                .insert(approval.approval_id.clone(), approval.clone())
                .is_some()
            {
                return Err(DeliveryError::Conflict(format!(
                    "approval {} already exists",
                    approval.approval_id
                )));
            }
        }
        let expected_revision = aggregate.revision;
        aggregate.revision += 1;
        self.commit(
            context,
            "record_review_bundle",
            command_digest,
            aggregate,
            "qa_review_bundle_recorded_v1",
            json!({
                "run_id": run_id,
                "review_id": review.review_id,
                "test_run_id": test_run.test_run_id,
                "finding_ids": findings
                    .iter()
                    .map(|finding| finding.finding_id.as_str())
                    .collect::<Vec<_>>(),
                "approval_id": approval.as_ref().map(|value| value.approval_id.as_str()),
            }),
            expected_revision,
        )
    }

    pub fn promote(
        &self,
        context: &CommandContextV1,
        tenant_id: &str,
        project_id: &str,
        candidate_id: &str,
        manifest: ReleaseManifestV1,
        mut release: ReleaseV1,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        require_role(context, AuthorityRole::ReleaseManager)?;
        require_tenant(context, tenant_id)?;
        let command_digest = command_digest(
            context,
            &(tenant_id, project_id, candidate_id, &manifest, &release),
        )?;
        if let Some(receipt) = self.existing(context, "promote", tenant_id, &command_digest)? {
            return Ok(receipt);
        }
        self.require_integration()?;
        let mut aggregate = self.required_aggregate(tenant_id, project_id)?;
        let candidate = aggregate
            .candidates
            .get(candidate_id)
            .cloned()
            .ok_or_else(|| DeliveryError::NotFound(format!("candidate {candidate_id}")))?;
        if candidate.state != CandidateState::GatePassed
            || candidate
                .implementer_principal_ids
                .contains(&context.principal.principal_id)
            || manifest.manifest_digest != manifest.computed_digest()?
            || manifest.candidate.id != candidate.candidate_id
            || manifest.candidate.generation != candidate.generation
            || manifest.candidate.digest != candidate.candidate_digest
            || manifest.release_actor.principal_id != context.principal.principal_id
            || manifest.release_actor.authority_generation != context.principal.authority_generation
        {
            return Err(DeliveryError::StaleEvidence(
                "manifest or release authority is stale, self-authored, or differently digested"
                    .to_string(),
            ));
        }
        let gate = aggregate
            .gates
            .get(&manifest.qa_gate.id)
            .ok_or_else(|| DeliveryError::MissingEvidence("release gate".to_string()))?;
        if !gate.passed
            || gate.expires_at_ms <= context.now_ms
            || gate.candidate != manifest.candidate
            || gate.actor.principal_id == context.principal.principal_id
            || gate.release_manifest_digest != manifest.gate_input_digest()?
        {
            return Err(DeliveryError::StaleEvidence(
                "release gate is missing, expired, self-approved, or bound elsewhere".to_string(),
            ));
        }
        let authority = self
            .integration
            .candidate_authority(&CandidateAuthorityQueryV1 {
                tenant_id: tenant_id.to_string(),
                agreement: candidate.agreement.clone(),
                project: candidate.project.clone(),
                work_items_digest: candidate.work_items_digest.clone(),
                candidate_digest: candidate.candidate_digest.clone(),
            })?;
        if authority.current_candidate_generation != candidate.generation
            || authority.current_candidate_digest != candidate.candidate_digest
        {
            return Err(DeliveryError::StaleEvidence(
                "candidate is no longer current".to_string(),
            ));
        }
        if release.manifest.id != manifest.manifest_id
            || release.manifest.generation != manifest.generation
            || release.manifest.digest != manifest.manifest_digest
            || release.state != ReleaseState::Approved
        {
            return Err(DeliveryError::Validation(
                "release does not reference the immutable approved manifest".to_string(),
            ));
        }
        transition_release(release.state, ReleaseState::Active)?;
        release.state = ReleaseState::Active;
        release.activated_at_ms = Some(context.now_ms);
        if let Some(previous_id) = aggregate.active_release_id.clone() {
            let previous = aggregate
                .releases
                .get_mut(&previous_id)
                .ok_or_else(|| DeliveryError::CorruptStore("active release missing".to_string()))?;
            transition_release(previous.state, ReleaseState::Superseded)?;
            previous.state = ReleaseState::Superseded;
        }
        aggregate
            .manifests
            .insert(manifest.manifest_id.clone(), manifest.clone());
        aggregate
            .releases
            .insert(release.release_id.clone(), release.clone());
        aggregate.active_release_id = Some(release.release_id.clone());
        let candidate_mut = aggregate
            .candidates
            .get_mut(candidate_id)
            .ok_or_else(|| DeliveryError::NotFound(format!("candidate {candidate_id}")))?;
        transition_candidate(candidate_mut.state, CandidateState::Promoted)?;
        candidate_mut.state = CandidateState::Promoted;
        let expected_revision = aggregate.revision;
        aggregate.revision += 1;
        self.commit(
            context,
            "promote",
            command_digest,
            aggregate,
            "release_promoted_v1",
            json!({
                "candidate_id": candidate_id,
                "manifest_id": manifest.manifest_id,
                "manifest_digest": manifest.manifest_digest,
                "release_id": release.release_id,
            }),
            expected_revision,
        )
    }

    pub fn issue_delivery(
        &self,
        context: &CommandContextV1,
        project_id: &str,
        mut receipt: DeliveryReceiptV1,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        require_role(context, AuthorityRole::ReleaseManager)?;
        require_tenant(context, &receipt.tenant_id)?;
        let command_digest = command_digest(context, &(project_id, &receipt))?;
        if let Some(existing) = self.existing(
            context,
            "issue_delivery",
            &receipt.tenant_id,
            &command_digest,
        )? {
            return Ok(existing);
        }
        let mut aggregate = self.required_aggregate(&receipt.tenant_id, project_id)?;
        if receipt.state != DeliveryState::PreviewReady
            || receipt.expires_at_ms <= context.now_ms
            || receipt.receipt_digest != receipt.computed_digest()?
        {
            return Err(DeliveryError::Validation(
                "delivery receipt state, expiry, or digest is invalid".to_string(),
            ));
        }
        let release = aggregate
            .releases
            .get(&receipt.release.id)
            .ok_or_else(|| DeliveryError::NotFound(format!("release {}", receipt.release.id)))?;
        if release.state != ReleaseState::Active
            || release.generation != receipt.release.generation
            || receipt.release.digest != ContentDigest::of(release)?
        {
            return Err(DeliveryError::StaleEvidence(
                "delivery is not bound to the active release".to_string(),
            ));
        }
        transition_delivery(receipt.state, DeliveryState::Delivered)?;
        receipt.state = DeliveryState::Delivered;
        receipt = receipt.seal()?;
        if aggregate
            .deliveries
            .insert(receipt.delivery_id.clone(), receipt.clone())
            .is_some()
        {
            return Err(DeliveryError::Conflict(format!(
                "delivery {} already exists",
                receipt.delivery_id
            )));
        }
        let expected_revision = aggregate.revision;
        aggregate.revision += 1;
        self.commit(
            context,
            "issue_delivery",
            command_digest,
            aggregate,
            "customer_delivery_issued_v1",
            json!({
                "delivery_id": receipt.delivery_id,
                "release_id": receipt.release.id,
                "customer_principal_id": receipt.customer_principal_id,
                "receipt_digest": receipt.receipt_digest,
            }),
            expected_revision,
        )
    }

    pub fn customer_action(
        &self,
        context: &CommandContextV1,
        tenant_id: &str,
        project_id: &str,
        feedback: CustomerFeedbackV1,
        acceptance: Option<AcceptanceV1>,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        require_role(context, AuthorityRole::Customer)?;
        require_tenant(context, tenant_id)?;
        let command_digest =
            command_digest(context, &(tenant_id, project_id, &feedback, &acceptance))?;
        if let Some(existing) =
            self.existing(context, "customer_action", tenant_id, &command_digest)?
        {
            return Ok(existing);
        }
        if feedback.customer.principal_id != context.principal.principal_id
            || feedback.customer.authority_generation != context.principal.authority_generation
            || feedback.schema_version != super::schema::DELIVERY_SCHEMA_V1
            || feedback.created_at_ms != context.now_ms
            || feedback.feedback_digest != feedback.computed_digest()?
        {
            return Err(DeliveryError::AuthorityDenied(
                "customer feedback principal is not authenticated authority".to_string(),
            ));
        }
        let mut aggregate = self.required_aggregate(tenant_id, project_id)?;
        let delivery = aggregate
            .deliveries
            .get_mut(&feedback.delivery.id)
            .ok_or_else(|| DeliveryError::NotFound(format!("delivery {}", feedback.delivery.id)))?;
        if delivery.customer_principal_id != context.principal.principal_id
            || delivery.generation != feedback.delivery.generation
            || delivery.receipt_digest != feedback.delivery.digest
            || delivery.expires_at_ms <= context.now_ms
            || delivery.state != DeliveryState::Delivered
        {
            return Err(DeliveryError::AuthorityDenied(
                "delivery is expired, already terminal, or belongs to another customer".to_string(),
            ));
        }
        let next = match feedback.action {
            CustomerAction::Accept => DeliveryState::Accepted,
            CustomerAction::Reject => DeliveryState::Rejected,
            CustomerAction::RequestChanges => DeliveryState::ChangesRequested,
        };
        transition_delivery(delivery.state, next)?;
        delivery.state = next;
        match feedback.action {
            CustomerAction::Accept => {
                let acceptance = acceptance.ok_or_else(|| {
                    DeliveryError::MissingEvidence("explicit customer acceptance".to_string())
                })?;
                if acceptance.schema_version != super::schema::DELIVERY_SCHEMA_V1
                    || acceptance.customer.principal_id != context.principal.principal_id
                    || acceptance.customer.authority_generation
                        != context.principal.authority_generation
                    || acceptance.delivery != feedback.delivery
                    || acceptance.release.id != delivery.release.id
                    || acceptance.accepted_at_ms != context.now_ms
                    || acceptance.acceptance_digest != acceptance.computed_digest()?
                {
                    return Err(DeliveryError::AuthorityDenied(
                        "acceptance is not bound to the authenticated delivery".to_string(),
                    ));
                }
                if aggregate
                    .acceptances
                    .insert(acceptance.acceptance_id.clone(), acceptance)
                    .is_some()
                {
                    return Err(DeliveryError::Conflict(
                        "acceptance ID already exists".to_string(),
                    ));
                }
            }
            CustomerAction::RequestChanges if feedback.requested_work_item_refs.is_empty() => {
                return Err(DeliveryError::MissingEvidence(
                    "request changes requires authoritative linked work items".to_string(),
                ));
            }
            _ if acceptance.is_some() => {
                return Err(DeliveryError::Validation(
                    "acceptance evidence is allowed only for accept".to_string(),
                ));
            }
            _ => {}
        }
        if aggregate
            .feedback
            .insert(feedback.feedback_id.clone(), feedback.clone())
            .is_some()
        {
            return Err(DeliveryError::Conflict(format!(
                "feedback {} already exists",
                feedback.feedback_id
            )));
        }
        let expected_revision = aggregate.revision;
        aggregate.revision += 1;
        self.commit(
            context,
            "customer_action",
            command_digest,
            aggregate,
            "customer_delivery_action_v1",
            json!({
                "feedback_id": feedback.feedback_id,
                "delivery_id": feedback.delivery.id,
                "action": feedback.action,
                "feedback_digest": feedback.feedback_digest,
            }),
            expected_revision,
        )
    }

    pub fn rollback(
        &self,
        context: &CommandContextV1,
        tenant_id: &str,
        project_id: &str,
        rollback: RollbackV1,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        require_role(context, AuthorityRole::ReleaseManager)?;
        require_tenant(context, tenant_id)?;
        let command_digest = command_digest(context, &(tenant_id, project_id, &rollback))?;
        if let Some(existing) = self.existing(context, "rollback", tenant_id, &command_digest)? {
            return Ok(existing);
        }
        if rollback.actor.principal_id != context.principal.principal_id
            || rollback.actor.authority_generation != context.principal.authority_generation
            || rollback.schema_version != super::schema::DELIVERY_SCHEMA_V1
            || rollback.from_release.id == rollback.to_release.id
            || rollback.from_release.digest == ContentDigest::zero()
            || rollback.to_release.digest == ContentDigest::zero()
            || rollback.reason_digest == ContentDigest::zero()
            || rollback.effect_receipt.digest == ContentDigest::zero()
            || rollback.created_at_ms != context.now_ms
        {
            return Err(DeliveryError::AuthorityDenied(
                "rollback schema, binding, receipt, or actor is invalid".to_string(),
            ));
        }
        let mut aggregate = self.required_aggregate(tenant_id, project_id)?;
        if aggregate.active_release_id.as_deref() != Some(rollback.from_release.id.as_str()) {
            return Err(DeliveryError::StaleEvidence(
                "rollback source is not the active release".to_string(),
            ));
        }
        {
            let from = aggregate
                .releases
                .get_mut(&rollback.from_release.id)
                .ok_or_else(|| DeliveryError::NotFound("rollback source release".to_string()))?;
            if ContentDigest::of(&*from)? != rollback.from_release.digest {
                return Err(DeliveryError::StaleEvidence(
                    "rollback source digest mismatch".to_string(),
                ));
            }
            transition_release(from.state, ReleaseState::RolledBack)?;
            from.state = ReleaseState::RolledBack;
        }
        {
            let to = aggregate
                .releases
                .get_mut(&rollback.to_release.id)
                .ok_or_else(|| DeliveryError::NotFound("rollback target release".to_string()))?;
            if ContentDigest::of(&*to)? != rollback.to_release.digest {
                return Err(DeliveryError::StaleEvidence(
                    "rollback target digest mismatch".to_string(),
                ));
            }
            transition_release(to.state, ReleaseState::Active)?;
            to.state = ReleaseState::Active;
            to.activated_at_ms = Some(context.now_ms);
        }
        aggregate.active_release_id = Some(rollback.to_release.id.clone());
        if aggregate
            .rollbacks
            .insert(rollback.rollback_id.clone(), rollback.clone())
            .is_some()
        {
            return Err(DeliveryError::Conflict(format!(
                "rollback {} already exists",
                rollback.rollback_id
            )));
        }
        let expected_revision = aggregate.revision;
        aggregate.revision += 1;
        self.commit(
            context,
            "rollback",
            command_digest,
            aggregate,
            "release_rolled_back_v1",
            json!({
                "rollback_id": rollback.rollback_id,
                "from_release_id": rollback.from_release.id,
                "to_release_id": rollback.to_release.id,
                "effect_receipt": rollback.effect_receipt,
            }),
            expected_revision,
        )
    }

    pub fn closeout(
        &self,
        context: &CommandContextV1,
        tenant_id: &str,
        project_id: &str,
        closeout: ProjectCloseoutV1,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        require_role(context, AuthorityRole::ReleaseManager)?;
        require_tenant(context, tenant_id)?;
        let command_digest = command_digest(context, &(tenant_id, project_id, &closeout))?;
        if let Some(existing) = self.existing(context, "closeout", tenant_id, &command_digest)? {
            return Ok(existing);
        }
        if closeout.schema_version != super::schema::DELIVERY_SCHEMA_V1
            || closeout.project.id != project_id
            || closeout.closed_by.principal_id != context.principal.principal_id
            || closeout.closed_by.authority_generation != context.principal.authority_generation
            || closeout.created_at_ms != context.now_ms
            || closeout.decisions_digest == ContentDigest::zero()
            || closeout.artifact_inventory_digest == ContentDigest::zero()
            || closeout.failures_digest == ContentDigest::zero()
            || closeout.lessons_digest == ContentDigest::zero()
            || closeout
                .memory_publication
                .as_ref()
                .is_none_or(|receipt| receipt.digest == ContentDigest::zero())
        {
            return Err(DeliveryError::AdapterUnavailable {
                dependency: "closeout_memory",
                reason: "a current authoritative #695/NMDA publication receipt is required"
                    .to_string(),
            });
        }
        let mut aggregate = self.required_aggregate(tenant_id, project_id)?;
        let acceptance = aggregate
            .acceptances
            .get(&closeout.acceptance.id)
            .ok_or_else(|| DeliveryError::MissingEvidence("customer acceptance".to_string()))?;
        if acceptance.generation != closeout.acceptance.generation
            || acceptance.acceptance_digest != closeout.acceptance.digest
            || acceptance.release != closeout.accepted_release
        {
            return Err(DeliveryError::StaleEvidence(
                "closeout acceptance is stale or differently digested".to_string(),
            ));
        }
        if aggregate
            .closeouts
            .insert(closeout.closeout_id.clone(), closeout.clone())
            .is_some()
        {
            return Err(DeliveryError::Conflict(format!(
                "closeout {} already exists",
                closeout.closeout_id
            )));
        }
        let expected_revision = aggregate.revision;
        aggregate.revision += 1;
        self.commit(
            context,
            "closeout",
            command_digest,
            aggregate,
            "project_closeout_recorded_v1",
            json!({
                "closeout_id": closeout.closeout_id,
                "project": closeout.project,
                "accepted_release": closeout.accepted_release,
                "memory_publication": closeout.memory_publication,
            }),
            expected_revision,
        )
    }

    pub fn publish_pending<P: DeliveryPublicationPort>(
        &self,
        publisher: &P,
    ) -> Result<usize, DeliveryError> {
        let pending = self.store.pending_publications()?;
        let mut published = 0;
        for entry in pending {
            let receipt = publisher.publish(&entry.request)?;
            self.store
                .mark_published(&entry.request.payload_digest, receipt)?;
            published += 1;
        }
        Ok(published)
    }

    pub fn store(&self) -> &DeliveryStore {
        &self.store
    }

    fn require_integration(&self) -> Result<(), DeliveryError> {
        match self.integration.readiness() {
            AdapterReadiness::Ready { .. } => Ok(()),
            AdapterReadiness::Unavailable { reason } => Err(DeliveryError::AdapterUnavailable {
                dependency: "delivery_integration",
                reason,
            }),
        }
    }

    fn required_aggregate(
        &self,
        tenant_id: &str,
        project_id: &str,
    ) -> Result<DeliveryAggregateV1, DeliveryError> {
        self.store
            .load(tenant_id, project_id)?
            .ok_or_else(|| DeliveryError::NotFound(format!("project {project_id}")))
    }

    fn existing(
        &self,
        context: &CommandContextV1,
        command_kind: &str,
        tenant_id: &str,
        command_digest: &ContentDigest,
    ) -> Result<Option<DeliveryCommitReceiptV1>, DeliveryError> {
        self.store.lookup_idempotency(
            tenant_id,
            &context.principal.principal_id,
            command_kind,
            &context.idempotency_key,
            command_digest,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn commit(
        &self,
        context: &CommandContextV1,
        command_kind: &str,
        command_digest: ContentDigest,
        aggregate: DeliveryAggregateV1,
        event_type: &str,
        event_payload: serde_json::Value,
        expected_revision: u64,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        self.store.commit(&DeliveryCommitRequestV1 {
            tenant_id: aggregate.tenant_id.clone(),
            project_id: aggregate.project_id.clone(),
            expected_revision,
            principal_id: context.principal.principal_id.clone(),
            command_kind: command_kind.to_string(),
            idempotency_key: context.idempotency_key.clone(),
            command_digest,
            aggregate,
            event_type: event_type.to_string(),
            event_payload,
            committed_at_ms: context.now_ms,
        })
    }
}

fn command_digest<T: Serialize>(
    context: &CommandContextV1,
    payload: &T,
) -> Result<ContentDigest, DeliveryError> {
    ContentDigest::of(&(
        &context.principal.tenant_id,
        &context.principal.principal_id,
        context.principal.authority_generation,
        payload,
    ))
}

fn require_tenant(context: &CommandContextV1, tenant_id: &str) -> Result<(), DeliveryError> {
    if context.principal.tenant_id != tenant_id {
        return Err(DeliveryError::AuthorityDenied(
            "cross-tenant command denied".to_string(),
        ));
    }
    Ok(())
}

fn require_same_tenant(left: &PrincipalV1, right: &PrincipalV1) -> Result<(), DeliveryError> {
    if left.tenant_id != right.tenant_id {
        return Err(DeliveryError::AuthorityDenied(
            "cross-tenant authority denied".to_string(),
        ));
    }
    Ok(())
}

fn require_role(context: &CommandContextV1, role: AuthorityRole) -> Result<(), DeliveryError> {
    require_principal_role(&context.principal, role)
}

fn require_principal_role(
    principal: &PrincipalV1,
    role: AuthorityRole,
) -> Result<(), DeliveryError> {
    if !principal.has_role(role.clone()) {
        return Err(DeliveryError::AuthorityDenied(format!(
            "{} lacks {role:?}",
            principal.principal_id
        )));
    }
    Ok(())
}

pub fn versioned_ref<T: Serialize>(
    id: impl Into<String>,
    generation: u64,
    value: &T,
) -> Result<VersionedRefV1, DeliveryError> {
    Ok(VersionedRefV1 {
        id: id.into(),
        generation,
        digest: ContentDigest::of(value)?,
    })
}
