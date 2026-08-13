//! Versioned protocol shared by the daemon and the isolated agent workbench.
//!
//! The wire format is newline-delimited JSON. Every effect-bearing request is
//! digest-bound so replay and recovery can distinguish an idempotent retry from
//! reuse of an invocation ID with different authority or inputs.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::AgentId;

pub const WORKBENCH_SCHEMA_VERSION: u16 = 1;
pub const WORKBENCH_RUNTIME_BWRAP: &str = "bwrap-landlock";
/// Exact isolated runtime version accepted by the v1 startup attestation.
pub const WORKBENCH_AGENT_RUNTIME_VERSION: &str = "0.1.0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkbenchRequest {
    pub schema_version: u16,
    pub invocation_id: String,
    pub agent_id: AgentId,
    pub project_id: String,
    pub work_item_id: String,
    pub workspace_id: String,
    pub caller_id: String,
    pub caller_role: String,
    pub assignment_version: u64,
    pub credential_generation: u64,
    pub policy_digest: String,
    pub tool_profile: String,
    pub tool_profile_digest: String,
    pub runtime_key: String,
    pub capabilities: BTreeSet<String>,
    #[serde(default)]
    pub output_artifact_kinds: BTreeSet<String>,
    #[serde(default)]
    pub inputs: Vec<WorkbenchInputRef>,
    #[serde(default)]
    pub command_policy: Vec<CommandRule>,
    pub resource_limits: WorkbenchResourceLimits,
    pub deadline_unix_ms: u64,
    pub attempt: u32,
    pub tool: WorkbenchTool,
    pub input_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkbenchInputRef {
    pub artifact_id: String,
    pub sha256: String,
    pub mount_path: String,
    pub media_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRule {
    pub program: String,
    #[serde(default)]
    pub required_arg_prefix: Vec<String>,
    pub max_args: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkbenchResourceLimits {
    pub wall_time_ms: u64,
    pub cpu_time_ms: u64,
    pub memory_bytes: u64,
    pub process_count: u32,
    pub file_bytes: u64,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkbenchTool {
    InspectFile {
        path: String,
        max_bytes: u64,
    },
    WriteFile {
        path: String,
        content: String,
        #[serde(default)]
        expected_sha256: Option<String>,
    },
    ApplyPatch {
        path: String,
        expected_sha256: String,
        replacements: Vec<TextReplacement>,
    },
    RunCommand {
        program: String,
        #[serde(default)]
        args: Vec<String>,
    },
    RunTests {
        suite_id: String,
        program: String,
        #[serde(default)]
        args: Vec<String>,
    },
    PackageArtifact {
        artifact_kind: String,
        media_type: String,
        paths: Vec<String>,
    },
}

impl WorkbenchTool {
    pub fn required_capability(&self) -> &'static str {
        match self {
            Self::InspectFile { .. } => "file.inspect",
            Self::WriteFile { .. } => "file.write",
            Self::ApplyPatch { .. } => "patch.apply",
            Self::RunCommand { .. } => "command.run_allowlisted",
            Self::RunTests { .. } => "test.run_profile",
            Self::PackageArtifact { .. } => "artifact.commit",
        }
    }

    pub fn command(&self) -> Option<(&str, &[String])> {
        match self {
            Self::RunCommand { program, args } | Self::RunTests { program, args, .. } => {
                Some((program, args))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextReplacement {
    pub old: String,
    pub new: String,
    #[serde(default = "one_occurrence")]
    pub expected_occurrences: u32,
}

const fn one_occurrence() -> u32 {
    1
}

#[derive(Serialize)]
struct CanonicalRequest<'a> {
    schema_version: u16,
    invocation_id: &'a str,
    agent_id: AgentId,
    project_id: &'a str,
    work_item_id: &'a str,
    workspace_id: &'a str,
    caller_id: &'a str,
    caller_role: &'a str,
    assignment_version: u64,
    credential_generation: u64,
    policy_digest: &'a str,
    tool_profile: &'a str,
    tool_profile_digest: &'a str,
    runtime_key: &'a str,
    capabilities: &'a BTreeSet<String>,
    output_artifact_kinds: &'a BTreeSet<String>,
    inputs: &'a [WorkbenchInputRef],
    command_policy: &'a [CommandRule],
    resource_limits: &'a WorkbenchResourceLimits,
    deadline_unix_ms: u64,
    attempt: u32,
    tool: &'a WorkbenchTool,
}

impl WorkbenchRequest {
    pub fn canonical_digest(&self) -> Result<String, WorkbenchValidationError> {
        let canonical = CanonicalRequest {
            schema_version: self.schema_version,
            invocation_id: &self.invocation_id,
            agent_id: self.agent_id,
            project_id: &self.project_id,
            work_item_id: &self.work_item_id,
            workspace_id: &self.workspace_id,
            caller_id: &self.caller_id,
            caller_role: &self.caller_role,
            assignment_version: self.assignment_version,
            credential_generation: self.credential_generation,
            policy_digest: &self.policy_digest,
            tool_profile: &self.tool_profile,
            tool_profile_digest: &self.tool_profile_digest,
            runtime_key: &self.runtime_key,
            capabilities: &self.capabilities,
            output_artifact_kinds: &self.output_artifact_kinds,
            inputs: &self.inputs,
            command_policy: &self.command_policy,
            resource_limits: &self.resource_limits,
            deadline_unix_ms: self.deadline_unix_ms,
            attempt: self.attempt,
            tool: &self.tool,
        };
        let bytes = serde_json::to_vec(&canonical)
            .map_err(|error| WorkbenchValidationError::Serialization(error.to_string()))?;
        Ok(hex_sha256(&bytes))
    }

    pub fn bind_digest(mut self) -> Result<Self, WorkbenchValidationError> {
        self.input_digest = self.canonical_digest()?;
        Ok(self)
    }

    pub fn validate_at(&self, now_unix_ms: u64) -> Result<(), WorkbenchValidationError> {
        if self.schema_version != WORKBENCH_SCHEMA_VERSION {
            return Err(WorkbenchValidationError::UnsupportedVersion(
                self.schema_version,
            ));
        }
        Uuid::parse_str(&self.invocation_id)
            .map_err(|_| WorkbenchValidationError::InvalidInvocationId)?;
        if AgentId::new(self.agent_id.0).is_err() {
            return Err(WorkbenchValidationError::InvalidAgentId(self.agent_id.0));
        }
        for (name, value) in [
            ("project_id", self.project_id.as_str()),
            ("work_item_id", self.work_item_id.as_str()),
            ("workspace_id", self.workspace_id.as_str()),
            ("caller_id", self.caller_id.as_str()),
            ("caller_role", self.caller_role.as_str()),
            ("tool_profile", self.tool_profile.as_str()),
            ("runtime_key", self.runtime_key.as_str()),
        ] {
            validate_identifier(name, value)?;
        }
        if self.workspace_id != format!("{}:{}", self.project_id, self.work_item_id) {
            return Err(WorkbenchValidationError::InvalidWorkspaceBinding);
        }
        for (name, digest) in [
            ("policy_digest", self.policy_digest.as_str()),
            ("tool_profile_digest", self.tool_profile_digest.as_str()),
            ("input_digest", self.input_digest.as_str()),
        ] {
            validate_sha256(name, digest)?;
        }
        if self.assignment_version == 0 || self.credential_generation == 0 || self.attempt == 0 {
            return Err(WorkbenchValidationError::InvalidGeneration);
        }
        if self.deadline_unix_ms <= now_unix_ms {
            return Err(WorkbenchValidationError::DeadlineExpired);
        }
        self.resource_limits.validate()?;
        for capability in &self.capabilities {
            validate_identifier("capability", capability)?;
        }
        for artifact_kind in &self.output_artifact_kinds {
            validate_identifier("output_artifact_kind", artifact_kind)?;
        }
        if let WorkbenchTool::PackageArtifact { artifact_kind, .. } = &self.tool {
            if !self.output_artifact_kinds.contains(artifact_kind) {
                return Err(WorkbenchValidationError::ArtifactKindDenied);
            }
        }
        if !self.capabilities.contains(self.tool.required_capability()) {
            return Err(WorkbenchValidationError::CapabilityDenied(
                self.tool.required_capability().to_string(),
            ));
        }
        for input in &self.inputs {
            validate_identifier("artifact_id", &input.artifact_id)?;
            validate_sha256("input sha256", &input.sha256)?;
            validate_relative_path(&input.mount_path)?;
            if input.media_type.is_empty() {
                return Err(WorkbenchValidationError::InvalidIdentifier(
                    "media_type".to_string(),
                ));
            }
        }
        for rule in &self.command_policy {
            rule.validate()?;
        }
        validate_tool_paths(&self.tool)?;
        if let Some((program, args)) = self.tool.command() {
            validate_program(program)?;
            for argument in args {
                validate_command_argument(argument)?;
            }
            if !self
                .command_policy
                .iter()
                .any(|rule| rule.allows(program, args))
            {
                return Err(WorkbenchValidationError::CommandDenied);
            }
        }
        let actual = self.canonical_digest()?;
        if !constant_time_ascii_eq(actual.as_bytes(), self.input_digest.as_bytes()) {
            return Err(WorkbenchValidationError::DigestConflict);
        }
        Ok(())
    }
}

fn validate_command_argument(argument: &str) -> Result<(), WorkbenchValidationError> {
    let path = std::path::Path::new(argument);
    if argument.is_empty()
        || argument.contains('\0')
        || argument.starts_with('~')
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(WorkbenchValidationError::CommandArgumentEscape);
    }
    Ok(())
}

impl CommandRule {
    fn validate(&self) -> Result<(), WorkbenchValidationError> {
        validate_program(&self.program)?;
        if self.required_arg_prefix.len() > usize::from(self.max_args) {
            return Err(WorkbenchValidationError::InvalidCommandRule);
        }
        Ok(())
    }

    pub fn allows(&self, program: &str, args: &[String]) -> bool {
        self.program == program
            && args.len() <= usize::from(self.max_args)
            && args.starts_with(&self.required_arg_prefix)
            && args[self.required_arg_prefix.len()..]
                .iter()
                .all(|argument| !argument.starts_with('-'))
    }
}

impl WorkbenchResourceLimits {
    fn validate(&self) -> Result<(), WorkbenchValidationError> {
        if self.wall_time_ms == 0
            || self.cpu_time_ms == 0
            || self.memory_bytes == 0
            || self.process_count == 0
            || self.file_bytes == 0
            || self.stdout_bytes == 0
            || self.stderr_bytes == 0
        {
            return Err(WorkbenchValidationError::InvalidResourceLimits);
        }
        Ok(())
    }
}

fn validate_tool_paths(tool: &WorkbenchTool) -> Result<(), WorkbenchValidationError> {
    match tool {
        WorkbenchTool::InspectFile { path, max_bytes } => {
            validate_relative_path(path)?;
            if *max_bytes == 0 {
                return Err(WorkbenchValidationError::InvalidResourceLimits);
            }
            Ok(())
        }
        WorkbenchTool::WriteFile {
            path,
            expected_sha256,
            ..
        } => {
            validate_relative_path(path)?;
            if let Some(digest) = expected_sha256 {
                validate_sha256("expected_sha256", digest)?;
            }
            Ok(())
        }
        WorkbenchTool::ApplyPatch {
            path,
            expected_sha256,
            replacements,
        } => {
            validate_relative_path(path)?;
            validate_sha256("expected_sha256", expected_sha256)?;
            if replacements.is_empty()
                || replacements.iter().any(|replacement| {
                    replacement.old.is_empty() || replacement.expected_occurrences == 0
                })
            {
                return Err(WorkbenchValidationError::InvalidPatch);
            }
            Ok(())
        }
        WorkbenchTool::PackageArtifact {
            artifact_kind,
            media_type,
            paths,
        } => {
            validate_identifier("artifact_kind", artifact_kind)?;
            validate_media_type(media_type)?;
            if paths.is_empty() {
                return Err(WorkbenchValidationError::EmptyArtifact);
            }
            paths
                .iter()
                .try_for_each(|path| validate_relative_path(path))
        }
        WorkbenchTool::RunTests { suite_id, .. } => validate_identifier("suite_id", suite_id),
        WorkbenchTool::RunCommand { .. } => Ok(()),
    }
}

fn validate_media_type(value: &str) -> Result<(), WorkbenchValidationError> {
    if value.is_empty()
        || value.len() > 128
        || !value.contains('/')
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'+' | b'-')
        })
    {
        return Err(WorkbenchValidationError::InvalidIdentifier(
            "media_type".to_string(),
        ));
    }
    Ok(())
}

fn validate_relative_path(path: &str) -> Result<(), WorkbenchValidationError> {
    let path = std::path::Path::new(path);
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(WorkbenchValidationError::InvalidPath);
    }
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        return Err(WorkbenchValidationError::InvalidPath);
    }
    Ok(())
}

fn validate_program(program: &str) -> Result<(), WorkbenchValidationError> {
    if program.is_empty()
        || program.contains('/')
        || !program
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(WorkbenchValidationError::InvalidProgram);
    }
    Ok(())
}

fn validate_identifier(name: &str, value: &str) -> Result<(), WorkbenchValidationError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(WorkbenchValidationError::InvalidIdentifier(
            name.to_string(),
        ));
    }
    Ok(())
}

fn validate_sha256(name: &str, digest: &str) -> Result<(), WorkbenchValidationError> {
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(WorkbenchValidationError::InvalidDigest(name.to_string()));
    }
    Ok(())
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn constant_time_ascii_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkbenchCommand {
    Execute {
        request: Box<WorkbenchRequest>,
    },
    Cancel {
        schema_version: u16,
        invocation_id: String,
        reason: String,
    },
    Recover {
        schema_version: u16,
        invocation_id: String,
        input_digest: String,
    },
    Health {
        schema_version: u16,
        request_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkbenchMessage {
    Progress {
        schema_version: u16,
        invocation_id: String,
        stage: WorkbenchProgressStage,
        elapsed_ms: u64,
    },
    Result {
        schema_version: u16,
        invocation_id: String,
        input_digest: String,
        outcome: WorkbenchOutcome,
        resources: WorkbenchResourceUsage,
        #[serde(default)]
        artifacts: Vec<WorkbenchArtifactRef>,
        #[serde(default)]
        output: BTreeMap<String, String>,
        #[serde(default)]
        error: Option<WorkbenchErrorInfo>,
    },
    Cancelled {
        schema_version: u16,
        invocation_id: String,
    },
    Health {
        schema_version: u16,
        request_id: String,
        healthy: bool,
        active_invocations: u32,
    },
    Error {
        schema_version: u16,
        #[serde(default)]
        invocation_id: Option<String>,
        error: WorkbenchErrorInfo,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkbenchProgressStage {
    Validated,
    Executing,
    Packaging,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkbenchOutcome {
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    DigestConflict,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkbenchResourceUsage {
    pub duration_ms: u64,
    pub cpu_time_ms: u64,
    pub peak_memory_bytes: u64,
    pub peak_process_count: u32,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub artifact_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkbenchArtifactRef {
    pub artifact_id: String,
    pub sha256: String,
    pub artifact_kind: String,
    pub media_type: String,
    pub size_bytes: u64,
    pub manifest_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkbenchErrorInfo {
    pub class: WorkbenchErrorClass,
    pub code: String,
    pub safe_message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkbenchErrorClass {
    Protocol,
    Authorization,
    Policy,
    Workspace,
    Runtime,
    Tool,
    Resource,
    Recovery,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WorkbenchValidationError {
    #[error("unsupported workbench schema version {0}")]
    UnsupportedVersion(u16),
    #[error("invalid invocation ID")]
    InvalidInvocationId,
    #[error("invalid agent ID {0}")]
    InvalidAgentId(u16),
    #[error("invalid identifier: {0}")]
    InvalidIdentifier(String),
    #[error("invalid digest: {0}")]
    InvalidDigest(String),
    #[error("invalid assignment, credential, or attempt generation")]
    InvalidGeneration,
    #[error("workspace ID is not bound to its project and work item")]
    InvalidWorkspaceBinding,
    #[error("request deadline has expired")]
    DeadlineExpired,
    #[error("invalid resource limits")]
    InvalidResourceLimits,
    #[error("required capability denied: {0}")]
    CapabilityDenied(String),
    #[error("command is not allowed by the bound policy")]
    CommandDenied,
    #[error("invalid program")]
    InvalidProgram,
    #[error("invalid command policy rule")]
    InvalidCommandRule,
    #[error("command argument escapes the assigned workspace")]
    CommandArgumentEscape,
    #[error("invalid patch definition")]
    InvalidPatch,
    #[error("invalid workspace-relative path")]
    InvalidPath,
    #[error("artifact path set must not be empty")]
    EmptyArtifact,
    #[error("artifact kind is not declared by the request")]
    ArtifactKindDenied,
    #[error("request digest does not match canonical bytes")]
    DigestConflict,
    #[error("canonical serialization failed: {0}")]
    Serialization(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> WorkbenchRequest {
        WorkbenchRequest {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: "018f3f32-4f01-7f2c-a6c1-f6f4a81b2801".to_string(),
            agent_id: AgentId(7),
            project_id: "project-01".to_string(),
            work_item_id: "work-04".to_string(),
            workspace_id: "project-01:work-04".to_string(),
            caller_id: "AGENT-07".to_string(),
            caller_role: "developer".to_string(),
            assignment_version: 2,
            credential_generation: 1,
            policy_digest: "a".repeat(64),
            tool_profile: "web-authoring-v1".to_string(),
            tool_profile_digest: "b".repeat(64),
            runtime_key: WORKBENCH_RUNTIME_BWRAP.to_string(),
            capabilities: BTreeSet::from(["file.write".to_string()]),
            output_artifact_kinds: BTreeSet::from(["source_tree".to_string()]),
            inputs: vec![],
            command_policy: vec![],
            resource_limits: WorkbenchResourceLimits {
                wall_time_ms: 30_000,
                cpu_time_ms: 10_000,
                memory_bytes: 134_217_728,
                process_count: 16,
                file_bytes: 8_388_608,
                stdout_bytes: 65_536,
                stderr_bytes: 65_536,
            },
            deadline_unix_ms: 2_000_000_000_000,
            attempt: 1,
            tool: WorkbenchTool::WriteFile {
                path: "src/index.html".to_string(),
                content: "<!doctype html>".to_string(),
                expected_sha256: None,
            },
            input_digest: String::new(),
        }
        .bind_digest()
        .unwrap()
    }

    #[test]
    fn request_round_trip_preserves_digest_and_validates() {
        let request = request();
        let json = serde_json::to_string(&WorkbenchCommand::Execute {
            request: Box::new(request.clone()),
        })
        .unwrap();
        let decoded: WorkbenchCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(
            decoded,
            WorkbenchCommand::Execute {
                request: Box::new(request.clone())
            }
        );
        request.validate_at(1_900_000_000_000).unwrap();
    }

    #[test]
    fn unknown_version_and_unknown_fields_fail_closed() {
        let mut request = request();
        request.schema_version = 2;
        request.input_digest = request.canonical_digest().unwrap();
        assert_eq!(
            request.validate_at(1_900_000_000_000),
            Err(WorkbenchValidationError::UnsupportedVersion(2))
        );

        let json =
            r#"{"kind":"health","schema_version":1,"request_id":"r1","ambient_authority":true}"#;
        assert!(serde_json::from_str::<WorkbenchCommand>(json).is_err());
    }

    #[test]
    fn authority_or_input_mutation_is_a_digest_conflict() {
        let mut request = request();
        request.assignment_version += 1;
        assert_eq!(
            request.validate_at(1_900_000_000_000),
            Err(WorkbenchValidationError::DigestConflict)
        );
    }

    #[test]
    fn traversal_and_missing_capability_fail_closed() {
        let mut traversal_request = request();
        traversal_request.tool = WorkbenchTool::WriteFile {
            path: "../foreign/secret".to_string(),
            content: "x".to_string(),
            expected_sha256: None,
        };
        traversal_request.input_digest = traversal_request.canonical_digest().unwrap();
        assert_eq!(
            traversal_request.validate_at(1_900_000_000_000),
            Err(WorkbenchValidationError::InvalidPath)
        );

        let mut request = request();
        request.capabilities.clear();
        request.input_digest = request.canonical_digest().unwrap();
        assert_eq!(
            request.validate_at(1_900_000_000_000),
            Err(WorkbenchValidationError::CapabilityDenied(
                "file.write".to_string()
            ))
        );
    }

    #[test]
    fn workspace_binding_and_command_argument_escape_fail_closed() {
        let mut mismatched = request();
        mismatched.workspace_id = "project-01:work-99".to_string();
        mismatched.input_digest = mismatched.canonical_digest().unwrap();
        assert_eq!(
            mismatched.validate_at(1_900_000_000_000),
            Err(WorkbenchValidationError::InvalidWorkspaceBinding)
        );

        let mut command = request();
        command.capabilities = BTreeSet::from(["command.run_allowlisted".to_string()]);
        command.command_policy = vec![CommandRule {
            program: "node".to_string(),
            required_arg_prefix: vec!["--check".to_string()],
            max_args: 2,
        }];
        command.tool = WorkbenchTool::RunCommand {
            program: "node".to_string(),
            args: vec!["--check".to_string(), "../foreign/index.js".to_string()],
        };
        command.input_digest = command.canonical_digest().unwrap();
        assert_eq!(
            command.validate_at(1_900_000_000_000),
            Err(WorkbenchValidationError::CommandArgumentEscape)
        );
    }

    #[test]
    fn command_must_match_the_digest_bound_rule() {
        let mut request = request();
        request.capabilities = BTreeSet::from(["command.run_allowlisted".to_string()]);
        request.command_policy = vec![CommandRule {
            program: "node".to_string(),
            required_arg_prefix: vec!["--check".to_string()],
            max_args: 2,
        }];
        request.tool = WorkbenchTool::RunCommand {
            program: "node".to_string(),
            args: vec!["--check".to_string(), "src/app.js".to_string()],
        };
        request.input_digest = request.canonical_digest().unwrap();
        request.validate_at(1_900_000_000_000).unwrap();

        request.tool = WorkbenchTool::RunCommand {
            program: "node".to_string(),
            args: vec![
                "--check".to_string(),
                "--require=/proc/self/environ".to_string(),
            ],
        };
        request.input_digest = request.canonical_digest().unwrap();
        assert_eq!(
            request.validate_at(1_900_000_000_000),
            Err(WorkbenchValidationError::CommandDenied)
        );

        request.tool = WorkbenchTool::RunCommand {
            program: "sh".to_string(),
            args: vec!["-c".to_string(), "id".to_string()],
        };
        request.input_digest = request.canonical_digest().unwrap();
        assert_eq!(
            request.validate_at(1_900_000_000_000),
            Err(WorkbenchValidationError::CommandDenied)
        );
    }

    #[test]
    fn package_artifact_kind_must_be_digest_bound_and_declared() {
        let mut request = request();
        request.capabilities = BTreeSet::from(["artifact.commit".to_string()]);
        request.tool = WorkbenchTool::PackageArtifact {
            artifact_kind: "binary".to_string(),
            media_type: "application/octet-stream".to_string(),
            paths: vec!["dist/app".to_string()],
        };
        request.input_digest = request.canonical_digest().unwrap();
        assert_eq!(
            request.validate_at(1_900_000_000_000),
            Err(WorkbenchValidationError::ArtifactKindDenied)
        );

        request.output_artifact_kinds.insert("binary".to_string());
        request.input_digest = request.canonical_digest().unwrap();
        request.validate_at(1_900_000_000_000).unwrap();
    }
}
