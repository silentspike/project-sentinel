use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::delivery::{
    canonical_release_reference_digest, qa_case_inventory_digest, qa_deterministic_evidence_digest,
    qa_evidence_inventory_digest, qa_fixture_inventory_digest, qa_flake_disposition_digest,
    qa_model_evidence_digest, qa_source_evidence_digest, AcceptanceV1, ApprovalV1, ArtifactRefV1,
    AuthorityRole, CandidateState, CommandContextV1, ContentDigest, CostRefV1, CustomerAction,
    CustomerFeedbackV1, DataControlV1, DeliveryError, DeliveryReceiptV1, DeliveryState,
    PrincipalV1, ProjectCloseoutV1, QaEvaluationPlanV1, QaEvaluationRunReceiptV1, QaHarnessOutcome,
    QaReleaseGateReceiptV1, QaRunState, ReleaseCandidateV1, ReleaseManifestV1, ReleaseState,
    ReleaseV1, ReviewV1, TestRunV1, VersionedRefV1, DELIVERY_PREVIEW_MAX_TTL_MS,
    DELIVERY_PREVIEW_TTL_POLICY_V1, DELIVERY_SCHEMA_V1,
};
use sentinel_workflow::{
    CompanyRoleV1, CompanyWorkStateV1, ProjectId, ProjectLifecycleStateV1, TenantId, WorkItemState,
    WorkflowError, WorkflowErrorCode,
};

use super::delivery_runtime::{m0_qa_evidence_graph, m0_qa_fixture_cases};
use super::{
    delivery_error, delivery_principal, json, json_error, BoundPrincipal, WorkflowApi,
    WorkflowHttpResponse,
};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeliveryIntentEnvelope {
    operation_id: Uuid,
    intent: DeliveryIntentV1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum DeliveryIntentV1 {
    PrepareCandidate { project_id: ProjectId },
    AssignQa { project_id: ProjectId },
    ExecuteQa { project_id: ProjectId },
    Release { project_id: ProjectId },
    Accept { project_id: ProjectId },
    Closeout { project_id: ProjectId },
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct DeliveryIntentResponse {
    replayed: bool,
    action: &'static str,
    tenant_id: String,
    project_id: String,
    candidate_id: String,
    qa_run_id: Option<String>,
    release_id: Option<String>,
    delivery_id: Option<String>,
    acceptance_id: Option<String>,
    closeout_id: Option<String>,
}

struct ProjectMaterial {
    project: sentinel_workflow::ProjectV1,
    agreement: sentinel_workflow::AgreementV1,
    request: sentinel_workflow::CustomerRequestV1,
    candidate: ReleaseCandidateV1,
}

pub(super) fn handle(
    api: &WorkflowApi,
    principal: &BoundPrincipal,
    body: &[u8],
) -> WorkflowHttpResponse {
    let envelope: DeliveryIntentEnvelope = match super::decode_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let observed_now_ms = super::now_unix_ms();
    if envelope.operation_id.is_nil() {
        return json_error(
            400,
            "invalid_input",
            "delivery intent identity is invalid",
            false,
        );
    }
    let Some(delivery) = api.delivery.as_ref() else {
        return json_error(
            503,
            "delivery_unavailable",
            "delivery authority is unavailable",
            true,
        );
    };
    let Some(caller) = delivery_principal(&principal.principal) else {
        return json_error(
            403,
            "authority_conflict",
            "principal has no delivery authority",
            false,
        );
    };
    let required_role = match &envelope.intent {
        DeliveryIntentV1::PrepareCandidate { .. } => AuthorityRole::Developer,
        DeliveryIntentV1::AssignQa { .. }
        | DeliveryIntentV1::Release { .. }
        | DeliveryIntentV1::Closeout { .. } => AuthorityRole::ReleaseManager,
        DeliveryIntentV1::ExecuteQa { .. } => AuthorityRole::Qa,
        DeliveryIntentV1::Accept { .. } => AuthorityRole::Customer,
    };
    if let Err(error) = require_role(&caller, required_role) {
        return delivery_error(error);
    }
    let Ok(_guard) = api.mutation_fence.read() else {
        return json_error(503, "workflow_busy", "workflow recovery is active", true);
    };
    let intent_digest = match ContentDigest::of_domain(
        "m0-delivery-intent",
        DELIVERY_SCHEMA_V1,
        &(
            &caller.tenant_id,
            &caller.principal_id,
            caller.authority_generation,
            &envelope.intent,
        ),
    ) {
        Ok(digest) => digest,
        Err(error) => return delivery_error(error),
    };
    let operation_namespace = format!(
        "delivery-intent-v1:{}:{}:{}",
        caller.tenant_id, caller.principal_id, caller.authority_generation
    );
    let effective_now_ms = match api.store.reserve_operation_timestamp(
        &operation_namespace,
        envelope.operation_id,
        intent_digest.as_str(),
        observed_now_ms,
    ) {
        Ok((_, reserved_at_ms)) => reserved_at_ms,
        Err(error) => return delivery_error(workflow_delivery_error(error)),
    };
    let result = match &envelope.intent {
        DeliveryIntentV1::PrepareCandidate { project_id } => prepare_candidate(
            api,
            delivery,
            &caller,
            envelope.operation_id,
            effective_now_ms,
            project_id,
        ),
        DeliveryIntentV1::AssignQa { project_id } => assign_qa(
            api,
            delivery,
            &caller,
            envelope.operation_id,
            effective_now_ms,
            project_id,
        ),
        DeliveryIntentV1::ExecuteQa { project_id } => execute_qa(
            api,
            delivery,
            &caller,
            envelope.operation_id,
            effective_now_ms,
            project_id,
        ),
        DeliveryIntentV1::Release { project_id } => release(
            api,
            delivery,
            &caller,
            envelope.operation_id,
            effective_now_ms,
            observed_now_ms,
            project_id,
        ),
        DeliveryIntentV1::Accept { project_id } => accept(
            api,
            delivery,
            &caller,
            envelope.operation_id,
            effective_now_ms,
            observed_now_ms,
            project_id,
        ),
        DeliveryIntentV1::Closeout { project_id } => closeout(
            api,
            delivery,
            &caller,
            envelope.operation_id,
            effective_now_ms,
            project_id,
        ),
    };
    match result {
        Ok(response) => json(200, &response),
        Err(error) => {
            tracing::warn!(
                operation_id = %envelope.operation_id,
                intent = ?envelope.intent,
                error = %error,
                "Delivery intent rejected"
            );
            delivery_error(error)
        }
    }
}

fn context(
    principal: PrincipalV1,
    operation_id: Uuid,
    stage: &str,
    now_ms: u64,
) -> CommandContextV1 {
    CommandContextV1 {
        principal,
        idempotency_key: format!("{operation_id}.{stage}"),
        now_ms,
    }
}

fn require_role(principal: &PrincipalV1, role: AuthorityRole) -> Result<(), DeliveryError> {
    if principal.has_role(role) {
        Ok(())
    } else {
        Err(DeliveryError::AuthorityDenied(
            "delivery intent is not authorized for this role".to_string(),
        ))
    }
}

fn require_current_principal(
    caller: &PrincipalV1,
    expected: &PrincipalV1,
) -> Result<(), DeliveryError> {
    if caller == expected {
        Ok(())
    } else {
        Err(DeliveryError::AuthorityDenied(
            "delivery intent principal is not current for this project".to_string(),
        ))
    }
}

fn workflow_delivery_error(error: WorkflowError) -> DeliveryError {
    match error.code {
        WorkflowErrorCode::NotFound => DeliveryError::NotFound("workflow entity".to_string()),
        WorkflowErrorCode::AuthorityConflict => {
            DeliveryError::AuthorityDenied("workflow authority is stale".to_string())
        }
        WorkflowErrorCode::InvalidDigest | WorkflowErrorCode::CorruptStore => {
            DeliveryError::CorruptStore("workflow integrity validation failed".to_string())
        }
        WorkflowErrorCode::OrganizationUnavailable
        | WorkflowErrorCode::ExecutionUnavailable
        | WorkflowErrorCode::CompletionUnavailable
        | WorkflowErrorCode::GateUnavailable
        | WorkflowErrorCode::UnknownOutcome => DeliveryError::AdapterUnavailable {
            dependency: "workflow",
            reason: error.message.to_string(),
        },
        WorkflowErrorCode::VersionConflict | WorkflowErrorCode::IdempotencyConflict => {
            DeliveryError::Conflict("workflow state changed concurrently".to_string())
        }
        WorkflowErrorCode::PersistenceFailure => {
            DeliveryError::Storage("workflow storage is unavailable".to_string())
        }
        WorkflowErrorCode::InvalidInput | WorkflowErrorCode::InvalidTransition => {
            DeliveryError::Validation("workflow state is not delivery-ready".to_string())
        }
    }
}

fn current_principal(
    api: &WorkflowApi,
    project: &sentinel_workflow::ProjectV1,
    role: CompanyRoleV1,
) -> Result<PrincipalV1, DeliveryError> {
    let participant = project
        .governance
        .participants
        .iter()
        .find(|participant| participant.role == role)
        .ok_or_else(|| {
            DeliveryError::AuthorityDenied(format!("project has no current {role:?} participant"))
        })?;
    let bound = api
        .principals
        .principal(&participant.principal_id)
        .filter(|bound| {
            bound.principal.tenant_id == project.tenant_id
                && bound.principal.agent_id == Some(participant.agent_id)
                && bound.principal.role == role
        })
        .ok_or_else(|| DeliveryError::AuthorityDenied("project principal is stale".to_string()))?;
    delivery_principal(&bound.principal).ok_or_else(|| {
        DeliveryError::AuthorityDenied("project principal has no delivery role".to_string())
    })
}

fn customer_principal(
    api: &WorkflowApi,
    project: &sentinel_workflow::ProjectV1,
    agreement: &sentinel_workflow::AgreementV1,
) -> Result<PrincipalV1, DeliveryError> {
    let bound = api
        .principals
        .principal(&agreement.accepted_by)
        .filter(|bound| {
            bound.principal.tenant_id == project.tenant_id
                && bound.principal.role == CompanyRoleV1::Customer
        })
        .ok_or_else(|| DeliveryError::AuthorityDenied("customer principal is stale".to_string()))?;
    delivery_principal(&bound.principal).ok_or_else(|| {
        DeliveryError::AuthorityDenied("customer principal has no delivery role".to_string())
    })
}

fn load_material(
    api: &WorkflowApi,
    tenant_id: &TenantId,
    project_id: &ProjectId,
) -> Result<ProjectMaterial, DeliveryError> {
    let project = api
        .store
        .company_project(tenant_id, project_id)
        .map_err(workflow_delivery_error)?
        .ok_or_else(|| DeliveryError::NotFound(format!("workflow project {project_id}")))?;
    if project.lifecycle_state != ProjectLifecycleStateV1::DeliveryCandidate
        || project
            .work_items
            .values()
            .any(|work| work.state != CompanyWorkStateV1::Done)
    {
        return Err(DeliveryError::StaleEvidence(
            "workflow project is not a completed delivery candidate".to_string(),
        ));
    }
    let agreement = api
        .store
        .company_agreement(tenant_id, &project.agreement_id)
        .map_err(workflow_delivery_error)?
        .ok_or_else(|| DeliveryError::NotFound("workflow agreement".to_string()))?;
    let request = api
        .store
        .company_customer_request(tenant_id, &agreement.request_id)
        .map_err(workflow_delivery_error)?
        .ok_or_else(|| DeliveryError::NotFound("workflow customer request".to_string()))?;
    let agreement_ref = VersionedRefV1 {
        id: agreement.agreement_id.clone(),
        generation: 1,
        digest: ContentDigest::parse(agreement.proposal_digest.clone())?,
    };
    let project_ref = VersionedRefV1 {
        id: project.project_id.0.clone(),
        generation: project.version,
        digest: ContentDigest::of_domain("workflow-project", DELIVERY_SCHEMA_V1, &project)?,
    };
    let work_items_digest = ContentDigest::of_domain(
        "workflow-work-items",
        DELIVERY_SCHEMA_V1,
        &project.work_items,
    )?;
    let mut artifacts = Vec::new();
    let mut implementers = BTreeSet::new();
    let mut execution_plans = Vec::new();
    for (work_item_id, work) in &project.work_items {
        let execution = api
            .store
            .work_item(tenant_id, project_id, work_item_id)
            .map_err(workflow_delivery_error)?
            .ok_or_else(|| DeliveryError::MissingEvidence(format!("execution {work_item_id}")))?;
        if execution.state != WorkItemState::Done {
            return Err(DeliveryError::MissingEvidence(format!(
                "work item {work_item_id} is not execution-complete"
            )));
        }
        let terminal = execution.terminal_execution_evidence.ok_or_else(|| {
            DeliveryError::MissingEvidence(format!("terminal evidence {work_item_id}"))
        })?;
        let assignment = work
            .assignments
            .iter()
            .find(|assignment| assignment.active)
            .ok_or_else(|| {
                DeliveryError::AuthorityDenied(format!("active assignment {work_item_id}"))
            })?;
        let owner = project
            .governance
            .participants
            .iter()
            .find(|participant| participant.agent_id == assignment.agent_id)
            .ok_or_else(|| {
                DeliveryError::AuthorityDenied(format!("artifact owner {work_item_id}"))
            })?;
        if !matches!(
            owner.role,
            CompanyRoleV1::Designer | CompanyRoleV1::Developer
        ) {
            continue;
        }
        implementers.insert(owner.principal_id.clone());
        for (ordinal, artifact) in terminal.artifacts.iter().enumerate() {
            artifacts.push(ArtifactRefV1 {
                artifact_id: format!("{}-{}-{ordinal}", work_item_id.0, artifact.artifact_kind),
                generation: 1,
                digest: ContentDigest::parse(artifact.digest.clone())?,
                media_type: artifact.media_type.clone(),
                owner_principal_id: owner.principal_id.clone(),
            });
        }
        execution_plans.push(execution.plan);
    }
    artifacts.sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));
    if artifacts.is_empty() || implementers.is_empty() {
        return Err(DeliveryError::MissingEvidence(
            "delivery candidate has no sealed implementer artifact".to_string(),
        ));
    }
    let source_digest =
        ContentDigest::of_domain("m0-source-inventory", DELIVERY_SCHEMA_V1, &artifacts)?;
    let toolchain_digest = ContentDigest::of_domain(
        "m0-workbench-toolchain",
        DELIVERY_SCHEMA_V1,
        &execution_plans,
    )?;
    let runtime_profile_digest = ContentDigest::of_domain(
        "m0-runtime-profile",
        DELIVERY_SCHEMA_V1,
        &execution_plans
            .iter()
            .map(|plan| {
                (
                    &plan.runtime_key,
                    &plan.runtime_digest,
                    &plan.profile_digest,
                )
            })
            .collect::<Vec<_>>(),
    )?;
    let cost = CostRefV1 {
        ledger_id: format!("ledger-{}", project.project_id.0),
        generation: project.version,
        digest: ContentDigest::of_domain(
            "m0-project-cost",
            DELIVERY_SCHEMA_V1,
            &(
                project.cost_ceiling_micros,
                project.reserved_cost_micros,
                project.committed_cost_micros,
                &project.reservations,
            ),
        )?,
        currency: "USD".to_string(),
        amount_minor: project.committed_cost_micros / 10_000,
    };
    let candidate = ReleaseCandidateV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        candidate_id: format!("candidate-{}-{}", project.project_id.0, project.version + 1),
        generation: project.version + 1,
        tenant_id: tenant_id.0.clone(),
        agreement: agreement_ref,
        project: project_ref,
        work_items_digest,
        source_digest,
        artifacts,
        toolchain_digest,
        runtime_profile_digest,
        acceptance_criteria_digest: ContentDigest::parse(project.agreement_digest.clone())?,
        implementer_principal_ids: implementers,
        cost,
        state: CandidateState::Draft,
        candidate_digest: ContentDigest::zero(),
        created_at_ms: project.updated_at_unix_ms,
    }
    .seal()?;
    Ok(ProjectMaterial {
        project,
        agreement,
        request,
        candidate,
    })
}

fn data_control(project: &sentinel_workflow::ProjectV1) -> Result<DataControlV1, DeliveryError> {
    Ok(DataControlV1 {
        classification: "internal".to_string(),
        encryption_key_owner: "sentinel".to_string(),
        access_policy_digest: ContentDigest::of_domain(
            "m0-access-policy",
            DELIVERY_SCHEMA_V1,
            &project.governance,
        )?,
        redaction_policy_digest: ContentDigest::of_domain(
            "m0-redaction-policy",
            DELIVERY_SCHEMA_V1,
            &project.project_id,
        )?,
        retention_frontier: VersionedRefV1 {
            id: format!("retention-{}", project.project_id.0),
            generation: project.version,
            digest: ContentDigest::of_domain(
                "m0-retention-frontier",
                DELIVERY_SCHEMA_V1,
                &(project.version, project.updated_at_unix_ms),
            )?,
        },
        audit_policy_digest: ContentDigest::of_domain(
            "m0-audit-policy",
            DELIVERY_SCHEMA_V1,
            &project.project_id,
        )?,
    })
}

fn build_plan(material: &ProjectMaterial) -> Result<QaEvaluationPlanV1, DeliveryError> {
    let fixtures = m0_qa_fixture_cases()?;
    QaEvaluationPlanV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        plan_id: format!(
            "qa-plan-{}-{}",
            material.project.project_id.0, material.candidate.generation
        ),
        generation: material.candidate.generation,
        request: VersionedRefV1 {
            id: material.request.request_id.clone(),
            generation: material.request.version,
            digest: ContentDigest::of_domain(
                "workflow-request",
                DELIVERY_SCHEMA_V1,
                &material.request,
            )?,
        },
        candidate: VersionedRefV1 {
            id: material.candidate.candidate_id.clone(),
            generation: material.candidate.generation,
            digest: material.candidate.candidate_digest.clone(),
        },
        agreement: material.candidate.agreement.clone(),
        project: material.candidate.project.clone(),
        work_items_digest: material.candidate.work_items_digest.clone(),
        acceptance_criteria_digest: material.candidate.acceptance_criteria_digest.clone(),
        required_case_ids: fixtures
            .iter()
            .filter(|case| case.required)
            .map(|case| case.case_id.clone())
            .collect(),
        optional_case_ids: fixtures
            .iter()
            .filter(|case| !case.required)
            .map(|case| case.case_id.clone())
            .collect(),
        fixture_inventory_digest: qa_fixture_inventory_digest(&fixtures)?,
        evaluator_policy_digest: ContentDigest::of_domain(
            "m0-qa-evaluator-policy",
            DELIVERY_SCHEMA_V1,
            &material.project.governance,
        )?,
        aggregation_policy_digest: ContentDigest::of_domain(
            "m0-qa-aggregation-policy",
            DELIVERY_SCHEMA_V1,
            &fixtures,
        )?,
        release_policy_digest: ContentDigest::of_domain(
            "m0-release-policy",
            DELIVERY_SCHEMA_V1,
            &material.project.project_id,
        )?,
        runner_binary_digest: ContentDigest::of_domain(
            "m0-qa-runner",
            DELIVERY_SCHEMA_V1,
            &"sentinel-web-qa",
        )?,
        toolchain_digest: material.candidate.toolchain_digest.clone(),
        sandbox_profile_digest: material.candidate.runtime_profile_digest.clone(),
        capability_digest: ContentDigest::of_domain(
            "m0-qa-capabilities",
            DELIVERY_SCHEMA_V1,
            &BTreeSet::from(["test.run_profile"]),
        )?,
        environment_digest: ContentDigest::of_domain(
            "m0-qa-environment",
            DELIVERY_SCHEMA_V1,
            &"web-qa-v1",
        )?,
        credential_policy_digest: ContentDigest::of_domain(
            "m0-qa-credential-policy",
            DELIVERY_SCHEMA_V1,
            &"no-provider-credentials",
        )?,
        declared_seeds: BTreeSet::new(),
        retry_limit: 0,
        retryable_classes: BTreeSet::new(),
        data_control: data_control(&material.project)?,
        plan_digest: ContentDigest::zero(),
    }
    .seal()
}

fn run_id(material: &ProjectMaterial) -> String {
    format!(
        "qa-run-{}-{}",
        material.project.project_id.0, material.candidate.generation
    )
}

fn lineage_time(material: &ProjectMaterial) -> u64 {
    material.candidate.created_at_ms
}

fn delivery_preview_window(now_ms: u64) -> Result<(u64, u64), DeliveryError> {
    let expires_at_ms = now_ms
        .checked_add(DELIVERY_PREVIEW_MAX_TTL_MS)
        .ok_or_else(|| {
            DeliveryError::Validation("delivery preview timestamp overflow".to_string())
        })?;
    Ok((now_ms, expires_at_ms))
}

fn require_live_preview_or_replay(
    observed_now_ms: u64,
    expires_at_ms: u64,
    already_committed: bool,
) -> Result<(), DeliveryError> {
    if observed_now_ms < expires_at_ms || already_committed {
        Ok(())
    } else {
        Err(DeliveryError::Conflict(
            "delivery preview expired before the first durable commit".to_string(),
        ))
    }
}

fn build_run(
    material: &ProjectMaterial,
    plan: &QaEvaluationPlanV1,
    qa: PrincipalV1,
) -> Result<QaEvaluationRunReceiptV1, DeliveryError> {
    Ok(QaEvaluationRunReceiptV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        run_id: run_id(material),
        generation: material.candidate.generation,
        plan: VersionedRefV1 {
            id: plan.plan_id.clone(),
            generation: plan.generation,
            digest: plan.plan_digest.clone(),
        },
        request_digest: ContentDigest::of_domain(
            "m0-qa-run-request",
            DELIVERY_SCHEMA_V1,
            &(&plan.plan_digest, &qa),
        )?,
        state: QaRunState::Planned,
        retry_of: None,
        supersedes: None,
        actors: vec![qa],
        durable_event_generation: 0,
        started_at_ms: None,
        finished_at_ms: None,
        attempts: 0,
        case_attempt_history_digest: None,
        harness_outcome: None,
        cleanup_receipt: None,
        aggregate_outcomes: None,
        gate_receipt: None,
    })
}

fn prepare_candidate(
    api: &WorkflowApi,
    delivery: &super::ProductDeliveryCore,
    caller: &PrincipalV1,
    operation_id: Uuid,
    now_ms: u64,
    project_id: &ProjectId,
) -> Result<DeliveryIntentResponse, DeliveryError> {
    require_role(caller, AuthorityRole::Developer)?;
    let material = load_material(api, &TenantId(caller.tenant_id.clone()), project_id)?;
    if !material
        .candidate
        .implementer_principal_ids
        .contains(&caller.principal_id)
    {
        return Err(DeliveryError::AuthorityDenied(
            "delivery candidate must be prepared by an assigned implementer".to_string(),
        ));
    }
    let receipt = delivery.register_candidate(
        &context(caller.clone(), operation_id, "candidate", now_ms),
        material.candidate.clone(),
    )?;
    Ok(response(
        receipt.duplicate,
        "prepare_candidate",
        &material,
        None,
        None,
        None,
        None,
        None,
    ))
}

fn assign_qa(
    api: &WorkflowApi,
    delivery: &super::ProductDeliveryCore,
    caller: &PrincipalV1,
    operation_id: Uuid,
    now_ms: u64,
    project_id: &ProjectId,
) -> Result<DeliveryIntentResponse, DeliveryError> {
    require_role(caller, AuthorityRole::ReleaseManager)?;
    let material = load_material(api, &TenantId(caller.tenant_id.clone()), project_id)?;
    let release_manager = current_principal(api, &material.project, CompanyRoleV1::ReleaseManager)?;
    require_current_principal(caller, &release_manager)?;
    let plan = build_plan(&material)?;
    let qa = current_principal(api, &material.project, CompanyRoleV1::Qa)?;
    let run = build_run(&material, &plan, qa)?;
    let receipt = delivery.assign_qa(
        &context(caller.clone(), operation_id, "assign-qa", now_ms),
        &caller.tenant_id,
        &project_id.0,
        &material.candidate.candidate_id,
        plan,
        run,
    )?;
    Ok(response(
        receipt.duplicate,
        "assign_qa",
        &material,
        Some(run_id(&material)),
        None,
        None,
        None,
        None,
    ))
}

fn execute_qa(
    api: &WorkflowApi,
    delivery: &super::ProductDeliveryCore,
    caller: &PrincipalV1,
    operation_id: Uuid,
    now_ms: u64,
    project_id: &ProjectId,
) -> Result<DeliveryIntentResponse, DeliveryError> {
    require_role(caller, AuthorityRole::Qa)?;
    let material = load_material(api, &TenantId(caller.tenant_id.clone()), project_id)?;
    let qa = current_principal(api, &material.project, CompanyRoleV1::Qa)?;
    require_current_principal(caller, &qa)?;
    let plan = build_plan(&material)?;
    let run_id = run_id(&material);
    let admitted = delivery.transition_qa(
        &context(caller.clone(), operation_id, "qa-admit", now_ms),
        &caller.tenant_id,
        &project_id.0,
        &run_id,
        QaRunState::Admitted,
    )?;
    let running = delivery.transition_qa(
        &context(caller.clone(), operation_id, "qa-running", now_ms),
        &caller.tenant_id,
        &project_id.0,
        &run_id,
        QaRunState::Running,
    )?;
    let (executed, receipt) = delivery.execute_qa(
        &context(caller.clone(), operation_id, "qa-execute", now_ms),
        &caller.tenant_id,
        &project_id.0,
        &run_id,
    )?;
    let run_ref = VersionedRefV1 {
        id: run_id.clone(),
        generation: material.candidate.generation,
        digest: ContentDigest::of_domain(
            "m0-qa-run-request",
            DELIVERY_SCHEMA_V1,
            &(&plan.plan_digest, caller),
        )?,
    };
    let graph = m0_qa_evidence_graph(&run_ref, &plan.plan_digest, &receipt)?;
    if receipt.result_inventory_digest != qa_evidence_inventory_digest(&graph)? {
        return Err(DeliveryError::StaleEvidence(
            "QA receipt and evidence graph inventory differ".to_string(),
        ));
    }
    let evidence = delivery.import_evidence_graph(
        &context(caller.clone(), operation_id, "qa-evidence", now_ms),
        &caller.tenant_id,
        &project_id.0,
        &run_id,
        graph.clone(),
    )?;
    let terminal_state = match receipt.harness_outcome {
        QaHarnessOutcome::Pass => QaRunState::CompletedPass,
        QaHarnessOutcome::Fail => QaRunState::CompletedFail,
        QaHarnessOutcome::Error => QaRunState::HarnessError,
    };
    let terminal = delivery.transition_qa(
        &context(caller.clone(), operation_id, "qa-terminal", now_ms),
        &caller.tenant_id,
        &project_id.0,
        &run_id,
        terminal_state,
    )?;
    if terminal_state != QaRunState::CompletedPass {
        return Err(DeliveryError::MissingEvidence(
            "M0 deterministic QA did not pass".to_string(),
        ));
    }
    let release_manager = current_principal(api, &material.project, CompanyRoleV1::ReleaseManager)?;
    let (manifest, gate) = build_manifest_and_gate(
        &material,
        &plan,
        &graph,
        &receipt,
        caller.clone(),
        release_manager,
        lineage_time(&material),
    )?;
    let candidate_ref = gate.candidate.clone();
    let gate_ref = VersionedRefV1 {
        id: gate.gate_id.clone(),
        generation: gate.generation,
        digest: ContentDigest::of_domain("qa-release-gate", DELIVERY_SCHEMA_V1, &gate)?,
    };
    let review = delivery.record_review_bundle(
        &context(caller.clone(), operation_id, "qa-review", now_ms),
        &caller.tenant_id,
        &project_id.0,
        &run_id,
        ReviewV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            review_id: format!("review-{}", material.candidate.candidate_id),
            generation: material.candidate.generation,
            candidate: candidate_ref.clone(),
            reviewer: caller.clone(),
            findings_digest: ContentDigest::of_domain(
                "qa-findings",
                DELIVERY_SCHEMA_V1,
                &Vec::<crate::delivery::FindingV1>::new(),
            )?,
            approved: true,
            created_at_ms: lineage_time(&material),
        },
        TestRunV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            test_run_id: format!("test-run-{}", material.candidate.candidate_id),
            generation: material.candidate.generation,
            candidate: candidate_ref.clone(),
            qa_plan: gate.plan.clone(),
            runner_receipt: VersionedRefV1 {
                id: receipt.invocation.id.clone(),
                generation: receipt.invocation.generation,
                digest: receipt.receipt_digest.clone(),
            },
            result_inventory_digest: receipt.result_inventory_digest.clone(),
            logs_digest: receipt.logs_digest.clone(),
            screenshots_digest: receipt.screenshots_digest.clone(),
            passed: true,
        },
        vec![],
        Some(ApprovalV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            approval_id: format!("approval-{}", material.candidate.candidate_id),
            generation: material.candidate.generation,
            candidate: candidate_ref,
            gate: gate_ref,
            approver: caller.clone(),
            policy_digest: gate.policy_digest.clone(),
            approved_at_ms: lineage_time(&material),
        }),
    )?;
    let gate = delivery.record_gate(
        &context(caller.clone(), operation_id, "qa-gate", now_ms),
        &caller.tenant_id,
        &project_id.0,
        &run_id,
        gate,
    )?;
    let _ = manifest;
    Ok(response(
        [
            admitted, running, executed, evidence, terminal, review, gate,
        ]
        .iter()
        .all(|receipt| receipt.duplicate),
        "execute_qa",
        &material,
        Some(run_id),
        None,
        None,
        None,
        None,
    ))
}

#[allow(clippy::too_many_arguments)]
fn build_manifest_and_gate(
    material: &ProjectMaterial,
    plan: &QaEvaluationPlanV1,
    graph: &crate::delivery::QaEvidenceGraphV1,
    receipt: &crate::delivery::WorkbenchEvidenceReceiptV1,
    qa: PrincipalV1,
    release_manager: PrincipalV1,
    now_ms: u64,
) -> Result<(ReleaseManifestV1, QaReleaseGateReceiptV1), DeliveryError> {
    let candidate_ref = VersionedRefV1 {
        id: material.candidate.candidate_id.clone(),
        generation: material.candidate.generation,
        digest: material.candidate.candidate_digest.clone(),
    };
    let plan_ref = VersionedRefV1 {
        id: plan.plan_id.clone(),
        generation: plan.generation,
        digest: plan.plan_digest.clone(),
    };
    let gate_id = format!("gate-{}", material.candidate.candidate_id);
    let mut manifest = ReleaseManifestV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        manifest_id: format!("manifest-{}", material.candidate.candidate_id),
        generation: material.candidate.generation,
        tenant_id: material.candidate.tenant_id.clone(),
        agreement: material.candidate.agreement.clone(),
        project: material.candidate.project.clone(),
        candidate: candidate_ref.clone(),
        work_items_digest: material.candidate.work_items_digest.clone(),
        source_digest: material.candidate.source_digest.clone(),
        artifacts: material.candidate.artifacts.clone(),
        toolchain_digest: material.candidate.toolchain_digest.clone(),
        runtime_profile_digest: material.candidate.runtime_profile_digest.clone(),
        qa_gate: VersionedRefV1 {
            id: gate_id.clone(),
            generation: material.candidate.generation,
            digest: ContentDigest::zero(),
        },
        qa_evidence_digest: graph.graph_digest.clone(),
        sbom_digest: ContentDigest::of_domain(
            "m0-sbom",
            DELIVERY_SCHEMA_V1,
            &material.candidate.artifacts,
        )?,
        dependency_snapshot_digest: ContentDigest::of_domain(
            "m0-dependency-snapshot",
            DELIVERY_SCHEMA_V1,
            &material.candidate.toolchain_digest,
        )?,
        provenance_digest: ContentDigest::of_domain(
            "m0-provenance",
            DELIVERY_SCHEMA_V1,
            &(
                &material.candidate.candidate_digest,
                &graph.graph_digest,
                &receipt.receipt_digest,
            ),
        )?,
        release_actor: release_manager,
        cost: material.candidate.cost.clone(),
        rollback_release: None,
        manifest_digest: ContentDigest::zero(),
        created_at_ms: now_ms,
    };
    let gate = QaReleaseGateReceiptV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        gate_id,
        generation: material.candidate.generation,
        candidate: candidate_ref,
        plan: plan_ref,
        case_inventory_digest: qa_case_inventory_digest(graph)?,
        deterministic_evidence_digest: qa_deterministic_evidence_digest(graph)?,
        model_evidence_digest: qa_model_evidence_digest(graph)?,
        calibration_digest: None,
        source_evidence_digest: qa_source_evidence_digest(graph)?,
        flake_disposition_digest: qa_flake_disposition_digest(graph)?,
        policy_digest: plan.release_policy_digest.clone(),
        release_manifest_digest: manifest.gate_input_digest()?,
        actor: qa,
        passed: true,
        issued_at_ms: now_ms,
        expires_at_ms: u64::MAX,
    };
    manifest.qa_gate.digest =
        ContentDigest::of_domain("qa-release-gate", DELIVERY_SCHEMA_V1, &gate)?;
    Ok((manifest.seal()?, gate))
}

fn release(
    api: &WorkflowApi,
    delivery: &super::ProductDeliveryCore,
    caller: &PrincipalV1,
    operation_id: Uuid,
    now_ms: u64,
    observed_now_ms: u64,
    project_id: &ProjectId,
) -> Result<DeliveryIntentResponse, DeliveryError> {
    require_role(caller, AuthorityRole::ReleaseManager)?;
    let material = load_material(api, &TenantId(caller.tenant_id.clone()), project_id)?;
    let release_manager = current_principal(api, &material.project, CompanyRoleV1::ReleaseManager)?;
    require_current_principal(caller, &release_manager)?;
    let plan = build_plan(&material)?;
    let run_id = run_id(&material);
    let aggregate = delivery
        .aggregate(&caller.tenant_id, &project_id.0)?
        .ok_or_else(|| DeliveryError::NotFound("delivery aggregate".to_string()))?;
    let run = aggregate
        .qa_runs
        .get(&run_id)
        .ok_or_else(|| DeliveryError::MissingEvidence("QA run".to_string()))?;
    let graph = aggregate
        .evidence_graphs
        .get(&run_id)
        .ok_or_else(|| DeliveryError::MissingEvidence("QA evidence graph".to_string()))?;
    let receipt = aggregate
        .workbench_receipts
        .values()
        .find(|receipt| receipt.qa_run.id == run_id)
        .ok_or_else(|| DeliveryError::MissingEvidence("QA workbench receipt".to_string()))?;
    let gate = aggregate
        .gates
        .values()
        .find(|gate| gate.candidate.id == material.candidate.candidate_id)
        .ok_or_else(|| DeliveryError::MissingEvidence("QA release gate".to_string()))?;
    if run.state != QaRunState::CompletedPass || !gate.passed {
        return Err(DeliveryError::MissingEvidence(
            "release requires passing terminal QA".to_string(),
        ));
    }
    let delivery_id = format!("delivery-{}", material.candidate.candidate_id);
    let (issued_at_ms, expires_at_ms) = delivery_preview_window(now_ms)?;
    require_live_preview_or_replay(
        observed_now_ms,
        expires_at_ms,
        aggregate.deliveries.contains_key(&delivery_id),
    )?;
    let qa = gate.actor.clone();
    let (manifest, expected_gate) = build_manifest_and_gate(
        &material,
        &plan,
        graph,
        receipt,
        qa,
        caller.clone(),
        lineage_time(&material),
    )?;
    if &expected_gate != gate {
        return Err(DeliveryError::StaleEvidence(
            "release manifest no longer matches the recorded QA gate".to_string(),
        ));
    }
    let release_id = format!("release-{}", material.candidate.candidate_id);
    let manifest_ref = VersionedRefV1 {
        id: manifest.manifest_id.clone(),
        generation: manifest.generation,
        digest: manifest.manifest_digest.clone(),
    };
    let promotion = delivery.promote(
        &context(caller.clone(), operation_id, "promote", now_ms),
        &caller.tenant_id,
        &project_id.0,
        &material.candidate.candidate_id,
        manifest,
        ReleaseV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            release_id: release_id.clone(),
            generation: material.candidate.generation,
            manifest: manifest_ref,
            state: ReleaseState::Approved,
            activated_at_ms: None,
            rollout_receipt: None,
        },
    )?;
    let promoted = delivery
        .aggregate(&caller.tenant_id, &project_id.0)?
        .ok_or_else(|| DeliveryError::NotFound("promoted delivery aggregate".to_string()))?;
    let active = promoted
        .releases
        .get(&release_id)
        .ok_or_else(|| DeliveryError::MissingEvidence("active release".to_string()))?;
    let customer = customer_principal(api, &material.project, &material.agreement)?;
    let release_ref = VersionedRefV1 {
        id: active.release_id.clone(),
        generation: active.generation,
        digest: canonical_release_reference_digest(active)?,
    };
    let receipt = DeliveryReceiptV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        delivery_id: delivery_id.clone(),
        generation: material.candidate.generation,
        tenant_id: caller.tenant_id.clone(),
        release: release_ref,
        customer_principal_id: customer.principal_id,
        preview_digest: ContentDigest::of_domain(
            "m0-preview",
            DELIVERY_SCHEMA_V1,
            &material.candidate.source_digest,
        )?,
        preview_ttl_policy_version: DELIVERY_PREVIEW_TTL_POLICY_V1,
        receipt_digest: ContentDigest::zero(),
        state: DeliveryState::PreviewReady,
        issued_at_ms,
        expires_at_ms,
    }
    .seal()?;
    let issued = delivery.issue_delivery(
        &context(caller.clone(), operation_id, "issue-delivery", now_ms),
        &project_id.0,
        receipt,
    )?;
    Ok(response(
        promotion.duplicate && issued.duplicate,
        "release",
        &material,
        Some(run_id),
        Some(release_id),
        Some(delivery_id),
        None,
        None,
    ))
}

fn accept(
    api: &WorkflowApi,
    delivery: &super::ProductDeliveryCore,
    caller: &PrincipalV1,
    operation_id: Uuid,
    now_ms: u64,
    observed_now_ms: u64,
    project_id: &ProjectId,
) -> Result<DeliveryIntentResponse, DeliveryError> {
    require_role(caller, AuthorityRole::Customer)?;
    let material = load_material(api, &TenantId(caller.tenant_id.clone()), project_id)?;
    let customer = customer_principal(api, &material.project, &material.agreement)?;
    require_current_principal(caller, &customer)?;
    let aggregate = delivery
        .aggregate(&caller.tenant_id, &project_id.0)?
        .ok_or_else(|| DeliveryError::NotFound("delivery aggregate".to_string()))?;
    let release_id = format!("release-{}", material.candidate.candidate_id);
    let delivery_id = format!("delivery-{}", material.candidate.candidate_id);
    let release = aggregate
        .releases
        .get(&release_id)
        .ok_or_else(|| DeliveryError::MissingEvidence("active release".to_string()))?;
    let receipt = aggregate
        .deliveries
        .get(&delivery_id)
        .ok_or_else(|| DeliveryError::MissingEvidence("delivery receipt".to_string()))?;
    let acceptance_id = format!("acceptance-{delivery_id}");
    require_live_preview_or_replay(
        observed_now_ms,
        receipt.expires_at_ms,
        aggregate.acceptances.contains_key(&acceptance_id),
    )?;
    if receipt.customer_principal_id != caller.principal_id {
        return Err(DeliveryError::AuthorityDenied(
            "delivery belongs to another customer".to_string(),
        ));
    }
    let delivery_ref = VersionedRefV1 {
        id: receipt.delivery_id.clone(),
        generation: receipt.generation,
        digest: receipt.receipt_digest.clone(),
    };
    let release_ref = VersionedRefV1 {
        id: release.release_id.clone(),
        generation: release.generation,
        digest: canonical_release_reference_digest(release)?,
    };
    let feedback = CustomerFeedbackV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        feedback_id: format!("feedback-{delivery_id}"),
        generation: material.candidate.generation,
        delivery: delivery_ref.clone(),
        customer: caller.clone(),
        action: CustomerAction::Accept,
        feedback_digest: ContentDigest::zero(),
        requested_work_item_refs: vec![],
        created_at_ms: now_ms,
    }
    .seal()?;
    let acceptance = AcceptanceV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        acceptance_id: acceptance_id.clone(),
        generation: material.candidate.generation,
        delivery: delivery_ref,
        release: release_ref,
        customer: caller.clone(),
        acceptance_digest: ContentDigest::zero(),
        accepted_at_ms: now_ms,
    }
    .seal()?;
    let receipt = delivery.customer_action(
        &context(caller.clone(), operation_id, "customer-accept", now_ms),
        &caller.tenant_id,
        &project_id.0,
        feedback,
        Some(acceptance),
    )?;
    Ok(response(
        receipt.duplicate,
        "accept",
        &material,
        Some(run_id(&material)),
        Some(release_id),
        Some(delivery_id),
        Some(acceptance_id),
        None,
    ))
}

fn closeout(
    api: &WorkflowApi,
    delivery: &super::ProductDeliveryCore,
    caller: &PrincipalV1,
    operation_id: Uuid,
    now_ms: u64,
    project_id: &ProjectId,
) -> Result<DeliveryIntentResponse, DeliveryError> {
    require_role(caller, AuthorityRole::ReleaseManager)?;
    let material = load_material(api, &TenantId(caller.tenant_id.clone()), project_id)?;
    let release_manager = current_principal(api, &material.project, CompanyRoleV1::ReleaseManager)?;
    require_current_principal(caller, &release_manager)?;
    let aggregate = delivery
        .aggregate(&caller.tenant_id, &project_id.0)?
        .ok_or_else(|| DeliveryError::NotFound("delivery aggregate".to_string()))?;
    let release_id = format!("release-{}", material.candidate.candidate_id);
    let delivery_id = format!("delivery-{}", material.candidate.candidate_id);
    let release = aggregate
        .releases
        .get(&release_id)
        .ok_or_else(|| DeliveryError::MissingEvidence("accepted release".to_string()))?;
    let acceptance = aggregate
        .acceptances
        .values()
        .find(|acceptance| acceptance.delivery.id == delivery_id)
        .ok_or_else(|| DeliveryError::MissingEvidence("customer acceptance".to_string()))?;
    let closeout_id = format!("closeout-{}", material.candidate.candidate_id);
    let receipt = delivery.closeout(
        &context(caller.clone(), operation_id, "closeout", now_ms),
        &caller.tenant_id,
        &project_id.0,
        ProjectCloseoutV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            closeout_id: closeout_id.clone(),
            generation: material.candidate.generation,
            project: material.candidate.project.clone(),
            accepted_release: VersionedRefV1 {
                id: release.release_id.clone(),
                generation: release.generation,
                digest: canonical_release_reference_digest(release)?,
            },
            acceptance: VersionedRefV1 {
                id: acceptance.acceptance_id.clone(),
                generation: acceptance.generation,
                digest: acceptance.acceptance_digest.clone(),
            },
            decisions_digest: ContentDigest::of_domain(
                "m0-closeout-decisions",
                DELIVERY_SCHEMA_V1,
                &material.project.decisions,
            )?,
            artifact_inventory_digest: ContentDigest::of_domain(
                "m0-closeout-artifacts",
                DELIVERY_SCHEMA_V1,
                &material.candidate.artifacts,
            )?,
            failures_digest: ContentDigest::of_domain(
                "m0-closeout-failures",
                DELIVERY_SCHEMA_V1,
                &material.project.blockers,
            )?,
            lessons_digest: ContentDigest::of_domain(
                "m0-closeout-lessons",
                DELIVERY_SCHEMA_V1,
                &(&material.project.decisions, &material.project.handoffs),
            )?,
            memory_publication: None,
            closed_by: caller.clone(),
            created_at_ms: now_ms,
        },
    )?;
    Ok(response(
        receipt.duplicate,
        "closeout",
        &material,
        Some(run_id(&material)),
        Some(release_id),
        Some(delivery_id),
        Some(acceptance.acceptance_id.clone()),
        Some(closeout_id),
    ))
}

#[allow(clippy::too_many_arguments)]
fn response(
    replayed: bool,
    action: &'static str,
    material: &ProjectMaterial,
    qa_run_id: Option<String>,
    release_id: Option<String>,
    delivery_id: Option<String>,
    acceptance_id: Option<String>,
    closeout_id: Option<String>,
) -> DeliveryIntentResponse {
    DeliveryIntentResponse {
        replayed,
        action,
        tenant_id: material.project.tenant_id.0.clone(),
        project_id: material.project.project_id.0.clone(),
        candidate_id: material.candidate.candidate_id.clone(),
        qa_run_id,
        release_id,
        delivery_id,
        acceptance_id,
        closeout_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal(role: AuthorityRole) -> PrincipalV1 {
        PrincipalV1 {
            tenant_id: "tenant-m0".to_string(),
            principal_id: "principal-m0".to_string(),
            authority_generation: 1,
            roles: BTreeSet::from([role]),
        }
    }

    #[test]
    fn delivery_intent_wire_uses_only_stable_operation_and_project_identity() {
        let operation_id = Uuid::parse_str("018f3f32-4f01-7f2c-a6c1-f6f4a81b2809").unwrap();
        let envelope: DeliveryIntentEnvelope = serde_json::from_value(serde_json::json!({
            "operation_id": operation_id,
            "intent": {"action": "release", "project_id": "project-m0"}
        }))
        .unwrap();
        assert_eq!(envelope.operation_id, operation_id);
        assert!(matches!(envelope.intent, DeliveryIntentV1::Release { .. }));

        assert!(
            serde_json::from_value::<DeliveryIntentEnvelope>(serde_json::json!({
                "operation_id": operation_id,
                "effective_at_ms": 123,
                "intent": {"action": "release", "project_id": "project-m0"}
            }))
            .is_err()
        );
    }

    #[test]
    fn delivery_intent_stage_identity_is_stable_across_retry_time() {
        let operation_id = Uuid::parse_str("018f3f32-4f01-7f2c-a6c1-f6f4a81b2809").unwrap();
        let first = context(
            principal(AuthorityRole::ReleaseManager),
            operation_id,
            "promote",
            100,
        );
        let retry = context(
            principal(AuthorityRole::ReleaseManager),
            operation_id,
            "promote",
            200,
        );
        assert_eq!(first.principal, retry.principal);
        assert_eq!(first.idempotency_key, retry.idempotency_key);
        assert_ne!(first.now_ms, retry.now_ms);
        assert!(first
            .idempotency_key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')));
    }

    #[test]
    fn delivery_preview_window_is_server_timed_and_hard_bounded() {
        let now_ms = 1_000;
        assert_eq!(
            delivery_preview_window(now_ms).unwrap(),
            (now_ms, now_ms + DELIVERY_PREVIEW_MAX_TTL_MS)
        );
        assert!(delivery_preview_window(u64::MAX).is_err());
    }

    #[test]
    fn expired_preview_allows_exact_replay_but_no_first_effect() {
        assert!(require_live_preview_or_replay(1_999, 2_000, false).is_ok());
        assert!(require_live_preview_or_replay(2_000, 2_000, false).is_err());
        assert!(require_live_preview_or_replay(2_001, 2_000, true).is_ok());
    }

    #[test]
    fn delivery_intent_response_exposes_exact_replay_state() {
        let response = DeliveryIntentResponse {
            replayed: true,
            action: "accept",
            tenant_id: "tenant-m0".to_string(),
            project_id: "project-m0".to_string(),
            candidate_id: "candidate-m0".to_string(),
            qa_run_id: Some("qa-run-m0".to_string()),
            release_id: Some("release-m0".to_string()),
            delivery_id: Some("delivery-m0".to_string()),
            acceptance_id: Some("acceptance-m0".to_string()),
            closeout_id: None,
        };
        let value = serde_json::to_value(response).unwrap();
        assert_eq!(value["replayed"], true);
        assert_eq!(value["action"], "accept");
        assert_eq!(value["project_id"], "project-m0");
    }

    #[test]
    fn delivery_intent_role_check_is_fail_closed() {
        assert!(require_role(
            &principal(AuthorityRole::ReleaseManager),
            AuthorityRole::ReleaseManager
        )
        .is_ok());
        assert!(matches!(
            require_role(
                &principal(AuthorityRole::Developer),
                AuthorityRole::ReleaseManager
            ),
            Err(DeliveryError::AuthorityDenied(_))
        ));
    }
}
