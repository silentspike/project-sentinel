//! Durable, fail-closed coordination for capability-scoped workbench invocations.
//!
//! Runtime stdout and private tool output are deliberately absent from this
//! store. The durable record contains only authority bindings, state, resource
//! accounting, safe error classification, and content-addressed artifacts.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use anyhow::{bail, Context};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use sentinel_common::{
    AgentId, DomainEventPayload, WorkbenchArtifactRef, WorkbenchErrorInfo, WorkbenchMessage,
    WorkbenchOutcome, WorkbenchRequest, WorkbenchResourceUsage, WORKBENCH_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const INVOCATIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("workbench_invocations_v1");
const STORE_SCHEMA_VERSION: u16 = 1;
const PROFILE_SCHEMA_VERSION: u16 = 1;
const MAX_PROFILE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkbenchProfile {
    pub schema_version: u16,
    pub id: String,
    pub runtime_key: String,
    pub network: String,
    pub environment: BTreeMap<String, String>,
    pub capabilities: BTreeSet<String>,
    pub output_artifact_kinds: BTreeSet<String>,
    pub resource_ceilings: sentinel_common::WorkbenchResourceLimits,
    pub command_rules: Vec<sentinel_common::CommandRule>,
    pub test_suites: Vec<WorkbenchTestSuite>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkbenchTestSuite {
    pub id: String,
    pub program: String,
    pub required_arg_prefix: Vec<String>,
    pub max_args: u16,
}

impl WorkbenchProfile {
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<(Self, String)> {
        let metadata = fs::metadata(path.as_ref()).context("stat workbench profile")?;
        if metadata.len() == 0 || metadata.len() > MAX_PROFILE_BYTES {
            bail!("workbench profile size is outside the accepted boundary");
        }
        let bytes = fs::read(path.as_ref()).context("read workbench profile")?;
        let profile: Self =
            toml::from_str(std::str::from_utf8(&bytes).context("workbench profile is not UTF-8")?)
                .context("parse workbench profile")?;
        profile.validate_definition()?;
        Ok((profile, hex_sha256(&bytes)))
    }

    pub fn authorize_request(
        &self,
        profile_digest: &str,
        request: &WorkbenchRequest,
    ) -> anyhow::Result<()> {
        if request.tool_profile != self.id
            || request.tool_profile_digest != profile_digest
            || request.runtime_key != self.runtime_key
            || !request.capabilities.is_subset(&self.capabilities)
            || !request
                .output_artifact_kinds
                .is_subset(&self.output_artifact_kinds)
            || !within_resource_ceilings(&request.resource_limits, &self.resource_ceilings)
            || !request
                .command_policy
                .iter()
                .all(|rule| self.command_rules.contains(rule))
        {
            bail!("workbench request exceeds or mismatches its immutable profile");
        }
        if let sentinel_common::WorkbenchTool::RunTests {
            suite_id,
            program,
            args,
        } = &request.tool
        {
            let permitted = self.test_suites.iter().any(|suite| {
                suite.id == *suite_id
                    && suite.program == *program
                    && args.len() <= usize::from(suite.max_args)
                    && args.starts_with(&suite.required_arg_prefix)
            });
            if !permitted {
                bail!("workbench test suite is not declared by its immutable profile");
            }
        }
        Ok(())
    }

    fn validate_definition(&self) -> anyhow::Result<()> {
        let safe_environment = BTreeMap::from([
            ("HOME".to_string(), "/workspace".to_string()),
            ("LANG".to_string(), "C.UTF-8".to_string()),
            ("LC_ALL".to_string(), "C.UTF-8".to_string()),
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
        ]);
        if self.schema_version != PROFILE_SCHEMA_VERSION
            || self.id.is_empty()
            || self.runtime_key != sentinel_common::WORKBENCH_RUNTIME_BWRAP
            || self.network != "deny"
            || self.capabilities.is_empty()
            || self.output_artifact_kinds.is_empty()
            || self.environment != safe_environment
        {
            bail!("invalid or unsafe workbench profile definition");
        }
        Ok(())
    }
}

fn within_resource_ceilings(
    requested: &sentinel_common::WorkbenchResourceLimits,
    ceiling: &sentinel_common::WorkbenchResourceLimits,
) -> bool {
    requested.wall_time_ms <= ceiling.wall_time_ms
        && requested.cpu_time_ms <= ceiling.cpu_time_ms
        && requested.memory_bytes <= ceiling.memory_bytes
        && requested.process_count <= ceiling.process_count
        && requested.file_bytes <= ceiling.file_bytes
        && requested.stdout_bytes <= ceiling.stdout_bytes
        && requested.stderr_bytes <= ceiling.stderr_bytes
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

#[derive(Debug, thiserror::Error)]
pub enum WorkbenchStoreError {
    #[error("workbench invocation digest conflict")]
    DigestConflict,
    #[error("workbench invocation is not reserved")]
    NotReserved,
    #[error("invalid workbench invocation transition from {from:?} to {to:?}")]
    InvalidTransition {
        from: WorkbenchInvocationState,
        to: WorkbenchInvocationState,
    },
    #[error("unsupported workbench result version")]
    UnsupportedResultVersion,
    #[error("digest conflicts are rejected before terminal state persistence")]
    ResultDigestConflict,
    #[error("workbench result failed bound output validation")]
    OutputRejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkbenchInvocationState {
    Reserved,
    Executing,
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
}

impl WorkbenchInvocationState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::TimedOut
        )
    }

    fn permits(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Reserved, Self::Executing)
                | (Self::Reserved, Self::Cancelled)
                | (Self::Reserved, Self::Failed)
                | (Self::Reserved, Self::TimedOut)
                | (Self::Executing, Self::Succeeded)
                | (Self::Executing, Self::Failed)
                | (Self::Executing, Self::Cancelled)
                | (Self::Executing, Self::TimedOut)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkbenchInvocationRecord {
    pub store_schema_version: u16,
    pub invocation_id: String,
    pub request_digest: String,
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
    pub tool_class: String,
    pub output_artifact_kinds: BTreeSet<String>,
    pub attempt: u32,
    pub state: WorkbenchInvocationState,
    pub reserved_at_ms: u64,
    pub started_at_ms: Option<u64>,
    pub completed_at_ms: Option<u64>,
    pub resources: Option<WorkbenchResourceUsage>,
    #[serde(default)]
    pub artifacts: Vec<WorkbenchArtifactRef>,
    pub error: Option<WorkbenchErrorInfo>,
}

impl WorkbenchInvocationRecord {
    fn reserved(request: &WorkbenchRequest, now_ms: u64) -> Self {
        Self {
            store_schema_version: STORE_SCHEMA_VERSION,
            invocation_id: request.invocation_id.clone(),
            request_digest: request.input_digest.clone(),
            agent_id: request.agent_id,
            project_id: request.project_id.clone(),
            work_item_id: request.work_item_id.clone(),
            workspace_id: request.workspace_id.clone(),
            caller_id: request.caller_id.clone(),
            caller_role: request.caller_role.clone(),
            assignment_version: request.assignment_version,
            credential_generation: request.credential_generation,
            policy_digest: request.policy_digest.clone(),
            tool_profile: request.tool_profile.clone(),
            tool_profile_digest: request.tool_profile_digest.clone(),
            runtime_key: request.runtime_key.clone(),
            tool_class: request.tool.required_capability().to_string(),
            output_artifact_kinds: request.output_artifact_kinds.clone(),
            attempt: request.attempt,
            state: WorkbenchInvocationState::Reserved,
            reserved_at_ms: now_ms,
            started_at_ms: None,
            completed_at_ms: None,
            resources: None,
            artifacts: Vec::new(),
            error: None,
        }
    }

    pub fn safe_event_payload(&self) -> DomainEventPayload {
        DomainEventPayload::WorkbenchInvocationUpdated {
            invocation_id: self.invocation_id.clone(),
            agent_id: self.agent_id,
            project_id: self.project_id.clone(),
            work_item_id: self.work_item_id.clone(),
            tool_class: self.tool_class.clone(),
            runtime_key: self.runtime_key.clone(),
            state: invocation_state_name(self.state).to_string(),
            resources: self.resources.clone().unwrap_or_default(),
            artifact_ids: self
                .artifacts
                .iter()
                .map(|artifact| artifact.artifact_id.clone())
                .collect(),
            error_code: self.error.as_ref().map(|error| error.code.clone()),
        }
    }
}

fn invocation_state_name(state: WorkbenchInvocationState) -> &'static str {
    match state {
        WorkbenchInvocationState::Reserved => "reserved",
        WorkbenchInvocationState::Executing => "executing",
        WorkbenchInvocationState::Succeeded => "succeeded",
        WorkbenchInvocationState::Failed => "failed",
        WorkbenchInvocationState::Cancelled => "cancelled",
        WorkbenchInvocationState::TimedOut => "timed_out",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReservationOutcome {
    Reserved(WorkbenchInvocationRecord),
    Replay(WorkbenchInvocationRecord),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkbenchRecoveryAction {
    DispatchReserved,
    ProbeExecuting,
    ReplayTerminal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkbenchRecoveryItem {
    pub record: WorkbenchInvocationRecord,
    pub action: WorkbenchRecoveryAction,
}

pub struct WorkbenchInvocationStore {
    db: Database,
}

impl WorkbenchInvocationStore {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let db = Database::create(path).with_context(|| {
            format!(
                "failed to create/open workbench invocation store at {}",
                path.display()
            )
        })?;
        let write = db.begin_write()?;
        {
            write.open_table(INVOCATIONS)?;
        }
        write.commit()?;
        Ok(Self { db })
    }

    pub fn reserve(
        &self,
        request: &WorkbenchRequest,
        now_ms: u64,
    ) -> anyhow::Result<ReservationOutcome> {
        request
            .validate_at(now_ms)
            .context("workbench request failed validation before reservation")?;
        let write = self.db.begin_write()?;
        let outcome;
        {
            let mut table = write.open_table(INVOCATIONS)?;
            let existing = table
                .get(request.invocation_id.as_str())?
                .map(|guard| guard.value().to_vec());
            if let Some(bytes) = existing {
                let record = decode_record(&bytes)?;
                if record.request_digest != request.input_digest {
                    return Err(WorkbenchStoreError::DigestConflict.into());
                }
                outcome = ReservationOutcome::Replay(record);
            } else {
                let record = WorkbenchInvocationRecord::reserved(request, now_ms);
                let bytes = encode_record(&record)?;
                table.insert(record.invocation_id.as_str(), bytes.as_slice())?;
                outcome = ReservationOutcome::Reserved(record);
            }
        }
        write.commit()?;
        Ok(outcome)
    }

    pub fn load(&self, invocation_id: &str) -> anyhow::Result<Option<WorkbenchInvocationRecord>> {
        let read = self.db.begin_read()?;
        let table = read.open_table(INVOCATIONS)?;
        table
            .get(invocation_id)?
            .map(|guard| decode_record(guard.value()))
            .transpose()
    }

    pub fn mark_executing(
        &self,
        invocation_id: &str,
        request_digest: &str,
        now_ms: u64,
    ) -> anyhow::Result<WorkbenchInvocationRecord> {
        self.transition(invocation_id, request_digest, now_ms, |record| {
            if !record.state.permits(WorkbenchInvocationState::Executing) {
                return Err(WorkbenchStoreError::InvalidTransition {
                    from: record.state,
                    to: WorkbenchInvocationState::Executing,
                }
                .into());
            }
            record.state = WorkbenchInvocationState::Executing;
            record.started_at_ms = Some(now_ms);
            Ok(())
        })
    }

    pub fn accept_result(
        &self,
        message: &WorkbenchMessage,
        now_ms: u64,
    ) -> anyhow::Result<WorkbenchInvocationRecord> {
        let WorkbenchMessage::Result {
            schema_version,
            invocation_id,
            input_digest,
            outcome,
            resources,
            artifacts,
            output: _,
            error,
        } = message
        else {
            bail!("only a workbench result can complete an invocation");
        };
        if *schema_version != WORKBENCH_SCHEMA_VERSION {
            return Err(WorkbenchStoreError::UnsupportedResultVersion.into());
        }
        let next = state_for_outcome(*outcome)?;
        self.transition(invocation_id, input_digest, now_ms, |record| {
            if record.state == next && record.state.is_terminal() {
                if record.resources.as_ref() != Some(resources)
                    || record.artifacts != *artifacts
                    || record.error.as_ref() != error.as_ref()
                {
                    return Err(WorkbenchStoreError::ResultDigestConflict.into());
                }
                return Ok(());
            }
            if !record.state.permits(next) {
                return Err(WorkbenchStoreError::InvalidTransition {
                    from: record.state,
                    to: next,
                }
                .into());
            }
            validate_bound_outputs(record, *outcome, artifacts, error.as_ref())?;
            record.state = next;
            record.completed_at_ms = Some(now_ms);
            record.resources = Some(resources.clone());
            record.artifacts = artifacts.clone();
            record.error = error.clone();
            Ok(())
        })
    }

    pub fn recovery_items(&self) -> anyhow::Result<Vec<WorkbenchRecoveryItem>> {
        let read = self.db.begin_read()?;
        let table = read.open_table(INVOCATIONS)?;
        let mut items = Vec::new();
        for entry in table.iter()? {
            let (_, value) = entry?;
            let record = decode_record(value.value())?;
            let action = match record.state {
                WorkbenchInvocationState::Reserved => WorkbenchRecoveryAction::DispatchReserved,
                WorkbenchInvocationState::Executing => WorkbenchRecoveryAction::ProbeExecuting,
                state if state.is_terminal() => WorkbenchRecoveryAction::ReplayTerminal,
                _ => unreachable!("all workbench states are covered"),
            };
            items.push(WorkbenchRecoveryItem { record, action });
        }
        items.sort_by(|left, right| left.record.invocation_id.cmp(&right.record.invocation_id));
        Ok(items)
    }

    fn transition(
        &self,
        invocation_id: &str,
        request_digest: &str,
        _now_ms: u64,
        update: impl FnOnce(&mut WorkbenchInvocationRecord) -> anyhow::Result<()>,
    ) -> anyhow::Result<WorkbenchInvocationRecord> {
        let write = self.db.begin_write()?;
        let record;
        {
            let mut table = write.open_table(INVOCATIONS)?;
            let bytes = table
                .get(invocation_id)?
                .map(|guard| guard.value().to_vec())
                .ok_or(WorkbenchStoreError::NotReserved)?;
            let mut current = decode_record(&bytes)?;
            if current.request_digest != request_digest {
                return Err(WorkbenchStoreError::DigestConflict.into());
            }
            update(&mut current)?;
            let bytes = encode_record(&current)?;
            table.insert(invocation_id, bytes.as_slice())?;
            record = current;
        }
        write.commit()?;
        Ok(record)
    }
}

fn validate_bound_outputs(
    record: &WorkbenchInvocationRecord,
    outcome: WorkbenchOutcome,
    artifacts: &[WorkbenchArtifactRef],
    error: Option<&WorkbenchErrorInfo>,
) -> anyhow::Result<()> {
    if matches!(outcome, WorkbenchOutcome::Succeeded) == error.is_some() {
        return Err(WorkbenchStoreError::OutputRejected.into());
    }
    let mut artifact_ids = BTreeSet::new();
    for artifact in artifacts {
        let digest_valid = artifact.sha256.len() == 64
            && artifact
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
        let manifest_path = std::path::Path::new(&artifact.manifest_path);
        let path_valid = !manifest_path.as_os_str().is_empty()
            && !manifest_path.is_absolute()
            && !manifest_path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            });
        if !record
            .output_artifact_kinds
            .contains(&artifact.artifact_kind)
            || artifact.artifact_id != format!("sha256:{}", artifact.sha256)
            || !digest_valid
            || !path_valid
            || artifact.media_type.is_empty()
            || !artifact_ids.insert(&artifact.artifact_id)
        {
            return Err(WorkbenchStoreError::OutputRejected.into());
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct WorkbenchAuthoritySnapshot {
    pub agent_id: AgentId,
    pub caller_id: String,
    pub caller_role: String,
    pub project_id: String,
    pub work_item_id: String,
    pub assignment_version: u64,
    pub credential_generation: u64,
    pub policy_digest: String,
    pub tool_profile: String,
    pub tool_profile_digest: String,
    pub runtime_key: String,
    pub assignment_active: bool,
    pub agent_capabilities: BTreeSet<String>,
    pub role_capabilities: BTreeSet<String>,
    pub assignment_capabilities: BTreeSet<String>,
    pub project_capabilities: BTreeSet<String>,
    pub profile_capabilities: BTreeSet<String>,
}

pub fn authorize_workbench_request(
    request: &WorkbenchRequest,
    authority: &WorkbenchAuthoritySnapshot,
) -> anyhow::Result<BTreeSet<String>> {
    if !authority.assignment_active {
        bail!("workbench assignment is not active");
    }
    if request.agent_id != authority.agent_id
        || request.caller_id != authority.caller_id
        || request.caller_role != authority.caller_role
        || request.project_id != authority.project_id
        || request.work_item_id != authority.work_item_id
        || request.assignment_version != authority.assignment_version
        || request.credential_generation != authority.credential_generation
        || request.policy_digest != authority.policy_digest
        || request.tool_profile != authority.tool_profile
        || request.tool_profile_digest != authority.tool_profile_digest
        || request.runtime_key != authority.runtime_key
    {
        bail!("workbench authority binding is stale or mismatched");
    }
    let effective = authority
        .agent_capabilities
        .intersection(&authority.role_capabilities)
        .cloned()
        .collect::<BTreeSet<_>>()
        .intersection(&authority.assignment_capabilities)
        .cloned()
        .collect::<BTreeSet<_>>()
        .intersection(&authority.project_capabilities)
        .cloned()
        .collect::<BTreeSet<_>>()
        .intersection(&authority.profile_capabilities)
        .cloned()
        .collect::<BTreeSet<_>>();
    if !request.capabilities.is_subset(&effective) {
        bail!("workbench capability denied by effective grant intersection");
    }
    Ok(effective)
}

fn state_for_outcome(outcome: WorkbenchOutcome) -> anyhow::Result<WorkbenchInvocationState> {
    match outcome {
        WorkbenchOutcome::Succeeded => Ok(WorkbenchInvocationState::Succeeded),
        WorkbenchOutcome::Failed => Ok(WorkbenchInvocationState::Failed),
        WorkbenchOutcome::Cancelled => Ok(WorkbenchInvocationState::Cancelled),
        WorkbenchOutcome::TimedOut => Ok(WorkbenchInvocationState::TimedOut),
        WorkbenchOutcome::DigestConflict => Err(WorkbenchStoreError::ResultDigestConflict.into()),
    }
}

fn encode_record(record: &WorkbenchInvocationRecord) -> anyhow::Result<Vec<u8>> {
    serde_json::to_vec(record).context("serialize workbench invocation record")
}

fn decode_record(bytes: &[u8]) -> anyhow::Result<WorkbenchInvocationRecord> {
    let record: WorkbenchInvocationRecord =
        serde_json::from_slice(bytes).context("deserialize workbench invocation record")?;
    if record.store_schema_version != STORE_SCHEMA_VERSION {
        bail!("unsupported workbench invocation store version");
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use sentinel_common::{
        WorkbenchErrorClass, WorkbenchResourceLimits, WorkbenchTool, WORKBENCH_RUNTIME_BWRAP,
    };

    use super::*;

    fn request(invocation_id: &str) -> WorkbenchRequest {
        WorkbenchRequest {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: invocation_id.to_string(),
            agent_id: AgentId(7),
            project_id: "project-01".to_string(),
            work_item_id: "work-04".to_string(),
            workspace_id: "project-01:work-04".to_string(),
            caller_id: "AGENT-07".to_string(),
            caller_role: "developer".to_string(),
            assignment_version: 2,
            credential_generation: 3,
            policy_digest: "a".repeat(64),
            tool_profile: "web-authoring-v1".to_string(),
            tool_profile_digest: "b".repeat(64),
            runtime_key: WORKBENCH_RUNTIME_BWRAP.to_string(),
            capabilities: BTreeSet::from(["file.write".to_string()]),
            output_artifact_kinds: BTreeSet::from(["source_tree".to_string()]),
            inputs: Vec::new(),
            command_policy: Vec::new(),
            resource_limits: WorkbenchResourceLimits {
                wall_time_ms: 30_000,
                cpu_time_ms: 10_000,
                memory_bytes: 128 * 1024 * 1024,
                process_count: 16,
                file_bytes: 1024 * 1024,
                stdout_bytes: 64 * 1024,
                stderr_bytes: 64 * 1024,
            },
            deadline_unix_ms: 2_000_000_000_000,
            attempt: 1,
            tool: WorkbenchTool::WriteFile {
                path: "src/index.html".to_string(),
                content: "ok".to_string(),
                expected_sha256: None,
            },
            input_digest: String::new(),
        }
        .bind_digest()
        .unwrap()
    }

    fn store(directory: &tempfile::TempDir) -> WorkbenchInvocationStore {
        WorkbenchInvocationStore::open(directory.path().join("workbench.redb")).unwrap()
    }

    #[test]
    fn reservation_is_idempotent_and_digest_reuse_conflicts() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        let request = request("018f3f32-4f01-7f2c-a6c1-f6f4a81b2801");
        assert!(matches!(
            store.reserve(&request, 1_900_000_000_000).unwrap(),
            ReservationOutcome::Reserved(_)
        ));
        assert!(matches!(
            store.reserve(&request, 1_900_000_000_001).unwrap(),
            ReservationOutcome::Replay(_)
        ));

        let mut conflicting = request.clone();
        conflicting.tool = WorkbenchTool::WriteFile {
            path: "src/index.html".to_string(),
            content: "different".to_string(),
            expected_sha256: None,
        };
        conflicting.input_digest = conflicting.canonical_digest().unwrap();
        assert!(store
            .reserve(&conflicting, 1_900_000_000_002)
            .unwrap_err()
            .to_string()
            .contains("digest conflict"));
    }

    #[test]
    fn transitions_persist_safe_result_without_private_output() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("workbench.redb");
        let store = WorkbenchInvocationStore::open(&path).unwrap();
        let request = request("018f3f32-4f01-7f2c-a6c1-f6f4a81b2802");
        store.reserve(&request, 1_900_000_000_000).unwrap();
        store
            .mark_executing(
                &request.invocation_id,
                &request.input_digest,
                1_900_000_000_001,
            )
            .unwrap();
        let message = WorkbenchMessage::Result {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: request.invocation_id.clone(),
            input_digest: request.input_digest.clone(),
            outcome: WorkbenchOutcome::Succeeded,
            resources: WorkbenchResourceUsage {
                duration_ms: 42,
                bytes_written: 2,
                ..WorkbenchResourceUsage::default()
            },
            artifacts: Vec::new(),
            output: BTreeMap::from([("private".to_string(), "SECRET-VALUE".to_string())]),
            error: None,
        };
        let completed = store.accept_result(&message, 1_900_000_000_042).unwrap();
        assert_eq!(completed.state, WorkbenchInvocationState::Succeeded);
        let safe_payload = completed.safe_event_payload();
        assert_eq!(
            safe_payload.event_type_str(),
            "workbench_invocation_updated"
        );
        let safe_event = safe_payload.to_json();
        assert!(safe_event.contains("succeeded"));
        assert!(!safe_event.contains("SECRET-VALUE"));
        drop(store);

        let reopened = WorkbenchInvocationStore::open(&path).unwrap();
        let recovered = reopened.load(&request.invocation_id).unwrap().unwrap();
        assert_eq!(recovered.state, WorkbenchInvocationState::Succeeded);
        assert!(!serde_json::to_string(&recovered)
            .unwrap()
            .contains("SECRET-VALUE"));
    }

    #[test]
    fn restart_recovery_never_blindly_retries_executing_work() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        let reserved = request("018f3f32-4f01-7f2c-a6c1-f6f4a81b2803");
        let executing = request("018f3f32-4f01-7f2c-a6c1-f6f4a81b2804");
        store.reserve(&reserved, 1_900_000_000_000).unwrap();
        store.reserve(&executing, 1_900_000_000_000).unwrap();
        store
            .mark_executing(
                &executing.invocation_id,
                &executing.input_digest,
                1_900_000_000_001,
            )
            .unwrap();
        let items = store.recovery_items().unwrap();
        assert_eq!(items[0].action, WorkbenchRecoveryAction::DispatchReserved);
        assert_eq!(items[1].action, WorkbenchRecoveryAction::ProbeExecuting);
    }

    #[test]
    fn effective_authority_is_a_five_way_intersection() {
        let request = request("018f3f32-4f01-7f2c-a6c1-f6f4a81b2805");
        let granted = BTreeSet::from(["file.write".to_string(), "file.inspect".to_string()]);
        let authority = WorkbenchAuthoritySnapshot {
            agent_id: request.agent_id,
            caller_id: request.caller_id.clone(),
            caller_role: request.caller_role.clone(),
            project_id: request.project_id.clone(),
            work_item_id: request.work_item_id.clone(),
            assignment_version: request.assignment_version,
            credential_generation: request.credential_generation,
            policy_digest: request.policy_digest.clone(),
            tool_profile: request.tool_profile.clone(),
            tool_profile_digest: request.tool_profile_digest.clone(),
            runtime_key: request.runtime_key.clone(),
            assignment_active: true,
            agent_capabilities: granted.clone(),
            role_capabilities: granted.clone(),
            assignment_capabilities: granted.clone(),
            project_capabilities: granted.clone(),
            profile_capabilities: BTreeSet::from(["file.write".to_string()]),
        };
        assert_eq!(
            authorize_workbench_request(&request, &authority).unwrap(),
            BTreeSet::from(["file.write".to_string()])
        );

        let mut stale = authority;
        stale.assignment_version += 1;
        assert!(authorize_workbench_request(&request, &stale).is_err());
    }

    #[test]
    fn immutable_profile_binds_runtime_capabilities_and_resource_ceilings() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/workbench-profiles/web-authoring-v1.toml");
        let (profile, digest) = WorkbenchProfile::load(path).unwrap();
        let mut request = request("018f3f32-4f01-7f2c-a6c1-f6f4a81b2809");
        request.tool_profile_digest = digest.clone();
        request.input_digest = request.canonical_digest().unwrap();
        profile.authorize_request(&digest, &request).unwrap();

        request.resource_limits.memory_bytes = profile.resource_ceilings.memory_bytes + 1;
        request.input_digest = request.canonical_digest().unwrap();
        assert!(profile
            .authorize_request(&digest, &request)
            .unwrap_err()
            .to_string()
            .contains("exceeds"));
    }

    #[test]
    fn illegal_transition_and_digest_conflict_result_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        let request = request("018f3f32-4f01-7f2c-a6c1-f6f4a81b2806");
        store.reserve(&request, 1_900_000_000_000).unwrap();
        let result = WorkbenchMessage::Result {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: request.invocation_id.clone(),
            input_digest: request.input_digest.clone(),
            outcome: WorkbenchOutcome::DigestConflict,
            resources: WorkbenchResourceUsage::default(),
            artifacts: Vec::new(),
            output: BTreeMap::new(),
            error: Some(WorkbenchErrorInfo {
                class: WorkbenchErrorClass::Recovery,
                code: "digest_conflict".to_string(),
                safe_message: "conflict".to_string(),
                retryable: false,
            }),
        };
        assert!(store.accept_result(&result, 1_900_000_000_001).is_err());
        assert_eq!(
            store.load(&request.invocation_id).unwrap().unwrap().state,
            WorkbenchInvocationState::Reserved
        );
    }

    #[test]
    fn result_artifacts_are_bound_and_terminal_replay_must_match() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        let request = request("018f3f32-4f01-7f2c-a6c1-f6f4a81b2807");
        store.reserve(&request, 1_900_000_000_000).unwrap();
        store
            .mark_executing(
                &request.invocation_id,
                &request.input_digest,
                1_900_000_000_001,
            )
            .unwrap();
        let artifact = WorkbenchArtifactRef {
            artifact_id: format!("sha256:{}", "c".repeat(64)),
            sha256: "c".repeat(64),
            artifact_kind: "source_tree".to_string(),
            media_type: "application/json".to_string(),
            size_bytes: 42,
            manifest_path: "artifact.manifest.json".to_string(),
        };
        let result = WorkbenchMessage::Result {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: request.invocation_id.clone(),
            input_digest: request.input_digest.clone(),
            outcome: WorkbenchOutcome::Succeeded,
            resources: WorkbenchResourceUsage {
                duration_ms: 7,
                artifact_bytes: 42,
                ..WorkbenchResourceUsage::default()
            },
            artifacts: vec![artifact],
            output: BTreeMap::new(),
            error: None,
        };
        assert_eq!(
            store
                .accept_result(&result, 1_900_000_000_010)
                .unwrap()
                .state,
            WorkbenchInvocationState::Succeeded
        );
        assert!(store.accept_result(&result, 1_900_000_000_011).is_ok());

        let mut conflicting = result;
        let WorkbenchMessage::Result { resources, .. } = &mut conflicting else {
            unreachable!();
        };
        resources.duration_ms += 1;
        assert!(store
            .accept_result(&conflicting, 1_900_000_000_012)
            .unwrap_err()
            .to_string()
            .contains("digest conflicts"));
    }

    #[test]
    fn undeclared_artifact_kind_is_rejected_before_commit() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        let request = request("018f3f32-4f01-7f2c-a6c1-f6f4a81b2808");
        store.reserve(&request, 1_900_000_000_000).unwrap();
        store
            .mark_executing(
                &request.invocation_id,
                &request.input_digest,
                1_900_000_000_001,
            )
            .unwrap();
        let result = WorkbenchMessage::Result {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: request.invocation_id.clone(),
            input_digest: request.input_digest.clone(),
            outcome: WorkbenchOutcome::Succeeded,
            resources: WorkbenchResourceUsage::default(),
            artifacts: vec![WorkbenchArtifactRef {
                artifact_id: format!("sha256:{}", "d".repeat(64)),
                sha256: "d".repeat(64),
                artifact_kind: "binary".to_string(),
                media_type: "application/octet-stream".to_string(),
                size_bytes: 1,
                manifest_path: "artifact.manifest.json".to_string(),
            }],
            output: BTreeMap::new(),
            error: None,
        };
        assert!(store
            .accept_result(&result, 1_900_000_000_010)
            .unwrap_err()
            .to_string()
            .contains("output validation"));
        assert_eq!(
            store.load(&request.invocation_id).unwrap().unwrap().state,
            WorkbenchInvocationState::Executing
        );
    }
}
