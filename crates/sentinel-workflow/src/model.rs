use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

pub use sentinel_common::AgentId;

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(prefix: &str) -> Self {
                Self(format!("{prefix}-{}", uuid::Uuid::now_v7()))
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

string_id!(CustomerRequestId);
string_id!(ProposalId);
string_id!(AgreementId);
string_id!(ProjectId);
string_id!(WorkItemId);
string_id!(AssignmentId);
string_id!(DecisionId);
string_id!(HandoffId);
string_id!(BlockerId);
string_id!(ApprovalId);
string_id!(EvidenceId);
string_id!(CostReservationId);
string_id!(ProjectRoomId);
string_id!(ActionItemId);
string_id!(QuestionId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorRole {
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

impl ActorRole {
    pub fn is_internal(self) -> bool {
        !matches!(self, Self::Customer)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthenticatedActor {
    pub actor_id: String,
    pub role: ActorRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomerRequestState {
    Submitted,
    Clarifying,
    Qualified,
    Proposed,
    Accepted,
    Rejected,
    Expired,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clarification {
    pub question_ref: String,
    pub answer_ref: String,
    pub recorded_by: String,
    pub recorded_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomerFeedback {
    pub feedback_ref: String,
    pub recorded_by: String,
    pub recorded_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomerRequest {
    pub schema_version: u32,
    pub id: CustomerRequestId,
    pub customer_id: String,
    pub summary_ref: String,
    pub desired_outcome: String,
    pub constraints: Vec<String>,
    pub clarifications: Vec<Clarification>,
    pub feedback: Vec<CustomerFeedback>,
    pub state: CustomerRequestState,
    pub version: u64,
    pub proposal_ids: Vec<ProposalId>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalBinding {
    pub scope: String,
    pub deliverables: Vec<String>,
    pub exclusions: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub assumptions: Vec<String>,
    pub cost_ceiling_micros: u64,
    /// Immutable per-provider admission ceilings. Callers cannot supply or
    /// raise these limits when reserving cost.
    pub provider_cost_ceilings_micros: BTreeMap<String, u64>,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub schema_version: u32,
    pub id: ProposalId,
    pub customer_request_id: CustomerRequestId,
    pub generation: u32,
    pub binding: ProposalBinding,
    pub digest: String,
    pub created_by: String,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agreement {
    pub schema_version: u32,
    pub id: AgreementId,
    pub customer_request_id: CustomerRequestId,
    pub proposal_id: ProposalId,
    pub proposal_digest: String,
    pub proposal_binding: ProposalBinding,
    pub customer_id: String,
    pub accepted_by: String,
    pub accepted_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectState {
    Planned,
    Active,
    Blocked,
    DeliveryCandidate,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub schema_version: u32,
    pub id: ProjectId,
    pub agreement_id: AgreementId,
    pub agreement_digest: String,
    pub profile_id: String,
    pub owner: AgentId,
    pub participants: Vec<AgentId>,
    pub cost_ceiling_micros: u64,
    pub provider_cost_ceilings_micros: BTreeMap<String, u64>,
    pub reserved_cost_micros: u64,
    pub committed_cost_micros: u64,
    pub state: ProjectState,
    pub version: u64,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemState {
    Proposed,
    Ready,
    Assigned,
    Claimed,
    InProgress,
    InReview,
    Done,
    Blocked,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItemSpec {
    pub id: WorkItemId,
    pub title: String,
    pub objective: String,
    pub owner: AgentId,
    pub required_role: ActorRole,
    pub required_capabilities: BTreeSet<String>,
    pub dependency_ids: BTreeSet<WorkItemId>,
    pub input_refs: Vec<String>,
    pub required_output_kinds: BTreeSet<String>,
    pub quality_gate: String,
    pub budget_micros: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItem {
    pub schema_version: u32,
    pub project_id: ProjectId,
    #[serde(flatten)]
    pub spec: WorkItemSpec,
    pub assignee: Option<AgentId>,
    pub assignment_version: u64,
    pub state: WorkItemState,
    pub version: u64,
    pub output_refs: BTreeMap<String, String>,
    pub completion_evidence_id: Option<EvidenceId>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfile {
    pub agent_id: AgentId,
    pub role: ActorRole,
    pub capabilities: BTreeSet<String>,
    pub reports_to: Option<AgentId>,
    pub active: bool,
    pub current_assignments: u32,
    pub max_assignments: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assignment {
    pub schema_version: u32,
    pub id: AssignmentId,
    pub project_id: ProjectId,
    pub work_item_id: WorkItemId,
    pub assignee: AgentId,
    /// Immutable policy input used for this assignment decision.
    pub assignee_profile: AgentProfile,
    pub assigned_by: String,
    pub assignment_version: u64,
    pub reason: String,
    pub active: bool,
    pub created_at_ms: u64,
    pub revoked_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionEvidence {
    pub schema_version: u32,
    pub id: EvidenceId,
    pub project_id: ProjectId,
    pub work_item_id: WorkItemId,
    pub assignment_version: u64,
    pub output_refs: BTreeMap<String, String>,
    pub gate_id: String,
    pub gate_passed: bool,
    pub recorded_by: String,
    pub recorded_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectDecision {
    pub schema_version: u32,
    pub id: DecisionId,
    pub project_id: ProjectId,
    pub work_item_id: Option<WorkItemId>,
    pub choice: String,
    pub alternatives: Vec<String>,
    pub rationale_ref: String,
    pub evidence_refs: Vec<String>,
    pub decided_by: String,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffState {
    Offered,
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handoff {
    pub schema_version: u32,
    pub id: HandoffId,
    pub project_id: ProjectId,
    pub work_item_id: WorkItemId,
    pub producer: AgentId,
    pub consumer: AgentId,
    pub artifact_digests: BTreeSet<String>,
    pub state: HandoffState,
    pub reason: String,
    pub created_at_ms: u64,
    pub acknowledged_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockerState {
    Open,
    Escalated,
    Resolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blocker {
    pub schema_version: u32,
    pub id: BlockerId,
    pub project_id: ProjectId,
    pub work_item_id: Option<WorkItemId>,
    pub cause_ref: String,
    pub impact: String,
    pub owner: AgentId,
    pub required_resolution_role: ActorRole,
    pub escalation_target: Option<AgentId>,
    pub state: BlockerState,
    pub resolution_ref: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    Approved,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub schema_version: u32,
    pub id: ApprovalId,
    pub project_id: ProjectId,
    pub work_item_id: Option<WorkItemId>,
    pub gate_id: String,
    pub subject_digest: String,
    pub state: ApprovalState,
    pub actor_id: String,
    pub actor_role: ActorRole,
    pub reason: String,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostReservation {
    pub schema_version: u32,
    pub id: CostReservationId,
    pub project_id: ProjectId,
    pub work_item_id: Option<WorkItemId>,
    pub provider: String,
    pub amount_micros: u64,
    pub committed_micros: Option<u64>,
    pub created_by: String,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectRoomKind {
    Project,
    Team,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRoom {
    pub schema_version: u32,
    pub id: ProjectRoomId,
    pub project_id: ProjectId,
    pub kind: ProjectRoomKind,
    pub team_ref: Option<String>,
    pub members: Vec<AgentId>,
    pub created_by: String,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionItemState {
    Open,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionItem {
    pub schema_version: u32,
    pub id: ActionItemId,
    pub project_id: ProjectId,
    pub work_item_id: Option<WorkItemId>,
    pub owner: AgentId,
    pub action_ref: String,
    pub due_at_ms: Option<u64>,
    pub state: ActionItemState,
    pub resolution_ref: Option<String>,
    pub created_by: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectQuestionState {
    Open,
    Resolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectQuestion {
    pub schema_version: u32,
    pub id: QuestionId,
    pub project_id: ProjectId,
    pub work_item_id: Option<WorkItemId>,
    pub question_ref: String,
    pub owner: AgentId,
    pub state: ProjectQuestionState,
    pub resolution_ref: Option<String>,
    pub created_by: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowEventType {
    CustomerRequestSubmitted,
    CustomerRequestClarified,
    CustomerRequestQualified,
    ProposalCreated,
    CustomerRequestRejected,
    CustomerRequestCancelled,
    CustomerFeedbackRecorded,
    AgreementAccepted,
    ProjectCreated,
    WorkGraphPlanned,
    WorkAssigned,
    WorkClaimed,
    WorkExecutionRequested,
    WorkExecutionStarted,
    WorkCompleted,
    DecisionRecorded,
    HandoffCreated,
    HandoffAcknowledged,
    BlockerRaised,
    BlockerEscalated,
    BlockerResolved,
    ApprovalRecorded,
    CostReserved,
    CostCommitted,
    BudgetExhausted,
    ProjectRoomCreated,
    ActionItemRecorded,
    ActionItemResolved,
    QuestionRecorded,
    QuestionResolved,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowEvent {
    pub sequence: i64,
    pub schema_version: u32,
    pub event_id: String,
    pub event_type: WorkflowEventType,
    pub aggregate_type: String,
    pub aggregate_id: String,
    pub actor_id: String,
    pub actor_role: ActorRole,
    pub operation_id: String,
    pub operation_digest: String,
    pub before_state: Option<String>,
    pub after_state: Option<String>,
    pub reason: String,
    pub payload: serde_json::Value,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectProjection {
    pub schema_version: u32,
    pub project_id: ProjectId,
    pub agreement: Agreement,
    pub state: ProjectState,
    pub work_items_total: u32,
    pub work_items_by_state: BTreeMap<String, u32>,
    pub participants: Vec<AgentId>,
    pub work_items: Vec<WorkItem>,
    pub assignments: Vec<Assignment>,
    pub completion_evidence: Vec<CompletionEvidence>,
    pub decisions: Vec<ProjectDecision>,
    pub handoffs: Vec<Handoff>,
    pub blockers: Vec<Blocker>,
    pub approvals: Vec<Approval>,
    pub rooms: Vec<ProjectRoom>,
    pub action_items: Vec<ActionItem>,
    pub unresolved_questions: Vec<ProjectQuestion>,
    pub open_blockers: u32,
    pub open_action_items: u32,
    pub open_questions: u32,
    pub cost_ceiling_micros: u64,
    pub reserved_cost_micros: u64,
    pub committed_cost_micros: u64,
    pub last_event_sequence: i64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum WorkflowCommand {
    SubmitCustomerRequest {
        customer_id: String,
        summary_ref: String,
        desired_outcome: String,
        constraints: Vec<String>,
    },
    ClarifyCustomerRequest {
        request_id: CustomerRequestId,
        expected_version: u64,
        question_ref: String,
        answer_ref: String,
    },
    QualifyCustomerRequest {
        request_id: CustomerRequestId,
        expected_version: u64,
        reason: String,
    },
    CreateProposal {
        request_id: CustomerRequestId,
        expected_version: u64,
        binding: ProposalBinding,
    },
    AcceptProposal {
        request_id: CustomerRequestId,
        expected_version: u64,
        proposal_id: ProposalId,
        proposal_digest: String,
        profile_id: String,
        project_owner: AgentId,
        participants: Vec<AgentId>,
    },
    RejectProposal {
        request_id: CustomerRequestId,
        expected_version: u64,
        proposal_id: ProposalId,
        proposal_digest: String,
        reason_ref: String,
    },
    CancelCustomerRequest {
        request_id: CustomerRequestId,
        expected_version: u64,
        reason_ref: String,
    },
    RecordCustomerFeedback {
        request_id: CustomerRequestId,
        feedback_ref: String,
    },
    PlanWorkGraph {
        project_id: ProjectId,
        expected_version: u64,
        items: Vec<WorkItemSpec>,
    },
    AssignWork {
        work_item_id: WorkItemId,
        expected_version: u64,
        assignee: AgentProfile,
        reason: String,
    },
    ClaimWork {
        work_item_id: WorkItemId,
        expected_version: u64,
        agent_id: AgentId,
        input_digest: String,
        deadline_ms: u64,
    },
    CompleteWork {
        work_item_id: WorkItemId,
        expected_version: u64,
        assignment_version: u64,
        output_refs: BTreeMap<String, String>,
        gate_id: String,
        gate_passed: bool,
    },
    RecordDecision {
        project_id: ProjectId,
        work_item_id: Option<WorkItemId>,
        choice: String,
        alternatives: Vec<String>,
        rationale_ref: String,
        evidence_refs: Vec<String>,
    },
    CreateHandoff {
        project_id: ProjectId,
        work_item_id: WorkItemId,
        producer: AgentId,
        consumer: AgentId,
        artifact_digests: BTreeSet<String>,
        reason: String,
    },
    AcknowledgeHandoff {
        handoff_id: HandoffId,
        accepted: bool,
        reason: String,
    },
    RaiseBlocker {
        project_id: ProjectId,
        work_item_id: Option<WorkItemId>,
        cause_ref: String,
        impact: String,
        owner: AgentId,
        required_resolution_role: ActorRole,
    },
    EscalateBlocker {
        blocker_id: BlockerId,
        escalation_target: AgentId,
        reason: String,
    },
    ResolveBlocker {
        blocker_id: BlockerId,
        resolution_ref: String,
    },
    RecordApproval {
        project_id: ProjectId,
        work_item_id: Option<WorkItemId>,
        gate_id: String,
        subject_digest: String,
        approved: bool,
        reason: String,
    },
    ReserveCost {
        project_id: ProjectId,
        work_item_id: Option<WorkItemId>,
        provider: String,
        amount_micros: u64,
    },
    CommitCost {
        reservation_id: CostReservationId,
        actual_micros: u64,
    },
    CreateProjectRoom {
        project_id: ProjectId,
        kind: ProjectRoomKind,
        team_ref: Option<String>,
        members: Vec<AgentId>,
    },
    RecordActionItem {
        project_id: ProjectId,
        work_item_id: Option<WorkItemId>,
        owner: AgentId,
        action_ref: String,
        due_at_ms: Option<u64>,
    },
    ResolveActionItem {
        action_item_id: ActionItemId,
        completed: bool,
        resolution_ref: String,
    },
    RecordQuestion {
        project_id: ProjectId,
        work_item_id: Option<WorkItemId>,
        owner: AgentId,
        question_ref: String,
    },
    ResolveQuestion {
        question_id: QuestionId,
        resolution_ref: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", content = "value", rename_all = "snake_case")]
pub enum WorkflowResponse {
    CustomerRequest(CustomerRequest),
    Proposal(Proposal),
    AgreementProject {
        agreement: Agreement,
        project: Project,
    },
    Project(Project),
    WorkItems(Vec<WorkItem>),
    Assignment(Assignment),
    WorkItem(WorkItem),
    Decision(ProjectDecision),
    Handoff(Handoff),
    Blocker(Blocker),
    Approval(Approval),
    CostReservation(CostReservation),
    ProjectRoom(ProjectRoom),
    ActionItem(ActionItem),
    ProjectQuestion(ProjectQuestion),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandOutcome {
    pub replayed: bool,
    pub response: WorkflowResponse,
}
