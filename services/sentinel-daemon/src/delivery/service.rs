use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    digest::ContentDigest,
    error::DeliveryError,
    ports::{
        expected_effect_saga_contract_digest, expected_integration_contract_digest,
        expected_publication_contract_digest, expected_workbench_execution_saga_contract_digest,
        AdapterReadiness, AuthorityReceiptV1, AuthorityValidationRequestV1,
        CandidateAuthorityQueryV1, CandidateAuthoritySnapshotV1, DeliveryEffectKind,
        DeliveryEffectPort, DeliveryEffectRequestV1, DeliveryIntegrationPort,
        DeliveryPublicationPort, WorkbenchEvidenceReceiptV1, WorkbenchEvidenceRequestV1,
        WorkflowLineageQueryV1,
    },
    schema::{
        AcceptanceV1, ApprovalV1, AuthorityRole, CandidateState, CustomerAction,
        CustomerFeedbackV1, DataControlV1, DeliveryReceiptV1, DeliveryState, FindingV1,
        PrincipalV1, ProjectCloseoutV1, QaAggregateOutcomesV1, QaCaseOutcome, QaCaseReasonCode,
        QaDatasetCaseV1, QaEvaluationPlanV1, QaEvaluationRunReceiptV1, QaEvidenceGraphV1,
        QaHarnessOutcome, QaReleaseGateReceiptV1, QaRunState, ReleaseCandidateV1,
        ReleaseManifestV1, ReleaseState, ReleaseV1, ReviewV1, RollbackV1, SourceTupleV1, TestRunV1,
        VersionedRefV1, DELIVERY_PREVIEW_MAX_TTL_MS, DELIVERY_PREVIEW_TTL_POLICY_V1,
        DELIVERY_SCHEMA_V1,
    },
    state::{
        transition_candidate, transition_delivery, transition_qa_run, transition_release,
        DeliveryAggregateV1,
    },
    store::{
        DeliveryAggregateStorePort, DeliveryCommitReceiptV1, DeliveryCommitRequestV1,
        DeliveryPublicationStatePort, DeliveryStore, DeliveryStoreConfigV1,
    },
    canonical_release_reference, canonical_release_reference_digest, PublicDeliveryLineageDtoV1,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandContextV1 {
    pub principal: PrincipalV1,
    pub idempotency_key: String,
    pub now_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryCommandV1 {
    RegisterCandidate,
    AssignQa,
    TransitionQa,
    ExecuteQa,
    ImportEvidenceGraph,
    RecordGate,
    RecordReviewBundle,
    Promote,
    IssueDelivery,
    CustomerAccept,
    CustomerReject,
    CustomerRequestChanges,
    Rollback,
    Closeout,
}

pub struct DeliveryCore<I, S = DeliveryStore, E = super::ports::UnavailableDeliveryEffects> {
    store: S,
    integration: I,
    effects: E,
}

/// Product-shaped composition with every external authority injected.
///
/// Construction opens and verifies the local durable store but deliberately
/// succeeds with unavailable external ports so the daemon can start before
/// #694/#695 integration. `readiness` and every dependent command remain
/// fail-closed until the exact versioned contracts are ready.
pub struct ConfiguredDeliveryCore<I, E, P> {
    core: DeliveryCore<I, DeliveryStore, E>,
    publication: P,
}

impl<I, S, E> DeliveryCore<I, S, E>
where
    I: DeliveryIntegrationPort,
    S: DeliveryAggregateStorePort + DeliveryPublicationStatePort,
    E: DeliveryEffectPort,
{
    fn with_ports(store: S, integration: I, effects: E) -> Self {
        Self {
            store,
            integration,
            effects,
        }
    }

    #[doc(hidden)]
    pub fn new_test_only(store: S, integration: I, effects: E) -> Self {
        Self::with_ports(store, integration, effects)
    }

    pub fn command_readiness(&self, command: DeliveryCommandV1) -> Result<(), DeliveryError> {
        self.require_integration()?;
        match command {
            DeliveryCommandV1::ExecuteQa => self.require_execution_saga(),
            DeliveryCommandV1::Promote
            | DeliveryCommandV1::CustomerRequestChanges
            | DeliveryCommandV1::Rollback
            | DeliveryCommandV1::Closeout => self.require_effect_saga(),
            DeliveryCommandV1::RegisterCandidate
            | DeliveryCommandV1::AssignQa
            | DeliveryCommandV1::TransitionQa
            | DeliveryCommandV1::ImportEvidenceGraph
            | DeliveryCommandV1::RecordGate
            | DeliveryCommandV1::RecordReviewBundle
            | DeliveryCommandV1::IssueDelivery
            | DeliveryCommandV1::CustomerAccept
            | DeliveryCommandV1::CustomerReject => Ok(()),
        }
    }

    pub fn load(
        &self,
        tenant_id: &str,
        project_id: &str,
    ) -> Result<Option<DeliveryAggregateV1>, DeliveryError> {
        self.store.load(tenant_id, project_id)
    }

    fn read_public_lineage_authorized(
        &self,
        context: &CommandContextV1,
        tenant_id: &str,
        project_id: &str,
    ) -> Result<PublicDeliveryLineageDtoV1, DeliveryError> {
        let role = lineage_role(&context.principal).ok_or_else(|| {
            DeliveryError::AuthorityDenied(
                "principal has no public delivery-lineage reader role".to_string(),
            )
        })?;
        let authority = self.require_current_authority(
            context,
            tenant_id,
            role.clone(),
            "read_public_delivery_lineage",
        )?;
        let aggregate = self.required_aggregate(tenant_id, project_id)?;
        if role == AuthorityRole::Customer
            && !aggregate.deliveries.values().any(|delivery| {
                delivery.customer_principal_id == context.principal.principal_id
            })
        {
            return Err(DeliveryError::AuthorityDenied(
                "customer has no delivery in the requested project".to_string(),
            ));
        }
        let candidate = aggregate
            .candidates
            .values()
            .max_by(|left, right| {
                left.generation
                    .cmp(&right.generation)
                    .then_with(|| left.candidate_id.cmp(&right.candidate_id))
            })
            .ok_or_else(|| DeliveryError::MissingEvidence("release candidate".to_string()))?;
        let candidate_query = CandidateAuthorityQueryV1 {
            tenant_id: tenant_id.to_string(),
            agreement: candidate.agreement.clone(),
            project: candidate.project.clone(),
            work_items_digest: candidate.work_items_digest.clone(),
            candidate_digest: candidate.candidate_digest.clone(),
        };
        let candidate_authority = self.integration.candidate_authority(&candidate_query)?;
        validate_candidate_authority(
            &candidate_authority,
            candidate,
            authority.contract_authority_generation,
        )?;
        if role == AuthorityRole::Customer {
            require_candidate_participant(
                &candidate_authority,
                &authority.principal,
                AuthorityRole::Customer,
            )?;
        }
        let query = WorkflowLineageQueryV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            tenant_id: tenant_id.to_string(),
            project: candidate.project.clone(),
            candidate: VersionedRefV1 {
                id: candidate.candidate_id.clone(),
                generation: candidate.generation,
                digest: candidate.candidate_digest.clone(),
            },
            authority_generation: candidate_authority.authority_generation,
            authority_identity_digest: candidate_authority.snapshot_digest,
            query_digest: ContentDigest::zero(),
        }
        .seal()?;
        let workflow = self.integration.workflow_lineage(&query)?;
        let authority_after = self.require_current_authority(
            context,
            tenant_id,
            role,
            "read_public_delivery_lineage",
        )?;
        let candidate_authority_after = self.integration.candidate_authority(&candidate_query)?;
        let aggregate_after = self.required_aggregate(tenant_id, project_id)?;
        if !same_authority_identity(&authority_after, &authority)
            || authority_after.contract_authority_generation
                != candidate_authority.authority_generation
            || candidate_authority_after != candidate_authority
            || aggregate_after.revision != aggregate.revision
        {
            return Err(DeliveryError::StaleEvidence(
                "lineage authority changed during workflow snapshot read".to_string(),
            ));
        }
        PublicDeliveryLineageDtoV1::from_authorized_aggregate(
            &aggregate,
            &query,
            &workflow,
            authority.contract_authority_generation,
            context.now_ms,
        )
    }

    pub fn register_candidate(
        &self,
        context: &CommandContextV1,
        candidate: ReleaseCandidateV1,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        validate_record_header(
            candidate.schema_version,
            "candidate_id",
            &candidate.candidate_id,
            candidate.generation,
        )?;
        validate_ref("candidate agreement", &candidate.agreement)?;
        validate_ref("candidate project", &candidate.project)?;
        validate_cost_ref("candidate cost", &candidate.cost)?;
        let developer_authority = self.require_current_authority(
            context,
            &candidate.tenant_id,
            AuthorityRole::Developer,
            "register_candidate",
        )?;
        if candidate.schema_version != super::schema::DELIVERY_SCHEMA_V1
            || candidate.state != CandidateState::Draft
            || candidate.candidate_digest != candidate.computed_digest()?
            || candidate.work_items_digest == ContentDigest::zero()
            || candidate.source_digest == ContentDigest::zero()
            || candidate.toolchain_digest == ContentDigest::zero()
            || candidate.runtime_profile_digest == ContentDigest::zero()
            || candidate.acceptance_criteria_digest == ContentDigest::zero()
            || candidate.created_at_ms > context.now_ms
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
        validate_candidate_authority(
            &authority,
            &candidate,
            developer_authority.contract_authority_generation,
        )?;
        require_candidate_participant(
            &authority,
            &developer_authority.principal,
            AuthorityRole::Developer,
        )?;
        if candidate.implementer_principal_ids.is_empty() {
            return Err(DeliveryError::Validation(
                "candidate has no authoritative implementer".to_string(),
            ));
        }
        for implementer_id in &candidate.implementer_principal_ids {
            require_candidate_participant_id(
                &authority,
                &candidate.tenant_id,
                implementer_id,
                AuthorityRole::Developer,
            )?;
        }
        if candidate.artifacts.iter().any(|artifact| {
            !candidate
                .implementer_principal_ids
                .contains(&artifact.owner_principal_id)
        }) {
            return Err(DeliveryError::AuthorityDenied(
                "candidate artifact owner is not an authoritative implementer".to_string(),
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
        validate_qa_plan(&plan)?;
        validate_record_header(run.schema_version, "run_id", &run.run_id, run.generation)?;
        validate_ref("QA plan candidate", &plan.candidate)?;
        validate_ref("QA run plan", &run.plan)?;
        let release_manager_authority = self.require_current_authority(
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
            || run.case_attempt_history_digest.is_some()
            || run.harness_outcome.is_some()
            || run.cleanup_receipt.is_some()
            || run.aggregate_outcomes.is_some()
            || run.gate_receipt.is_some()
            || run.durable_event_generation != 0
            || run.request_digest == ContentDigest::zero()
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
        let candidate_snapshot = aggregate
            .candidates
            .get(candidate_id)
            .cloned()
            .ok_or_else(|| DeliveryError::NotFound(format!("candidate {candidate_id}")))?;
        let candidate_authority = self
            .integration
            .candidate_authority(&candidate_authority_query(&candidate_snapshot))?;
        validate_candidate_authority(
            &candidate_authority,
            &candidate_snapshot,
            release_manager_authority.contract_authority_generation,
        )?;
        require_candidate_participant(
            &candidate_authority,
            &release_manager_authority.principal,
            AuthorityRole::ReleaseManager,
        )?;
        require_candidate_participant(
            &candidate_authority,
            &qa_authority.principal,
            AuthorityRole::Qa,
        )?;
        let candidate = aggregate
            .candidates
            .get_mut(candidate_id)
            .expect("candidate existence was checked above");
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
        self.require_execution_saga()?;
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
                &(tenant_id, project_id, &run_ref, &authority_before.principal),
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
            authority_identity_digest: authority_before.stable_identity_digest()?,
            invocation,
            request_digest: ContentDigest::zero(),
        }
        .seal()?;
        let command_digest = command_digest(
            context,
            &(tenant_id, project_id, run_id, &request.request_digest),
        )?;
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
        if !same_authority_identity(&authority_after, &authority_before) {
            return Err(DeliveryError::StaleEvidence(
                "QA authority changed between workbench request and evidence adoption".to_string(),
            ));
        }
        validate_ref("workbench cleanup receipt", &receipt.cleanup_receipt)?;
        if receipt.schema_version != DELIVERY_SCHEMA_V1
            || receipt.receipt_digest != receipt.computed_digest()?
            || receipt.input_digest != request.request_digest
            || receipt.invocation != request.invocation
            || receipt.assignment != request.qa_run
            || receipt.qa_run != request.qa_run
            || receipt.assigned_qa != request.assigned_qa
            // This authenticates the durable outcome as sealed by the workbench.
            // It deliberately need not equal a later, short-lived authority receipt.
            || receipt.authority_receipt_digest == ContentDigest::zero()
            || receipt.authority_identity_digest != request.authority_identity_digest
            || receipt.authority_identity_digest != authority_after.stable_identity_digest()?
            || receipt.output_digest == ContentDigest::zero()
            || receipt.artifact_ownership_digest == ContentDigest::zero()
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
        validate_qa_evidence_graph(plan, &run_ref, &graph, &authority.principal, context.now_ms)?;
        validate_evidence_outcome(workbench, &graph)?;
        let attempt_history_digest = qa_case_attempt_history_digest(&graph)?;
        if aggregate
            .evidence_graphs
            .insert(run_id.to_string(), graph.clone())
            .is_some()
        {
            return Err(DeliveryError::Conflict(format!(
                "evidence graph for run {run_id} already exists"
            )));
        }
        aggregate
            .qa_runs
            .get_mut(run_id)
            .ok_or_else(|| DeliveryError::CorruptStore("QA run disappeared".to_string()))?
            .case_attempt_history_digest = Some(attempt_history_digest.clone());
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
                "case_attempt_history_digest": attempt_history_digest,
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
        validate_record_header(
            gate.schema_version,
            "gate_id",
            &gate.gate_id,
            gate.generation,
        )?;
        validate_ref("gate candidate", &gate.candidate)?;
        validate_ref("gate plan", &gate.plan)?;
        let gate_authority =
            self.require_current_authority(context, tenant_id, AuthorityRole::Qa, "record_gate")?;
        let command_digest = command_digest(context, &(tenant_id, project_id, run_id, &gate))?;
        if let Some(receipt) = self.existing(context, "record_gate", tenant_id, &command_digest)? {
            return Ok(receipt);
        }
        if gate.actor != gate_authority.principal
            || gate.issued_at_ms > context.now_ms
            || gate.expires_at_ms <= context.now_ms
            || gate.case_inventory_digest == ContentDigest::zero()
            || gate.deterministic_evidence_digest == ContentDigest::zero()
            || gate.source_evidence_digest == ContentDigest::zero()
            || gate.policy_digest == ContentDigest::zero()
            || gate.release_manifest_digest == ContentDigest::zero()
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
            || gate.model_evidence_digest.is_some()
            || gate.calibration_digest.is_some()
            || gate.flake_disposition_digest != qa_flake_disposition_digest(graph)?
            || gate.source_evidence_digest != qa_source_evidence_digest(graph)?
            || gate.policy_digest != plan.release_policy_digest
            || run.case_attempt_history_digest.as_ref()
                != Some(&qa_case_attempt_history_digest(graph)?)
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
        validate_record_header(
            review.schema_version,
            "review_id",
            &review.review_id,
            review.generation,
        )?;
        validate_record_header(
            test_run.schema_version,
            "test_run_id",
            &test_run.test_run_id,
            test_run.generation,
        )?;
        validate_ref("review candidate", &review.candidate)?;
        validate_ref("test-run candidate", &test_run.candidate)?;
        validate_ref("test-run QA plan", &test_run.qa_plan)?;
        validate_ref("test-run runner receipt", &test_run.runner_receipt)?;
        if review.created_at_ms > context.now_ms
            || review.findings_digest == ContentDigest::zero()
            || test_run.result_inventory_digest == ContentDigest::zero()
            || test_run.logs_digest == ContentDigest::zero()
        {
            return Err(DeliveryError::Validation(
                "review or test-run timestamps and evidence digests are invalid".to_string(),
            ));
        }
        for finding in &findings {
            validate_record_header(
                finding.schema_version,
                "finding_id",
                &finding.finding_id,
                finding.generation,
            )?;
            validate_ref("finding candidate", &finding.candidate)?;
            require_unique_source_tuples("finding evidence", &finding.evidence)?;
            if let Some(resolution) = &finding.resolved_by {
                validate_ref("finding resolution", resolution)?;
            }
        }
        let mut finding_source_digests = std::collections::BTreeMap::new();
        for source in findings.iter().flat_map(|finding| finding.evidence.iter()) {
            let locator = (
                source.owner.as_str(),
                source.source_type.as_str(),
                source.id.as_str(),
                source.generation,
            );
            if let Some(existing_digest) = finding_source_digests.insert(locator, &source.digest) {
                if existing_digest != &source.digest {
                    return Err(DeliveryError::Conflict(
                        "one finding-source locator generation carries conflicting digests"
                            .to_string(),
                    ));
                }
            }
        }
        if let Some(value) = &approval {
            validate_record_header(
                value.schema_version,
                "approval_id",
                &value.approval_id,
                value.generation,
            )?;
            validate_ref("approval candidate", &value.candidate)?;
            validate_ref("approval gate", &value.gate)?;
            if value.policy_digest == ContentDigest::zero() || value.approved_at_ms > context.now_ms
            {
                return Err(DeliveryError::Validation(
                    "approval policy or timestamp is invalid".to_string(),
                ));
            }
        }
        require_unique_ids(
            "finding",
            findings.iter().map(|value| value.finding_id.as_str()),
        )?;
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
        let unresolved_findings = findings.iter().any(|finding| finding.resolved_by.is_none());
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
            || test_run.passed != terminal_pass
            || (review.approved && (!terminal_pass || unresolved_findings))
        {
            return Err(DeliveryError::StaleEvidence(
                "review bundle is self-authored, inconsistent, or bound to another candidate"
                    .to_string(),
            ));
        }
        if let Some(approval) = &approval {
            if !terminal_pass
                || !review.approved
                || unresolved_findings
                || approval.candidate != plan.candidate
                || approval.approver != review_authority.principal
            {
                return Err(DeliveryError::AuthorityDenied(
                    "approval is not an independent exact-candidate QA approval".to_string(),
                ));
            }
        } else if review.approved {
            return Err(DeliveryError::MissingEvidence(
                "approved review requires an independent approval".to_string(),
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
        validate_record_header(
            manifest.schema_version,
            "manifest_id",
            &manifest.manifest_id,
            manifest.generation,
        )?;
        validate_record_header(
            release.schema_version,
            "release_id",
            &release.release_id,
            release.generation,
        )?;
        validate_ref("release manifest", &release.manifest)?;
        validate_cost_ref("release manifest cost", &manifest.cost)?;
        if manifest.tenant_id != tenant_id
            || manifest.project.id != project_id
            || manifest.created_at_ms > context.now_ms
            || manifest.qa_evidence_digest == ContentDigest::zero()
            || manifest.sbom_digest == ContentDigest::zero()
            || manifest.dependency_snapshot_digest == ContentDigest::zero()
            || manifest.provenance_digest == ContentDigest::zero()
        {
            return Err(DeliveryError::Validation(
                "manifest tenant, project, timestamp, or evidence digest is invalid".to_string(),
            ));
        }
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
            || manifest.cost != candidate.cost
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
        let candidate_query = candidate_authority_query(&candidate);
        let authority = self.integration.candidate_authority(&candidate_query)?;
        validate_candidate_authority(
            &authority,
            &candidate,
            release_authority.contract_authority_generation,
        )?;
        require_candidate_participant(
            &authority,
            &release_authority.principal,
            AuthorityRole::ReleaseManager,
        )?;
        require_candidate_participant(
            &authority,
            &gate.actor,
            AuthorityRole::Qa,
        )?;
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
            operation_id: format!("rollout:{tenant_id}:{project_id}:{}", release.release_id),
            kind: DeliveryEffectKind::Rollout,
            tenant_id: tenant_id.to_string(),
            project: candidate.project.clone(),
            candidate: Some(manifest.candidate.clone()),
            subject: release.manifest.clone(),
            target: None,
            actor: release_authority.principal.clone(),
            actor_authority_receipt_digest: release_authority.receipt_digest.clone(),
            actor_authority_identity_digest: release_authority.stable_identity_digest()?,
            request_digest: ContentDigest::zero(),
        }
        .seal()?;
        self.require_effect_saga()?;
        let effect_receipt = self.effects.apply(&effect_request)?;
        let authority_after = self.require_current_authority(
            context,
            tenant_id,
            AuthorityRole::ReleaseManager,
            "promote",
        )?;
        if !same_authority_identity(&authority_after, &release_authority) {
            return Err(DeliveryError::StaleEvidence(
                "release authority changed between rollout and local adoption".to_string(),
            ));
        }
        validate_effect_receipt(
            &effect_request,
            &effect_receipt,
            &authority_after,
            context.now_ms,
        )?;
        let authority_after_effect =
            self.integration
                .candidate_authority(&candidate_query)?;
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
        validate_record_header(
            receipt.schema_version,
            "delivery_id",
            &receipt.delivery_id,
            receipt.generation,
        )?;
        validate_ref("delivery release", &receipt.release)?;
        let preview_ttl_ms = receipt.expires_at_ms.checked_sub(receipt.issued_at_ms);
        if receipt.issued_at_ms != context.now_ms
            || receipt.preview_ttl_policy_version != DELIVERY_PREVIEW_TTL_POLICY_V1
            || !matches!(
                preview_ttl_ms,
                Some(ttl) if ttl > 0 && ttl <= DELIVERY_PREVIEW_MAX_TTL_MS
            )
            || receipt.preview_digest == ContentDigest::zero()
            || !canonical_id(&receipt.customer_principal_id)
        {
            return Err(DeliveryError::Validation(
                "delivery preview TTL policy, timestamp, digest, or customer identity is invalid"
                    .to_string(),
            ));
        }
        let release_manager_authority = self.require_current_authority(
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
            || receipt.receipt_digest != receipt.computed_digest()?
        {
            return Err(DeliveryError::Validation(
                "delivery receipt state, expiry, or digest is invalid".to_string(),
            ));
        }
        let release = aggregate
            .releases
            .get(&receipt.release.id)
            .cloned()
            .ok_or_else(|| DeliveryError::NotFound(format!("release {}", receipt.release.id)))?;
        if release.state != ReleaseState::Active
            || release.generation != receipt.release.generation
            || receipt.release.digest != canonical_release_reference_digest(&release)?
        {
            return Err(DeliveryError::StaleEvidence(
                "delivery is not bound to the active release".to_string(),
            ));
        }
        let manifest = aggregate
            .manifests
            .get(&release.manifest.id)
            .ok_or_else(|| DeliveryError::CorruptStore("release manifest missing".to_string()))?;
        let candidate = aggregate
            .candidates
            .get(&manifest.candidate.id)
            .ok_or_else(|| DeliveryError::CorruptStore("release candidate missing".to_string()))?;
        let candidate_authority = self
            .integration
            .candidate_authority(&candidate_authority_query(candidate))?;
        validate_candidate_authority(
            &candidate_authority,
            candidate,
            release_manager_authority.contract_authority_generation,
        )?;
        require_candidate_participant(
            &candidate_authority,
            &release_manager_authority.principal,
            AuthorityRole::ReleaseManager,
        )?;
        let customer_participant = require_candidate_participant_id(
            &candidate_authority,
            &receipt.tenant_id,
            &receipt.customer_principal_id,
            AuthorityRole::Customer,
        )?;
        self.current_authority_for(
            &customer_participant,
            &receipt.tenant_id,
            AuthorityRole::Customer,
            "delivery_recipient",
            context.now_ms,
        )?;
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
        validate_record_header(
            feedback.schema_version,
            "feedback_id",
            &feedback.feedback_id,
            feedback.generation,
        )?;
        validate_ref("feedback delivery", &feedback.delivery)?;
        if let Some(value) = &acceptance {
            validate_record_header(
                value.schema_version,
                "acceptance_id",
                &value.acceptance_id,
                value.generation,
            )?;
            validate_ref("acceptance delivery", &value.delivery)?;
            validate_ref("acceptance release", &value.release)?;
        }
        match (feedback.action, acceptance.as_ref()) {
            (CustomerAction::Accept, None) => {
                return Err(DeliveryError::MissingEvidence(
                    "explicit customer acceptance".to_string(),
                ));
            }
            (CustomerAction::Reject | CustomerAction::RequestChanges, Some(_)) => {
                return Err(DeliveryError::Validation(
                    "acceptance evidence is allowed only for accept".to_string(),
                ));
            }
            _ => {}
        }
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
            || !feedback.requested_work_item_refs.is_empty()
        {
            return Err(DeliveryError::AuthorityDenied(
                "customer feedback principal is not authenticated authority".to_string(),
            ));
        }
        let mut aggregate = self.required_aggregate(tenant_id, project_id)?;
        if aggregate.feedback.contains_key(&feedback.feedback_id) {
            return Err(DeliveryError::Conflict(format!(
                "feedback {} already exists",
                feedback.feedback_id
            )));
        }
        if acceptance
            .as_ref()
            .is_some_and(|value| aggregate.acceptances.contains_key(&value.acceptance_id))
        {
            return Err(DeliveryError::Conflict(
                "acceptance ID already exists".to_string(),
            ));
        }
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
        let next = match feedback.action {
            CustomerAction::Accept => DeliveryState::Accepted,
            CustomerAction::Reject => DeliveryState::Rejected,
            CustomerAction::RequestChanges => DeliveryState::ChangesRequested,
        };
        transition_delivery(delivery_snapshot.state, next)?;
        if let Some(value) = acceptance.as_ref() {
            if value.schema_version != super::schema::DELIVERY_SCHEMA_V1
                || value.customer != customer_authority.principal
                || value.delivery != feedback.delivery
                || value.release != delivery_snapshot.release
                || value.accepted_at_ms != context.now_ms
                || value.acceptance_digest != value.computed_digest()?
            {
                return Err(DeliveryError::AuthorityDenied(
                    "acceptance is not bound to the authenticated delivery".to_string(),
                ));
            }
        }
        if feedback.action == CustomerAction::RequestChanges {
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
                operation_id: format!("rework:{tenant_id}:{project_id}:{}", feedback.feedback_id),
                kind: DeliveryEffectKind::GovernedRework,
                tenant_id: tenant_id.to_string(),
                project: manifest.project.clone(),
                candidate: Some(manifest.candidate.clone()),
                subject: feedback.delivery.clone(),
                target: None,
                actor: customer_authority.principal.clone(),
                actor_authority_receipt_digest: customer_authority.receipt_digest.clone(),
                actor_authority_identity_digest: customer_authority.stable_identity_digest()?,
                request_digest: ContentDigest::zero(),
            }
            .seal()?;
            self.require_effect_saga()?;
            let receipt = self.effects.apply(&request)?;
            let authority_after = self.require_current_authority(
                context,
                tenant_id,
                AuthorityRole::Customer,
                "customer_action",
            )?;
            if !same_authority_identity(&authority_after, &customer_authority) {
                return Err(DeliveryError::StaleEvidence(
                    "customer authority changed between rework effect and adoption".to_string(),
                ));
            }
            validate_effect_receipt(&request, &receipt, &authority_after, context.now_ms)?;
            feedback.requested_work_item_refs = vec![receipt.effect_ref];
            feedback = feedback.seal()?;
        }
        let delivery = aggregate
            .deliveries
            .get_mut(&feedback.delivery.id)
            .ok_or_else(|| DeliveryError::NotFound(format!("delivery {}", feedback.delivery.id)))?;
        transition_delivery(delivery.state, next)?;
        delivery.state = next;
        if let Some(acceptance) = acceptance {
            aggregate
                .acceptances
                .insert(acceptance.acceptance_id.clone(), acceptance);
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
        validate_record_header(
            rollback.schema_version,
            "rollback_id",
            &rollback.rollback_id,
            rollback.generation,
        )?;
        validate_ref("rollback source", &rollback.from_release)?;
        validate_ref("rollback target", &rollback.to_release)?;
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
        if canonical_release_reference_digest(from_snapshot)? != rollback.from_release.digest
            || canonical_release_reference_digest(to_snapshot)? != rollback.to_release.digest
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
            operation_id: format!("rollback:{tenant_id}:{project_id}:{}", rollback.rollback_id),
            kind: DeliveryEffectKind::Rollback,
            tenant_id: tenant_id.to_string(),
            project: from_manifest.project.clone(),
            candidate: Some(from_manifest.candidate.clone()),
            subject: rollback.from_release.clone(),
            target: Some(rollback.to_release.clone()),
            actor: rollback_authority.principal.clone(),
            actor_authority_receipt_digest: rollback_authority.receipt_digest.clone(),
            actor_authority_identity_digest: rollback_authority.stable_identity_digest()?,
            request_digest: ContentDigest::zero(),
        }
        .seal()?;
        self.require_effect_saga()?;
        let effect_receipt = self.effects.apply(&effect_request)?;
        let authority_after = self.require_current_authority(
            context,
            tenant_id,
            AuthorityRole::ReleaseManager,
            "rollback",
        )?;
        if !same_authority_identity(&authority_after, &rollback_authority) {
            return Err(DeliveryError::StaleEvidence(
                "release authority changed between rollback effect and adoption".to_string(),
            ));
        }
        validate_effect_receipt(
            &effect_request,
            &effect_receipt,
            &authority_after,
            context.now_ms,
        )?;
        rollback.effect_receipt = Some(effect_receipt.effect_ref);
        {
            let from = aggregate
                .releases
                .get_mut(&rollback.from_release.id)
                .ok_or_else(|| DeliveryError::NotFound("rollback source release".to_string()))?;
            if canonical_release_reference_digest(&*from)? != rollback.from_release.digest
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
            if canonical_release_reference_digest(&*to)? != rollback.to_release.digest
            {
                return Err(DeliveryError::StaleEvidence(
                    "rollback target digest mismatch".to_string(),
                ));
            }
            transition_release(to.state, ReleaseState::Active)?;
            to.state = ReleaseState::Active;
            if to.activated_at_ms.is_none() {
                return Err(DeliveryError::CorruptStore(
                    "reactivated release has no original activation timestamp".to_string(),
                ));
            }
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
        validate_record_header(
            closeout.schema_version,
            "closeout_id",
            &closeout.closeout_id,
            closeout.generation,
        )?;
        validate_ref("closeout project", &closeout.project)?;
        validate_ref("closeout release", &closeout.accepted_release)?;
        validate_ref("closeout acceptance", &closeout.acceptance)?;
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
        if closeout.accepted_release != canonical_release_reference(accepted_release)? {
            return Err(DeliveryError::StaleEvidence(
                "closeout release reference is stale or differently digested".to_string(),
            ));
        }
        let manifest = aggregate
            .manifests
            .get(&accepted_release.manifest.id)
            .ok_or_else(|| DeliveryError::MissingEvidence("accepted manifest".to_string()))?;
        let effect_request = DeliveryEffectRequestV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            operation_id: format!("memory:{tenant_id}:{project_id}:{}", closeout.closeout_id),
            kind: DeliveryEffectKind::MemoryPublication,
            tenant_id: tenant_id.to_string(),
            project: closeout.project.clone(),
            candidate: Some(manifest.candidate.clone()),
            subject: closeout.accepted_release.clone(),
            target: None,
            actor: closeout_authority.principal.clone(),
            actor_authority_receipt_digest: closeout_authority.receipt_digest.clone(),
            actor_authority_identity_digest: closeout_authority.stable_identity_digest()?,
            request_digest: ContentDigest::zero(),
        }
        .seal()?;
        self.require_effect_saga()?;
        let effect_receipt = self.effects.apply(&effect_request)?;
        let authority_after = self.require_current_authority(
            context,
            tenant_id,
            AuthorityRole::ReleaseManager,
            "closeout",
        )?;
        if !same_authority_identity(&authority_after, &closeout_authority) {
            return Err(DeliveryError::StaleEvidence(
                "release authority changed between memory publication and closeout".to_string(),
            ));
        }
        validate_effect_receipt(
            &effect_request,
            &effect_receipt,
            &authority_after,
            context.now_ms,
        )?;
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
        // Publication is an external effect. Validate the complete local
        // aggregate/journal/outbox authority before exposing any request to the
        // publisher; a decodable but corrupt outbox row must fail closed without
        // an upstream call.
        self.store.health()?;
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

    fn require_execution_saga(&self) -> Result<(), DeliveryError> {
        require_saga_readiness(
            self.integration.execution_saga_readiness(),
            "workbench_execution_saga",
            &expected_workbench_execution_saga_contract_digest(),
        )
    }

    fn require_effect_saga(&self) -> Result<(), DeliveryError> {
        require_saga_readiness(
            self.effects.readiness(),
            "delivery_effect_saga",
            &expected_effect_saga_contract_digest(),
        )
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

impl<I, E, P> ConfiguredDeliveryCore<I, E, P>
where
    I: DeliveryIntegrationPort,
    E: DeliveryEffectPort,
    P: DeliveryPublicationPort,
{
    pub fn open(
        config: &DeliveryStoreConfigV1,
        integration: I,
        effects: E,
        publication: P,
    ) -> Result<Self, DeliveryError> {
        let store = DeliveryStore::open(config)?;
        Ok(Self {
            core: DeliveryCore::with_ports(store, integration, effects),
            publication,
        })
    }

    pub fn readiness(&self) -> Result<(), DeliveryError> {
        self.core.store.health()?;
        self.core.require_integration()?;
        self.core.require_execution_saga()?;
        self.core.require_effect_saga()?;
        require_saga_readiness(
            self.publication.readiness(),
            "delivery_publication",
            &expected_publication_contract_digest(),
        )
    }

    pub fn command_readiness(&self, command: DeliveryCommandV1) -> Result<(), DeliveryError> {
        self.core.command_readiness(command)
    }

    /// Read-only lineage needs the verified local authority plus the exact
    /// workflow/authentication integration contract. Workbench execution,
    /// delivery effects, and event publication are independent capabilities
    /// and must not hide already committed history when unavailable.
    pub fn lineage_readiness(&self) -> Result<(), DeliveryError> {
        self.core.store.health()?;
        self.core.require_integration()?;
        Ok(())
    }

    /// Productive local commit. Publication readiness is deliberately not
    /// required: the durable local outbox remains the safe hand-off boundary.
    pub fn register_candidate(
        &self,
        context: &CommandContextV1,
        candidate: ReleaseCandidateV1,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        self.core.command_readiness(DeliveryCommandV1::RegisterCandidate)?;
        self.core.register_candidate(context, candidate)
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
        self.core.command_readiness(DeliveryCommandV1::AssignQa)?;
        self.core
            .assign_qa(context, tenant_id, project_id, candidate_id, plan, run)
    }

    pub fn transition_qa(
        &self,
        context: &CommandContextV1,
        tenant_id: &str,
        project_id: &str,
        run_id: &str,
        next: QaRunState,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        self.core.command_readiness(DeliveryCommandV1::TransitionQa)?;
        self.core
            .transition_qa(context, tenant_id, project_id, run_id, next)
    }

    pub fn execute_qa(
        &self,
        context: &CommandContextV1,
        tenant_id: &str,
        project_id: &str,
        run_id: &str,
    ) -> Result<(DeliveryCommitReceiptV1, WorkbenchEvidenceReceiptV1), DeliveryError> {
        self.core.command_readiness(DeliveryCommandV1::ExecuteQa)?;
        self.core.execute_qa(context, tenant_id, project_id, run_id)
    }

    pub fn import_evidence_graph(
        &self,
        context: &CommandContextV1,
        tenant_id: &str,
        project_id: &str,
        run_id: &str,
        graph: QaEvidenceGraphV1,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        self.core
            .command_readiness(DeliveryCommandV1::ImportEvidenceGraph)?;
        self.core
            .import_evidence_graph(context, tenant_id, project_id, run_id, graph)
    }

    pub fn record_gate(
        &self,
        context: &CommandContextV1,
        tenant_id: &str,
        project_id: &str,
        run_id: &str,
        gate: QaReleaseGateReceiptV1,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        self.core.command_readiness(DeliveryCommandV1::RecordGate)?;
        self.core
            .record_gate(context, tenant_id, project_id, run_id, gate)
    }

    #[allow(clippy::too_many_arguments)]
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
        self.core
            .command_readiness(DeliveryCommandV1::RecordReviewBundle)?;
        self.core.record_review_bundle(
            context, tenant_id, project_id, run_id, review, test_run, findings, approval,
        )
    }

    pub fn promote(
        &self,
        context: &CommandContextV1,
        tenant_id: &str,
        project_id: &str,
        candidate_id: &str,
        manifest: ReleaseManifestV1,
        release: ReleaseV1,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        self.core.command_readiness(DeliveryCommandV1::Promote)?;
        self.core
            .promote(context, tenant_id, project_id, candidate_id, manifest, release)
    }

    pub fn issue_delivery(
        &self,
        context: &CommandContextV1,
        project_id: &str,
        receipt: DeliveryReceiptV1,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        self.core
            .command_readiness(DeliveryCommandV1::IssueDelivery)?;
        self.core.issue_delivery(context, project_id, receipt)
    }

    pub fn customer_action(
        &self,
        context: &CommandContextV1,
        tenant_id: &str,
        project_id: &str,
        feedback: CustomerFeedbackV1,
        acceptance: Option<AcceptanceV1>,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        let command = match feedback.action {
            CustomerAction::Accept => DeliveryCommandV1::CustomerAccept,
            CustomerAction::Reject => DeliveryCommandV1::CustomerReject,
            CustomerAction::RequestChanges => DeliveryCommandV1::CustomerRequestChanges,
        };
        self.core.command_readiness(command)?;
        self.core
            .customer_action(context, tenant_id, project_id, feedback, acceptance)
    }

    pub fn rollback(
        &self,
        context: &CommandContextV1,
        tenant_id: &str,
        project_id: &str,
        rollback: RollbackV1,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        self.core.command_readiness(DeliveryCommandV1::Rollback)?;
        self.core.rollback(context, tenant_id, project_id, rollback)
    }

    pub fn closeout(
        &self,
        context: &CommandContextV1,
        tenant_id: &str,
        project_id: &str,
        closeout: ProjectCloseoutV1,
    ) -> Result<DeliveryCommitReceiptV1, DeliveryError> {
        self.core.command_readiness(DeliveryCommandV1::Closeout)?;
        self.core.closeout(context, tenant_id, project_id, closeout)
    }

    pub fn health(&self) -> Result<(), DeliveryError> {
        self.core.store.health()
    }

    pub fn pending_publication_count(&self) -> Result<usize, DeliveryError> {
        Ok(self.core.store.pending_publications()?.len())
    }

    #[doc(hidden)]
    pub fn pending_publications_test_only(
        &self,
    ) -> Result<Vec<super::store::DeliveryOutboxEntryV1>, DeliveryError> {
        self.core.store.pending_publications()
    }

    pub fn read_public_lineage(
        &self,
        context: &CommandContextV1,
        tenant_id: &str,
        project_id: &str,
    ) -> Result<PublicDeliveryLineageDtoV1, DeliveryError> {
        self.lineage_readiness()?;
        self.core
            .read_public_lineage_authorized(context, tenant_id, project_id)
    }

    pub fn publish_pending(&self) -> Result<usize, DeliveryError> {
        require_saga_readiness(
            self.publication.readiness(),
            "delivery_publication",
            &expected_publication_contract_digest(),
        )?;
        self.core.publish_pending(&self.publication)
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

fn lineage_role(principal: &PrincipalV1) -> Option<AuthorityRole> {
    [
        AuthorityRole::Auditor,
        AuthorityRole::ReleaseManager,
        AuthorityRole::Customer,
        AuthorityRole::GaiaObserver,
    ]
    .into_iter()
    .find(|role| principal.has_role(role.clone()))
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

pub fn qa_case_attempt_history_digest(
    graph: &QaEvidenceGraphV1,
) -> Result<ContentDigest, DeliveryError> {
    let histories: Vec<_> = graph
        .case_results
        .iter()
        .map(|result| {
            (
                &result.result_id,
                result.generation,
                &result.attempt_history,
            )
        })
        .collect();
    ContentDigest::of_domain("qa-case-attempt-history", DELIVERY_SCHEMA_V1, &histories)
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

fn canonical_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn validate_record_header(
    schema_version: u16,
    id_name: &str,
    id: &str,
    generation: u64,
) -> Result<(), DeliveryError> {
    if schema_version != DELIVERY_SCHEMA_V1 || generation == 0 || !canonical_id(id) {
        return Err(DeliveryError::Validation(format!(
            "{id_name} schema, identifier, or generation is invalid"
        )));
    }
    Ok(())
}

fn validate_ref(name: &str, value: &VersionedRefV1) -> Result<(), DeliveryError> {
    if !canonical_id(&value.id) || value.generation == 0 || value.digest == ContentDigest::zero() {
        return Err(DeliveryError::Validation(format!(
            "{name} is not a canonical versioned reference"
        )));
    }
    Ok(())
}

fn validate_cost_ref(name: &str, value: &super::schema::CostRefV1) -> Result<(), DeliveryError> {
    if !canonical_id(&value.ledger_id)
        || value.generation == 0
        || value.digest == ContentDigest::zero()
        || value.currency != "USD"
    {
        return Err(DeliveryError::Validation(format!(
            "{name} is not a canonical USD minor-unit ledger reference"
        )));
    }
    Ok(())
}

fn candidate_authority_query(candidate: &ReleaseCandidateV1) -> CandidateAuthorityQueryV1 {
    CandidateAuthorityQueryV1 {
        tenant_id: candidate.tenant_id.clone(),
        agreement: candidate.agreement.clone(),
        project: candidate.project.clone(),
        work_items_digest: candidate.work_items_digest.clone(),
        candidate_digest: candidate.candidate_digest.clone(),
    }
}

fn validate_candidate_authority(
    authority: &CandidateAuthoritySnapshotV1,
    candidate: &ReleaseCandidateV1,
    expected_authority_generation: u64,
) -> Result<(), DeliveryError> {
    let mut participant_ids = BTreeSet::new();
    let participant_inventory_valid = !authority.participant_principals.is_empty()
        && authority.participant_principals.iter().all(|principal| {
            principal.tenant_id == candidate.tenant_id
                && canonical_id(&principal.principal_id)
                && principal.authority_generation > 0
                && !principal.roles.is_empty()
                && participant_ids.insert(principal.principal_id.clone())
        });
    if authority.schema_version != DELIVERY_SCHEMA_V1
        || authority.authority_generation != expected_authority_generation
        || authority.agreement != candidate.agreement
        || authority.project != candidate.project
        || authority.work_items_digest != candidate.work_items_digest
        || authority.current_candidate_generation != candidate.generation
        || authority.current_candidate_digest != candidate.candidate_digest
        || authority.snapshot_digest != authority.computed_digest()?
        || !participant_inventory_valid
    {
        return Err(DeliveryError::StaleEvidence(
            "workflow candidate authority is stale, malformed, or differently bound".to_string(),
        ));
    }
    Ok(())
}

fn require_candidate_participant(
    authority: &CandidateAuthoritySnapshotV1,
    principal: &PrincipalV1,
    role: AuthorityRole,
) -> Result<(), DeliveryError> {
    if authority.participant_principals.iter().any(|participant| {
        participant.tenant_id == principal.tenant_id
            && participant.principal_id == principal.principal_id
            && participant.authority_generation == principal.authority_generation
            && participant.has_role(role.clone())
    }) {
        Ok(())
    } else {
        Err(DeliveryError::AuthorityDenied(
            "authenticated principal is not a current workflow participant".to_string(),
        ))
    }
}

fn require_candidate_participant_id(
    authority: &CandidateAuthoritySnapshotV1,
    tenant_id: &str,
    principal_id: &str,
    role: AuthorityRole,
) -> Result<PrincipalV1, DeliveryError> {
    if let Some(participant) = authority.participant_principals.iter().find(|participant| {
        participant.tenant_id == tenant_id
            && participant.principal_id == principal_id
            && participant.has_role(role.clone())
    }) {
        Ok(participant.clone())
    } else {
        Err(DeliveryError::AuthorityDenied(
            "referenced principal is not a current workflow participant".to_string(),
        ))
    }
}

fn validate_data_control(name: &str, value: &DataControlV1) -> Result<(), DeliveryError> {
    if !canonical_id(&value.classification)
        || !canonical_id(&value.encryption_key_owner)
        || value.access_policy_digest == ContentDigest::zero()
        || value.redaction_policy_digest == ContentDigest::zero()
        || value.audit_policy_digest == ContentDigest::zero()
    {
        return Err(DeliveryError::Validation(format!(
            "{name} data-control policy is incomplete"
        )));
    }
    validate_ref(
        &format!("{name} retention frontier"),
        &value.retention_frontier,
    )
}

fn validate_source_tuple(name: &str, value: &SourceTupleV1) -> Result<(), DeliveryError> {
    if !canonical_id(&value.owner)
        || !canonical_id(&value.source_type)
        || !canonical_id(&value.id)
        || value.generation == 0
        || value.digest == ContentDigest::zero()
    {
        return Err(DeliveryError::Validation(format!(
            "{name} source tuple is incomplete or malformed"
        )));
    }
    Ok(())
}

fn validate_qa_plan(plan: &QaEvaluationPlanV1) -> Result<(), DeliveryError> {
    validate_record_header(
        plan.schema_version,
        "plan_id",
        &plan.plan_id,
        plan.generation,
    )?;
    for (name, value) in [
        ("QA request", &plan.request),
        ("QA candidate", &plan.candidate),
        ("QA agreement", &plan.agreement),
        ("QA project", &plan.project),
    ] {
        validate_ref(name, value)?;
    }
    if plan.work_items_digest == ContentDigest::zero()
        || plan.acceptance_criteria_digest == ContentDigest::zero()
        || plan.fixture_inventory_digest == ContentDigest::zero()
        || plan.evaluator_policy_digest == ContentDigest::zero()
        || plan.aggregation_policy_digest == ContentDigest::zero()
        || plan.release_policy_digest == ContentDigest::zero()
        || plan.runner_binary_digest == ContentDigest::zero()
        || plan.toolchain_digest == ContentDigest::zero()
        || plan.sandbox_profile_digest == ContentDigest::zero()
        || plan.capability_digest == ContentDigest::zero()
        || plan.environment_digest == ContentDigest::zero()
        || plan.credential_policy_digest == ContentDigest::zero()
        || plan.required_case_ids.is_empty()
        || (plan.retry_limit == 0 && !plan.retryable_classes.is_empty())
        || (plan.retry_limit > 0 && plan.retryable_classes.is_empty())
        || plan
            .required_case_ids
            .iter()
            .chain(&plan.optional_case_ids)
            .any(|case_id| !canonical_id(case_id))
        || plan
            .required_case_ids
            .iter()
            .any(|case_id| plan.optional_case_ids.contains(case_id))
        || plan
            .retryable_classes
            .iter()
            .any(|class| !canonical_id(class))
        || plan.plan_digest != plan.computed_digest()?
    {
        return Err(DeliveryError::Validation(
            "QA plan is incomplete, ambiguous, or has an invalid digest".to_string(),
        ));
    }
    validate_data_control("QA plan", &plan.data_control)
}

fn validate_dataset_case(case: &QaDatasetCaseV1) -> Result<(), DeliveryError> {
    validate_record_header(
        case.schema_version,
        "dataset case_id",
        &case.case_id,
        case.generation,
    )?;
    if !canonical_id(&case.required_class)
        || case.input_digest == ContentDigest::zero()
        || case.oracle_digest == ContentDigest::zero()
        || case.provenance.is_empty()
        || case.license.trim().is_empty()
        || case.access_policy_digest == ContentDigest::zero()
        || case.contamination_policy_digest == ContentDigest::zero()
        || case.retired_at_ms.is_some()
        || case.superseded_by.is_some()
        || case.slices.is_empty()
        || case
            .slices
            .iter()
            .any(|(key, value)| !canonical_id(key) || !canonical_id(value))
    {
        return Err(DeliveryError::MissingEvidence(format!(
            "dataset case {} is incomplete, retired, or superseded",
            case.case_id
        )));
    }
    validate_data_control("dataset case", &case.data_control)?;
    require_unique_source_tuples("dataset provenance", &case.provenance)
}

fn require_unique_source_tuples(
    name: &str,
    sources: &[SourceTupleV1],
) -> Result<(), DeliveryError> {
    if sources.is_empty() {
        return Err(DeliveryError::MissingEvidence(format!(
            "{name} source inventory is empty"
        )));
    }
    let mut seen_locators = std::collections::BTreeMap::new();
    for source in sources {
        validate_source_tuple(name, source)?;
        let locator = (
            source.owner.as_str(),
            source.source_type.as_str(),
            source.id.as_str(),
            source.generation,
        );
        if let Some(existing_digest) = seen_locators.insert(locator, &source.digest) {
            let conflict = if existing_digest == &source.digest {
                "an exactly duplicated source tuple"
            } else {
                "one immutable locator generation with conflicting digests"
            };
            return Err(DeliveryError::Conflict(format!(
                "{name} contains {conflict}"
            )));
        }
    }
    Ok(())
}

fn validate_graph_source_immutability(graph: &QaEvidenceGraphV1) -> Result<(), DeliveryError> {
    let mut digests_by_locator = std::collections::BTreeMap::new();
    for source in graph
        .dataset_cases
        .iter()
        .flat_map(|case| case.provenance.iter())
        .chain(
            graph
                .case_results
                .iter()
                .flat_map(|result| result.sources.iter()),
        )
    {
        let locator = (
            source.owner.as_str(),
            source.source_type.as_str(),
            source.id.as_str(),
            source.generation,
        );
        if let Some(existing_digest) = digests_by_locator.insert(locator, &source.digest) {
            if existing_digest != &source.digest {
                return Err(DeliveryError::Conflict(
                    "one source locator generation carries conflicting digests across the evidence graph"
                        .to_string(),
                ));
            }
        }
    }
    Ok(())
}

pub fn qa_fixture_inventory_digest(
    cases: &[QaDatasetCaseV1],
) -> Result<ContentDigest, DeliveryError> {
    let inventory: std::collections::BTreeMap<_, _> = cases
        .iter()
        .map(|case| {
            Ok((
                case.case_id.clone(),
                VersionedRefV1 {
                    id: case.case_id.clone(),
                    generation: case.generation,
                    digest: ContentDigest::of_domain("qa-dataset-case", DELIVERY_SCHEMA_V1, case)?,
                },
            ))
        })
        .collect::<Result<_, DeliveryError>>()?;
    if inventory.len() != cases.len() {
        return Err(DeliveryError::Conflict(
            "fixture inventory contains duplicate case identifiers".to_string(),
        ));
    }
    ContentDigest::of_domain("qa-fixture-inventory", DELIVERY_SCHEMA_V1, &inventory)
}

fn require_unique_ids<'a>(
    record_type: &str,
    ids: impl Iterator<Item = &'a str>,
) -> Result<(), DeliveryError> {
    let mut seen = BTreeSet::new();
    for id in ids {
        if !canonical_id(id) {
            return Err(DeliveryError::Validation(format!(
                "{record_type} has a non-canonical identifier"
            )));
        }
        if !seen.insert(id) {
            return Err(DeliveryError::Conflict(format!(
                "duplicate {record_type} identifier {id}"
            )));
        }
    }
    Ok(())
}

fn legal_case_reason(outcome: QaCaseOutcome, reason: QaCaseReasonCode) -> bool {
    matches!(
        (outcome, reason),
        (QaCaseOutcome::Pass, QaCaseReasonCode::Verified)
            | (QaCaseOutcome::Fail, QaCaseReasonCode::AssertionFailed)
            | (QaCaseOutcome::Fail, QaCaseReasonCode::ModelRejected)
            | (QaCaseOutcome::Error, QaCaseReasonCode::HarnessError)
            | (QaCaseOutcome::Skipped, QaCaseReasonCode::SkippedByPolicy)
            | (
                QaCaseOutcome::Unscored | QaCaseOutcome::NeedsHumanReview,
                QaCaseReasonCode::NeedsHumanReview
            )
            | (
                QaCaseOutcome::FlakyUnresolved,
                QaCaseReasonCode::FlakyUnresolved
            )
    )
}

fn validate_case_attempt_history(
    result: &super::schema::QaCaseResultV1,
    graph: &QaEvidenceGraphV1,
    plan: &QaEvaluationPlanV1,
    case_digest: &ContentDigest,
) -> Result<(), DeliveryError> {
    if result.attempt_history.is_empty()
        || usize::from(result.attempts) != result.attempt_history.len()
    {
        return Err(DeliveryError::MissingEvidence(
            "case attempt history is empty or does not match the declared attempt count"
                .to_string(),
        ));
    }
    require_unique_ids(
        "case attempt",
        result
            .attempt_history
            .iter()
            .map(|attempt| attempt.attempt_id.as_str()),
    )?;
    let mut referenced = BTreeSet::new();
    let mut earlier_deterministic_failure = false;
    for (index, attempt) in result.attempt_history.iter().enumerate() {
        let expected_number = u16::try_from(index + 1).map_err(|_| {
            DeliveryError::Validation("case attempt inventory exceeds u16".to_string())
        })?;
        if attempt.schema_version != DELIVERY_SCHEMA_V1
            || attempt.generation == 0
            || attempt.attempt_number != expected_number
            || attempt.run != result.run
            || attempt.case_ref != result.case_ref
            || !legal_case_reason(attempt.outcome, attempt.reason_code)
            || attempt.attempt_digest != attempt.computed_digest()?
        {
            return Err(DeliveryError::StaleEvidence(
                "case attempt history has a gap or stale run/case/digest binding".to_string(),
            ));
        }
        require_unique_ids(
            "case attempt assertion reference",
            attempt.assertion_refs.iter().map(|value| value.id.as_str()),
        )?;
        if attempt.outcome == QaCaseOutcome::Pass && attempt.assertion_refs.is_empty() {
            return Err(DeliveryError::MissingEvidence(
                "passing case attempt has no deterministic evidence".to_string(),
            ));
        }
        let mut has_failed_assertion = false;
        for assertion_ref in &attempt.assertion_refs {
            let assertion = graph
                .deterministic_results
                .iter()
                .find(|value| value.assertion_id == assertion_ref.id)
                .ok_or_else(|| {
                    DeliveryError::MissingEvidence(
                        "case attempt deterministic assertion".to_string(),
                    )
                })?;
            let expected_digest =
                ContentDigest::of_domain("qa-deterministic-result", DELIVERY_SCHEMA_V1, assertion)?;
            if assertion_ref.generation != assertion.generation
                || assertion_ref.digest != expected_digest
                || assertion.plan_digest != plan.plan_digest
                || assertion.case_digest != *case_digest
            {
                return Err(DeliveryError::StaleEvidence(
                    "case attempt assertion reference is stale or differently bound".to_string(),
                ));
            }
            if attempt.outcome == QaCaseOutcome::Pass && !assertion.passed {
                return Err(DeliveryError::MissingEvidence(
                    "failed deterministic assertion cannot support a passing attempt".to_string(),
                ));
            }
            has_failed_assertion |= !assertion.passed;
            if !referenced.insert((
                assertion_ref.id.clone(),
                assertion_ref.generation,
                assertion_ref.digest.clone(),
            )) {
                return Err(DeliveryError::Conflict(
                    "deterministic attempt evidence was reused across attempts".to_string(),
                ));
            }
        }
        if attempt.outcome == QaCaseOutcome::Fail && !has_failed_assertion {
            return Err(DeliveryError::MissingEvidence(
                "failed case attempt has no failed deterministic assertion".to_string(),
            ));
        }
        if index + 1 < result.attempt_history.len() && attempt.outcome == QaCaseOutcome::Fail {
            earlier_deterministic_failure = true;
        }
    }
    let parent_references: BTreeSet<_> = result
        .assertion_refs
        .iter()
        .map(|value| (value.id.clone(), value.generation, value.digest.clone()))
        .collect();
    if parent_references.len() != result.assertion_refs.len() || parent_references != referenced {
        return Err(DeliveryError::MissingEvidence(
            "case result does not retain the exact union of attempt evidence".to_string(),
        ));
    }
    let final_attempt = result
        .attempt_history
        .last()
        .ok_or_else(|| DeliveryError::MissingEvidence("case attempt history".to_string()))?;
    if earlier_deterministic_failure && final_attempt.outcome == QaCaseOutcome::Pass {
        if result.disposition.is_none()
            || !matches!(
                (result.outcome, result.reason_code),
                (QaCaseOutcome::Pass, QaCaseReasonCode::Verified)
                    | (
                        QaCaseOutcome::FlakyUnresolved,
                        QaCaseReasonCode::FlakyUnresolved
                    )
            )
        {
            return Err(DeliveryError::MissingEvidence(
                "a later pass cannot hide an earlier deterministic failure".to_string(),
            ));
        }
    } else if result.outcome != final_attempt.outcome
        || result.reason_code != final_attempt.reason_code
    {
        return Err(DeliveryError::StaleEvidence(
            "final case status is not derived from the terminal attempt".to_string(),
        ));
    }
    Ok(())
}

fn legal_flake_disposition(
    outcome: QaCaseOutcome,
    disposition: &super::schema::QaFlakeDispositionV1,
) -> bool {
    use super::schema::{QaFlakeClassification, QaFlakeReason};

    matches!(
        (outcome, disposition.reason, disposition.classification),
        (
            QaCaseOutcome::Pass,
            QaFlakeReason::RetryPassed,
            QaFlakeClassification::Infrastructure
                | QaFlakeClassification::ModelVariance
                | QaFlakeClassification::TestHarness
        ) | (
            QaCaseOutcome::NeedsHumanReview,
            QaFlakeReason::KnownInfrastructure | QaFlakeReason::EvaluatorVariance,
            QaFlakeClassification::Infrastructure
                | QaFlakeClassification::ModelVariance
                | QaFlakeClassification::TestHarness
        ) | (
            QaCaseOutcome::FlakyUnresolved,
            QaFlakeReason::Unresolved,
            QaFlakeClassification::Infrastructure
                | QaFlakeClassification::ModelVariance
                | QaFlakeClassification::TestHarness
                | QaFlakeClassification::ProductDefect
        )
    )
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

pub fn validate_qa_evidence_graph(
    plan: &QaEvaluationPlanV1,
    run_ref: &VersionedRefV1,
    graph: &QaEvidenceGraphV1,
    qa_authority: &PrincipalV1,
    now_ms: u64,
) -> Result<(), DeliveryError> {
    validate_qa_plan(plan)?;
    if graph.schema_version != DELIVERY_SCHEMA_V1
        || graph.run != *run_ref
        || graph.graph_digest != graph.computed_digest()?
    {
        return Err(DeliveryError::StaleEvidence(
            "evidence graph schema, run, or digest is invalid".to_string(),
        ));
    }
    if !graph.model_results.is_empty()
        || graph
            .case_results
            .iter()
            .any(|result| !result.grader_refs.is_empty())
    {
        return Err(DeliveryError::AdapterUnavailable {
            dependency: "qa_model_evidence_#749",
            reason: "model evidence and calibration remain unavailable until #749 supplies their authoritative contract".to_string(),
        });
    }
    require_unique_ids(
        "dataset case",
        graph
            .dataset_cases
            .iter()
            .map(|value| value.case_id.as_str()),
    )?;
    require_unique_ids(
        "case result",
        graph
            .case_results
            .iter()
            .map(|value| value.result_id.as_str()),
    )?;
    require_unique_ids(
        "deterministic assertion",
        graph
            .deterministic_results
            .iter()
            .map(|value| value.assertion_id.as_str()),
    )?;
    require_unique_ids(
        "flake disposition",
        graph
            .flake_dispositions
            .iter()
            .map(|value| value.disposition_id.as_str()),
    )?;
    let case_ids: BTreeSet<_> = graph
        .dataset_cases
        .iter()
        .map(|case| case.case_id.as_str())
        .collect();
    let planned_case_ids: BTreeSet<_> = plan
        .required_case_ids
        .iter()
        .chain(&plan.optional_case_ids)
        .map(String::as_str)
        .collect();
    for case in &graph.dataset_cases {
        validate_dataset_case(case)?;
    }
    if case_ids != planned_case_ids
        || graph
            .dataset_cases
            .iter()
            .any(|case| case.required != plan.required_case_ids.contains(&case.case_id))
        || qa_fixture_inventory_digest(&graph.dataset_cases)? != plan.fixture_inventory_digest
    {
        return Err(DeliveryError::MissingEvidence(
            "dataset inventory does not exactly match the digest-bound QA fixture plan".to_string(),
        ));
    }
    require_unique_ids(
        "case result dataset case",
        graph
            .case_results
            .iter()
            .map(|result| result.case_ref.id.as_str()),
    )?;
    let required_result_case_ids: BTreeSet<_> = graph
        .case_results
        .iter()
        .filter(|result| result.required)
        .map(|result| result.case_ref.id.as_str())
        .collect();
    let planned_required_case_ids: BTreeSet<_> =
        plan.required_case_ids.iter().map(String::as_str).collect();
    if required_result_case_ids != planned_required_case_ids {
        return Err(DeliveryError::MissingEvidence(
            "required QA case results are missing, duplicated, or differently bound".to_string(),
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
            || result.generation == 0
            || !canonical_id(&result.result_id)
            || result.run != *run_ref
            || result.case_ref.generation != case.generation
            || result.case_ref.digest != case_digest
            || result.required != case.required
            || !legal_case_reason(result.outcome, result.reason_code)
            || result.attempts == 0
            || result.slices != case.slices
        {
            return Err(DeliveryError::StaleEvidence(
                "case result is bound to another run or dataset case".to_string(),
            ));
        }
        validate_case_attempt_history(result, graph, plan, &case_digest)?;
        require_unique_source_tuples("case result", &result.sources)?;
        if result.outcome == QaCaseOutcome::Pass && result.assertion_refs.is_empty() {
            return Err(DeliveryError::MissingEvidence(
                "PASS case has no deterministic assertion evidence; model grading is unavailable until #749".to_string(),
            ));
        }
        if result.outcome == QaCaseOutcome::FlakyUnresolved && result.disposition.is_none() {
            return Err(DeliveryError::MissingEvidence(
                "unresolved flaky result requires a current authorized disposition".to_string(),
            ));
        }
        require_unique_ids(
            "case assertion reference",
            result.assertion_refs.iter().map(|value| value.id.as_str()),
        )?;
        for assertion_ref in &result.assertion_refs {
            let assertion = graph
                .deterministic_results
                .iter()
                .find(|value| value.assertion_id == assertion_ref.id)
                .ok_or_else(|| {
                    DeliveryError::MissingEvidence("deterministic assertion".to_string())
                })?;
            if assertion.schema_version != DELIVERY_SCHEMA_V1
                || assertion.generation == 0
                || !canonical_id(&assertion.assertion_id)
                || assertion.plan_digest != plan.plan_digest
                || assertion.case_digest != case_digest
                || assertion.assertion_digest == ContentDigest::zero()
                || assertion.oracle_digest != case.oracle_digest
                || assertion.input_digest != case.input_digest
                || assertion.evidence_digest == ContentDigest::zero()
                || assertion.actual_digest == ContentDigest::zero()
                || assertion_ref.generation != assertion.generation
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
            let retained_historical_failure = result.disposition.is_some()
                && result
                    .attempt_history
                    .iter()
                    .take(result.attempt_history.len().saturating_sub(1))
                    .any(|attempt| attempt.assertion_refs.contains(assertion_ref));
            if !assertion.passed
                && result.outcome == QaCaseOutcome::Pass
                && !retained_historical_failure
            {
                return Err(DeliveryError::MissingEvidence(
                    "failed deterministic assertion cannot support PASS".to_string(),
                ));
            }
        }
        if let Some(disposition_ref) = &result.disposition {
            validate_ref("case flake disposition", disposition_ref)?;
            let disposition = graph
                .flake_dispositions
                .iter()
                .find(|value| value.disposition_id == disposition_ref.id)
                .ok_or_else(|| DeliveryError::MissingEvidence("flake disposition".to_string()))?;
            if disposition_ref.generation != disposition.generation
                || disposition_ref.digest
                    != ContentDigest::of_domain(
                        "qa-flake-disposition",
                        DELIVERY_SCHEMA_V1,
                        disposition,
                    )?
            {
                return Err(DeliveryError::StaleEvidence(
                    "case flake disposition reference is stale".to_string(),
                ));
            }
        }
    }
    validate_graph_source_immutability(graph)?;
    let referenced_assertions: BTreeSet<_> = graph
        .case_results
        .iter()
        .flat_map(|result| result.assertion_refs.iter().map(|value| value.id.as_str()))
        .collect();
    if referenced_assertions.len() != graph.deterministic_results.len()
        || graph
            .deterministic_results
            .iter()
            .any(|value| !referenced_assertions.contains(value.assertion_id.as_str()))
    {
        return Err(DeliveryError::MissingEvidence(
            "deterministic evidence inventory contains missing or unreferenced records".to_string(),
        ));
    }
    for disposition in &graph.flake_dispositions {
        validate_ref("flake result", &disposition.result)?;
        validate_ref("flake defect", &disposition.defect_ref)?;
        validate_ref(
            "flake deterministic regression fixture",
            &disposition.deterministic_regression_fixture,
        )?;
        let result = graph
            .case_results
            .iter()
            .find(|value| value.result_id == disposition.result.id)
            .ok_or_else(|| DeliveryError::MissingEvidence("flake result".to_string()))?;
        if disposition.schema_version != DELIVERY_SCHEMA_V1
            || disposition.generation == 0
            || !canonical_id(&disposition.disposition_id)
            || disposition.owner != *qa_authority
            || disposition.policy_revision != plan.generation
            || disposition.expires_at_ms <= now_ms
            || result.disposition.as_ref().map(|value| value.id.as_str())
                != Some(disposition.disposition_id.as_str())
            || !legal_flake_disposition(result.outcome, disposition)
            || disposition.result.generation != result.generation
            || disposition.result.digest != qa_case_result_binding_digest(result)?
        {
            return Err(DeliveryError::StaleEvidence(
                "flake disposition references another result".to_string(),
            ));
        }
        let regression = graph
            .deterministic_results
            .iter()
            .find(|value| value.assertion_id == disposition.deterministic_regression_fixture.id)
            .ok_or_else(|| {
                DeliveryError::MissingEvidence(
                    "flake deterministic regression evidence".to_string(),
                )
            })?;
        let regression_digest =
            ContentDigest::of_domain("qa-deterministic-result", DELIVERY_SCHEMA_V1, regression)?;
        let regression_ref = &disposition.deterministic_regression_fixture;
        let final_attempt = result
            .attempt_history
            .last()
            .ok_or_else(|| DeliveryError::MissingEvidence("case attempt history".to_string()))?;
        if regression_ref.generation != regression.generation
            || regression_ref.digest != regression_digest
            || !regression.passed
            || regression.plan_digest != plan.plan_digest
            || regression.case_digest != result.case_ref.digest
            || !result.assertion_refs.contains(regression_ref)
            || !final_attempt.assertion_refs.contains(regression_ref)
        {
            return Err(DeliveryError::StaleEvidence(
                "flake deterministic regression references another result".to_string(),
            ));
        }
    }
    Ok(())
}

pub fn qa_case_result_binding_digest(
    result: &super::schema::QaCaseResultV1,
) -> Result<ContentDigest, DeliveryError> {
    let mut binding = result.clone();
    if let Some(disposition) = &mut binding.disposition {
        disposition.digest = ContentDigest::zero();
    }
    ContentDigest::of_domain("qa-case-result-binding", DELIVERY_SCHEMA_V1, &binding)
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
                && required.iter().all(|result| {
                    result.attempt_history.last().is_some_and(|attempt| {
                        attempt.outcome == QaCaseOutcome::Pass
                            && attempt.assertion_refs.iter().all(|assertion_ref| {
                                graph.deterministic_results.iter().any(|assertion| {
                                    assertion.assertion_id == assertion_ref.id && assertion.passed
                                })
                            })
                    })
                })
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
        || receipt.operation_id != request.operation_id
        || receipt.kind != request.kind
        || receipt.tenant_id != request.tenant_id
        || receipt.project != request.project
        || receipt.candidate != request.candidate
        || receipt.subject != request.subject
        || receipt.target != request.target
        || receipt.actor != request.actor
        || receipt.request_digest != request.request_digest
        // The effect receipt remains immutable across reconciliation. Its original
        // authority receipt must be present, while stable identity is checked below.
        || receipt.actor_authority_receipt_digest == ContentDigest::zero()
        || receipt.actor_authority_identity_digest != request.actor_authority_identity_digest
        || receipt.actor_authority_identity_digest != authority.stable_identity_digest()?
        || request.actor != authority.principal
        || receipt.effect_ref.digest == ContentDigest::zero()
        || receipt.effect_ref.generation == 0
        || receipt.effect_ref.id.is_empty()
        || receipt.issuer.is_empty()
        || receipt.issued_at_ms > now_ms
    {
        return Err(DeliveryError::StaleEvidence(
            "external effect receipt is absent, stale, ambiguous, or differently bound".to_string(),
        ));
    }
    Ok(())
}

fn same_authority_identity(left: &AuthorityReceiptV1, right: &AuthorityReceiptV1) -> bool {
    left.principal == right.principal
        && left.contract_version == right.contract_version
        && left.contract_authority_generation == right.contract_authority_generation
        && left.contract_digest == right.contract_digest
        && left.issuer == right.issuer
}

fn require_saga_readiness(
    readiness: AdapterReadiness,
    dependency: &'static str,
    expected_digest: &ContentDigest,
) -> Result<(), DeliveryError> {
    match readiness {
        AdapterReadiness::Ready {
            contract_version,
            authority_generation,
            contract_digest,
        } if contract_version == DELIVERY_SCHEMA_V1
            && authority_generation > 0
            && &contract_digest == expected_digest =>
        {
            Ok(())
        }
        AdapterReadiness::Ready { .. } => Err(DeliveryError::StaleEvidence(format!(
            "{dependency} contract version, generation, or digest is not current"
        ))),
        AdapterReadiness::Unavailable { reason } => {
            Err(DeliveryError::AdapterUnavailable { dependency, reason })
        }
    }
}
