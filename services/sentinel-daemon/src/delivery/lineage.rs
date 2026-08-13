use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{
    digest::ContentDigest,
    error::DeliveryError,
    ports::{
        WorkflowLineageKindV1, WorkflowLineageQueryV1, WorkflowLineageSnapshotV1,
        WorkflowLineageStateV1,
    },
    schema::{
        AuthorityRole, CandidateState, CostRefV1, DeliveryReceiptV1, DeliveryState, QaRunState,
        ReleaseCandidateV1, ReleaseState, ReleaseV1, VersionedRefV1, DELIVERY_SCHEMA_V1,
    },
    state::DeliveryAggregateV1,
};

pub fn validate_delivery_aggregate_references(
    aggregate: &DeliveryAggregateV1,
) -> Result<(), DeliveryError> {
    let corrupt = |detail: &str| {
        DeliveryError::CorruptStore(format!("delivery reference integrity: {detail}"))
    };

    for candidate in aggregate.candidates.values() {
        if !matches_candidate_seal(candidate)? || validate_cost(candidate.cost.clone()).is_err() {
            return Err(corrupt("release candidate self-digest or cost is invalid"));
        }
    }
    for plan in aggregate.qa_plans.values() {
        if plan.plan_digest != plan.computed_digest()? {
            return Err(corrupt("QA plan self-digest is invalid"));
        }
    }
    for receipt in aggregate.workbench_receipts.values() {
        if receipt.receipt_digest != receipt.computed_digest()? {
            return Err(corrupt("workbench receipt self-digest is invalid"));
        }
        let run = aggregate
            .qa_runs
            .get(&receipt.qa_run.id)
            .ok_or_else(|| corrupt("workbench receipt QA run is missing"))?;
        if receipt.qa_run.id != run.run_id
            || receipt.qa_run.generation != run.generation
            || receipt.qa_run.digest != run.request_digest
            || receipt.assignment != receipt.qa_run
        {
            return Err(corrupt("workbench receipt QA run reference is stale"));
        }
    }
    for graph in aggregate.evidence_graphs.values() {
        if graph.graph_digest != graph.computed_digest()?
            || graph
                .case_results
                .iter()
                .flat_map(|result| result.attempt_history.iter())
                .any(|attempt| {
                    attempt.computed_digest().ok() != Some(attempt.attempt_digest.clone())
                })
        {
            return Err(corrupt("QA evidence graph self-digest is invalid"));
        }
    }
    for manifest in aggregate.manifests.values() {
        if manifest.manifest_digest != manifest.computed_digest()?
            || validate_cost(manifest.cost.clone()).is_err()
        {
            return Err(corrupt("release manifest self-digest or cost is invalid"));
        }
    }
    for delivery in aggregate.deliveries.values() {
        if !matches_delivery_seal(delivery)? {
            return Err(corrupt("delivery receipt self-digest is invalid"));
        }
    }
    for feedback in aggregate.feedback.values() {
        if feedback.feedback_digest != feedback.computed_digest()? {
            return Err(corrupt("customer feedback self-digest is invalid"));
        }
    }
    for acceptance in aggregate.acceptances.values() {
        if acceptance.acceptance_digest != acceptance.computed_digest()? {
            return Err(corrupt("customer acceptance self-digest is invalid"));
        }
    }

    // Check required endpoints before detailed binding validation so corruption
    // is reported at the missing edge rather than at an unrelated dependent row.
    for delivery in aggregate.deliveries.values() {
        if !aggregate.releases.contains_key(&delivery.release.id) {
            return Err(corrupt("delivery release is missing"));
        }
    }
    for acceptance in aggregate.acceptances.values() {
        if !aggregate.deliveries.contains_key(&acceptance.delivery.id) {
            return Err(corrupt("acceptance delivery is missing"));
        }
    }
    for rollback in aggregate.rollbacks.values() {
        if !aggregate.releases.contains_key(&rollback.from_release.id) {
            return Err(corrupt("rollback source release is missing"));
        }
        if !aggregate.releases.contains_key(&rollback.to_release.id) {
            return Err(corrupt("rollback target release is missing"));
        }
    }
    for closeout in aggregate.closeouts.values() {
        if !aggregate
            .releases
            .contains_key(&closeout.accepted_release.id)
        {
            return Err(corrupt("closeout accepted release is missing"));
        }
    }

    for plan in aggregate.qa_plans.values() {
        let candidate = aggregate
            .candidates
            .get(&plan.candidate.id)
            .ok_or_else(|| corrupt("QA plan candidate is missing"))?;
        if plan.candidate.generation != candidate.generation
            || plan.candidate.digest != candidate.candidate_digest
        {
            return Err(corrupt("QA plan candidate reference is stale"));
        }
    }
    for run in aggregate.qa_runs.values() {
        let plan = aggregate
            .qa_plans
            .get(&run.plan.id)
            .ok_or_else(|| corrupt("QA run plan is missing"))?;
        if run.plan.generation != plan.generation || run.plan.digest != plan.plan_digest {
            return Err(corrupt("QA run plan reference is stale"));
        }
        if let Some(gate_ref) = &run.gate_receipt {
            let gate = aggregate
                .gates
                .get(&gate_ref.id)
                .ok_or_else(|| corrupt("QA run gate is missing"))?;
            let digest = ContentDigest::of_domain("qa-release-gate", DELIVERY_SCHEMA_V1, gate)?;
            if gate_ref.generation != gate.generation || gate_ref.digest != digest {
                return Err(corrupt("QA run gate reference is stale"));
            }
        }
    }
    for graph in aggregate.evidence_graphs.values() {
        let run = aggregate
            .qa_runs
            .get(&graph.run.id)
            .ok_or_else(|| corrupt("QA evidence run is missing"))?;
        let receipt = aggregate
            .workbench_receipts
            .get(&graph.workbench_receipt.id)
            .ok_or_else(|| corrupt("QA evidence workbench receipt is missing"))?;
        if graph.run.generation != run.generation
            || graph.run.digest != run.request_digest
            || graph.workbench_receipt.generation != receipt.invocation.generation
            || graph.workbench_receipt.digest != receipt.receipt_digest
        {
            return Err(corrupt("QA evidence graph reference is stale"));
        }
    }
    for gate in aggregate.gates.values() {
        let candidate = aggregate
            .candidates
            .get(&gate.candidate.id)
            .ok_or_else(|| corrupt("QA gate candidate is missing"))?;
        let plan = aggregate
            .qa_plans
            .get(&gate.plan.id)
            .ok_or_else(|| corrupt("QA gate plan is missing"))?;
        let gate_digest = ContentDigest::of_domain("qa-release-gate", DELIVERY_SCHEMA_V1, gate)?;
        if gate.candidate.generation != candidate.generation
            || gate.candidate.digest != candidate.candidate_digest
            || gate.plan.generation != plan.generation
            || gate.plan.digest != plan.plan_digest
            || !aggregate.qa_runs.values().any(|run| {
                run.gate_receipt.as_ref().is_some_and(|reference| {
                    reference.id == gate.gate_id
                        && reference.generation == gate.generation
                        && reference.digest == gate_digest
                })
            })
        {
            return Err(corrupt("QA gate references are stale or orphaned"));
        }
    }
    for review in aggregate.reviews.values() {
        let candidate = aggregate
            .candidates
            .get(&review.candidate.id)
            .ok_or_else(|| corrupt("review candidate is missing"))?;
        if review.candidate.generation != candidate.generation
            || review.candidate.digest != candidate.candidate_digest
        {
            return Err(corrupt("review candidate reference is stale"));
        }
    }
    for test in aggregate.test_runs.values() {
        let candidate = aggregate
            .candidates
            .get(&test.candidate.id)
            .ok_or_else(|| corrupt("test candidate is missing"))?;
        let plan = aggregate
            .qa_plans
            .get(&test.qa_plan.id)
            .ok_or_else(|| corrupt("test QA plan is missing"))?;
        let receipt = aggregate
            .workbench_receipts
            .get(&test.runner_receipt.id)
            .ok_or_else(|| corrupt("test workbench receipt is missing"))?;
        if test.candidate.generation != candidate.generation
            || test.candidate.digest != candidate.candidate_digest
            || test.qa_plan.generation != plan.generation
            || test.qa_plan.digest != plan.plan_digest
            || test.runner_receipt.generation != receipt.invocation.generation
            || test.runner_receipt.digest != receipt.receipt_digest
        {
            return Err(corrupt("test reference is stale"));
        }
    }
    for finding in aggregate.findings.values() {
        let candidate = aggregate
            .candidates
            .get(&finding.candidate.id)
            .ok_or_else(|| corrupt("finding candidate is missing"))?;
        if finding.candidate.generation != candidate.generation
            || finding.candidate.digest != candidate.candidate_digest
        {
            return Err(corrupt("finding candidate reference is stale"));
        }
    }
    for approval in aggregate.approvals.values() {
        let candidate = aggregate
            .candidates
            .get(&approval.candidate.id)
            .ok_or_else(|| corrupt("approval candidate is missing"))?;
        if approval.candidate.generation != candidate.generation
            || approval.candidate.digest != candidate.candidate_digest
        {
            return Err(corrupt("approval candidate reference is stale"));
        }
        if let Some(gate) = aggregate.gates.get(&approval.gate.id) {
            let gate_digest =
                ContentDigest::of_domain("qa-release-gate", DELIVERY_SCHEMA_V1, gate)?;
            if approval.gate.generation != gate.generation
                || approval.gate.digest != gate_digest
            {
                return Err(corrupt("approval gate reference is stale"));
            }
        } else if candidate.state != CandidateState::QaRunning {
            return Err(corrupt(
                "approval gate is missing outside staged QA review",
            ));
        }
    }
    for manifest in aggregate.manifests.values() {
        let candidate = aggregate
            .candidates
            .get(&manifest.candidate.id)
            .ok_or_else(|| corrupt("release manifest candidate is missing"))?;
        let gate = aggregate
            .gates
            .get(&manifest.qa_gate.id)
            .ok_or_else(|| corrupt("release manifest QA gate is missing"))?;
        let gate_digest = ContentDigest::of_domain("qa-release-gate", DELIVERY_SCHEMA_V1, gate)?;
        if manifest.candidate.generation != candidate.generation
            || manifest.candidate.digest != candidate.candidate_digest
            || manifest.qa_gate.generation != gate.generation
            || manifest.qa_gate.digest != gate_digest
            || manifest.cost != candidate.cost
            || manifest.artifacts != candidate.artifacts
        {
            return Err(corrupt("release manifest reference is stale"));
        }
    }
    for release in aggregate.releases.values() {
        let manifest = aggregate
            .manifests
            .get(&release.manifest.id)
            .ok_or_else(|| corrupt("release manifest is missing"))?;
        if release.manifest.generation != manifest.generation
            || release.manifest.digest != manifest.manifest_digest
        {
            return Err(corrupt("release manifest reference is stale"));
        }
    }
    for delivery in aggregate.deliveries.values() {
        let release = aggregate
            .releases
            .get(&delivery.release.id)
            .ok_or_else(|| corrupt("delivery release is missing"))?;
        if !matches_release_ref(&delivery.release, release)? {
            return Err(corrupt("delivery release reference is stale"));
        }
    }
    for acceptance in aggregate.acceptances.values() {
        let delivery = aggregate
            .deliveries
            .get(&acceptance.delivery.id)
            .ok_or_else(|| corrupt("acceptance delivery is missing"))?;
        let release = aggregate
            .releases
            .get(&acceptance.release.id)
            .ok_or_else(|| corrupt("acceptance release is missing"))?;
        if acceptance.delivery.generation != delivery.generation
            || acceptance.delivery.digest != delivery.receipt_digest
            || !matches_delivery_ref(&acceptance.delivery, delivery)?
            || !matches_release_ref(&acceptance.release, release)?
        {
            return Err(corrupt("acceptance reference is stale"));
        }
    }
    for feedback in aggregate.feedback.values() {
        let delivery = aggregate
            .deliveries
            .get(&feedback.delivery.id)
            .ok_or_else(|| corrupt("feedback delivery is missing"))?;
        if !matches_delivery_ref(&feedback.delivery, delivery)? {
            return Err(corrupt("feedback delivery reference is stale"));
        }
    }
    for rollback in aggregate.rollbacks.values() {
        let from = aggregate
            .releases
            .get(&rollback.from_release.id)
            .ok_or_else(|| corrupt("rollback source release is missing"))?;
        let to = aggregate
            .releases
            .get(&rollback.to_release.id)
            .ok_or_else(|| corrupt("rollback target release is missing"))?;
        if !matches_release_ref(&rollback.from_release, from)?
            || !matches_release_ref(&rollback.to_release, to)?
        {
            return Err(corrupt("rollback release reference is stale"));
        }
    }
    for closeout in aggregate.closeouts.values() {
        let release = aggregate
            .releases
            .get(&closeout.accepted_release.id)
            .ok_or_else(|| corrupt("closeout accepted release is missing"))?;
        let acceptance = aggregate
            .acceptances
            .get(&closeout.acceptance.id)
            .ok_or_else(|| corrupt("closeout acceptance is missing"))?;
        if !matches_release_ref(&closeout.accepted_release, release)?
            || closeout.acceptance.generation != acceptance.generation
            || closeout.acceptance.digest != acceptance.acceptance_digest
        {
            return Err(corrupt("closeout reference is stale"));
        }
    }
    Ok(())
}

fn matches_release_ref(
    reference: &VersionedRefV1,
    release: &ReleaseV1,
) -> Result<bool, DeliveryError> {
    Ok(reference.id == release.release_id
        && reference.generation == release.generation
        && reference.digest == canonical_release_reference_digest(release)?)
}

/// Stable release authority reference. Mutable lifecycle state and timestamps
/// are deliberately excluded; the immutable manifest and the rollout receipt
/// that first activated this release remain bound for its entire history.
pub fn canonical_release_reference_digest(
    release: &ReleaseV1,
) -> Result<ContentDigest, DeliveryError> {
    if release.schema_version != DELIVERY_SCHEMA_V1
        || release.release_id.is_empty()
        || release.generation == 0
        || release.manifest.id.is_empty()
        || release.manifest.generation == 0
        || release.manifest.digest == ContentDigest::zero()
    {
        return Err(DeliveryError::CorruptStore(
            "release immutable identity is invalid".to_string(),
        ));
    }
    let rollout = release.rollout_receipt.as_ref().ok_or_else(|| {
        DeliveryError::CorruptStore("release rollout receipt is missing".to_string())
    })?;
    if rollout.id.is_empty() || rollout.generation == 0 || rollout.digest == ContentDigest::zero() {
        return Err(DeliveryError::CorruptStore(
            "release rollout receipt is invalid".to_string(),
        ));
    }
    ContentDigest::of_domain(
        "release-reference",
        DELIVERY_SCHEMA_V1,
        &(
            release.schema_version,
            &release.release_id,
            release.generation,
            &release.manifest,
            rollout,
        ),
    )
}

pub fn canonical_release_reference(release: &ReleaseV1) -> Result<VersionedRefV1, DeliveryError> {
    Ok(VersionedRefV1 {
        id: release.release_id.clone(),
        generation: release.generation,
        digest: canonical_release_reference_digest(release)?,
    })
}

fn matches_candidate_seal(candidate: &ReleaseCandidateV1) -> Result<bool, DeliveryError> {
    let mut sealed_version = candidate.clone();
    sealed_version.state = CandidateState::Draft;
    Ok(candidate.candidate_digest == sealed_version.computed_digest()?)
}

fn matches_delivery_seal(delivery: &DeliveryReceiptV1) -> Result<bool, DeliveryError> {
    let mut sealed_version = delivery.clone();
    sealed_version.state = DeliveryState::Delivered;
    Ok(delivery.receipt_digest == sealed_version.computed_digest()?)
}

fn matches_delivery_ref(
    reference: &VersionedRefV1,
    delivery: &DeliveryReceiptV1,
) -> Result<bool, DeliveryError> {
    Ok(reference.id == delivery.delivery_id
        && reference.generation == delivery.generation
        && reference.digest == delivery.receipt_digest
        && matches_delivery_seal(delivery)?)
}

fn validate_cost(cost: CostRefV1) -> Result<(), DeliveryError> {
    let canonical_id = |value: &str| {
        !value.is_empty()
            && value.len() <= 160
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            })
    };
    if !canonical_id(&cost.ledger_id)
        || cost.generation == 0
        || cost.digest == ContentDigest::zero()
        || cost.currency != "USD"
    {
        return Err(DeliveryError::CorruptStore(
            "cost is not a canonical USD minor-unit ledger reference".to_string(),
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryLineageStageV1 {
    CustomerRequest,
    Agreement,
    Project,
    WorkItem,
    Participant,
    Decision,
    Handoff,
    Blocker,
    Candidate,
    Qa,
    Workbench,
    Artifact,
    Review,
    Test,
    Finding,
    Approval,
    Manifest,
    Release,
    Delivery,
    Acceptance,
    Closeout,
    Rollback,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicDeliveryLineageNodeV1 {
    pub id: String,
    pub stage: DeliveryLineageStageV1,
    pub label: String,
    pub state: String,
    pub digest: ContentDigest,
    pub generation: u64,
    pub actor_role: AuthorityRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_minor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicDeliveryLineageEdgeV1 {
    pub from: String,
    pub to: String,
}

/// Authenticated, server-redacted read model for the Console.
///
/// Tenant IDs, internal project/record IDs, principals, artifact locations,
/// source evidence, credentials, prompts, and infrastructure identifiers are
/// deliberately not representable. Node IDs are response-local sequence
/// numbers and the project label is generic, so low-entropy private keys cannot
/// be dictionary-linked across responses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicDeliveryLineageDtoV1 {
    pub schema_version: u16,
    pub server_redacted: bool,
    pub project_label: String,
    pub revision: u64,
    pub nodes: Vec<PublicDeliveryLineageNodeV1>,
    pub edges: Vec<PublicDeliveryLineageEdgeV1>,
    pub blockers: Vec<String>,
    pub adapter_ready: bool,
    pub authority_generation: u64,
    pub read_at_ms: u64,
}

impl PublicDeliveryLineageDtoV1 {
    pub(crate) fn from_authorized_aggregate(
        aggregate: &DeliveryAggregateV1,
        query: &WorkflowLineageQueryV1,
        workflow: &WorkflowLineageSnapshotV1,
        authority_generation: u64,
        read_at_ms: u64,
    ) -> Result<Self, DeliveryError> {
        if aggregate.schema_version != DELIVERY_SCHEMA_V1 || authority_generation == 0 {
            return Err(DeliveryError::Validation(
                "lineage aggregate or authority generation is invalid".to_string(),
            ));
        }
        validate_delivery_aggregate_references(aggregate)?;
        validate_workflow_lineage(aggregate, query, workflow)?;
        let mut nodes = Vec::new();
        let mut lookup = BTreeMap::new();
        let mut blockers = BTreeSet::new();

        for workflow_node in &workflow.nodes {
            let id = push_node(
                &mut nodes,
                workflow_stage(workflow_node.kind),
                workflow_label(workflow_node.kind),
                workflow_state(workflow_node.state),
                workflow_node.digest.clone(),
                workflow_node.generation,
                workflow_node
                    .participant_role
                    .clone()
                    .unwrap_or(AuthorityRole::GaiaObserver),
                None,
                None,
            )?;
            lookup.insert(format!("workflow:{}", workflow_node.node_ordinal), id);
            if workflow_node.kind == WorkflowLineageKindV1::Blocker
                && workflow_node.state == WorkflowLineageStateV1::Blocked
            {
                blockers.insert("Workflow has an active blocker".to_string());
            }
        }

        for candidate in aggregate.candidates.values() {
            let key = format!("candidate:{}", candidate.candidate_id);
            let id = push_node(
                &mut nodes,
                DeliveryLineageStageV1::Candidate,
                "Release candidate",
                wire_state(candidate.state)?,
                candidate.candidate_digest.clone(),
                candidate.generation,
                AuthorityRole::Developer,
                Some(candidate.cost.amount_minor.to_string()),
                Some(candidate.cost.currency.clone()),
            )?;
            lookup.insert(key, id);
            if candidate.state == CandidateState::GateFailed {
                blockers.insert("Independent QA requires rework".to_string());
            }
            for artifact in &candidate.artifacts {
                let artifact_id = push_node(
                    &mut nodes,
                    DeliveryLineageStageV1::Artifact,
                    "Candidate artifact",
                    "recorded",
                    artifact.digest.clone(),
                    artifact.generation,
                    AuthorityRole::Developer,
                    None,
                    None,
                )?;
                lookup.insert(
                    format!(
                        "artifact:{}:{}",
                        candidate.candidate_id, artifact.artifact_id
                    ),
                    artifact_id,
                );
            }
        }

        for plan in aggregate.qa_plans.values() {
            let id = push_node(
                &mut nodes,
                DeliveryLineageStageV1::Qa,
                "Independent QA plan",
                "recorded",
                plan.plan_digest.clone(),
                plan.generation,
                AuthorityRole::Qa,
                None,
                None,
            )?;
            lookup.insert(format!("qa-plan:{}", plan.plan_id), id);
        }

        for receipt in aggregate.workbench_receipts.values() {
            let id = push_node(
                &mut nodes,
                DeliveryLineageStageV1::Workbench,
                "Workbench receipt",
                wire_state(receipt.harness_outcome)?,
                receipt.receipt_digest.clone(),
                receipt.invocation.generation,
                AuthorityRole::Qa,
                None,
                None,
            )?;
            lookup.insert(format!("workbench:{}", receipt.invocation.id), id);
        }

        for graph in aggregate.evidence_graphs.values() {
            let id = push_node(
                &mut nodes,
                DeliveryLineageStageV1::Qa,
                "QA result inventory",
                "recorded",
                graph.graph_digest.clone(),
                graph.run.generation,
                AuthorityRole::Qa,
                None,
                None,
            )?;
            lookup.insert(format!("qa-result:{}", graph.run.id), id);
        }

        for review in aggregate.reviews.values() {
            let id = push_node(
                &mut nodes,
                DeliveryLineageStageV1::Review,
                "Independent review",
                if review.approved {
                    "approved"
                } else {
                    "recorded"
                },
                ContentDigest::of_domain("review-lineage", DELIVERY_SCHEMA_V1, review)?,
                review.generation,
                AuthorityRole::Qa,
                None,
                None,
            )?;
            lookup.insert(format!("review:{}", review.review_id), id);
        }
        for test in aggregate.test_runs.values() {
            let id = push_node(
                &mut nodes,
                DeliveryLineageStageV1::Test,
                "Test evidence",
                if test.passed { "passed" } else { "failed" },
                ContentDigest::of_domain("test-run-lineage", DELIVERY_SCHEMA_V1, test)?,
                test.generation,
                AuthorityRole::Qa,
                None,
                None,
            )?;
            lookup.insert(format!("test:{}", test.test_run_id), id);
        }
        for finding in aggregate.findings.values() {
            let id = push_node(
                &mut nodes,
                DeliveryLineageStageV1::Finding,
                "Review finding",
                if finding.resolved_by.is_some() {
                    "resolved"
                } else {
                    "unresolved"
                },
                ContentDigest::of_domain("finding-lineage", DELIVERY_SCHEMA_V1, finding)?,
                finding.generation,
                AuthorityRole::Qa,
                None,
                None,
            )?;
            lookup.insert(format!("finding:{}", finding.finding_id), id);
            if finding.resolved_by.is_none() {
                blockers.insert("Independent review has an unresolved finding".to_string());
            }
        }
        for approval in aggregate.approvals.values() {
            let id = push_node(
                &mut nodes,
                DeliveryLineageStageV1::Approval,
                "Independent approval",
                "approved",
                ContentDigest::of_domain("approval-lineage", DELIVERY_SCHEMA_V1, approval)?,
                approval.generation,
                AuthorityRole::Qa,
                None,
                None,
            )?;
            lookup.insert(format!("approval:{}", approval.approval_id), id);
        }
        for manifest in aggregate.manifests.values() {
            let id = push_node(
                &mut nodes,
                DeliveryLineageStageV1::Manifest,
                "Release manifest evidence",
                "recorded",
                manifest.manifest_digest.clone(),
                manifest.generation,
                AuthorityRole::ReleaseManager,
                None,
                None,
            )?;
            lookup.insert(format!("manifest:{}", manifest.manifest_id), id);
        }

        for run in aggregate.qa_runs.values() {
            let key = format!("qa-run:{}", run.run_id);
            let digest = ContentDigest::of_domain("qa-run-lineage", DELIVERY_SCHEMA_V1, run)?;
            let id = push_node(
                &mut nodes,
                DeliveryLineageStageV1::Qa,
                "Independent QA run",
                wire_state(run.state)?,
                digest,
                run.generation,
                AuthorityRole::Qa,
                None,
                None,
            )?;
            lookup.insert(key, id);
            if matches!(
                run.state,
                QaRunState::CompletedFail
                    | QaRunState::HarnessError
                    | QaRunState::NeedsHumanReview
                    | QaRunState::Quarantined
            ) {
                blockers.insert("Independent QA has no promotable result".to_string());
            }
        }

        for gate in aggregate.gates.values() {
            let key = format!("qa-gate:{}", gate.gate_id);
            let digest = ContentDigest::of_domain("qa-gate-lineage", DELIVERY_SCHEMA_V1, gate)?;
            let id = push_node(
                &mut nodes,
                DeliveryLineageStageV1::Qa,
                "Release gate",
                if gate.passed { "passed" } else { "failed" },
                digest,
                gate.generation,
                AuthorityRole::Qa,
                None,
                None,
            )?;
            lookup.insert(key, id);
            if !gate.passed {
                blockers.insert("Release gate failed".to_string());
            }
        }

        for release in aggregate.releases.values() {
            let key = format!("release:{}", release.release_id);
            let digest = ContentDigest::of_domain("release-lineage", DELIVERY_SCHEMA_V1, release)?;
            let id = push_node(
                &mut nodes,
                DeliveryLineageStageV1::Release,
                "Release",
                wire_state(release.state)?,
                digest,
                release.generation,
                AuthorityRole::ReleaseManager,
                None,
                None,
            )?;
            lookup.insert(key, id);
            if release.state == ReleaseState::RolledBack {
                blockers.insert("Release was rolled back".to_string());
            }
        }

        for delivery in aggregate.deliveries.values() {
            let key = format!("delivery:{}", delivery.delivery_id);
            let id = push_node(
                &mut nodes,
                DeliveryLineageStageV1::Delivery,
                "Customer delivery",
                wire_state(delivery.state)?,
                delivery.receipt_digest.clone(),
                delivery.generation,
                AuthorityRole::ReleaseManager,
                None,
                None,
            )?;
            lookup.insert(key, id);
            if matches!(
                delivery.state,
                DeliveryState::Rejected | DeliveryState::ChangesRequested | DeliveryState::Expired
            ) {
                blockers.insert("Customer delivery is not accepted".to_string());
            }
        }

        for acceptance in aggregate.acceptances.values() {
            let key = format!("acceptance:{}", acceptance.acceptance_id);
            let id = push_node(
                &mut nodes,
                DeliveryLineageStageV1::Acceptance,
                "Customer acceptance",
                "accepted",
                acceptance.acceptance_digest.clone(),
                acceptance.generation,
                AuthorityRole::Customer,
                None,
                None,
            )?;
            lookup.insert(key, id);
        }

        for rollback in aggregate.rollbacks.values() {
            let key = format!("rollback:{}", rollback.rollback_id);
            let digest =
                ContentDigest::of_domain("rollback-lineage", DELIVERY_SCHEMA_V1, rollback)?;
            let id = push_node(
                &mut nodes,
                DeliveryLineageStageV1::Rollback,
                "Rollback",
                "recorded",
                digest,
                rollback.generation,
                AuthorityRole::ReleaseManager,
                None,
                None,
            )?;
            lookup.insert(key, id);
        }

        for closeout in aggregate.closeouts.values() {
            let key = format!("closeout:{}", closeout.closeout_id);
            let digest =
                ContentDigest::of_domain("closeout-lineage", DELIVERY_SCHEMA_V1, closeout)?;
            let id = push_node(
                &mut nodes,
                DeliveryLineageStageV1::Closeout,
                "Project closeout",
                "closed",
                digest,
                closeout.generation,
                AuthorityRole::ReleaseManager,
                None,
                None,
            )?;
            lookup.insert(key, id);
        }

        let mut edges = Vec::new();
        for edge in &workflow.edges {
            required_link(
                &mut edges,
                &lookup,
                &format!("workflow:{}", edge.from_ordinal),
                &format!("workflow:{}", edge.to_ordinal),
            )?;
        }
        let project_node = workflow
            .nodes
            .iter()
            .find(|node| node.kind == WorkflowLineageKindV1::Project)
            .ok_or_else(|| {
                DeliveryError::CorruptStore("workflow project node missing".to_string())
            })?;
        for candidate in aggregate.candidates.values() {
            required_link(
                &mut edges,
                &lookup,
                &format!("workflow:{}", project_node.node_ordinal),
                &format!("candidate:{}", candidate.candidate_id),
            )?;
            for artifact in &candidate.artifacts {
                required_link(
                    &mut edges,
                    &lookup,
                    &format!("candidate:{}", candidate.candidate_id),
                    &format!(
                        "artifact:{}:{}",
                        candidate.candidate_id, artifact.artifact_id
                    ),
                )?;
            }
        }
        for plan in aggregate.qa_plans.values() {
            required_link(
                &mut edges,
                &lookup,
                &format!("candidate:{}", plan.candidate.id),
                &format!("qa-plan:{}", plan.plan_id),
            )?;
        }
        for run in aggregate.qa_runs.values() {
            let plan = aggregate.qa_plans.get(&run.plan.id).ok_or_else(|| {
                DeliveryError::CorruptStore("required QA plan edge is missing".to_string())
            })?;
            required_link(
                &mut edges,
                &lookup,
                &format!("qa-plan:{}", plan.plan_id),
                &format!("qa-run:{}", run.run_id),
            )?;
            if let Some(graph) = aggregate.evidence_graphs.get(&run.run_id) {
                required_link(
                    &mut edges,
                    &lookup,
                    &format!("qa-run:{}", run.run_id),
                    &format!("workbench:{}", graph.workbench_receipt.id),
                )?;
                required_link(
                    &mut edges,
                    &lookup,
                    &format!("workbench:{}", graph.workbench_receipt.id),
                    &format!("qa-result:{}", run.run_id),
                )?;
            }
            if let Some(gate) = &run.gate_receipt {
                required_link(
                    &mut edges,
                    &lookup,
                    &format!("qa-run:{}", run.run_id),
                    &format!("qa-gate:{}", gate.id),
                )?;
            }
        }
        for release in aggregate.releases.values() {
            let manifest = aggregate
                .manifests
                .get(&release.manifest.id)
                .ok_or_else(|| {
                    DeliveryError::CorruptStore(
                        "required release manifest edge is missing".to_string(),
                    )
                })?;
            required_link(
                &mut edges,
                &lookup,
                &format!("candidate:{}", manifest.candidate.id),
                &format!("manifest:{}", manifest.manifest_id),
            )?;
            required_link(
                &mut edges,
                &lookup,
                &format!("qa-gate:{}", manifest.qa_gate.id),
                &format!("manifest:{}", manifest.manifest_id),
            )?;
            required_link(
                &mut edges,
                &lookup,
                &format!("manifest:{}", manifest.manifest_id),
                &format!("release:{}", release.release_id),
            )?;
        }
        for review in aggregate.reviews.values() {
            required_link(
                &mut edges,
                &lookup,
                &format!("candidate:{}", review.candidate.id),
                &format!("review:{}", review.review_id),
            )?;
        }
        for test in aggregate.test_runs.values() {
            required_link(
                &mut edges,
                &lookup,
                &format!("candidate:{}", test.candidate.id),
                &format!("test:{}", test.test_run_id),
            )?;
        }
        for finding in aggregate.findings.values() {
            required_link(
                &mut edges,
                &lookup,
                &format!("candidate:{}", finding.candidate.id),
                &format!("finding:{}", finding.finding_id),
            )?;
        }
        for approval in aggregate.approvals.values() {
            required_link(
                &mut edges,
                &lookup,
                &format!("candidate:{}", approval.candidate.id),
                &format!("approval:{}", approval.approval_id),
            )?;
        }
        for delivery in aggregate.deliveries.values() {
            required_link(
                &mut edges,
                &lookup,
                &format!("release:{}", delivery.release.id),
                &format!("delivery:{}", delivery.delivery_id),
            )?;
        }
        for acceptance in aggregate.acceptances.values() {
            required_link(
                &mut edges,
                &lookup,
                &format!("delivery:{}", acceptance.delivery.id),
                &format!("acceptance:{}", acceptance.acceptance_id),
            )?;
        }
        for rollback in aggregate.rollbacks.values() {
            required_link(
                &mut edges,
                &lookup,
                &format!("release:{}", rollback.from_release.id),
                &format!("rollback:{}", rollback.rollback_id),
            )?;
            required_link(
                &mut edges,
                &lookup,
                &format!("rollback:{}", rollback.rollback_id),
                &format!("release:{}", rollback.to_release.id),
            )?;
        }
        for closeout in aggregate.closeouts.values() {
            required_link(
                &mut edges,
                &lookup,
                &format!("release:{}", closeout.accepted_release.id),
                &format!("closeout:{}", closeout.closeout_id),
            )?;
        }

        Ok(Self {
            schema_version: DELIVERY_SCHEMA_V1,
            server_redacted: true,
            project_label: "Project".to_string(),
            revision: aggregate.revision,
            nodes,
            edges,
            blockers: blockers.into_iter().collect(),
            adapter_ready: true,
            authority_generation,
            read_at_ms,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn push_node(
    nodes: &mut Vec<PublicDeliveryLineageNodeV1>,
    stage: DeliveryLineageStageV1,
    label: &str,
    state: &str,
    digest: ContentDigest,
    generation: u64,
    actor_role: AuthorityRole,
    cost_minor: Option<String>,
    currency: Option<String>,
) -> Result<String, DeliveryError> {
    if generation == 0 || digest == ContentDigest::zero() {
        return Err(DeliveryError::CorruptStore(
            "lineage record has invalid generation or digest".to_string(),
        ));
    }
    match (&cost_minor, &currency) {
        (Some(value), Some(currency))
            if !value.is_empty()
                && value.bytes().all(|byte| byte.is_ascii_digit())
                && currency == "USD" => {}
        (None, None) => {}
        _ => {
            return Err(DeliveryError::CorruptStore(
                "lineage cost is not a canonical minor-unit amount".to_string(),
            ));
        }
    }
    let id = format!("node-{:06}", nodes.len() + 1);
    nodes.push(PublicDeliveryLineageNodeV1 {
        id: id.clone(),
        stage,
        label: label.to_string(),
        state: state.to_string(),
        digest,
        generation,
        actor_role,
        cost_minor,
        currency,
    });
    Ok(id)
}

fn required_link(
    edges: &mut Vec<PublicDeliveryLineageEdgeV1>,
    lookup: &BTreeMap<String, String>,
    from: &str,
    to: &str,
) -> Result<(), DeliveryError> {
    let from = lookup.get(from).ok_or_else(|| {
        DeliveryError::CorruptStore("required public lineage source is missing".to_string())
    })?;
    let to = lookup.get(to).ok_or_else(|| {
        DeliveryError::CorruptStore("required public lineage target is missing".to_string())
    })?;
    let edge = PublicDeliveryLineageEdgeV1 {
        from: from.clone(),
        to: to.clone(),
    };
    if !edges.contains(&edge) {
        edges.push(edge);
    }
    Ok(())
}

fn validate_workflow_lineage(
    aggregate: &DeliveryAggregateV1,
    query: &WorkflowLineageQueryV1,
    snapshot: &WorkflowLineageSnapshotV1,
) -> Result<(), DeliveryError> {
    let corrupt = |detail: &str| DeliveryError::CorruptStore(format!("workflow lineage: {detail}"));
    if query.schema_version != DELIVERY_SCHEMA_V1
        || query.query_digest != query.computed_digest()?
        || query.tenant_id != aggregate.tenant_id
        || query.project.id != aggregate.project_id
        || query.authority_generation == 0
        || query.authority_identity_digest == ContentDigest::zero()
    {
        return Err(corrupt("query binding is invalid"));
    }
    if snapshot.schema_version != DELIVERY_SCHEMA_V1
        || !snapshot.server_redacted
        || snapshot.tenant_id != query.tenant_id
        || snapshot.project != query.project
        || snapshot.candidate != query.candidate
        || snapshot.authority_generation != query.authority_generation
        || snapshot.authority_identity_digest != query.authority_identity_digest
        || snapshot.query_digest != query.query_digest
        || snapshot.snapshot_generation == 0
        || snapshot.snapshot_digest != snapshot.computed_digest()?
    {
        return Err(corrupt("snapshot digest or authority binding is invalid"));
    }
    let required = BTreeSet::from([
        WorkflowLineageKindV1::CustomerRequest,
        WorkflowLineageKindV1::Agreement,
        WorkflowLineageKindV1::Project,
        WorkflowLineageKindV1::WorkItem,
        WorkflowLineageKindV1::Participant,
        WorkflowLineageKindV1::Decision,
        WorkflowLineageKindV1::Handoff,
        WorkflowLineageKindV1::Blocker,
    ]);
    let mut kinds = BTreeSet::new();
    let mut ordinals = BTreeSet::new();
    let mut kind_ordinals: BTreeMap<WorkflowLineageKindV1, Vec<u32>> = BTreeMap::new();
    for node in &snapshot.nodes {
        if node.node_ordinal == 0
            || !ordinals.insert(node.node_ordinal)
            || node.generation == 0
            || node.digest == ContentDigest::zero()
            || (node.kind == WorkflowLineageKindV1::Participant && node.participant_role.is_none())
            || (node.kind != WorkflowLineageKindV1::Participant && node.participant_role.is_some())
            || !workflow_state_allowed(node.kind, node.state)
        {
            return Err(corrupt(
                "node inventory is malformed or contains private labels",
            ));
        }
        kinds.insert(node.kind);
        kind_ordinals
            .entry(node.kind)
            .or_default()
            .push(node.node_ordinal);
    }
    if kinds != required {
        return Err(corrupt("required workflow class is omitted"));
    }
    for root in [
        WorkflowLineageKindV1::CustomerRequest,
        WorkflowLineageKindV1::Agreement,
        WorkflowLineageKindV1::Project,
    ] {
        if kind_ordinals.get(&root).map(Vec::len) != Some(1) {
            return Err(corrupt("workflow root class is not unique"));
        }
    }
    let mut unique_edges = BTreeSet::new();
    let mut adjacency: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    let mut indegree = ordinals
        .iter()
        .copied()
        .map(|ordinal| (ordinal, 0usize))
        .collect::<BTreeMap<_, _>>();
    for edge in &snapshot.edges {
        if edge.from_ordinal == edge.to_ordinal
            || !ordinals.contains(&edge.from_ordinal)
            || !ordinals.contains(&edge.to_ordinal)
            || !unique_edges.insert((edge.from_ordinal, edge.to_ordinal))
        {
            return Err(corrupt("edge is dangling, duplicated, or self-referential"));
        }
        adjacency
            .entry(edge.from_ordinal)
            .or_default()
            .push(edge.to_ordinal);
        let degree = indegree
            .get_mut(&edge.to_ordinal)
            .ok_or_else(|| corrupt("edge target disappeared during validation"))?;
        *degree += 1;
    }
    let request = kind_ordinals[&WorkflowLineageKindV1::CustomerRequest][0];
    let agreement = kind_ordinals[&WorkflowLineageKindV1::Agreement][0];
    let project = kind_ordinals[&WorkflowLineageKindV1::Project][0];
    if !unique_edges.contains(&(request, agreement))
        || !unique_edges.contains(&(agreement, project))
    {
        return Err(corrupt("request-agreement-project chain is incomplete"));
    }
    let mut ready = indegree
        .iter()
        .filter_map(|(ordinal, degree)| (*degree == 0).then_some(*ordinal))
        .collect::<Vec<_>>();
    let mut visited_count = 0usize;
    while let Some(current) = ready.pop() {
        visited_count += 1;
        for next in adjacency.get(&current).into_iter().flatten() {
            let degree = indegree
                .get_mut(next)
                .ok_or_else(|| corrupt("edge target disappeared during traversal"))?;
            *degree -= 1;
            if *degree == 0 {
                ready.push(*next);
            }
        }
    }
    if visited_count != ordinals.len() {
        return Err(corrupt("workflow topology contains a cycle"));
    }
    let reachable = reachable_ordinals(request, &adjacency);
    if reachable != ordinals {
        return Err(corrupt(
            "workflow topology is disconnected from the request",
        ));
    }
    let project_reachable = reachable_ordinals(project, &adjacency);
    if snapshot.nodes.iter().any(|node| {
        !matches!(
            node.kind,
            WorkflowLineageKindV1::CustomerRequest
                | WorkflowLineageKindV1::Agreement
                | WorkflowLineageKindV1::Project
        ) && !project_reachable.contains(&node.node_ordinal)
    }) {
        return Err(corrupt(
            "dynamic workflow node is not bound below the project",
        ));
    }
    Ok(())
}

fn reachable_ordinals(start: u32, adjacency: &BTreeMap<u32, Vec<u32>>) -> BTreeSet<u32> {
    let mut reached = BTreeSet::new();
    let mut pending = vec![start];
    while let Some(current) = pending.pop() {
        if reached.insert(current) {
            pending.extend(adjacency.get(&current).into_iter().flatten().copied());
        }
    }
    reached
}

fn workflow_state_allowed(kind: WorkflowLineageKindV1, state: WorkflowLineageStateV1) -> bool {
    matches!(
        (kind, state),
        (
            WorkflowLineageKindV1::CustomerRequest,
            WorkflowLineageStateV1::Requested
        ) | (
            WorkflowLineageKindV1::Agreement,
            WorkflowLineageStateV1::Approved
        ) | (
            WorkflowLineageKindV1::Project,
            WorkflowLineageStateV1::Active | WorkflowLineageStateV1::Completed
        ) | (
            WorkflowLineageKindV1::WorkItem,
            WorkflowLineageStateV1::Active
                | WorkflowLineageStateV1::Completed
                | WorkflowLineageStateV1::Blocked
        ) | (
            WorkflowLineageKindV1::Participant,
            WorkflowLineageStateV1::Active
        ) | (
            WorkflowLineageKindV1::Decision,
            WorkflowLineageStateV1::Approved
        ) | (
            WorkflowLineageKindV1::Handoff,
            WorkflowLineageStateV1::HandedOff
        ) | (
            WorkflowLineageKindV1::Blocker,
            WorkflowLineageStateV1::Blocked | WorkflowLineageStateV1::Clear
        )
    )
}

fn workflow_stage(kind: WorkflowLineageKindV1) -> DeliveryLineageStageV1 {
    match kind {
        WorkflowLineageKindV1::CustomerRequest => DeliveryLineageStageV1::CustomerRequest,
        WorkflowLineageKindV1::Agreement => DeliveryLineageStageV1::Agreement,
        WorkflowLineageKindV1::Project => DeliveryLineageStageV1::Project,
        WorkflowLineageKindV1::WorkItem => DeliveryLineageStageV1::WorkItem,
        WorkflowLineageKindV1::Participant => DeliveryLineageStageV1::Participant,
        WorkflowLineageKindV1::Decision => DeliveryLineageStageV1::Decision,
        WorkflowLineageKindV1::Handoff => DeliveryLineageStageV1::Handoff,
        WorkflowLineageKindV1::Blocker => DeliveryLineageStageV1::Blocker,
    }
}

fn workflow_label(kind: WorkflowLineageKindV1) -> &'static str {
    match kind {
        WorkflowLineageKindV1::CustomerRequest => "Customer request",
        WorkflowLineageKindV1::Agreement => "Customer agreement",
        WorkflowLineageKindV1::Project => "Project",
        WorkflowLineageKindV1::WorkItem => "Work item",
        WorkflowLineageKindV1::Participant => "Project participant",
        WorkflowLineageKindV1::Decision => "Governance decision",
        WorkflowLineageKindV1::Handoff => "Work handoff",
        WorkflowLineageKindV1::Blocker => "Workflow blocker status",
    }
}

fn workflow_state(state: WorkflowLineageStateV1) -> &'static str {
    match state {
        WorkflowLineageStateV1::Requested => "requested",
        WorkflowLineageStateV1::Active => "active",
        WorkflowLineageStateV1::Completed => "completed",
        WorkflowLineageStateV1::Approved => "approved",
        WorkflowLineageStateV1::HandedOff => "handed_off",
        WorkflowLineageStateV1::Blocked => "blocked",
        WorkflowLineageStateV1::Clear => "clear",
    }
}

fn wire_state<T: Serialize>(state: T) -> Result<&'static str, DeliveryError> {
    let value = serde_json::to_value(state)?;
    match value.as_str() {
        Some("draft") => Ok("draft"),
        Some("qa_assigned") => Ok("qa_assigned"),
        Some("qa_running") => Ok("qa_running"),
        Some("gate_passed") => Ok("gate_passed"),
        Some("gate_failed") => Ok("gate_failed"),
        Some("promoted") => Ok("promoted"),
        Some("superseded") => Ok("superseded"),
        Some("planned") => Ok("planned"),
        Some("admitted") => Ok("admitted"),
        Some("running") => Ok("running"),
        Some("needs_human_review") => Ok("needs_human_review"),
        Some("completed_pass") => Ok("completed_pass"),
        Some("completed_fail") => Ok("completed_fail"),
        Some("harness_error") => Ok("harness_error"),
        Some("cancelled") => Ok("cancelled"),
        Some("quarantined") => Ok("quarantined"),
        Some("approved") => Ok("approved"),
        Some("active") => Ok("active"),
        Some("rolled_back") => Ok("rolled_back"),
        Some("preview_ready") => Ok("preview_ready"),
        Some("delivered") => Ok("delivered"),
        Some("accepted") => Ok("accepted"),
        Some("rejected") => Ok("rejected"),
        Some("changes_requested") => Ok("changes_requested"),
        Some("expired") => Ok("expired"),
        Some("pass") => Ok("pass"),
        Some("fail") => Ok("fail"),
        Some("error") => Ok("error"),
        _ => Err(DeliveryError::CorruptStore(
            "lineage record has an unsupported state".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_reference_is_immutable_across_state_history_and_rejects_substitution() {
        let active = ReleaseV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            release_id: "release-1".to_string(),
            generation: 7,
            manifest: VersionedRefV1 {
                id: "manifest-1".to_string(),
                generation: 3,
                digest: ContentDigest::of(&"manifest").expect("manifest digest"),
            },
            state: ReleaseState::Active,
            activated_at_ms: Some(100),
            rollout_receipt: Some(VersionedRefV1 {
                id: "rollout-1".to_string(),
                generation: 1,
                digest: ContentDigest::of(&"rollout").expect("rollout digest"),
            }),
        };
        let exact = canonical_release_reference(&active).expect("canonical reference");
        assert!(matches_release_ref(&exact, &active).expect("exact reference"));

        let hypothetical_state = VersionedRefV1 {
            digest: ContentDigest::of_domain("release", DELIVERY_SCHEMA_V1, &active)
                .expect("hypothetical state digest"),
            ..exact.clone()
        };
        assert!(!matches_release_ref(&hypothetical_state, &active)
            .expect("state digest is not an authority reference"));

        let mut rolled_back = active.clone();
        rolled_back.state = ReleaseState::RolledBack;
        assert_eq!(
            canonical_release_reference_digest(&active).expect("active digest"),
            canonical_release_reference_digest(&rolled_back).expect("rolled-back digest")
        );
        let mut reactivated = rolled_back;
        reactivated.state = ReleaseState::Active;
        assert_eq!(
            canonical_release_reference_digest(&active).expect("initial digest"),
            canonical_release_reference_digest(&reactivated).expect("reactivated digest")
        );
        assert!(matches_release_ref(&exact, &reactivated).expect("historical reference"));

        let mut manifest_substitute = active.clone();
        manifest_substitute.manifest.digest =
            ContentDigest::of(&"other-manifest").expect("manifest substitute");
        assert!(!matches_release_ref(&exact, &manifest_substitute)
            .expect("manifest substitution is rejected"));

        let mut receipt_substitute = active.clone();
        receipt_substitute
            .rollout_receipt
            .as_mut()
            .expect("rollout receipt")
            .digest = ContentDigest::of(&"other-rollout").expect("rollout substitute");
        assert!(!matches_release_ref(&exact, &receipt_substitute)
            .expect("rollout substitution is rejected"));

        let wrong = VersionedRefV1 {
            digest: ContentDigest::of(&"same-generation-substitute").expect("wrong digest"),
            ..exact
        };
        assert!(!matches_release_ref(&wrong, &active).expect("wrong reference"));
    }
}
