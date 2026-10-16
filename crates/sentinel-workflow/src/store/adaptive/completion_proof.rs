//! Historical producer evidence, never a new provider or tool authorization.

use super::*;
use crate::{CompanyWorkItemV1, CompanyWorkStateV1, ProjectId, ProjectV1, SubscriptionCallAllowanceV1, TenantId};

/// Exact pre-claim journal input and its resolved completion. Descriptive only:
/// consumers must still authenticate execution, gate and project membership.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveSourceCompletionV1 {
    pub provider_source: AdaptiveSessionV1,
    pub effect: AdaptiveEffectV1,
    pub model_response_digest: String,
    pub artifact_digest: String,
}

struct Completion {
    source: AdaptiveSourceCompletionV1,
    package: crate::ExecutionToolV1,
    resolved_at_ms: u64,
}

fn validate_allowance_input(allowance: &SubscriptionCallAllowanceV1) -> Result<(), WorkflowError> {
    let grant = &allowance.grant;
    crate::model::validate_identifier(&allowance.allowance_id)?;
    crate::model::validate_identifier(&allowance.created_by)?;
    crate::model::validate_identifier(&grant.assignment_id)?;
    crate::model::validate_identifier(&grant.model)?;
    crate::model::validate_digest(&grant.catalog_digest)?;
    grant.work_item_id.validate()?;
    if grant.schema_version != 1 || grant.agent_id.0 == 0 || grant.assignment_version == 0
        || grant.provider != "codex-cli" || !(1..=crate::ADAPTIVE_SESSION_MAX_CALLS).contains(&grant.max_calls)
        || grant.max_concurrent != 1 || !(1_000..=120_000).contains(&grant.max_duration_ms)
        || allowance.created_at_unix_ms == 0
        || grant.expires_at_unix_ms.checked_sub(allowance.created_at_unix_ms).is_none_or(|window| window == 0 || window > 300_000)
        || (grant.max_duration_ms != 120_000
            && grant.max_duration_ms > grant.expires_at_unix_ms.saturating_sub(allowance.created_at_unix_ms))
    { return Err(authority_conflict()); }
    Ok(())
}

fn completion_plan_id(source: &AdaptiveSourceCompletionV1) -> Uuid {
    let request_id = format!("company-adaptive-{}-{}", source.provider_source.grant.session_id, source.effect.id);
    let hash = Sha256::digest(format!(
        "sentinel.workflow.adaptive-completion.v1:{request_id}:{}:1", source.effect.request_digest,
    ).as_bytes());
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

// Discover canonical namespace keys independently of JSON locators and heads.
// Replaced heads remain evidence; a corrupt root must not disappear in a filter.
pub(super) fn journal_ids(connection: &Connection) -> Result<BTreeSet<Uuid>, WorkflowError> {
    let mut statement = connection.prepare(
        "SELECT DISTINCT operation_namespace FROM workflow_operations
         WHERE operation_namespace GLOB 'adaptive-session-v1:*'
         ORDER BY operation_namespace LIMIT ?1",
    ).map_err(map_sqlite_error)?;
    let mut rows = statement.query([(MAX_LOCATOR_SCAN_ROWS + 1) as i64]).map_err(map_sqlite_error)?;
    let mut ids = BTreeSet::new();
    let mut count = 0;
    while let Some(row) = rows.next().map_err(map_sqlite_error)? {
        count += 1;
        if count > MAX_LOCATOR_SCAN_ROWS { return Err(corrupt_store()); }
        let ns: String = row.get(0).map_err(map_sqlite_error)?;
        let key = ns.strip_prefix("adaptive-session-v1:").ok_or_else(corrupt_store)?;
        let (id, suffix) = key.split_once(':').map_or((key, None), |(id, suffix)| (id, Some(suffix)));
        let parsed = Uuid::parse_str(id).map_err(|_| corrupt_store())?;
        if parsed.is_nil() || parsed.to_string() != id
            || !matches!(suffix, None | Some("operations" | "rejected-model-dispositions"))
        { return Err(corrupt_store()); }
        ids.insert(parsed);
    }
    Ok(ids)
}

pub(super) fn allowance_matches(source: &AdaptiveSessionV1, allowance: &SubscriptionCallAllowanceV1) -> Result<bool, WorkflowError> {
    let authority = &source.grant.authority;
    let effective = source.effective_grant();
    let window_matches = match source.continuation.as_ref().and_then(|state| state.authorizations.last()) {
        Some(authorization) => allowance.grant.max_calls == authorization.additional_model_calls
            && allowance.created_at_unix_ms == authorization.issued_at_ms
            && allowance.grant.expires_at_unix_ms == authorization.deadline_ms
            && allowance.grant.max_duration_ms == effective.max_call_duration_ms,
        None => allowance.grant.max_calls >= source.grant.max_model_calls,
    };
    Ok(allowance.dispatch.is_none()
        && window_matches
        && allowance.allowance_id == source.active_provider_allowance_id()
        && allowance.grant.schema_version == 1
        && allowance.grant.work_item_id == authority.work_item_id
        && allowance.grant.agent_id == authority.agent_id
        && allowance.grant.assignment_version == authority.assignment_version
        && allowance.grant.provider == effective.provider
        && allowance.grant.model == effective.model
        && allowance.grant.catalog_digest == effective.catalog_digest
        && allowance.created_at_unix_ms <= effective.created_at_ms
        && allowance.grant.expires_at_unix_ms >= effective.deadline_ms
        && allowance.grant.max_duration_ms >= effective.max_call_duration_ms
        && adaptive_continuation_provider_digest(allowance, authority)? == effective.provider_authority_digest)
}

fn completions(
    connection: &Connection,
    tenant: &TenantId,
    project: &ProjectId,
    allowance: &SubscriptionCallAllowanceV1,
) -> Result<Vec<Completion>, WorkflowError> {
    let mut result = Vec::new();
    let mut selected = 0;
    let mut lineages = BTreeSet::new();
    for id in journal_ids(connection)? {
        let root = journal_entry(connection, id, 1)?.ok_or_else(corrupt_store)?;
        require_journal_step(connection, &root, None)?;
        let authority = &root.0.entry.session.grant.authority;
        if authority.tenant_id != *tenant || authority.project_id != *project
            || authority.work_item_id != allowance.grant.work_item_id
            || authority.agent_id != allowance.grant.agent_id
        { continue; }
        selected += 1;
        if selected > MAX_SCOPED_ADAPTIVE_HEADS { return Err(corrupt_store()); }
        if lineages.insert(authority.canonical_digest()?) {
            require_canonical_journal_lineage(connection, authority)?;
        }
        if let Some(completion) = completion_for_session(connection, id, tenant, project, allowance)? {
            result.push(completion);
        }
    }
    Ok(result)
}

// Targeted immutable producer proof: deliberately no head/rollover discovery.
// Corrective edges depend on this leaf, never the other way around.
fn completion_for_session(
    connection: &Connection,
    id: Uuid,
    tenant: &TenantId,
    project: &ProjectId,
    allowance: &SubscriptionCallAllowanceV1,
) -> Result<Option<Completion>, WorkflowError> {
    let root = journal_entry(connection, id, 1)?.ok_or_else(corrupt_store)?;
    require_journal_step(connection, &root, None)?;
    let authority = &root.0.entry.session.grant.authority;
    if authority.tenant_id != *tenant || authority.project_id != *project
        || authority.work_item_id != allowance.grant.work_item_id
        || authority.agent_id != allowance.grant.agent_id
    { return Ok(None); }
    recovery_lineage::validate_recovery_lineage(connection, &root.0.entry)?;
    let (head, _) = load(connection, id)?.ok_or_else(corrupt_store)?;
    let crate::AdaptiveCursorV1::CompletionProposed { artifact_digest } = &head.cursor else { return Ok(None); };
    let operations = validated_journal_operations(connection, &head)?;
    let versions: BTreeSet<_> = operations.iter().map(|(record, _)| record.session_version).collect();
    if (2..=head.version).any(|version| !versions.contains(&version)) { return Err(corrupt_store()); }
    let terminal = journal_entry(connection, id, head.version)?.ok_or_else(corrupt_store)?;
    let Some(AdaptiveTransitionV1::ResolveModel { effect, result_digest, decision: AdaptiveModelDecisionV1::ProposeCompletion { artifact_digest: proposed } }) = &terminal.0.entry.command else { return Err(corrupt_store()); };
    if proposed != artifact_digest || head.last_model_result_digest.as_ref() != Some(result_digest) { return Err(corrupt_store()); }
    let claims: Vec<_> = operations.iter().filter(|(_, command)| matches!(command,
        AdaptiveTransitionV1::ClaimModel { effect: claimed, .. } if claimed == effect)).collect();
    if claims.len() != 1 { return Err(corrupt_store()); }
    let claim_version = claims[0].0.session_version;
    let provider_source = journal_entry(connection, id, claim_version - 1)?.ok_or_else(corrupt_store)?.0.entry.session.clone();
    if !matches!(provider_source.cursor, crate::AdaptiveCursorV1::ReadyForModel)
        || provider_source.requires_fresh_observation()
    { return Err(corrupt_store()); }
    if !allowance_matches(&provider_source, allowance)? { return Ok(None); }
    let observation = provider_source.last_observation.as_ref().ok_or_else(corrupt_store)?;
    let observations: Vec<_> = operations.iter().filter(|(_, command)| matches!(command,
        AdaptiveTransitionV1::ObserveTool { observation: observed } if observed == observation)).collect();
    if observations.len() != 1 || observations[0].0.session_version >= claim_version { return Err(corrupt_store()); }
    let before_observation = journal_entry(connection, id, observations[0].0.session_version - 1)?.ok_or_else(corrupt_store)?;
    let (crate::AdaptiveCursorV1::ToolPending { effect: package_effect, tool, .. }
        | crate::AdaptiveCursorV1::ToolUnknown { effect: package_effect, tool, .. }) = &before_observation.0.entry.session.cursor else { return Err(corrupt_store()); };
    let sentinel_common::WorkbenchTool::PackageArtifact { artifact_kind, media_type, paths } = tool else { return Err(corrupt_store()); };
    if *package_effect != observation.effect { return Err(corrupt_store()); }
    Ok(Some(Completion {
        source: AdaptiveSourceCompletionV1 { provider_source, effect: effect.clone(), model_response_digest: result_digest.clone(), artifact_digest: artifact_digest.clone() },
        package: crate::ExecutionToolV1::PackageArtifact { artifact_kind: artifact_kind.clone(), media_type: media_type.clone(), paths: paths.clone() },
        resolved_at_ms: head.updated_at_ms,
    }))
}
fn completed_execution(connection: &Connection, completion: &Completion) -> Result<Option<WorkItemExecutionV1>, WorkflowError> {
    let authority = &completion.source.provider_source.grant.authority;
    let plan_id = completion_plan_id(&completion.source);
    let Some(work) = super::super::revisions::read_plan_version(connection, &authority.tenant_id, &authority.project_id, &authority.work_item_id, plan_id)? else {
        let admission = format!("v1:{}:{}:{}", authority.tenant_id.0, authority.principal.principal_id, authority.principal.principal_generation);
        let subject = canonical_sha256("sentinel.workflow.execution-subject.v1", &(&authority.tenant_id, &authority.project_id, &authority.work_item_id))?;
        // These keys can only reject orphaned storage, never establish a producer.
        let retained: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM workflow_gate_outbox WHERE plan_id=?1)
             OR EXISTS(SELECT 1 FROM workflow_operations WHERE operation_id=?1
                AND operation_namespace IN (?2,?3))",
            params![plan_id.to_string(), admission, format!("revision-admission:{subject}")], |row| row.get(0),
        ).map_err(map_sqlite_error)?;
        return if retained { Err(corrupt_store()) } else { Ok(None) };
    };
    if work.state != WorkItemState::Done { return Ok(None); }
    let [step] = work.plan.steps.as_slice() else { return Err(corrupt_store()); };
    let evidence = work.terminal_execution_evidence.as_ref().ok_or_else(corrupt_store)?;
    let [artifact] = evidence.artifacts.as_slice() else { return Err(corrupt_store()); };
    if !work.plan.authority_matches(authority) || step.tool != completion.package
        || artifact.digest != completion.source.artifact_digest
        || work.plan.created_at_unix_ms < completion.source.provider_source.updated_at_ms
        || work.plan.created_at_unix_ms > completion.resolved_at_ms
    { return Err(corrupt_store()); }
    let completion_id = completion_request_identity(plan_id, step.step_id, step.invocation_id, &work.plan.request_digest)?;
    let row = read_completion_row(connection, &completion_id)?.ok_or_else(corrupt_store)?;
    let request: PendingCompletionEvidenceV1 = decode(&row.request)?;
    let authority_digest = authority.canonical_digest()?;
    if request.authority_snapshot_digest != authority_digest
        || stored_u64(row.updated_at_ms)? < evidence.completed_at_unix_ms
        || stored_u64(row.updated_at_ms)? > work.updated_at_unix_ms
        || !validate_completion_request(connection, &work, &request)?
    { return Err(corrupt_store()); }
    let gate_id = gate_request_identity(plan_id, &work.plan.request_digest, &evidence.receipt_id)?;
    let row = read_gate_row(connection, &gate_id)?.ok_or_else(corrupt_store)?;
    let request: PendingGateEvidenceV1 = decode(&row.request)?;
    if request.authority_snapshot_digest != authority_digest
        || request.created_at_unix_ms < evidence.completed_at_unix_ms
        || stored_u64(row.updated_at_ms)? != work.updated_at_unix_ms
        || work.gate_evidence.as_ref().is_none_or(|gate| gate.completed_at_unix_ms > work.updated_at_unix_ms)
        || !validate_gate_request(connection, &work, &request)?
    { return Err(corrupt_store()); }
    Ok(Some(work))
}

impl WorkflowStore {
    /// Historical description only. No current Project is loaded, and no grant,
    /// clock or provider effect is created by discovering a retained producer.
    pub fn adaptive_source_completion(
        &self,
        tenant: &TenantId,
        project: &ProjectId,
        source_allowance: &SubscriptionCallAllowanceV1,
        expected_plan_id: Uuid,
    ) -> Result<Option<AdaptiveSourceCompletionV1>, WorkflowError> {
        tenant.validate()?;
        project.validate()?;
        validate_allowance_input(source_allowance)?;
        if expected_plan_id.is_nil() { return Err(authority_conflict()); }
        let connection = self.lock()?;
        crate::domain_store::validation_scope::with_scope(&connection, || {
            let mut found = None;
            for completion in completions(&connection, tenant, project, source_allowance)? {
                if completion_plan_id(&completion.source) != expected_plan_id { continue; }
                if completed_execution(&connection, &completion)?.is_some() {
                    if found.is_some() { return Err(corrupt_store()); }
                    found = Some(completion.source);
                }
            }
            Ok(found)
        })
    }
}

/// Same-transaction authority check for an undispatched adaptive producer.
/// The supplied source may be an archived work item, not today's project head.
pub(crate) fn require_source_review_producer(
    connection: &Connection,
    project: &ProjectV1,
    source: &CompanyWorkItemV1,
    previous: &SubscriptionCallAllowanceV1,
    cutoff_ms: u64,
) -> Result<(), WorkflowError> {
    crate::domain_store::validation_scope::with_scope(connection, || {
        validate_allowance_input(previous)?;
        let mut found = false;
        for completion in completions(connection, &project.tenant_id, &project.project_id, previous)? {
            if source.output_receipts.first().is_none_or(|receipt| receipt.content_digest != completion.source.artifact_digest) { continue; }
            require_source_binding(connection, source, previous, cutoff_ms, &completion)?;
            if found { return Err(corrupt_store()); }
            found = true;
        }
        if found { Ok(()) } else { Err(authority_conflict()) }
    })
}

fn require_source_binding(
    connection: &Connection,
    source: &CompanyWorkItemV1,
    previous: &SubscriptionCallAllowanceV1,
    cutoff_ms: u64,
    completion: &Completion,
) -> Result<WorkItemExecutionV1, WorkflowError> {
    validate_allowance_input(previous)?;
    if previous.dispatch.is_some() || source.state != CompanyWorkStateV1::Done
        || previous.grant.work_item_id != source.spec.work_item_id
        || cutoff_ms == 0
    { return Err(authority_conflict()); }
    let [output] = source.spec.outputs.as_slice() else { return Err(authority_conflict()); };
    let [receipt] = source.output_receipts.as_slice() else { return Err(authority_conflict()); };
    let gate = source.gate_receipt.as_ref().ok_or_else(authority_conflict)?;
    if !matches!(source.spec.required_role, crate::CompanyRoleV1::Developer | crate::CompanyRoleV1::Designer)
        || receipt.name != output.name || receipt.contract_generation != output.contract_generation
        || receipt.contract_digest != output.contract_digest || !gate.passed
        || gate.gate_id != source.spec.quality_gate.gate_id
        || gate.generation != source.spec.quality_gate.generation
        || gate.gate_digest != source.spec.quality_gate.digest
    { return Err(authority_conflict()); }

    if completion.source.artifact_digest != receipt.content_digest { return Err(authority_conflict()); }
    let work = completed_execution(connection, completion)?.ok_or_else(authority_conflict)?;
    let evidence = work.terminal_execution_evidence.as_ref().ok_or_else(corrupt_store)?;
    let observed_gate = work.gate_evidence.as_ref().ok_or_else(corrupt_store)?;
    let [observed] = evidence.outputs.as_slice() else { return Err(corrupt_store()); };
    let [artifact] = evidence.artifacts.as_slice() else { return Err(corrupt_store()); };
    let authority = &completion.source.provider_source.grant.authority;
    let assignments: Vec<_> = source.assignments.iter().filter(|assignment| assignment.assignment_id == previous.grant.assignment_id
        && assignment.assignment_version == authority.assignment_version
        && assignment.agent_id == authority.agent_id).collect();
    if assignments.len() != 1 { return Err(authority_conflict()); }
    let assignment = assignments[0];
    let expected_kind = if source.spec.required_role == crate::CompanyRoleV1::Developer { "source_tree" } else { "design_specification" };
    if assignment.canonical_digest()? != authority.assignment_digest
        || observed.name != output.name || observed.digest != receipt.content_digest
        || observed.kind != expected_kind || artifact.artifact_kind != expected_kind
        || observed.digest_algorithm != output.digest_algorithm || artifact.media_type != output.media_type
        || observed_gate.profile_id != gate.gate_id || observed_gate.profile_generation != gate.generation
        || observed_gate.profile_digest != gate.gate_digest || observed_gate.subject_digest != gate.subject_digest
        || observed_gate.completed_at_unix_ms > cutoff_ms || work.updated_at_unix_ms > cutoff_ms
        || completion.resolved_at_ms > cutoff_ms
    { return Err(authority_conflict()); }
    Ok(work)
}

/// Leaf used by corrective lineage validation. No Project or head discovery.
pub(super) fn correction_source(
    connection: &Connection,
    prior: &AdaptiveSessionV1,
    project: &ProjectV1,
    record: &crate::WorkCorrectionV1,
) -> Result<WorkItemExecutionV1, WorkflowError> {
    let allowance = record.previous_subscription_call.as_ref().ok_or_else(corrupt_store)?;
    let completion = completion_for_session(connection, prior.grant.session_id, &project.tenant_id, &project.project_id, allowance)?
        .ok_or_else(corrupt_store)?;
    let (stored, _) = load(connection, prior.grant.session_id)?.ok_or_else(corrupt_store)?;
    if stored != *prior { return Err(corrupt_store()); }
    let work = require_source_binding(connection, &record.previous, allowance, record.requested_at_unix_ms, &completion)?;
    if crate::ExecutionRevisionV1::from_completed_work(&work, record.execution_revision.feedback_digest.clone())? != record.execution_revision {
        return Err(corrupt_store());
    }
    Ok(work)
}
#[cfg(test)]
pub(crate) mod tests;
