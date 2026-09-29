use super::service::{validate_evidence_outcome, validate_recorded_qa_evidence_graph};
use super::{
    canonical_release_reference, qa_case_attempt_history_digest, qa_case_inventory_digest,
    qa_deterministic_evidence_digest, qa_evidence_inventory_digest, qa_flake_disposition_digest,
    qa_model_evidence_digest, qa_source_evidence_digest, validate_delivery_aggregate_references,
    AuthorityRole, CandidateState, ContentDigest, DeliveryAggregateV1, DeliveryError,
    DeliveryState, QaAggregateOutcomesV1, QaHarnessOutcome, QaRunState, ReleaseState,
    VersionedRefV1, DELIVERY_SCHEMA_V1,
};

/// Recognize an already issued delivery using only persisted, immutable lineage.
/// Preview and gate expiry govern new authority, not whether delivery occurred.
/// The caller must separately require that the project's active work is done.
pub fn settled_delivery_matches(
    aggregate: &DeliveryAggregateV1,
    project: &VersionedRefV1,
    agreement: &VersionedRefV1,
    work_items_digest: &ContentDigest,
    customer_principal_id: &str,
) -> Result<bool, DeliveryError> {
    validate_aggregate_identity(aggregate)?;
    validate_delivery_aggregate_references(aggregate)?;

    let Some(candidate_generation) = project.generation.checked_add(1) else {
        return Ok(false);
    };
    if project.id != aggregate.project_id
        || project.generation == 0
        || project.digest == ContentDigest::zero()
        || agreement.id.is_empty()
        || agreement.generation == 0
        || agreement.digest == ContentDigest::zero()
        || *work_items_digest == ContentDigest::zero()
        || customer_principal_id.is_empty()
    {
        return Ok(false);
    }
    let Some(release) = aggregate
        .active_release_id
        .as_ref()
        .and_then(|id| aggregate.releases.get(id))
    else {
        return Ok(false);
    };
    if release.state != ReleaseState::Active {
        return Ok(false);
    }
    let release_ref = canonical_release_reference(release)?;
    if aggregate
        .rollbacks
        .values()
        .any(|rollback| rollback.from_release == release_ref || rollback.to_release == release_ref)
    {
        return Ok(false);
    }
    let Some(manifest) = aggregate.manifests.get(&release.manifest.id) else {
        return Ok(false);
    };
    let Some(candidate) = aggregate.candidates.get(&manifest.candidate.id) else {
        return Ok(false);
    };
    let candidate_ref = VersionedRefV1 {
        id: candidate.candidate_id.clone(),
        generation: candidate.generation,
        digest: candidate.candidate_digest.clone(),
    };
    if candidate.project != *project
        || candidate.agreement != *agreement
        || candidate.work_items_digest != *work_items_digest
        || candidate.generation != candidate_generation
        || candidate.state != CandidateState::Promoted
        || manifest.candidate != candidate_ref
        || manifest.project != *project
        || manifest.agreement != *agreement
        || manifest.work_items_digest != *work_items_digest
        || release.manifest
            != (VersionedRefV1 {
                id: manifest.manifest_id.clone(),
                generation: manifest.generation,
                digest: manifest.manifest_digest.clone(),
            })
    {
        return Ok(false);
    }
    let Some(gate) = aggregate.gates.get(&manifest.qa_gate.id) else {
        return Ok(false);
    };
    let Some(plan) = aggregate.qa_plans.get(&gate.plan.id) else {
        return Ok(false);
    };
    let plan_ref = VersionedRefV1 {
        id: plan.plan_id.clone(),
        generation: plan.generation,
        digest: plan.plan_digest.clone(),
    };
    let gate_ref = VersionedRefV1 {
        id: gate.gate_id.clone(),
        generation: gate.generation,
        digest: ContentDigest::of_domain("qa-release-gate", DELIVERY_SCHEMA_V1, gate)?,
    };
    if !gate.passed
        || gate.expires_at_ms <= gate.issued_at_ms
        || gate.candidate != candidate_ref
        || gate.plan != plan_ref
        || plan.candidate != candidate_ref
        || plan.project != *project
        || plan.agreement != *agreement
        || plan.work_items_digest != *work_items_digest
        || plan.acceptance_criteria_digest != candidate.acceptance_criteria_digest
        || manifest.qa_gate != gate_ref
        || gate.release_manifest_digest != manifest.gate_input_digest()?
        || gate.policy_digest != plan.release_policy_digest
        || gate.actor.tenant_id != aggregate.tenant_id
        || gate.actor.principal_id.is_empty()
        || gate.actor.authority_generation == 0
        || !gate.actor.has_role(AuthorityRole::Qa)
        || candidate
            .implementer_principal_ids
            .contains(&gate.actor.principal_id)
        || gate.actor.principal_id == manifest.release_actor.principal_id
    {
        return Ok(false);
    }
    let mut completed_pass = false;
    for run in aggregate.qa_runs.values() {
        if run.state != QaRunState::CompletedPass
            || run.plan != plan_ref
            || run.gate_receipt.as_ref() != Some(&gate_ref)
            || run.actors.len() != 1
            || run.actors.first() != Some(&gate.actor)
            || run.harness_outcome != Some(QaHarnessOutcome::Pass)
            || !matches!(
                run.aggregate_outcomes.as_ref(),
                Some(QaAggregateOutcomesV1 {
                    required_cases_complete: true,
                    contaminated: false,
                    needs_human_review: false,
                    flaky_unresolved: false,
                })
            )
        {
            continue;
        }
        let Some(graph) = aggregate.evidence_graphs.get(&run.run_id) else {
            continue;
        };
        let Some(receipt) = aggregate
            .workbench_receipts
            .get(&graph.workbench_receipt.id)
        else {
            continue;
        };
        let run_ref = VersionedRefV1 {
            id: run.run_id.clone(),
            generation: run.generation,
            digest: run.request_digest.clone(),
        };
        if graph.run == run_ref
            && graph
                .case_results
                .iter()
                .all(|result| result.run == run_ref)
            && graph
                .deterministic_results
                .iter()
                .all(|result| result.plan_digest == plan.plan_digest)
            && receipt.qa_run == run_ref
            && receipt.assigned_qa == gate.actor
            && receipt.harness_outcome == QaHarnessOutcome::Pass
            && receipt.required_cases_complete
            && !receipt.contaminated
            && !receipt.needs_human_review
            && !receipt.flaky_unresolved
            && run.cleanup_receipt.as_ref() == Some(&receipt.cleanup_receipt)
            && manifest.qa_evidence_digest == graph.graph_digest
            && receipt.result_inventory_digest == qa_evidence_inventory_digest(graph)?
            && gate.case_inventory_digest == qa_case_inventory_digest(graph)?
            && gate.deterministic_evidence_digest == qa_deterministic_evidence_digest(graph)?
            && gate.model_evidence_digest == qa_model_evidence_digest(graph)?
            && gate.flake_disposition_digest == qa_flake_disposition_digest(graph)?
            && gate.source_evidence_digest == qa_source_evidence_digest(graph)?
            && run.case_attempt_history_digest.as_ref()
                == Some(&qa_case_attempt_history_digest(graph)?)
        {
            validate_recorded_qa_evidence_graph(plan, &run_ref, graph, &gate.actor)?;
            validate_evidence_outcome(receipt, graph)?;
            completed_pass = true;
            break;
        }
    }
    if !completed_pass {
        return Ok(false);
    }
    Ok(aggregate.deliveries.values().any(|delivery| {
        delivery.release == release_ref
            && delivery.customer_principal_id == customer_principal_id
            && match delivery.state {
                DeliveryState::Delivered => true,
                DeliveryState::Accepted => aggregate.acceptances.values().any(|acceptance| {
                    acceptance.delivery.id == delivery.delivery_id
                        && acceptance.delivery.generation == delivery.generation
                        && acceptance.delivery.digest == delivery.receipt_digest
                        && acceptance.release == release_ref
                        && acceptance.customer.tenant_id == aggregate.tenant_id
                        && acceptance.customer.principal_id == customer_principal_id
                        && acceptance.customer.authority_generation != 0
                        && acceptance.customer.has_role(AuthorityRole::Customer)
                }),
                _ => false,
            }
    }))
}

// Reference validation checks seals and edges; also check the containing rows
// so an in-memory aggregate has the same identity guarantees as stored rows.
fn validate_aggregate_identity(aggregate: &DeliveryAggregateV1) -> Result<(), DeliveryError> {
    let corrupt =
        |detail: &str| DeliveryError::CorruptStore(format!("settled delivery identity: {detail}"));
    if aggregate.schema_version != DELIVERY_SCHEMA_V1
        || aggregate.tenant_id.is_empty()
        || aggregate.project_id.is_empty()
    {
        return Err(corrupt("aggregate schema, tenant, or project is invalid"));
    }
    macro_rules! validate_identity_map {
        ($map:ident, $id:ident) => {
            for (key, row) in &aggregate.$map {
                if key != &row.$id
                    || key.is_empty()
                    || row.schema_version != DELIVERY_SCHEMA_V1
                    || row.generation == 0
                {
                    return Err(corrupt(concat!(stringify!($map), " row is invalid")));
                }
            }
        };
    }
    validate_identity_map!(candidates, candidate_id);
    validate_identity_map!(qa_plans, plan_id);
    validate_identity_map!(qa_runs, run_id);
    validate_identity_map!(reviews, review_id);
    validate_identity_map!(test_runs, test_run_id);
    validate_identity_map!(findings, finding_id);
    validate_identity_map!(approvals, approval_id);
    validate_identity_map!(gates, gate_id);
    validate_identity_map!(manifests, manifest_id);
    validate_identity_map!(releases, release_id);
    validate_identity_map!(deliveries, delivery_id);
    validate_identity_map!(feedback, feedback_id);
    validate_identity_map!(acceptances, acceptance_id);
    validate_identity_map!(rollbacks, rollback_id);
    validate_identity_map!(closeouts, closeout_id);
    for (key, receipt) in &aggregate.workbench_receipts {
        if key != &receipt.invocation.id
            || key.is_empty()
            || receipt.schema_version != DELIVERY_SCHEMA_V1
            || receipt.invocation.generation == 0
            || receipt.assigned_qa.tenant_id != aggregate.tenant_id
        {
            return Err(corrupt("workbench receipt row is invalid"));
        }
    }
    for (key, graph) in &aggregate.evidence_graphs {
        if key != &graph.run.id
            || key.is_empty()
            || graph.schema_version != DELIVERY_SCHEMA_V1
            || graph.run.generation == 0
        {
            return Err(corrupt("evidence graph row is invalid"));
        }
    }
    let valid_project = |reference: &VersionedRefV1| {
        reference.id == aggregate.project_id
            && reference.generation != 0
            && reference.digest != ContentDigest::zero()
    };
    if aggregate
        .candidates
        .values()
        .any(|row| row.tenant_id != aggregate.tenant_id || !valid_project(&row.project))
        || aggregate.manifests.values().any(|row| {
            row.tenant_id != aggregate.tenant_id
                || row.release_actor.tenant_id != aggregate.tenant_id
                || !valid_project(&row.project)
        })
        || aggregate
            .deliveries
            .values()
            .any(|row| row.tenant_id != aggregate.tenant_id)
        || aggregate
            .qa_plans
            .values()
            .any(|row| !valid_project(&row.project))
        || aggregate
            .closeouts
            .values()
            .any(|row| !valid_project(&row.project))
    {
        return Err(corrupt("tenant or project binding is invalid"));
    }
    if aggregate
        .active_release_id
        .as_ref()
        .is_some_and(|id| !aggregate.releases.contains_key(id))
    {
        return Err(corrupt("active release is missing"));
    }
    Ok(())
}
