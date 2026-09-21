//! Internal adaptive execution journal. References do not confer tool or provider authority.

use std::collections::BTreeSet;

use sentinel_common::WorkbenchTool;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::digest::{canonical_sha256, validate_sha256};
use crate::{RuntimeAuthoritySnapshotV1, WorkflowError, WorkflowErrorCode};

pub const ADAPTIVE_SESSION_MAX_CALLS: u16 = 64;
pub const ADAPTIVE_TOOL_MAX_BYTES: usize = 256 * 1024;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveObservationRefV1 {
    pub effect: AdaptiveEffectV1,
    pub observation_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdaptiveModelDecisionV1 {
    Tool {
        tool: WorkbenchTool,
        tool_digest: String,
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
    CompletionProposed {
        artifact_digest: String,
    },
    Blocked {
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
    // Bounded by the explicit call ceilings; prevents an effect ID crossing turns.
    pub(crate) effect_ids: BTreeSet<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdaptiveTransitionV1 {
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
    MarkUnknown {
        effect: AdaptiveEffectV1,
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
                Cursor::ReadyForModel,
                Command::ClaimModel {
                    effect,
                    previous_observation_digest,
                },
            ) => {
                if now_ms >= self.grant.deadline_ms
                    || self.model_calls >= self.grant.max_model_calls
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
                if !validate_sha256(result_digest) {
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
                if now_ms >= self.grant.deadline_ms || self.tool_calls >= self.grant.max_tool_calls
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
                Cursor::ToolPending { effect, .. } | Cursor::ToolUnknown { effect, .. },
                Command::ObserveTool { observation },
            ) if *effect == observation.effect => {
                if !validate_sha256(&observation.observation_digest) {
                    return Err(invalid());
                }
                // A failed command's confirmed output is feedback, not a failed work item.
                next.last_observation = Some(observation.clone());
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
            (Cursor::ReadyForModel | Cursor::ReadyForTool { .. }, Command::Cancel) => {
                Cursor::Cancelled
            }
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
}

pub fn adaptive_tool_digest(tool: &WorkbenchTool) -> Result<String, WorkflowError> {
    tool.validate_shape().map_err(|_| invalid())?;
    let encoded = serde_json::to_vec(tool).map_err(|_| invalid())?;
    if encoded.is_empty() || encoded.len() > ADAPTIVE_TOOL_MAX_BYTES {
        return Err(invalid());
    }
    canonical_sha256("sentinel.workflow.adaptive-tool.v1", tool)
}

fn valid_reason(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn invalid() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::InvalidTransition,
        false,
        "adaptive transition is invalid or exceeds its grant",
    )
}
