use std::collections::BTreeSet;

use sentinel_common::{
    AppendProposalV2, CausationPolicyV1, EventContractError, EventDurability, EventPayloadCodec,
    EventSchemaDefinition, EventSchemaRegistry,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::admission::CollaborationAdmissionDecisionV1;
use crate::digest::canonical_sha256;
use crate::domain::{
    validate_text, validate_text_collection, AuthenticatedCompanyPrincipalV1,
    CompanyPrincipalKindV1, CompanyRoleV1, CompanyWorkflowCommandV1, ProjectV1,
};
use crate::model::{validate_digest, validate_identifier};
use crate::{AgentId, ProjectId, TenantId, WorkItemId, WorkflowError, WorkflowErrorCode};

pub const COLLABORATION_SCHEMA_VERSION: u16 = 1;
pub const COLLABORATION_EVENT_SCHEMA_VERSION: u32 = 1;
pub const COLLABORATION_EVENT_TYPE: &str = "company_collaboration_recorded";
pub const COLLABORATION_EVENT_PRODUCER: &str = "sentinel-workflow-collaboration";
pub const COLLABORATION_DELIVERY_TOPIC: &str = "sentinel/company/collaboration/v1";

const MAX_COLLABORATION_MEMBERS: usize = 16;
const MAX_COLLABORATION_RECORDS: usize = 128;
const MAX_PROMPT_ITEMS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BehaviorMandateV1 {
    Discover,
    Implement,
    Verify,
    Challenge,
    Synthesize,
    Decide,
    Escalate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BehaviorMandateContractV1 {
    pub mandate: BehaviorMandateV1,
    pub required_inputs: Vec<String>,
    pub expected_output: String,
    pub stop_condition: String,
    pub forbidden_actions: Vec<String>,
}

impl BehaviorMandateV1 {
    pub fn contract(self) -> BehaviorMandateContractV1 {
        let (required_inputs, expected_output, stop_condition) = match self {
            Self::Discover => (
                vec!["subject", "known_evidence", "unknowns"],
                "bounded finding set with source references and explicit unknowns",
                "the requested evidence boundary is covered or a typed gap is found",
            ),
            Self::Implement => (
                vec!["accepted_decision", "input_contracts", "acceptance_checks"],
                "artifact or patch bound to the accepted decision and checks",
                "the artifact is ready for independent verification or work is blocked",
            ),
            Self::Verify => (
                vec!["subject", "acceptance_checks", "evidence"],
                "independent pass, fail, or blocked finding with reproducible evidence",
                "every assigned check has a terminal evidence-backed outcome",
            ),
            Self::Challenge => (
                vec!["candidate_claims", "evidence", "decision_constraints"],
                "counterexample, unresolved risk, or evidence-backed concurrence",
                "material counterevidence is recorded or the challenge budget is exhausted",
            ),
            Self::Synthesize => (
                vec!["independent_claims", "dissent", "decision_constraints"],
                "comparison preserving agreements, disagreements, evidence, and uncertainty",
                "all exposed claims are represented without inventing consensus",
            ),
            Self::Decide => (
                vec!["authorized_options", "evidence_synthesis", "dissent"],
                "typed decision proposal with cited evidence and residual risk",
                "an authorized decision or typed escalation is ready",
            ),
            Self::Escalate => (
                vec!["blocked_subject", "attempt_history", "authority_route"],
                "minimal escalation packet naming the unresolved decision and required authority",
                "the correct authority has enough bounded evidence to act",
            ),
        };
        BehaviorMandateContractV1 {
            mandate: self,
            required_inputs: required_inputs.into_iter().map(str::to_owned).collect(),
            expected_output: expected_output.to_owned(),
            stop_condition: stop_condition.to_owned(),
            forbidden_actions: vec![
                "do not grant authority from model output, confidence, seniority, or consensus"
                    .to_owned(),
                "do not mutate workflow state outside authenticated typed commands".to_owned(),
                "do not expose private peer claims before the declared barrier".to_owned(),
                "do not omit material dissent or uncertainty".to_owned(),
                "do not continue after the stop condition or collaboration budget".to_owned(),
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollaborationModeV1 {
    IndependentReview,
    DirectedHandoff,
    DecisionSupport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollaborationSessionStateV1 {
    Planned,
    CollectingIndependentClaims,
    ExchangingEvidence,
    Deciding,
    Completed,
    Blocked,
    Escalated,
    Cancelled,
}

impl CollaborationSessionStateV1 {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Blocked | Self::Escalated | Self::Cancelled
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimExposureStateV1 {
    Private,
    Exposed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UncertaintyClassV1 {
    Low,
    Material,
    Blocking,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffPacketStateV1 {
    Prepared,
    Offered,
    ClarificationRequested,
    Accepted,
    Rejected,
    Escalated,
    Consumed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffGapClassV1 {
    DataGap,
    SignalCorruption,
    ReferentialDrift,
    CapabilityGap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffConsumptionKindV1 {
    IndependentClaim,
    WorkbenchInvocation,
    Review,
    ProjectDecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollaborationBudgetV1 {
    pub max_participants: u16,
    pub max_claims: u16,
    pub max_handoffs: u16,
    pub max_clarification_rounds: u16,
    pub max_transitions: u16,
    pub deadline_unix_ms: u64,
}

impl CollaborationBudgetV1 {
    pub(crate) fn validate(&self, created_at_unix_ms: u64) -> Result<(), WorkflowError> {
        if self.max_participants < 2
            || usize::from(self.max_participants) > MAX_COLLABORATION_MEMBERS
            || self.max_claims < self.max_participants
            || usize::from(self.max_claims) > MAX_COLLABORATION_RECORDS
            || self.max_handoffs == 0
            || usize::from(self.max_handoffs) > MAX_COLLABORATION_RECORDS
            || self.max_clarification_rounds == 0
            || self.max_clarification_rounds > 8
            || self.max_transitions == 0
            || usize::from(self.max_transitions) > MAX_COLLABORATION_RECORDS
            || self.deadline_unix_ms <= created_at_unix_ms
        {
            return Err(invalid("collaboration budget is invalid"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollaborationParticipantV1 {
    pub agent_id: AgentId,
    pub permanent_role: CompanyRoleV1,
    pub mandate: BehaviorMandateV1,
    pub capability_snapshot_digest: String,
    pub capabilities: BTreeSet<String>,
    pub privacy_classes: BTreeSet<String>,
}

impl CollaborationParticipantV1 {
    pub(crate) fn validate(&self) -> Result<(), WorkflowError> {
        if self.agent_id.0 == 0
            || self.permanent_role == CompanyRoleV1::Customer
            || self.capabilities.is_empty()
            || self.capabilities.len() > MAX_PROMPT_ITEMS
            || self.privacy_classes.len() > MAX_PROMPT_ITEMS
        {
            return Err(invalid("collaboration participant is invalid"));
        }
        validate_digest(&self.capability_snapshot_digest)?;
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
pub struct CollaborationAuthorityFenceV1 {
    pub organization_generation: u64,
    pub organization_digest: String,
    pub assignment_id: String,
    pub assignment_version: u64,
    pub assignment_digest: String,
    pub policy_version: u64,
    pub policy_digest: String,
}

impl CollaborationAuthorityFenceV1 {
    pub(crate) fn validate(&self) -> Result<(), WorkflowError> {
        if self.organization_generation == 0
            || self.assignment_version == 0
            || self.policy_version == 0
        {
            return Err(invalid("collaboration authority fence is invalid"));
        }
        validate_digest(&self.organization_digest)?;
        validate_identifier(&self.assignment_id)?;
        validate_digest(&self.assignment_digest)?;
        validate_digest(&self.policy_digest)
    }

    pub(crate) fn matches(&self, session: &CollaborationSessionV1) -> bool {
        self.organization_generation == session.organization_generation
            && self.organization_digest == session.organization_digest
            && self.assignment_id == session.assignment_id
            && self.assignment_version == session.assignment_version
            && self.assignment_digest == session.assignment_digest
            && self.policy_version == session.policy_version
            && self.policy_digest == session.policy_digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollaborationTransitionV1 {
    pub sequence: u64,
    pub from: CollaborationSessionStateV1,
    pub to: CollaborationSessionStateV1,
    pub actor: AgentId,
    pub reason_ref: String,
    pub occurred_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceReferenceV1 {
    pub reference: String,
    pub digest: String,
}

impl EvidenceReferenceV1 {
    pub(crate) fn validate(&self) -> Result<(), WorkflowError> {
        validate_text(&self.reference)?;
        validate_digest(&self.digest)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndependentClaimV1 {
    pub claim_id: String,
    pub session_id: String,
    pub contributor: AgentId,
    pub mandate: BehaviorMandateV1,
    pub conclusion_ref: String,
    pub evidence: Vec<EvidenceReferenceV1>,
    pub assumptions: Vec<String>,
    pub uncertainty: UncertaintyClassV1,
    pub confidence_basis: String,
    pub capability_snapshot_digest: String,
    pub input_digest: String,
    pub exposure_state: ClaimExposureStateV1,
    pub claim_digest: String,
    pub created_at_unix_ms: u64,
}

impl IndependentClaimV1 {
    pub fn expected_digest(&self) -> Result<String, WorkflowError> {
        #[derive(Serialize)]
        struct Material<'a> {
            claim_id: &'a str,
            session_id: &'a str,
            contributor: AgentId,
            mandate: BehaviorMandateV1,
            conclusion_ref: &'a str,
            evidence: &'a [EvidenceReferenceV1],
            assumptions: &'a [String],
            uncertainty: UncertaintyClassV1,
            confidence_basis: &'a str,
            capability_snapshot_digest: &'a str,
            input_digest: &'a str,
            created_at_unix_ms: u64,
        }
        canonical_sha256(
            "sentinel.workflow.independent-claim.v1",
            &Material {
                claim_id: &self.claim_id,
                session_id: &self.session_id,
                contributor: self.contributor,
                mandate: self.mandate,
                conclusion_ref: &self.conclusion_ref,
                evidence: &self.evidence,
                assumptions: &self.assumptions,
                uncertainty: self.uncertainty,
                confidence_basis: &self.confidence_basis,
                capability_snapshot_digest: &self.capability_snapshot_digest,
                input_digest: &self.input_digest,
                created_at_unix_ms: self.created_at_unix_ms,
            },
        )
    }

    pub(crate) fn validate(&self) -> Result<(), WorkflowError> {
        validate_identifier(&self.claim_id)?;
        validate_identifier(&self.session_id)?;
        validate_text(&self.conclusion_ref)?;
        validate_text_collection(&self.assumptions, false)?;
        validate_text(&self.confidence_basis)?;
        validate_digest(&self.capability_snapshot_digest)?;
        validate_digest(&self.input_digest)?;
        validate_digest(&self.claim_digest)?;
        if self.contributor.0 == 0
            || self.evidence.is_empty()
            || self.evidence.len() > MAX_PROMPT_ITEMS
            || self.created_at_unix_ms == 0
            || self.expected_digest()? != self.claim_digest
        {
            return Err(invalid("independent claim is invalid"));
        }
        for evidence in &self.evidence {
            evidence.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffClarificationV1 {
    pub clarification_id: String,
    pub gap_class: HandoffGapClassV1,
    pub question_ref: String,
    pub basis_digest: String,
    pub question_generation: u16,
    pub requested_by: AgentId,
    pub requested_at_unix_ms: u64,
    pub answer_ref: Option<String>,
    pub new_information_digest: Option<String>,
    pub answer_generation: Option<u16>,
    pub answered_by: Option<AgentId>,
    pub answered_at_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffConsumptionV1 {
    pub kind: HandoffConsumptionKindV1,
    pub subject_id: String,
    pub subject_digest: String,
    pub consumed_by: AgentId,
    pub consumed_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffPacketTransitionV1 {
    pub sequence: u64,
    pub from: HandoffPacketStateV1,
    pub to: HandoffPacketStateV1,
    pub actor: AgentId,
    pub reason_ref: String,
    pub occurred_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffPacketV1 {
    pub packet_id: String,
    pub session_id: String,
    pub work_item_id: WorkItemId,
    pub producer: AgentId,
    pub consumer: AgentId,
    pub objective_ref: String,
    pub authority_scope_ref: String,
    pub authority_scope_digest: String,
    pub input_digests: BTreeSet<String>,
    pub artifact_digests: BTreeSet<String>,
    pub evidence: Vec<EvidenceReferenceV1>,
    pub assumptions: Vec<String>,
    pub unresolved_questions: Vec<String>,
    pub uncertainty: UncertaintyClassV1,
    pub acceptance_checks: Vec<String>,
    pub required_capabilities: BTreeSet<String>,
    pub privacy_classes: BTreeSet<String>,
    pub organization_generation: u64,
    pub organization_digest: String,
    pub policy_version: u64,
    pub policy_digest: String,
    pub packet_generation: u16,
    pub packet_digest: String,
    pub state: HandoffPacketStateV1,
    pub transition_history: Vec<HandoffPacketTransitionV1>,
    pub clarifications: Vec<HandoffClarificationV1>,
    pub consumption: Option<HandoffConsumptionV1>,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
}

impl HandoffPacketV1 {
    pub fn expected_digest(&self) -> Result<String, WorkflowError> {
        #[derive(Serialize)]
        struct Material<'a> {
            packet_id: &'a str,
            session_id: &'a str,
            work_item_id: &'a WorkItemId,
            producer: AgentId,
            consumer: AgentId,
            objective_ref: &'a str,
            authority_scope_ref: &'a str,
            authority_scope_digest: &'a str,
            input_digests: &'a BTreeSet<String>,
            artifact_digests: &'a BTreeSet<String>,
            evidence: &'a [EvidenceReferenceV1],
            assumptions: &'a [String],
            unresolved_questions: &'a [String],
            uncertainty: UncertaintyClassV1,
            acceptance_checks: &'a [String],
            required_capabilities: &'a BTreeSet<String>,
            privacy_classes: &'a BTreeSet<String>,
            organization_generation: u64,
            organization_digest: &'a str,
            policy_version: u64,
            policy_digest: &'a str,
            packet_generation: u16,
            created_at_unix_ms: u64,
        }
        canonical_sha256(
            "sentinel.workflow.handoff-packet.v1",
            &Material {
                packet_id: &self.packet_id,
                session_id: &self.session_id,
                work_item_id: &self.work_item_id,
                producer: self.producer,
                consumer: self.consumer,
                objective_ref: &self.objective_ref,
                authority_scope_ref: &self.authority_scope_ref,
                authority_scope_digest: &self.authority_scope_digest,
                input_digests: &self.input_digests,
                artifact_digests: &self.artifact_digests,
                evidence: &self.evidence,
                assumptions: &self.assumptions,
                unresolved_questions: &self.unresolved_questions,
                uncertainty: self.uncertainty,
                acceptance_checks: &self.acceptance_checks,
                required_capabilities: &self.required_capabilities,
                privacy_classes: &self.privacy_classes,
                organization_generation: self.organization_generation,
                organization_digest: &self.organization_digest,
                policy_version: self.policy_version,
                policy_digest: &self.policy_digest,
                packet_generation: self.packet_generation,
                created_at_unix_ms: self.created_at_unix_ms,
            },
        )
    }

    pub(crate) fn validate(&self) -> Result<(), WorkflowError> {
        validate_identifier(&self.packet_id)?;
        validate_identifier(&self.session_id)?;
        self.work_item_id.validate()?;
        validate_text(&self.objective_ref)?;
        validate_text(&self.authority_scope_ref)?;
        validate_digest(&self.authority_scope_digest)?;
        validate_text_collection(&self.assumptions, false)?;
        validate_text_collection(&self.unresolved_questions, false)?;
        validate_text_collection(&self.acceptance_checks, true)?;
        validate_digest(&self.organization_digest)?;
        validate_digest(&self.policy_digest)?;
        validate_digest(&self.packet_digest)?;
        if self.producer.0 == 0
            || self.consumer.0 == 0
            || self.producer == self.consumer
            || self.input_digests.is_empty()
            || self.evidence.is_empty()
            || self.required_capabilities.is_empty()
            || self.organization_generation == 0
            || self.policy_version == 0
            || self.packet_generation == 0
            || self.created_at_unix_ms == 0
            || self.updated_at_unix_ms < self.created_at_unix_ms
            || self.transition_history.is_empty()
            || self.transition_history.len() > MAX_COLLABORATION_RECORDS
            || self.clarifications.len() > 8
            || self.expected_digest()? != self.packet_digest
        {
            return Err(invalid("handoff packet is invalid"));
        }
        for digest in self.input_digests.iter().chain(&self.artifact_digests) {
            validate_digest(digest)?;
        }
        for evidence in &self.evidence {
            evidence.validate()?;
        }
        for capability in &self.required_capabilities {
            validate_identifier(capability)?;
        }
        for class in &self.privacy_classes {
            validate_identifier(class)?;
        }
        validate_clarifications(self)?;
        validate_handoff_history(self)?;
        match (self.state, &self.consumption) {
            (HandoffPacketStateV1::Consumed, Some(consumption)) => {
                validate_identifier(&consumption.subject_id)?;
                validate_digest(&consumption.subject_digest)?;
                if consumption.consumed_by != self.consumer
                    || consumption.consumed_at_unix_ms < self.created_at_unix_ms
                    || consumption.consumed_at_unix_ms > self.updated_at_unix_ms
                {
                    return Err(invalid("handoff consumption is invalid"));
                }
            }
            (HandoffPacketStateV1::Consumed, None) => {
                return Err(invalid("consumed handoff lacks evidence"))
            }
            (_, Some(_)) => return Err(invalid("unconsumed handoff has consumption evidence")),
            _ => {}
        }
        Ok(())
    }
}

fn validate_handoff_history(packet: &HandoffPacketV1) -> Result<(), WorkflowError> {
    let mut current = HandoffPacketStateV1::Prepared;
    let mut previous_sequence = 0_u64;
    for transition in &packet.transition_history {
        validate_text(&transition.reason_ref)?;
        if transition.sequence <= previous_sequence
            || transition.from != current
            || !legal_handoff_transition(transition.from, transition.to)
            || transition.actor.0 == 0
            || transition.occurred_at_unix_ms < packet.created_at_unix_ms
            || transition.occurred_at_unix_ms > packet.updated_at_unix_ms
        {
            return Err(invalid("handoff transition history is invalid"));
        }
        previous_sequence = transition.sequence;
        current = transition.to;
    }
    if current != packet.state {
        return Err(invalid("handoff state does not match transition history"));
    }
    Ok(())
}

fn legal_handoff_transition(from: HandoffPacketStateV1, to: HandoffPacketStateV1) -> bool {
    matches!(
        (from, to),
        (
            HandoffPacketStateV1::Prepared,
            HandoffPacketStateV1::Offered
        ) | (
            HandoffPacketStateV1::Offered,
            HandoffPacketStateV1::ClarificationRequested
                | HandoffPacketStateV1::Accepted
                | HandoffPacketStateV1::Rejected
                | HandoffPacketStateV1::Escalated
        ) | (
            HandoffPacketStateV1::ClarificationRequested,
            HandoffPacketStateV1::Offered | HandoffPacketStateV1::Escalated
        ) | (
            HandoffPacketStateV1::Accepted,
            HandoffPacketStateV1::Consumed
        )
    )
}

fn validate_clarifications(packet: &HandoffPacketV1) -> Result<(), WorkflowError> {
    let mut ids = BTreeSet::new();
    let mut basis = BTreeSet::new();
    for (index, clarification) in packet.clarifications.iter().enumerate() {
        validate_identifier(&clarification.clarification_id)?;
        validate_text(&clarification.question_ref)?;
        validate_digest(&clarification.basis_digest)?;
        if clarification.question_generation != u16::try_from(index + 1).unwrap_or(u16::MAX)
            || clarification.requested_by != packet.consumer
            || clarification.requested_at_unix_ms < packet.created_at_unix_ms
            || clarification.requested_at_unix_ms > packet.updated_at_unix_ms
            || !ids.insert(&clarification.clarification_id)
            || !basis.insert(&clarification.basis_digest)
        {
            return Err(invalid("handoff clarification is invalid"));
        }
        match (
            &clarification.answer_ref,
            &clarification.new_information_digest,
            clarification.answer_generation,
            clarification.answered_by,
            clarification.answered_at_unix_ms,
        ) {
            (Some(answer), Some(digest), Some(generation), Some(actor), Some(at)) => {
                validate_text(answer)?;
                validate_digest(digest)?;
                if generation != clarification.question_generation
                    || actor != packet.producer
                    || at < clarification.requested_at_unix_ms
                    || at > packet.updated_at_unix_ms
                {
                    return Err(invalid("handoff clarification answer is invalid"));
                }
            }
            (None, None, None, None, None) => {}
            _ => return Err(invalid("handoff clarification answer is incomplete")),
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DissentRecordV1 {
    pub dissent_id: String,
    pub session_id: String,
    pub decision_id: String,
    pub author: AgentId,
    pub claim_id: Option<String>,
    pub rationale_ref: String,
    pub evidence: Vec<EvidenceReferenceV1>,
    pub residual_risk_ref: String,
    pub dissent_digest: String,
    pub created_at_unix_ms: u64,
}

impl DissentRecordV1 {
    pub fn expected_digest(&self) -> Result<String, WorkflowError> {
        let mut material = self.clone();
        material.dissent_digest.clear();
        canonical_sha256("sentinel.workflow.dissent.v1", &material)
    }

    pub(crate) fn validate(&self) -> Result<(), WorkflowError> {
        validate_identifier(&self.dissent_id)?;
        validate_identifier(&self.session_id)?;
        validate_identifier(&self.decision_id)?;
        if let Some(claim_id) = &self.claim_id {
            validate_identifier(claim_id)?;
        }
        validate_text(&self.rationale_ref)?;
        validate_text(&self.residual_risk_ref)?;
        validate_digest(&self.dissent_digest)?;
        if self.author.0 == 0
            || self.evidence.is_empty()
            || self.created_at_unix_ms == 0
            || self.expected_digest()? != self.dissent_digest
        {
            return Err(invalid("dissent record is invalid"));
        }
        for evidence in &self.evidence {
            evidence.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionEvidenceLinkV1 {
    pub link_id: String,
    pub session_id: String,
    pub decision_id: String,
    pub claim_ids: BTreeSet<String>,
    pub dissent_ids: BTreeSet<String>,
    pub linked_by: AgentId,
    pub link_digest: String,
    pub created_at_unix_ms: u64,
}

impl DecisionEvidenceLinkV1 {
    pub fn expected_digest(&self) -> Result<String, WorkflowError> {
        let mut material = self.clone();
        material.link_digest.clear();
        canonical_sha256("sentinel.workflow.decision-evidence-link.v1", &material)
    }

    pub(crate) fn validate(&self) -> Result<(), WorkflowError> {
        validate_identifier(&self.link_id)?;
        validate_identifier(&self.session_id)?;
        validate_identifier(&self.decision_id)?;
        validate_digest(&self.link_digest)?;
        if self.claim_ids.is_empty()
            || self.linked_by.0 == 0
            || self.created_at_unix_ms == 0
            || self.expected_digest()? != self.link_digest
        {
            return Err(invalid("decision evidence link is invalid"));
        }
        for value in self.claim_ids.iter().chain(&self.dissent_ids) {
            validate_identifier(value)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollaborationSessionV1 {
    pub schema_version: u16,
    pub session_id: String,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub work_item_id: Option<WorkItemId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_contract_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collaboration_generation: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub admission_routes: Vec<crate::admission::CollaborationRouteV1>,
    pub organization_generation: u64,
    pub organization_digest: String,
    pub assignment_id: String,
    pub assignment_version: u64,
    pub assignment_digest: String,
    pub policy_version: u64,
    pub policy_digest: String,
    pub subject_ref: String,
    pub input_digest: String,
    pub mode: CollaborationModeV1,
    pub budget: CollaborationBudgetV1,
    pub participants: Vec<CollaborationParticipantV1>,
    pub state: CollaborationSessionStateV1,
    pub transition_sequence: u64,
    pub publication_revision: u64,
    pub binding_digest: String,
    pub claims: Vec<IndependentClaimV1>,
    pub transition_history: Vec<CollaborationTransitionV1>,
    pub created_by: AgentId,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
}

impl CollaborationSessionV1 {
    pub fn expected_binding_digest(&self) -> Result<String, WorkflowError> {
        #[derive(Serialize)]
        struct LegacyMaterial<'a> {
            schema_version: u16,
            session_id: &'a str,
            tenant_id: &'a TenantId,
            project_id: &'a ProjectId,
            work_item_id: &'a Option<WorkItemId>,
            organization_generation: u64,
            organization_digest: &'a str,
            assignment_id: &'a str,
            assignment_version: u64,
            assignment_digest: &'a str,
            policy_version: u64,
            policy_digest: &'a str,
            subject_ref: &'a str,
            input_digest: &'a str,
            mode: CollaborationModeV1,
            budget: &'a CollaborationBudgetV1,
            participants: &'a [CollaborationParticipantV1],
            created_by: AgentId,
            created_at_unix_ms: u64,
        }
        let legacy = LegacyMaterial {
            schema_version: self.schema_version,
            session_id: &self.session_id,
            tenant_id: &self.tenant_id,
            project_id: &self.project_id,
            work_item_id: &self.work_item_id,
            organization_generation: self.organization_generation,
            organization_digest: &self.organization_digest,
            assignment_id: &self.assignment_id,
            assignment_version: self.assignment_version,
            assignment_digest: &self.assignment_digest,
            policy_version: self.policy_version,
            policy_digest: &self.policy_digest,
            subject_ref: &self.subject_ref,
            input_digest: &self.input_digest,
            mode: self.mode,
            budget: &self.budget,
            participants: &self.participants,
            created_by: self.created_by,
            created_at_unix_ms: self.created_at_unix_ms,
        };
        let (Some(admission_id), Some(admission_contract_digest), Some(collaboration_generation)) = (
            self.admission_id.as_deref(),
            self.admission_contract_digest.as_deref(),
            self.collaboration_generation,
        ) else {
            return canonical_sha256(
                "sentinel.workflow.collaboration-session-binding.v1",
                &legacy,
            );
        };
        canonical_sha256(
            "sentinel.workflow.collaboration-session-binding.v2",
            &(
                &legacy,
                admission_id,
                admission_contract_digest,
                collaboration_generation,
                &self.admission_routes,
            ),
        )
    }

    pub(crate) fn participant(
        &self,
        agent_id: AgentId,
    ) -> Result<&CollaborationParticipantV1, WorkflowError> {
        self.participants
            .iter()
            .find(|participant| participant.agent_id == agent_id)
            .ok_or_else(unauthorized)
    }

    fn exposure_barrier_opened(&self) -> bool {
        self.transition_history
            .iter()
            .any(|transition| transition.to == CollaborationSessionStateV1::ExchangingEvidence)
    }

    pub(crate) fn validate(&self) -> Result<(), WorkflowError> {
        if self.schema_version != COLLABORATION_SCHEMA_VERSION
            || self.organization_generation == 0
            || self.assignment_version == 0
            || self.policy_version == 0
            || self.transition_sequence == 0
            || self.publication_revision > self.transition_sequence
            || self.created_by.0 == 0
            || self.work_item_id.is_none()
            || self.created_at_unix_ms == 0
            || self.updated_at_unix_ms < self.created_at_unix_ms
            || (!self.state.is_terminal() && self.updated_at_unix_ms > self.budget.deadline_unix_ms)
            || self.participants.len() < 2
            || self.participants.len() > usize::from(self.budget.max_participants)
            || self.claims.len() > usize::from(self.budget.max_claims)
            || self.transition_history.len() > usize::from(self.budget.max_transitions)
        {
            return Err(invalid("collaboration session is invalid"));
        }
        match (
            self.admission_id.as_deref(),
            self.admission_contract_digest.as_deref(),
            self.collaboration_generation,
        ) {
            (None, None, None) if self.admission_routes.is_empty() => {}
            (Some(admission_id), Some(contract_digest), Some(generation)) => {
                validate_identifier(admission_id)?;
                validate_digest(contract_digest)?;
                if generation == 0 {
                    return Err(invalid("collaboration admission generation is invalid"));
                }
                if self.admission_routes.is_empty() {
                    return Err(invalid("collaboration admission routes are missing"));
                }
            }
            _ => return Err(invalid("collaboration admission binding is incomplete")),
        }
        validate_identifier(&self.session_id)?;
        self.tenant_id.validate()?;
        self.project_id.validate()?;
        if let Some(work_item_id) = &self.work_item_id {
            work_item_id.validate()?;
        }
        validate_digest(&self.organization_digest)?;
        validate_identifier(&self.assignment_id)?;
        validate_digest(&self.assignment_digest)?;
        validate_digest(&self.policy_digest)?;
        validate_text(&self.subject_ref)?;
        validate_digest(&self.input_digest)?;
        validate_digest(&self.binding_digest)?;
        self.budget.validate(self.created_at_unix_ms)?;
        if self.expected_binding_digest()? != self.binding_digest {
            return Err(invalid("collaboration binding digest is invalid"));
        }
        let mut members = BTreeSet::new();
        for participant in &self.participants {
            participant.validate()?;
            if !members.insert(participant.agent_id.0) {
                return Err(invalid("collaboration participant is duplicated"));
            }
        }
        for route in &self.admission_routes {
            if route.from == route.to
                || !members.contains(&route.from.0)
                || !members.contains(&route.to.0)
                || route.permitted_packet_classes.is_empty()
                || route.visibility
                    != crate::admission::CollaborationRouteVisibilityV1::PrivateDirected
            {
                return Err(invalid("collaboration admission route is invalid"));
            }
            for packet_class in &route.permitted_packet_classes {
                validate_identifier(packet_class)?;
            }
        }
        validate_session_history(self)?;
        let barrier_opened = self.exposure_barrier_opened();
        if barrier_opened && self.claims.len() != self.participants.len() {
            return Err(invalid("claim exposure barrier is incomplete"));
        }
        let required_exposure = if barrier_opened {
            ClaimExposureStateV1::Exposed
        } else {
            ClaimExposureStateV1::Private
        };
        let mut claims = BTreeSet::new();
        let mut contributors = BTreeSet::new();
        for claim in &self.claims {
            claim.validate()?;
            let participant = self.participant(claim.contributor)?;
            if claim.session_id != self.session_id
                || claim.mandate != participant.mandate
                || claim.capability_snapshot_digest != participant.capability_snapshot_digest
                || claim.input_digest != self.input_digest
                || !claims.insert(&claim.claim_id)
                || !contributors.insert(claim.contributor.0)
                || claim.exposure_state != required_exposure
            {
                return Err(invalid("independent claim binding is invalid"));
            }
        }
        Ok(())
    }
}

fn validate_session_history(session: &CollaborationSessionV1) -> Result<(), WorkflowError> {
    let mut previous_sequence = 1_u64;
    let mut current = CollaborationSessionStateV1::Planned;
    for transition in &session.transition_history {
        validate_text(&transition.reason_ref)?;
        if transition.actor != session.created_by {
            session.participant(transition.actor)?;
        }
        if transition.sequence <= previous_sequence
            || transition.sequence > session.transition_sequence
            || transition.from != current
            || !legal_session_transition(transition.from, transition.to)
            || transition.occurred_at_unix_ms < session.created_at_unix_ms
            || transition.occurred_at_unix_ms > session.updated_at_unix_ms
        {
            return Err(invalid("collaboration transition history is invalid"));
        }
        previous_sequence = transition.sequence;
        current = transition.to;
    }
    if current != session.state {
        return Err(invalid(
            "collaboration state does not match transition history",
        ));
    }
    Ok(())
}

pub(crate) fn legal_session_transition(
    from: CollaborationSessionStateV1,
    to: CollaborationSessionStateV1,
) -> bool {
    matches!(
        (from, to),
        (
            CollaborationSessionStateV1::Planned,
            CollaborationSessionStateV1::CollectingIndependentClaims
        ) | (
            CollaborationSessionStateV1::CollectingIndependentClaims,
            CollaborationSessionStateV1::ExchangingEvidence
        ) | (
            CollaborationSessionStateV1::ExchangingEvidence,
            CollaborationSessionStateV1::Deciding
        ) | (
            CollaborationSessionStateV1::Deciding,
            CollaborationSessionStateV1::Completed
        ) | (
            CollaborationSessionStateV1::Planned
                | CollaborationSessionStateV1::CollectingIndependentClaims
                | CollaborationSessionStateV1::ExchangingEvidence
                | CollaborationSessionStateV1::Deciding,
            CollaborationSessionStateV1::Blocked
                | CollaborationSessionStateV1::Escalated
                | CollaborationSessionStateV1::Cancelled
        )
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "record", content = "value")]
pub enum CollaborationEventRecordV1 {
    Session(CollaborationSessionV1),
    Admission(Box<CollaborationAdmissionDecisionV1>),
    Claim(IndependentClaimV1),
    Handoff(HandoffPacketV1),
    Dissent(DissentRecordV1),
    DecisionEvidence(DecisionEvidenceLinkV1),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollaborationEventPayloadV1 {
    pub schema_version: u16,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub session_id: String,
    pub transition_sequence: u64,
    pub command_digest: String,
    pub record: CollaborationEventRecordV1,
}

impl CollaborationEventPayloadV1 {
    pub fn validate(&self) -> Result<(), WorkflowError> {
        if self.schema_version != COLLABORATION_SCHEMA_VERSION || self.transition_sequence == 0 {
            return Err(invalid("collaboration event payload is invalid"));
        }
        self.tenant_id.validate()?;
        self.project_id.validate()?;
        validate_identifier(&self.session_id)?;
        validate_digest(&self.command_digest)?;
        match &self.record {
            CollaborationEventRecordV1::Session(value) => {
                value.validate()?;
                if value.session_id != self.session_id
                    || value.tenant_id != self.tenant_id
                    || value.project_id != self.project_id
                {
                    return Err(invalid("collaboration event session mismatch"));
                }
            }
            CollaborationEventRecordV1::Admission(value) => {
                value.validate(value.updated_at_unix_ms)?;
                if value.admission_id != self.session_id
                    || value.input.tenant_id != self.tenant_id
                    || value.input.project_id != self.project_id
                    || value.transition_sequence != self.transition_sequence
                {
                    return Err(invalid("collaboration event admission mismatch"));
                }
            }
            CollaborationEventRecordV1::Claim(value) => {
                value.validate()?;
                if value.session_id != self.session_id {
                    return Err(invalid("collaboration event claim mismatch"));
                }
            }
            CollaborationEventRecordV1::Handoff(value) => {
                value.validate()?;
                if value.session_id != self.session_id {
                    return Err(invalid("collaboration event handoff mismatch"));
                }
            }
            CollaborationEventRecordV1::Dissent(value) => {
                value.validate()?;
                if value.session_id != self.session_id {
                    return Err(invalid("collaboration event dissent mismatch"));
                }
            }
            CollaborationEventRecordV1::DecisionEvidence(value) => {
                value.validate()?;
                if value.session_id != self.session_id {
                    return Err(invalid("collaboration event decision mismatch"));
                }
            }
        }
        Ok(())
    }
}

fn validate_collaboration_event_payload(payload: &[u8]) -> Result<(), EventContractError> {
    let value: CollaborationEventPayloadV1 =
        serde_json::from_slice(payload).map_err(|_| EventContractError::InvalidField {
            field: "collaboration_event.payload",
            reason: "must be canonical collaboration JSON",
        })?;
    value
        .validate()
        .map_err(|_| EventContractError::InvalidField {
            field: "collaboration_event.payload",
            reason: "failed collaboration contract validation",
        })
}

pub fn collaboration_event_schema_registry() -> Result<EventSchemaRegistry, EventContractError> {
    EventSchemaRegistry::new([EventSchemaDefinition {
        event_type: COLLABORATION_EVENT_TYPE.to_owned(),
        schema_version: COLLABORATION_EVENT_SCHEMA_VERSION,
        durability: EventDurability::Authoritative,
        payload_codec: EventPayloadCodec::Json,
        causation_policy: CausationPolicyV1::RootRequired,
        allowed_producers: BTreeSet::from([COLLABORATION_EVENT_PRODUCER.to_owned()]),
        deterministic_event_id_producers: BTreeSet::new(),
        validator_id: "company-collaboration-json-v1".to_owned(),
        validate_payload: validate_collaboration_event_payload,
        upcast: None,
    }])
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollaborationPublicationV1 {
    pub operation_id: Uuid,
    pub session_id: String,
    pub transition_sequence: u64,
    pub proposal: AppendProposalV2,
}

pub fn is_collaboration_command_v1(command: &CompanyWorkflowCommandV1) -> bool {
    matches!(
        command,
        CompanyWorkflowCommandV1::CreateCollaborationSession { .. }
            | CompanyWorkflowCommandV1::RecordIndependentClaim { .. }
            | CompanyWorkflowCommandV1::OpenClaimExposureBarrier { .. }
            | CompanyWorkflowCommandV1::OfferHandoffPacket { .. }
            | CompanyWorkflowCommandV1::RequestHandoffClarification { .. }
            | CompanyWorkflowCommandV1::AnswerHandoffClarification { .. }
            | CompanyWorkflowCommandV1::AcceptHandoffPacket { .. }
            | CompanyWorkflowCommandV1::RejectHandoffPacket { .. }
            | CompanyWorkflowCommandV1::ConsumeHandoffPacket { .. }
            | CompanyWorkflowCommandV1::RecordDissent { .. }
            | CompanyWorkflowCommandV1::LinkDecisionEvidence { .. }
            | CompanyWorkflowCommandV1::TransitionCollaborationSession { .. }
    )
}

impl CollaborationPublicationV1 {
    pub(crate) fn validate(&self) -> Result<(), WorkflowError> {
        validate_identifier(&self.session_id)?;
        self.proposal
            .validate()
            .map_err(|_| invalid("collaboration publication proposal is invalid"))?;
        if self.operation_id.to_string() != self.proposal.causal_context.operation_id
            || self.session_id != self.proposal.causal_context.correlation_id
            || self.transition_sequence != self.proposal.causal_context.source_generation
            || self.proposal.event_type != COLLABORATION_EVENT_TYPE
            || self.proposal.producer != COLLABORATION_EVENT_PRODUCER
        {
            return Err(invalid("collaboration publication binding is invalid"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollaborationViewV1 {
    pub session: CollaborationSessionV1,
    pub handoffs: Vec<HandoffPacketV1>,
    pub dissent: Vec<DissentRecordV1>,
    pub decision_evidence: Vec<DecisionEvidenceLinkV1>,
}

pub fn filtered_collaboration_view(
    project: &ProjectV1,
    principal: &AuthenticatedCompanyPrincipalV1,
    session_id: &str,
) -> Result<CollaborationViewV1, WorkflowError> {
    principal.validate()?;
    validate_identifier(session_id)?;
    if principal.tenant_id != project.tenant_id {
        return Err(unauthorized());
    }
    let source = project
        .collaboration_sessions
        .iter()
        .find(|session| session.session_id == session_id)
        .ok_or_else(|| invalid("collaboration session was not found"))?;
    source.validate()?;
    let admission = source
        .admission_id
        .as_ref()
        .map(|admission_id| {
            project
                .collaboration_admissions
                .iter()
                .find(|decision| decision.admission_id == *admission_id)
                .ok_or_else(unauthorized)
        })
        .transpose()?;
    let actor = match principal.kind {
        CompanyPrincipalKindV1::Operator => None,
        CompanyPrincipalKindV1::Agent => {
            let agent_id = principal.agent_id.ok_or_else(unauthorized)?;
            let governed_leader = project.governance.participants.iter().any(|participant| {
                participant.agent_id == agent_id
                    && participant.principal_id == principal.principal_id
                    && participant.role == principal.role
                    && matches!(
                        participant.role,
                        CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
                    )
            });
            if governed_leader {
                return Ok(CollaborationViewV1 {
                    session: source.clone(),
                    handoffs: project
                        .handoff_packets
                        .iter()
                        .filter(|packet| packet.session_id == session_id)
                        .cloned()
                        .collect(),
                    dissent: project
                        .dissent_records
                        .iter()
                        .filter(|record| record.session_id == session_id)
                        .cloned()
                        .collect(),
                    decision_evidence: project
                        .decision_evidence
                        .iter()
                        .filter(|record| record.session_id == session_id)
                        .cloned()
                        .collect(),
                });
            }
            let participant = source.participant(agent_id)?;
            if participant.permanent_role != principal.role {
                return Err(unauthorized());
            }
            Some(agent_id)
        }
        CompanyPrincipalKindV1::Customer => return Err(unauthorized()),
    };
    let peers_exposed = source.exposure_barrier_opened();
    let mut session = source.clone();
    if let Some(agent_id) = actor {
        let observer = source.participant(agent_id)?;
        session.claims.retain(|claim| {
            claim.contributor == agent_id
                || (claim.exposure_state == ClaimExposureStateV1::Exposed
                    && admission.is_none_or(|decision| {
                        collaboration_route_allows(
                            decision,
                            claim.contributor,
                            agent_id,
                            "evidence",
                        )
                    })
                    && source
                        .participant(claim.contributor)
                        .is_ok_and(|contributor| {
                            contributor
                                .privacy_classes
                                .is_subset(&observer.privacy_classes)
                        }))
        });
    }
    let visible_claim_ids = session
        .claims
        .iter()
        .map(|claim| claim.claim_id.as_str())
        .collect::<BTreeSet<_>>();
    let handoffs = project
        .handoff_packets
        .iter()
        .filter(|packet| {
            packet.session_id == session_id
                && actor.is_none_or(|agent_id| {
                    packet.producer == agent_id || packet.consumer == agent_id
                })
        })
        .cloned()
        .collect();
    let dissent = project
        .dissent_records
        .iter()
        .filter(|record| {
            record.session_id == session_id
                && actor.is_none_or(|agent_id| {
                    if record.author == agent_id {
                        return true;
                    }
                    if !peers_exposed {
                        return false;
                    }
                    if admission.is_some_and(|decision| {
                        !collaboration_route_allows(decision, record.author, agent_id, "finding")
                    }) {
                        return false;
                    }
                    let Ok(observer) = source.participant(agent_id) else {
                        return false;
                    };
                    source.participant(record.author).is_ok_and(|author| {
                        author.privacy_classes.is_subset(&observer.privacy_classes)
                    })
                })
        })
        .cloned()
        .collect::<Vec<_>>();
    let visible_dissent_ids = dissent
        .iter()
        .map(|record| record.dissent_id.as_str())
        .collect::<BTreeSet<_>>();
    let decision_evidence = project
        .decision_evidence
        .iter()
        .filter(|record| {
            record.session_id == session_id
                && actor.is_none_or(|_| {
                    peers_exposed
                        && record
                            .claim_ids
                            .iter()
                            .all(|claim_id| visible_claim_ids.contains(claim_id.as_str()))
                        && record
                            .dissent_ids
                            .iter()
                            .all(|dissent_id| visible_dissent_ids.contains(dissent_id.as_str()))
                })
        })
        .cloned()
        .collect();
    Ok(CollaborationViewV1 {
        session,
        handoffs,
        dissent,
        decision_evidence,
    })
}

fn collaboration_route_allows(
    decision: &CollaborationAdmissionDecisionV1,
    from: AgentId,
    to: AgentId,
    packet_class: &str,
) -> bool {
    decision.routes.iter().any(|route| {
        route.from == from
            && route.to == to
            && route.visibility == crate::admission::CollaborationRouteVisibilityV1::PrivateDirected
            && route.permitted_packet_classes.contains(packet_class)
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayPromptMessageV1 {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollaborationGatewayRequestV1 {
    pub schema_version: u16,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub work_item_id: Option<WorkItemId>,
    pub session_id: String,
    pub agent_id: AgentId,
    pub permanent_role: CompanyRoleV1,
    pub mandate: BehaviorMandateV1,
    pub session_binding_digest: String,
    pub input_digest: String,
    pub visible_claim_digests: Vec<String>,
    pub messages: Vec<GatewayPromptMessageV1>,
}

pub fn compile_collaboration_gateway_request(
    project: &ProjectV1,
    session_id: &str,
    agent_id: AgentId,
    now_ms: u64,
) -> Result<CollaborationGatewayRequestV1, WorkflowError> {
    validate_identifier(session_id)?;
    let mut matches = project
        .collaboration_sessions
        .iter()
        .filter(|session| session.session_id == session_id);
    let session = matches.next().ok_or_else(unauthorized)?;
    if matches.next().is_some() {
        return Err(unauthorized());
    }
    crate::domain_store::validate_current_collaboration_runtime_authority(
        project, session, now_ms,
    )?;
    compile_collaboration_gateway_request_from_validated_session(session, agent_id)
}

pub fn authorize_collaboration_gateway_result(
    project: &ProjectV1,
    dispatched_request: &CollaborationGatewayRequestV1,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    if dispatched_request.schema_version != COLLABORATION_SCHEMA_VERSION {
        return Err(unauthorized());
    }
    let current = compile_collaboration_gateway_request(
        project,
        &dispatched_request.session_id,
        dispatched_request.agent_id,
        now_ms,
    )?;
    if current != *dispatched_request {
        return Err(unauthorized());
    }
    Ok(())
}

fn compile_collaboration_gateway_request_from_validated_session(
    session: &CollaborationSessionV1,
    agent_id: AgentId,
) -> Result<CollaborationGatewayRequestV1, WorkflowError> {
    session.validate()?;
    if session.state.is_terminal()
        || session.admission_id.is_none()
        || session.admission_contract_digest.is_none()
        || session.collaboration_generation.is_none()
        || session.admission_routes.is_empty()
    {
        return Err(unauthorized());
    }
    let participant = session.participant(agent_id)?;
    let contract = participant.mandate.contract();
    let visible_claim_digests = session
        .claims
        .iter()
        .filter(|claim| {
            claim.contributor == agent_id
                || (claim.exposure_state == ClaimExposureStateV1::Exposed
                    && (session.admission_routes.is_empty()
                        || session.admission_routes.iter().any(|route| {
                            route.from == claim.contributor
                                && route.to == agent_id
                                && route.permitted_packet_classes.contains("evidence")
                        }))
                    && session
                        .participant(claim.contributor)
                        .is_ok_and(|contributor| {
                            contributor
                                .privacy_classes
                                .is_subset(&participant.privacy_classes)
                        }))
        })
        .map(|claim| claim.claim_digest.clone())
        .collect::<Vec<_>>();
    let required_inputs = contract.required_inputs.join(", ");
    let forbidden = contract.forbidden_actions.join("; ");
    let permanent_role = format!("{:?}", participant.permanent_role).to_ascii_lowercase();
    let mandate = format!("{:?}", participant.mandate).to_ascii_lowercase();
    let system = format!(
        "You are a permanent {permanent_role} employee. Your task-local mandate is {mandate}. Required inputs: {required_inputs}. Expected output: {}. Stop when: {}. Forbidden: {forbidden}.",
        contract.expected_output, contract.stop_condition
    );
    let context = serde_json::to_string(&serde_json::json!({
        "session_id": session.session_id,
        "subject_ref": session.subject_ref,
        "input_digest": session.input_digest,
        "organization_generation": session.organization_generation,
        "policy_version": session.policy_version,
        "transition_sequence": session.transition_sequence,
        "visible_claim_digests": &visible_claim_digests,
    }))
    .map_err(|_| invalid("collaboration prompt context is invalid"))?;
    Ok(CollaborationGatewayRequestV1 {
        schema_version: COLLABORATION_SCHEMA_VERSION,
        tenant_id: session.tenant_id.clone(),
        project_id: session.project_id.clone(),
        work_item_id: session.work_item_id.clone(),
        session_id: session.session_id.clone(),
        agent_id,
        permanent_role: participant.permanent_role,
        mandate: participant.mandate,
        session_binding_digest: session.binding_digest.clone(),
        input_digest: session.input_digest.clone(),
        visible_claim_digests,
        messages: vec![
            GatewayPromptMessageV1 {
                role: "system".to_owned(),
                content: system,
            },
            GatewayPromptMessageV1 {
                role: "user".to_owned(),
                content: context,
            },
        ],
    })
}

fn invalid(message: &'static str) -> WorkflowError {
    WorkflowError::new(WorkflowErrorCode::InvalidInput, false, message)
}

fn unauthorized() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::AuthorityConflict,
        false,
        "collaboration authority is invalid or stale",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::COMPANY_DOMAIN_SCHEMA_VERSION;

    const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn participant(
        agent_id: u16,
        role: CompanyRoleV1,
        mandate: BehaviorMandateV1,
    ) -> CollaborationParticipantV1 {
        CollaborationParticipantV1 {
            agent_id: AgentId(agent_id),
            permanent_role: role,
            mandate,
            capability_snapshot_digest: DIGEST.to_owned(),
            capabilities: BTreeSet::from(["source-analysis".to_owned()]),
            privacy_classes: BTreeSet::from(["project-internal".to_owned()]),
        }
    }

    fn legacy_session(first_mandate: BehaviorMandateV1) -> CollaborationSessionV1 {
        let mut session = CollaborationSessionV1 {
            schema_version: COLLABORATION_SCHEMA_VERSION,
            session_id: "collaboration-a".to_owned(),
            tenant_id: TenantId::parse("tenant-a").unwrap(),
            project_id: ProjectId::parse("project-a").unwrap(),
            work_item_id: Some(WorkItemId::parse("work-a").unwrap()),
            admission_id: None,
            admission_contract_digest: None,
            collaboration_generation: None,
            admission_routes: Vec::new(),
            organization_generation: 7,
            organization_digest: DIGEST.to_owned(),
            assignment_id: "assignment-a".to_owned(),
            assignment_version: 1,
            assignment_digest: DIGEST.to_owned(),
            policy_version: 3,
            policy_digest: DIGEST.to_owned(),
            subject_ref: "review the bounded implementation".to_owned(),
            input_digest: DIGEST.to_owned(),
            mode: CollaborationModeV1::IndependentReview,
            budget: CollaborationBudgetV1 {
                max_participants: 2,
                max_claims: 2,
                max_handoffs: 2,
                max_clarification_rounds: 2,
                max_transitions: 16,
                deadline_unix_ms: 100,
            },
            participants: vec![
                participant(1, CompanyRoleV1::ProjectManager, first_mandate),
                participant(2, CompanyRoleV1::Developer, BehaviorMandateV1::Verify),
            ],
            state: CollaborationSessionStateV1::Planned,
            transition_sequence: 1,
            publication_revision: 0,
            binding_digest: String::new(),
            claims: Vec::new(),
            transition_history: Vec::new(),
            created_by: AgentId(1),
            created_at_unix_ms: 1,
            updated_at_unix_ms: 1,
        };
        session.binding_digest = session.expected_binding_digest().unwrap();
        session
    }

    fn bind_admission(mut session: CollaborationSessionV1) -> CollaborationSessionV1 {
        session.admission_id = Some("admission-a".to_owned());
        session.admission_contract_digest = Some(DIGEST.to_owned());
        session.collaboration_generation = Some(2);
        session.admission_routes = vec![
            crate::admission::CollaborationRouteV1 {
                from: AgentId(1),
                to: AgentId(2),
                permitted_packet_classes: BTreeSet::from([
                    "evidence".to_owned(),
                    "finding".to_owned(),
                    "handoff".to_owned(),
                ]),
                visibility: crate::admission::CollaborationRouteVisibilityV1::PrivateDirected,
            },
            crate::admission::CollaborationRouteV1 {
                from: AgentId(2),
                to: AgentId(1),
                permitted_packet_classes: BTreeSet::from([
                    "evidence".to_owned(),
                    "finding".to_owned(),
                    "handoff".to_owned(),
                ]),
                visibility: crate::admission::CollaborationRouteVisibilityV1::PrivateDirected,
            },
        ];
        session.binding_digest = session.expected_binding_digest().unwrap();
        session
    }

    fn session(first_mandate: BehaviorMandateV1) -> CollaborationSessionV1 {
        legacy_session(first_mandate)
    }

    fn admitted_session(first_mandate: BehaviorMandateV1) -> CollaborationSessionV1 {
        bind_admission(legacy_session(first_mandate))
    }

    fn principal(agent_id: u16, role: CompanyRoleV1) -> AuthenticatedCompanyPrincipalV1 {
        AuthenticatedCompanyPrincipalV1 {
            schema_version: COMPANY_DOMAIN_SCHEMA_VERSION,
            tenant_id: TenantId::parse("tenant-a").unwrap(),
            principal_id: format!("agent-{agent_id}"),
            kind: CompanyPrincipalKindV1::Agent,
            role,
            customer_id: None,
            agent_id: Some(AgentId(agent_id)),
            authority_generation: 1,
            authority_digest: DIGEST.to_owned(),
        }
    }

    fn claim(session: &CollaborationSessionV1, agent_id: u16) -> IndependentClaimV1 {
        let member = session.participant(AgentId(agent_id)).unwrap();
        let mut claim = IndependentClaimV1 {
            claim_id: format!("claim-{agent_id}"),
            session_id: session.session_id.clone(),
            contributor: member.agent_id,
            mandate: member.mandate,
            conclusion_ref: format!("bounded conclusion {agent_id}"),
            evidence: vec![EvidenceReferenceV1 {
                reference: format!("evidence-{agent_id}"),
                digest: DIGEST.to_owned(),
            }],
            assumptions: vec!["source remains stable".to_owned()],
            uncertainty: UncertaintyClassV1::Material,
            confidence_basis: "reproducible evidence".to_owned(),
            capability_snapshot_digest: member.capability_snapshot_digest.clone(),
            input_digest: session.input_digest.clone(),
            exposure_state: ClaimExposureStateV1::Private,
            claim_digest: String::new(),
            created_at_unix_ms: 2,
        };
        claim.claim_digest = claim.expected_digest().unwrap();
        claim
    }

    #[test]
    fn every_behavior_mandate_compiles_role_mandate_inputs_and_stop_contract() {
        for mandate in [
            BehaviorMandateV1::Discover,
            BehaviorMandateV1::Implement,
            BehaviorMandateV1::Verify,
            BehaviorMandateV1::Challenge,
            BehaviorMandateV1::Synthesize,
            BehaviorMandateV1::Decide,
            BehaviorMandateV1::Escalate,
        ] {
            let session = admitted_session(mandate);
            let request =
                compile_collaboration_gateway_request_from_validated_session(&session, AgentId(1))
                    .unwrap();
            let contract = mandate.contract();
            assert_eq!(request.permanent_role, CompanyRoleV1::ProjectManager);
            assert_eq!(request.mandate, mandate);
            assert_eq!(request.messages.len(), 2);
            assert!(request.messages[0]
                .content
                .contains(&contract.expected_output));
            assert!(request.messages[0]
                .content
                .contains(&contract.stop_condition));
            for required in contract.required_inputs {
                assert!(request.messages[0].content.contains(&required));
            }
            for forbidden in contract.forbidden_actions {
                assert!(request.messages[0].content.contains(&forbidden));
            }
            assert!(request.visible_claim_digests.is_empty());
        }
    }

    #[test]
    fn claim_visibility_is_private_until_the_explicit_exposure_barrier() {
        let mut session = session(BehaviorMandateV1::Challenge);
        session.participants[0]
            .privacy_classes
            .insert("restricted-review".to_owned());
        session.binding_digest = session.expected_binding_digest().unwrap();
        session.state = CollaborationSessionStateV1::CollectingIndependentClaims;
        session.transition_sequence = 2;
        session.updated_at_unix_ms = 2;
        session.transition_history.push(CollaborationTransitionV1 {
            sequence: 2,
            from: CollaborationSessionStateV1::Planned,
            to: CollaborationSessionStateV1::CollectingIndependentClaims,
            actor: AgentId(1),
            reason_ref: "begin independent review".to_owned(),
            occurred_at_unix_ms: 2,
        });
        session.claims = vec![claim(&session, 1), claim(&session, 2)];
        session.validate().unwrap();

        let mut project: ProjectV1 = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "tenant_id": "tenant-a",
            "project_id": "project-a",
            "agreement_id": "agreement-a",
            "agreement_digest": DIGEST,
            "governance": {
                "owner": 1,
                "participants": [],
                "project_profile": {"profile_id":"profile-a","generation":1,"digest":DIGEST}
            },
            "cost_ceiling_micros": 1,
            "provider_cost_ceilings_micros": {},
            "lifecycle_state": "active",
            "reserved_cost_micros": 0,
            "committed_cost_micros": 0,
            "work_items": {},
            "decisions": [],
            "handoffs": [],
            "blockers": [],
            "approvals": [],
            "reservations": [],
            "rooms": [],
            "questions": [],
            "actions": [],
            "collaboration_schema_version": 1,
            "collaboration_sessions": [],
            "version": 1,
            "created_at_unix_ms": 1,
            "updated_at_unix_ms": 2
        }))
        .unwrap();
        project.collaboration_sessions.push(session.clone());

        let private = filtered_collaboration_view(
            &project,
            &principal(1, CompanyRoleV1::ProjectManager),
            &session.session_id,
        )
        .unwrap();
        assert_eq!(private.session.claims.len(), 1);
        assert_eq!(private.session.claims[0].contributor, AgentId(1));

        let exposed_session = &mut project.collaboration_sessions[0];
        for value in &mut exposed_session.claims {
            value.exposure_state = ClaimExposureStateV1::Exposed;
        }
        exposed_session.state = CollaborationSessionStateV1::ExchangingEvidence;
        exposed_session.transition_sequence = 3;
        exposed_session.updated_at_unix_ms = 3;
        exposed_session
            .transition_history
            .push(CollaborationTransitionV1 {
                sequence: 3,
                from: CollaborationSessionStateV1::CollectingIndependentClaims,
                to: CollaborationSessionStateV1::ExchangingEvidence,
                actor: AgentId(1),
                reason_ref: "all claims are committed".to_owned(),
                occurred_at_unix_ms: 3,
            });
        let exposed = filtered_collaboration_view(
            &project,
            &principal(1, CompanyRoleV1::ProjectManager),
            &session.session_id,
        )
        .unwrap();
        assert_eq!(exposed.session.claims.len(), 2);

        let privacy_filtered = filtered_collaboration_view(
            &project,
            &principal(2, CompanyRoleV1::Developer),
            &session.session_id,
        )
        .unwrap();
        assert_eq!(privacy_filtered.session.claims.len(), 1);
        assert_eq!(privacy_filtered.session.claims[0].contributor, AgentId(2));

        let gateway_session = bind_admission(project.collaboration_sessions[0].clone());
        let gateway_request = compile_collaboration_gateway_request_from_validated_session(
            &gateway_session,
            AgentId(2),
        )
        .unwrap();
        assert_eq!(gateway_request.visible_claim_digests.len(), 1);
        assert_eq!(
            gateway_request.visible_claim_digests[0],
            project.collaboration_sessions[0].claims[1].claim_digest
        );
    }

    #[test]
    fn governed_leadership_can_supervise_without_inflating_the_task_team() {
        let mut session = legacy_session(BehaviorMandateV1::Challenge);
        session.created_by = AgentId(3);
        session.binding_digest = session.expected_binding_digest().unwrap();
        session.validate().unwrap();

        let mut project: ProjectV1 = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "tenant_id": "tenant-a",
            "project_id": "project-a",
            "agreement_id": "agreement-a",
            "agreement_digest": DIGEST,
            "governance": {
                "owner": 3,
                "participants": [{
                    "agent_id": 3,
                    "principal_id": "agent-3",
                    "role": "project_manager",
                    "specialties": ["project_planning"],
                    "reports_to": null,
                    "profile": {"profile_id":"profile-a","generation":1,"digest":DIGEST}
                }],
                "project_profile": {"profile_id":"profile-a","generation":1,"digest":DIGEST}
            },
            "cost_ceiling_micros": 1,
            "provider_cost_ceilings_micros": {},
            "lifecycle_state": "active",
            "reserved_cost_micros": 0,
            "committed_cost_micros": 0,
            "work_items": {},
            "decisions": [],
            "handoffs": [],
            "blockers": [],
            "approvals": [],
            "reservations": [],
            "rooms": [],
            "questions": [],
            "actions": [],
            "collaboration_schema_version": 1,
            "collaboration_sessions": [],
            "version": 1,
            "created_at_unix_ms": 1,
            "updated_at_unix_ms": 1
        }))
        .unwrap();
        project.collaboration_sessions.push(session.clone());

        let view = filtered_collaboration_view(
            &project,
            &principal(3, CompanyRoleV1::ProjectManager),
            &session.session_id,
        )
        .unwrap();
        assert_eq!(view.session, session);
        assert_eq!(view.session.participants.len(), 2);
        assert_eq!(
            filtered_collaboration_view(
                &project,
                &principal(4, CompanyRoleV1::Developer),
                &view.session.session_id,
            )
            .unwrap_err()
            .code,
            WorkflowErrorCode::AuthorityConflict
        );
    }

    #[test]
    fn terminal_before_barrier_preserves_private_claims_and_rejects_mixed_exposure() {
        let mut session = session(BehaviorMandateV1::Challenge);
        session.state = CollaborationSessionStateV1::Cancelled;
        session.transition_sequence = 3;
        session.updated_at_unix_ms = 3;
        session.transition_history = vec![
            CollaborationTransitionV1 {
                sequence: 2,
                from: CollaborationSessionStateV1::Planned,
                to: CollaborationSessionStateV1::CollectingIndependentClaims,
                actor: AgentId(1),
                reason_ref: "begin independent review".to_owned(),
                occurred_at_unix_ms: 2,
            },
            CollaborationTransitionV1 {
                sequence: 3,
                from: CollaborationSessionStateV1::CollectingIndependentClaims,
                to: CollaborationSessionStateV1::Cancelled,
                actor: AgentId(1),
                reason_ref: "stop before exposure".to_owned(),
                occurred_at_unix_ms: 3,
            },
        ];
        session.claims = vec![claim(&session, 1), claim(&session, 2)];
        session.validate().unwrap();
        assert_eq!(
            compile_collaboration_gateway_request_from_validated_session(&session, AgentId(1))
                .unwrap_err()
                .code,
            WorkflowErrorCode::AuthorityConflict
        );

        session.claims[1].exposure_state = ClaimExposureStateV1::Exposed;
        assert!(session.validate().is_err());
    }

    #[test]
    fn legacy_session_binding_is_stable_when_admission_fields_are_absent() {
        let legacy = legacy_session(BehaviorMandateV1::Discover);
        let expected = legacy.expected_binding_digest().unwrap();
        let mut value = serde_json::to_value(&legacy).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("admission_id");
        object.remove("admission_contract_digest");
        object.remove("collaboration_generation");
        object.remove("admission_routes");

        let decoded: CollaborationSessionV1 = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.expected_binding_digest().unwrap(), expected);
        assert_eq!(decoded.binding_digest, expected);
        decoded.validate().unwrap();
        assert_eq!(
            compile_collaboration_gateway_request_from_validated_session(&decoded, AgentId(1))
                .unwrap_err()
                .code,
            WorkflowErrorCode::AuthorityConflict
        );
    }

    #[test]
    fn event_registry_and_payload_reject_cross_project_session_rebinding() {
        let session = session(BehaviorMandateV1::Discover);
        let payload = CollaborationEventPayloadV1 {
            schema_version: COLLABORATION_SCHEMA_VERSION,
            tenant_id: session.tenant_id.clone(),
            project_id: session.project_id.clone(),
            session_id: session.session_id.clone(),
            transition_sequence: 1,
            command_digest: DIGEST.to_owned(),
            record: CollaborationEventRecordV1::Session(session.clone()),
        };
        payload.validate().unwrap();
        collaboration_event_schema_registry().unwrap();

        let mut rebound = payload;
        rebound.project_id = ProjectId::parse("project-b").unwrap();
        assert_eq!(
            rebound.validate().unwrap_err().code,
            WorkflowErrorCode::InvalidInput
        );
    }

    #[test]
    fn every_handoff_gap_class_has_a_stable_wire_value() {
        for (gap, wire) in [
            (HandoffGapClassV1::DataGap, "\"data_gap\""),
            (HandoffGapClassV1::SignalCorruption, "\"signal_corruption\""),
            (HandoffGapClassV1::ReferentialDrift, "\"referential_drift\""),
            (HandoffGapClassV1::CapabilityGap, "\"capability_gap\""),
        ] {
            assert_eq!(serde_json::to_string(&gap).unwrap(), wire);
            assert_eq!(
                serde_json::from_str::<HandoffGapClassV1>(wire).unwrap(),
                gap
            );
        }
    }

    #[test]
    fn legacy_project_without_collaboration_fields_decodes_without_permissive_defaults() {
        let project: ProjectV1 = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "tenant_id": "tenant-a",
            "project_id": "project-a",
            "agreement_id": "agreement-a",
            "agreement_digest": DIGEST,
            "governance": {
                "owner": 1,
                "participants": [],
                "project_profile": {"profile_id":"profile-a","generation":1,"digest":DIGEST}
            },
            "cost_ceiling_micros": 1,
            "provider_cost_ceilings_micros": {},
            "lifecycle_state": "active",
            "reserved_cost_micros": 0,
            "committed_cost_micros": 0,
            "work_items": {},
            "decisions": [],
            "handoffs": [],
            "blockers": [],
            "approvals": [],
            "reservations": [],
            "rooms": [],
            "questions": [],
            "actions": [],
            "version": 1,
            "created_at_unix_ms": 1,
            "updated_at_unix_ms": 1
        }))
        .unwrap();
        assert_eq!(project.collaboration_schema_version, None);
        assert!(project.collaboration_sessions.is_empty());
        assert!(project.handoff_packets.is_empty());
        assert!(project.dissent_records.is_empty());
        assert!(project.decision_evidence.is_empty());
        assert!(project.collaboration_publications.is_empty());
    }
}
