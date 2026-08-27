use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::time::Duration;

use sentinel_common::{
    AgentId, CommandRule, DomainEvent, DomainEventPayload, WorkbenchRequest,
    WorkbenchResourceLimits, WorkbenchTool, WORKBENCH_RUNTIME_BWRAP, WORKBENCH_SCHEMA_VERSION,
};
use sentinel_limbo::EventStore;
use sentinel_workflow::{
    CompanyRoleV1, CompanyWorkStateV1, CompanyWorkflowCommandV1, CompanyWorkflowResponseV1,
    DependencyReadiness, GateEvidencePort, IndependentGateEvidence, PendingGateEvidenceV1,
    ProjectId, ProjectLifecycleStateV1, TenantId, WorkflowPortError, WorkflowStore,
};

use crate::delivery::{
    expected_effect_saga_contract_digest, expected_integration_contract_digest,
    expected_publication_contract_digest, expected_workbench_execution_saga_contract_digest,
    qa_evidence_inventory_digest, AdapterReadiness, AuthorityReceiptV1, AuthorityRole,
    AuthorityValidationRequestV1, CandidateAuthorityQueryV1, CandidateAuthoritySnapshotV1,
    ContentDigest, DataControlV1, DatasetSplit, DeliveryEffectPort, DeliveryEffectReceiptV1,
    DeliveryEffectRequestV1, DeliveryError, DeliveryIntegrationPort, DeliveryPublicationPort,
    PrincipalV1, PublicationReceiptV1, PublicationRequestV1, QaCaseAttemptEvidenceV1,
    QaCaseOutcome, QaCaseReasonCode, QaCaseResultV1, QaDatasetCaseV1,
    QaDeterministicAssertionResultV1, QaEvidenceGraphV1, QaHarnessOutcome, SourceTupleV1,
    VersionedRefV1, WorkbenchEvidenceReceiptV1, WorkbenchEvidenceRequestV1, WorkflowLineageEdgeV1,
    WorkflowLineageKindV1, WorkflowLineageNodeV1, WorkflowLineageQueryV1,
    WorkflowLineageSnapshotV1, WorkflowLineageStateV1, DELIVERY_SCHEMA_V1,
};
use crate::workbench::{
    dispatch_workbench, stage_verified_artifact_inputs, WorkbenchAuthoritySnapshot,
    WorkbenchAuthoritySource, WorkbenchDispatchCommand, WorkbenchInvocationRecord,
    WorkbenchInvocationState, WorkbenchProfile,
};

use super::PrincipalAuthenticator;

const DELIVERY_AUTHORITY_GENERATION: u64 = 1;
const DELIVERY_EVENT_TOPIC: &str = "sentinel.delivery.events";
const WEB_QA_PROGRAM: &str = "sentinel-web-qa";
const WORK_ITEM_GATE_PROGRAM: &str = "sentinel-work-item-gate";

type M0QaEvidenceComponents = (
    Vec<QaDatasetCaseV1>,
    Vec<QaCaseResultV1>,
    Vec<QaDeterministicAssertionResultV1>,
);

#[derive(Clone)]
pub(super) struct WorkflowWorkItemGate {
    integration: WorkflowDeliveryIntegration,
}

impl WorkflowWorkItemGate {
    pub(super) fn new(integration: WorkflowDeliveryIntegration) -> Self {
        Self { integration }
    }
}

fn work_item_gate_command_rule(paths: &[String]) -> Result<CommandRule, WorkflowPortError> {
    let max_args = u16::try_from(paths.len()).map_err(|_| WorkflowPortError::Rejected)?;
    if max_args == 0 || paths.len() > 64 {
        return Err(WorkflowPortError::Rejected);
    }
    Ok(CommandRule {
        program: WORK_ITEM_GATE_PROGRAM.to_string(),
        required_arg_prefix: paths.to_vec(),
        max_args,
    })
}

fn web_qa_command_rule(paths: &[String]) -> Result<CommandRule, DeliveryError> {
    let max_args = u16::try_from(paths.len()).map_err(|_| {
        DeliveryError::Validation("QA input inventory exceeds the command policy".to_string())
    })?;
    if max_args == 0 || paths.len() > 64 {
        return Err(DeliveryError::Validation(
            "QA input inventory is empty or exceeds the command policy".to_string(),
        ));
    }
    Ok(CommandRule {
        program: WEB_QA_PROGRAM.to_string(),
        required_arg_prefix: paths.to_vec(),
        max_args,
    })
}

fn work_item_gate_deadline(
    created_at_unix_ms: u64,
    wall_time_ms: u64,
    plan_deadline_unix_ms: u64,
    now_unix_ms: u64,
) -> Result<u64, WorkflowPortError> {
    let deadline = created_at_unix_ms
        .saturating_add(wall_time_ms)
        .min(plan_deadline_unix_ms);
    if deadline <= now_unix_ms {
        Err(WorkflowPortError::TimedOut)
    } else {
        Ok(deadline)
    }
}

struct WorkItemGateReceipt {
    receipt_id: String,
    profile_id: String,
    profile_generation: u64,
    profile_digest: String,
    subject_digest: String,
    required_checks_digest: String,
    completed_at_ms: u64,
}

impl IndependentGateEvidence for WorkItemGateReceipt {
    fn schema_version(&self) -> u16 {
        sentinel_workflow::WORKFLOW_SCHEMA_VERSION
    }

    fn receipt_id(&self) -> &str {
        &self.receipt_id
    }

    fn profile_id(&self) -> &str {
        &self.profile_id
    }

    fn profile_generation(&self) -> u64 {
        self.profile_generation
    }

    fn profile_digest(&self) -> &str {
        &self.profile_digest
    }

    fn subject_digest(&self) -> &str {
        &self.subject_digest
    }

    fn required_checks_digest(&self) -> &str {
        &self.required_checks_digest
    }

    fn passed(&self) -> bool {
        true
    }

    fn completed_at_unix_ms(&self) -> u64 {
        self.completed_at_ms
    }
}

#[derive(Clone)]
pub(super) struct WorkflowDeliveryIntegration {
    workflow: Arc<WorkflowStore>,
    principals: Arc<PrincipalAuthenticator>,
    qa_profile: WorkbenchProfile,
    qa_profile_digest: String,
    agent_capabilities: Arc<HashMap<AgentId, BTreeSet<String>>>,
    artifact_roots: Arc<HashMap<AgentId, PathBuf>>,
}

impl WorkflowDeliveryIntegration {
    pub(super) fn new(
        workflow: Arc<WorkflowStore>,
        principals: Arc<PrincipalAuthenticator>,
        qa_profile: WorkbenchProfile,
        qa_profile_digest: String,
        agent_capabilities: Arc<HashMap<AgentId, BTreeSet<String>>>,
        artifact_roots: Arc<HashMap<AgentId, PathBuf>>,
    ) -> Self {
        Self {
            workflow,
            principals,
            qa_profile,
            qa_profile_digest,
            agent_capabilities,
            artifact_roots,
        }
    }

    fn qa_snapshot(
        &self,
        tenant_id: &str,
        project_id: &str,
        work_item_id: &str,
        invocation_id: &str,
        principal_id: &str,
        agent_id: AgentId,
    ) -> anyhow::Result<WorkbenchAuthoritySnapshot> {
        if work_item_id != qa_work_item_id(invocation_id)? {
            anyhow::bail!("QA workbench scope is not bound to its invocation");
        }
        let project = self
            .project(tenant_id, project_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let participant = project
            .governance
            .participants
            .iter()
            .find(|value| {
                value.principal_id == principal_id
                    && value.agent_id == agent_id
                    && value.role == CompanyRoleV1::Qa
            })
            .ok_or_else(|| anyhow::anyhow!("QA participant is not current"))?;
        let principal = self
            .principals
            .principal(principal_id)
            .filter(|value| {
                value.principal.tenant_id.0 == tenant_id
                    && value.principal.agent_id == Some(agent_id)
                    && value.principal.role == CompanyRoleV1::Qa
            })
            .ok_or_else(|| anyhow::anyhow!("QA principal is not current"))?;
        if participant.principal_id != principal.principal.principal_id
            || project.lifecycle_state != ProjectLifecycleStateV1::DeliveryCandidate
            || project
                .work_items
                .values()
                .any(|work| work.state != CompanyWorkStateV1::Done)
            || project.governance.project_profile.profile_id != "web-project-v1"
        {
            anyhow::bail!("QA project authority is not current");
        }
        let capabilities = self
            .agent_capabilities
            .get(&agent_id)
            .cloned()
            .unwrap_or_default()
            .intersection(&self.qa_profile.capabilities)
            .cloned()
            .collect::<BTreeSet<_>>();
        if !capabilities.contains("test.run_profile") {
            anyhow::bail!("QA test capability is unavailable");
        }
        Ok(WorkbenchAuthoritySnapshot {
            agent_id,
            caller_id: principal_id.to_string(),
            caller_role: "qa".to_string(),
            project_id: project_id.to_string(),
            work_item_id: work_item_id.to_string(),
            assignment_version: project.version,
            credential_generation: principal.execution_authority.principal_generation,
            policy_digest: project.governance.project_profile.digest,
            tool_profile: self.qa_profile.id.clone(),
            tool_profile_digest: self.qa_profile_digest.clone(),
            runtime_key: WORKBENCH_RUNTIME_BWRAP.to_string(),
            assignment_active: true,
            agent_capabilities: capabilities.clone(),
            role_capabilities: self.qa_profile.capabilities.clone(),
            assignment_capabilities: capabilities.clone(),
            project_capabilities: capabilities.clone(),
            profile_capabilities: self.qa_profile.capabilities.clone(),
        })
    }

    fn exchange(
        &self,
        command: impl FnOnce(
            mpsc::SyncSender<anyhow::Result<crate::workbench::WorkbenchCoordinatorUpdate>>,
        ) -> WorkbenchDispatchCommand,
    ) -> Result<crate::workbench::WorkbenchCoordinatorUpdate, DeliveryError> {
        let (response, receiver) = mpsc::sync_channel(1);
        dispatch_workbench(command(response)).map_err(|error| {
            DeliveryError::AdapterUnavailable {
                dependency: "workbench_qa_execution",
                reason: error.to_string(),
            }
        })?;
        match receiver.recv_timeout(Duration::from_secs(35)) {
            Ok(Ok(update)) => Ok(update),
            Ok(Err(error)) => Err(DeliveryError::Storage(error.to_string())),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(DeliveryError::AdapterUnavailable {
                dependency: "workbench_qa_execution",
                reason: "QA workbench response is pending; retry the stable invocation".to_string(),
            }),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(DeliveryError::AdapterUnavailable {
                dependency: "workbench_qa_execution",
                reason: "QA workbench dispatcher disconnected".to_string(),
            }),
        }
    }

    fn terminal_record(
        &self,
        invocation_id: &str,
        authority: Arc<dyn WorkbenchAuthoritySource>,
        initial: crate::workbench::WorkbenchCoordinatorUpdate,
    ) -> Result<WorkbenchInvocationRecord, DeliveryError> {
        let initial_record = initial.records.last().cloned().ok_or_else(|| {
            DeliveryError::Storage("workbench returned no invocation record".to_string())
        })?;
        if initial_record.state.is_terminal() {
            return Ok(initial_record);
        }
        let polled = self.exchange(|response| WorkbenchDispatchCommand::Poll {
            invocation_id: invocation_id.to_string(),
            authority,
            response,
        })?;
        let record = polled.records.last().cloned().ok_or_else(|| {
            DeliveryError::Storage("workbench poll returned no invocation record".to_string())
        })?;
        if record.state.is_terminal() {
            Ok(record)
        } else {
            Err(DeliveryError::AdapterUnavailable {
                dependency: "workbench_qa_execution",
                reason: "workbench invocation is durable and still executing".to_string(),
            })
        }
    }

    fn project(
        &self,
        tenant_id: &str,
        project_id: &str,
    ) -> Result<sentinel_workflow::ProjectV1, DeliveryError> {
        self.workflow
            .company_project(
                &TenantId::parse(tenant_id).map_err(workflow_error)?,
                &ProjectId::parse(project_id).map_err(workflow_error)?,
            )
            .map_err(workflow_error)?
            .ok_or_else(|| DeliveryError::NotFound(format!("workflow project {project_id}")))
    }

    fn mapped_principal(
        &self,
        tenant_id: &str,
        principal_id: &str,
    ) -> Result<PrincipalV1, DeliveryError> {
        self.mapped_delivery_principal(tenant_id, principal_id)?
            .ok_or_else(|| {
                DeliveryError::AuthorityDenied("principal has no delivery authority".to_string())
            })
    }

    fn mapped_delivery_principal(
        &self,
        tenant_id: &str,
        principal_id: &str,
    ) -> Result<Option<PrincipalV1>, DeliveryError> {
        let bound = self
            .principals
            .principal(principal_id)
            .filter(|bound| bound.principal.tenant_id.0 == tenant_id)
            .ok_or_else(|| {
                DeliveryError::AuthorityDenied("principal is not current".to_string())
            })?;
        let roles = delivery_roles(bound.principal.role);
        if roles.is_empty() {
            return Ok(None);
        }
        Ok(Some(PrincipalV1 {
            tenant_id: tenant_id.to_string(),
            principal_id: bound.principal.principal_id,
            authority_generation: bound.principal.authority_generation,
            roles,
        }))
    }

    fn expected_project_ref(
        project: &sentinel_workflow::ProjectV1,
    ) -> Result<VersionedRefV1, DeliveryError> {
        Ok(VersionedRefV1 {
            id: project.project_id.0.clone(),
            generation: project.version,
            digest: ContentDigest::of_domain("workflow-project", DELIVERY_SCHEMA_V1, project)?,
        })
    }

    fn expected_work_items_digest(
        project: &sentinel_workflow::ProjectV1,
    ) -> Result<ContentDigest, DeliveryError> {
        ContentDigest::of_domain(
            "workflow-work-items",
            DELIVERY_SCHEMA_V1,
            &project.work_items,
        )
    }

    fn participant_inventory(
        &self,
        project: &sentinel_workflow::ProjectV1,
    ) -> Result<Vec<PrincipalV1>, DeliveryError> {
        let tenant_id = project.tenant_id.0.as_str();
        let agreement = self
            .workflow
            .company_agreement(&project.tenant_id, &project.agreement_id)
            .map_err(workflow_error)?
            .ok_or_else(|| DeliveryError::NotFound("workflow agreement".to_string()))?;
        let mut principal_ids = BTreeSet::from([agreement.accepted_by]);
        principal_ids.extend(
            project
                .governance
                .participants
                .iter()
                .map(|participant| participant.principal_id.clone()),
        );
        let mut principals = Vec::new();
        for principal_id in principal_ids {
            if let Some(principal) = self.mapped_delivery_principal(tenant_id, &principal_id)? {
                principals.push(principal);
            }
        }
        principals.sort_by(|left, right| left.principal_id.cmp(&right.principal_id));
        if !principals
            .iter()
            .any(|value| value.has_role(AuthorityRole::Customer))
            || !principals
                .iter()
                .any(|value| value.has_role(AuthorityRole::Developer))
            || !principals
                .iter()
                .any(|value| value.has_role(AuthorityRole::Qa))
            || !principals
                .iter()
                .any(|value| value.has_role(AuthorityRole::ReleaseManager))
        {
            return Err(DeliveryError::AuthorityDenied(
                "workflow project lacks the required delivery separation of duties".to_string(),
            ));
        }
        Ok(principals)
    }

    fn validate_project_query(
        &self,
        query: &CandidateAuthorityQueryV1,
    ) -> Result<sentinel_workflow::ProjectV1, DeliveryError> {
        let project = self.project(&query.tenant_id, &query.project.id)?;
        let agreement = self
            .workflow
            .company_agreement(&project.tenant_id, &project.agreement_id)
            .map_err(workflow_error)?
            .ok_or_else(|| DeliveryError::NotFound("workflow agreement".to_string()))?;
        let expected_agreement = VersionedRefV1 {
            id: agreement.agreement_id,
            generation: 1,
            digest: ContentDigest::parse(agreement.proposal_digest)?,
        };
        if query.project != Self::expected_project_ref(&project)?
            || query.agreement != expected_agreement
            || query.work_items_digest != Self::expected_work_items_digest(&project)?
            || project.lifecycle_state != ProjectLifecycleStateV1::DeliveryCandidate
            || project
                .work_items
                .values()
                .any(|work| work.state != CompanyWorkStateV1::Done)
        {
            return Err(DeliveryError::StaleEvidence(
                "workflow project is not the exact current delivery candidate".to_string(),
            ));
        }
        Ok(project)
    }

    fn push_node<T: serde::Serialize>(
        nodes: &mut Vec<WorkflowLineageNodeV1>,
        kind: WorkflowLineageKindV1,
        state: WorkflowLineageStateV1,
        generation: u64,
        role: Option<AuthorityRole>,
        domain: &str,
        value: &T,
    ) -> Result<u32, DeliveryError> {
        let ordinal = u32::try_from(nodes.len())
            .map_err(|_| DeliveryError::Validation("workflow lineage is too large".to_string()))?;
        nodes.push(WorkflowLineageNodeV1 {
            node_ordinal: ordinal,
            kind,
            state,
            generation,
            digest: ContentDigest::of_domain(domain, DELIVERY_SCHEMA_V1, value)?,
            participant_role: role,
        });
        Ok(ordinal)
    }
}

impl DeliveryIntegrationPort for WorkflowDeliveryIntegration {
    fn readiness(&self) -> AdapterReadiness {
        AdapterReadiness::Ready {
            contract_version: DELIVERY_SCHEMA_V1,
            authority_generation: DELIVERY_AUTHORITY_GENERATION,
            contract_digest: expected_integration_contract_digest(),
        }
    }

    fn execution_saga_readiness(&self) -> AdapterReadiness {
        AdapterReadiness::Ready {
            contract_version: DELIVERY_SCHEMA_V1,
            authority_generation: DELIVERY_AUTHORITY_GENERATION,
            contract_digest: expected_workbench_execution_saga_contract_digest(),
        }
    }

    fn candidate_authority(
        &self,
        query: &CandidateAuthorityQueryV1,
    ) -> Result<CandidateAuthoritySnapshotV1, DeliveryError> {
        let project = self.validate_project_query(query)?;
        CandidateAuthoritySnapshotV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            authority_generation: DELIVERY_AUTHORITY_GENERATION,
            agreement: query.agreement.clone(),
            project: query.project.clone(),
            work_items_digest: query.work_items_digest.clone(),
            current_candidate_generation: project.version.saturating_add(1),
            current_candidate_digest: query.candidate_digest.clone(),
            participant_principals: self.participant_inventory(&project)?,
            snapshot_digest: ContentDigest::zero(),
        }
        .seal()
    }

    fn workflow_lineage(
        &self,
        query: &WorkflowLineageQueryV1,
    ) -> Result<WorkflowLineageSnapshotV1, DeliveryError> {
        if query.schema_version != DELIVERY_SCHEMA_V1
            || query.query_digest != query.computed_digest()?
            || query.authority_generation != DELIVERY_AUTHORITY_GENERATION
        {
            return Err(DeliveryError::StaleEvidence(
                "workflow lineage query is not current".to_string(),
            ));
        }
        let project = self.project(&query.tenant_id, &query.project.id)?;
        let work_items_digest = Self::expected_work_items_digest(&project)?;
        let candidate_query = CandidateAuthorityQueryV1 {
            tenant_id: query.tenant_id.clone(),
            agreement: VersionedRefV1 {
                id: project.agreement_id.clone(),
                generation: 1,
                digest: ContentDigest::parse(project.agreement_digest.clone())?,
            },
            project: query.project.clone(),
            work_items_digest,
            candidate_digest: query.candidate.digest.clone(),
        };
        let authority = self.candidate_authority(&candidate_query)?;
        if query.authority_identity_digest != authority.snapshot_digest
            || query.candidate.generation != project.version.saturating_add(1)
        {
            return Err(DeliveryError::StaleEvidence(
                "workflow lineage authority changed".to_string(),
            ));
        }
        let agreement = self
            .workflow
            .company_agreement(&project.tenant_id, &project.agreement_id)
            .map_err(workflow_error)?
            .ok_or_else(|| DeliveryError::NotFound("workflow agreement".to_string()))?;
        let request = self
            .workflow
            .company_customer_request(&project.tenant_id, &agreement.request_id)
            .map_err(workflow_error)?
            .ok_or_else(|| DeliveryError::NotFound("workflow customer request".to_string()))?;

        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let request_node = Self::push_node(
            &mut nodes,
            WorkflowLineageKindV1::CustomerRequest,
            WorkflowLineageStateV1::Approved,
            request.version,
            Some(AuthorityRole::Customer),
            "workflow-request",
            &request,
        )?;
        let agreement_node = Self::push_node(
            &mut nodes,
            WorkflowLineageKindV1::Agreement,
            WorkflowLineageStateV1::Approved,
            1,
            Some(AuthorityRole::Customer),
            "workflow-agreement",
            &agreement,
        )?;
        edges.push(WorkflowLineageEdgeV1 {
            from_ordinal: request_node,
            to_ordinal: agreement_node,
        });
        let project_node = Self::push_node(
            &mut nodes,
            WorkflowLineageKindV1::Project,
            WorkflowLineageStateV1::Completed,
            project.version,
            None,
            "workflow-project",
            &project,
        )?;
        edges.push(WorkflowLineageEdgeV1 {
            from_ordinal: agreement_node,
            to_ordinal: project_node,
        });
        for work in project.work_items.values() {
            let work_node = Self::push_node(
                &mut nodes,
                WorkflowLineageKindV1::WorkItem,
                WorkflowLineageStateV1::Completed,
                work.version,
                delivery_roles(work.spec.required_role).into_iter().next(),
                "workflow-work-item",
                work,
            )?;
            edges.push(WorkflowLineageEdgeV1 {
                from_ordinal: project_node,
                to_ordinal: work_node,
            });
        }
        for participant in self.participant_inventory(&project)? {
            let participant_node = Self::push_node(
                &mut nodes,
                WorkflowLineageKindV1::Participant,
                WorkflowLineageStateV1::Active,
                participant.authority_generation,
                participant.roles.iter().next().cloned(),
                "workflow-participant",
                &participant,
            )?;
            edges.push(WorkflowLineageEdgeV1 {
                from_ordinal: project_node,
                to_ordinal: participant_node,
            });
        }
        for decision in &project.decisions {
            let node = Self::push_node(
                &mut nodes,
                WorkflowLineageKindV1::Decision,
                WorkflowLineageStateV1::Approved,
                1,
                None,
                "workflow-decision",
                decision,
            )?;
            edges.push(WorkflowLineageEdgeV1 {
                from_ordinal: project_node,
                to_ordinal: node,
            });
        }
        for handoff in &project.handoffs {
            let node = Self::push_node(
                &mut nodes,
                WorkflowLineageKindV1::Handoff,
                WorkflowLineageStateV1::HandedOff,
                u64::try_from(handoff.transition_history.len()).unwrap_or(u64::MAX) + 1,
                None,
                "workflow-handoff",
                handoff,
            )?;
            edges.push(WorkflowLineageEdgeV1 {
                from_ordinal: project_node,
                to_ordinal: node,
            });
        }
        for blocker in &project.blockers {
            let state = match blocker.state {
                sentinel_workflow::BlockerStateV1::Resolved => WorkflowLineageStateV1::Clear,
                sentinel_workflow::BlockerStateV1::Open
                | sentinel_workflow::BlockerStateV1::Escalated => WorkflowLineageStateV1::Blocked,
            };
            let node = Self::push_node(
                &mut nodes,
                WorkflowLineageKindV1::Blocker,
                state,
                u64::try_from(blocker.transition_history.len()).unwrap_or(u64::MAX) + 1,
                None,
                "workflow-blocker",
                blocker,
            )?;
            edges.push(WorkflowLineageEdgeV1 {
                from_ordinal: project_node,
                to_ordinal: node,
            });
        }
        WorkflowLineageSnapshotV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            server_redacted: true,
            tenant_id: query.tenant_id.clone(),
            project: query.project.clone(),
            candidate: query.candidate.clone(),
            authority_generation: query.authority_generation,
            authority_identity_digest: query.authority_identity_digest.clone(),
            query_digest: query.query_digest.clone(),
            snapshot_generation: project.version,
            nodes,
            edges,
            snapshot_digest: ContentDigest::zero(),
        }
        .seal()
    }

    fn authorize(
        &self,
        request: &AuthorityValidationRequestV1,
    ) -> Result<AuthorityReceiptV1, DeliveryError> {
        if request.schema_version != DELIVERY_SCHEMA_V1
            || request.request_digest != request.computed_digest()?
            || request.contract_version != DELIVERY_SCHEMA_V1
            || request.contract_digest != expected_integration_contract_digest()
            || request.validated_at_ms == 0
        {
            return Err(DeliveryError::StaleEvidence(
                "delivery authority request is not current".to_string(),
            ));
        }
        let principal = self.mapped_principal(&request.tenant_id, &request.principal_id)?;
        if principal.authority_generation != request.claimed_authority_generation
            || !principal.has_role(request.required_role.clone())
        {
            return Err(DeliveryError::AuthorityDenied(
                "delivery role or generation is stale".to_string(),
            ));
        }
        let issued_at_ms = request.validated_at_ms;
        AuthorityReceiptV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            request_digest: request.request_digest.clone(),
            principal,
            contract_version: DELIVERY_SCHEMA_V1,
            contract_authority_generation: DELIVERY_AUTHORITY_GENERATION,
            contract_digest: expected_integration_contract_digest(),
            issued_at_ms,
            expires_at_ms: issued_at_ms.saturating_add(60_000),
            issuer: "sentinel-company-workflow".to_string(),
            receipt_digest: ContentDigest::zero(),
        }
        .seal()
    }

    fn execute_qa(
        &self,
        request: &WorkbenchEvidenceRequestV1,
    ) -> Result<WorkbenchEvidenceReceiptV1, DeliveryError> {
        if request.schema_version != DELIVERY_SCHEMA_V1
            || request.request_digest != request.computed_digest()?
            || request.candidate_artifacts.is_empty()
        {
            return Err(DeliveryError::StaleEvidence(
                "QA workbench request is not sealed".to_string(),
            ));
        }
        let project = self.project(&request.tenant_id, &request.project.id)?;
        if request.project != Self::expected_project_ref(&project)?
            || project.lifecycle_state != ProjectLifecycleStateV1::DeliveryCandidate
        {
            return Err(DeliveryError::StaleEvidence(
                "QA workbench project is not current".to_string(),
            ));
        }
        let qa = self
            .principals
            .principal(&request.assigned_qa.principal_id)
            .filter(|value| {
                value.principal.tenant_id.0 == request.tenant_id
                    && value.principal.role == CompanyRoleV1::Qa
                    && value.principal.authority_generation
                        == request.assigned_qa.authority_generation
            })
            .ok_or_else(|| {
                DeliveryError::AuthorityDenied("assigned QA is not current".to_string())
            })?;
        let qa_agent = qa.principal.agent_id.ok_or_else(|| {
            DeliveryError::AuthorityDenied("assigned QA has no runtime agent".to_string())
        })?;
        let work_item_id = qa_work_item_id(&request.invocation.id).map_err(storage_error)?;
        let mut inputs = Vec::new();
        for artifact in &request.candidate_artifacts {
            let owner = self
                .principals
                .principal(&artifact.owner_principal_id)
                .filter(|value| {
                    value.principal.tenant_id.0 == request.tenant_id
                        && matches!(
                            value.principal.role,
                            CompanyRoleV1::Designer | CompanyRoleV1::Developer
                        )
                })
                .and_then(|value| value.principal.agent_id)
                .ok_or_else(|| {
                    DeliveryError::AuthorityDenied(
                        "candidate artifact owner is not a current implementer".to_string(),
                    )
                })?;
            inputs.extend(
                stage_verified_artifact_inputs(
                    &self.artifact_roots,
                    owner,
                    qa_agent,
                    &request.project.id,
                    &work_item_id,
                    artifact.digest.as_str(),
                    None,
                    &artifact.media_type,
                )
                .map_err(storage_error)?,
            );
        }
        inputs.sort_by(|left, right| left.mount_path.cmp(&right.mount_path));
        if inputs.is_empty()
            || inputs.len() > 64
            || inputs
                .windows(2)
                .any(|pair| pair[0].mount_path == pair[1].mount_path)
        {
            return Err(DeliveryError::Validation(
                "candidate inputs are empty, ambiguous, or exceed the QA profile".to_string(),
            ));
        }
        let authority_snapshot = self
            .qa_snapshot(
                &request.tenant_id,
                &request.project.id,
                &work_item_id,
                &request.invocation.id,
                &request.assigned_qa.principal_id,
                qa_agent,
            )
            .map_err(storage_error)?;
        let deadline_unix_ms = request
            .started_at_ms
            .checked_add(self.qa_profile.resource_ceilings.wall_time_ms)
            .ok_or_else(|| {
                DeliveryError::Validation("QA workbench deadline overflow".to_string())
            })?;
        let input_paths = inputs
            .iter()
            .map(|input| input.mount_path.clone())
            .collect::<Vec<_>>();
        let command_policy = vec![web_qa_command_rule(&input_paths)?];
        let mut workbench_request = WorkbenchRequest {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: request.invocation.id.clone(),
            agent_id: qa_agent,
            project_id: request.project.id.clone(),
            work_item_id: work_item_id.clone(),
            workspace_id: format!("{}:{work_item_id}", request.project.id),
            caller_id: request.assigned_qa.principal_id.clone(),
            caller_role: "qa".to_string(),
            assignment_version: authority_snapshot.assignment_version,
            credential_generation: authority_snapshot.credential_generation,
            policy_digest: authority_snapshot.policy_digest.clone(),
            tool_profile: self.qa_profile.id.clone(),
            tool_profile_digest: self.qa_profile_digest.clone(),
            runtime_key: WORKBENCH_RUNTIME_BWRAP.to_string(),
            capabilities: BTreeSet::from(["test.run_profile".to_string()]),
            output_artifact_kinds: BTreeSet::new(),
            inputs,
            command_policy,
            resource_limits: WorkbenchResourceLimits {
                wall_time_ms: self.qa_profile.resource_ceilings.wall_time_ms,
                cpu_time_ms: self.qa_profile.resource_ceilings.cpu_time_ms,
                memory_bytes: self.qa_profile.resource_ceilings.memory_bytes,
                process_count: self.qa_profile.resource_ceilings.process_count,
                file_bytes: self.qa_profile.resource_ceilings.file_bytes,
                stdout_bytes: self.qa_profile.resource_ceilings.stdout_bytes,
                stderr_bytes: self.qa_profile.resource_ceilings.stderr_bytes,
            },
            deadline_unix_ms,
            attempt: 1,
            tool: WorkbenchTool::RunTests {
                suite_id: "web-qa-v1".to_string(),
                program: WEB_QA_PROGRAM.to_string(),
                args: input_paths,
            },
            input_digest: String::new(),
        };
        workbench_request.input_digest = workbench_request
            .canonical_digest()
            .map_err(storage_error)?;
        // Existing reservations remain replayable after their deadline. The
        // store performs time admission only when it creates the reservation.
        workbench_request
            .validate_for_replay()
            .map_err(storage_error)?;

        let authority: Arc<dyn WorkbenchAuthoritySource> = Arc::new(self.clone());
        let update = self.exchange(|response| WorkbenchDispatchCommand::Submit {
            request: Box::new(workbench_request.clone()),
            authority: Arc::clone(&authority),
            response,
        })?;
        let record = self.terminal_record(&request.invocation.id, authority, update)?;
        qa_receipt(request, &workbench_request, &record)
    }
}

impl WorkbenchAuthoritySource for WorkflowDeliveryIntegration {
    fn current_for_request(
        &self,
        request: &WorkbenchRequest,
    ) -> anyhow::Result<WorkbenchAuthoritySnapshot> {
        let principal = self
            .principals
            .principal(&request.caller_id)
            .ok_or_else(|| anyhow::anyhow!("QA principal is unavailable"))?;
        self.qa_snapshot(
            &principal.principal.tenant_id.0,
            &request.project_id,
            &request.work_item_id,
            &request.invocation_id,
            &request.caller_id,
            request.agent_id,
        )
    }

    fn current_for_record(
        &self,
        record: &WorkbenchInvocationRecord,
    ) -> anyhow::Result<WorkbenchAuthoritySnapshot> {
        let principal = self
            .principals
            .principal(&record.caller_id)
            .ok_or_else(|| anyhow::anyhow!("QA principal is unavailable"))?;
        self.qa_snapshot(
            &principal.principal.tenant_id.0,
            &record.project_id,
            &record.work_item_id,
            &record.invocation_id,
            &record.caller_id,
            record.agent_id,
        )
    }
}

fn qa_work_item_id(invocation_id: &str) -> anyhow::Result<String> {
    let compact = invocation_id.replace('-', "");
    if compact.len() != 32 || !compact.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!("QA invocation is not a canonical UUID");
    }
    Ok(format!("qa-{}", &compact[..24]))
}

fn m0_qa_digest(label: &str) -> Result<ContentDigest, DeliveryError> {
    ContentDigest::of_domain("m0-web-qa", DELIVERY_SCHEMA_V1, &label)
}

fn m0_qa_data_control() -> Result<DataControlV1, DeliveryError> {
    Ok(DataControlV1 {
        classification: "internal".to_string(),
        encryption_key_owner: "sentinel".to_string(),
        access_policy_digest: m0_qa_digest("fixture-access-policy")?,
        redaction_policy_digest: m0_qa_digest("fixture-redaction-policy")?,
        retention_frontier: VersionedRefV1 {
            id: "m0-web-qa-retention".to_string(),
            generation: 1,
            digest: m0_qa_digest("fixture-retention-frontier")?,
        },
        audit_policy_digest: m0_qa_digest("fixture-audit-policy")?,
    })
}

pub(super) fn m0_qa_fixture_cases() -> Result<Vec<QaDatasetCaseV1>, DeliveryError> {
    let source = SourceTupleV1 {
        owner: "project-sentinel".to_string(),
        source_type: "repository_fixture".to_string(),
        id: "web-qa-v1".to_string(),
        generation: 1,
        digest: m0_qa_digest("web-qa-v1-source")?,
    };
    [
        ("web-security", true, "security"),
        ("web-structure", true, "structure"),
        ("web-visual", false, "visual"),
    ]
    .into_iter()
    .map(|(case_id, required, surface)| {
        Ok(QaDatasetCaseV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            case_id: case_id.to_string(),
            generation: 1,
            split: DatasetSplit::HiddenHoldout,
            required,
            required_class: if required {
                "deterministic".to_string()
            } else {
                "optional".to_string()
            },
            slices: BTreeMap::from([("surface".to_string(), surface.to_string())]),
            input_digest: m0_qa_digest(&format!("{case_id}-input"))?,
            oracle_digest: m0_qa_digest(&format!("{case_id}-oracle"))?,
            provenance: vec![source.clone()],
            license: "project-sentinel-internal".to_string(),
            access_policy_digest: m0_qa_digest("fixture-access-policy")?,
            contamination_policy_digest: m0_qa_digest("fixture-contamination-policy")?,
            retired_at_ms: None,
            superseded_by: None,
            data_control: m0_qa_data_control()?,
        })
    })
    .collect()
}

fn m0_qa_evidence_components(
    run: &VersionedRefV1,
    plan_digest: &ContentDigest,
    harness_outcome: QaHarnessOutcome,
    output_digest: &ContentDigest,
    logs_digest: &ContentDigest,
) -> Result<M0QaEvidenceComponents, DeliveryError> {
    let cases = m0_qa_fixture_cases()?;
    let outcome = match harness_outcome {
        QaHarnessOutcome::Pass => QaCaseOutcome::Pass,
        QaHarnessOutcome::Fail => QaCaseOutcome::Fail,
        QaHarnessOutcome::Error => QaCaseOutcome::Error,
    };
    let reason_code = match outcome {
        QaCaseOutcome::Pass => QaCaseReasonCode::Verified,
        QaCaseOutcome::Fail => QaCaseReasonCode::AssertionFailed,
        QaCaseOutcome::Error => QaCaseReasonCode::HarnessError,
        _ => {
            return Err(DeliveryError::Validation(
                "M0 QA produced an unsupported outcome".to_string(),
            ))
        }
    };
    let deterministic_results = cases
        .iter()
        .filter(|case| case.required)
        .map(|case| {
            let case_digest =
                ContentDigest::of_domain("qa-dataset-case", DELIVERY_SCHEMA_V1, case)?;
            Ok(QaDeterministicAssertionResultV1 {
                schema_version: DELIVERY_SCHEMA_V1,
                assertion_id: format!("assertion-{}", case.case_id),
                generation: 1,
                plan_digest: plan_digest.clone(),
                case_digest,
                assertion_digest: ContentDigest::of_domain(
                    "m0-web-qa-assertion",
                    DELIVERY_SCHEMA_V1,
                    &(&case.case_id, output_digest, logs_digest),
                )?,
                oracle_digest: case.oracle_digest.clone(),
                input_digest: case.input_digest.clone(),
                evidence_digest: ContentDigest::of_domain(
                    "m0-web-qa-evidence",
                    DELIVERY_SCHEMA_V1,
                    &(&case.case_id, output_digest, logs_digest),
                )?,
                actual_digest: output_digest.clone(),
                passed: outcome == QaCaseOutcome::Pass,
            })
        })
        .collect::<Result<Vec<_>, DeliveryError>>()?;
    let case_results = cases
        .iter()
        .filter(|case| case.required)
        .zip(&deterministic_results)
        .map(|(case, assertion)| {
            let case_ref = VersionedRefV1 {
                id: case.case_id.clone(),
                generation: case.generation,
                digest: ContentDigest::of_domain("qa-dataset-case", DELIVERY_SCHEMA_V1, case)?,
            };
            let assertion_ref = VersionedRefV1 {
                id: assertion.assertion_id.clone(),
                generation: assertion.generation,
                digest: ContentDigest::of_domain(
                    "qa-deterministic-result",
                    DELIVERY_SCHEMA_V1,
                    assertion,
                )?,
            };
            let attempt = QaCaseAttemptEvidenceV1 {
                schema_version: DELIVERY_SCHEMA_V1,
                attempt_id: format!("attempt-{}-1", case.case_id),
                generation: 1,
                attempt_number: 1,
                run: run.clone(),
                case_ref: case_ref.clone(),
                outcome,
                reason_code,
                assertion_refs: vec![assertion_ref.clone()],
                attempt_digest: ContentDigest::zero(),
            }
            .seal()?;
            Ok(QaCaseResultV1 {
                schema_version: DELIVERY_SCHEMA_V1,
                result_id: format!("result-{}", case.case_id),
                generation: 1,
                run: run.clone(),
                case_ref,
                outcome,
                required: true,
                reason_code,
                sources: case.provenance.clone(),
                assertion_refs: vec![assertion_ref],
                grader_refs: vec![],
                slices: case.slices.clone(),
                attempts: 1,
                attempt_history: vec![attempt],
                disposition: None,
            })
        })
        .collect::<Result<Vec<_>, DeliveryError>>()?;
    Ok((cases, case_results, deterministic_results))
}

pub(super) fn m0_qa_evidence_graph(
    run: &VersionedRefV1,
    plan_digest: &ContentDigest,
    receipt: &WorkbenchEvidenceReceiptV1,
) -> Result<QaEvidenceGraphV1, DeliveryError> {
    let (dataset_cases, case_results, deterministic_results) = m0_qa_evidence_components(
        run,
        plan_digest,
        receipt.harness_outcome,
        &receipt.output_digest,
        &receipt.logs_digest,
    )?;
    QaEvidenceGraphV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        run: run.clone(),
        workbench_receipt: VersionedRefV1 {
            id: receipt.invocation.id.clone(),
            generation: receipt.invocation.generation,
            digest: receipt.receipt_digest.clone(),
        },
        dataset_cases,
        case_results,
        deterministic_results,
        model_results: vec![],
        flake_dispositions: vec![],
        graph_digest: ContentDigest::zero(),
    }
    .seal()
}

fn qa_receipt(
    request: &WorkbenchEvidenceRequestV1,
    workbench_request: &WorkbenchRequest,
    record: &WorkbenchInvocationRecord,
) -> Result<WorkbenchEvidenceReceiptV1, DeliveryError> {
    if record.invocation_id != workbench_request.invocation_id
        || record.request_digest != workbench_request.input_digest
        || record.agent_id != workbench_request.agent_id
        || record.project_id != workbench_request.project_id
        || record.work_item_id != workbench_request.work_item_id
        || record.caller_id != workbench_request.caller_id
        || record.tool_profile != workbench_request.tool_profile
        || record.tool_profile_digest != workbench_request.tool_profile_digest
        || !record.state.is_terminal()
    {
        return Err(DeliveryError::StaleEvidence(
            "QA terminal record is bound to another invocation".to_string(),
        ));
    }
    let output_digest = match record.result_digest.as_deref() {
        Some(value) => ContentDigest::parse(value.to_string())?,
        None => ContentDigest::of_domain(
            "qa-terminal-output",
            DELIVERY_SCHEMA_V1,
            &(&record.state, &record.error),
        )?,
    };
    let harness_outcome = match record.state {
        WorkbenchInvocationState::Succeeded => crate::delivery::QaHarnessOutcome::Pass,
        WorkbenchInvocationState::Failed => crate::delivery::QaHarnessOutcome::Fail,
        WorkbenchInvocationState::Cancelled
        | WorkbenchInvocationState::TimedOut
        | WorkbenchInvocationState::UnknownOutcome => crate::delivery::QaHarnessOutcome::Error,
        WorkbenchInvocationState::Reserved | WorkbenchInvocationState::Executing => {
            return Err(DeliveryError::Storage(
                "QA invocation is not terminal".to_string(),
            ))
        }
    };
    let cleanup_receipt = VersionedRefV1 {
        id: format!("cleanup-{}", record.invocation_id),
        generation: 1,
        digest: ContentDigest::of_domain(
            "workbench-cleanup",
            DELIVERY_SCHEMA_V1,
            &(
                &record.invocation_id,
                record.agent_id,
                &record.state,
                &record.resources,
            ),
        )?,
    };
    let logs_digest = ContentDigest::of_domain(
        "qa-safe-log-summary",
        DELIVERY_SCHEMA_V1,
        &(&record.state, &record.error, &record.resources),
    )?;
    let (dataset_cases, case_results, deterministic_results) = m0_qa_evidence_components(
        &request.qa_run,
        &request.qa_plan.digest,
        harness_outcome,
        &output_digest,
        &logs_digest,
    )?;
    let result_inventory_digest = qa_evidence_inventory_digest(&QaEvidenceGraphV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        run: request.qa_run.clone(),
        workbench_receipt: VersionedRefV1 {
            id: request.invocation.id.clone(),
            generation: request.invocation.generation,
            digest: ContentDigest::zero(),
        },
        dataset_cases,
        case_results,
        deterministic_results,
        model_results: vec![],
        flake_dispositions: vec![],
        graph_digest: ContentDigest::zero(),
    })?;
    WorkbenchEvidenceReceiptV1 {
        schema_version: DELIVERY_SCHEMA_V1,
        invocation: request.invocation.clone(),
        assignment: request.qa_run.clone(),
        qa_run: request.qa_run.clone(),
        assigned_qa: request.assigned_qa.clone(),
        authority_receipt_digest: request.authority_receipt_digest.clone(),
        authority_identity_digest: request.authority_identity_digest.clone(),
        input_digest: request.request_digest.clone(),
        output_digest,
        artifact_ownership_digest: ContentDigest::of_domain(
            "qa-artifact-ownership",
            DELIVERY_SCHEMA_V1,
            &(
                &request.candidate,
                &request.candidate_artifacts,
                &workbench_request.inputs,
                &request.assigned_qa,
            ),
        )?,
        result_inventory_digest,
        logs_digest,
        screenshots_digest: None,
        failure_classification_digest: ContentDigest::of_domain(
            "qa-failure-classification",
            DELIVERY_SCHEMA_V1,
            &(&record.state, &record.error),
        )?,
        harness_outcome,
        required_cases_complete: matches!(
            record.state,
            WorkbenchInvocationState::Succeeded | WorkbenchInvocationState::Failed
        ),
        contaminated: false,
        needs_human_review: false,
        flaky_unresolved: false,
        cleanup_receipt,
        receipt_digest: ContentDigest::zero(),
    }
    .seal()
}

#[derive(Clone)]
struct GateWorkbenchAuthority {
    gate: WorkflowWorkItemGate,
    request: PendingGateEvidenceV1,
    qa_principal_id: String,
    qa_agent: AgentId,
}

impl GateWorkbenchAuthority {
    fn snapshot(
        &self,
        invocation_id: &str,
        project_id: &str,
        work_item_id: &str,
        caller_id: &str,
        agent_id: AgentId,
    ) -> anyhow::Result<WorkbenchAuthoritySnapshot> {
        let (work, completed) = self.gate.integration.workflow.gate_context(&self.request)?;
        let expected_invocation = uuid_from_digest(&self.request.request_digest)?;
        let expected_work_item = format!("gate-{}", &self.request.request_digest[..24]);
        if completed
            || work.state != sentinel_workflow::WorkItemState::InReview
            || invocation_id != expected_invocation
            || project_id != work.project_id.0
            || work_item_id != expected_work_item
            || caller_id != self.qa_principal_id
            || agent_id != self.qa_agent
        {
            anyhow::bail!("work-item gate authority is stale");
        }
        let project = self
            .gate
            .integration
            .workflow
            .company_project(&work.tenant_id, &work.project_id)?
            .ok_or_else(|| anyhow::anyhow!("gate project is unavailable"))?;
        let participant = project.governance.participants.iter().find(|participant| {
            participant.principal_id == self.qa_principal_id
                && participant.agent_id == self.qa_agent
                && participant.role == CompanyRoleV1::Qa
        });
        let principal = self
            .gate
            .integration
            .principals
            .principal(&self.qa_principal_id)
            .filter(|value| {
                value.principal.tenant_id == work.tenant_id
                    && value.principal.agent_id == Some(self.qa_agent)
                    && value.principal.role == CompanyRoleV1::Qa
            });
        if participant.is_none()
            || principal.is_none()
            || !matches!(
                project.lifecycle_state,
                ProjectLifecycleStateV1::Active | ProjectLifecycleStateV1::DeliveryCandidate
            )
        {
            anyhow::bail!("work-item gate QA authority is unavailable");
        }
        let principal = principal.expect("principal was checked above");
        let capabilities = self
            .gate
            .integration
            .agent_capabilities
            .get(&self.qa_agent)
            .cloned()
            .unwrap_or_default()
            .intersection(&self.gate.integration.qa_profile.capabilities)
            .cloned()
            .collect::<BTreeSet<_>>();
        if !capabilities.contains("test.run_profile") {
            anyhow::bail!("work-item gate capability is unavailable");
        }
        Ok(WorkbenchAuthoritySnapshot {
            agent_id: self.qa_agent,
            caller_id: self.qa_principal_id.clone(),
            caller_role: "qa".to_string(),
            project_id: work.project_id.0,
            work_item_id: expected_work_item,
            assignment_version: work.version,
            credential_generation: principal.execution_authority.principal_generation,
            policy_digest: project.governance.project_profile.digest,
            tool_profile: self.gate.integration.qa_profile.id.clone(),
            tool_profile_digest: self.gate.integration.qa_profile_digest.clone(),
            runtime_key: WORKBENCH_RUNTIME_BWRAP.to_string(),
            assignment_active: true,
            agent_capabilities: capabilities.clone(),
            role_capabilities: self.gate.integration.qa_profile.capabilities.clone(),
            assignment_capabilities: capabilities.clone(),
            project_capabilities: capabilities,
            profile_capabilities: self.gate.integration.qa_profile.capabilities.clone(),
        })
    }
}

impl WorkbenchAuthoritySource for GateWorkbenchAuthority {
    fn current_for_request(
        &self,
        request: &WorkbenchRequest,
    ) -> anyhow::Result<WorkbenchAuthoritySnapshot> {
        self.snapshot(
            &request.invocation_id,
            &request.project_id,
            &request.work_item_id,
            &request.caller_id,
            request.agent_id,
        )
    }

    fn current_for_record(
        &self,
        record: &WorkbenchInvocationRecord,
    ) -> anyhow::Result<WorkbenchAuthoritySnapshot> {
        self.snapshot(
            &record.invocation_id,
            &record.project_id,
            &record.work_item_id,
            &record.caller_id,
            record.agent_id,
        )
    }
}

impl GateEvidencePort for WorkflowWorkItemGate {
    fn readiness(&self) -> DependencyReadiness {
        DependencyReadiness::Ready
    }

    fn gate_evidence(
        &self,
        request: &PendingGateEvidenceV1,
    ) -> Result<Box<dyn IndependentGateEvidence>, WorkflowPortError> {
        let (work, completed) = self
            .integration
            .workflow
            .gate_context(request)
            .map_err(|_| WorkflowPortError::AuthorityConflict)?;
        if completed || work.state != sentinel_workflow::WorkItemState::InReview {
            return Err(WorkflowPortError::AuthorityConflict);
        }
        let evidence = work
            .terminal_execution_evidence
            .as_ref()
            .filter(|value| {
                value.receipt_id == request.execution_receipt_id && !value.artifacts.is_empty()
            })
            .ok_or(WorkflowPortError::AuthorityConflict)?;
        let project = self
            .integration
            .workflow
            .company_project(&work.tenant_id, &work.project_id)
            .map_err(|_| WorkflowPortError::AuthorityConflict)?
            .ok_or(WorkflowPortError::AuthorityConflict)?;
        let qa_bindings = project
            .governance
            .participants
            .iter()
            .filter(|participant| participant.role == CompanyRoleV1::Qa)
            .map(|participant| {
                self.integration
                    .principals
                    .principal(&participant.principal_id)
                    .filter(|value| {
                        value.principal.tenant_id == work.tenant_id
                            && value.principal.agent_id == Some(participant.agent_id)
                            && value.principal.role == CompanyRoleV1::Qa
                    })
                    .map(|value| (participant, value))
                    .ok_or(WorkflowPortError::AuthorityConflict)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let [(qa_participant, _qa)] = qa_bindings.as_slice() else {
            return Err(WorkflowPortError::AuthorityConflict);
        };
        let work_item_id = format!("gate-{}", &request.request_digest[..24]);
        let invocation_id =
            uuid_from_digest(&request.request_digest).map_err(|_| WorkflowPortError::Rejected)?;
        let mut inputs = Vec::new();
        for artifact in &evidence.artifacts {
            inputs.extend(
                stage_verified_artifact_inputs(
                    &self.integration.artifact_roots,
                    work.agent_id,
                    qa_participant.agent_id,
                    &work.project_id.0,
                    &work_item_id,
                    &artifact.digest,
                    Some(&artifact.artifact_kind),
                    &artifact.media_type,
                )
                .map_err(|_| WorkflowPortError::Rejected)?,
            );
        }
        inputs.sort_by(|left, right| left.mount_path.cmp(&right.mount_path));
        if inputs.is_empty()
            || inputs.len() > 64
            || inputs
                .windows(2)
                .any(|pair| pair[0].mount_path == pair[1].mount_path)
        {
            return Err(WorkflowPortError::Rejected);
        }
        let now = now_ms();
        // The invocation ID is stable across outbox retries, so every field in
        // the canonical Workbench request must be stable too. Anchor the
        // execution window to the persisted gate request, not this attempt.
        let deadline = work_item_gate_deadline(
            request.created_at_unix_ms,
            self.integration.qa_profile.resource_ceilings.wall_time_ms,
            work.plan.deadline_unix_ms,
            now,
        )?;
        let authority = GateWorkbenchAuthority {
            gate: self.clone(),
            request: request.clone(),
            qa_principal_id: qa_participant.principal_id.clone(),
            qa_agent: qa_participant.agent_id,
        };
        let snapshot = authority
            .snapshot(
                &invocation_id,
                &work.project_id.0,
                &work_item_id,
                &qa_participant.principal_id,
                qa_participant.agent_id,
            )
            .map_err(|_| WorkflowPortError::AuthorityConflict)?;
        let mut workbench = WorkbenchRequest {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: invocation_id.clone(),
            agent_id: qa_participant.agent_id,
            project_id: work.project_id.0.clone(),
            work_item_id: work_item_id.clone(),
            workspace_id: format!("{}:{work_item_id}", work.project_id.0),
            caller_id: qa_participant.principal_id.clone(),
            caller_role: "qa".to_string(),
            assignment_version: snapshot.assignment_version,
            credential_generation: snapshot.credential_generation,
            policy_digest: snapshot.policy_digest,
            tool_profile: self.integration.qa_profile.id.clone(),
            tool_profile_digest: self.integration.qa_profile_digest.clone(),
            runtime_key: WORKBENCH_RUNTIME_BWRAP.to_string(),
            capabilities: BTreeSet::from(["test.run_profile".to_string()]),
            output_artifact_kinds: BTreeSet::new(),
            inputs,
            command_policy: Vec::new(),
            resource_limits: self.integration.qa_profile.resource_ceilings.clone(),
            deadline_unix_ms: deadline,
            attempt: 1,
            tool: WorkbenchTool::RunTests {
                suite_id: "web-work-item-qa-v1".to_string(),
                program: WORK_ITEM_GATE_PROGRAM.to_string(),
                args: Vec::new(),
            },
            input_digest: String::new(),
        };
        let paths: Vec<String> = workbench
            .inputs
            .iter()
            .map(|input| input.mount_path.clone())
            .collect();
        if let WorkbenchTool::RunTests { args, .. } = &mut workbench.tool {
            *args = paths.clone();
        }
        workbench.command_policy = vec![work_item_gate_command_rule(&paths)?];
        workbench.input_digest = workbench
            .canonical_digest()
            .map_err(|_| WorkflowPortError::Rejected)?;
        workbench
            .validate_at(now)
            .map_err(|_| WorkflowPortError::Rejected)?;
        let authority: Arc<dyn WorkbenchAuthoritySource> = Arc::new(authority);
        let update = self
            .integration
            .exchange(|response| WorkbenchDispatchCommand::Submit {
                request: Box::new(workbench),
                authority: Arc::clone(&authority),
                response,
            })
            .map_err(delivery_port_error)?;
        let record = self
            .integration
            .terminal_record(&invocation_id, authority, update)
            .map_err(delivery_port_error)?;
        if record.state != WorkbenchInvocationState::Succeeded
            || record.result_digest.is_none()
            || record.completed_at_ms.is_none()
        {
            return Err(if record.state.is_terminal() {
                WorkflowPortError::Rejected
            } else {
                WorkflowPortError::UnknownOutcome
            });
        }
        Ok(Box::new(WorkItemGateReceipt {
            receipt_id: format!("gate-receipt-{invocation_id}"),
            profile_id: request.expectation.profile_id.clone(),
            profile_generation: request.expectation.profile_generation,
            profile_digest: request.expectation.profile_digest.clone(),
            subject_digest: request.subject_digest.clone(),
            required_checks_digest: request.required_checks_digest.clone(),
            completed_at_ms: record
                .completed_at_ms
                .expect("completion was checked above"),
        }))
    }
}

fn uuid_from_digest(digest: &str) -> anyhow::Result<String> {
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!("digest is not a canonical SHA-256 value");
    }
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &digest[0..8],
        &digest[8..12],
        &digest[12..16],
        &digest[16..20],
        &digest[20..32]
    ))
}

fn delivery_port_error(error: DeliveryError) -> WorkflowPortError {
    match error {
        DeliveryError::AdapterUnavailable { .. } => WorkflowPortError::Unavailable,
        DeliveryError::StaleEvidence(_) | DeliveryError::AuthorityDenied(_) => {
            WorkflowPortError::AuthorityConflict
        }
        DeliveryError::Storage(_) => WorkflowPortError::UnknownOutcome,
        _ => WorkflowPortError::Rejected,
    }
}

#[derive(Clone)]
pub(super) struct LimboDeliveryEffects {
    events: EventStore,
    workflow: Arc<WorkflowStore>,
    principals: Arc<PrincipalAuthenticator>,
}

impl LimboDeliveryEffects {
    pub(super) fn new(
        events: EventStore,
        workflow: Arc<WorkflowStore>,
        principals: Arc<PrincipalAuthenticator>,
    ) -> Self {
        Self {
            events,
            workflow,
            principals,
        }
    }

    fn governed_rework(
        &self,
        request: &DeliveryEffectRequestV1,
    ) -> Result<Vec<VersionedRefV1>, DeliveryError> {
        let bound = self
            .principals
            .principal(&request.actor.principal_id)
            .filter(|bound| {
                bound.principal.tenant_id.0 == request.tenant_id
                    && bound.principal.authority_generation == request.actor.authority_generation
                    && delivery_roles(bound.principal.role) == request.actor.roles
            })
            .ok_or_else(|| {
                DeliveryError::AuthorityDenied(
                    "rework customer authority is not current".to_string(),
                )
            })?;
        let feedback_digest = request.feedback_digest.as_ref().ok_or_else(|| {
            DeliveryError::Validation("rework feedback digest is absent".to_string())
        })?;
        let candidate = request
            .candidate
            .as_ref()
            .ok_or_else(|| DeliveryError::Validation("rework candidate is absent".to_string()))?;
        let operation_uuid =
            uuid_from_digest(request.request_digest.as_str()).map_err(storage_error)?;
        let operation_id = uuid::Uuid::parse_str(&operation_uuid)
            .map_err(|error| DeliveryError::Validation(error.to_string()))?;
        let outcome = self
            .workflow
            .apply_company_command(
                &bound.principal,
                operation_id,
                &CompanyWorkflowCommandV1::CreateGovernedRework {
                    project_id: ProjectId::parse(&request.project.id).map_err(workflow_error)?,
                    expected_version: request.project.generation,
                    source_candidate_digest: candidate.digest.as_str().to_string(),
                    feedback_digest: feedback_digest.as_str().to_string(),
                    source_delivery_id: request.subject.id.clone(),
                },
                now_ms(),
            )
            .map_err(workflow_error)?;
        let CompanyWorkflowResponseV1::Project(project) = outcome.response else {
            return Err(DeliveryError::Storage(
                "rework command returned a non-project response".to_string(),
            ));
        };
        let mut refs = project
            .work_items
            .values()
            .filter(|work| {
                work.spec.rework.as_ref().is_some_and(|binding| {
                    binding.operation_id == operation_id
                        && binding.source_delivery_id == request.subject.id
                        && binding.source_candidate_digest == candidate.digest.as_str()
                        && binding.feedback_digest == feedback_digest.as_str()
                })
            })
            .map(|work| {
                Ok(VersionedRefV1 {
                    id: work.spec.work_item_id.0.clone(),
                    generation: work.version,
                    digest: ContentDigest::parse(work.canonical_digest().map_err(workflow_error)?)?,
                })
            })
            .collect::<Result<Vec<_>, DeliveryError>>()?;
        refs.sort_by(|left, right| left.id.cmp(&right.id));
        if refs.is_empty() {
            return Err(DeliveryError::MissingEvidence(
                "governed rework produced no work items".to_string(),
            ));
        }
        Ok(refs)
    }
}

impl DeliveryEffectPort for LimboDeliveryEffects {
    fn readiness(&self) -> AdapterReadiness {
        AdapterReadiness::Ready {
            contract_version: DELIVERY_SCHEMA_V1,
            authority_generation: DELIVERY_AUTHORITY_GENERATION,
            contract_digest: expected_effect_saga_contract_digest(),
        }
    }

    fn apply(
        &self,
        request: &DeliveryEffectRequestV1,
    ) -> Result<DeliveryEffectReceiptV1, DeliveryError> {
        let affected_refs = if request.kind == crate::delivery::DeliveryEffectKind::GovernedRework {
            self.governed_rework(request)?
        } else {
            Vec::new()
        };
        let event = effect_event(request)?;
        ensure_or_append_event(&self.events, &event, None)?;
        DeliveryEffectReceiptV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            operation_id: request.operation_id.clone(),
            kind: request.kind,
            tenant_id: request.tenant_id.clone(),
            project: request.project.clone(),
            candidate: request.candidate.clone(),
            subject: request.subject.clone(),
            target: request.target.clone(),
            actor: request.actor.clone(),
            request_digest: request.request_digest.clone(),
            actor_authority_receipt_digest: request.actor_authority_receipt_digest.clone(),
            actor_authority_identity_digest: request.actor_authority_identity_digest.clone(),
            effect_ref: VersionedRefV1 {
                id: event.event_id,
                generation: 1,
                digest: request.request_digest.clone(),
            },
            affected_refs,
            issuer: "sentinel-limbo".to_string(),
            issued_at_ms: request.occurred_at_ms,
            receipt_digest: ContentDigest::zero(),
        }
        .seal()
    }
}

#[derive(Clone)]
pub(super) struct LimboDeliveryPublication {
    events: EventStore,
}

impl LimboDeliveryPublication {
    pub(super) fn new(events: EventStore) -> Self {
        Self { events }
    }
}

impl DeliveryPublicationPort for LimboDeliveryPublication {
    fn readiness(&self) -> AdapterReadiness {
        AdapterReadiness::Ready {
            contract_version: DELIVERY_SCHEMA_V1,
            authority_generation: DELIVERY_AUTHORITY_GENERATION,
            contract_digest: expected_publication_contract_digest(),
        }
    }

    fn publish(
        &self,
        request: &PublicationRequestV1,
    ) -> Result<PublicationReceiptV1, DeliveryError> {
        let payload = String::from_utf8(request.payload.clone())
            .map_err(|error| DeliveryError::Validation(error.to_string()))?;
        let event = DomainEvent {
            event_id: format!("delivery-event-{}", request.request_digest.as_str()),
            event_type: request.event_type.clone(),
            aggregate_id: request.aggregate_id.clone(),
            payload,
            correlation_id: request.aggregate_id.clone(),
            causation_id: None,
            operation_id: request.operation_id.clone(),
            tick: 0,
            timestamp_ms: request.occurred_at_ms,
            schema_version: u32::from(DELIVERY_SCHEMA_V1),
            compensation_type: "none".to_string(),
        };
        ensure_or_append_event(&self.events, &event, Some(DELIVERY_EVENT_TOPIC))?;
        Ok(PublicationReceiptV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            operation_id: request.operation_id.clone(),
            event_id: event.event_id,
            aggregate_id: request.aggregate_id.clone(),
            row_identity: request.row_identity.clone(),
            payload_digest: request.payload_digest.clone(),
            request_digest: request.request_digest.clone(),
        })
    }
}

fn effect_event(request: &DeliveryEffectRequestV1) -> Result<DomainEvent, DeliveryError> {
    let (event_type, payload) =
        if request.kind == crate::delivery::DeliveryEffectKind::MemoryPublication {
            let source = request.closeout_memory.as_ref().ok_or_else(|| {
                DeliveryError::Validation("closeout memory source is absent".to_string())
            })?;
            let candidate = request.candidate.as_ref().ok_or_else(|| {
                DeliveryError::Validation("closeout candidate is absent".to_string())
            })?;
            let payload = DomainEventPayload::ProjectCloseoutPublished {
                tenant_id: request.tenant_id.clone(),
                project_id: request.project.id.clone(),
                project_generation: request.project.generation,
                project_digest: request.project.digest.as_str().to_string(),
                candidate_id: candidate.id.clone(),
                candidate_generation: candidate.generation,
                candidate_digest: candidate.digest.as_str().to_string(),
                release_id: request.subject.id.clone(),
                release_generation: request.subject.generation,
                release_digest: request.subject.digest.as_str().to_string(),
                acceptance_id: source.acceptance.id.clone(),
                acceptance_generation: source.acceptance.generation,
                acceptance_digest: source.acceptance.digest.as_str().to_string(),
                decisions_digest: source.decisions_digest.as_str().to_string(),
                artifact_inventory_digest: source.artifact_inventory_digest.as_str().to_string(),
                failures_digest: source.failures_digest.as_str().to_string(),
                lessons_digest: source.lessons_digest.as_str().to_string(),
            };
            (
                "project_closeout_published".to_string(),
                serde_json::to_string(&payload)
                    .map_err(|error| DeliveryError::Storage(error.to_string()))?,
            )
        } else {
            (
                format!("delivery_effect_{:?}", request.kind).to_ascii_lowercase(),
                String::from_utf8(ContentDigest::canonical_bytes(request)?)
                    .map_err(|error| DeliveryError::Storage(error.to_string()))?,
            )
        };
    Ok(DomainEvent {
        event_id: format!("delivery-effect-{}", request.request_digest.as_str()),
        event_type,
        aggregate_id: format!("DELIVERY:{}:{}", request.tenant_id, request.project.id),
        payload,
        correlation_id: request.project.id.clone(),
        causation_id: None,
        operation_id: request.operation_id.clone(),
        tick: 0,
        timestamp_ms: request.occurred_at_ms,
        schema_version: u32::from(DELIVERY_SCHEMA_V1),
        compensation_type: "none".to_string(),
    })
}

fn ensure_or_append_event(
    store: &EventStore,
    expected: &DomainEvent,
    topic: Option<&str>,
) -> Result<(), DeliveryError> {
    if let Some(existing) = store
        .event_by_operation_id(&expected.operation_id)
        .map_err(storage_error)?
    {
        return same_event(&existing, expected);
    }
    match topic {
        Some(topic) => store.append_with_outbox(expected, topic),
        None => store.append_event(expected),
    }
    .map_err(storage_error)?;
    let existing = store
        .event_by_operation_id(&expected.operation_id)
        .map_err(storage_error)?
        .ok_or_else(|| {
            DeliveryError::Storage("delivery event disappeared after commit".to_string())
        })?;
    same_event(&existing, expected)
}

fn same_event(existing: &DomainEvent, expected: &DomainEvent) -> Result<(), DeliveryError> {
    if existing.event_id == expected.event_id
        && existing.event_type == expected.event_type
        && existing.aggregate_id == expected.aggregate_id
        && existing.payload == expected.payload
        && existing.correlation_id == expected.correlation_id
        && existing.causation_id == expected.causation_id
        && existing.operation_id == expected.operation_id
        && existing.tick == expected.tick
        && existing.timestamp_ms == expected.timestamp_ms
        && existing.schema_version == expected.schema_version
        && existing.compensation_type == expected.compensation_type
    {
        Ok(())
    } else {
        Err(DeliveryError::Conflict(
            "delivery operation id is already bound to different content".to_string(),
        ))
    }
}

fn delivery_roles(role: CompanyRoleV1) -> BTreeSet<AuthorityRole> {
    match role {
        CompanyRoleV1::Customer => BTreeSet::from([AuthorityRole::Customer]),
        CompanyRoleV1::Designer | CompanyRoleV1::Developer => {
            BTreeSet::from([AuthorityRole::Developer])
        }
        CompanyRoleV1::Qa => BTreeSet::from([AuthorityRole::Qa]),
        CompanyRoleV1::ReleaseManager => BTreeSet::from([AuthorityRole::ReleaseManager]),
        CompanyRoleV1::Gaia => BTreeSet::from([AuthorityRole::GaiaObserver]),
        CompanyRoleV1::Sales | CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead => {
            BTreeSet::new()
        }
    }
}

fn workflow_error(error: impl std::fmt::Display) -> DeliveryError {
    DeliveryError::Storage(error.to_string())
}

fn storage_error(error: impl std::fmt::Display) -> DeliveryError {
    DeliveryError::Storage(error.to_string())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_qa_command_policy_is_bound_to_the_exact_input_inventory() {
        let paths = vec!["design.md".to_string(), "src/index.html".to_string()];
        let rule = web_qa_command_rule(&paths).unwrap();

        assert!(rule.allows(WEB_QA_PROGRAM, &paths));
        assert!(!rule.allows(WEB_QA_PROGRAM, &paths[..1]));
        assert!(!rule.allows(WEB_QA_PROGRAM, &[paths[1].clone(), paths[0].clone()]));
        let mut extended = paths.clone();
        extended.push("foreign.txt".to_string());
        assert!(!rule.allows(WEB_QA_PROGRAM, &extended));
        assert!(web_qa_command_rule(&[]).is_err());
    }

    #[test]
    fn company_governance_roles_without_delivery_authority_stay_out_of_delivery_inventory() {
        for role in [
            CompanyRoleV1::Sales,
            CompanyRoleV1::ProjectManager,
            CompanyRoleV1::TechnicalLead,
        ] {
            assert!(delivery_roles(role).is_empty());
        }
        assert_eq!(
            delivery_roles(CompanyRoleV1::Developer),
            BTreeSet::from([AuthorityRole::Developer])
        );
    }

    #[test]
    fn work_item_gate_command_policy_binds_exact_helper_and_inputs() {
        let paths = vec!["design.md".to_string(), "screens/home.txt".to_string()];
        let rule = work_item_gate_command_rule(&paths).unwrap();

        assert!(rule.allows(WORK_ITEM_GATE_PROGRAM, &paths));
        assert!(!rule.allows("node", &paths));
        assert!(!rule.allows(WORK_ITEM_GATE_PROGRAM, &paths[..1]));
        let mut extended = paths.clone();
        extended.push("foreign.txt".to_string());
        assert!(!rule.allows(WORK_ITEM_GATE_PROGRAM, &extended));
        assert!(work_item_gate_command_rule(&[]).is_err());
    }

    #[test]
    fn work_item_gate_deadline_is_stable_across_retries() {
        let first = work_item_gate_deadline(1_000, 30_000, 90_000, 2_000).unwrap();
        let retry = work_item_gate_deadline(1_000, 30_000, 90_000, 20_000).unwrap();

        assert_eq!(first, 31_000);
        assert_eq!(retry, first);
        assert_eq!(
            work_item_gate_deadline(1_000, 30_000, 20_000, 2_000).unwrap(),
            20_000
        );
        assert_eq!(
            work_item_gate_deadline(1_000, 30_000, 90_000, first),
            Err(WorkflowPortError::TimedOut)
        );
    }

    fn digest(value: &str) -> ContentDigest {
        ContentDigest::of(&value).unwrap()
    }

    fn reference(id: &str, generation: u64) -> VersionedRefV1 {
        VersionedRefV1 {
            id: id.to_string(),
            generation,
            digest: digest(id),
        }
    }

    #[test]
    fn qa_receipt_inventory_matches_the_canonical_evidence_graph() {
        let qa = PrincipalV1 {
            tenant_id: "tenant-a".to_string(),
            principal_id: "qa-a".to_string(),
            authority_generation: 1,
            roles: BTreeSet::from([AuthorityRole::Qa]),
        };
        let request = WorkbenchEvidenceRequestV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            tenant_id: "tenant-a".to_string(),
            project: reference("project-a", 1),
            candidate: reference("candidate-a", 1),
            qa_plan: reference("qa-plan-a", 1),
            qa_run: reference("qa-run-a", 1),
            candidate_artifacts: vec![crate::delivery::ArtifactRefV1 {
                artifact_id: "artifact-a".to_string(),
                generation: 1,
                digest: digest("artifact-a"),
                media_type: "application/octet-stream".to_string(),
                owner_principal_id: "developer-a".to_string(),
            }],
            assigned_qa: qa,
            authority_receipt_digest: digest("authority-receipt"),
            authority_identity_digest: digest("authority-identity"),
            invocation: reference("invocation-a", 1),
            started_at_ms: 100,
            request_digest: digest("workbench-request"),
        };
        let workbench_request = WorkbenchRequest {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: request.invocation.id.clone(),
            agent_id: AgentId(7),
            project_id: request.project.id.clone(),
            work_item_id: "qa-work-a".to_string(),
            workspace_id: "workspace-a".to_string(),
            caller_id: request.assigned_qa.principal_id.clone(),
            caller_role: "qa".to_string(),
            assignment_version: 1,
            credential_generation: 1,
            policy_digest: digest("policy").as_str().to_string(),
            tool_profile: "web-qa-v1".to_string(),
            tool_profile_digest: digest("profile").as_str().to_string(),
            runtime_key: WORKBENCH_RUNTIME_BWRAP.to_string(),
            capabilities: BTreeSet::from(["test.run_profile".to_string()]),
            output_artifact_kinds: BTreeSet::new(),
            inputs: Vec::new(),
            command_policy: Vec::new(),
            resource_limits: WorkbenchResourceLimits {
                wall_time_ms: 1_000,
                cpu_time_ms: 1_000,
                memory_bytes: 1_048_576,
                process_count: 1,
                file_bytes: 4_096,
                stdout_bytes: 4_096,
                stderr_bytes: 4_096,
            },
            deadline_unix_ms: 2_000,
            attempt: 1,
            tool: WorkbenchTool::RunTests {
                suite_id: "web-qa-v1".to_string(),
                program: "sentinel-web-qa".to_string(),
                args: Vec::new(),
            },
            input_digest: request.request_digest.as_str().to_string(),
        };
        let record = WorkbenchInvocationRecord {
            store_schema_version: 2,
            invocation_id: workbench_request.invocation_id.clone(),
            request_digest: workbench_request.input_digest.clone(),
            agent_id: workbench_request.agent_id,
            project_id: workbench_request.project_id.clone(),
            work_item_id: workbench_request.work_item_id.clone(),
            workspace_id: workbench_request.workspace_id.clone(),
            caller_id: workbench_request.caller_id.clone(),
            caller_role: workbench_request.caller_role.clone(),
            assignment_version: workbench_request.assignment_version,
            credential_generation: workbench_request.credential_generation,
            policy_digest: workbench_request.policy_digest.clone(),
            tool_profile: workbench_request.tool_profile.clone(),
            tool_profile_digest: workbench_request.tool_profile_digest.clone(),
            runtime_key: workbench_request.runtime_key.clone(),
            tool_class: "test.run_profile".to_string(),
            package_artifact_kind: None,
            package_media_type: None,
            capabilities: workbench_request.capabilities.clone(),
            output_artifact_kinds: BTreeSet::new(),
            attempt: 1,
            state: WorkbenchInvocationState::Succeeded,
            reserved_at_ms: 100,
            started_at_ms: Some(100),
            completed_at_ms: Some(101),
            resources: None,
            result_digest: Some(digest("qa-result").as_str().to_string()),
            artifacts: Vec::new(),
            error: None,
        };

        let receipt = qa_receipt(&request, &workbench_request, &record).unwrap();
        let graph =
            m0_qa_evidence_graph(&request.qa_run, &request.qa_plan.digest, &receipt).unwrap();

        assert_eq!(
            receipt.result_inventory_digest,
            qa_evidence_inventory_digest(&graph).unwrap()
        );
        assert_eq!(receipt.harness_outcome, QaHarnessOutcome::Pass);
    }

    #[test]
    fn memory_effect_emits_typed_source_linked_closeout_event() {
        let request = DeliveryEffectRequestV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            operation_id: "memory:tenant-a:project-a:closeout-a".to_string(),
            kind: crate::delivery::DeliveryEffectKind::MemoryPublication,
            tenant_id: "tenant-a".to_string(),
            project: reference("project-a", 7),
            candidate: Some(reference("candidate-a", 3)),
            subject: reference("release-a", 2),
            target: None,
            feedback_digest: None,
            closeout_memory: Some(crate::delivery::CloseoutMemorySourceV1 {
                acceptance: reference("acceptance-a", 1),
                decisions_digest: digest("decisions"),
                artifact_inventory_digest: digest("artifacts"),
                failures_digest: digest("failures"),
                lessons_digest: digest("lessons"),
            }),
            occurred_at_ms: 100,
            actor: PrincipalV1 {
                tenant_id: "tenant-a".to_string(),
                principal_id: "release-manager-a".to_string(),
                authority_generation: 4,
                roles: BTreeSet::from([AuthorityRole::ReleaseManager]),
            },
            actor_authority_receipt_digest: digest("receipt"),
            actor_authority_identity_digest: digest("identity"),
            request_digest: ContentDigest::zero(),
        }
        .seal()
        .unwrap();

        let event = effect_event(&request).unwrap();
        assert_eq!(event.event_type, "project_closeout_published");
        assert_eq!(event.operation_id, request.operation_id);
        assert!(matches!(
            serde_json::from_str::<DomainEventPayload>(&event.payload).unwrap(),
            DomainEventPayload::ProjectCloseoutPublished {
                project_id,
                project_generation: 7,
                candidate_id,
                candidate_generation: 3,
                release_id,
                release_generation: 2,
                acceptance_id,
                acceptance_generation: 1,
                ..
            } if project_id == "project-a"
                && candidate_id == "candidate-a"
                && release_id == "release-a"
                && acceptance_id == "acceptance-a"
        ));
    }

    #[test]
    fn effect_and_publication_retries_reuse_the_exact_committed_event() {
        let directory = tempfile::tempdir().unwrap();
        let events =
            EventStore::open(directory.path().join("events.db").to_str().unwrap()).unwrap();
        let effect_request = DeliveryEffectRequestV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            operation_id: "memory:tenant-a:project-a:retry".to_string(),
            kind: crate::delivery::DeliveryEffectKind::MemoryPublication,
            tenant_id: "tenant-a".to_string(),
            project: reference("project-a", 7),
            candidate: Some(reference("candidate-a", 3)),
            subject: reference("release-a", 2),
            target: None,
            feedback_digest: None,
            closeout_memory: Some(crate::delivery::CloseoutMemorySourceV1 {
                acceptance: reference("acceptance-a", 1),
                decisions_digest: digest("decisions"),
                artifact_inventory_digest: digest("artifacts"),
                failures_digest: digest("failures"),
                lessons_digest: digest("lessons"),
            }),
            occurred_at_ms: 100,
            actor: PrincipalV1 {
                tenant_id: "tenant-a".to_string(),
                principal_id: "release-manager-a".to_string(),
                authority_generation: 4,
                roles: BTreeSet::from([AuthorityRole::ReleaseManager]),
            },
            actor_authority_receipt_digest: digest("receipt"),
            actor_authority_identity_digest: digest("identity"),
            request_digest: ContentDigest::zero(),
        }
        .seal()
        .unwrap();
        let effect = effect_event(&effect_request).unwrap();
        ensure_or_append_event(&events, &effect, None).unwrap();
        ensure_or_append_event(&events, &effect, None).unwrap();
        let mut conflicting_effect = effect.clone();
        conflicting_effect.timestamp_ms += 1;
        assert!(matches!(
            ensure_or_append_event(&events, &conflicting_effect, None),
            Err(DeliveryError::Conflict(_))
        ));

        let publication = PublicationRequestV1 {
            schema_version: DELIVERY_SCHEMA_V1,
            operation_id: "delivery:tenant-a:project-a:0001:test".to_string(),
            event_type: "delivery_test_v1".to_string(),
            aggregate_id: "tenant-a:project-a".to_string(),
            row_identity: "delivery-journal:test".to_string(),
            payload_digest: digest("payload"),
            payload: br#"{"schema_version":1}"#.to_vec(),
            occurred_at_ms: 101,
            request_digest: ContentDigest::zero(),
        }
        .seal()
        .unwrap();
        let publisher = LimboDeliveryPublication::new(events.clone());
        let first = publisher.publish(&publication).unwrap();
        let second = publisher.publish(&publication).unwrap();
        assert_eq!(first, second);
        assert_eq!(events.get_latest_event_id().unwrap(), 2);
    }
}
