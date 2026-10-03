//! Internal adaptive execution journal. References do not confer tool or provider authority.

use std::collections::BTreeSet;

use sentinel_common::WorkbenchTool;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::digest::{canonical_sha256, validate_sha256};
use crate::{CompanyRoleV1, RuntimeAuthoritySnapshotV1, WorkflowError, WorkflowErrorCode};

pub const ADAPTIVE_SESSION_MAX_CALLS: u16 = 64;
pub const ADAPTIVE_SCHEMA_MAX_CORRECTIONS: u16 = 2;
pub const ADAPTIVE_TOOL_MAX_BYTES: usize = 256 * 1024;
pub const ADAPTIVE_CONTINUATION_MAX_WINDOWS: usize = 3;
pub const ADAPTIVE_CONTINUATION_MAX_WINDOW_MS: u64 = 300_000;
pub const ADAPTIVE_WORKING_MEMORY_MAX_ROWS: usize = 16;
pub const ADAPTIVE_WORKING_MEMORY_MAX_LABEL_BYTES: usize = 256;
pub const ADAPTIVE_WORKING_MEMORY_MAX_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdaptiveContinuationSourceV1 {
    Blocked {
        reason_code: String,
    },
    BlockedResolved {
        reason_code: String,
        resolution_event_id: String,
    },
    ModelUnknown,
    BudgetWindowExhausted {
        active_allowance_digest: String,
        continuation_history_digest: String,
    },
}

/// A reviewed authorization, never a retry of the original provider request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveContinuationAuthorizationV1 {
    pub schema_version: u16,
    pub operation_id: Uuid,
    pub review_id: Uuid,
    pub resolution_event_id: Uuid,
    pub session_id: Uuid,
    pub source_session_version: u64,
    pub source: AdaptiveContinuationSourceV1,
    pub abandoned_model_effect: Option<AdaptiveEffectV1>,
    pub provider_allowance_id: String,
    pub provider_authority_digest: String,
    pub issued_at_ms: u64,
    pub deadline_ms: u64,
    pub additional_model_calls: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_adoption: Option<Box<crate::AdaptiveLeadershipLocalAdoptionV1>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_policy: Option<Box<crate::AdaptiveResumePolicyBindingV1>>,
}

impl AdaptiveContinuationAuthorizationV1 {
    pub fn validate(&self) -> Result<(), WorkflowError> {
        if self.schema_version != 1
            || self.operation_id.is_nil()
            || self.review_id.is_nil()
            || self.resolution_event_id.is_nil()
            || self.session_id.is_nil()
            || self.source_session_version == 0
            || !valid_identifier(&self.provider_allowance_id)
            || !validate_sha256(&self.provider_authority_digest)
            || self.issued_at_ms == 0
            || self.deadline_ms <= self.issued_at_ms
            || self.deadline_ms - self.issued_at_ms < 1_000
            || self.deadline_ms - self.issued_at_ms > ADAPTIVE_CONTINUATION_MAX_WINDOW_MS
            || !(1..=ADAPTIVE_SESSION_MAX_CALLS).contains(&self.additional_model_calls)
            || self.abandoned_model_effect.as_ref().is_some_and(|effect| {
                effect.id.is_nil() || !validate_sha256(&effect.request_digest)
            })
        {
            return Err(invalid());
        }
        if let Some(binding) = &self.resume_policy {
            validate_resume_policy_binding(binding)?;
            let limits = &binding.limits;
            if self.local_adoption.is_some()
                || !matches!(
                    self.source,
                    AdaptiveContinuationSourceV1::ModelUnknown
                        | AdaptiveContinuationSourceV1::BudgetWindowExhausted { .. }
                )
                || self.deadline_ms > limits.expires_at_unix_ms
                || self.deadline_ms - self.issued_at_ms > limits.max_window_ms
                || limits
                    .max_call_duration_ms
                    .checked_add(limits.dispatch_margin_ms)
                    .is_none_or(|minimum| self.deadline_ms - self.issued_at_ms < minimum)
            {
                return Err(invalid());
            }
        }
        match (&self.source, &self.abandoned_model_effect) {
            (AdaptiveContinuationSourceV1::Blocked { reason_code }, None)
                if valid_reason(reason_code) => {}
            (
                AdaptiveContinuationSourceV1::BlockedResolved {
                    reason_code,
                    resolution_event_id,
                },
                None,
            ) if valid_reason(reason_code) && valid_resolution(resolution_event_id) => {}
            (AdaptiveContinuationSourceV1::ModelUnknown, Some(_)) => {}
            (
                AdaptiveContinuationSourceV1::BudgetWindowExhausted {
                    active_allowance_digest,
                    continuation_history_digest,
                },
                None,
            ) if validate_sha256(active_allowance_digest)
                && validate_sha256(continuation_history_digest)
                && self.local_adoption.is_none() => {}
            _ => return Err(invalid()),
        }
        if let Some(adoption) = &self.local_adoption {
            adoption.validate()?;
            let crate::AdaptiveLeadershipReviewDecisionKindV1::Continue {
                additional_model_calls,
                window_ms,
                ..
            } = &adoption.request.decision.decision
            else {
                return Err(invalid());
            };
            if adoption.request.session_id != self.session_id
                || adoption.request.review_id != self.review_id
                || adoption.issued_at_unix_ms != self.issued_at_ms
                || adoption.continuation_deadline_ms != self.deadline_ms
                || *additional_model_calls != self.additional_model_calls
                || self.deadline_ms - self.issued_at_ms != *window_ms
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
}

pub(crate) fn validate_resume_policy_binding(
    binding: &crate::AdaptiveResumePolicyBindingV1,
) -> Result<(), WorkflowError> {
    binding.validate()?;
    let limits = &binding.limits;
    if binding.schema_version != 1
        || !valid_identifier(&binding.policy_id)
        || !validate_sha256(&binding.receipt_digest)
        || binding.ordinal == 0
        || binding.ordinal > limits.total_review_ceiling
        || !(1..=128).contains(&limits.total_review_ceiling)
        || !(1_000..=120_000).contains(&limits.max_call_duration_ms)
        || !(1_000..=ADAPTIVE_CONTINUATION_MAX_WINDOW_MS).contains(&limits.max_window_ms)
        || limits.dispatch_margin_ms == 0
        || limits
            .max_call_duration_ms
            .checked_add(limits.dispatch_margin_ms)
            .is_none_or(|minimum| minimum > limits.max_window_ms)
        || limits.expires_at_unix_ms == 0
        || limits.expires_at_unix_ms > i64::MAX as u64
    {
        return Err(invalid());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdaptiveModelAdmissionV1 {
    Admissible,
    CallsExhausted,
    DeadlineExpired,
    InsufficientSlack,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveContinuationStateV1 {
    pub authorizations: Vec<AdaptiveContinuationAuthorizationV1>,
    pub model_ceiling: u16,
    pub observation_required: bool,
}

/// Matches the daemon's original allowance/authority provider digest encoding.
pub fn adaptive_continuation_provider_digest(
    allowance: &crate::SubscriptionCallAllowanceV1,
    current: &RuntimeAuthoritySnapshotV1,
) -> Result<String, WorkflowError> {
    use sha2::{Digest, Sha256};
    let authority_digest = current.canonical_digest()?;
    let bytes = serde_json::to_vec(&(allowance, &authority_digest)).map_err(|_| invalid())?;
    let mut hash = Sha256::new();
    hash.update(b"sentinel.workflow.adaptive-provider-authority.v1");
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
    Ok(format!("{:x}", hash.finalize()))
}

/// Bounded rejection context, not a provider grant or successful work receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveRecoveryFeedbackV1 {
    /// Schema-correction rollovers already consumed by this exact authority.
    pub count: u16,
    pub reason_code: String,
    pub resolution_event_id: String,
    /// Session that supplied this rejection, including the current rejected head.
    pub previous_session_id: Uuid,
}

impl AdaptiveRecoveryFeedbackV1 {
    pub(crate) fn validate(&self) -> Result<(), WorkflowError> {
        if self.count > ADAPTIVE_SCHEMA_MAX_CORRECTIONS
            || !valid_reason(&self.reason_code)
            || !valid_resolution(&self.resolution_event_id)
            || self.previous_session_id.is_nil()
        {
            return Err(invalid());
        }
        Ok(())
    }
}

/// The composition layer must bind this journal to separately validated provider grants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveSessionGrantV1 {
    pub schema_version: u16,
    pub session_id: Uuid,
    pub authority: RuntimeAuthoritySnapshotV1,
    pub provider_allowance_id: String,
    pub provider_authority_digest: String,
    pub provider: String,
    pub model: String,
    pub catalog_digest: String,
    pub max_output_tokens: u32,
    pub max_call_duration_ms: u64,
    pub max_model_calls: u16,
    pub max_tool_calls: u16,
    pub created_at_ms: u64,
    pub deadline_ms: u64,
}

impl AdaptiveSessionGrantV1 {
    pub(crate) fn validate(&self) -> Result<(), WorkflowError> {
        self.authority.validate()?;
        if self.schema_version != 1
            || self.session_id.is_nil()
            || !valid_identifier(&self.provider_allowance_id)
            || !validate_sha256(&self.provider_authority_digest)
            || self.provider != "codex-cli"
            || !valid_identifier(&self.model)
            || !validate_sha256(&self.catalog_digest)
            || !(1..=32_768).contains(&self.max_output_tokens)
            || !(1_000..=120_000).contains(&self.max_call_duration_ms)
            || !(1..=ADAPTIVE_SESSION_MAX_CALLS).contains(&self.max_model_calls)
            || !(1..=ADAPTIVE_SESSION_MAX_CALLS).contains(&self.max_tool_calls)
            || self.created_at_ms >= self.deadline_ms
            || !self
                .authority
                .capabilities
                .contains("observation.retain_private")
        {
            return Err(invalid());
        }
        Ok(())
    }
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'-'))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveEffectV1 {
    pub id: Uuid,
    pub request_digest: String,
}

/// Trusted retained-result evidence, not a model decision or renewed allowance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveRejectedModelReceiptV1 {
    pub schema_version: u16,
    pub session_id: Uuid,
    pub source_session_version: u64,
    pub source_entry_digest: String,
    pub effect: AdaptiveEffectV1,
    pub resolution_event_id: Uuid,
    pub reason_code: String,
    pub reservation_digest: String,
    pub authority_binding_digest: String,
    pub completion_payload_digest: String,
    pub model_response_digest: String,
    pub context_digest: String,
    pub tool_digest: String,
    pub usage_event_id: String,
    pub usage_event_digest: String,
}

impl AdaptiveRejectedModelReceiptV1 {
    pub fn validate(&self) -> Result<(), WorkflowError> {
        if self.schema_version != 1
            || self.session_id.is_nil()
            || self.source_session_version < 2
            || self.source_session_version.checked_add(2).is_none()
            || self.effect.id.is_nil()
            || self.resolution_event_id.is_nil()
            || self.reason_code != "fresh_observation_required"
            || !valid_resolution(&self.usage_event_id)
            || Uuid::parse_str(&self.usage_event_id)
                .is_ok_and(|id| id.to_string() != self.usage_event_id)
            || [
                &self.source_entry_digest,
                &self.effect.request_digest,
                &self.reservation_digest,
                &self.authority_binding_digest,
                &self.completion_payload_digest,
                &self.model_response_digest,
                &self.context_digest,
                &self.tool_digest,
                &self.usage_event_digest,
            ]
            .into_iter()
            .any(|digest| !validate_sha256(digest))
        {
            return Err(invalid());
        }
        Ok(())
    }

    pub fn canonical_digest(&self) -> Result<String, WorkflowError> {
        self.validate()?;
        canonical_sha256("sentinel.workflow.adaptive-rejected-model-receipt.v1", self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveModelJournalRecordEvidenceV1 {
    pub session_version: u64,
    pub entry_digest: String,
    pub operation_id: Uuid,
    pub command_digest: String,
    pub recorded_at_ms: u64,
}

/// Journal facts only: not proof of provider purity, usage or retry authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveFirstUnknownModelJournalEvidenceV1 {
    pub schema_version: u16,
    pub root_grant: AdaptiveSessionGrantV1,
    pub root_entry_digest: String,
    pub root_recorded_at_ms: u64,
    pub effect: AdaptiveEffectV1,
    pub claim: AdaptiveModelJournalRecordEvidenceV1,
    pub seal: AdaptiveModelJournalRecordEvidenceV1,
    pub sealed_model_calls: u16,
    pub sealed_tool_calls: u16,
    pub observed_head_version: u64,
    pub observed_head_entry_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveObservationRefV1 {
    pub effect: AdaptiveEffectV1,
    pub observation_digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdaptiveWorkingMemoryToolKindV1 {
    ListDirectory,
    InspectFile,
    WriteFile,
    ApplyPatch,
    RunCommand,
    RunTests,
    PackageArtifact,
}

/// Historical action pointers only: no tool contents, arguments or output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveWorkingMemoryCompletedRowV1 {
    pub session_version: u64,
    pub entry_digest: String,
    pub recorded_at_ms: u64,
    pub tool_kind: AdaptiveWorkingMemoryToolKindV1,
    pub tool_digest: String,
    pub target: Option<String>,
    pub program: Option<String>,
    pub suite_id: Option<String>,
    pub labels_omitted: bool,
    pub observation: AdaptiveObservationRefV1,
}

impl AdaptiveWorkingMemoryCompletedRowV1 {
    fn validate_labels(&self) -> Result<(), WorkflowError> {
        use AdaptiveWorkingMemoryToolKindV1 as Kind;
        let valid = match self.tool_kind {
            Kind::ListDirectory | Kind::InspectFile | Kind::WriteFile | Kind::ApplyPatch => {
                self.program.is_none()
                    && self.suite_id.is_none()
                    && self.target.as_ref().map_or(self.labels_omitted, |path| {
                        WorkbenchTool::ListDirectory {
                            path: path.clone(),
                            after: None,
                            max_entries: 1,
                        }
                        .validate_shape()
                        .is_ok()
                            && (self.tool_kind == Kind::ListDirectory || path != ".")
                    })
            }
            Kind::RunCommand | Kind::RunTests => {
                self.target.is_none()
                    && self
                        .program
                        .as_ref()
                        .map_or(self.labels_omitted, |program| {
                            WorkbenchTool::RunCommand {
                                program: program.clone(),
                                args: Vec::new(),
                            }
                            .validate_shape()
                            .is_ok()
                        })
                    && if self.tool_kind == Kind::RunTests {
                        self.suite_id
                            .as_ref()
                            .map_or(self.labels_omitted, |suite_id| {
                                WorkbenchTool::RunTests {
                                    suite_id: suite_id.clone(),
                                    program: "memory".into(),
                                    args: Vec::new(),
                                }
                                .validate_shape()
                                .is_ok()
                            })
                    } else {
                        self.suite_id.is_none()
                    }
            }
            Kind::PackageArtifact => {
                self.target.is_none() && self.program.is_none() && self.suite_id.is_none()
            }
        };
        if valid {
            Ok(())
        } else {
            Err(invalid())
        }
    }
}

/// Private, bounded journal projection; never execution or continuation authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveWorkingMemorySourceV1 {
    pub schema_version: u16,
    pub session_id: Uuid,
    pub authority: RuntimeAuthoritySnapshotV1,
    pub provider_version: u64,
    pub effect_id: Uuid,
    /// The sealed provider-prefix head, not the later model claim's current head.
    pub head_version: u64,
    pub head_entry_digest: String,
    pub last_observation: Option<AdaptiveObservationRefV1>,
    pub model_calls: u16,
    pub tool_calls: u16,
    pub root_model_ceiling: u16,
    pub root_tool_ceiling: u16,
    pub active_model_ceiling: u16,
    pub continuation_windows: u16,
    pub completed_tool_count: u16,
    pub omitted_count: u16,
    pub rows: Vec<AdaptiveWorkingMemoryCompletedRowV1>,
}

impl AdaptiveWorkingMemorySourceV1 {
    pub fn validate(&self) -> Result<(), WorkflowError> {
        self.authority.validate()?;
        if self.schema_version != 1
            || self.session_id.is_nil()
            || self.effect_id.is_nil()
            || self.provider_version == 0
            || self.head_version != self.provider_version
            || !validate_sha256(&self.head_entry_digest)
            || !(1..=ADAPTIVE_SESSION_MAX_CALLS).contains(&self.root_model_ceiling)
            || !(1..=ADAPTIVE_SESSION_MAX_CALLS).contains(&self.root_tool_ceiling)
            || self.active_model_ceiling == 0
            || self.active_model_ceiling > self.root_model_ceiling
            || self.model_calls > self.active_model_ceiling
            || self.tool_calls > self.root_tool_ceiling
            || self.completed_tool_count > self.tool_calls
            || (self.completed_tool_count > 0 && self.rows.is_empty())
            || self.rows.len() > ADAPTIVE_WORKING_MEMORY_MAX_ROWS
            || usize::from(self.completed_tool_count)
                != self.rows.len() + usize::from(self.omitted_count)
            || u64::from(self.continuation_windows) > self.head_version.saturating_sub(1)
            || serde_json::to_vec(self).map_err(|_| invalid())?.len()
                > ADAPTIVE_WORKING_MEMORY_MAX_BYTES
        {
            return Err(invalid());
        }
        let mut previous_version = 1;
        let mut previous_time = 0;
        let mut effects = BTreeSet::new();
        for row in &self.rows {
            if row.session_version <= previous_version
                || row.session_version > self.provider_version
                || row.recorded_at_ms < previous_time
                || row.recorded_at_ms == 0
                || row.recorded_at_ms > i64::MAX as u64
                || !validate_sha256(&row.entry_digest)
                || !validate_sha256(&row.tool_digest)
                || row.observation.effect.id.is_nil()
                || !effects.insert(row.observation.effect.id)
                || !validate_sha256(&row.observation.effect.request_digest)
                || !validate_sha256(&row.observation.observation_digest)
                || [&row.target, &row.program, &row.suite_id]
                    .into_iter()
                    .flatten()
                    .any(|label| {
                        label.trim().is_empty()
                            || label.len() > ADAPTIVE_WORKING_MEMORY_MAX_LABEL_BYTES
                            || label.chars().any(char::is_control)
                    })
            {
                return Err(invalid());
            }
            row.validate_labels()?;
            previous_version = row.session_version;
            previous_time = row.recorded_at_ms;
        }
        if self.rows.last().map(|row| &row.observation) != self.last_observation.as_ref() {
            return Err(invalid());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdaptiveModelDecisionV1 {
    Tool {
        tool: WorkbenchTool,
        tool_digest: String,
    },
    Collaborate {
        action: AdaptiveCollaborationActionV1,
    },
    ProposeCompletion {
        artifact_digest: String,
    },
    Blocked {
        reason_code: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdaptiveCursorV1 {
    ReadyForModel,
    ModelPending {
        effect: AdaptiveEffectV1,
    },
    ModelUnknown {
        effect: AdaptiveEffectV1,
    },
    ReadyForTool {
        tool: WorkbenchTool,
        tool_digest: String,
    },
    ToolPending {
        effect: AdaptiveEffectV1,
        tool: WorkbenchTool,
        tool_digest: String,
    },
    ToolUnknown {
        effect: AdaptiveEffectV1,
        tool: WorkbenchTool,
        tool_digest: String,
    },
    CollaborationProposed {
        effect: AdaptiveEffectV1,
        action: AdaptiveCollaborationActionV1,
    },
    CompletionProposed {
        artifact_digest: String,
    },
    Blocked {
        reason_code: String,
    },
    BlockedResolved {
        reason_code: String,
        resolution_event_id: String,
    },
    ModelRejected {
        resolution_event_id: String,
        reason_code: String,
    },
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveSessionV1 {
    pub grant: AdaptiveSessionGrantV1,
    pub version: u64,
    pub model_calls: u16,
    pub tool_calls: u16,
    pub cursor: AdaptiveCursorV1,
    pub last_observation: Option<AdaptiveObservationRefV1>,
    pub last_model_result_digest: Option<String>,
    pub updated_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<AdaptiveContinuationStateV1>,
    // Bounded by the explicit call ceilings; prevents an effect ID crossing turns.
    pub(crate) effect_ids: BTreeSet<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdaptiveTransitionV1 {
    /// Only the dedicated receipt-verifying store method may submit this.
    ContinueGoverned {
        authorization: AdaptiveContinuationAuthorizationV1,
    },
    ClaimModel {
        effect: AdaptiveEffectV1,
        previous_observation_digest: Option<String>,
    },
    ResolveModel {
        effect: AdaptiveEffectV1,
        result_digest: String,
        decision: AdaptiveModelDecisionV1,
    },
    ClaimTool {
        effect: AdaptiveEffectV1,
        tool_digest: String,
    },
    ObserveTool {
        observation: AdaptiveObservationRefV1,
    },
    CommitCollaboration {
        effect: AdaptiveEffectV1,
        action_digest: String,
    },
    MarkUnknown {
        effect: AdaptiveEffectV1,
    },
    RejectModel {
        effect: AdaptiveEffectV1,
        resolution_event_id: String,
        reason_code: String,
    },
    /// Only the dedicated receipt-verifying disposition may submit this.
    ResumeRejectedModel {
        disposition_operation_id: Uuid,
        expected_reason_code: String,
        resolution_event_id: String,
        receipt_digest: String,
    },
    ResolveBlocked {
        expected_reason_code: String,
        resolution_event_id: String,
    },
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdaptiveModelObservationV1 {
    Pending,
    Completed {
        result_digest: String,
        decision: AdaptiveModelDecisionV1,
    },
    UnknownOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdaptiveToolObservationV1 {
    Pending,
    Completed { observation_digest: String },
    UnknownOutcome,
}

/// Implementations reconcile the stable effect. They must not mint authority or a new effect ID.
pub trait AdaptiveModelPort: Send + Sync {
    fn reconcile_model(
        &self,
        session: &AdaptiveSessionV1,
        effect: &AdaptiveEffectV1,
    ) -> Result<AdaptiveModelObservationV1, crate::WorkflowPortError>;
}

/// Implementations reconcile the stable Workbench invocation named by `effect.id`.
pub trait AdaptiveToolPort: Send + Sync {
    fn reconcile_tool(
        &self,
        session: &AdaptiveSessionV1,
        effect: &AdaptiveEffectV1,
        tool: &WorkbenchTool,
        tool_digest: &str,
    ) -> Result<AdaptiveToolObservationV1, crate::WorkflowPortError>;
}

impl AdaptiveSessionV1 {
    pub(crate) fn initial(grant: AdaptiveSessionGrantV1) -> Result<Self, WorkflowError> {
        grant.validate()?;
        Ok(Self {
            version: 1,
            model_calls: 0,
            tool_calls: 0,
            cursor: AdaptiveCursorV1::ReadyForModel,
            last_observation: None,
            last_model_result_digest: None,
            effect_ids: BTreeSet::new(),
            updated_at_ms: grant.created_at_ms,
            continuation: None,
            grant,
        })
    }

    pub(crate) fn transition(
        &self,
        command: &AdaptiveTransitionV1,
        now_ms: u64,
    ) -> Result<Self, WorkflowError> {
        use AdaptiveCursorV1 as Cursor;
        use AdaptiveTransitionV1 as Command;
        if now_ms < self.updated_at_ms {
            return Err(invalid());
        }
        let mut next = self.clone();
        next.cursor = match (&self.cursor, command) {
            (
                Cursor::Blocked { .. }
                | Cursor::BlockedResolved { .. }
                | Cursor::ModelUnknown { .. }
                | Cursor::ReadyForModel,
                Command::ContinueGoverned { authorization },
            ) => {
                authorization.validate()?;
                let history = self
                    .continuation
                    .as_ref()
                    .map(|state| state.authorizations.as_slice())
                    .unwrap_or(&[]);
                let window_limit = authorization
                    .resume_policy
                    .as_ref()
                    .map_or(ADAPTIVE_CONTINUATION_MAX_WINDOWS, |binding| {
                        usize::from(binding.limits.total_window_ceiling)
                    });
                if let Some(binding) = &authorization.resume_policy {
                    if binding.limits.max_call_duration_ms != self.grant.max_call_duration_ms
                        || history
                            .iter()
                            .filter_map(|prior| prior.resume_policy.as_ref())
                            .any(|prior| {
                                prior.policy_id != binding.policy_id
                                    || prior.receipt_digest != binding.receipt_digest
                                    || prior.limits != binding.limits
                                    || prior.ordinal >= binding.ordinal
                            })
                    {
                        return Err(invalid());
                    }
                } else if history.iter().any(|prior| prior.resume_policy.is_some()) {
                    return Err(invalid());
                }
                if authorization.session_id != self.grant.session_id
                    || authorization.source_session_version != self.version
                    || now_ms < authorization.issued_at_ms
                    || now_ms >= authorization.deadline_ms
                    || (!matches!(
                        &authorization.source,
                        AdaptiveContinuationSourceV1::BudgetWindowExhausted { .. }
                    ) && authorization.resume_policy.is_none()
                        && authorization.issued_at_ms < self.active_deadline_ms())
                    || history.len() >= window_limit
                    || authorization.provider_allowance_id == self.grant.provider_allowance_id
                    || history.iter().any(|prior| {
                        prior.operation_id == authorization.operation_id
                            || prior.review_id == authorization.review_id
                            || prior.resolution_event_id == authorization.resolution_event_id
                            || prior.provider_allowance_id == authorization.provider_allowance_id
                    })
                    || self
                        .model_calls
                        .checked_add(authorization.additional_model_calls)
                        .is_none_or(|ceiling| ceiling > self.grant.max_model_calls)
                {
                    return Err(invalid());
                }
                match (
                    &self.cursor,
                    &authorization.source,
                    &authorization.abandoned_model_effect,
                ) {
                    (
                        Cursor::ReadyForModel,
                        AdaptiveContinuationSourceV1::BudgetWindowExhausted {
                            continuation_history_digest,
                            ..
                        },
                        None,
                    ) if authorization.issued_at_ms >= self.updated_at_ms
                        && self.model_window_exhausted_at(authorization.issued_at_ms)
                        && *continuation_history_digest
                            == crate::adaptive_budget_history_digest(&self.continuation)? =>
                    {
                        // The transaction checks the raw active allowance digest;
                        // the journal only owns the history and exhaustion clock.
                    }
                    (
                        Cursor::Blocked { reason_code },
                        AdaptiveContinuationSourceV1::Blocked {
                            reason_code: expected,
                        },
                        None,
                    ) if reason_code == expected => {}
                    (
                        Cursor::BlockedResolved {
                            reason_code,
                            resolution_event_id,
                        },
                        AdaptiveContinuationSourceV1::BlockedResolved {
                            reason_code: expected,
                            resolution_event_id: expected_event,
                        },
                        None,
                    ) if reason_code == expected && resolution_event_id == expected_event => {}
                    (
                        Cursor::ModelUnknown { effect },
                        AdaptiveContinuationSourceV1::ModelUnknown,
                        Some(abandoned),
                    ) if effect == abandoned => {}
                    _ => return Err(invalid()),
                }
                let mut authorizations = history.to_vec();
                authorizations.push(authorization.clone());
                next.continuation = Some(AdaptiveContinuationStateV1 {
                    authorizations,
                    model_ceiling: self.model_calls + authorization.additional_model_calls,
                    observation_required: true,
                });
                Cursor::ReadyForModel
            }
            (
                Cursor::ReadyForModel,
                Command::ClaimModel {
                    effect,
                    previous_observation_digest,
                },
            ) => {
                if self.model_admission_at(now_ms) != AdaptiveModelAdmissionV1::Admissible
                    || previous_observation_digest.as_deref()
                        != self
                            .last_observation
                            .as_ref()
                            .map(|o| o.observation_digest.as_str())
                {
                    return Err(invalid());
                }
                next.claim(effect)?;
                next.model_calls += 1;
                Cursor::ModelPending {
                    effect: effect.clone(),
                }
            }
            (
                Cursor::ModelPending { effect } | Cursor::ModelUnknown { effect },
                Command::ResolveModel {
                    effect: resolved,
                    result_digest,
                    decision,
                },
            ) if effect == resolved => {
                if !validate_sha256(result_digest)
                    || self.is_abandoned_model_effect(resolved)
                    || (self.requires_fresh_observation()
                        && !matches!(
                            decision,
                            AdaptiveModelDecisionV1::Tool {
                                tool: WorkbenchTool::ListDirectory { .. }
                                    | WorkbenchTool::InspectFile { .. },
                                ..
                            } | AdaptiveModelDecisionV1::Blocked { .. }
                        ))
                {
                    return Err(invalid());
                }
                next.last_model_result_digest = Some(result_digest.clone());
                match decision {
                    AdaptiveModelDecisionV1::Tool { tool, tool_digest }
                        if adaptive_tool_digest(tool).as_ref() == Ok(tool_digest) =>
                    {
                        Cursor::ReadyForTool {
                            tool: tool.clone(),
                            tool_digest: tool_digest.clone(),
                        }
                    }
                    AdaptiveModelDecisionV1::ProposeCompletion { artifact_digest }
                        if validate_sha256(artifact_digest) =>
                    {
                        Cursor::CompletionProposed {
                            artifact_digest: artifact_digest.clone(),
                        }
                    }
                    AdaptiveModelDecisionV1::Blocked { reason_code }
                        if valid_reason(reason_code) =>
                    {
                        Cursor::Blocked {
                            reason_code: reason_code.clone(),
                        }
                    }
                    AdaptiveModelDecisionV1::Collaborate { action }
                        if validate_collaboration_action(action).is_ok() =>
                    {
                        Cursor::CollaborationProposed {
                            effect: effect.clone(),
                            action: action.clone(),
                        }
                    }
                    _ => return Err(invalid()),
                }
            }
            (
                Cursor::ReadyForTool { tool, tool_digest },
                Command::ClaimTool {
                    effect,
                    tool_digest: proposed,
                },
            ) if tool_digest == proposed => {
                if now_ms >= self.active_deadline_ms()
                    || self.tool_calls >= self.grant.max_tool_calls
                    || (self.requires_fresh_observation()
                        && !matches!(
                            tool,
                            WorkbenchTool::ListDirectory { .. } | WorkbenchTool::InspectFile { .. }
                        ))
                {
                    return Err(invalid());
                }
                next.claim(effect)?;
                next.tool_calls += 1;
                Cursor::ToolPending {
                    effect: effect.clone(),
                    tool: tool.clone(),
                    tool_digest: proposed.clone(),
                }
            }
            (
                Cursor::ToolPending { effect, tool, .. } | Cursor::ToolUnknown { effect, tool, .. },
                Command::ObserveTool { observation },
            ) if *effect == observation.effect => {
                if !validate_sha256(&observation.observation_digest) {
                    return Err(invalid());
                }
                // A failed command's confirmed output is feedback, not a failed work item.
                next.last_observation = Some(observation.clone());
                if matches!(
                    tool,
                    WorkbenchTool::ListDirectory { .. } | WorkbenchTool::InspectFile { .. }
                ) {
                    if let Some(state) = next.continuation.as_mut() {
                        state.observation_required = false;
                    }
                }
                Cursor::ReadyForModel
            }
            (
                Cursor::CollaborationProposed { effect, action },
                Command::CommitCollaboration {
                    effect: committed,
                    action_digest,
                },
            ) if effect == committed
                && adaptive_collaboration_digest(action).as_ref() == Ok(action_digest) =>
            {
                Cursor::ReadyForModel
            }
            (Cursor::ModelPending { effect }, Command::MarkUnknown { effect: unknown })
                if effect == unknown =>
            {
                Cursor::ModelUnknown {
                    effect: effect.clone(),
                }
            }
            (
                Cursor::ModelPending { effect } | Cursor::ModelUnknown { effect },
                Command::RejectModel {
                    effect: rejected,
                    resolution_event_id,
                    reason_code,
                },
            ) if effect == rejected
                && valid_resolution(resolution_event_id)
                && valid_reason(reason_code) =>
            {
                Cursor::ModelRejected {
                    resolution_event_id: resolution_event_id.clone(),
                    reason_code: reason_code.clone(),
                }
            }
            (
                Cursor::ModelRejected {
                    resolution_event_id: recorded_event,
                    reason_code: recorded_reason,
                },
                Command::ResumeRejectedModel {
                    disposition_operation_id,
                    expected_reason_code,
                    resolution_event_id,
                    receipt_digest,
                },
            ) if !disposition_operation_id.is_nil()
                && expected_reason_code == "fresh_observation_required"
                && recorded_reason == expected_reason_code
                && recorded_event == resolution_event_id
                && valid_resolution(resolution_event_id)
                && validate_sha256(receipt_digest)
                && self.requires_fresh_observation() =>
            {
                Cursor::ReadyForModel
            }
            (
                Cursor::Blocked { reason_code },
                Command::ResolveBlocked {
                    expected_reason_code,
                    resolution_event_id,
                },
            ) if reason_code == expected_reason_code && valid_resolution(resolution_event_id) => {
                Cursor::BlockedResolved {
                    reason_code: reason_code.clone(),
                    resolution_event_id: resolution_event_id.clone(),
                }
            }
            (
                Cursor::ToolPending {
                    effect,
                    tool,
                    tool_digest,
                },
                Command::MarkUnknown { effect: unknown },
            ) if effect == unknown => Cursor::ToolUnknown {
                effect: effect.clone(),
                tool: tool.clone(),
                tool_digest: tool_digest.clone(),
            },
            (
                Cursor::ReadyForModel
                | Cursor::ReadyForTool { .. }
                | Cursor::ModelRejected { .. }
                | Cursor::BlockedResolved { .. },
                Command::Cancel,
            ) => Cursor::Cancelled,
            _ => return Err(invalid()),
        };
        next.version = next.version.checked_add(1).ok_or_else(invalid)?;
        next.updated_at_ms = now_ms;
        Ok(next)
    }

    fn claim(&mut self, effect: &AdaptiveEffectV1) -> Result<(), WorkflowError> {
        if effect.id.is_nil()
            || !validate_sha256(&effect.request_digest)
            || !self.effect_ids.insert(effect.id)
        {
            return Err(invalid());
        }
        Ok(())
    }

    pub fn active_provider_allowance_id(&self) -> &str {
        self.continuation
            .as_ref()
            .and_then(|state| state.authorizations.last())
            .map_or(self.grant.provider_allowance_id.as_str(), |authorization| {
                authorization.provider_allowance_id.as_str()
            })
    }

    pub fn effective_grant(&self) -> AdaptiveSessionGrantV1 {
        let mut grant = self.grant.clone();
        if let Some(authorization) = self
            .continuation
            .as_ref()
            .and_then(|state| state.authorizations.last())
        {
            grant.provider_allowance_id = authorization.provider_allowance_id.clone();
            grant.provider_authority_digest = authorization.provider_authority_digest.clone();
            grant.created_at_ms = authorization.issued_at_ms;
            grant.deadline_ms = authorization.deadline_ms;
            grant.max_model_calls = self.active_model_ceiling();
            grant.max_call_duration_ms = self.effective_call_duration_ms();
        }
        grant
    }

    /// Normal budget windows restore the root duration cap; recovery windows
    /// retain every preceding restriction until the next normal budget window.
    pub fn effective_call_duration_ms(&self) -> u64 {
        let mut duration = self.grant.max_call_duration_ms;
        if let Some(state) = &self.continuation {
            for authorization in &state.authorizations {
                if let Some(binding) = &authorization.resume_policy {
                    duration = self
                        .grant
                        .max_call_duration_ms
                        .min(binding.limits.max_call_duration_ms);
                    continue;
                }
                // Invalid clocks fail closed here; receipt validation rejects them.
                let Some(window) = authorization
                    .deadline_ms
                    .checked_sub(authorization.issued_at_ms)
                    .filter(|window| *window > 0)
                else {
                    return 0;
                };
                duration = match &authorization.source {
                    AdaptiveContinuationSourceV1::BudgetWindowExhausted { .. } => {
                        self.grant.max_call_duration_ms.min(window)
                    }
                    AdaptiveContinuationSourceV1::Blocked { .. }
                    | AdaptiveContinuationSourceV1::BlockedResolved { .. }
                    | AdaptiveContinuationSourceV1::ModelUnknown => duration.min(window),
                };
            }
        }
        duration
    }

    pub fn active_deadline_ms(&self) -> u64 {
        self.continuation
            .as_ref()
            .and_then(|state| state.authorizations.last())
            .map_or(self.grant.deadline_ms, |authorization| {
                authorization.deadline_ms
            })
    }

    pub fn active_model_ceiling(&self) -> u16 {
        self.continuation
            .as_ref()
            .map_or(self.grant.max_model_calls, |state| state.model_ceiling)
    }

    /// Reports active model-window exhaustion, including root exhaustion,
    /// without authorizing continuation or changing session state.
    pub fn model_window_exhausted_at(&self, now_ms: u64) -> bool {
        matches!(self.cursor, AdaptiveCursorV1::ReadyForModel)
            && now_ms >= self.updated_at_ms
            && matches!(
                self.model_admission_at(now_ms),
                AdaptiveModelAdmissionV1::CallsExhausted
                    | AdaptiveModelAdmissionV1::DeadlineExpired
                    | AdaptiveModelAdmissionV1::InsufficientSlack
            )
    }

    pub fn model_admission_at(&self, now_ms: u64) -> AdaptiveModelAdmissionV1 {
        if now_ms < self.updated_at_ms {
            return AdaptiveModelAdmissionV1::DeadlineExpired;
        }
        if self.model_calls >= self.active_model_ceiling() {
            return AdaptiveModelAdmissionV1::CallsExhausted;
        }
        if now_ms >= self.active_deadline_ms() {
            return AdaptiveModelAdmissionV1::DeadlineExpired;
        }
        if let Some(binding) = self
            .continuation
            .as_ref()
            .and_then(|state| state.authorizations.last())
            .and_then(|authorization| authorization.resume_policy.as_ref())
        {
            if self
                .effective_call_duration_ms()
                .checked_add(binding.limits.dispatch_margin_ms)
                .is_none_or(|minimum| self.active_deadline_ms() - now_ms < minimum)
            {
                return AdaptiveModelAdmissionV1::InsufficientSlack;
            }
        }
        AdaptiveModelAdmissionV1::Admissible
    }

    pub(crate) fn continuation_window_limit(&self) -> usize {
        self.continuation
            .as_ref()
            .and_then(|state| state.authorizations.last())
            .and_then(|authorization| authorization.resume_policy.as_ref())
            .map_or(ADAPTIVE_CONTINUATION_MAX_WINDOWS, |binding| {
                usize::from(binding.limits.total_window_ceiling)
            })
    }

    pub fn requires_fresh_observation(&self) -> bool {
        self.continuation
            .as_ref()
            .is_some_and(|state| state.observation_required)
    }

    pub fn is_abandoned_model_effect(&self, effect: &AdaptiveEffectV1) -> bool {
        self.continuation.as_ref().is_some_and(|state| {
            state.authorizations.iter().any(|authorization| {
                authorization
                    .abandoned_model_effect
                    .as_ref()
                    .is_some_and(|old| old.id == effect.id)
            })
        })
    }
}

pub fn adaptive_tool_digest(tool: &WorkbenchTool) -> Result<String, WorkflowError> {
    tool.validate_shape().map_err(|_| invalid())?;
    let encoded = serde_json::to_vec(tool).map_err(|_| invalid())?;
    if encoded.is_empty() || encoded.len() > ADAPTIVE_TOOL_MAX_BYTES {
        return Err(invalid());
    }
    canonical_sha256("sentinel.workflow.adaptive-tool.v1", tool)
}

/// A model may request durable collaboration, but it can only name a role and
/// content. The daemon resolves the role to a current organization participant
/// and validates all referenced artifacts before applying the command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdaptiveCollaborationActionV1 {
    AskQuestion {
        question_ref: String,
    },
    OfferHandoff {
        consumer_role: CompanyRoleV1,
        artifact_digests: BTreeSet<String>,
        reason_ref: String,
    },
}

pub fn adaptive_collaboration_digest(
    action: &AdaptiveCollaborationActionV1,
) -> Result<String, WorkflowError> {
    validate_collaboration_action(action)?;
    canonical_sha256("sentinel.workflow.adaptive-collaboration.v1", action)
}

fn validate_collaboration_action(
    action: &AdaptiveCollaborationActionV1,
) -> Result<(), WorkflowError> {
    match action {
        AdaptiveCollaborationActionV1::AskQuestion { question_ref }
            if valid_text(question_ref, 4096) =>
        {
            Ok(())
        }
        AdaptiveCollaborationActionV1::OfferHandoff {
            consumer_role,
            artifact_digests,
            reason_ref,
        } if !matches!(
            consumer_role,
            CompanyRoleV1::Customer | CompanyRoleV1::Sales | CompanyRoleV1::Gaia
        ) && !artifact_digests.is_empty()
            && artifact_digests.len() <= 64
            && artifact_digests
                .iter()
                .all(|digest| validate_sha256(digest))
            && valid_text(reason_ref, 4096) =>
        {
            Ok(())
        }
        _ => Err(invalid()),
    }
}

fn valid_text(value: &str, max_bytes: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max_bytes && !value.bytes().any(|byte| byte == 0)
}

fn valid_reason(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn valid_resolution(value: &str) -> bool {
    Uuid::parse_str(value).is_ok_and(|id| !id.is_nil())
}

fn invalid() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::InvalidTransition,
        false,
        "adaptive transition is invalid or exceeds its grant",
    )
}

#[cfg(test)]
pub(crate) mod continuation_tests {
    use super::*;
    use crate::{
        AgentId, PrincipalAuthorityV1, ProjectId, TenantId, WorkItemId, WORKFLOW_SCHEMA_VERSION,
    };

    pub(crate) const NOW: u64 = 1_900_000_000_000;

    pub(crate) fn grant() -> AdaptiveSessionGrantV1 {
        AdaptiveSessionGrantV1 {
            schema_version: 1,
            session_id: Uuid::from_u128(101),
            authority: RuntimeAuthoritySnapshotV1 {
                schema_version: WORKFLOW_SCHEMA_VERSION,
                tenant_id: TenantId::parse("tenant-01").unwrap(),
                project_id: ProjectId::parse("project-01").unwrap(),
                work_item_id: WorkItemId::parse("work-01").unwrap(),
                agent_id: AgentId(7),
                assignment_version: 3,
                assignment_digest: "1".repeat(64),
                organization_generation: 9,
                organization_digest: "2".repeat(64),
                principal: PrincipalAuthorityV1::derive("agent-07", 4, &[0x5a; 32]).unwrap(),
                profile_id: "coding-agent-v1".into(),
                profile_generation: 2,
                profile_digest: "3".repeat(64),
                runtime_key: "bwrap-coding-v1".into(),
                runtime_generation: 2,
                runtime_digest: "4".repeat(64),
                policy_generation: 6,
                policy_digest: "5".repeat(64),
                active: true,
                capabilities: BTreeSet::from([
                    "file.inspect".into(),
                    "observation.retain_private".into(),
                ]),
            },
            provider_allowance_id: "root-allowance".into(),
            provider_authority_digest: "6".repeat(64),
            provider: "codex-cli".into(),
            model: "gpt-5.6-luna".into(),
            catalog_digest: "7".repeat(64),
            max_output_tokens: 4096,
            max_call_duration_ms: 120_000,
            max_model_calls: 16,
            max_tool_calls: 16,
            created_at_ms: NOW,
            deadline_ms: NOW + 1_000,
        }
    }

    pub(crate) fn effect(id: u128) -> AdaptiveEffectV1 {
        AdaptiveEffectV1 {
            id: Uuid::from_u128(id),
            request_digest: "8".repeat(64),
        }
    }

    pub(crate) fn unknown() -> AdaptiveSessionV1 {
        let initial = AdaptiveSessionV1::initial(grant()).unwrap();
        let pending = initial
            .transition(
                &AdaptiveTransitionV1::ClaimModel {
                    effect: effect(102),
                    previous_observation_digest: None,
                },
                NOW + 1,
            )
            .unwrap();
        pending
            .transition(
                &AdaptiveTransitionV1::MarkUnknown {
                    effect: effect(102),
                },
                NOW + 2,
            )
            .unwrap()
    }

    pub(crate) fn authorization(
        session: &AdaptiveSessionV1,
    ) -> AdaptiveContinuationAuthorizationV1 {
        AdaptiveContinuationAuthorizationV1 {
            schema_version: 1,
            operation_id: Uuid::from_u128(201),
            review_id: Uuid::from_u128(202),
            resolution_event_id: Uuid::from_u128(203),
            session_id: session.grant.session_id,
            source_session_version: session.version,
            source: AdaptiveContinuationSourceV1::ModelUnknown,
            abandoned_model_effect: Some(effect(102)),
            provider_allowance_id: "continued-allowance".into(),
            provider_authority_digest: "9".repeat(64),
            issued_at_ms: NOW + 1_000,
            deadline_ms: NOW + 11_000,
            additional_model_calls: 3,
            local_adoption: None,
            resume_policy: None,
        }
    }

    fn continue_with(
        session: &AdaptiveSessionV1,
        auth: AdaptiveContinuationAuthorizationV1,
    ) -> Result<AdaptiveSessionV1, WorkflowError> {
        let now = auth.issued_at_ms;
        session.transition(
            &AdaptiveTransitionV1::ContinueGoverned {
                authorization: auth,
            },
            now,
        )
    }

    fn budget_ready() -> AdaptiveSessionV1 {
        let source = unknown();
        let mut prior = authorization(&source);
        prior.additional_model_calls = 1;
        let mut ready = continue_with(&source, prior).unwrap();
        ready.model_calls = ready.active_model_ceiling();
        ready.tool_calls = 4;
        ready.updated_at_ms = NOW + 1_001;
        ready
    }

    #[test]
    fn unamended_admission_preserves_legacy_short_slack_and_clock_boundaries() {
        let initial = AdaptiveSessionV1::initial(grant()).unwrap();
        assert_eq!(
            initial.model_admission_at(NOW + 999),
            AdaptiveModelAdmissionV1::Admissible
        );
        assert_eq!(
            initial.model_admission_at(NOW + 1_000),
            AdaptiveModelAdmissionV1::DeadlineExpired
        );
        assert!(!initial.model_window_exhausted_at(NOW - 1));
        let mut spent = initial;
        spent.model_calls = spent.grant.max_model_calls;
        assert_eq!(
            spent.model_admission_at(NOW + 1),
            AdaptiveModelAdmissionV1::CallsExhausted
        );
        assert!(spent.model_window_exhausted_at(NOW + 1));
    }

    #[test]
    fn expired_tool_unknown_observation_preserves_nonempty_continuation_history() {
        let source = unknown();
        let ready = continue_with(&source, authorization(&source)).unwrap();
        let tool = WorkbenchTool::InspectFile {
            path: "src/main.rs".into(),
            max_bytes: 1024,
        };
        let tool_digest = adaptive_tool_digest(&tool).unwrap();
        let commands = [
            AdaptiveTransitionV1::ClaimModel {
                effect: effect(103),
                previous_observation_digest: None,
            },
            AdaptiveTransitionV1::ResolveModel {
                effect: effect(103),
                result_digest: "a".repeat(64),
                decision: AdaptiveModelDecisionV1::Tool {
                    tool,
                    tool_digest: tool_digest.clone(),
                },
            },
            AdaptiveTransitionV1::ClaimTool {
                effect: effect(104),
                tool_digest,
            },
            AdaptiveTransitionV1::MarkUnknown {
                effect: effect(104),
            },
        ];
        let mut unknown = ready;
        for (index, command) in commands.iter().enumerate() {
            unknown = unknown
                .transition(command, NOW + 1_001 + index as u64)
                .unwrap();
        }
        assert!(unknown.requires_fresh_observation());
        assert_eq!(
            unknown.continuation.as_ref().unwrap().authorizations.len(),
            1
        );
        let observation = AdaptiveObservationRefV1 {
            effect: effect(104),
            observation_digest: "b".repeat(64),
        };
        let observed_at = unknown.active_deadline_ms() + 1;
        let adopted = unknown
            .transition(
                &AdaptiveTransitionV1::ObserveTool {
                    observation: observation.clone(),
                },
                observed_at,
            )
            .unwrap();
        let mut expected = unknown.clone();
        expected.cursor = AdaptiveCursorV1::ReadyForModel;
        expected.version += 1;
        expected.updated_at_ms = observed_at;
        expected.last_observation = Some(observation);
        expected.continuation.as_mut().unwrap().observation_required = false;
        assert_eq!(adopted, expected);
        assert_eq!(
            adopted.continuation.as_ref().unwrap().authorizations,
            unknown.continuation.as_ref().unwrap().authorizations
        );
    }

    fn budget_authorization(
        source: &AdaptiveSessionV1,
        id: u128,
    ) -> AdaptiveContinuationAuthorizationV1 {
        let mut auth = authorization(source);
        auth.operation_id = Uuid::from_u128(id);
        auth.review_id = Uuid::from_u128(id + 1);
        auth.resolution_event_id = Uuid::from_u128(id + 2);
        auth.provider_allowance_id = format!("budget-allowance-{id}");
        auth.source = AdaptiveContinuationSourceV1::BudgetWindowExhausted {
            active_allowance_digest: "a".repeat(64),
            continuation_history_digest: crate::adaptive_budget_history_digest(
                &source.continuation,
            )
            .unwrap(),
        };
        auth.abandoned_model_effect = None;
        auth.issued_at_ms = source.updated_at_ms + 1;
        auth.deadline_ms = auth.issued_at_ms + 120_000;
        auth.additional_model_calls = 4;
        auth
    }

    #[test]
    fn budget_continuation_before_deadline_preserves_root_counters_and_inspection_fence() {
        let source = budget_ready();
        let auth = budget_authorization(&source, 401);
        assert!(auth.issued_at_ms < source.active_deadline_ms());
        let ready = continue_with(&source, auth.clone()).unwrap();
        assert_eq!(ready.grant, source.grant);
        assert_eq!(
            (ready.model_calls, ready.tool_calls),
            (source.model_calls, source.tool_calls)
        );
        assert_eq!(ready.effect_ids, source.effect_ids);
        assert_eq!(ready.last_observation, source.last_observation);
        assert_eq!(ready.active_model_ceiling(), source.model_calls + 4);
        assert!(ready.requires_fresh_observation());
        let history = &ready.continuation.as_ref().unwrap().authorizations;
        assert_eq!(
            &history[..history.len() - 1],
            source
                .continuation
                .as_ref()
                .unwrap()
                .authorizations
                .as_slice()
        );
        let pending = ready
            .transition(
                &AdaptiveTransitionV1::ClaimModel {
                    effect: effect(410),
                    previous_observation_digest: None,
                },
                auth.issued_at_ms,
            )
            .unwrap();
        let write = WorkbenchTool::WriteFile {
            path: "src/main.rs".into(),
            content: "changed".into(),
            expected_sha256: None,
        };
        assert!(pending
            .transition(
                &AdaptiveTransitionV1::ResolveModel {
                    effect: effect(410),
                    result_digest: "b".repeat(64),
                    decision: AdaptiveModelDecisionV1::Tool {
                        tool_digest: adaptive_tool_digest(&write).unwrap(),
                        tool: write
                    },
                },
                auth.issued_at_ms
            )
            .is_err());
        assert!(continue_with(&ready, auth).is_err());
    }

    #[test]
    fn budget_continuation_rejects_stale_history_clock_nonexhaustion_and_wrong_source() {
        let original = budget_ready();
        let auth = budget_authorization(&original, 421);
        for field in 0..8 {
            let mut source = original.clone();
            let mut changed = auth.clone();
            match field {
                0 => {
                    if let AdaptiveContinuationSourceV1::BudgetWindowExhausted {
                        continuation_history_digest,
                        ..
                    } = &mut changed.source
                    {
                        *continuation_history_digest = "b".repeat(64);
                    }
                }
                1 => {
                    if let AdaptiveContinuationSourceV1::BudgetWindowExhausted {
                        active_allowance_digest,
                        ..
                    } = &mut changed.source
                    {
                        *active_allowance_digest = "invalid".into();
                    }
                }
                2 => changed.issued_at_ms = source.updated_at_ms - 1,
                3 => source.model_calls -= 1,
                4 => {
                    source.cursor = AdaptiveCursorV1::Blocked {
                        reason_code: "needs_review".into(),
                    }
                }
                5 => changed.abandoned_model_effect = Some(effect(102)),
                6 => changed.source_session_version += 1,
                _ => changed.additional_model_calls = 15,
            }
            assert!(continue_with(&source, changed).is_err(), "field {field}");
        }
        let mut legacy = auth;
        legacy.source = AdaptiveContinuationSourceV1::Blocked {
            reason_code: "needs_review".into(),
        };
        let mut blocked = original;
        blocked.cursor = AdaptiveCursorV1::Blocked {
            reason_code: "needs_review".into(),
        };
        assert!(continue_with(&blocked, legacy).is_err());
    }

    #[test]
    fn budget_continuation_history_stays_finite_without_resetting_old_windows() {
        let mut source = budget_ready();
        for id in [441, 451] {
            let auth = budget_authorization(&source, id);
            source = continue_with(&source, auth).unwrap();
            source.model_calls = source.active_model_ceiling();
        }
        assert_eq!(
            source.continuation.as_ref().unwrap().authorizations.len(),
            ADAPTIVE_CONTINUATION_MAX_WINDOWS
        );
        assert_eq!(source.grant.max_model_calls, 16);
        assert!(continue_with(&source, budget_authorization(&source, 461)).is_err());
    }

    #[test]
    fn budget_continuation_after_first_deadline_does_not_require_a_model_result() {
        let mut source = AdaptiveSessionV1::initial(grant()).unwrap();
        source.updated_at_ms = source.grant.deadline_ms;
        let auth = budget_authorization(&source, 481);
        let next = continue_with(&source, auth).unwrap();
        assert_eq!(next.model_calls, 0);
        assert!(next.last_model_result_digest.is_none());
        assert!(next.last_observation.is_none());
        assert!(next.requires_fresh_observation());
        assert_eq!(next.grant, source.grant);
    }

    fn append_duration_window(
        session: &mut AdaptiveSessionV1,
        source: AdaptiveContinuationSourceV1,
        window_ms: u64,
    ) {
        let mut auth = authorization(session);
        let index = session
            .continuation
            .as_ref()
            .map_or(0, |state| state.authorizations.len());
        auth.operation_id = Uuid::from_u128(801 + index as u128 * 3);
        auth.review_id = Uuid::from_u128(802 + index as u128 * 3);
        auth.resolution_event_id = Uuid::from_u128(803 + index as u128 * 3);
        auth.provider_allowance_id = format!("duration-allowance-{index}");
        auth.issued_at_ms = session.active_deadline_ms();
        auth.deadline_ms = auth.issued_at_ms + window_ms;
        auth.abandoned_model_effect =
            if matches!(&source, AdaptiveContinuationSourceV1::ModelUnknown) {
                Some(effect(102))
            } else {
                None
            };
        auth.source = source;
        auth.validate().unwrap();
        session
            .continuation
            .get_or_insert(AdaptiveContinuationStateV1 {
                authorizations: Vec::new(),
                model_ceiling: 3,
                observation_required: true,
            })
            .authorizations
            .push(auth);
        session.version += 1;
    }

    fn duration_recovery_sources() -> [AdaptiveContinuationSourceV1; 3] {
        [
            AdaptiveContinuationSourceV1::Blocked {
                reason_code: "needs_review".into(),
            },
            AdaptiveContinuationSourceV1::BlockedResolved {
                reason_code: "needs_review".into(),
                resolution_event_id: Uuid::from_u128(900).to_string(),
            },
            AdaptiveContinuationSourceV1::ModelUnknown,
        ]
    }

    #[test]
    fn effective_duration_retains_tight_recovery_until_normal_window_resets_it() {
        for source in duration_recovery_sources() {
            let mut session = AdaptiveSessionV1::initial(grant()).unwrap();
            assert_eq!(session.effective_call_duration_ms(), 120_000);
            assert_eq!(session.effective_grant(), session.grant);
            append_duration_window(&mut session, source.clone(), 1_000);
            append_duration_window(&mut session, source, 60_000);
            assert_eq!(session.effective_call_duration_ms(), 1_000);
            assert_eq!(session.effective_grant().max_call_duration_ms, 1_000);
            let normal = budget_authorization(&session, 901).source;
            append_duration_window(&mut session, normal, 300_000);
            let effective = session.effective_grant();
            assert_eq!(session.effective_call_duration_ms(), 120_000);
            assert_eq!(effective.max_call_duration_ms, 120_000);
            assert_eq!(
                effective.provider_allowance_id,
                session.active_provider_allowance_id()
            );
            assert_eq!(effective.deadline_ms, session.active_deadline_ms());
            assert_eq!(session.grant, grant());
            assert_eq!((session.model_calls, session.tool_calls), (0, 0));
            let first = &mut session.continuation.as_mut().unwrap().authorizations[0];
            first.deadline_ms = first.issued_at_ms;
            assert_eq!(session.effective_call_duration_ms(), 0);
            assert_eq!(session.effective_grant().max_call_duration_ms, 0);
        }
    }

    #[test]
    fn effective_duration_normal_reset_is_followed_by_recovery_restrictions() {
        for source in duration_recovery_sources() {
            let mut session = AdaptiveSessionV1::initial(grant()).unwrap();
            append_duration_window(&mut session, source.clone(), 1_000);
            let normal = budget_authorization(&session, 921).source;
            append_duration_window(&mut session, normal, 60_000);
            assert_eq!(session.effective_call_duration_ms(), 60_000);
            assert_eq!(session.effective_grant().max_call_duration_ms, 60_000);
            append_duration_window(&mut session, source, 120_000);
            assert_eq!(session.effective_call_duration_ms(), 60_000);
            assert_eq!(session.effective_grant().max_call_duration_ms, 60_000);
        }
    }

    #[test]
    fn model_window_exhaustion_initial_boundaries_include_root_limits() {
        let mut session = AdaptiveSessionV1::initial(grant()).unwrap();
        let initial = session.clone();
        assert!(!session.model_window_exhausted_at(NOW));
        assert!(!session.model_window_exhausted_at(session.grant.deadline_ms - 1));
        assert!(session.model_window_exhausted_at(session.grant.deadline_ms));
        assert!(session.model_window_exhausted_at(session.grant.deadline_ms + 1));
        assert_eq!(session, initial);

        session.model_calls = session.grant.max_model_calls - 1;
        assert!(!session.model_window_exhausted_at(NOW));
        session.model_calls += 1;
        let exhausted = session.clone();
        assert!(session.model_window_exhausted_at(NOW));
        assert_eq!(session, exhausted);
        session.model_calls += 1;
        assert!(session.model_window_exhausted_at(NOW));
    }

    #[test]
    fn model_window_exhaustion_uses_continued_cumulative_limits_not_root_grant() {
        let source = unknown();
        let auth = authorization(&source);
        let mut session = continue_with(&source, auth.clone()).unwrap();
        let continued = session.clone();
        assert_eq!(session.model_calls, 1);
        assert_eq!(session.active_model_ceiling(), 4);
        assert_eq!(session.grant, source.grant);
        assert!(!session.model_window_exhausted_at(auth.issued_at_ms));
        assert!(!session.model_window_exhausted_at(auth.deadline_ms - 1));
        assert!(session.model_window_exhausted_at(auth.deadline_ms));
        assert!(session.model_window_exhausted_at(auth.deadline_ms + 1));
        assert_eq!(session, continued);

        session.model_calls = session.active_model_ceiling() - 1;
        assert_eq!(session.model_calls, auth.additional_model_calls);
        assert!(!session.model_window_exhausted_at(auth.issued_at_ms));
        session.model_calls += 1;
        assert!(session.model_calls < session.grant.max_model_calls);
        let exhausted = session.clone();
        assert!(session.model_window_exhausted_at(auth.issued_at_ms));
        assert_eq!(session, exhausted);
        session.model_calls += 1;
        assert!(session.model_window_exhausted_at(auth.issued_at_ms));

        let mut root_auth = auth;
        root_auth.additional_model_calls = source.grant.max_model_calls - source.model_calls;
        let mut root_exhausted = continue_with(&source, root_auth.clone()).unwrap();
        root_exhausted.model_calls = root_exhausted.grant.max_model_calls;
        assert_eq!(root_exhausted.grant, source.grant);
        assert!(root_exhausted.model_window_exhausted_at(root_auth.issued_at_ms));
    }

    #[test]
    fn model_window_exhaustion_excludes_every_non_ready_cursor() {
        let mut session = AdaptiveSessionV1::initial(grant()).unwrap();
        let tool = WorkbenchTool::InspectFile {
            path: "src/main.rs".into(),
            max_bytes: 1024,
        };
        let tool_digest = adaptive_tool_digest(&tool).unwrap();
        for cursor in [
            AdaptiveCursorV1::ModelPending {
                effect: effect(102),
            },
            AdaptiveCursorV1::ModelUnknown {
                effect: effect(102),
            },
            AdaptiveCursorV1::ReadyForTool {
                tool: tool.clone(),
                tool_digest: tool_digest.clone(),
            },
            AdaptiveCursorV1::ToolPending {
                effect: effect(103),
                tool: tool.clone(),
                tool_digest: tool_digest.clone(),
            },
            AdaptiveCursorV1::ToolUnknown {
                effect: effect(103),
                tool,
                tool_digest,
            },
            AdaptiveCursorV1::CollaborationProposed {
                effect: effect(102),
                action: AdaptiveCollaborationActionV1::AskQuestion {
                    question_ref: "question".into(),
                },
            },
            AdaptiveCursorV1::CompletionProposed {
                artifact_digest: "a".repeat(64),
            },
            AdaptiveCursorV1::Blocked {
                reason_code: "needs_review".into(),
            },
            AdaptiveCursorV1::BlockedResolved {
                reason_code: "needs_review".into(),
                resolution_event_id: Uuid::from_u128(900).to_string(),
            },
            AdaptiveCursorV1::ModelRejected {
                resolution_event_id: Uuid::from_u128(901).to_string(),
                reason_code: "rejected".into(),
            },
            AdaptiveCursorV1::Cancelled,
        ] {
            session.cursor = cursor;
            session.model_calls = 0;
            assert!(!session.model_window_exhausted_at(session.grant.deadline_ms));
            session.model_calls = session.grant.max_model_calls;
            assert!(!session.model_window_exhausted_at(NOW));
            let exhausted = session.clone();
            assert!(!session.model_window_exhausted_at(session.grant.deadline_ms));
            assert_eq!(session, exhausted);
        }
    }

    #[test]
    fn model_window_exhaustion_rejects_backwards_clock_even_when_exhausted() {
        let mut session = AdaptiveSessionV1::initial(grant()).unwrap();
        session.model_calls = session.grant.max_model_calls;
        assert!(!session.model_window_exhausted_at(NOW - 1));
        assert!(session.model_window_exhausted_at(NOW));

        let source = unknown();
        let auth = authorization(&source);
        session = continue_with(&source, auth.clone()).unwrap();
        session.model_calls = session.active_model_ceiling();
        assert!(!session.model_window_exhausted_at(auth.issued_at_ms - 1));
        assert!(session.model_window_exhausted_at(auth.issued_at_ms));

        session.model_calls = 0;
        session.updated_at_ms = auth.deadline_ms + 1;
        assert!(!session.model_window_exhausted_at(auth.deadline_ms));
        assert!(session.model_window_exhausted_at(session.updated_at_ms));
        session.model_calls = session.active_model_ceiling();
        let exhausted = session.clone();
        assert!(!session.model_window_exhausted_at(auth.deadline_ms));
        assert!(session.model_window_exhausted_at(session.updated_at_ms));
        assert_eq!(session, exhausted);
    }

    #[test]
    fn continuation_preserves_root_spending_and_effects_with_short_effective_grant() {
        let source = unknown();
        let auth = authorization(&source);
        let next = continue_with(&source, auth.clone()).unwrap();
        assert_eq!(next.grant, source.grant);
        assert_eq!((next.model_calls, next.tool_calls), (1, 0));
        assert_eq!(next.effect_ids, source.effect_ids);
        assert_eq!(next.last_observation, source.last_observation);
        assert_eq!(
            next.last_model_result_digest,
            source.last_model_result_digest
        );
        assert_eq!(next.active_model_ceiling(), 4);
        assert_eq!(
            next.active_provider_allowance_id(),
            auth.provider_allowance_id
        );
        assert_eq!(next.active_deadline_ms(), auth.deadline_ms);
        let effective = next.effective_grant();
        effective.validate().unwrap();
        assert_eq!(effective.max_call_duration_ms, 10_000);
        assert_eq!(effective.max_tool_calls, 16);
        assert_eq!(effective.authority, source.grant.authority);
        assert_eq!(effective.session_id, source.grant.session_id);
        assert_eq!(
            effective.provider_authority_digest,
            auth.provider_authority_digest
        );
        assert!(next.requires_fresh_observation());
        assert!(next.is_abandoned_model_effect(&effect(102)));
        let mut changed_digest = effect(102);
        changed_digest.request_digest = "a".repeat(64);
        assert!(next.is_abandoned_model_effect(&changed_digest));
        assert!(next
            .transition(
                &AdaptiveTransitionV1::ClaimModel {
                    effect: effect(102),
                    previous_observation_digest: None,
                },
                auth.issued_at_ms
            )
            .is_err());
    }

    #[test]
    fn continuation_rejects_budget_window_stale_source_and_reused_authority() {
        let source = unknown();
        let original = authorization(&source);
        let mut invalids = Vec::new();
        let mut a = original.clone();
        a.additional_model_calls = 16;
        invalids.push(a);
        let mut a = original.clone();
        a.additional_model_calls = 0;
        invalids.push(a);
        let mut a = original.clone();
        a.deadline_ms = a.issued_at_ms + 300_001;
        invalids.push(a);
        let mut a = original.clone();
        a.deadline_ms = a.issued_at_ms + 999;
        invalids.push(a);
        let mut a = original.clone();
        a.issued_at_ms -= 1;
        invalids.push(a);
        let mut a = original.clone();
        a.source_session_version -= 1;
        invalids.push(a);
        let mut a = original.clone();
        a.abandoned_model_effect = Some(effect(103));
        invalids.push(a);
        let mut a = original.clone();
        a.provider_allowance_id = source.grant.provider_allowance_id.clone();
        invalids.push(a);
        let mut a = original.clone();
        a.source = AdaptiveContinuationSourceV1::Blocked {
            reason_code: "blocked".into(),
        };
        invalids.push(a);
        for auth in invalids {
            assert!(continue_with(&source, auth).is_err());
        }
        let mut next = continue_with(&source, original).unwrap();
        for window in 2..=4 {
            next.cursor = AdaptiveCursorV1::Blocked {
                reason_code: "needs_review".into(),
            };
            let mut auth = authorization(&next);
            auth.source = AdaptiveContinuationSourceV1::Blocked {
                reason_code: "needs_review".into(),
            };
            auth.abandoned_model_effect = None;
            auth.operation_id = Uuid::from_u128(300 + window);
            auth.review_id = Uuid::from_u128(400 + window);
            auth.resolution_event_id = Uuid::from_u128(500 + window);
            auth.provider_allowance_id = format!("window-{window}");
            auth.issued_at_ms = next.active_deadline_ms();
            auth.deadline_ms = auth.issued_at_ms + 10_000;
            if window == 4 {
                assert!(continue_with(&next, auth).is_err());
            } else {
                next = continue_with(&next, auth).unwrap();
            }
        }
        assert_eq!(next.continuation.as_ref().unwrap().authorizations.len(), 3);
        assert_eq!(next.model_calls, 1);
        assert!(next.requires_fresh_observation());
    }

    #[test]
    fn fresh_observation_fence_rejects_completion_collaboration_and_write() {
        let source = unknown();
        let ready = continue_with(&source, authorization(&source)).unwrap();
        let model = effect(104);
        let pending = ready
            .transition(
                &AdaptiveTransitionV1::ClaimModel {
                    effect: model.clone(),
                    previous_observation_digest: None,
                },
                NOW + 1_001,
            )
            .unwrap();
        let write = WorkbenchTool::WriteFile {
            path: "src/main.rs".into(),
            content: "new".into(),
            expected_sha256: None,
        };
        let command = WorkbenchTool::RunCommand {
            program: "node".into(),
            args: vec!["--version".into()],
        };
        for decision in [
            AdaptiveModelDecisionV1::ProposeCompletion {
                artifact_digest: "a".repeat(64),
            },
            AdaptiveModelDecisionV1::Collaborate {
                action: AdaptiveCollaborationActionV1::AskQuestion {
                    question_ref: "question".into(),
                },
            },
            AdaptiveModelDecisionV1::Tool {
                tool_digest: adaptive_tool_digest(&write).unwrap(),
                tool: write,
            },
            AdaptiveModelDecisionV1::Tool {
                tool_digest: adaptive_tool_digest(&command).unwrap(),
                tool: command,
            },
        ] {
            assert!(pending
                .transition(
                    &AdaptiveTransitionV1::ResolveModel {
                        effect: model.clone(),
                        result_digest: "b".repeat(64),
                        decision,
                    },
                    NOW + 1_002
                )
                .is_err());
        }
        let inspect = WorkbenchTool::InspectFile {
            path: "src/main.rs".into(),
            max_bytes: 1024,
        };
        let digest = adaptive_tool_digest(&inspect).unwrap();
        let tool_ready = pending
            .transition(
                &AdaptiveTransitionV1::ResolveModel {
                    effect: model,
                    result_digest: "b".repeat(64),
                    decision: AdaptiveModelDecisionV1::Tool {
                        tool: inspect,
                        tool_digest: digest.clone(),
                    },
                },
                NOW + 1_002,
            )
            .unwrap();
        assert!(tool_ready.requires_fresh_observation());
        let tool_pending = tool_ready
            .transition(
                &AdaptiveTransitionV1::ClaimTool {
                    effect: effect(105),
                    tool_digest: digest,
                },
                NOW + 1_003,
            )
            .unwrap();
        let observed = tool_pending
            .transition(
                &AdaptiveTransitionV1::ObserveTool {
                    observation: AdaptiveObservationRefV1 {
                        effect: effect(105),
                        observation_digest: "c".repeat(64),
                    },
                },
                NOW + 1_004,
            )
            .unwrap();
        assert!(!observed.requires_fresh_observation());
        assert_eq!(observed.model_calls, 2);
        assert_eq!(observed.tool_calls, 1);
        assert!(observed.is_abandoned_model_effect(&effect(102)));
    }

    #[test]
    fn old_serialized_session_omits_continuation_and_retains_hash() {
        let session = unknown();
        let bytes = serde_json::to_vec(&session).unwrap();
        assert!(!String::from_utf8(bytes.clone())
            .unwrap()
            .contains("continuation"));
        let restored: AdaptiveSessionV1 = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(serde_json::to_vec(&restored).unwrap(), bytes);
        assert_eq!(
            canonical_sha256("fixture", &session).unwrap(),
            canonical_sha256("fixture", &restored).unwrap()
        );
    }

    #[test]
    fn resolved_source_binds_original_reason_and_resolution_without_reset() {
        let mut source = unknown();
        let old_event = Uuid::from_u128(900).to_string();
        source.cursor = AdaptiveCursorV1::BlockedResolved {
            reason_code: "needs_review".into(),
            resolution_event_id: old_event.clone(),
        };
        let mut auth = authorization(&source);
        auth.abandoned_model_effect = None;
        auth.source = AdaptiveContinuationSourceV1::BlockedResolved {
            reason_code: "needs_review".into(),
            resolution_event_id: old_event,
        };
        let next = continue_with(&source, auth.clone()).unwrap();
        assert_eq!(next.grant, source.grant);
        assert_eq!(next.model_calls, source.model_calls);
        assert_eq!(
            next.continuation.as_ref().unwrap().authorizations[0].source,
            auth.source
        );
        auth.source = AdaptiveContinuationSourceV1::BlockedResolved {
            reason_code: "different_reason".into(),
            resolution_event_id: Uuid::from_u128(901).to_string(),
        };
        assert!(continue_with(&source, auth).is_err());
    }

    #[test]
    fn old_observation_cannot_lift_fence_and_window_budget_is_cumulative() {
        let mut source = unknown();
        source.last_observation = Some(AdaptiveObservationRefV1 {
            effect: effect(99),
            observation_digest: "c".repeat(64),
        });
        source.tool_calls = 4;
        let mut auth = authorization(&source);
        auth.additional_model_calls = 1;
        let next = continue_with(&source, auth.clone()).unwrap();
        assert_eq!(next.tool_calls, 4);
        assert!(next.requires_fresh_observation());
        let pending = next
            .transition(
                &AdaptiveTransitionV1::ClaimModel {
                    effect: effect(104),
                    previous_observation_digest: Some("c".repeat(64)),
                },
                auth.issued_at_ms,
            )
            .unwrap();
        let blocked = pending
            .transition(
                &AdaptiveTransitionV1::ResolveModel {
                    effect: effect(104),
                    result_digest: "b".repeat(64),
                    decision: AdaptiveModelDecisionV1::Blocked {
                        reason_code: "needs_review".into(),
                    },
                },
                auth.issued_at_ms,
            )
            .unwrap();
        assert!(blocked.requires_fresh_observation());
        let mut at_ceiling = blocked.clone();
        at_ceiling.cursor = AdaptiveCursorV1::ReadyForModel;
        assert!(at_ceiling
            .transition(
                &AdaptiveTransitionV1::ClaimModel {
                    effect: effect(105),
                    previous_observation_digest: Some("c".repeat(64)),
                },
                auth.issued_at_ms
            )
            .is_err());
        assert_eq!(blocked.model_calls, 2);
        assert_eq!(blocked.active_model_ceiling(), 2);
        assert_eq!(blocked.grant.max_model_calls, 16);
    }

    #[test]
    fn blocked_continuation_requires_new_inspection_despite_retained_observation() {
        for resolved in [false, true] {
            let mut source = unknown();
            let old_event = Uuid::from_u128(900).to_string();
            source.cursor = if resolved {
                AdaptiveCursorV1::BlockedResolved {
                    reason_code: "needs_review".into(),
                    resolution_event_id: old_event.clone(),
                }
            } else {
                AdaptiveCursorV1::Blocked {
                    reason_code: "needs_review".into(),
                }
            };
            source.last_observation = Some(AdaptiveObservationRefV1 {
                effect: effect(99),
                observation_digest: "c".repeat(64),
            });
            source.tool_calls = 4;
            assert!(!source.requires_fresh_observation());
            let mut auth = authorization(&source);
            auth.abandoned_model_effect = None;
            auth.source = if resolved {
                AdaptiveContinuationSourceV1::BlockedResolved {
                    reason_code: "needs_review".into(),
                    resolution_event_id: old_event,
                }
            } else {
                AdaptiveContinuationSourceV1::Blocked {
                    reason_code: "needs_review".into(),
                }
            };
            let ready = continue_with(&source, auth.clone()).unwrap();
            assert!(ready.requires_fresh_observation());
            assert_eq!(ready.grant, source.grant);
            assert_eq!(
                (ready.model_calls, ready.tool_calls),
                (source.model_calls, source.tool_calls)
            );
            assert_eq!(ready.effect_ids, source.effect_ids);
            assert_eq!(ready.last_observation, source.last_observation);
            assert_eq!(
                ready.continuation.as_ref().unwrap().authorizations,
                vec![auth.clone()]
            );
            let pending = ready
                .transition(
                    &AdaptiveTransitionV1::ClaimModel {
                        effect: effect(104),
                        previous_observation_digest: Some("c".repeat(64)),
                    },
                    auth.issued_at_ms,
                )
                .unwrap();
            for tool in [
                WorkbenchTool::WriteFile {
                    path: "src/main.rs".into(),
                    content: "new".into(),
                    expected_sha256: None,
                },
                WorkbenchTool::RunCommand {
                    program: "node".into(),
                    args: vec!["--version".into()],
                },
            ] {
                assert!(pending
                    .transition(
                        &AdaptiveTransitionV1::ResolveModel {
                            effect: effect(104),
                            result_digest: "b".repeat(64),
                            decision: AdaptiveModelDecisionV1::Tool {
                                tool_digest: adaptive_tool_digest(&tool).unwrap(),
                                tool
                            },
                        },
                        auth.issued_at_ms
                    )
                    .is_err());
            }
            assert!(pending
                .transition(
                    &AdaptiveTransitionV1::ResolveModel {
                        effect: effect(104),
                        result_digest: "b".repeat(64),
                        decision: AdaptiveModelDecisionV1::ProposeCompletion {
                            artifact_digest: "a".repeat(64)
                        },
                    },
                    auth.issued_at_ms
                )
                .is_err());
            let inspect = WorkbenchTool::InspectFile {
                path: "src/main.rs".into(),
                max_bytes: 1024,
            };
            let tool_digest = adaptive_tool_digest(&inspect).unwrap();
            let tool_ready = pending
                .transition(
                    &AdaptiveTransitionV1::ResolveModel {
                        effect: effect(104),
                        result_digest: "b".repeat(64),
                        decision: AdaptiveModelDecisionV1::Tool {
                            tool: inspect,
                            tool_digest: tool_digest.clone(),
                        },
                    },
                    auth.issued_at_ms,
                )
                .unwrap();
            assert!(tool_ready.requires_fresh_observation());
            let tool_pending = tool_ready
                .transition(
                    &AdaptiveTransitionV1::ClaimTool {
                        effect: effect(105),
                        tool_digest,
                    },
                    auth.issued_at_ms,
                )
                .unwrap();
            assert!(tool_pending.requires_fresh_observation());
            let observed = tool_pending
                .transition(
                    &AdaptiveTransitionV1::ObserveTool {
                        observation: AdaptiveObservationRefV1 {
                            effect: effect(105),
                            observation_digest: "d".repeat(64),
                        },
                    },
                    auth.issued_at_ms,
                )
                .unwrap();
            assert!(!observed.requires_fresh_observation());
            assert_eq!(observed.grant, source.grant);
            assert_eq!((observed.model_calls, observed.tool_calls), (2, 5));
        }
    }
}
