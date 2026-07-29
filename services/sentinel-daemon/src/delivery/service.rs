use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    digest::ContentDigest,
    error::DeliveryError,
    ports::{
        expected_integration_contract_digest, AdapterReadiness, AuthorityReceiptV1,
        AuthorityValidationRequestV1, CandidateAuthorityQueryV1, DeliveryEffectKind,
        DeliveryEffectPort, DeliveryEffectRequestV1, DeliveryIntegrationPort,
        DeliveryPublicationPort, WorkbenchEvidenceReceiptV1, WorkbenchEvidenceRequestV1,
    },
    schema::{
        AcceptanceV1, ApprovalV1, AuthorityRole, CandidateState, CustomerAction,
        CustomerFeedbackV1, DeliveryReceiptV1, DeliveryState, FindingV1, PrincipalV1,
        ProjectCloseoutV1, QaAggregateOutcomesV1, QaCaseOutcome, QaEvaluationPlanV1,
        QaEvaluationRunReceiptV1, QaEvidenceGraphV1, QaHarnessOutcome, QaReleaseGateReceiptV1,
        QaRunState, ReleaseCandidateV1, ReleaseManifestV1, ReleaseState, ReleaseV1, ReviewV1,
        RollbackV1, TestRunV1, VersionedRefV1, DELIVERY_SCHEMA_V1,
    },
    state::{
        transition_candidate, transition_delivery, transition_qa_run, transition_release,
        DeliveryAggregateV1,
    },
    store::{
        DeliveryAggregateStorePort, DeliveryCommitReceiptV1, DeliveryCommitRequestV1,
        DeliveryPublicationStatePort, DeliveryStore,
    },
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandContextV1 {
    pub principal: PrincipalV1,
    pub idempotency_key: String,
    pub now_ms: u64,
}

pub struct DeliveryCore<I, S = DeliveryStore, E = super::ports::UnavailableDeliveryEffects> {
    store: S,
    integration: I,
    effects: E,
}

impl<I, S, E> DeliveryCore<I, S, E>
where
    I: DeliveryIntegrationPort,
    S: DeliveryAggregateStorePort + DeliveryPublicationStatePort,
    E: DeliveryEffectPort,
{
    /// Deterministic constructor for the dependency-independent core.
    ///
    /// Productive construction remains unavailable until #732 and #733 provide
    /// the canonical trajectory and publication adapters.
    #[doc(hidden)]
    pub fn new_test_only(store: S, integration: I, effects: E) -> Self {
        Self {
            store,
            integration,
            effects,
        }
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
        self.require_current_authority(
            context,
            &candidate.tenant_id,
            AuthorityRole::Developer,
            "register_candidate",
        )?;
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
        mut run: QaEvaluationRunReceiptV1,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        self.require_current_authority(
            context,
            tenant_id,
            AuthorityRole::ReleaseManager,
            "assign_qa",
        )?;
        let command_digest =
            command_digest(context, &(tenant_id, project_id, candidate_id, &plan, &run))?;
        if let Some(receipt) = self.existing(context, "assign_qa", tenant_id, &command_digest)? {
            return Ok(receipt);
        }
        if run.state != QaRunState::Planned
            || run.actors.len() != 1
            || run.started_at_ms.is_some()
            || run.finished_at_ms.is_some()
            || run.attempts != 0
            || run.harness_outcome.is_some()
            || run.cleanup_receipt.is_some()
            || run.aggregate_outcomes.is_some()
            || run.gate_receipt.is_some()
            || run.durable_event_generation != 0
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
        let qa = run.actors[0].clone();
        require_principal_role(&qa, AuthorityRole::Qa)?;
        require_same_tenant(&context.principal, &qa)?;
        let qa_authority = self.current_authority_for(
            &qa,
            tenant_id,
            AuthorityRole::Qa,
            "qa_assignment",
            context.now_ms,
        )?;
        if qa_authority.principal != qa {
            return Err(DeliveryError::StaleEvidence(
                "assigned QA principal is not the current authenticated authority".to_string(),
            ));
        }
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
        run.durable_event_generation = aggregate.revision + 1;
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
        let current_authority =
            self.require_current_authority(context, tenant_id, AuthorityRole::Qa, "transition_qa")?;
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
        if run.actors.as_slice() != [current_authority.principal.clone()] {
            return Err(DeliveryError::AuthorityDenied(
                "only the exact assigned QA authority may transition the run".to_string(),
            ));
        }
        transition_qa_run(run.state, next)?;
        let outcomes = run.aggregate_outcomes.as_ref();
        match next {
            QaRunState::CompletedPass
                if run.harness_outcome != Some(QaHarnessOutcome::Pass)
                    || run.cleanup_receipt.is_none()
                    || !matches!(
                        outcomes,
                        Some(QaAggregateOutcomesV1 {
                            required_cases_complete: true,
                            contaminated: false,
                            needs_human_review: false,
                            flaky_unresolved: false,
                        })
                    ) =>
            {
                return Err(DeliveryError::MissingEvidence(
                    "completed-pass requires exact clean workbench evidence".to_string(),
                ));
            }
            QaRunState::CompletedFail
                if run.harness_outcome != Some(QaHarnessOutcome::Fail)
                    || run.cleanup_receipt.is_none()
                    || outcomes.is_none() =>
            {
                return Err(DeliveryError::MissingEvidence(
                    "completed-fail requires an exact failed workbench receipt".to_string(),
                ));
            }
            QaRunState::HarnessError
                if run.harness_outcome != Some(QaHarnessOutcome::Error)
                    || run.cleanup_receipt.is_none() =>
            {
                return Err(DeliveryError::MissingEvidence(
                    "harness-error requires an exact workbench error and cleanup receipt"
                        .to_string(),
                ));
            }
            _ => {}
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
        let authority_before =
            self.require_current_authority(context, tenant_id, AuthorityRole::Qa, "execute_qa")?;
        let mut aggregate = self.required_aggregate(tenant_id, project_id)?;
        let run = aggregate
            .qa_runs
            .get(run_id)
            .ok_or_else(|| DeliveryError::NotFound(format!("QA run {run_id}")))?
            .clone();
        if run.state != QaRunState::Running
            || run.actors.as_slice() != [authority_before.principal.clone()]
        {
            return Err(DeliveryError::AuthorityDenied(
                "only the exact assigned QA authority may execute a running plan".to_string(),
            ));
        }
        let plan = aggregate
            .qa_plans
            .get(&run.plan.id)
            .ok_or_else(|| DeliveryError::CorruptStore("QA plan missing".to_string()))?
            .clone();
        let run_ref = VersionedRefV1 {
            id: run.run_id.clone(),
            generation: run.generation,
            digest: run.request_digest.clone(),
        };
        let invocation = VersionedRefV1 {
            id: format!("qa:{}:{}:{}", tenant_id, project_id, run.run_id),
            generation: run.generation,
            digest: ContentDigest::of_domain(
                "workbench-invocation",
                DELIVERY_SCHEMA_V1,
                &(
                    tenant_id,
                    project_id,
                    &run_ref,
                    &authority_before.receipt_digest,
                ),
            )?,
        };
        let request = WorkbenchEvidenceRequestV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            tenant_id: tenant_id.to_string(),
            project: plan.project.clone(),
            candidate: plan.candidate.clone(),
            qa_plan: run.plan.clone(),
            qa_run: run_ref.clone(),
            assigned_qa: authority_before.principal.clone(),
            authority_receipt_digest: authority_before.receipt_digest.clone(),
            invocation,
            request_digest: ContentDigest::zero(),
        }
        .seal()?;
        let command_digest = command_digest(context, &(tenant_id, project_id, run_id, &request))?;
        if let Some(existing) = self.existing(context, "execute_qa", tenant_id, &command_digest)? {
            let receipt = aggregate
                .workbench_receipts
                .values()
                .find(|receipt| {
                    receipt.input_digest == request.request_digest
                        && receipt.qa_run == request.qa_run
                        && receipt.invocation == request.invocation
                })
                .cloned()
                .ok_or_else(|| {
                    DeliveryError::CorruptStore(
                        "idempotent QA commit has no matching receipt".to_string(),
                    )
                })?;
            return Ok((existing, receipt));
        }
        // The external effect occurs only after the durable running state exists and
        // no database writer is held.
        let receipt = self.integration.execute_qa(&request)?;
        let authority_after =
            self.require_current_authority(context, tenant_id, AuthorityRole::Qa, "execute_qa")?;
        if authority_after != authority_before {
            return Err(DeliveryError::StaleEvidence(
                "QA authority changed between workbench request and evidence adoption".to_string(),
            ));
        }
        if receipt.receipt_digest != receipt.computed_digest()?
            || receipt.input_digest != request.request_digest
            || receipt.invocation != request.invocation
            || receipt.assignment != request.qa_run
            || receipt.qa_run != request.qa_run
            || receipt.assigned_qa != request.assigned_qa
            || receipt.authority_receipt_digest != request.authority_receipt_digest
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
        run_mut.harness_outcome = Some(receipt.harness_outcome);
        run_mut.aggregate_outcomes = Some(QaAggregateOutcomesV1 {
            required_cases_complete: receipt.required_cases_complete,
            contaminated: receipt.contaminated,
            needs_human_review: receipt.needs_human_review,
            flaky_unresolved: receipt.flaky_unresolved,
        });
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

    pub fn import_evidence_graph(
        &self,
        context: &CommandContextV1,
        tenant_id: &str,
        project_id: &str,
        run_id: &str,
        graph: QaEvidenceGraphV1,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        let authority = self.require_current_authority(
            context,
            tenant_id,
            AuthorityRole::Qa,
            "import_evidence_graph",
        )?;
        let command_digest = command_digest(context, &(tenant_id, project_id, run_id, &graph))?;
        if let Some(receipt) =
            self.existing(context, "import_evidence_graph", tenant_id, &command_digest)?
        {
            return Ok(receipt);
        }
        let mut aggregate = self.required_aggregate(tenant_id, project_id)?;
        let run = aggregate
            .qa_runs
            .get(run_id)
            .ok_or_else(|| DeliveryError::NotFound(format!("QA run {run_id}")))?
            .clone();
        if run.state != QaRunState::Running
            || run.actors.as_slice() != [authority.principal.clone()]
            || graph.schema_version != DELIVERY_SCHEMA_V1
            || graph.graph_digest != graph.computed_digest()?
        {
            return Err(DeliveryError::StaleEvidence(
                "evidence graph is not bound to the exact running QA authority".to_string(),
            ));
        }
        let run_ref = VersionedRefV1 {
            id: run.run_id.clone(),
            generation: run.generation,
            digest: run.request_digest.clone(),
        };
        if graph.run != run_ref {
            return Err(DeliveryError::StaleEvidence(
                "evidence graph references another QA run".to_string(),
            ));
        }
        let workbench = aggregate
            .workbench_receipts
            .get(&graph.workbench_receipt.id)
            .ok_or_else(|| DeliveryError::MissingEvidence("workbench receipt".to_string()))?;
        if graph.workbench_receipt.generation != workbench.invocation.generation
            || graph.workbench_receipt.digest != workbench.receipt_digest
            || workbench.qa_run != run_ref
            || workbench.assigned_qa != authority.principal
            || workbench.result_inventory_digest != qa_evidence_inventory_digest(&graph)?
        {
            return Err(DeliveryError::StaleEvidence(
                "evidence inventory is not bound to the exact workbench receipt".to_string(),
            ));
        }
        let plan = aggregate
            .qa_plans
            .get(&run.plan.id)
            .ok_or_else(|| DeliveryError::CorruptStore("QA plan missing".to_string()))?;
        validate_evidence_graph(plan, &run_ref, &graph)?;
        validate_evidence_outcome(workbench, &graph)?;
        if aggregate
            .evidence_graphs
            .insert(run_id.to_string(), graph.clone())
            .is_some()
        {
            return Err(DeliveryError::Conflict(format!(
                "evidence graph for run {run_id} already exists"
            )));
        }
        let expected_revision = aggregate.revision;
        aggregate.revision += 1;
        self.commit(
            context,
            "import_evidence_graph",
            command_digest,
            aggregate,
            "qa_evidence_graph_imported_v1",
            json!({
                "run_id": run_id,
                "graph_digest": graph.graph_digest,
                "workbench_receipt": graph.workbench_receipt,
            }),
            expected_revision,
        )
    }

    pub fn record_gate(
        &self,
        context: &CommandContextV1,
        tenant_id: &str,
        project_id: &str,
        run_id: &str,
        gate: QaReleaseGateReceiptV1,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        let gate_authority =
            self.require_current_authority(context, tenant_id, AuthorityRole::Qa, "record_gate")?;
        let command_digest = command_digest(context, &(tenant_id, project_id, run_id, &gate))?;
        if let Some(receipt) = self.existing(context, "record_gate", tenant_id, &command_digest)? {
            return Ok(receipt);
        }
        if gate.actor != gate_authority.principal
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
        if run.plan != gate.plan {
            return Err(DeliveryError::MissingEvidence(
                "gate requires the exact terminal QA plan".to_string(),
            ));
        }
        let legal_outcome = matches!(
            (gate.passed, run.state, run.harness_outcome),
            (
                true,
                QaRunState::CompletedPass,
                Some(QaHarnessOutcome::Pass)
            ) | (
                false,
                QaRunState::CompletedFail,
                Some(QaHarnessOutcome::Fail)
            ) | (
                false,
                QaRunState::HarnessError,
                Some(QaHarnessOutcome::Error)
            )
        );
        let graph = aggregate
            .evidence_graphs
            .get(run_id)
            .ok_or_else(|| DeliveryError::MissingEvidence("QA evidence graph".to_string()))?;
        let plan = aggregate
            .qa_plans
            .get(&run.plan.id)
            .ok_or_else(|| DeliveryError::CorruptStore("QA plan missing".to_string()))?;
        if !legal_outcome
            || run.cleanup_receipt.is_none()
            || (gate.passed
                && !matches!(
                    run.aggregate_outcomes.as_ref(),
                    Some(QaAggregateOutcomesV1 {
                        required_cases_complete: true,
                        contaminated: false,
                        needs_human_review: false,
                        flaky_unresolved: false,
                    })
                ))
            || gate.case_inventory_digest != qa_case_inventory_digest(graph)?
            || gate.deterministic_evidence_digest != qa_deterministic_evidence_digest(graph)?
            || gate.model_evidence_digest != qa_model_evidence_digest(graph)?
            || gate.flake_disposition_digest != qa_flake_disposition_digest(graph)?
            || gate.calibration_digest != plan.aggregation_policy_digest
            || gate.source_evidence_digest != qa_source_evidence_digest(graph)?
            || gate.policy_digest != plan.release_policy_digest
        {
            return Err(DeliveryError::MissingEvidence(
                "gate outcome or evidence graph does not match the exact terminal run".to_string(),
            ));
        }
        let gate_digest = ContentDigest::of_domain("qa-release-gate", DELIVERY_SCHEMA_V1, &gate)?;
        let approval = aggregate.approvals.values().find(|approval| {
            approval.candidate == gate.candidate
                && approval.gate.id == gate.gate_id
                && approval.gate.generation == gate.generation
                && approval.gate.digest == gate_digest
                && approval.policy_digest == gate.policy_digest
                && approval.approver == gate_authority.principal
        });
        if gate.passed && approval.is_none() {
            return Err(DeliveryError::MissingEvidence(
                "passing gate requires an exact independent approval".to_string(),
            ));
        }
        if approval.is_some_and(|value| value.approved_at_ms > gate.issued_at_ms) {
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
        aggregate
            .qa_runs
            .get_mut(run_id)
            .ok_or_else(|| DeliveryError::CorruptStore("QA run disappeared".to_string()))?
            .gate_receipt = Some(VersionedRefV1 {
            id: gate.gate_id.clone(),
            generation: gate.generation,
            digest: gate_digest,
        });
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
        let review_authority = self.require_current_authority(
            context,
            tenant_id,
            AuthorityRole::Qa,
            "record_review_bundle",
        )?;
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
        if review.reviewer != review_authority.principal {
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
        let graph = aggregate
            .evidence_graphs
            .get(run_id)
            .ok_or_else(|| DeliveryError::MissingEvidence("QA evidence graph".to_string()))?;
        let workbench = aggregate
            .workbench_receipts
            .get(&graph.workbench_receipt.id)
            .ok_or_else(|| DeliveryError::MissingEvidence("workbench receipt".to_string()))?;
        let terminal_pass = run.state == QaRunState::CompletedPass
            && run.harness_outcome == Some(QaHarnessOutcome::Pass);
        let terminal_fail = matches!(
            (run.state, run.harness_outcome),
            (QaRunState::CompletedFail, Some(QaHarnessOutcome::Fail))
                | (QaRunState::HarnessError, Some(QaHarnessOutcome::Error))
        );
        if candidate
            .implementer_principal_ids
            .contains(&context.principal.principal_id)
            || (!terminal_pass && !terminal_fail)
            || review.candidate != plan.candidate
            || test_run.candidate != plan.candidate
            || test_run.qa_plan != run.plan
            || test_run.runner_receipt.id != workbench.invocation.id
            || test_run.runner_receipt.generation != workbench.invocation.generation
            || test_run.runner_receipt.digest != workbench.receipt_digest
            || test_run.result_inventory_digest != workbench.result_inventory_digest
            || test_run.logs_digest != workbench.logs_digest
            || test_run.screenshots_digest != workbench.screenshots_digest
            || findings
                .iter()
                .any(|finding| finding.candidate != plan.candidate)
            || review.findings_digest
                != ContentDigest::of_domain("qa-findings", DELIVERY_SCHEMA_V1, &findings)?
            || review.approved != terminal_pass
            || test_run.passed != terminal_pass
        {
            return Err(DeliveryError::StaleEvidence(
                "review bundle is self-authored, inconsistent, or bound to another candidate"
                    .to_string(),
            ));
        }
        if let Some(approval) = &approval {
            if !terminal_pass
                || approval.candidate != plan.candidate
                || approval.approver != review_authority.principal
            {
                return Err(DeliveryError::AuthorityDenied(
                    "approval is not an independent exact-candidate QA approval".to_string(),
                ));
            }
        } else if terminal_pass {
            return Err(DeliveryError::MissingEvidence(
                "passing review requires an independent approval".to_string(),
            ));
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
        let release_authority = self.require_current_authority(
            context,
            tenant_id,
            AuthorityRole::ReleaseManager,
            "promote",
        )?;
        let command_digest = command_digest(
            context,
            &(tenant_id, project_id, candidate_id, &manifest, &release),
        )?;
        if let Some(receipt) = self.existing(context, "promote", tenant_id, &command_digest)? {
            return Ok(receipt);
        }
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
            || manifest.release_actor != release_authority.principal
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
            || release.rollout_receipt.is_some()
        {
            return Err(DeliveryError::Validation(
                "release does not reference the immutable approved manifest".to_string(),
            ));
        }
        if aggregate.manifests.contains_key(&manifest.manifest_id)
            || aggregate.releases.contains_key(&release.release_id)
        {
            return Err(DeliveryError::Conflict(
                "manifest and release IDs are immutable and cannot be reused".to_string(),
            ));
        }
        let effect_request = DeliveryEffectRequestV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            kind: DeliveryEffectKind::Rollout,
            tenant_id: tenant_id.to_string(),
            project: candidate.project.clone(),
            candidate: Some(manifest.candidate.clone()),
            subject: release.manifest.clone(),
            actor_authority_receipt_digest: release_authority.receipt_digest.clone(),
            request_digest: ContentDigest::zero(),
        }
        .seal()?;
        let effect_receipt = self.effects.apply(&effect_request)?;
        validate_effect_receipt(
            &effect_request,
            &effect_receipt,
            &release_authority,
            context.now_ms,
        )?;
        let authority_after = self.require_current_authority(
            context,
            tenant_id,
            AuthorityRole::ReleaseManager,
            "promote",
        )?;
        if authority_after != release_authority {
            return Err(DeliveryError::StaleEvidence(
                "release authority changed between rollout and local adoption".to_string(),
            ));
        }
        let authority_after_effect =
            self.integration
                .candidate_authority(&CandidateAuthorityQueryV1 {
                    tenant_id: tenant_id.to_string(),
                    agreement: candidate.agreement.clone(),
                    project: candidate.project.clone(),
                    work_items_digest: candidate.work_items_digest.clone(),
                    candidate_digest: candidate.candidate_digest.clone(),
                })?;
        if authority_after_effect != authority {
            return Err(DeliveryError::StaleEvidence(
                "candidate authority changed between rollout and local adoption".to_string(),
            ));
        }
        transition_release(release.state, ReleaseState::Active)?;
        release.state = ReleaseState::Active;
        release.activated_at_ms = Some(context.now_ms);
        release.rollout_receipt = Some(effect_receipt.effect_ref);
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
        self.require_current_authority(
            context,
            &receipt.tenant_id,
            AuthorityRole::ReleaseManager,
            "issue_delivery",
        )?;
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
            || receipt.release.digest
                != ContentDigest::of_domain("release", DELIVERY_SCHEMA_V1, release)?
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
        mut feedback: CustomerFeedbackV1,
        acceptance: Option<AcceptanceV1>,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        let customer_authority = self.require_current_authority(
            context,
            tenant_id,
            AuthorityRole::Customer,
            "customer_action",
        )?;
        let command_digest =
            command_digest(context, &(tenant_id, project_id, &feedback, &acceptance))?;
        if let Some(existing) =
            self.existing(context, "customer_action", tenant_id, &command_digest)?
        {
            return Ok(existing);
        }
        if feedback.customer != customer_authority.principal
            || feedback.schema_version != super::schema::DELIVERY_SCHEMA_V1
            || feedback.created_at_ms != context.now_ms
            || feedback.feedback_digest != feedback.computed_digest()?
        {
            return Err(DeliveryError::AuthorityDenied(
                "customer feedback principal is not authenticated authority".to_string(),
            ));
        }
        let mut aggregate = self.required_aggregate(tenant_id, project_id)?;
        let delivery_snapshot = aggregate
            .deliveries
            .get(&feedback.delivery.id)
            .ok_or_else(|| DeliveryError::NotFound(format!("delivery {}", feedback.delivery.id)))?
            .clone();
        if delivery_snapshot.customer_principal_id != context.principal.principal_id
            || delivery_snapshot.generation != feedback.delivery.generation
            || delivery_snapshot.receipt_digest != feedback.delivery.digest
            || delivery_snapshot.expires_at_ms <= context.now_ms
            || delivery_snapshot.state != DeliveryState::Delivered
        {
            return Err(DeliveryError::AuthorityDenied(
                "delivery is expired, already terminal, or belongs to another customer".to_string(),
            ));
        }
        if feedback.action == CustomerAction::RequestChanges {
            if !feedback.requested_work_item_refs.is_empty() {
                return Err(DeliveryError::Validation(
                    "caller may not supply governed rework authority data".to_string(),
                ));
            }
            let release = aggregate
                .releases
                .get(&delivery_snapshot.release.id)
                .ok_or_else(|| {
                    DeliveryError::CorruptStore("delivery release missing".to_string())
                })?;
            let manifest = aggregate
                .manifests
                .get(&release.manifest.id)
                .ok_or_else(|| {
                    DeliveryError::CorruptStore("release manifest missing".to_string())
                })?;
            let request = DeliveryEffectRequestV1 {
                schema_version: DELIVERY_SCHEMA_V1,
                kind: DeliveryEffectKind::GovernedRework,
                tenant_id: tenant_id.to_string(),
                project: manifest.project.clone(),
                candidate: Some(manifest.candidate.clone()),
                subject: feedback.delivery.clone(),
                actor_authority_receipt_digest: customer_authority.receipt_digest.clone(),
                request_digest: ContentDigest::zero(),
            }
            .seal()?;
            let receipt = self.effects.apply(&request)?;
            validate_effect_receipt(&request, &receipt, &customer_authority, context.now_ms)?;
            let authority_after = self.require_current_authority(
                context,
                tenant_id,
                AuthorityRole::Customer,
                "customer_action",
            )?;
            if authority_after != customer_authority {
                return Err(DeliveryError::StaleEvidence(
                    "customer authority changed between rework effect and adoption".to_string(),
                ));
            }
            feedback.requested_work_item_refs = vec![receipt.effect_ref];
            feedback = feedback.seal()?;
        }
        let next = match feedback.action {
            CustomerAction::Accept => DeliveryState::Accepted,
            CustomerAction::Reject => DeliveryState::Rejected,
            CustomerAction::RequestChanges => DeliveryState::ChangesRequested,
        };
        let delivery = aggregate
            .deliveries
            .get_mut(&feedback.delivery.id)
            .ok_or_else(|| DeliveryError::NotFound(format!("delivery {}", feedback.delivery.id)))?;
        transition_delivery(delivery.state, next)?;
        delivery.state = next;
        match feedback.action {
            CustomerAction::Accept => {
                let acceptance = acceptance.ok_or_else(|| {
                    DeliveryError::MissingEvidence("explicit customer acceptance".to_string())
                })?;
                if acceptance.schema_version != super::schema::DELIVERY_SCHEMA_V1
                    || acceptance.customer != customer_authority.principal
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
        mut rollback: RollbackV1,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        let rollback_authority = self.require_current_authority(
            context,
            tenant_id,
            AuthorityRole::ReleaseManager,
            "rollback",
        )?;
        let command_digest = command_digest(context, &(tenant_id, project_id, &rollback))?;
        if let Some(existing) = self.existing(context, "rollback", tenant_id, &command_digest)? {
            return Ok(existing);
        }
        if rollback.actor != rollback_authority.principal
            || rollback.schema_version != super::schema::DELIVERY_SCHEMA_V1
            || rollback.from_release.id == rollback.to_release.id
            || rollback.from_release.digest == ContentDigest::zero()
            || rollback.to_release.digest == ContentDigest::zero()
            || rollback.reason_digest == ContentDigest::zero()
            || rollback.effect_receipt.is_some()
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
        let from_snapshot = aggregate
            .releases
            .get(&rollback.from_release.id)
            .ok_or_else(|| DeliveryError::NotFound("rollback source release".to_string()))?;
        let to_snapshot = aggregate
            .releases
            .get(&rollback.to_release.id)
            .ok_or_else(|| DeliveryError::NotFound("rollback target release".to_string()))?;
        if ContentDigest::of_domain("release", DELIVERY_SCHEMA_V1, from_snapshot)?
            != rollback.from_release.digest
            || ContentDigest::of_domain("release", DELIVERY_SCHEMA_V1, to_snapshot)?
                != rollback.to_release.digest
        {
            return Err(DeliveryError::StaleEvidence(
                "rollback release digest mismatch".to_string(),
            ));
        }
        let from_manifest = aggregate
            .manifests
            .get(&from_snapshot.manifest.id)
            .ok_or_else(|| {
                DeliveryError::MissingEvidence("rollback source manifest".to_string())
            })?;
        let effect_request = DeliveryEffectRequestV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            kind: DeliveryEffectKind::Rollback,
            tenant_id: tenant_id.to_string(),
            project: from_manifest.project.clone(),
            candidate: Some(from_manifest.candidate.clone()),
            subject: rollback.from_release.clone(),
            actor_authority_receipt_digest: rollback_authority.receipt_digest.clone(),
            request_digest: ContentDigest::zero(),
        }
        .seal()?;
        let effect_receipt = self.effects.apply(&effect_request)?;
        validate_effect_receipt(
            &effect_request,
            &effect_receipt,
            &rollback_authority,
            context.now_ms,
        )?;
        let authority_after = self.require_current_authority(
            context,
            tenant_id,
            AuthorityRole::ReleaseManager,
            "rollback",
        )?;
        if authority_after != rollback_authority {
            return Err(DeliveryError::StaleEvidence(
                "release authority changed between rollback effect and adoption".to_string(),
            ));
        }
        rollback.effect_receipt = Some(effect_receipt.effect_ref);
        {
            let from = aggregate
                .releases
                .get_mut(&rollback.from_release.id)
                .ok_or_else(|| DeliveryError::NotFound("rollback source release".to_string()))?;
            if ContentDigest::of_domain("release", DELIVERY_SCHEMA_V1, &*from)?
                != rollback.from_release.digest
            {
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
            if ContentDigest::of_domain("release", DELIVERY_SCHEMA_V1, &*to)?
                != rollback.to_release.digest
            {
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
        mut closeout: ProjectCloseoutV1,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        let closeout_authority = self.require_current_authority(
            context,
            tenant_id,
            AuthorityRole::ReleaseManager,
            "closeout",
        )?;
        let command_digest = command_digest(context, &(tenant_id, project_id, &closeout))?;
        if let Some(existing) = self.existing(context, "closeout", tenant_id, &command_digest)? {
            return Ok(existing);
        }
        if closeout.schema_version != super::schema::DELIVERY_SCHEMA_V1
            || closeout.project.id != project_id
            || closeout.closed_by != closeout_authority.principal
            || closeout.created_at_ms != context.now_ms
            || closeout.decisions_digest == ContentDigest::zero()
            || closeout.artifact_inventory_digest == ContentDigest::zero()
            || closeout.failures_digest == ContentDigest::zero()
            || closeout.lessons_digest == ContentDigest::zero()
            || closeout.memory_publication.is_some()
        {
            return Err(DeliveryError::Validation(
                "closeout input must be complete and may not self-attest memory publication"
                    .to_string(),
            ));
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
        let accepted_release = aggregate
            .releases
            .get(&closeout.accepted_release.id)
            .ok_or_else(|| DeliveryError::MissingEvidence("accepted release".to_string()))?;
        let manifest = aggregate
            .manifests
            .get(&accepted_release.manifest.id)
            .ok_or_else(|| DeliveryError::MissingEvidence("accepted manifest".to_string()))?;
        let effect_request = DeliveryEffectRequestV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            kind: DeliveryEffectKind::MemoryPublication,
            tenant_id: tenant_id.to_string(),
            project: closeout.project.clone(),
            candidate: Some(manifest.candidate.clone()),
            subject: closeout.accepted_release.clone(),
            actor_authority_receipt_digest: closeout_authority.receipt_digest.clone(),
            request_digest: ContentDigest::zero(),
        }
        .seal()?;
        let effect_receipt = self.effects.apply(&effect_request)?;
        validate_effect_receipt(
            &effect_request,
            &effect_receipt,
            &closeout_authority,
            context.now_ms,
        )?;
        let authority_after = self.require_current_authority(
            context,
            tenant_id,
            AuthorityRole::ReleaseManager,
            "closeout",
        )?;
        if authority_after != closeout_authority {
            return Err(DeliveryError::StaleEvidence(
                "release authority changed between memory publication and closeout".to_string(),
            ));
        }
        closeout.memory_publication = Some(effect_receipt.effect_ref);
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
                .mark_published(&entry.request.request_digest, receipt)?;
            published += 1;
        }
        Ok(published)
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    fn require_integration(&self) -> Result<(u16, u64, ContentDigest), DeliveryError> {
        match self.integration.readiness() {
            AdapterReadiness::Ready {
                contract_version,
                authority_generation,
                contract_digest,
            } if contract_version == DELIVERY_SCHEMA_V1
                && authority_generation > 0
                && contract_digest == expected_integration_contract_digest() =>
            {
                Ok((contract_version, authority_generation, contract_digest))
            }
            AdapterReadiness::Ready { .. } => Err(DeliveryError::StaleEvidence(
                "integration adapter contract version, generation, or digest is not current"
                    .to_string(),
            )),
            AdapterReadiness::Unavailable { reason } => Err(DeliveryError::AdapterUnavailable {
                dependency: "delivery_integration",
                reason,
            }),
        }
    }

    fn require_current_authority(
        &self,
        context: &CommandContextV1,
        tenant_id: &str,
        role: AuthorityRole,
        operation: &str,
    ) -> Result<AuthorityReceiptV1, DeliveryError> {
        require_tenant(context, tenant_id)?;
        require_role(context, role.clone())?;
        self.current_authority_for(
            &context.principal,
            tenant_id,
            role,
            operation,
            context.now_ms,
        )
    }

    fn current_authority_for(
        &self,
        principal: &PrincipalV1,
        tenant_id: &str,
        role: AuthorityRole,
        operation: &str,
        now_ms: u64,
    ) -> Result<AuthorityReceiptV1, DeliveryError> {
        require_principal_role(principal, role.clone())?;
        if principal.tenant_id != tenant_id {
            return Err(DeliveryError::AuthorityDenied(
                "cross-tenant authority denied".to_string(),
            ));
        }
        let (contract_version, contract_authority_generation, contract_digest) =
            self.require_integration()?;
        let request = AuthorityValidationRequestV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            tenant_id: tenant_id.to_string(),
            principal_id: principal.principal_id.clone(),
            claimed_authority_generation: principal.authority_generation,
            required_role: role.clone(),
            operation: operation.to_string(),
            contract_version,
            contract_digest: contract_digest.clone(),
            request_digest: ContentDigest::zero(),
        }
        .seal()?;
        let receipt = self.integration.authorize(&request)?;
        if receipt.schema_version != DELIVERY_SCHEMA_V1
            || receipt.receipt_digest != receipt.computed_digest()?
            || receipt.request_digest != request.request_digest
            || receipt.principal != *principal
            || !receipt.principal.has_role(role)
            || receipt.contract_version != contract_version
            || receipt.contract_authority_generation != contract_authority_generation
            || receipt.contract_digest != contract_digest
            || receipt.issued_at_ms > now_ms
            || receipt.expires_at_ms <= now_ms
            || receipt.issuer.is_empty()
        {
            return Err(DeliveryError::StaleEvidence(
                "authenticated authority receipt is missing, stale, revoked, or mismatched"
                    .to_string(),
            ));
        }
        Ok(receipt)
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
    ContentDigest::of_domain(
        "delivery-command",
        DELIVERY_SCHEMA_V1,
        &(
            &context.principal.tenant_id,
            &context.principal.principal_id,
            context.principal.authority_generation,
            payload,
        ),
    )
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
        digest: ContentDigest::of_domain("versioned-ref", DELIVERY_SCHEMA_V1, value)?,
    })
}

pub fn qa_evidence_inventory_digest(
    graph: &QaEvidenceGraphV1,
) -> Result<ContentDigest, DeliveryError> {
    ContentDigest::of_domain(
        "qa-evidence-inventory",
        DELIVERY_SCHEMA_V1,
        &(
            &graph.dataset_cases,
            &graph.case_results,
            &graph.deterministic_results,
            &graph.model_results,
            &graph.flake_dispositions,
        ),
    )
}

pub fn qa_case_inventory_digest(graph: &QaEvidenceGraphV1) -> Result<ContentDigest, DeliveryError> {
    ContentDigest::of_domain(
        "qa-case-inventory",
        DELIVERY_SCHEMA_V1,
        &(&graph.dataset_cases, &graph.case_results),
    )
}

pub fn qa_deterministic_evidence_digest(
    graph: &QaEvidenceGraphV1,
) -> Result<ContentDigest, DeliveryError> {
    ContentDigest::of_domain(
        "qa-deterministic-evidence",
        DELIVERY_SCHEMA_V1,
        &graph.deterministic_results,
    )
}

pub fn qa_model_evidence_digest(
    graph: &QaEvidenceGraphV1,
) -> Result<Option<ContentDigest>, DeliveryError> {
    if graph.model_results.is_empty() {
        Ok(None)
    } else {
        ContentDigest::of_domain(
            "qa-model-evidence-inventory",
            DELIVERY_SCHEMA_V1,
            &graph.model_results,
        )
        .map(Some)
    }
}

pub fn qa_flake_disposition_digest(
    graph: &QaEvidenceGraphV1,
) -> Result<Option<ContentDigest>, DeliveryError> {
    if graph.flake_dispositions.is_empty() {
        Ok(None)
    } else {
        ContentDigest::of_domain(
            "qa-flake-disposition-inventory",
            DELIVERY_SCHEMA_V1,
            &graph.flake_dispositions,
        )
        .map(Some)
    }
}

pub fn qa_source_evidence_digest(
    graph: &QaEvidenceGraphV1,
) -> Result<ContentDigest, DeliveryError> {
    let sources: Vec<_> = graph
        .dataset_cases
        .iter()
        .flat_map(|case| case.provenance.iter())
        .chain(
            graph
                .case_results
                .iter()
                .flat_map(|result| result.sources.iter()),
        )
        .collect();
    ContentDigest::of_domain("qa-source-evidence", DELIVERY_SCHEMA_V1, &sources)
}

fn validate_evidence_graph(
    plan: &QaEvaluationPlanV1,
    run_ref: &VersionedRefV1,
    graph: &QaEvidenceGraphV1,
) -> Result<(), DeliveryError> {
    let case_ids: BTreeSet<_> = graph
        .dataset_cases
        .iter()
        .map(|case| case.case_id.as_str())
        .collect();
    if plan
        .required_case_ids
        .iter()
        .any(|case_id| !case_ids.contains(case_id.as_str()))
        || graph.dataset_cases.iter().any(|case| {
            case.schema_version != DELIVERY_SCHEMA_V1
                || (!plan.required_case_ids.contains(&case.case_id)
                    && !plan.optional_case_ids.contains(&case.case_id))
                || case.required != plan.required_case_ids.contains(&case.case_id)
        })
    {
        return Err(DeliveryError::MissingEvidence(
            "dataset inventory does not cover the exact QA plan".to_string(),
        ));
    }
    let result_case_ids: BTreeSet<_> = graph
        .case_results
        .iter()
        .map(|result| result.case_ref.id.as_str())
        .collect();
    if plan
        .required_case_ids
        .iter()
        .any(|case_id| !result_case_ids.contains(case_id.as_str()))
    {
        return Err(DeliveryError::MissingEvidence(
            "required QA case result is missing".to_string(),
        ));
    }
    for result in &graph.case_results {
        let case = graph
            .dataset_cases
            .iter()
            .find(|case| case.case_id == result.case_ref.id)
            .ok_or_else(|| DeliveryError::MissingEvidence("dataset case".to_string()))?;
        let case_digest = ContentDigest::of_domain("qa-dataset-case", DELIVERY_SCHEMA_V1, case)?;
        if result.schema_version != DELIVERY_SCHEMA_V1
            || result.run != *run_ref
            || result.case_ref.generation != case.generation
            || result.case_ref.digest != case_digest
            || result.required != case.required
        {
            return Err(DeliveryError::StaleEvidence(
                "case result is bound to another run or dataset case".to_string(),
            ));
        }
        for assertion_ref in &result.assertion_refs {
            let assertion = graph
                .deterministic_results
                .iter()
                .find(|value| value.assertion_id == assertion_ref.id)
                .ok_or_else(|| {
                    DeliveryError::MissingEvidence("deterministic assertion".to_string())
                })?;
            if assertion_ref.generation != assertion.generation
                || assertion_ref.digest
                    != ContentDigest::of_domain(
                        "qa-deterministic-result",
                        DELIVERY_SCHEMA_V1,
                        assertion,
                    )?
            {
                return Err(DeliveryError::StaleEvidence(
                    "deterministic assertion reference is stale".to_string(),
                ));
            }
        }
        for grader_ref in &result.grader_refs {
            let grader = graph
                .model_results
                .iter()
                .find(|value| value.evidence_id == grader_ref.id)
                .ok_or_else(|| DeliveryError::MissingEvidence("model grader".to_string()))?;
            if grader_ref.generation != grader.generation
                || grader_ref.digest
                    != ContentDigest::of_domain("qa-model-evidence", DELIVERY_SCHEMA_V1, grader)?
            {
                return Err(DeliveryError::StaleEvidence(
                    "model grader reference is stale".to_string(),
                ));
            }
        }
    }
    for disposition in &graph.flake_dispositions {
        let result = graph
            .case_results
            .iter()
            .find(|value| value.result_id == disposition.result.id)
            .ok_or_else(|| DeliveryError::MissingEvidence("flake result".to_string()))?;
        if disposition.result.generation != result.generation
            || disposition.result.digest
                != ContentDigest::of_domain("qa-case-result", DELIVERY_SCHEMA_V1, result)?
        {
            return Err(DeliveryError::StaleEvidence(
                "flake disposition references another result".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_evidence_outcome(
    receipt: &WorkbenchEvidenceReceiptV1,
    graph: &QaEvidenceGraphV1,
) -> Result<(), DeliveryError> {
    let required: Vec<_> = graph
        .case_results
        .iter()
        .filter(|result| result.required)
        .collect();
    let legal = match receipt.harness_outcome {
        QaHarnessOutcome::Pass => {
            receipt.required_cases_complete
                && !receipt.contaminated
                && !receipt.needs_human_review
                && !receipt.flaky_unresolved
                && required
                    .iter()
                    .all(|result| result.outcome == QaCaseOutcome::Pass)
        }
        QaHarnessOutcome::Fail => {
            receipt.required_cases_complete
                && required.iter().any(|result| {
                    matches!(
                        result.outcome,
                        QaCaseOutcome::Fail
                            | QaCaseOutcome::NeedsHumanReview
                            | QaCaseOutcome::FlakyUnresolved
                    )
                })
        }
        QaHarnessOutcome::Error => required
            .iter()
            .any(|result| result.outcome == QaCaseOutcome::Error),
    };
    if !legal {
        return Err(DeliveryError::MissingEvidence(
            "workbench summary and persisted case-result graph are inconsistent".to_string(),
        ));
    }
    Ok(())
}

fn validate_effect_receipt(
    request: &DeliveryEffectRequestV1,
    receipt: &super::ports::DeliveryEffectReceiptV1,
    authority: &AuthorityReceiptV1,
    now_ms: u64,
) -> Result<(), DeliveryError> {
    if receipt.schema_version != DELIVERY_SCHEMA_V1
        || receipt.receipt_digest != receipt.computed_digest()?
        || receipt.kind != request.kind
        || receipt.tenant_id != request.tenant_id
        || receipt.project != request.project
        || receipt.candidate != request.candidate
        || receipt.request_digest != request.request_digest
        || receipt.actor_authority_receipt_digest != authority.receipt_digest
        || receipt.effect_ref.digest == ContentDigest::zero()
        || receipt.effect_ref.generation == 0
        || receipt.issuer.is_empty()
        || receipt.issued_at_ms > now_ms
    {
        return Err(DeliveryError::StaleEvidence(
            "external effect receipt is absent, stale, ambiguous, or differently bound".to_string(),
        ));
    }
    Ok(())
}
