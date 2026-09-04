use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::admission::*;
use crate::collaboration::*;
use crate::digest::canonical_sha256;
use crate::model::{validate_digest, validate_identifier};
use crate::{AgentId, ProjectId, TenantId, WorkItemId, WorkflowError, WorkflowErrorCode};

pub const COMPANY_DOMAIN_SCHEMA_VERSION: u16 = 1;
pub const MAX_COMPANY_ITEMS: usize = 256;
const MAX_TEXT_BYTES: usize = 4_096;
const MAX_COLLECTION_ITEMS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompanyRoleV1 {
    Customer,
    Sales,
    ProjectManager,
    TechnicalLead,
    Designer,
    Developer,
    Qa,
    ReleaseManager,
    Gaia,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompanyPrincipalKindV1 {
    Customer,
    Operator,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthenticatedCompanyPrincipalV1 {
    pub schema_version: u16,
    pub tenant_id: TenantId,
    pub principal_id: String,
    pub kind: CompanyPrincipalKindV1,
    pub role: CompanyRoleV1,
    pub customer_id: Option<String>,
    pub agent_id: Option<AgentId>,
    pub authority_generation: u64,
    pub authority_digest: String,
}

impl AuthenticatedCompanyPrincipalV1 {
    pub fn validate(&self) -> Result<(), WorkflowError> {
        if self.schema_version != COMPANY_DOMAIN_SCHEMA_VERSION || self.authority_generation == 0 {
            return Err(invalid("company principal authority is invalid"));
        }
        self.tenant_id.validate()?;
        validate_identifier(&self.principal_id)?;
        validate_digest(&self.authority_digest)?;
        match (self.kind, self.customer_id.as_deref(), self.agent_id) {
            (CompanyPrincipalKindV1::Customer, Some(customer), None) => {
                validate_identifier(customer)?;
                if self.role != CompanyRoleV1::Customer {
                    return Err(unauthorized());
                }
            }
            (CompanyPrincipalKindV1::Agent, None, Some(AgentId(id))) if id != 0 => {
                if self.role == CompanyRoleV1::Customer {
                    return Err(unauthorized());
                }
            }
            (CompanyPrincipalKindV1::Operator, None, None)
                if self.role != CompanyRoleV1::Customer => {}
            _ => return Err(unauthorized()),
        }
        Ok(())
    }

    pub(crate) fn namespace(&self) -> String {
        format!("company-v1:{}:{}", self.tenant_id.0, self.principal_id)
    }

    pub fn binding_digest(&self) -> Result<String, WorkflowError> {
        canonical_sha256("sentinel.workflow.company-principal.v1", self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkProfileBindingV1 {
    pub profile_id: String,
    pub generation: u64,
    pub digest: String,
}

impl WorkProfileBindingV1 {
    pub(crate) fn validate(&self) -> Result<(), WorkflowError> {
        validate_identifier(&self.profile_id)?;
        validate_digest(&self.digest)?;
        if self.generation == 0 {
            return Err(invalid("work profile generation must be positive"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParticipantBindingV1 {
    pub agent_id: AgentId,
    pub principal_id: String,
    pub role: CompanyRoleV1,
    pub specialties: BTreeSet<String>,
    pub reports_to: Option<AgentId>,
    pub profile: WorkProfileBindingV1,
}

impl ParticipantBindingV1 {
    pub(crate) fn validate(&self) -> Result<(), WorkflowError> {
        validate_identifier(&self.principal_id)?;
        if self.agent_id.0 == 0
            || self.role == CompanyRoleV1::Customer
            || self.specialties.is_empty()
            || self.specialties.len() > MAX_COLLECTION_ITEMS
            || self.reports_to == Some(self.agent_id)
        {
            return Err(invalid("participant binding is invalid"));
        }
        for specialty in &self.specialties {
            validate_identifier(specialty)?;
        }
        self.profile.validate()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomerRequestStateV1 {
    Submitted,
    Clarifying,
    Qualified,
    Proposed,
    Accepted,
    Rejected,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClarificationV1 {
    pub question_ref: String,
    pub answer_ref: String,
    pub recorded_by: String,
    pub recorded_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomerFeedbackV1 {
    pub feedback_ref: String,
    pub recorded_by: String,
    pub recorded_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomerRequestV1 {
    pub schema_version: u16,
    pub request_id: String,
    pub tenant_id: TenantId,
    pub customer_id: String,
    pub summary_ref: String,
    pub desired_outcome: String,
    pub constraints: Vec<String>,
    pub clarifications: Vec<ClarificationV1>,
    pub feedback: Vec<CustomerFeedbackV1>,
    pub state: CustomerRequestStateV1,
    pub version: u64,
    pub proposal_ids: Vec<String>,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalGovernanceV1 {
    pub owner: AgentId,
    pub participants: Vec<ParticipantBindingV1>,
    pub project_profile: WorkProfileBindingV1,
}

impl ProposalGovernanceV1 {
    pub(crate) fn validate(&self) -> Result<(), WorkflowError> {
        if self.owner.0 == 0 || self.participants.is_empty() || self.participants.len() > 64 {
            return Err(invalid("proposal governance is invalid"));
        }
        let mut ids = BTreeSet::new();
        let mut principal_ids = BTreeSet::new();
        let mut profiles = BTreeMap::new();
        for participant in &self.participants {
            participant.validate()?;
            if !ids.insert(participant.agent_id.0)
                || !principal_ids.insert(participant.principal_id.as_str())
            {
                return Err(invalid("participant identity is duplicated"));
            }
            if let Some(existing) = profiles.insert(
                participant.profile.profile_id.clone(),
                (
                    participant.profile.generation,
                    participant.profile.digest.clone(),
                ),
            ) {
                if existing
                    != (
                        participant.profile.generation,
                        participant.profile.digest.clone(),
                    )
                {
                    return Err(invalid("shared profile binding is inconsistent"));
                }
            }
        }
        if !ids.contains(&self.owner.0) {
            return Err(invalid("project owner is not a participant"));
        }
        for participant in &self.participants {
            if participant
                .reports_to
                .is_some_and(|manager| !ids.contains(&manager.0))
            {
                return Err(invalid(
                    "participant hierarchy references an unknown manager",
                ));
            }
        }
        for start in &ids {
            let mut seen = BTreeSet::new();
            let mut current = Some(AgentId(*start));
            while let Some(agent) = current {
                if !seen.insert(agent.0) {
                    return Err(invalid("participant hierarchy contains a cycle"));
                }
                current = self
                    .participants
                    .iter()
                    .find(|participant| participant.agent_id == agent)
                    .and_then(|participant| participant.reports_to);
            }
        }
        self.project_profile.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalBindingV1 {
    pub scope: String,
    pub deliverables: Vec<String>,
    pub exclusions: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub assumptions: Vec<String>,
    pub cost_ceiling_micros: u64,
    pub provider_cost_ceilings_micros: BTreeMap<String, u64>,
    pub governance: ProposalGovernanceV1,
    pub expires_at_unix_ms: u64,
}

impl ProposalBindingV1 {
    pub(crate) fn validate(&self, now_ms: u64) -> Result<(), WorkflowError> {
        validate_text(&self.scope)?;
        validate_text_collection(&self.deliverables, true)?;
        validate_text_collection(&self.exclusions, false)?;
        validate_text_collection(&self.acceptance_criteria, true)?;
        validate_text_collection(&self.assumptions, false)?;
        if self.cost_ceiling_micros == 0
            || self.expires_at_unix_ms <= now_ms
            || self.provider_cost_ceilings_micros.is_empty()
        {
            return Err(invalid("proposal cost or expiry contract is invalid"));
        }
        for (provider, ceiling) in &self.provider_cost_ceilings_micros {
            validate_identifier(provider)?;
            if *ceiling == 0 || *ceiling > self.cost_ceiling_micros {
                return Err(invalid("provider cost ceiling is invalid"));
            }
        }
        self.governance.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalV1 {
    pub schema_version: u16,
    pub proposal_id: String,
    pub tenant_id: TenantId,
    pub request_id: String,
    pub generation: u32,
    pub binding: ProposalBindingV1,
    pub proposal_digest: String,
    pub created_by: String,
    pub created_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgreementV1 {
    pub schema_version: u16,
    pub agreement_id: String,
    pub tenant_id: TenantId,
    pub request_id: String,
    pub proposal_id: String,
    pub proposal_digest: String,
    pub customer_id: String,
    pub accepted_by: String,
    pub accepted_at_unix_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompanyWorkStateV1 {
    DependencyPending,
    Ready,
    Assigned,
    InProgress,
    InReview,
    Done,
    Blocked,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectLifecycleStateV1 {
    Planning,
    Active,
    Blocked,
    DeliveryCandidate,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkOutputContractV1 {
    pub name: String,
    pub media_type: String,
    pub digest_algorithm: String,
    pub contract_generation: u64,
    pub contract_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkInputContractV1 {
    pub name: String,
    pub producer_work_item_id: WorkItemId,
    pub producer_output_name: String,
    pub expected_contract_generation: u64,
    pub expected_contract_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityGateBindingV1 {
    pub gate_id: String,
    pub generation: u64,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkOutputReceiptV1 {
    pub name: String,
    pub contract_generation: u64,
    pub contract_digest: String,
    pub content_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityGateReceiptBindingV1 {
    pub gate_id: String,
    pub generation: u64,
    pub gate_digest: String,
    pub subject_digest: String,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompanyWorkItemSpecV1 {
    pub work_item_id: WorkItemId,
    pub title: String,
    pub objective: String,
    pub required_role: CompanyRoleV1,
    pub required_specialties: BTreeSet<String>,
    pub dependency_ids: BTreeSet<WorkItemId>,
    pub owner: AgentId,
    pub inputs: Vec<WorkInputContractV1>,
    pub outputs: Vec<WorkOutputContractV1>,
    pub quality_gate: QualityGateBindingV1,
    pub budget_micros: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rework: Option<GovernedReworkBindingV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GovernedReworkBindingV1 {
    pub operation_id: Uuid,
    pub source_work_item_id: WorkItemId,
    pub source_delivery_id: String,
    pub source_candidate_digest: String,
    pub feedback_digest: String,
    pub generation: u64,
}

impl CompanyWorkItemSpecV1 {
    pub(crate) fn validate(&self) -> Result<(), WorkflowError> {
        self.work_item_id.validate()?;
        validate_text(&self.title)?;
        validate_text(&self.objective)?;
        if self.required_role == CompanyRoleV1::Customer
            || self.required_specialties.is_empty()
            || self.owner.0 == 0
            || self.budget_micros == 0
            || self.dependency_ids.contains(&self.work_item_id)
        {
            return Err(invalid("work item specification is invalid"));
        }
        for value in &self.required_specialties {
            validate_identifier(value)?;
        }
        let mut input_names = BTreeSet::new();
        for input in &self.inputs {
            validate_identifier(&input.name)?;
            input.producer_work_item_id.validate()?;
            validate_identifier(&input.producer_output_name)?;
            validate_digest(&input.expected_contract_digest)?;
            if input.expected_contract_generation == 0
                || !input_names.insert(input.name.as_str())
                || !self.dependency_ids.contains(&input.producer_work_item_id)
            {
                return Err(invalid("work input contract is invalid"));
            }
        }
        let mut output_names = BTreeSet::new();
        for output in &self.outputs {
            validate_identifier(&output.name)?;
            validate_text(&output.media_type)?;
            validate_identifier(&output.digest_algorithm)?;
            validate_digest(&output.contract_digest)?;
            if output.contract_generation == 0
                || output.digest_algorithm != "sha256"
                || !output_names.insert(output.name.as_str())
            {
                return Err(invalid("work output contract is invalid"));
            }
        }
        validate_identifier(&self.quality_gate.gate_id)?;
        validate_digest(&self.quality_gate.digest)?;
        if self.quality_gate.generation == 0 {
            return Err(invalid("quality gate binding is invalid"));
        }
        if let Some(rework) = &self.rework {
            if rework.operation_id.is_nil() {
                return Err(invalid("governed rework operation is invalid"));
            }
            rework.source_work_item_id.validate()?;
            validate_identifier(&rework.source_delivery_id)?;
            validate_digest(&rework.source_candidate_digest)?;
            validate_digest(&rework.feedback_digest)?;
            if rework.generation == 0 || rework.source_work_item_id == self.work_item_id {
                return Err(invalid("governed rework binding is invalid"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssignmentV1 {
    pub assignment_id: String,
    pub agent_id: AgentId,
    pub role: CompanyRoleV1,
    pub specialties: BTreeSet<String>,
    pub profile: WorkProfileBindingV1,
    pub organization_generation: u64,
    pub organization_digest: String,
    pub assignment_version: u64,
    pub delegated_by: Option<AgentId>,
    pub reason_ref: String,
    pub active: bool,
    pub assigned_by: String,
    pub created_at_unix_ms: u64,
    pub ended_at_unix_ms: Option<u64>,
}

impl AssignmentV1 {
    pub fn canonical_digest(&self) -> Result<String, WorkflowError> {
        canonical_sha256("sentinel.workflow.assignment.v1", self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompanyWorkItemV1 {
    pub spec: CompanyWorkItemSpecV1,
    pub state: CompanyWorkStateV1,
    pub version: u64,
    pub assignments: Vec<AssignmentV1>,
    pub output_receipts: Vec<WorkOutputReceiptV1>,
    pub gate_receipt: Option<QualityGateReceiptBindingV1>,
    pub transition_history: Vec<StateTransitionAuditV1>,
}

impl CompanyWorkItemV1 {
    pub fn canonical_digest(&self) -> Result<String, WorkflowError> {
        canonical_sha256("sentinel.workflow.company-work-item.v1", self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateTransitionAuditV1 {
    pub before: String,
    pub after: String,
    pub actor_id: String,
    pub actor_agent_id: AgentId,
    pub reason_ref: String,
    pub occurred_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionV1 {
    pub decision_id: String,
    pub work_item_id: Option<WorkItemId>,
    pub choice_ref: String,
    pub rationale_ref: String,
    pub decided_by: String,
    pub created_at_unix_ms: u64,
}

impl DecisionV1 {
    pub fn canonical_digest(&self) -> Result<String, WorkflowError> {
        canonical_sha256("sentinel.workflow.project-decision.v1", self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffStateV1 {
    Offered,
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffV1 {
    pub handoff_id: String,
    pub work_item_id: WorkItemId,
    pub producer: AgentId,
    pub consumer: AgentId,
    pub artifact_digests: BTreeSet<String>,
    pub state: HandoffStateV1,
    pub reason_ref: String,
    pub created_at_unix_ms: u64,
    pub acknowledged_by: Option<String>,
    pub acknowledged_at_unix_ms: Option<u64>,
    pub acknowledgement_reason_ref: Option<String>,
    pub transition_history: Vec<StateTransitionAuditV1>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockerStateV1 {
    Open,
    Escalated,
    Resolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockerV1 {
    pub blocker_id: String,
    pub work_item_id: Option<WorkItemId>,
    pub cause_ref: String,
    pub owner: AgentId,
    pub escalation_target: Option<AgentId>,
    pub state: BlockerStateV1,
    pub blocker_kind: BlockerKindV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_from_state: Option<ProjectLifecycleStateV1>,
    pub resolution_ref: Option<String>,
    pub last_actor_id: String,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
    pub transition_history: Vec<StateTransitionAuditV1>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockerKindV1 {
    Operational,
    BudgetExhausted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalV1 {
    pub approval_id: String,
    pub work_item_id: WorkItemId,
    pub subject_digest: String,
    pub approved: bool,
    pub actor_id: String,
    pub actor_agent_id: AgentId,
    pub created_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CostReservationV1 {
    pub reservation_id: String,
    pub work_item_id: Option<WorkItemId>,
    pub provider: String,
    pub reserved_micros: u64,
    pub committed_micros: Option<u64>,
    /// Durable Limbo usage-event operation bound to the committed provider cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_event_operation_id: Option<String>,
    pub state: CostReservationStateV1,
    pub created_by: String,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostReservationStateV1 {
    Active,
    Committed,
    Released,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectRoomKindV1 {
    Project,
    Team,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectRoomV1 {
    pub room_id: String,
    pub kind: ProjectRoomKindV1,
    pub members: Vec<AgentId>,
    pub created_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectQuestionV1 {
    pub question_id: String,
    pub work_item_id: Option<WorkItemId>,
    pub owner: AgentId,
    pub question_ref: String,
    pub resolution_ref: Option<String>,
    pub created_by: String,
    pub resolved_by: Option<String>,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectActionV1 {
    pub action_id: String,
    pub work_item_id: Option<WorkItemId>,
    pub owner: AgentId,
    pub action_ref: String,
    pub completed: bool,
    pub created_by: String,
    pub completed_by: Option<String>,
    pub resolution_ref: Option<String>,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectV1 {
    pub schema_version: u16,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub agreement_id: String,
    pub agreement_digest: String,
    pub governance: ProposalGovernanceV1,
    pub cost_ceiling_micros: u64,
    pub provider_cost_ceilings_micros: BTreeMap<String, u64>,
    pub lifecycle_state: ProjectLifecycleStateV1,
    pub reserved_cost_micros: u64,
    pub committed_cost_micros: u64,
    pub work_items: BTreeMap<WorkItemId, CompanyWorkItemV1>,
    pub decisions: Vec<DecisionV1>,
    pub handoffs: Vec<HandoffV1>,
    pub blockers: Vec<BlockerV1>,
    pub approvals: Vec<ApprovalV1>,
    pub reservations: Vec<CostReservationV1>,
    pub rooms: Vec<ProjectRoomV1>,
    pub questions: Vec<ProjectQuestionV1>,
    pub actions: Vec<ProjectActionV1>,
    /// Missing means a pre-#739 project whose legacy handoffs remain read-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collaboration_schema_version: Option<u16>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collaboration_sessions: Vec<CollaborationSessionV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handoff_packets: Vec<HandoffPacketV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dissent_records: Vec<DissentRecordV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decision_evidence: Vec<DecisionEvidenceLinkV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collaboration_publications: Vec<CollaborationPublicationV1>,
    #[serde(default = "default_collaboration_generation")]
    pub collaboration_generation: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collaboration_admissions: Vec<CollaborationAdmissionDecisionV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collaboration_reliability: Vec<ReliabilityObservationV1>,
    pub version: u64,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
}

pub fn collaboration_policy_uncertainty(
    project: &ProjectV1,
    work_item_id: &WorkItemId,
) -> UncertaintyClassV1 {
    let applies_to_work = |candidate: Option<&WorkItemId>| {
        candidate.is_none_or(|candidate| candidate == work_item_id)
    };
    if project.blockers.iter().any(|blocker| {
        applies_to_work(blocker.work_item_id.as_ref()) && blocker.state == BlockerStateV1::Escalated
    }) {
        return UncertaintyClassV1::Blocking;
    }
    if project.blockers.iter().any(|blocker| {
        applies_to_work(blocker.work_item_id.as_ref()) && blocker.state == BlockerStateV1::Open
    }) || project.questions.iter().any(|question| {
        applies_to_work(question.work_item_id.as_ref()) && question.resolution_ref.is_none()
    }) {
        return UncertaintyClassV1::Material;
    }
    UncertaintyClassV1::Low
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompanyWorkflowCommandV1 {
    SubmitCustomerRequest {
        summary_ref: String,
        desired_outcome: String,
        constraints: Vec<String>,
    },
    ClarifyCustomerRequest {
        request_id: String,
        expected_version: u64,
        question_ref: String,
        answer_ref: String,
    },
    QualifyCustomerRequest {
        request_id: String,
        expected_version: u64,
        reason_ref: String,
    },
    CreateProposal {
        request_id: String,
        expected_version: u64,
        binding: ProposalBindingV1,
    },
    AcceptProposal {
        request_id: String,
        expected_version: u64,
        proposal_id: String,
        proposal_digest: String,
    },
    RejectProposal {
        request_id: String,
        expected_version: u64,
        proposal_id: String,
        proposal_digest: String,
        reason_ref: String,
    },
    CancelCustomerRequest {
        request_id: String,
        expected_version: u64,
        reason_ref: String,
    },
    RecordCustomerFeedback {
        request_id: String,
        expected_version: u64,
        feedback_ref: String,
    },
    CreateGovernedRework {
        project_id: ProjectId,
        expected_version: u64,
        source_candidate_digest: String,
        feedback_digest: String,
        source_delivery_id: String,
    },
    PlanWorkGraph {
        project_id: ProjectId,
        expected_version: u64,
        items: Vec<CompanyWorkItemSpecV1>,
    },
    ActivateProject {
        project_id: ProjectId,
        expected_version: u64,
        reason_ref: String,
    },
    AssignWork {
        project_id: ProjectId,
        expected_version: u64,
        work_item_id: WorkItemId,
        agent_id: AgentId,
        organization_generation: u64,
        organization_digest: String,
        reason_ref: String,
    },
    ReassignWork {
        project_id: ProjectId,
        expected_version: u64,
        work_item_id: WorkItemId,
        expected_assignment_version: u64,
        agent_id: AgentId,
        organization_generation: u64,
        organization_digest: String,
        reason_ref: String,
    },
    DelegateWork {
        project_id: ProjectId,
        expected_version: u64,
        work_item_id: WorkItemId,
        expected_assignment_version: u64,
        delegate: AgentId,
        reason_ref: String,
    },
    ApplyWorkTransition {
        project_id: ProjectId,
        expected_version: u64,
        receipt: WorkTransitionReceiptV1,
    },
    RecordDecision {
        project_id: ProjectId,
        expected_version: u64,
        work_item_id: Option<WorkItemId>,
        choice_ref: String,
        rationale_ref: String,
    },
    CreateHandoff {
        project_id: ProjectId,
        expected_version: u64,
        work_item_id: WorkItemId,
        consumer: AgentId,
        artifact_digests: BTreeSet<String>,
        reason_ref: String,
    },
    AcknowledgeHandoff {
        project_id: ProjectId,
        expected_version: u64,
        handoff_id: String,
        accepted: bool,
        reason_ref: String,
    },
    RaiseBlocker {
        project_id: ProjectId,
        expected_version: u64,
        work_item_id: Option<WorkItemId>,
        cause_ref: String,
        owner: AgentId,
    },
    EscalateBlocker {
        project_id: ProjectId,
        expected_version: u64,
        blocker_id: String,
        escalation_target: AgentId,
        reason_ref: String,
    },
    ResolveBlocker {
        project_id: ProjectId,
        expected_version: u64,
        blocker_id: String,
        resolution_ref: String,
    },
    RecordApproval {
        project_id: ProjectId,
        expected_version: u64,
        work_item_id: WorkItemId,
        subject_digest: String,
        approved: bool,
    },
    ReserveCost {
        project_id: ProjectId,
        expected_version: u64,
        work_item_id: Option<WorkItemId>,
        provider: String,
        amount_micros: u64,
    },
    CommitCost {
        project_id: ProjectId,
        expected_version: u64,
        reservation_id: String,
        actual_micros: u64,
        /// `llm_usage_<request-id>` operation of the immutable provider-usage event.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage_event_operation_id: Option<String>,
    },
    ReleaseCost {
        project_id: ProjectId,
        expected_version: u64,
        reservation_id: String,
        reason_ref: String,
    },
    CreateProjectRoom {
        project_id: ProjectId,
        expected_version: u64,
        kind: ProjectRoomKindV1,
        members: Vec<AgentId>,
    },
    RecordQuestion {
        project_id: ProjectId,
        expected_version: u64,
        work_item_id: Option<WorkItemId>,
        owner: AgentId,
        question_ref: String,
    },
    ResolveQuestion {
        project_id: ProjectId,
        expected_version: u64,
        question_id: String,
        resolution_ref: String,
    },
    RecordAction {
        project_id: ProjectId,
        expected_version: u64,
        work_item_id: Option<WorkItemId>,
        owner: AgentId,
        action_ref: String,
    },
    ResolveAction {
        project_id: ProjectId,
        expected_version: u64,
        action_id: String,
        resolution_ref: String,
    },
    CreateCollaborationSession {
        project_id: ProjectId,
        expected_version: u64,
        work_item_id: Option<WorkItemId>,
        admission_id: String,
        admission_contract_digest: String,
        collaboration_generation: u64,
        authority: CollaborationAuthorityFenceV1,
        subject_ref: String,
        input_digest: String,
        mode: CollaborationModeV1,
        budget: CollaborationBudgetV1,
        participants: Vec<CollaborationParticipantV1>,
    },
    RecordIndependentClaim {
        project_id: ProjectId,
        expected_version: u64,
        session_id: String,
        expected_transition_sequence: u64,
        authority: CollaborationAuthorityFenceV1,
        conclusion_ref: String,
        evidence: Vec<EvidenceReferenceV1>,
        assumptions: Vec<String>,
        uncertainty: UncertaintyClassV1,
        confidence_basis: String,
        capability_snapshot_digest: String,
        input_digest: String,
    },
    OpenClaimExposureBarrier {
        project_id: ProjectId,
        expected_version: u64,
        session_id: String,
        expected_transition_sequence: u64,
        authority: CollaborationAuthorityFenceV1,
        reason_ref: String,
    },
    OfferHandoffPacket {
        project_id: ProjectId,
        expected_version: u64,
        session_id: String,
        expected_transition_sequence: u64,
        authority: CollaborationAuthorityFenceV1,
        work_item_id: WorkItemId,
        consumer: AgentId,
        objective_ref: String,
        authority_scope_ref: String,
        authority_scope_digest: String,
        input_digests: BTreeSet<String>,
        artifact_digests: BTreeSet<String>,
        evidence: Vec<EvidenceReferenceV1>,
        assumptions: Vec<String>,
        unresolved_questions: Vec<String>,
        uncertainty: UncertaintyClassV1,
        acceptance_checks: Vec<String>,
        required_capabilities: BTreeSet<String>,
        privacy_classes: BTreeSet<String>,
    },
    RequestHandoffClarification {
        project_id: ProjectId,
        expected_version: u64,
        session_id: String,
        expected_transition_sequence: u64,
        authority: CollaborationAuthorityFenceV1,
        packet_id: String,
        packet_digest: String,
        gap_class: HandoffGapClassV1,
        question_ref: String,
        basis_digest: String,
    },
    AnswerHandoffClarification {
        project_id: ProjectId,
        expected_version: u64,
        session_id: String,
        expected_transition_sequence: u64,
        authority: CollaborationAuthorityFenceV1,
        packet_id: String,
        packet_digest: String,
        clarification_id: String,
        question_generation: u16,
        answer_ref: String,
        new_information_digest: String,
    },
    AcceptHandoffPacket {
        project_id: ProjectId,
        expected_version: u64,
        session_id: String,
        expected_transition_sequence: u64,
        authority: CollaborationAuthorityFenceV1,
        packet_id: String,
        packet_digest: String,
        capability_snapshot_digest: String,
        reason_ref: String,
    },
    RejectHandoffPacket {
        project_id: ProjectId,
        expected_version: u64,
        session_id: String,
        expected_transition_sequence: u64,
        authority: CollaborationAuthorityFenceV1,
        packet_id: String,
        packet_digest: String,
        reason_ref: String,
    },
    ConsumeHandoffPacket {
        project_id: ProjectId,
        expected_version: u64,
        session_id: String,
        expected_transition_sequence: u64,
        authority: CollaborationAuthorityFenceV1,
        packet_id: String,
        packet_digest: String,
        kind: HandoffConsumptionKindV1,
        subject_id: String,
        subject_digest: String,
    },
    RecordDissent {
        project_id: ProjectId,
        expected_version: u64,
        session_id: String,
        expected_transition_sequence: u64,
        authority: CollaborationAuthorityFenceV1,
        decision_id: String,
        claim_id: Option<String>,
        rationale_ref: String,
        evidence: Vec<EvidenceReferenceV1>,
        residual_risk_ref: String,
    },
    LinkDecisionEvidence {
        project_id: ProjectId,
        expected_version: u64,
        session_id: String,
        expected_transition_sequence: u64,
        authority: CollaborationAuthorityFenceV1,
        decision_id: String,
        claim_ids: BTreeSet<String>,
        dissent_ids: BTreeSet<String>,
    },
    TransitionCollaborationSession {
        project_id: ProjectId,
        expected_version: u64,
        session_id: String,
        expected_transition_sequence: u64,
        authority: CollaborationAuthorityFenceV1,
        target: CollaborationSessionStateV1,
        reason_ref: String,
    },
    AdmitCollaboration {
        project_id: ProjectId,
        expected_version: u64,
        source_request_digest: String,
        input: CollaborationAdmissionInputV1,
        candidates: Vec<CollaborationCandidateV1>,
        reliability: Vec<ReliabilityObservationV1>,
        expected_benefit_ref: String,
    },
    ProgressCollaborationAdmission {
        project_id: ProjectId,
        expected_version: u64,
        source_request_digest: String,
        admission_id: String,
        fence: CollaborationAdmissionFenceV1,
        progress: CollaborationProgressV1,
    },
    RecordCollaborationReliability {
        project_id: ProjectId,
        expected_version: u64,
        work_item_id: WorkItemId,
        fence: CollaborationAdmissionFenceV1,
        observation: ReliabilityObservationV1,
    },
}

const fn default_collaboration_generation() -> u64 {
    1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkTransitionReceiptV1 {
    pub schema_version: u16,
    pub project_id: ProjectId,
    pub work_item_id: WorkItemId,
    pub expected_project_version: u64,
    pub expected_work_version: u64,
    pub expected_assignment_version: u64,
    pub from_state: CompanyWorkStateV1,
    pub to_state: CompanyWorkStateV1,
    pub output_receipts: Vec<WorkOutputReceiptV1>,
    pub gate_receipt: Option<QualityGateReceiptBindingV1>,
    pub phase_a_evidence_digest: String,
    pub reason_ref: String,
    pub occurred_at_unix_ms: u64,
}

impl CompanyWorkflowCommandV1 {
    pub fn canonical_digest(&self) -> Result<String, WorkflowError> {
        canonical_sha256("sentinel.workflow.company-command.v1", self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "result",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum CompanyWorkflowResponseV1 {
    CustomerRequest(CustomerRequestV1),
    Proposal(ProposalV1),
    AgreementProject {
        agreement: Box<AgreementV1>,
        project: Box<ProjectV1>,
    },
    Project(Box<ProjectV1>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompanyCommandOutcomeV1 {
    pub replayed: bool,
    pub response: CompanyWorkflowResponseV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectProjectionV1 {
    pub schema_version: u16,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub source_sequence: u64,
    pub project: ProjectV1,
    pub projection_digest: String,
}

/// Validated protected read model for one durable project-domain event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompanyProjectEventViewV1 {
    pub sequence: u64,
    pub event_id: String,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub event_type: String,
    pub operation_id: Uuid,
    pub principal_id: String,
    pub principal_kind: CompanyPrincipalKindV1,
    pub principal_role: CompanyRoleV1,
    pub created_at_unix_ms: u64,
    pub project: ProjectV1,
}

pub(crate) fn validate_work_graph(items: &[CompanyWorkItemSpecV1]) -> Result<(), WorkflowError> {
    if items.is_empty() || items.len() > MAX_COMPANY_ITEMS {
        return Err(invalid("work graph size is invalid"));
    }
    let mut by_id = BTreeMap::new();
    for item in items {
        item.validate()?;
        if by_id.insert(item.work_item_id.clone(), item).is_some() {
            return Err(invalid("work graph contains duplicate work item ids"));
        }
    }
    for item in items {
        if item.dependency_ids.iter().any(|id| !by_id.contains_key(id)) {
            return Err(invalid("work graph references an unknown dependency"));
        }
        if item.outputs.is_empty()
            || item.dependency_ids.iter().any(|dependency| {
                !item
                    .inputs
                    .iter()
                    .any(|input| input.producer_work_item_id == *dependency)
            })
        {
            return Err(invalid(
                "work graph contains an unreachable data dependency",
            ));
        }
        for input in &item.inputs {
            let producer = by_id
                .get(&input.producer_work_item_id)
                .ok_or_else(|| invalid("work input producer is unknown"))?;
            let output = producer
                .outputs
                .iter()
                .find(|output| output.name == input.producer_output_name)
                .ok_or_else(|| invalid("work input producer output is undeclared"))?;
            if input.expected_contract_generation != output.contract_generation
                || input.expected_contract_digest != output.contract_digest
            {
                return Err(invalid("work input binding is stale"));
            }
        }
    }
    fn visit(
        id: &WorkItemId,
        by_id: &BTreeMap<WorkItemId, &CompanyWorkItemSpecV1>,
        visiting: &mut BTreeSet<WorkItemId>,
        visited: &mut BTreeSet<WorkItemId>,
    ) -> Result<(), WorkflowError> {
        if visited.contains(id) {
            return Ok(());
        }
        if !visiting.insert(id.clone()) {
            return Err(invalid("work graph contains a dependency cycle"));
        }
        for dependency in &by_id[id].dependency_ids {
            visit(dependency, by_id, visiting, visited)?;
        }
        visiting.remove(id);
        visited.insert(id.clone());
        Ok(())
    }
    let mut visited = BTreeSet::new();
    for id in by_id.keys() {
        visit(id, &by_id, &mut BTreeSet::new(), &mut visited)?;
    }
    Ok(())
}

pub(crate) fn stable_domain_id(
    prefix: &str,
    tenant_id: &TenantId,
    operation_id: Uuid,
) -> Result<String, WorkflowError> {
    let digest = canonical_sha256(
        "sentinel.workflow.company-id.v1",
        &(prefix, &tenant_id.0, operation_id),
    )?;
    Ok(format!("{prefix}-{}", &digest[..24]))
}

pub(crate) fn validate_text(value: &str) -> Result<(), WorkflowError> {
    if value.is_empty()
        || value.len() > MAX_TEXT_BYTES
        || value.chars().any(|character| character.is_control())
    {
        return Err(invalid("bounded workflow text is invalid"));
    }
    Ok(())
}

pub(crate) fn validate_text_collection(
    values: &[String],
    required: bool,
) -> Result<(), WorkflowError> {
    if (required && values.is_empty()) || values.len() > MAX_COLLECTION_ITEMS {
        return Err(invalid("bounded workflow collection is invalid"));
    }
    let mut unique = BTreeSet::new();
    for value in values {
        validate_text(value)?;
        if !unique.insert(value) {
            return Err(invalid("bounded workflow collection contains duplicates"));
        }
    }
    Ok(())
}

pub(crate) fn invalid(message: &'static str) -> WorkflowError {
    WorkflowError::new(WorkflowErrorCode::InvalidInput, false, message)
}

pub(crate) fn unauthorized() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::AuthorityConflict,
        false,
        "company principal is not authorized",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OTHER_DIGEST: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn profile(id: &str, digest: &str) -> WorkProfileBindingV1 {
        WorkProfileBindingV1 {
            profile_id: id.to_owned(),
            generation: 1,
            digest: digest.to_owned(),
        }
    }

    fn participant(id: u16, profile: WorkProfileBindingV1) -> ParticipantBindingV1 {
        ParticipantBindingV1 {
            agent_id: AgentId(id),
            principal_id: format!("agent-{id}"),
            role: CompanyRoleV1::Developer,
            specialties: BTreeSet::from(["rust".to_owned()]),
            reports_to: None,
            profile,
        }
    }

    fn work(id: &str, dependency: Option<&str>) -> CompanyWorkItemSpecV1 {
        CompanyWorkItemSpecV1 {
            work_item_id: WorkItemId::parse(id).unwrap(),
            title: format!("title-{id}"),
            objective: format!("objective-{id}"),
            required_role: CompanyRoleV1::Developer,
            required_specialties: BTreeSet::from(["rust".to_owned()]),
            dependency_ids: dependency
                .map(|value| BTreeSet::from([WorkItemId::parse(value).unwrap()]))
                .unwrap_or_default(),
            owner: AgentId(1),
            inputs: dependency
                .map(|value| {
                    vec![WorkInputContractV1 {
                        name: format!("input-{value}"),
                        producer_work_item_id: WorkItemId::parse(value).unwrap(),
                        producer_output_name: "result".to_owned(),
                        expected_contract_generation: 1,
                        expected_contract_digest: DIGEST.to_owned(),
                    }]
                })
                .unwrap_or_default(),
            outputs: vec![WorkOutputContractV1 {
                name: "result".to_owned(),
                media_type: "application/octet-stream".to_owned(),
                digest_algorithm: "sha256".to_owned(),
                contract_generation: 1,
                contract_digest: DIGEST.to_owned(),
            }],
            quality_gate: QualityGateBindingV1 {
                gate_id: "web-work-item-qa-v1".to_owned(),
                generation: 1,
                digest: DIGEST.to_owned(),
            },
            budget_micros: 1,
            rework: None,
        }
    }

    #[test]
    fn shared_profile_requires_one_exact_generation_and_digest_binding() {
        let shared = profile("developer-v1", DIGEST);
        let mut governance = ProposalGovernanceV1 {
            owner: AgentId(1),
            participants: vec![participant(1, shared.clone()), participant(2, shared)],
            project_profile: profile("project-v1", DIGEST),
        };
        assert!(governance.validate().is_ok());
        governance.participants[1].profile.digest = OTHER_DIGEST.to_owned();
        assert_eq!(
            governance.validate().unwrap_err().code,
            WorkflowErrorCode::InvalidInput
        );
    }

    #[test]
    fn data_dag_rejects_missing_outputs_undeclared_producers_and_stale_bindings() {
        let producer = work("producer", None);
        let consumer = work("consumer", Some("producer"));
        assert!(validate_work_graph(&[producer.clone(), consumer.clone()]).is_ok());

        let mut missing_output = producer.clone();
        missing_output.outputs.clear();
        assert_eq!(
            validate_work_graph(&[missing_output, consumer.clone()])
                .unwrap_err()
                .code,
            WorkflowErrorCode::InvalidInput
        );

        let mut undeclared = consumer.clone();
        undeclared.inputs[0].producer_output_name = "unknown".to_owned();
        assert_eq!(
            validate_work_graph(&[producer.clone(), undeclared])
                .unwrap_err()
                .code,
            WorkflowErrorCode::InvalidInput
        );

        let mut stale = consumer;
        stale.inputs[0].expected_contract_digest = OTHER_DIGEST.to_owned();
        assert_eq!(
            validate_work_graph(&[producer, stale]).unwrap_err().code,
            WorkflowErrorCode::InvalidInput
        );
    }
}
