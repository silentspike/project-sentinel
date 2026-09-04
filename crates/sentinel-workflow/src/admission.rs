use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::collaboration::{BehaviorMandateV1, UncertaintyClassV1};
use crate::digest::canonical_sha256;
use crate::domain::{validate_text, CompanyRoleV1};
use crate::model::{validate_digest, validate_identifier};
use crate::{AgentId, ProjectId, TenantId, WorkItemId, WorkflowError, WorkflowErrorCode};

pub const COLLABORATION_ADMISSION_SCHEMA_VERSION: u16 = 1;
pub const COLLABORATION_POLICY_WINDOW_MS: u64 = 300_000;
pub const COLLABORATION_POLICY_MAX_ROUNDS: u16 = 4;
pub const COLLABORATION_POLICY_MAX_TOKENS: u64 = 32_000;
pub const COLLABORATION_POLICY_MAX_PARTICIPANTS: u16 = 4;
pub const COLLABORATION_POLICY_MINIMUM_NOVELTY_MICROS: u32 = 100_000;
pub const COLLABORATION_POLICY_MAX_STALLED_UPDATES: u16 = 2;
pub const COLLABORATION_POLICY_QUALITY_TOLERANCE_MICROS: u32 = 100_000;

const SCORE_SCALE: i64 = 1_000_000;
const MAX_ADMISSION_CANDIDATES: usize = 16;
const MAX_ADMISSION_ITEMS: usize = 64;
const MAX_ADMISSION_PROGRESS_UPDATES: u16 = 64;

pub const fn collaboration_policy_mandate(role: CompanyRoleV1) -> BehaviorMandateV1 {
    match role {
        CompanyRoleV1::Customer => BehaviorMandateV1::Escalate,
        CompanyRoleV1::Sales => BehaviorMandateV1::Discover,
        CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead => {
            BehaviorMandateV1::Synthesize
        }
        CompanyRoleV1::Designer | CompanyRoleV1::Developer => BehaviorMandateV1::Implement,
        CompanyRoleV1::Qa => BehaviorMandateV1::Challenge,
        CompanyRoleV1::ReleaseManager => BehaviorMandateV1::Verify,
        CompanyRoleV1::Gaia => BehaviorMandateV1::Escalate,
    }
}

pub fn collaboration_policy_team_shape(
    owner: AgentId,
    required_capabilities: &BTreeSet<String>,
    candidates: &[(AgentId, BTreeSet<String>)],
    required_handoff_agents: &[AgentId],
) -> Result<(bool, bool), WorkflowError> {
    if owner.0 == 0
        || required_capabilities.is_empty()
        || candidates.is_empty()
        || candidates.len() > MAX_ADMISSION_CANDIDATES
    {
        return Err(invalid("collaboration capability topology is invalid"));
    }
    let mut capabilities_by_agent = BTreeMap::new();
    for (agent_id, capabilities) in candidates {
        if agent_id.0 == 0
            || capabilities.is_empty()
            || capabilities_by_agent
                .insert(agent_id.0, capabilities)
                .is_some()
        {
            return Err(invalid("collaboration capability topology is invalid"));
        }
    }
    if !capabilities_by_agent.contains_key(&owner.0) {
        return Err(invalid("collaboration owner capability is unavailable"));
    }

    let mut selected = BTreeSet::from([owner.0]);
    for agent_id in required_handoff_agents {
        if agent_id.0 == 0
            || agent_id == &owner
            || !capabilities_by_agent.contains_key(&agent_id.0)
            || !selected.insert(agent_id.0)
        {
            return Err(invalid("required collaboration handoff is invalid"));
        }
    }
    let base_capabilities = selected
        .iter()
        .flat_map(|agent_id| capabilities_by_agent[agent_id].iter().cloned())
        .collect::<BTreeSet<_>>();
    let optional = capabilities_by_agent
        .iter()
        .filter(|(agent_id, _)| !selected.contains(agent_id))
        .collect::<Vec<_>>();
    let additional = if required_capabilities.is_subset(&base_capabilities) {
        Some(0_usize)
    } else {
        let mut smallest = None;
        for mask in 1_u32..(1_u32 << optional.len()) {
            let count = mask.count_ones() as usize;
            if smallest.is_some_and(|current| count >= current) {
                continue;
            }
            let mut covered = base_capabilities.clone();
            for (index, (_, capabilities)) in optional.iter().enumerate() {
                if mask & (1_u32 << index) != 0 {
                    covered.extend(capabilities.iter().cloned());
                }
            }
            if required_capabilities.is_subset(&covered) {
                smallest = Some(count);
            }
        }
        smallest
    };
    let required_helpers = match additional {
        Some(additional) => selected
            .len()
            .saturating_sub(1)
            .checked_add(additional)
            .ok_or_else(|| invalid("collaboration team size overflow"))?,
        None => selected.len().saturating_sub(1).max(1),
    };
    Ok((required_helpers == 1, required_helpers >= 2))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollaborationCapacitySnapshotV1 {
    pub assignment_load: BTreeMap<u16, u16>,
    pub reserved_load: BTreeMap<u16, u16>,
    pub project_reserved_cost_micros: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollaborationAdmissionModeV1 {
    Solo,
    DirectedHandoff,
    ParallelIndependentReview,
    SpecialistPanel,
    HumanEscalation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskRiskV1 {
    Low,
    Material,
    High,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReversibilityV1 {
    Reversible,
    Costly,
    Irreversible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AmbiguityClassV1 {
    Low,
    Material,
    Blocking,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeparationRequirementV1 {
    pub required_roles: BTreeSet<CompanyRoleV1>,
    pub minimum_distinct_agents: u16,
    pub owner_may_fill: bool,
}

impl SeparationRequirementV1 {
    fn validate(&self) -> Result<(), WorkflowError> {
        if self.minimum_distinct_agents == 0
            || usize::from(self.minimum_distinct_agents) > MAX_ADMISSION_CANDIDATES
            || self.required_roles.contains(&CompanyRoleV1::Customer)
        {
            return Err(invalid("collaboration separation requirement is invalid"));
        }
        Ok(())
    }
}

pub const fn collaboration_policy_role_name(role: CompanyRoleV1) -> &'static str {
    match role {
        CompanyRoleV1::Customer => "customer",
        CompanyRoleV1::Sales => "sales",
        CompanyRoleV1::ProjectManager => "project_manager",
        CompanyRoleV1::TechnicalLead => "technical_lead",
        CompanyRoleV1::Designer => "designer",
        CompanyRoleV1::Developer => "developer",
        CompanyRoleV1::Qa => "qa",
        CompanyRoleV1::ReleaseManager => "release_manager",
        CompanyRoleV1::Gaia => "gaia",
    }
}

pub fn collaboration_policy_task_risk(role: CompanyRoleV1) -> TaskRiskV1 {
    match role {
        CompanyRoleV1::Qa => TaskRiskV1::High,
        CompanyRoleV1::ReleaseManager | CompanyRoleV1::Gaia => TaskRiskV1::Critical,
        CompanyRoleV1::Sales | CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead => {
            TaskRiskV1::Material
        }
        CompanyRoleV1::Designer | CompanyRoleV1::Developer => TaskRiskV1::Low,
        CompanyRoleV1::Customer => TaskRiskV1::Critical,
    }
}

pub fn collaboration_policy_reversibility(role: CompanyRoleV1) -> ReversibilityV1 {
    match role {
        CompanyRoleV1::ReleaseManager | CompanyRoleV1::Gaia | CompanyRoleV1::Customer => {
            ReversibilityV1::Irreversible
        }
        CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead | CompanyRoleV1::Qa => {
            ReversibilityV1::Costly
        }
        CompanyRoleV1::Sales | CompanyRoleV1::Designer | CompanyRoleV1::Developer => {
            ReversibilityV1::Reversible
        }
    }
}

pub fn collaboration_policy_ambiguity(role: CompanyRoleV1) -> AmbiguityClassV1 {
    match role {
        CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead | CompanyRoleV1::Gaia => {
            AmbiguityClassV1::Material
        }
        CompanyRoleV1::Customer => AmbiguityClassV1::Blocking,
        CompanyRoleV1::Sales
        | CompanyRoleV1::Designer
        | CompanyRoleV1::Developer
        | CompanyRoleV1::Qa
        | CompanyRoleV1::ReleaseManager => AmbiguityClassV1::Low,
    }
}

pub fn collaboration_policy_separation_requirements(
    owner_role: CompanyRoleV1,
    task_risk: TaskRiskV1,
    ambiguity: AmbiguityClassV1,
    uncertainty: UncertaintyClassV1,
    evidence_conflict: bool,
) -> Vec<SeparationRequirementV1> {
    if task_risk < TaskRiskV1::High
        && ambiguity == AmbiguityClassV1::Low
        && uncertainty == UncertaintyClassV1::Low
        && !evidence_conflict
    {
        return Vec::new();
    }
    let required_roles = if owner_role == CompanyRoleV1::Qa {
        BTreeSet::from([CompanyRoleV1::ReleaseManager])
    } else {
        BTreeSet::from([CompanyRoleV1::Qa, CompanyRoleV1::ReleaseManager])
    };
    vec![SeparationRequirementV1 {
        required_roles,
        minimum_distinct_agents: 1,
        owner_may_fill: false,
    }]
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollaborationAdmissionBudgetV1 {
    pub max_participants: u16,
    pub max_rounds: u16,
    pub max_tokens: u64,
    pub max_cost_micros: u64,
    pub deadline_unix_ms: u64,
    pub minimum_novelty_micros: u32,
    pub max_stalled_updates: u16,
}

impl CollaborationAdmissionBudgetV1 {
    fn validate(&self, now_ms: u64) -> Result<(), WorkflowError> {
        if self.max_participants == 0
            || self.max_participants > COLLABORATION_POLICY_MAX_PARTICIPANTS
            || self.max_rounds == 0
            || self.max_rounds > MAX_ADMISSION_PROGRESS_UPDATES
            || self.max_stalled_updates == 0
            || self.max_stalled_updates > self.max_rounds
            || self.deadline_unix_ms <= now_ms
            || self.minimum_novelty_micros > 1_000_000
        {
            return Err(invalid("collaboration admission budget is invalid"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollaborationAdmissionInputV1 {
    pub schema_version: u16,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub work_item_id: WorkItemId,
    pub owner: AgentId,
    pub task_family: String,
    pub input_class: String,
    pub task_risk: TaskRiskV1,
    pub reversibility: ReversibilityV1,
    pub ambiguity: AmbiguityClassV1,
    pub required_capabilities: BTreeSet<String>,
    pub uncertainty: UncertaintyClassV1,
    pub evidence_conflict: bool,
    pub directed_handoff_required: bool,
    pub required_handoff_agents: Vec<AgentId>,
    pub specialist_panel_required: bool,
    pub separation_requirements: Vec<SeparationRequirementV1>,
    pub privacy_class: String,
    pub authority_conflict: bool,
    pub privacy_conflict: bool,
    pub human_approval_required: bool,
    pub remaining_cost_budget_micros: u64,
    pub remaining_time_budget_ms: u64,
    pub organization_generation: u64,
    pub organization_digest: String,
    pub assignment_id: String,
    pub assignment_version: u64,
    pub assignment_digest: String,
    pub behavior_policy_generation: u64,
    pub behavior_policy_digest: String,
    /// Remains false until #742 calibration evidence and maintainer approval exist.
    pub learned_reliability_enabled: bool,
    pub collaboration_generation: u64,
    pub quality_tolerance_micros: u32,
    pub permitted_packet_classes: BTreeSet<String>,
    pub budget: CollaborationAdmissionBudgetV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollaborationAdmissionFenceV1 {
    pub organization_generation: u64,
    pub organization_digest: String,
    pub assignment_id: String,
    pub assignment_version: u64,
    pub assignment_digest: String,
    pub behavior_policy_generation: u64,
    pub behavior_policy_digest: String,
    pub collaboration_generation: u64,
}

impl CollaborationAdmissionFenceV1 {
    pub fn validate(&self) -> Result<(), WorkflowError> {
        if self.organization_generation == 0
            || self.assignment_version == 0
            || self.behavior_policy_generation == 0
            || self.collaboration_generation == 0
        {
            return Err(invalid("collaboration admission fence is invalid"));
        }
        validate_digest(&self.organization_digest)?;
        validate_identifier(&self.assignment_id)?;
        validate_digest(&self.assignment_digest)?;
        validate_digest(&self.behavior_policy_digest)
    }
}

impl CollaborationAdmissionInputV1 {
    pub fn validate(&self, now_ms: u64) -> Result<(), WorkflowError> {
        if self.schema_version != COLLABORATION_ADMISSION_SCHEMA_VERSION
            || self.owner.0 == 0
            || self.required_capabilities.is_empty()
            || self.required_capabilities.len() > MAX_ADMISSION_ITEMS
            || self.separation_requirements.len() > MAX_ADMISSION_ITEMS
            || self.permitted_packet_classes.is_empty()
            || self.permitted_packet_classes.len() > MAX_ADMISSION_ITEMS
            || self.remaining_time_budget_ms == 0
            || self.organization_generation == 0
            || self.assignment_version == 0
            || self.behavior_policy_generation == 0
            || self.collaboration_generation == 0
            || self.quality_tolerance_micros > 1_000_000
            || (self.directed_handoff_required && self.specialist_panel_required)
            || (!self.required_handoff_agents.is_empty()
                && !self.directed_handoff_required
                && !self.specialist_panel_required)
            || self.required_handoff_agents.len() > MAX_ADMISSION_CANDIDATES
            || self.required_handoff_agents.contains(&self.owner)
            || self
                .required_handoff_agents
                .iter()
                .any(|agent_id| agent_id.0 == 0)
            || self
                .required_handoff_agents
                .windows(2)
                .any(|pair| pair[0].0 >= pair[1].0)
        {
            return Err(invalid("collaboration admission input is invalid"));
        }
        self.tenant_id.validate()?;
        self.project_id.validate()?;
        self.work_item_id.validate()?;
        validate_identifier(&self.task_family)?;
        validate_identifier(&self.input_class)?;
        validate_identifier(&self.privacy_class)?;
        validate_identifier(&self.assignment_id)?;
        validate_digest(&self.organization_digest)?;
        validate_digest(&self.assignment_digest)?;
        validate_digest(&self.behavior_policy_digest)?;
        for capability in &self.required_capabilities {
            validate_identifier(capability)?;
        }
        for packet_class in &self.permitted_packet_classes {
            validate_identifier(packet_class)?;
        }
        for requirement in &self.separation_requirements {
            requirement.validate()?;
        }
        self.budget.validate(now_ms)?;
        if self.budget.max_cost_micros > self.remaining_cost_budget_micros
            || self.budget.deadline_unix_ms - now_ms > self.remaining_time_budget_ms
        {
            return Err(invalid("collaboration admission exceeds remaining budget"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollaborationCandidateV1 {
    pub agent_id: AgentId,
    pub permanent_role: CompanyRoleV1,
    pub mandate: BehaviorMandateV1,
    pub active: bool,
    pub authority_scope_digest: String,
    pub organization_generation: u64,
    pub organization_digest: String,
    pub assignment_load: u16,
    pub assignment_limit: u16,
    pub capabilities: BTreeSet<String>,
    pub privacy_classes: BTreeSet<String>,
    pub runtime_available: bool,
    pub tools_available: bool,
    pub model_family: String,
    pub prompt_digest: String,
    pub tool_set_digest: String,
    pub data_provenance_digest: String,
    pub prior_claim_correlation_digest: Option<String>,
    pub queue_delay_ms: u64,
    pub estimated_cost_micros: u64,
}

impl CollaborationCandidateV1 {
    pub fn expected_snapshot_digest(&self) -> Result<String, WorkflowError> {
        canonical_sha256("sentinel.workflow.collaboration-candidate.v1", self)
    }

    fn validate(&self) -> Result<(), WorkflowError> {
        if self.agent_id.0 == 0
            || self.permanent_role == CompanyRoleV1::Customer
            || self.capabilities.is_empty()
            || self.capabilities.len() > MAX_ADMISSION_ITEMS
            || self.privacy_classes.len() > MAX_ADMISSION_ITEMS
            || self.organization_generation == 0
        {
            return Err(invalid("collaboration candidate is invalid"));
        }
        validate_digest(&self.authority_scope_digest)?;
        validate_digest(&self.organization_digest)?;
        validate_identifier(&self.model_family)?;
        validate_digest(&self.prompt_digest)?;
        validate_digest(&self.tool_set_digest)?;
        validate_digest(&self.data_provenance_digest)?;
        if let Some(digest) = &self.prior_claim_correlation_digest {
            validate_digest(digest)?;
        }
        for capability in &self.capabilities {
            validate_identifier(capability)?;
        }
        for class in &self.privacy_classes {
            validate_identifier(class)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollaborationSelectedParticipantV1 {
    pub agent_id: AgentId,
    pub permanent_role: CompanyRoleV1,
    pub mandate: BehaviorMandateV1,
    pub capabilities: BTreeSet<String>,
    pub privacy_classes: BTreeSet<String>,
    pub model_family: String,
    pub prompt_digest: String,
    pub tool_set_digest: String,
    pub data_provenance_digest: String,
    pub prior_claim_correlation_digest: Option<String>,
    pub candidate_snapshot_digest: String,
}

impl CollaborationSelectedParticipantV1 {
    fn from_candidate(candidate: &CollaborationCandidateV1) -> Result<Self, WorkflowError> {
        Ok(Self {
            agent_id: candidate.agent_id,
            permanent_role: candidate.permanent_role,
            mandate: candidate.mandate,
            capabilities: candidate.capabilities.clone(),
            privacy_classes: candidate.privacy_classes.clone(),
            model_family: candidate.model_family.clone(),
            prompt_digest: candidate.prompt_digest.clone(),
            tool_set_digest: candidate.tool_set_digest.clone(),
            data_provenance_digest: candidate.data_provenance_digest.clone(),
            prior_claim_correlation_digest: candidate.prior_claim_correlation_digest.clone(),
            candidate_snapshot_digest: candidate.expected_snapshot_digest()?,
        })
    }

    fn validate(&self) -> Result<(), WorkflowError> {
        if self.agent_id.0 == 0
            || self.permanent_role == CompanyRoleV1::Customer
            || self.capabilities.is_empty()
            || self.capabilities.len() > MAX_ADMISSION_ITEMS
            || self.privacy_classes.len() > MAX_ADMISSION_ITEMS
        {
            return Err(invalid("selected collaboration participant is invalid"));
        }
        for capability in &self.capabilities {
            validate_identifier(capability)?;
        }
        for class in &self.privacy_classes {
            validate_identifier(class)?;
        }
        validate_identifier(&self.model_family)?;
        validate_digest(&self.prompt_digest)?;
        validate_digest(&self.tool_set_digest)?;
        validate_digest(&self.data_provenance_digest)?;
        if let Some(digest) = &self.prior_claim_correlation_digest {
            validate_digest(digest)?;
        }
        validate_digest(&self.candidate_snapshot_digest)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReliabilityObservationV1 {
    pub observation_id: String,
    pub agent_id: AgentId,
    pub capability: String,
    pub task_family: String,
    pub input_class: String,
    pub claim_id: String,
    pub accepted_outcome_digest: String,
    pub independent_verification_digest: String,
    pub verifier_principal_id: String,
    pub verifier_authority_digest: String,
    pub accepted: bool,
    pub calibration_bucket: u16,
    pub evidence_quality_micros: u32,
    pub policy_generation: u64,
    pub observation_digest: String,
    pub recorded_at_unix_ms: u64,
}

impl ReliabilityObservationV1 {
    pub fn expected_digest(&self) -> Result<String, WorkflowError> {
        #[derive(Serialize)]
        struct Material<'a> {
            observation_id: &'a str,
            agent_id: AgentId,
            capability: &'a str,
            task_family: &'a str,
            input_class: &'a str,
            claim_id: &'a str,
            accepted_outcome_digest: &'a str,
            independent_verification_digest: &'a str,
            verifier_principal_id: &'a str,
            verifier_authority_digest: &'a str,
            accepted: bool,
            calibration_bucket: u16,
            evidence_quality_micros: u32,
            policy_generation: u64,
            recorded_at_unix_ms: u64,
        }
        canonical_sha256(
            "sentinel.workflow.reliability-observation.v1",
            &Material {
                observation_id: &self.observation_id,
                agent_id: self.agent_id,
                capability: &self.capability,
                task_family: &self.task_family,
                input_class: &self.input_class,
                claim_id: &self.claim_id,
                accepted_outcome_digest: &self.accepted_outcome_digest,
                independent_verification_digest: &self.independent_verification_digest,
                verifier_principal_id: &self.verifier_principal_id,
                verifier_authority_digest: &self.verifier_authority_digest,
                accepted: self.accepted,
                calibration_bucket: self.calibration_bucket,
                evidence_quality_micros: self.evidence_quality_micros,
                policy_generation: self.policy_generation,
                recorded_at_unix_ms: self.recorded_at_unix_ms,
            },
        )
    }

    pub fn validate(&self) -> Result<(), WorkflowError> {
        if self.agent_id.0 == 0
            || self.calibration_bucket > 100
            || self.evidence_quality_micros > 1_000_000
            || self.policy_generation == 0
            || self.recorded_at_unix_ms == 0
            || !self.accepted
        {
            return Err(invalid("reliability observation is invalid"));
        }
        validate_identifier(&self.observation_id)?;
        validate_identifier(&self.capability)?;
        validate_identifier(&self.task_family)?;
        validate_identifier(&self.input_class)?;
        validate_identifier(&self.claim_id)?;
        validate_digest(&self.accepted_outcome_digest)?;
        validate_digest(&self.independent_verification_digest)?;
        validate_identifier(&self.verifier_principal_id)?;
        validate_digest(&self.verifier_authority_digest)?;
        validate_digest(&self.observation_digest)?;
        if self.expected_digest()? != self.observation_digest {
            return Err(invalid("reliability observation digest is invalid"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionCandidateRejectionV1 {
    Inactive,
    AuthorityScope,
    StaleOrganization,
    AtLoadLimit,
    Privacy,
    Capability,
    RuntimeUnavailable,
    ToolsUnavailable,
    Cost,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionCandidateRejectionRecordV1 {
    pub agent_id: AgentId,
    pub reasons: Vec<AdmissionCandidateRejectionV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollaborationRouteV1 {
    pub from: AgentId,
    pub to: AgentId,
    pub permitted_packet_classes: BTreeSet<String>,
    pub visibility: CollaborationRouteVisibilityV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollaborationRouteVisibilityV1 {
    PrivateDirected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollaborationReservationV1 {
    pub agent_id: AgentId,
    pub load_units: u16,
    pub cost_micros: u64,
    pub released: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollaborationRequestBindingV1 {
    pub operation_id: Uuid,
    pub request_digest: String,
}

impl CollaborationRequestBindingV1 {
    pub fn validate(&self) -> Result<(), WorkflowError> {
        if self.operation_id.is_nil() {
            return Err(invalid("collaboration request binding is invalid"));
        }
        validate_digest(&self.request_digest)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollaborationAdmissionStateV1 {
    Admitted,
    Active,
    Completed,
    Blocked,
    Cancelled,
    Escalated,
    BudgetExhausted,
}

impl CollaborationAdmissionStateV1 {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::Blocked
                | Self::Cancelled
                | Self::Escalated
                | Self::BudgetExhausted
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollaborationAdmissionDecisionV1 {
    pub schema_version: u16,
    pub admission_id: String,
    pub input: CollaborationAdmissionInputV1,
    pub mode: CollaborationAdmissionModeV1,
    pub selected_agents: Vec<AgentId>,
    pub selected_participants: Vec<CollaborationSelectedParticipantV1>,
    pub eligible_agents: Vec<AgentId>,
    pub rejected_candidates: Vec<AdmissionCandidateRejectionRecordV1>,
    pub routes: Vec<CollaborationRouteV1>,
    pub reasons: Vec<String>,
    pub expected_benefit_ref: String,
    pub objective_score_micros: i64,
    pub correlation_penalty_micros: u64,
    pub reservations: Vec<CollaborationReservationV1>,
    pub request_bindings: Vec<CollaborationRequestBindingV1>,
    pub state: CollaborationAdmissionStateV1,
    pub transition_sequence: u64,
    pub publication_revision: u64,
    pub rounds_used: u16,
    pub tokens_used: u64,
    pub cost_used_micros: u64,
    pub novelty_digests: Vec<String>,
    pub milestone_digests: Vec<String>,
    pub work_digests: Vec<String>,
    pub stalled_updates: u16,
    pub termination_reason: Option<String>,
    pub decision_digest: String,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
}

impl CollaborationAdmissionDecisionV1 {
    pub fn expected_binding_digest(&self) -> Result<String, WorkflowError> {
        canonical_sha256(
            "sentinel.workflow.collaboration-admission-binding.v1",
            &(
                self.schema_version,
                self.admission_id.as_str(),
                &self.input.tenant_id,
                &self.input.project_id,
                &self.input.work_item_id,
                self.input.organization_generation,
                self.input.organization_digest.as_str(),
                self.input.assignment_id.as_str(),
                self.input.assignment_version,
                self.input.assignment_digest.as_str(),
                self.input.behavior_policy_generation,
                self.input.behavior_policy_digest.as_str(),
            ),
        )
    }

    pub fn expected_digest(&self) -> Result<String, WorkflowError> {
        #[derive(Serialize)]
        struct Material<'a> {
            schema_version: u16,
            admission_id: &'a str,
            input: &'a CollaborationAdmissionInputV1,
            mode: CollaborationAdmissionModeV1,
            selected_agents: &'a [AgentId],
            selected_participants: &'a [CollaborationSelectedParticipantV1],
            eligible_agents: &'a [AgentId],
            rejected_candidates: &'a [AdmissionCandidateRejectionRecordV1],
            routes: &'a [CollaborationRouteV1],
            reasons: &'a [String],
            expected_benefit_ref: &'a str,
            objective_score_micros: i64,
            correlation_penalty_micros: u64,
            reservations: &'a [CollaborationReservationV1],
            request_bindings: &'a [CollaborationRequestBindingV1],
            state: CollaborationAdmissionStateV1,
            transition_sequence: u64,
            rounds_used: u16,
            tokens_used: u64,
            cost_used_micros: u64,
            novelty_digests: &'a [String],
            milestone_digests: &'a [String],
            work_digests: &'a [String],
            stalled_updates: u16,
            termination_reason: &'a Option<String>,
            created_at_unix_ms: u64,
            updated_at_unix_ms: u64,
        }
        canonical_sha256(
            "sentinel.workflow.collaboration-admission-decision.v1",
            &Material {
                schema_version: self.schema_version,
                admission_id: &self.admission_id,
                input: &self.input,
                mode: self.mode,
                selected_agents: &self.selected_agents,
                selected_participants: &self.selected_participants,
                eligible_agents: &self.eligible_agents,
                rejected_candidates: &self.rejected_candidates,
                routes: &self.routes,
                reasons: &self.reasons,
                expected_benefit_ref: &self.expected_benefit_ref,
                objective_score_micros: self.objective_score_micros,
                correlation_penalty_micros: self.correlation_penalty_micros,
                reservations: &self.reservations,
                request_bindings: &self.request_bindings,
                state: self.state,
                transition_sequence: self.transition_sequence,
                rounds_used: self.rounds_used,
                tokens_used: self.tokens_used,
                cost_used_micros: self.cost_used_micros,
                novelty_digests: &self.novelty_digests,
                milestone_digests: &self.milestone_digests,
                work_digests: &self.work_digests,
                stalled_updates: self.stalled_updates,
                termination_reason: &self.termination_reason,
                created_at_unix_ms: self.created_at_unix_ms,
                updated_at_unix_ms: self.updated_at_unix_ms,
            },
        )
    }

    pub fn expected_session_contract_digest(&self) -> Result<String, WorkflowError> {
        canonical_sha256(
            "sentinel.workflow.collaboration-admission-session-contract.v1",
            &(
                self.schema_version,
                self.admission_id.as_str(),
                self.expected_binding_digest()?,
                self.mode,
                &self.selected_participants,
                &self.routes,
                &self.input.budget,
            ),
        )
    }

    pub fn refresh_digest(&mut self) -> Result<(), WorkflowError> {
        self.decision_digest = self.expected_digest()?;
        Ok(())
    }

    pub fn validate(&self, now_ms: u64) -> Result<(), WorkflowError> {
        self.input.validate(self.created_at_unix_ms)?;
        let human_escalation = self.mode == CollaborationAdmissionModeV1::HumanEscalation;
        if self.schema_version != COLLABORATION_ADMISSION_SCHEMA_VERSION
            || (!human_escalation && self.selected_agents.is_empty())
            || self.selected_agents.len() > usize::from(self.input.budget.max_participants)
            || self.eligible_agents.len() > MAX_ADMISSION_CANDIDATES
            || self.routes.len() > MAX_ADMISSION_ITEMS
            || self.reasons.is_empty()
            || self.transition_sequence == 0
            || self.publication_revision > self.transition_sequence
            || self.updated_at_unix_ms < self.created_at_unix_ms
            || now_ms < self.updated_at_unix_ms
        {
            return Err(invalid("collaboration admission decision is invalid"));
        }
        validate_identifier(&self.admission_id)?;
        validate_text(&self.expected_benefit_ref)?;
        validate_digest(&self.decision_digest)?;
        if self.expected_digest()? != self.decision_digest {
            return Err(invalid(
                "collaboration admission decision digest is invalid",
            ));
        }
        let selected = self
            .selected_agents
            .iter()
            .map(|agent_id| agent_id.0)
            .collect::<BTreeSet<_>>();
        let selected_participants = self
            .selected_participants
            .iter()
            .map(|participant| participant.agent_id.0)
            .collect::<BTreeSet<_>>();
        let eligible = self
            .eligible_agents
            .iter()
            .map(|agent_id| agent_id.0)
            .collect::<BTreeSet<_>>();
        if selected.len() != self.selected_agents.len()
            || selected_participants.len() != self.selected_participants.len()
            || selected_participants != selected
            || eligible.len() != self.eligible_agents.len()
            || self.reservations.len() != self.selected_agents.len()
            || if human_escalation {
                !selected.is_empty()
                    || !self.selected_participants.is_empty()
                    || !self.reservations.is_empty()
                    || self.state != CollaborationAdmissionStateV1::Escalated
            } else {
                selected.is_empty()
                    || !selected.contains(&self.input.owner.0)
                    || !selected.is_subset(&eligible)
            }
        {
            return Err(invalid("collaboration admission membership is invalid"));
        }
        for participant in &self.selected_participants {
            participant.validate()?;
        }
        let selected_capabilities = self
            .selected_participants
            .iter()
            .flat_map(|participant| participant.capabilities.iter().cloned())
            .collect::<BTreeSet<_>>();
        let minimum_participants = if self.input.specialist_panel_required {
            3
        } else if self.input.directed_handoff_required || requires_independent_review(&self.input) {
            2
        } else {
            1
        };
        let expected_mode = if human_escalation {
            CollaborationAdmissionModeV1::HumanEscalation
        } else {
            classify_mode(&self.input)
        };
        if self.mode != expected_mode
            || (!human_escalation && self.selected_participants.len() < minimum_participants)
            || (!human_escalation
                && !self
                    .input
                    .required_capabilities
                    .is_subset(&selected_capabilities))
            || (!human_escalation
                && self
                    .input
                    .required_handoff_agents
                    .iter()
                    .any(|agent_id| !selected.contains(&agent_id.0)))
            || (requires_independent_review(&self.input)
                && !human_escalation
                && selected_independent_channel_count(&self.selected_participants) < 2)
            || (!human_escalation
                && !selected_participants_satisfy_separation(
                    &self.input,
                    &self.selected_participants,
                ))
        {
            return Err(invalid("collaboration admission coverage is invalid"));
        }
        let rejected = self
            .rejected_candidates
            .iter()
            .map(|record| record.agent_id.0)
            .collect::<BTreeSet<_>>();
        if rejected.len() != self.rejected_candidates.len()
            || !rejected.is_disjoint(&eligible)
            || self.rejected_candidates.iter().any(|record| {
                record.agent_id.0 == 0
                    || record.reasons.is_empty()
                    || selected.contains(&record.agent_id.0)
            })
        {
            return Err(invalid("collaboration rejection record is invalid"));
        }
        if self.routes != build_sparse_routes(&self.input, self.mode, &self.selected_agents) {
            return Err(invalid("collaboration admission route is not canonical"));
        }
        for route in &self.routes {
            if route.from == route.to
                || !selected.contains(&route.from.0)
                || !selected.contains(&route.to.0)
                || route.permitted_packet_classes.is_empty()
                || !route
                    .permitted_packet_classes
                    .is_subset(&self.input.permitted_packet_classes)
                || route.visibility != CollaborationRouteVisibilityV1::PrivateDirected
            {
                return Err(invalid("collaboration admission route is invalid"));
            }
            for packet_class in &route.permitted_packet_classes {
                validate_identifier(packet_class)?;
            }
        }
        let terminal = self.state.is_terminal();
        let maximum_progress_records = usize::from(self.input.budget.max_rounds);
        let maximum_request_bindings = maximum_progress_records + 1;
        let mut request_operations = BTreeSet::new();
        for binding in &self.request_bindings {
            binding.validate()?;
            if !request_operations.insert(binding.operation_id) {
                return Err(invalid("collaboration request binding is duplicated"));
            }
        }
        let reservation_agents = self
            .reservations
            .iter()
            .map(|reservation| reservation.agent_id.0)
            .collect::<BTreeSet<_>>();
        let reservation_cost = self
            .reservations
            .iter()
            .try_fold(0_u64, |sum, reservation| {
                sum.checked_add(reservation.cost_micros)
            });
        if reservation_agents.len() != self.reservations.len()
            || reservation_agents != selected
            || self.reservations.iter().any(|reservation| {
                reservation.agent_id.0 == 0
                    || reservation.load_units != 1
                    || reservation.released != terminal
            })
            || reservation_cost.is_none_or(|cost| {
                if human_escalation {
                    cost != 0
                } else {
                    cost != self.input.budget.max_cost_micros
                        || cost > self.input.remaining_cost_budget_micros
                }
            })
            || self.request_bindings.len() > maximum_request_bindings
            || self.novelty_digests.len() > maximum_progress_records
            || self.milestone_digests.len() > maximum_progress_records
            || self.work_digests.len() > maximum_progress_records
            || usize::from(self.stalled_updates) > maximum_progress_records
            || self
                .novelty_digests
                .iter()
                .chain(&self.milestone_digests)
                .chain(&self.work_digests)
                .any(|digest| validate_digest(digest).is_err())
            || self.novelty_digests.iter().collect::<BTreeSet<_>>().len()
                != self.novelty_digests.len()
            || self.milestone_digests.iter().collect::<BTreeSet<_>>().len()
                != self.milestone_digests.len()
            || self.work_digests.iter().collect::<BTreeSet<_>>().len() != self.work_digests.len()
            || (terminal != self.termination_reason.is_some())
        {
            return Err(invalid("collaboration admission state is inconsistent"));
        }
        Ok(())
    }
}

pub fn authorize_collaboration_route(
    decision: &CollaborationAdmissionDecisionV1,
    from: AgentId,
    to: AgentId,
    packet_class: &str,
    visibility: CollaborationRouteVisibilityV1,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    decision.validate(now_ms)?;
    validate_identifier(packet_class)?;
    if decision.state.is_terminal()
        || now_ms >= decision.input.budget.deadline_unix_ms
        || !decision.routes.iter().any(|route| {
            route.from == from
                && route.to == to
                && route.visibility == visibility
                && route.permitted_packet_classes.contains(packet_class)
        })
    {
        return Err(WorkflowError::new(
            WorkflowErrorCode::AuthorityConflict,
            false,
            "collaboration route is not authorized",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollaborationProgressDispositionV1 {
    Continue,
    Complete,
    Block,
    Cancel,
    Escalate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollaborationProgressV1 {
    pub expected_transition_sequence: u64,
    pub rounds_delta: u16,
    pub tokens_delta: u64,
    pub cost_delta_micros: u64,
    pub novelty_micros: u32,
    pub novelty_digest: String,
    pub milestone_digest: Option<String>,
    pub work_digest: Option<String>,
    pub disposition: CollaborationProgressDispositionV1,
    pub reason_ref: String,
}

pub fn apply_collaboration_progress(
    decision: &mut CollaborationAdmissionDecisionV1,
    progress: &CollaborationProgressV1,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    decision.validate(now_ms)?;
    let mut next = decision.clone();
    apply_collaboration_progress_staged(&mut next, progress, now_ms)?;
    *decision = next;
    Ok(())
}

fn apply_collaboration_progress_staged(
    decision: &mut CollaborationAdmissionDecisionV1,
    progress: &CollaborationProgressV1,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    if decision.state.is_terminal()
        || progress.expected_transition_sequence != decision.transition_sequence
        || now_ms < decision.updated_at_unix_ms
    {
        return Err(transition("collaboration admission progress is stale"));
    }
    if progress.rounds_delta == 0 || progress.novelty_micros > 1_000_000 {
        return Err(invalid("collaboration progress is not bounded"));
    }
    validate_text(&progress.reason_ref)?;
    validate_digest(&progress.novelty_digest)?;
    if let Some(milestone) = &progress.milestone_digest {
        validate_digest(milestone)?;
    }
    if let Some(work) = &progress.work_digest {
        validate_digest(work)?;
    }
    decision.rounds_used = decision
        .rounds_used
        .checked_add(progress.rounds_delta)
        .ok_or_else(|| invalid("collaboration round counter overflow"))?;
    decision.tokens_used = decision
        .tokens_used
        .checked_add(progress.tokens_delta)
        .ok_or_else(|| invalid("collaboration token counter overflow"))?;
    decision.cost_used_micros = decision
        .cost_used_micros
        .checked_add(progress.cost_delta_micros)
        .ok_or_else(|| invalid("collaboration cost counter overflow"))?;

    let repeated = decision.novelty_digests.contains(&progress.novelty_digest);
    if !repeated {
        decision
            .novelty_digests
            .push(progress.novelty_digest.clone());
    }
    let milestone_advanced = progress.milestone_digest.as_ref().is_some_and(|milestone| {
        if decision.milestone_digests.contains(milestone) {
            false
        } else {
            decision.milestone_digests.push(milestone.clone());
            true
        }
    });
    if milestone_advanced {
        decision.stalled_updates = 0;
    } else {
        decision.stalled_updates = decision.stalled_updates.saturating_add(1);
    }
    let duplicate_work = progress.work_digest.as_ref().is_some_and(|work| {
        if decision.work_digests.contains(work) {
            true
        } else {
            decision.work_digests.push(work.clone());
            false
        }
    });

    let budget_exceeded = now_ms >= decision.input.budget.deadline_unix_ms
        || decision.rounds_used > decision.input.budget.max_rounds
        || decision.tokens_used > decision.input.budget.max_tokens
        || decision.cost_used_micros > decision.input.budget.max_cost_micros;
    let continuation_budget_exhausted = matches!(
        progress.disposition,
        CollaborationProgressDispositionV1::Continue
    ) && (decision.rounds_used
        >= decision.input.budget.max_rounds
        || decision.tokens_used >= decision.input.budget.max_tokens
        || decision.cost_used_micros >= decision.input.budget.max_cost_micros);
    if budget_exceeded || continuation_budget_exhausted {
        decision.state = CollaborationAdmissionStateV1::BudgetExhausted;
        decision.termination_reason = Some("collaboration-budget-exhausted".to_owned());
    } else if repeated
        || duplicate_work
        || progress.novelty_micros < decision.input.budget.minimum_novelty_micros
        || decision.stalled_updates > decision.input.budget.max_stalled_updates
    {
        decision.state = CollaborationAdmissionStateV1::Blocked;
        decision.termination_reason = Some(
            if duplicate_work {
                "collaboration-duplicate-work"
            } else if decision.stalled_updates > decision.input.budget.max_stalled_updates {
                "collaboration-stalled-progress"
            } else {
                "collaboration-no-new-information"
            }
            .to_owned(),
        );
    } else {
        decision.state = match progress.disposition {
            CollaborationProgressDispositionV1::Continue => CollaborationAdmissionStateV1::Active,
            CollaborationProgressDispositionV1::Complete => {
                CollaborationAdmissionStateV1::Completed
            }
            CollaborationProgressDispositionV1::Block => CollaborationAdmissionStateV1::Blocked,
            CollaborationProgressDispositionV1::Cancel => CollaborationAdmissionStateV1::Cancelled,
            CollaborationProgressDispositionV1::Escalate => {
                CollaborationAdmissionStateV1::Escalated
            }
        };
        if decision.state.is_terminal() {
            decision.termination_reason = Some(progress.reason_ref.clone());
        }
    }
    if decision.state.is_terminal() {
        for reservation in &mut decision.reservations {
            reservation.released = true;
        }
    }
    decision.transition_sequence = decision
        .transition_sequence
        .checked_add(1)
        .ok_or_else(|| invalid("collaboration transition sequence overflow"))?;
    decision.updated_at_unix_ms = now_ms;
    decision.refresh_digest()?;
    decision.validate(now_ms)
}

pub fn admit_collaboration(
    admission_id: String,
    input: CollaborationAdmissionInputV1,
    candidates: &[CollaborationCandidateV1],
    reliability: &[ReliabilityObservationV1],
    reserved_load: &BTreeMap<u16, u16>,
    expected_benefit_ref: String,
    now_ms: u64,
) -> Result<CollaborationAdmissionDecisionV1, WorkflowError> {
    input.validate(now_ms)?;
    validate_identifier(&admission_id)?;
    validate_text(&expected_benefit_ref)?;
    if candidates.is_empty() || candidates.len() > MAX_ADMISSION_CANDIDATES {
        return Err(invalid("collaboration candidate set is invalid"));
    }
    let mut seen = BTreeSet::new();
    for candidate in candidates {
        candidate.validate()?;
        if !seen.insert(candidate.agent_id.0) {
            return Err(invalid("collaboration candidate is duplicated"));
        }
    }
    let mut observation_ids = BTreeSet::new();
    let mut observation_digests = BTreeSet::new();
    for observation in reliability {
        observation.validate()?;
        if !observation_ids.insert(observation.observation_id.as_str())
            || !observation_digests.insert(observation.observation_digest.as_str())
        {
            return Err(invalid("reliability observation is duplicated"));
        }
    }

    let mut rejected = Vec::new();
    let mut eligible = Vec::new();
    for candidate in candidates {
        let mut reasons = Vec::new();
        if !candidate.active {
            reasons.push(AdmissionCandidateRejectionV1::Inactive);
        }
        if candidate.authority_scope_digest != input.assignment_digest {
            reasons.push(AdmissionCandidateRejectionV1::AuthorityScope);
        }
        if candidate.organization_generation != input.organization_generation
            || candidate.organization_digest != input.organization_digest
        {
            reasons.push(AdmissionCandidateRejectionV1::StaleOrganization);
        }
        let reserved = reserved_load
            .get(&candidate.agent_id.0)
            .copied()
            .unwrap_or(0);
        if candidate
            .assignment_load
            .checked_add(reserved)
            .is_none_or(|load| load >= candidate.assignment_limit)
        {
            reasons.push(AdmissionCandidateRejectionV1::AtLoadLimit);
        }
        if !candidate.privacy_classes.contains(&input.privacy_class) {
            reasons.push(AdmissionCandidateRejectionV1::Privacy);
        }
        let eligible_for_separation = input.separation_requirements.iter().any(|requirement| {
            (requirement.owner_may_fill || candidate.agent_id != input.owner)
                && (requirement.required_roles.is_empty()
                    || requirement
                        .required_roles
                        .contains(&candidate.permanent_role))
        });
        let required_for_handoff = input.required_handoff_agents.contains(&candidate.agent_id);
        if candidate
            .capabilities
            .is_disjoint(&input.required_capabilities)
            && !eligible_for_separation
            && !required_for_handoff
        {
            reasons.push(AdmissionCandidateRejectionV1::Capability);
        }
        if !candidate.runtime_available {
            reasons.push(AdmissionCandidateRejectionV1::RuntimeUnavailable);
        }
        if !candidate.tools_available {
            reasons.push(AdmissionCandidateRejectionV1::ToolsUnavailable);
        }
        if candidate.estimated_cost_micros > input.budget.max_cost_micros {
            reasons.push(AdmissionCandidateRejectionV1::Cost);
        }
        if reasons.is_empty() {
            eligible.push(candidate);
        } else {
            rejected.push(AdmissionCandidateRejectionRecordV1 {
                agent_id: candidate.agent_id,
                reasons,
            });
        }
    }
    eligible.sort_by_key(|candidate| candidate.agent_id.0);
    rejected.sort_by_key(|record| record.agent_id.0);
    let eligible_agents = eligible
        .iter()
        .map(|candidate| candidate.agent_id)
        .collect::<Vec<_>>();

    let forced_human = input.authority_conflict
        || input.privacy_conflict
        || input.human_approval_required
        || input.reversibility == ReversibilityV1::Irreversible;
    let selection = if forced_human {
        None
    } else {
        select_team(&input, &eligible, reliability)?
    };

    let (mode, selected, objective_score, correlation_penalty, reasons) = if let Some(selection) =
        selection
    {
        let mode = classify_mode(&input);
        let reasons = admission_reasons(&input, mode, &selection.members);
        (
            mode,
            selection.members,
            selection.score,
            selection.correlation_penalty,
            reasons,
        )
    } else {
        (
            CollaborationAdmissionModeV1::HumanEscalation,
            Vec::new(),
            0,
            0,
            vec![if forced_human {
                "human authority is required by the admission policy".to_owned()
            } else {
                "no eligible team satisfies capability, separation, privacy, and budget".to_owned()
            }],
        )
    };

    let selected_agents = selected
        .iter()
        .map(|candidate| candidate.agent_id)
        .collect::<Vec<_>>();
    let selected_participants = selected
        .iter()
        .map(|candidate| CollaborationSelectedParticipantV1::from_candidate(candidate))
        .collect::<Result<Vec<_>, _>>()?;
    let routes = build_sparse_routes(&input, mode, &selected_agents);
    let participant_count = u64::try_from(selected.len())
        .map_err(|_| invalid("collaboration reservation count overflow"))?;
    let cost_per_participant = input
        .budget
        .max_cost_micros
        .checked_div(participant_count)
        .unwrap_or(0);
    let cost_remainder = input
        .budget
        .max_cost_micros
        .checked_rem(participant_count)
        .unwrap_or(0);
    let cost_remainder = usize::try_from(cost_remainder)
        .map_err(|_| invalid("collaboration reservation remainder overflow"))?;
    let reservations = selected
        .iter()
        .enumerate()
        .map(|(index, candidate)| CollaborationReservationV1 {
            agent_id: candidate.agent_id,
            load_units: 1,
            cost_micros: cost_per_participant + u64::from(index < cost_remainder),
            released: mode == CollaborationAdmissionModeV1::HumanEscalation,
        })
        .collect();
    let state = if mode == CollaborationAdmissionModeV1::HumanEscalation {
        CollaborationAdmissionStateV1::Escalated
    } else {
        CollaborationAdmissionStateV1::Admitted
    };
    let mut decision = CollaborationAdmissionDecisionV1 {
        schema_version: COLLABORATION_ADMISSION_SCHEMA_VERSION,
        admission_id,
        input,
        mode,
        selected_agents,
        selected_participants,
        eligible_agents,
        rejected_candidates: rejected,
        routes,
        reasons,
        expected_benefit_ref,
        objective_score_micros: objective_score,
        correlation_penalty_micros: correlation_penalty,
        reservations,
        request_bindings: Vec::new(),
        state,
        transition_sequence: 1,
        publication_revision: 0,
        rounds_used: 0,
        tokens_used: 0,
        cost_used_micros: 0,
        novelty_digests: Vec::new(),
        milestone_digests: Vec::new(),
        work_digests: Vec::new(),
        stalled_updates: 0,
        termination_reason: (mode == CollaborationAdmissionModeV1::HumanEscalation)
            .then(|| "human-escalation-required".to_owned()),
        decision_digest: String::new(),
        created_at_unix_ms: now_ms,
        updated_at_unix_ms: now_ms,
    };
    decision.refresh_digest()?;
    decision.validate(now_ms)?;
    Ok(decision)
}

struct TeamSelection<'a> {
    members: Vec<&'a CollaborationCandidateV1>,
    score: i64,
    correlation_penalty: u64,
}

fn select_team<'a>(
    input: &CollaborationAdmissionInputV1,
    eligible: &'a [&'a CollaborationCandidateV1],
    reliability: &[ReliabilityObservationV1],
) -> Result<Option<TeamSelection<'a>>, WorkflowError> {
    let count = eligible.len();
    if count == 0 {
        return Ok(None);
    }
    let mut valid = Vec::new();
    for mask in 1_u32..(1_u32 << count) {
        let members = eligible
            .iter()
            .enumerate()
            .filter_map(|(index, candidate)| ((mask & (1 << index)) != 0).then_some(*candidate))
            .collect::<Vec<_>>();
        if members.len() > usize::from(input.budget.max_participants)
            || !members
                .iter()
                .any(|candidate| candidate.agent_id == input.owner)
            || !team_satisfies(input, &members)
        {
            continue;
        }
        let member_cost = members.iter().try_fold(0_u64, |sum, candidate| {
            sum.checked_add(candidate.estimated_cost_micros)
        });
        if member_cost.is_none_or(|value| {
            value > input.budget.max_cost_micros || value > input.remaining_cost_budget_micros
        }) {
            continue;
        }
        let (score, correlation_penalty) = team_score(input, &members, reliability)?;
        valid.push(TeamSelection {
            members,
            score,
            correlation_penalty,
        });
    }
    let Some(best_score) = valid.iter().map(|selection| selection.score).max() else {
        return Ok(None);
    };
    let tolerance = i64::from(input.quality_tolerance_micros);
    valid.retain(|selection| selection.score.saturating_add(tolerance) >= best_score);
    valid.sort_by(|left, right| {
        left.members
            .len()
            .cmp(&right.members.len())
            .then_with(|| right.score.cmp(&left.score))
            .then_with(|| {
                left.members
                    .iter()
                    .map(|candidate| candidate.agent_id.0)
                    .cmp(right.members.iter().map(|candidate| candidate.agent_id.0))
            })
    });
    Ok(valid.into_iter().next())
}

fn team_satisfies(
    input: &CollaborationAdmissionInputV1,
    members: &[&CollaborationCandidateV1],
) -> bool {
    let minimum_participants = if input.specialist_panel_required {
        3
    } else if input.directed_handoff_required || requires_independent_review(input) {
        2
    } else {
        1
    };
    if members.len() < minimum_participants {
        return false;
    }
    let capabilities = members
        .iter()
        .flat_map(|candidate| candidate.capabilities.iter().cloned())
        .collect::<BTreeSet<_>>();
    if !input.required_capabilities.is_subset(&capabilities) {
        return false;
    }
    if !input.required_handoff_agents.iter().all(|required| {
        members
            .iter()
            .any(|candidate| candidate.agent_id == *required)
    }) {
        return false;
    }
    if requires_independent_review(input) && independent_channel_count(members) < 2 {
        return false;
    }
    input.separation_requirements.iter().all(|requirement| {
        let eligible = members
            .iter()
            .copied()
            .filter(|candidate| {
                (requirement.owner_may_fill || candidate.agent_id != input.owner)
                    && (requirement.required_roles.is_empty()
                        || requirement
                            .required_roles
                            .contains(&candidate.permanent_role))
            })
            .collect::<Vec<_>>();
        let distinct_agents = eligible
            .iter()
            .map(|candidate| candidate.agent_id.0)
            .collect::<BTreeSet<_>>();
        let required = usize::from(requirement.minimum_distinct_agents);
        distinct_agents.len() >= required && independent_channel_count(&eligible) >= required
    })
}

fn requires_independent_review(input: &CollaborationAdmissionInputV1) -> bool {
    input.evidence_conflict
        || input.task_risk >= TaskRiskV1::High
        || !matches!(input.ambiguity, AmbiguityClassV1::Low)
        || !matches!(input.uncertainty, UncertaintyClassV1::Low)
}

fn independent_channel_count(members: &[&CollaborationCandidateV1]) -> usize {
    maximum_independent_channel_count(members)
}

fn selected_independent_channel_count(members: &[CollaborationSelectedParticipantV1]) -> usize {
    maximum_independent_channel_count(&members.iter().collect::<Vec<_>>())
}

trait CorrelationChannel {
    fn model_family(&self) -> &str;
    fn prompt_digest(&self) -> &str;
    fn tool_set_digest(&self) -> &str;
    fn data_provenance_digest(&self) -> &str;
    fn prior_claim_correlation_digest(&self) -> Option<&str>;
}

impl CorrelationChannel for CollaborationCandidateV1 {
    fn model_family(&self) -> &str {
        &self.model_family
    }

    fn prompt_digest(&self) -> &str {
        &self.prompt_digest
    }

    fn tool_set_digest(&self) -> &str {
        &self.tool_set_digest
    }

    fn data_provenance_digest(&self) -> &str {
        &self.data_provenance_digest
    }

    fn prior_claim_correlation_digest(&self) -> Option<&str> {
        self.prior_claim_correlation_digest.as_deref()
    }
}

impl CorrelationChannel for CollaborationSelectedParticipantV1 {
    fn model_family(&self) -> &str {
        &self.model_family
    }

    fn prompt_digest(&self) -> &str {
        &self.prompt_digest
    }

    fn tool_set_digest(&self) -> &str {
        &self.tool_set_digest
    }

    fn data_provenance_digest(&self) -> &str {
        &self.data_provenance_digest
    }

    fn prior_claim_correlation_digest(&self) -> Option<&str> {
        self.prior_claim_correlation_digest.as_deref()
    }
}

fn maximum_independent_channel_count<T: CorrelationChannel>(members: &[&T]) -> usize {
    let mut maximum = 0;
    for mask in 1_u32..(1_u32 << members.len()) {
        let count = mask.count_ones() as usize;
        if count <= maximum {
            continue;
        }
        let independent = (0..members.len()).all(|left| {
            mask & (1 << left) == 0
                || ((left + 1)..members.len()).all(|right| {
                    mask & (1 << right) == 0
                        || !channels_are_correlated(members[left], members[right])
                })
        });
        if independent {
            maximum = count;
        }
    }
    maximum
}

fn channels_are_correlated<T: CorrelationChannel>(left: &T, right: &T) -> bool {
    let shared_prior = left.prior_claim_correlation_digest().is_some()
        && left.prior_claim_correlation_digest() == right.prior_claim_correlation_digest();
    let identical_channel = left.model_family() == right.model_family()
        && left.prompt_digest() == right.prompt_digest()
        && left.tool_set_digest() == right.tool_set_digest()
        && left.data_provenance_digest() == right.data_provenance_digest();
    shared_prior || identical_channel
}

fn selected_participants_satisfy_separation(
    input: &CollaborationAdmissionInputV1,
    members: &[CollaborationSelectedParticipantV1],
) -> bool {
    input.separation_requirements.iter().all(|requirement| {
        let eligible = members
            .iter()
            .filter(|participant| {
                (requirement.owner_may_fill || participant.agent_id != input.owner)
                    && (requirement.required_roles.is_empty()
                        || requirement
                            .required_roles
                            .contains(&participant.permanent_role))
            })
            .collect::<Vec<_>>();
        let distinct_agents = eligible
            .iter()
            .map(|participant| participant.agent_id.0)
            .collect::<BTreeSet<_>>();
        let required = usize::from(requirement.minimum_distinct_agents);
        distinct_agents.len() >= required
            && maximum_independent_channel_count(&eligible) >= required
    })
}

fn team_score(
    input: &CollaborationAdmissionInputV1,
    members: &[&CollaborationCandidateV1],
    reliability: &[ReliabilityObservationV1],
) -> Result<(i64, u64), WorkflowError> {
    let mut score = i64::try_from(input.required_capabilities.len())
        .map_err(|_| invalid("capability score overflow"))?
        .saturating_mul(SCORE_SCALE);
    for capability in &input.required_capabilities {
        let strongest = members
            .iter()
            .filter(|candidate| candidate.capabilities.contains(capability))
            .map(|candidate| candidate_reliability(input, candidate, capability, reliability))
            .max()
            .unwrap_or(0);
        score = score.saturating_add(strongest);
    }
    for candidate in members {
        score = score
            .saturating_sub(i64::try_from(candidate.queue_delay_ms).unwrap_or(i64::MAX))
            .saturating_sub(
                i64::try_from(candidate.estimated_cost_micros / 1_000).unwrap_or(i64::MAX),
            );
    }
    let mut penalty = 0_u64;
    for left in 0..members.len() {
        for right in (left + 1)..members.len() {
            let equal_dimensions = [
                members[left].model_family == members[right].model_family,
                members[left].mandate == members[right].mandate,
                members[left].prompt_digest == members[right].prompt_digest,
                members[left].tool_set_digest == members[right].tool_set_digest,
                members[left].data_provenance_digest == members[right].data_provenance_digest,
                members[left].prior_claim_correlation_digest.is_some()
                    && members[left].prior_claim_correlation_digest
                        == members[right].prior_claim_correlation_digest,
            ]
            .into_iter()
            .filter(|equal| *equal)
            .count() as u64;
            let pair_penalty = equal_dimensions.saturating_mul(250_000);
            penalty = penalty.saturating_add(pair_penalty);
            score = score.saturating_sub(i64::try_from(pair_penalty).unwrap_or(i64::MAX));
        }
    }
    Ok((score, penalty))
}

fn candidate_reliability(
    input: &CollaborationAdmissionInputV1,
    candidate: &CollaborationCandidateV1,
    capability: &str,
    reliability: &[ReliabilityObservationV1],
) -> i64 {
    if !input.learned_reliability_enabled {
        return 0;
    }
    reliability
        .iter()
        .filter(|observation| {
            observation.agent_id == candidate.agent_id
                && observation.task_family == input.task_family
                && observation.input_class == input.input_class
                && observation.capability == capability
                && observation.policy_generation == input.behavior_policy_generation
        })
        .map(|observation| i64::from(observation.evidence_quality_micros))
        .max()
        .unwrap_or(0)
}

fn classify_mode(input: &CollaborationAdmissionInputV1) -> CollaborationAdmissionModeV1 {
    if input.specialist_panel_required {
        CollaborationAdmissionModeV1::SpecialistPanel
    } else if requires_independent_review(input) || !input.separation_requirements.is_empty() {
        CollaborationAdmissionModeV1::ParallelIndependentReview
    } else if input.directed_handoff_required {
        CollaborationAdmissionModeV1::DirectedHandoff
    } else {
        CollaborationAdmissionModeV1::Solo
    }
}

fn admission_reasons(
    input: &CollaborationAdmissionInputV1,
    mode: CollaborationAdmissionModeV1,
    members: &[&CollaborationCandidateV1],
) -> Vec<String> {
    match mode {
        CollaborationAdmissionModeV1::Solo => {
            vec!["owner covers routine reversible work within policy".to_owned()]
        }
        CollaborationAdmissionModeV1::DirectedHandoff => vec![
            "a directed specialist handoff closes the capability gap".to_owned(),
            format!("smallest eligible team has {} participants", members.len()),
        ],
        CollaborationAdmissionModeV1::ParallelIndependentReview => vec![
            if input.evidence_conflict {
                "conflicting evidence requires an independent channel"
            } else if input.task_risk >= TaskRiskV1::High {
                "high task risk requires an independent channel"
            } else if !matches!(input.ambiguity, AmbiguityClassV1::Low) {
                "task ambiguity requires an independent channel"
            } else if !matches!(input.uncertainty, UncertaintyClassV1::Low) {
                "material uncertainty requires an independent channel"
            } else {
                "separation of duties requires independent channels"
            }
            .to_owned(),
            format!("smallest eligible team has {} participants", members.len()),
        ],
        CollaborationAdmissionModeV1::SpecialistPanel => vec![
            "complementary capabilities require a bounded specialist panel".to_owned(),
            format!("smallest eligible team has {} participants", members.len()),
        ],
        CollaborationAdmissionModeV1::HumanEscalation => {
            vec![if input.authority_conflict || input.privacy_conflict {
                "authority or privacy conflict requires human resolution".to_owned()
            } else {
                "human approval is required by policy".to_owned()
            }]
        }
    }
}

fn build_sparse_routes(
    input: &CollaborationAdmissionInputV1,
    mode: CollaborationAdmissionModeV1,
    selected: &[AgentId],
) -> Vec<CollaborationRouteV1> {
    if matches!(
        mode,
        CollaborationAdmissionModeV1::Solo | CollaborationAdmissionModeV1::HumanEscalation
    ) {
        return Vec::new();
    }
    let mut routes = Vec::new();
    for agent in selected {
        if *agent == input.owner {
            continue;
        }
        routes.push(CollaborationRouteV1 {
            from: input.owner,
            to: *agent,
            permitted_packet_classes: input.permitted_packet_classes.clone(),
            visibility: CollaborationRouteVisibilityV1::PrivateDirected,
        });
        routes.push(CollaborationRouteV1 {
            from: *agent,
            to: input.owner,
            permitted_packet_classes: input.permitted_packet_classes.clone(),
            visibility: CollaborationRouteVisibilityV1::PrivateDirected,
        });
    }
    routes
}

fn invalid(message: &'static str) -> WorkflowError {
    WorkflowError::new(WorkflowErrorCode::InvalidInput, false, message)
}

fn transition(message: &'static str) -> WorkflowError {
    WorkflowError::new(WorkflowErrorCode::InvalidTransition, false, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn input() -> CollaborationAdmissionInputV1 {
        CollaborationAdmissionInputV1 {
            schema_version: COLLABORATION_ADMISSION_SCHEMA_VERSION,
            tenant_id: TenantId::parse("tenant-a").unwrap(),
            project_id: ProjectId::parse("project-a").unwrap(),
            work_item_id: WorkItemId::parse("work-a").unwrap(),
            owner: AgentId(1),
            task_family: "web-development".to_owned(),
            input_class: "static-site".to_owned(),
            task_risk: TaskRiskV1::Low,
            reversibility: ReversibilityV1::Reversible,
            ambiguity: AmbiguityClassV1::Low,
            required_capabilities: BTreeSet::from(["frontend".to_owned()]),
            uncertainty: UncertaintyClassV1::Low,
            evidence_conflict: false,
            directed_handoff_required: false,
            required_handoff_agents: Vec::new(),
            specialist_panel_required: false,
            separation_requirements: Vec::new(),
            privacy_class: "project-internal".to_owned(),
            authority_conflict: false,
            privacy_conflict: false,
            human_approval_required: false,
            remaining_cost_budget_micros: 10_000,
            remaining_time_budget_ms: 60_000,
            organization_generation: 1,
            organization_digest: DIGEST.to_owned(),
            assignment_id: "assignment-a".to_owned(),
            assignment_version: 1,
            assignment_digest: DIGEST.to_owned(),
            behavior_policy_generation: 1,
            behavior_policy_digest: DIGEST.to_owned(),
            learned_reliability_enabled: false,
            collaboration_generation: 1,
            quality_tolerance_micros: 100_000,
            permitted_packet_classes: BTreeSet::from(["evidence".to_owned()]),
            budget: CollaborationAdmissionBudgetV1 {
                max_participants: 4,
                max_rounds: 4,
                max_tokens: 10_000,
                max_cost_micros: 10_000,
                deadline_unix_ms: 61_000,
                minimum_novelty_micros: 100_000,
                max_stalled_updates: 2,
            },
        }
    }

    fn candidate(agent_id: u16, capabilities: &[&str]) -> CollaborationCandidateV1 {
        CollaborationCandidateV1 {
            agent_id: AgentId(agent_id),
            permanent_role: if agent_id == 1 {
                CompanyRoleV1::TechnicalLead
            } else {
                CompanyRoleV1::Developer
            },
            mandate: BehaviorMandateV1::Implement,
            active: true,
            authority_scope_digest: DIGEST.to_owned(),
            organization_generation: 1,
            organization_digest: DIGEST.to_owned(),
            assignment_load: 0,
            assignment_limit: 2,
            capabilities: capabilities
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            privacy_classes: BTreeSet::from(["project-internal".to_owned()]),
            runtime_available: true,
            tools_available: true,
            model_family: format!("model-{agent_id}"),
            prompt_digest: format!("{agent_id:064x}"),
            tool_set_digest: format!("{:064x}", agent_id + 20),
            data_provenance_digest: format!("{:064x}", agent_id + 40),
            prior_claim_correlation_digest: None,
            queue_delay_ms: 0,
            estimated_cost_micros: 100,
        }
    }

    fn decision(
        input: CollaborationAdmissionInputV1,
        candidates: &[CollaborationCandidateV1],
    ) -> CollaborationAdmissionDecisionV1 {
        admit_collaboration(
            "admission-a".to_owned(),
            input,
            candidates,
            &[],
            &BTreeMap::new(),
            "expected risk reduction".to_owned(),
            1_000,
        )
        .unwrap()
    }

    #[test]
    fn routine_work_stays_solo() {
        let result = decision(input(), &[candidate(1, &["frontend"])]);
        assert_eq!(result.mode, CollaborationAdmissionModeV1::Solo);
        assert_eq!(result.selected_agents, vec![AgentId(1)]);
        assert!(result.routes.is_empty());
    }

    #[test]
    fn capability_topology_keeps_a_multi_capability_owner_solo() {
        let required = BTreeSet::from(["backend".to_owned(), "frontend".to_owned()]);
        let candidates = vec![(AgentId(1), required.clone())];
        assert_eq!(
            collaboration_policy_team_shape(AgentId(1), &required, &candidates, &[]).unwrap(),
            (false, false)
        );
    }

    #[test]
    fn capability_topology_distinguishes_one_handoff_from_a_panel() {
        let required = BTreeSet::from([
            "backend".to_owned(),
            "frontend".to_owned(),
            "security".to_owned(),
        ]);
        let owner = (AgentId(1), BTreeSet::from(["frontend".to_owned()]));
        let one_specialist = (
            AgentId(2),
            BTreeSet::from(["backend".to_owned(), "security".to_owned()]),
        );
        assert_eq!(
            collaboration_policy_team_shape(
                AgentId(1),
                &required,
                &[owner.clone(), one_specialist],
                &[],
            )
            .unwrap(),
            (true, false)
        );
        assert_eq!(
            collaboration_policy_team_shape(
                AgentId(1),
                &required,
                &[
                    owner,
                    (AgentId(2), BTreeSet::from(["backend".to_owned()])),
                    (AgentId(3), BTreeSet::from(["security".to_owned()])),
                ],
                &[],
            )
            .unwrap(),
            (false, true)
        );
    }

    #[test]
    fn hard_eligibility_rejections_are_explainable() {
        let owner = candidate(1, &["frontend"]);
        let mut inactive = candidate(2, &["frontend"]);
        inactive.active = false;
        let mut unauthorized = candidate(3, &["frontend"]);
        unauthorized.authority_scope_digest = "b".repeat(64);
        let mut stale = candidate(4, &["frontend"]);
        stale.organization_generation = 2;
        let mut overloaded = candidate(5, &["frontend"]);
        overloaded.assignment_load = overloaded.assignment_limit;
        let mut private = candidate(6, &["frontend"]);
        private.privacy_classes.clear();
        let missing_capability = candidate(7, &["backend"]);
        let mut runtime_unavailable = candidate(8, &["frontend"]);
        runtime_unavailable.runtime_available = false;
        let mut tools_unavailable = candidate(9, &["frontend"]);
        tools_unavailable.tools_available = false;
        let mut unaffordable = candidate(10, &["frontend"]);
        unaffordable.estimated_cost_micros = 10_001;
        let result = decision(
            input(),
            &[
                owner,
                inactive,
                unauthorized,
                stale,
                overloaded,
                private,
                missing_capability,
                runtime_unavailable,
                tools_unavailable,
                unaffordable,
            ],
        );
        assert_eq!(result.selected_agents, vec![AgentId(1)]);
        let rejected = result
            .rejected_candidates
            .iter()
            .map(|record| (record.agent_id.0, record.reasons.as_slice()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(rejected[&2], &[AdmissionCandidateRejectionV1::Inactive]);
        assert_eq!(
            rejected[&3],
            &[AdmissionCandidateRejectionV1::AuthorityScope]
        );
        assert_eq!(
            rejected[&4],
            &[AdmissionCandidateRejectionV1::StaleOrganization]
        );
        assert_eq!(rejected[&5], &[AdmissionCandidateRejectionV1::AtLoadLimit]);
        assert_eq!(rejected[&6], &[AdmissionCandidateRejectionV1::Privacy]);
        assert_eq!(rejected[&7], &[AdmissionCandidateRejectionV1::Capability]);
        assert_eq!(
            rejected[&8],
            &[AdmissionCandidateRejectionV1::RuntimeUnavailable]
        );
        assert_eq!(
            rejected[&9],
            &[AdmissionCandidateRejectionV1::ToolsUnavailable]
        );
        assert_eq!(rejected[&10], &[AdmissionCandidateRejectionV1::Cost]);
    }

    #[test]
    fn smallest_capability_cover_wins_within_tolerance() {
        let mut request = input();
        request.directed_handoff_required = true;
        request.required_handoff_agents.push(AgentId(2));
        request.required_capabilities.insert("backend".to_owned());
        let owner = candidate(1, &["frontend"]);
        let full_stack = candidate(2, &["frontend", "backend"]);
        let frontend = candidate(3, &["frontend"]);
        let backend = candidate(4, &["backend"]);
        let result = decision(request, &[owner, full_stack, frontend, backend]);
        assert_eq!(result.selected_agents, vec![AgentId(1), AgentId(2)]);
        assert_eq!(result.mode, CollaborationAdmissionModeV1::DirectedHandoff);
    }

    #[test]
    fn dependency_handoff_keeps_the_exact_upstream_owner() {
        let mut request = input();
        request.directed_handoff_required = true;
        request.required_handoff_agents.push(AgentId(2));
        let result = decision(
            request,
            &[
                candidate(1, &["frontend"]),
                candidate(2, &["backend"]),
                candidate(3, &["frontend"]),
            ],
        );
        assert_eq!(result.mode, CollaborationAdmissionModeV1::DirectedHandoff);
        assert_eq!(result.selected_agents, vec![AgentId(1), AgentId(2)]);
    }

    #[test]
    fn exhaustive_small_capability_sets_match_minimal_team_oracle() {
        let capability_sets = [
            vec!["frontend"],
            vec!["backend"],
            vec!["frontend", "backend"],
        ];
        for second in &capability_sets {
            for third in &capability_sets {
                for fourth in &capability_sets {
                    let mut request = input();
                    request.required_capabilities.insert("backend".to_owned());
                    let candidates = [
                        candidate(1, &["frontend"]),
                        candidate(2, second),
                        candidate(3, third),
                        candidate(4, fourth),
                    ];
                    let expected_specialist = candidates
                        .iter()
                        .skip(1)
                        .find(|candidate| candidate.capabilities.contains("backend"))
                        .map(|candidate| candidate.agent_id);
                    if let Some(expected) = expected_specialist {
                        request.directed_handoff_required = true;
                        request.required_handoff_agents.push(expected);
                    }
                    let result = decision(request, &candidates);
                    if let Some(expected) = expected_specialist {
                        assert_eq!(result.selected_agents, vec![AgentId(1), expected]);
                    } else {
                        assert_eq!(result.mode, CollaborationAdmissionModeV1::HumanEscalation);
                    }
                }
            }
        }
    }

    #[test]
    fn specialist_panel_requires_three_eligible_participants() {
        let mut request = input();
        request.specialist_panel_required = true;
        request.required_capabilities.insert("backend".to_owned());
        let owner = candidate(1, &["frontend", "backend"]);
        let reviewer = candidate(2, &["frontend"]);
        let specialist = candidate(3, &["backend"]);
        let result = decision(request, &[owner, reviewer, specialist]);
        assert_eq!(result.mode, CollaborationAdmissionModeV1::SpecialistPanel);
        assert_eq!(result.selected_agents.len(), 3);
    }

    #[test]
    fn correlated_clones_do_not_displace_diverse_specialist() {
        let mut request = input();
        request.task_risk = TaskRiskV1::High;
        request
            .separation_requirements
            .push(SeparationRequirementV1 {
                required_roles: BTreeSet::from([CompanyRoleV1::Developer]),
                minimum_distinct_agents: 2,
                owner_may_fill: false,
            });
        let owner = candidate(1, &["frontend"]);
        let clone_a = candidate(2, &["frontend"]);
        let mut clone_b = candidate(3, &["frontend"]);
        clone_b.model_family = clone_a.model_family.clone();
        clone_b.prompt_digest = clone_a.prompt_digest.clone();
        clone_b.tool_set_digest = clone_a.tool_set_digest.clone();
        clone_b.data_provenance_digest = clone_a.data_provenance_digest.clone();
        clone_b.prior_claim_correlation_digest = Some(DIGEST.to_owned());
        let mut clone_a = clone_a;
        clone_a.prior_claim_correlation_digest = Some(DIGEST.to_owned());
        let diverse = candidate(4, &["frontend"]);
        let result = decision(request, &[owner, clone_a, clone_b, diverse]);
        assert_eq!(result.selected_agents.len(), 3);
        assert!(result.selected_agents.contains(&AgentId(4)));
        assert_ne!(
            result.selected_agents.contains(&AgentId(2)),
            result.selected_agents.contains(&AgentId(3))
        );
        assert_eq!(
            result.mode,
            CollaborationAdmissionModeV1::ParallelIndependentReview
        );
    }

    #[test]
    fn risk_ambiguity_uncertainty_and_evidence_conflict_require_independent_review() {
        let cases = [
            (
                TaskRiskV1::High,
                AmbiguityClassV1::Low,
                UncertaintyClassV1::Low,
                false,
            ),
            (
                TaskRiskV1::Low,
                AmbiguityClassV1::Material,
                UncertaintyClassV1::Low,
                false,
            ),
            (
                TaskRiskV1::Low,
                AmbiguityClassV1::Low,
                UncertaintyClassV1::Material,
                false,
            ),
            (
                TaskRiskV1::Low,
                AmbiguityClassV1::Low,
                UncertaintyClassV1::Low,
                true,
            ),
        ];
        for (task_risk, ambiguity, uncertainty, evidence_conflict) in cases {
            let mut request = input();
            request.task_risk = task_risk;
            request.ambiguity = ambiguity;
            request.uncertainty = uncertainty;
            request.evidence_conflict = evidence_conflict;

            let no_reviewer = decision(request.clone(), &[candidate(1, &["frontend"])]);
            assert_eq!(
                no_reviewer.mode,
                CollaborationAdmissionModeV1::HumanEscalation
            );
            assert!(no_reviewer.selected_agents.is_empty());

            let reviewed = decision(
                request,
                &[candidate(1, &["frontend"]), candidate(2, &["frontend"])],
            );
            assert_eq!(
                reviewed.mode,
                CollaborationAdmissionModeV1::ParallelIndependentReview
            );
            assert_eq!(reviewed.selected_agents, vec![AgentId(1), AgentId(2)]);
            assert_eq!(reviewed.routes.len(), 2);
        }
    }

    #[test]
    fn correlated_replica_cannot_satisfy_implicit_independent_review() {
        let mut request = input();
        request.task_risk = TaskRiskV1::High;
        let owner = candidate(1, &["frontend"]);
        let mut replica = candidate(2, &["frontend"]);
        replica.model_family = owner.model_family.clone();
        replica.mandate = owner.mandate;
        replica.prompt_digest = owner.prompt_digest.clone();
        replica.tool_set_digest = owner.tool_set_digest.clone();
        replica.data_provenance_digest = owner.data_provenance_digest.clone();
        replica.prior_claim_correlation_digest = owner.prior_claim_correlation_digest.clone();

        let result = decision(request, &[owner, replica]);
        assert_eq!(result.mode, CollaborationAdmissionModeV1::HumanEscalation);
        assert!(result.selected_agents.is_empty());
    }

    #[test]
    fn shared_prior_evidence_is_correlated_even_when_other_channels_differ() {
        let mut request = input();
        request.task_risk = TaskRiskV1::High;
        let mut owner = candidate(1, &["frontend"]);
        owner.prior_claim_correlation_digest = Some(DIGEST.to_owned());
        let mut reviewer = candidate(2, &["frontend"]);
        reviewer.model_family = "different-model".to_owned();
        reviewer.prompt_digest = format!("{:064x}", 989);
        reviewer.tool_set_digest = format!("{:064x}", 990);
        reviewer.data_provenance_digest = format!("{:064x}", 991);
        reviewer.prior_claim_correlation_digest = Some(DIGEST.to_owned());

        let result = decision(request, &[owner, reviewer]);
        assert_eq!(result.mode, CollaborationAdmissionModeV1::HumanEscalation);
        assert!(result.selected_agents.is_empty());
    }

    #[test]
    fn verified_task_specialist_is_not_averaged_away() {
        let mut request = input();
        request.learned_reliability_enabled = true;
        let owner = candidate(1, &["frontend"]);
        let specialist = candidate(2, &["frontend"]);
        let generalist_a = candidate(3, &["frontend"]);
        let generalist_b = candidate(4, &["frontend"]);
        let generalist_c = candidate(5, &["frontend"]);
        let mut observation = ReliabilityObservationV1 {
            observation_id: "observation-a".to_owned(),
            agent_id: AgentId(2),
            capability: "frontend".to_owned(),
            task_family: request.task_family.clone(),
            input_class: request.input_class.clone(),
            claim_id: "claim-a".to_owned(),
            accepted_outcome_digest: DIGEST.to_owned(),
            independent_verification_digest: DIGEST.to_owned(),
            verifier_principal_id: "qa-agent".to_owned(),
            verifier_authority_digest: DIGEST.to_owned(),
            accepted: true,
            calibration_bucket: 90,
            evidence_quality_micros: 900_000,
            policy_generation: 1,
            observation_digest: String::new(),
            recorded_at_unix_ms: 500,
        };
        observation.observation_digest = observation.expected_digest().unwrap();
        let result = admit_collaboration(
            "admission-a".to_owned(),
            request,
            &[owner, specialist, generalist_a, generalist_b, generalist_c],
            &[observation],
            &BTreeMap::new(),
            "verified expertise".to_owned(),
            1_000,
        )
        .unwrap();
        assert_eq!(result.selected_agents, vec![AgentId(1), AgentId(2)]);
    }

    #[test]
    fn hard_conflict_escalates_without_fake_team() {
        let mut request = input();
        request.authority_conflict = true;
        let result = decision(request, &[candidate(1, &["frontend"])]);
        assert_eq!(result.mode, CollaborationAdmissionModeV1::HumanEscalation);
        assert_eq!(result.state, CollaborationAdmissionStateV1::Escalated);
        assert!(result.selected_agents.is_empty());
        assert!(result.selected_participants.is_empty());
        assert!(result.reservations.is_empty());
        assert!(result.routes.is_empty());
    }

    #[test]
    fn irreversible_work_requires_human_authority_even_when_low_risk() {
        let mut request = input();
        request.reversibility = ReversibilityV1::Irreversible;
        let result = decision(
            request,
            &[candidate(1, &["frontend"]), candidate(2, &["frontend"])],
        );
        assert_eq!(result.mode, CollaborationAdmissionModeV1::HumanEscalation);
        assert_eq!(result.state, CollaborationAdmissionStateV1::Escalated);
        assert!(result.selected_agents.is_empty());
        assert_eq!(
            result.termination_reason.as_deref(),
            Some("human-escalation-required")
        );
    }

    #[test]
    fn ineligible_owner_escalates_without_reserving_a_fake_runtime() {
        let request = input();
        let mut owner = candidate(1, &["frontend"]);
        owner.runtime_available = false;
        let result = decision(request, &[owner]);
        assert_eq!(result.mode, CollaborationAdmissionModeV1::HumanEscalation);
        assert_eq!(result.state, CollaborationAdmissionStateV1::Escalated);
        assert!(result.selected_agents.is_empty());
        assert!(result.reservations.is_empty());
    }

    #[test]
    fn progress_budget_and_novelty_terminate_deterministically() {
        let mut result = decision(input(), &[candidate(1, &["frontend"])]);
        let first = CollaborationProgressV1 {
            expected_transition_sequence: 1,
            rounds_delta: 1,
            tokens_delta: 10,
            cost_delta_micros: 1,
            novelty_micros: 500_000,
            novelty_digest: "b".repeat(64),
            milestone_digest: Some("c".repeat(64)),
            work_digest: Some("d".repeat(64)),
            disposition: CollaborationProgressDispositionV1::Continue,
            reason_ref: "new evidence".to_owned(),
        };
        apply_collaboration_progress(&mut result, &first, 2_000).unwrap();
        assert_eq!(result.state, CollaborationAdmissionStateV1::Active);
        let mut repeated = first;
        repeated.expected_transition_sequence = 2;
        apply_collaboration_progress(&mut result, &repeated, 3_000).unwrap();
        assert_eq!(result.state, CollaborationAdmissionStateV1::Blocked);
        assert!(result.reservations.iter().all(|value| value.released));
    }

    #[test]
    fn progress_requires_a_bounded_round_and_normalized_novelty() {
        let baseline = decision(input(), &[candidate(1, &["frontend"])]);
        for (rounds_delta, novelty_micros) in [(0, 500_000), (1, 1_000_001)] {
            let mut result = baseline.clone();
            let error = apply_collaboration_progress(
                &mut result,
                &CollaborationProgressV1 {
                    expected_transition_sequence: 1,
                    rounds_delta,
                    tokens_delta: 0,
                    cost_delta_micros: 0,
                    novelty_micros,
                    novelty_digest: "9".repeat(64),
                    milestone_digest: Some("8".repeat(64)),
                    work_digest: None,
                    disposition: CollaborationProgressDispositionV1::Continue,
                    reason_ref: "bounded progress".to_owned(),
                },
                2_000,
            )
            .unwrap_err();
            assert_eq!(error.code, WorkflowErrorCode::InvalidInput);
            assert_eq!(result, baseline);
        }

        let mut tampered = baseline.clone();
        tampered.selected_agents.clear();
        let before = tampered.clone();
        let error = apply_collaboration_progress(
            &mut tampered,
            &CollaborationProgressV1 {
                expected_transition_sequence: 1,
                rounds_delta: 1,
                tokens_delta: 0,
                cost_delta_micros: 0,
                novelty_micros: 500_000,
                novelty_digest: "7".repeat(64),
                milestone_digest: Some("6".repeat(64)),
                work_digest: None,
                disposition: CollaborationProgressDispositionV1::Continue,
                reason_ref: "must not normalize invalid state".to_owned(),
            },
            2_000,
        )
        .unwrap_err();
        assert_eq!(error.code, WorkflowErrorCode::InvalidInput);
        assert_eq!(tampered, before);
    }

    #[test]
    fn admission_budget_caps_progress_and_stall_updates() {
        let mut request = input();
        request.budget.max_rounds = MAX_ADMISSION_PROGRESS_UPDATES + 1;
        assert!(request.validate(1_000).is_err());

        request.budget.max_rounds = 4;
        request.budget.max_stalled_updates = 5;
        assert!(request.validate(1_000).is_err());
    }

    #[test]
    fn learned_reliability_is_inert_until_explicitly_enabled() {
        let request = input();
        let owner = candidate(1, &["frontend"]);
        let specialist = candidate(2, &["frontend"]);
        let mut observation = ReliabilityObservationV1 {
            observation_id: "observation-disabled".to_owned(),
            agent_id: AgentId(2),
            capability: "frontend".to_owned(),
            task_family: request.task_family.clone(),
            input_class: request.input_class.clone(),
            claim_id: "claim-disabled".to_owned(),
            accepted_outcome_digest: DIGEST.to_owned(),
            independent_verification_digest: DIGEST.to_owned(),
            verifier_principal_id: "qa-agent".to_owned(),
            verifier_authority_digest: DIGEST.to_owned(),
            accepted: true,
            calibration_bucket: 100,
            evidence_quality_micros: 1_000_000,
            policy_generation: 1,
            observation_digest: String::new(),
            recorded_at_unix_ms: 500,
        };
        observation.observation_digest = observation.expected_digest().unwrap();
        let result = admit_collaboration(
            "admission-disabled".to_owned(),
            request,
            &[owner, specialist],
            &[observation],
            &BTreeMap::new(),
            "solo remains the strong baseline".to_owned(),
            1_000,
        )
        .unwrap();
        assert_eq!(result.mode, CollaborationAdmissionModeV1::Solo);
        assert_eq!(result.selected_agents, vec![AgentId(1)]);
    }

    #[test]
    fn learned_reliability_is_exactly_policy_generation_bound() {
        let mut request = input();
        request.learned_reliability_enabled = true;
        request.behavior_policy_generation = 2;
        let candidate = candidate(2, &["frontend"]);
        let mut observation = ReliabilityObservationV1 {
            observation_id: "observation-stale-policy".to_owned(),
            agent_id: candidate.agent_id,
            capability: "frontend".to_owned(),
            task_family: request.task_family.clone(),
            input_class: request.input_class.clone(),
            claim_id: "claim-stale-policy".to_owned(),
            accepted_outcome_digest: DIGEST.to_owned(),
            independent_verification_digest: DIGEST.to_owned(),
            verifier_principal_id: "qa-agent".to_owned(),
            verifier_authority_digest: DIGEST.to_owned(),
            accepted: true,
            calibration_bucket: 100,
            evidence_quality_micros: 1_000_000,
            policy_generation: 1,
            observation_digest: String::new(),
            recorded_at_unix_ms: 500,
        };
        observation.observation_digest = observation.expected_digest().unwrap();
        assert_eq!(
            candidate_reliability(&request, &candidate, "frontend", &[observation]),
            0
        );
    }

    #[test]
    fn route_authorization_is_exact_and_need_to_know() {
        let mut request = input();
        request.directed_handoff_required = true;
        request.required_handoff_agents.push(AgentId(2));
        request.required_capabilities.insert("backend".to_owned());
        request
            .permitted_packet_classes
            .insert("finding".to_owned());
        let result = decision(
            request,
            &[candidate(1, &["frontend"]), candidate(2, &["backend"])],
        );
        authorize_collaboration_route(
            &result,
            AgentId(1),
            AgentId(2),
            "finding",
            CollaborationRouteVisibilityV1::PrivateDirected,
            1_000,
        )
        .unwrap();
        assert_eq!(
            authorize_collaboration_route(
                &result,
                AgentId(2),
                AgentId(3),
                "finding",
                CollaborationRouteVisibilityV1::PrivateDirected,
                1_000,
            )
            .unwrap_err()
            .code,
            WorkflowErrorCode::AuthorityConflict
        );
        assert_eq!(
            authorize_collaboration_route(
                &result,
                AgentId(1),
                AgentId(2),
                "undeclared",
                CollaborationRouteVisibilityV1::PrivateDirected,
                1_000,
            )
            .unwrap_err()
            .code,
            WorkflowErrorCode::AuthorityConflict
        );
        assert_eq!(
            authorize_collaboration_route(
                &result,
                AgentId(1),
                AgentId(2),
                "finding",
                CollaborationRouteVisibilityV1::PrivateDirected,
                result.input.budget.deadline_unix_ms,
            )
            .unwrap_err()
            .code,
            WorkflowErrorCode::AuthorityConflict
        );
    }

    #[test]
    fn zero_token_budget_is_valid_and_first_token_terminates() {
        let mut request = input();
        request.budget.max_tokens = 0;
        let mut result = decision(request, &[candidate(1, &["frontend"])]);
        apply_collaboration_progress(
            &mut result,
            &CollaborationProgressV1 {
                expected_transition_sequence: 1,
                rounds_delta: 1,
                tokens_delta: 1,
                cost_delta_micros: 0,
                novelty_micros: 500_000,
                novelty_digest: "e".repeat(64),
                milestone_digest: Some("f".repeat(64)),
                work_digest: None,
                disposition: CollaborationProgressDispositionV1::Continue,
                reason_ref: "unexpected token use".to_owned(),
            },
            2_000,
        )
        .unwrap();
        assert_eq!(result.state, CollaborationAdmissionStateV1::BudgetExhausted);
        assert!(result.reservations.iter().all(|value| value.released));
    }

    #[test]
    fn final_round_must_complete_or_terminate_as_budget_exhausted() {
        let mut request = input();
        request.budget.max_rounds = 1;
        request.budget.max_stalled_updates = 1;
        let baseline = decision(request, &[candidate(1, &["frontend"])]);

        let progress = |disposition| CollaborationProgressV1 {
            expected_transition_sequence: 1,
            rounds_delta: 1,
            tokens_delta: 0,
            cost_delta_micros: 0,
            novelty_micros: 1_000_000,
            novelty_digest: "1".repeat(64),
            milestone_digest: Some("2".repeat(64)),
            work_digest: Some("3".repeat(64)),
            disposition,
            reason_ref: "final bounded round".to_owned(),
        };

        let mut continuing = baseline.clone();
        apply_collaboration_progress(
            &mut continuing,
            &progress(CollaborationProgressDispositionV1::Continue),
            2_000,
        )
        .unwrap();
        assert_eq!(
            continuing.state,
            CollaborationAdmissionStateV1::BudgetExhausted
        );
        assert_eq!(continuing.novelty_digests.len(), 1);
        assert!(continuing.reservations.iter().all(|value| value.released));

        let mut completing = baseline;
        apply_collaboration_progress(
            &mut completing,
            &progress(CollaborationProgressDispositionV1::Complete),
            2_000,
        )
        .unwrap();
        assert_eq!(completing.state, CollaborationAdmissionStateV1::Completed);
        assert_eq!(completing.novelty_digests.len(), 1);
        assert!(completing.reservations.iter().all(|value| value.released));
    }

    #[test]
    fn tampered_reservations_and_routes_fail_closed() {
        let mut request = input();
        request.directed_handoff_required = true;
        request.required_handoff_agents.push(AgentId(2));
        request.required_capabilities.insert("backend".to_owned());
        let result = decision(
            request,
            &[candidate(1, &["frontend"]), candidate(2, &["backend"])],
        );

        let mut bad_reservation = result.clone();
        bad_reservation.reservations[0].agent_id = AgentId(9);
        bad_reservation.refresh_digest().unwrap();
        assert_eq!(
            bad_reservation.validate(1_000).unwrap_err().code,
            WorkflowErrorCode::InvalidInput
        );

        let mut missing_handoff = result.clone();
        missing_handoff
            .input
            .required_handoff_agents
            .push(AgentId(3));
        missing_handoff.refresh_digest().unwrap();
        assert_eq!(
            missing_handoff.validate(1_000).unwrap_err().code,
            WorkflowErrorCode::InvalidInput
        );

        let mut bad_route = result;
        bad_route.routes[0]
            .permitted_packet_classes
            .insert("broadcast".to_owned());
        bad_route.refresh_digest().unwrap();
        assert_eq!(
            bad_route.validate(1_000).unwrap_err().code,
            WorkflowErrorCode::InvalidInput
        );
    }
}
