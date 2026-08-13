//! Durable, fail-closed coordination for capability-scoped workbench invocations.
//!
//! Runtime stdout and private tool output are deliberately absent from this
//! store. The durable record contains only authority bindings, state, resource
//! accounting, safe error classification, and content-addressed artifacts.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex, OnceLock, RwLock};

use anyhow::{bail, Context};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use sentinel_common::{
    AgentId, DomainEvent, DomainEventPayload, NanoExecRequest, NanoExecResult,
    WorkbenchArtifactRef, WorkbenchCommand, WorkbenchErrorInfo, WorkbenchMessage, WorkbenchOutcome,
    WorkbenchRequest, WorkbenchResourceUsage, WORKBENCH_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const INVOCATIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("workbench_invocations_v1");
const STORE_SCHEMA_VERSION: u16 = 2;
const PROFILE_SCHEMA_VERSION: u16 = 1;
const MAX_PROFILE_BYTES: u64 = 1024 * 1024;
const MAX_ARTIFACT_MANIFEST_BYTES: u64 = 1024 * 1024;
const LINUX_O_NOFOLLOW: i32 = 0o400000;
const LINUX_O_CLOEXEC: i32 = 0o2000000;

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
        let path = path.as_ref();
        let base = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("workbench profile base is unavailable"))?;
        let name = path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("workbench profile name is unavailable"))?;
        let (mut file, identity, canonical_path) =
            open_secure_authority_file(base, Path::new(name), MAX_PROFILE_BYTES, None)
                .context("open workbench profile authority")?;
        let mut bytes = Vec::new();
        file.by_ref()
            .take(MAX_PROFILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .context("read workbench profile")?;
        ensure_path_identity(&canonical_path, identity)
            .context("workbench profile identity changed during read")?;
        if bytes.is_empty() || bytes.len() as u64 > MAX_PROFILE_BYTES {
            bail!("workbench profile size is outside the accepted boundary");
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

impl FileIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

fn current_daemon_uid() -> anyhow::Result<u32> {
    fs::metadata("/proc/self")
        .map(|metadata| metadata.uid())
        .context("read daemon uid")
}

fn canonical_secure_authority_base(base: &Path) -> anyhow::Result<PathBuf> {
    if !base.is_absolute() {
        bail!("authority base must be absolute");
    }
    let mut current = PathBuf::new();
    for component in base.components() {
        match component {
            std::path::Component::RootDir => current.push(std::path::MAIN_SEPARATOR_STR),
            std::path::Component::Normal(component) => current.push(component),
            _ => bail!("authority base is not canonical"),
        }
        let metadata = fs::symlink_metadata(&current).context("inspect authority base")?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("authority base contains an invalid component");
        }
    }
    let canonical = fs::canonicalize(&current).context("resolve authority base")?;
    if canonical != current {
        bail!("authority base changed identity");
    }
    let metadata = fs::metadata(&canonical).context("inspect canonical authority base")?;
    if metadata.uid() != current_daemon_uid()? || metadata.mode() & 0o022 != 0 {
        bail!("authority base ownership or mode is unsafe");
    }
    Ok(canonical)
}

fn secure_authority_name(relative: &Path) -> anyhow::Result<&Path> {
    let mut components = relative.components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        bail!("authority file name is invalid");
    }
    Ok(relative)
}

fn validate_authority_file_metadata(
    metadata: &fs::Metadata,
    max_bytes: u64,
    exact_mode: Option<u32>,
) -> anyhow::Result<FileIdentity> {
    if !authority_file_metadata_is_safe(
        metadata,
        current_daemon_uid()?,
        max_bytes,
        exact_mode,
    ) {
        bail!("authority file identity or size is unsafe");
    }
    Ok(FileIdentity::from_metadata(metadata))
}

fn authority_file_metadata_is_safe(
    metadata: &fs::Metadata,
    expected_uid: u32,
    max_bytes: u64,
    exact_mode: Option<u32>,
) -> bool {
    let mode = metadata.mode() & 0o777;
    metadata.is_file()
        && !metadata.file_type().is_symlink()
        && metadata.nlink() == 1
        && metadata.uid() == expected_uid
        && metadata.len() > 0
        && metadata.len() <= max_bytes
        && !exact_mode.map_or(mode & 0o022 != 0, |expected| mode != expected)
}

fn open_secure_authority_file(
    base: &Path,
    relative: &Path,
    max_bytes: u64,
    exact_mode: Option<u32>,
) -> anyhow::Result<(File, FileIdentity, PathBuf)> {
    let base = canonical_secure_authority_base(base)?;
    let path = base.join(secure_authority_name(relative)?);
    let before = fs::symlink_metadata(&path).context("inspect authority file")?;
    let expected = validate_authority_file_metadata(&before, max_bytes, exact_mode)?;
    let file = OpenOptions::new()
        .read(true)
        .open(&path)
        .context("open authority file")?;
    let opened = file.metadata().context("inspect opened authority file")?;
    if FileIdentity::from_metadata(&opened) != expected {
        bail!("authority file identity changed before open");
    }
    ensure_path_identity(&path, expected)?;
    Ok((file, expected, path))
}

fn open_secure_store_file(
    base: &Path,
    relative: &Path,
) -> anyhow::Result<(File, FileIdentity, PathBuf)> {
    let base = canonical_secure_authority_base(base)?;
    let path = base.join(secure_authority_name(relative)?);
    open_secure_store_file_at(&base, &path, |path| {
        OpenOptions::new().read(true).write(true).open(path)
    })
}

fn open_secure_store_file_at<Open>(
    base: &Path,
    path: &Path,
    open_existing: Open,
) -> anyhow::Result<(File, FileIdentity, PathBuf)>
where
    Open: FnOnce(&Path) -> std::io::Result<File>,
{
    let (file, inspected_identity) = match fs::symlink_metadata(path) {
        Ok(metadata) => {
            let identity = validate_authority_file_metadata(&metadata, u64::MAX, Some(0o600))?;
            (
                open_existing(path).context("open existing workbench store")?,
                Some(identity),
            )
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .context("create workbench store")?;
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .context("set workbench store mode")?;
            file.sync_all().context("sync new workbench store")?;
            File::open(&base)
                .and_then(|directory| directory.sync_all())
                .context("sync workbench store directory")?;
            (file, None)
        }
        Err(error) => return Err(error).context("inspect workbench store"),
    };
    let metadata = file.metadata().context("inspect opened workbench store")?;
    let identity = FileIdentity::from_metadata(&metadata);
    if inspected_identity.is_some_and(|expected| expected != identity) {
        bail!("workbench store identity changed before open");
    }
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != current_daemon_uid()?
        || metadata.mode() & 0o777 != 0o600
    {
        bail!("opened workbench store identity or mode is unsafe");
    }
    ensure_path_identity(path, identity)?;
    Ok((file, identity, path.to_path_buf()))
}

fn ensure_path_identity(path: &Path, expected: FileIdentity) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path).context("reinspect authority file")?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.nlink() != 1
        || FileIdentity::from_metadata(&metadata) != expected
    {
        bail!("authority file identity changed");
    }
    Ok(())
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkbenchRuntimeEnvelope {
    pub schema_version: u16,
    pub invocation_id: String,
    pub state: String,
    pub messages: Vec<WorkbenchMessage>,
}

#[derive(Serialize)]
struct WorkbenchPollFrame<'a> {
    kind: &'static str,
    schema_version: u16,
    invocation_id: &'a str,
}

impl WorkbenchRuntimeEnvelope {
    pub fn start(request: &WorkbenchRequest) -> anyhow::Result<NanoExecRequest> {
        Ok(NanoExecRequest {
            operation: "workbench_start".to_string(),
            input: serde_json::to_string(&WorkbenchCommand::Execute {
                request: Box::new(request.clone()),
            })?,
        })
    }

    pub fn poll(invocation_id: &str) -> anyhow::Result<NanoExecRequest> {
        Ok(NanoExecRequest {
            operation: "workbench_poll".to_string(),
            input: serde_json::to_string(&WorkbenchPollFrame {
                kind: "poll",
                schema_version: WORKBENCH_SCHEMA_VERSION,
                invocation_id,
            })?,
        })
    }

    pub fn cancel(invocation_id: &str) -> anyhow::Result<NanoExecRequest> {
        Ok(NanoExecRequest {
            operation: "workbench_cancel".to_string(),
            input: serde_json::to_string(&WorkbenchCommand::Cancel {
                schema_version: WORKBENCH_SCHEMA_VERSION,
                invocation_id: invocation_id.to_string(),
                reason: "explicit_cancel".to_string(),
            })?,
        })
    }

    pub fn recover(invocation_id: &str, input_digest: &str) -> anyhow::Result<NanoExecRequest> {
        Ok(NanoExecRequest {
            operation: "workbench_recover".to_string(),
            input: serde_json::to_string(&WorkbenchCommand::Recover {
                schema_version: WORKBENCH_SCHEMA_VERSION,
                invocation_id: invocation_id.to_string(),
                input_digest: input_digest.to_string(),
            })?,
        })
    }

    pub fn decode(
        expected_invocation_id: &str,
        expected_workload_id: &str,
        result: &NanoExecResult,
    ) -> anyhow::Result<Self> {
        if result.runtime_key != sentinel_common::WORKBENCH_RUNTIME_BWRAP
            || result.workload_id != expected_workload_id
            || !result.success
        {
            bail!("workbench runtime result is not a successful bwrap exchange");
        }
        let envelope: Self = serde_json::from_str(&result.output)
            .context("decode workbench runtime response envelope")?;
        if envelope.schema_version != WORKBENCH_SCHEMA_VERSION
            || envelope.invocation_id != expected_invocation_id
            || !matches!(
                envelope.state.as_str(),
                "accepted" | "pending" | "cancelling" | "completed"
            )
        {
            bail!("workbench runtime response envelope mismatched its request");
        }
        Ok(envelope)
    }
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
    UnknownOutcome,
}

impl WorkbenchInvocationState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::Failed
                | Self::Cancelled
                | Self::TimedOut
                | Self::UnknownOutcome
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
                | (Self::Executing, Self::UnknownOutcome)
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
    #[serde(default)]
    pub package_artifact_kind: Option<String>,
    #[serde(default)]
    pub package_media_type: Option<String>,
    pub capabilities: BTreeSet<String>,
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
            package_artifact_kind: match &request.tool {
                sentinel_common::WorkbenchTool::PackageArtifact { artifact_kind, .. } => {
                    Some(artifact_kind.clone())
                }
                _ => None,
            },
            package_media_type: match &request.tool {
                sentinel_common::WorkbenchTool::PackageArtifact { media_type, .. } => {
                    Some(media_type.clone())
                }
                _ => None,
            },
            capabilities: request.capabilities.clone(),
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

    pub fn safe_event(&self, tick: u64) -> anyhow::Result<DomainEvent> {
        let payload = self.safe_event_payload();
        let payload_json = payload.to_json();
        let operation_id = format!(
            "workbench:{}:{}",
            self.invocation_id,
            hex_sha256(payload_json.as_bytes())
        );
        Ok(DomainEvent::new(
            payload.event_type_str(),
            &format!("AGENT-{:02}", self.agent_id.0),
            &payload_json,
            &self.invocation_id,
            tick,
        )
        .with_operation_id(&operation_id))
    }
}

pub fn publish_workbench_records(
    event_store: &sentinel_limbo::EventStore,
    records: &[WorkbenchInvocationRecord],
    tick: u64,
) -> anyhow::Result<Vec<i64>> {
    records
        .iter()
        .map(|record| event_store.append_event(&record.safe_event(tick)?))
        .collect()
}

fn invocation_state_name(state: WorkbenchInvocationState) -> &'static str {
    match state {
        WorkbenchInvocationState::Reserved => "reserved",
        WorkbenchInvocationState::Executing => "executing",
        WorkbenchInvocationState::Succeeded => "succeeded",
        WorkbenchInvocationState::Failed => "failed",
        WorkbenchInvocationState::Cancelled => "cancelled",
        WorkbenchInvocationState::TimedOut => "timed_out",
        WorkbenchInvocationState::UnknownOutcome => "unknown_outcome",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReservationOutcome {
    Reserved(WorkbenchInvocationRecord),
    Replay(WorkbenchInvocationRecord),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkbenchRecoveryAction {
    AwaitAuthorizedReplay,
    ProbeExecuting,
    ReplayTerminal,
    ManualRecoveryRequired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkbenchRecoveryItem {
    pub record: WorkbenchInvocationRecord,
    pub action: WorkbenchRecoveryAction,
}

pub struct WorkbenchInvocationStore {
    db: Database,
    artifact_roots: HashMap<AgentId, PathBuf>,
}

impl WorkbenchInvocationStore {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let base = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("workbench store base is unavailable"))?;
        let name = path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("workbench store name is unavailable"))?;
        let (file, identity, canonical_path) =
            open_secure_store_file(base, Path::new(name)).context("open workbench store")?;
        let fd_path = PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()));
        let db = Database::create(&fd_path).context("create/open workbench invocation store")?;
        ensure_path_identity(&canonical_path, identity)
            .context("workbench store identity changed during open")?;
        let write = db.begin_write()?;
        {
            write.open_table(INVOCATIONS)?;
        }
        write.commit()?;
        Ok(Self {
            db,
            artifact_roots: HashMap::new(),
        })
    }

    pub(crate) fn open_with_artifact_roots(
        path: impl AsRef<Path>,
        artifact_roots: HashMap<AgentId, PathBuf>,
    ) -> anyhow::Result<Self> {
        let mut store = Self::open(path)?;
        store.artifact_roots = artifact_roots;
        Ok(store)
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
        self.accept_result_guarded(message, now_ms, &|| Ok(()))
    }

    fn accept_result_guarded(
        &self,
        message: &WorkbenchMessage,
        now_ms: u64,
        revalidate: &dyn Fn() -> anyhow::Result<()>,
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
        let safe_error = error.as_ref().map(sanitize_runtime_error).transpose()?;
        self.transition_guarded(invocation_id, input_digest, revalidate, |record| {
            if record.state == next && record.state.is_terminal() {
                if record.resources.as_ref() != Some(resources)
                    || record.artifacts != *artifacts
                    || record.error.as_ref() != safe_error.as_ref()
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
            validate_bound_outputs(
                record,
                *outcome,
                artifacts,
                safe_error.as_ref(),
                &self.artifact_roots,
            )?;
            record.state = next;
            record.completed_at_ms = Some(now_ms);
            record.resources = Some(resources.clone());
            record.artifacts = artifacts.clone();
            record.error = safe_error.clone();
            Ok(())
        })
    }

    pub fn mark_failed(
        &self,
        invocation_id: &str,
        request_digest: &str,
        now_ms: u64,
        error: WorkbenchErrorInfo,
    ) -> anyhow::Result<WorkbenchInvocationRecord> {
        self.mark_failed_guarded(invocation_id, request_digest, now_ms, error, &|| Ok(()))
    }

    fn mark_failed_guarded(
        &self,
        invocation_id: &str,
        request_digest: &str,
        now_ms: u64,
        error: WorkbenchErrorInfo,
        revalidate: &dyn Fn() -> anyhow::Result<()>,
    ) -> anyhow::Result<WorkbenchInvocationRecord> {
        let safe_error = sanitize_runtime_error(&error)?;
        self.transition_guarded(invocation_id, request_digest, revalidate, |record| {
            if record.state == WorkbenchInvocationState::Failed {
                if record.resources.as_ref() == Some(&WorkbenchResourceUsage::default())
                    && record.artifacts.is_empty()
                    && record.error.as_ref() == Some(&safe_error)
                {
                    return Ok(());
                }
                return Err(WorkbenchStoreError::ResultDigestConflict.into());
            }
            if !record.state.permits(WorkbenchInvocationState::Failed) {
                return Err(WorkbenchStoreError::InvalidTransition {
                    from: record.state,
                    to: WorkbenchInvocationState::Failed,
                }
                .into());
            }
            record.state = WorkbenchInvocationState::Failed;
            record.completed_at_ms = Some(now_ms);
            record.resources = Some(WorkbenchResourceUsage::default());
            record.artifacts.clear();
            record.error = Some(safe_error);
            Ok(())
        })
    }

    pub fn mark_cancelled(
        &self,
        invocation_id: &str,
        request_digest: &str,
        now_ms: u64,
    ) -> anyhow::Result<WorkbenchInvocationRecord> {
        self.mark_cancelled_with_error(
            invocation_id,
            request_digest,
            now_ms,
            WorkbenchErrorInfo {
                class: sentinel_common::WorkbenchErrorClass::Runtime,
                code: "cancelled_before_dispatch".to_string(),
                safe_message: "the invocation was cancelled before dispatch".to_string(),
                retryable: false,
            },
        )
    }

    fn mark_unknown_outcome_guarded(
        &self,
        invocation_id: &str,
        request_digest: &str,
        now_ms: u64,
        error: WorkbenchErrorInfo,
        revalidate: &dyn Fn() -> anyhow::Result<()>,
    ) -> anyhow::Result<WorkbenchInvocationRecord> {
        let safe_error = sanitize_runtime_error(&error)?;
        self.transition_guarded(invocation_id, request_digest, revalidate, |record| {
            if record.state == WorkbenchInvocationState::UnknownOutcome {
                if record.resources.as_ref() == Some(&WorkbenchResourceUsage::default())
                    && record.artifacts.is_empty()
                    && record.error.as_ref() == Some(&safe_error)
                {
                    return Ok(());
                }
                return Err(WorkbenchStoreError::ResultDigestConflict.into());
            }
            if !record
                .state
                .permits(WorkbenchInvocationState::UnknownOutcome)
            {
                return Err(WorkbenchStoreError::InvalidTransition {
                    from: record.state,
                    to: WorkbenchInvocationState::UnknownOutcome,
                }
                .into());
            }
            record.state = WorkbenchInvocationState::UnknownOutcome;
            record.completed_at_ms = Some(now_ms);
            record.resources = Some(WorkbenchResourceUsage::default());
            record.artifacts.clear();
            record.error = Some(safe_error);
            Ok(())
        })
    }

    fn mark_runtime_cancelled_guarded(
        &self,
        invocation_id: &str,
        request_digest: &str,
        now_ms: u64,
        revalidate: &dyn Fn() -> anyhow::Result<()>,
    ) -> anyhow::Result<WorkbenchInvocationRecord> {
        self.mark_cancelled_with_error_guarded(
            invocation_id,
            request_digest,
            now_ms,
            WorkbenchErrorInfo {
                class: sentinel_common::WorkbenchErrorClass::Runtime,
                code: "runtime_cancelled".to_string(),
                safe_message: "the isolated workbench acknowledged cancellation".to_string(),
                retryable: false,
            },
            revalidate,
        )
    }

    fn mark_cancelled_with_error(
        &self,
        invocation_id: &str,
        request_digest: &str,
        now_ms: u64,
        error: WorkbenchErrorInfo,
    ) -> anyhow::Result<WorkbenchInvocationRecord> {
        self.mark_cancelled_with_error_guarded(
            invocation_id,
            request_digest,
            now_ms,
            error,
            &|| Ok(()),
        )
    }

    fn mark_cancelled_with_error_guarded(
        &self,
        invocation_id: &str,
        request_digest: &str,
        now_ms: u64,
        error: WorkbenchErrorInfo,
        revalidate: &dyn Fn() -> anyhow::Result<()>,
    ) -> anyhow::Result<WorkbenchInvocationRecord> {
        let safe_error = sanitize_runtime_error(&error)?;
        self.transition_guarded(invocation_id, request_digest, revalidate, |record| {
            if record.state == WorkbenchInvocationState::Cancelled {
                if record.resources.as_ref() == Some(&WorkbenchResourceUsage::default())
                    && record.artifacts.is_empty()
                    && record.error.as_ref() == Some(&safe_error)
                {
                    return Ok(());
                }
                return Err(WorkbenchStoreError::ResultDigestConflict.into());
            }
            if !record.state.permits(WorkbenchInvocationState::Cancelled) {
                return Err(WorkbenchStoreError::InvalidTransition {
                    from: record.state,
                    to: WorkbenchInvocationState::Cancelled,
                }
                .into());
            }
            record.state = WorkbenchInvocationState::Cancelled;
            record.completed_at_ms = Some(now_ms);
            record.resources = Some(WorkbenchResourceUsage::default());
            record.artifacts.clear();
            record.error = Some(safe_error);
            Ok(())
        })
    }

    pub fn accept_runtime_envelope(
        &self,
        envelope: &WorkbenchRuntimeEnvelope,
        now_ms: u64,
    ) -> anyhow::Result<Option<WorkbenchInvocationRecord>> {
        self.accept_runtime_envelope_guarded(envelope, now_ms, &|| Ok(()))
    }

    fn accept_runtime_envelope_guarded(
        &self,
        envelope: &WorkbenchRuntimeEnvelope,
        now_ms: u64,
        revalidate: &dyn Fn() -> anyhow::Result<()>,
    ) -> anyhow::Result<Option<WorkbenchInvocationRecord>> {
        let mut current = self
            .load(&envelope.invocation_id)?
            .ok_or(WorkbenchStoreError::NotReserved)?;
        let mut terminal_messages = 0_u8;
        for message in &envelope.messages {
            match message {
                WorkbenchMessage::Progress {
                    schema_version,
                    invocation_id,
                    ..
                }
                | WorkbenchMessage::Cancelled {
                    schema_version,
                    invocation_id,
                } => {
                    validate_message_binding(
                        *schema_version,
                        invocation_id,
                        &envelope.invocation_id,
                    )?;
                    if matches!(message, WorkbenchMessage::Cancelled { .. }) {
                        terminal_messages = terminal_messages.saturating_add(1);
                    }
                }
                WorkbenchMessage::Result {
                    schema_version,
                    invocation_id,
                    ..
                } => {
                    validate_message_binding(
                        *schema_version,
                        invocation_id,
                        &envelope.invocation_id,
                    )?;
                    terminal_messages = terminal_messages.saturating_add(1);
                }
                WorkbenchMessage::Error {
                    schema_version,
                    invocation_id,
                    ..
                } => {
                    if *schema_version != WORKBENCH_SCHEMA_VERSION
                        || invocation_id.as_deref() != Some(envelope.invocation_id.as_str())
                    {
                        bail!("workbench runtime message mismatched its invocation");
                    }
                    terminal_messages = terminal_messages.saturating_add(1);
                }
                WorkbenchMessage::Health { .. } => {
                    bail!("health response is invalid inside a workbench exchange");
                }
            }
        }
        if terminal_messages > 1 {
            bail!("workbench runtime envelope contains duplicate terminal messages");
        }

        if envelope.state != "completed" {
            return Ok(None);
        }
        if terminal_messages != 1 {
            bail!("completed workbench exchange must contain one terminal message");
        }

        for message in &envelope.messages {
            match message {
                WorkbenchMessage::Result { .. } => {
                    current = self.accept_result_guarded(message, now_ms, revalidate)?;
                }
                WorkbenchMessage::Error { error, .. } => {
                    current = if error.class == sentinel_common::WorkbenchErrorClass::Recovery {
                        self.mark_unknown_outcome_guarded(
                            &envelope.invocation_id,
                            &current.request_digest,
                            now_ms,
                            error.clone(),
                            revalidate,
                        )?
                    } else {
                        self.mark_failed_guarded(
                            &envelope.invocation_id,
                            &current.request_digest,
                            now_ms,
                            error.clone(),
                            revalidate,
                        )?
                    };
                }
                WorkbenchMessage::Cancelled { .. } => {
                    current = self.mark_runtime_cancelled_guarded(
                        &envelope.invocation_id,
                        &current.request_digest,
                        now_ms,
                        revalidate,
                    )?;
                }
                _ => {}
            }
        }
        if !current.state.is_terminal() {
            bail!("completed workbench exchange has no durable terminal result");
        }
        Ok(current.state.is_terminal().then_some(current))
    }

    pub fn recovery_items(&self) -> anyhow::Result<Vec<WorkbenchRecoveryItem>> {
        let read = self.db.begin_read()?;
        let table = read.open_table(INVOCATIONS)?;
        let mut items = Vec::new();
        for entry in table.iter()? {
            let (_, value) = entry?;
            let record = decode_record(value.value())?;
            let action = match record.state {
                WorkbenchInvocationState::Reserved => {
                    WorkbenchRecoveryAction::AwaitAuthorizedReplay
                }
                WorkbenchInvocationState::Executing => WorkbenchRecoveryAction::ProbeExecuting,
                WorkbenchInvocationState::UnknownOutcome => {
                    WorkbenchRecoveryAction::ManualRecoveryRequired
                }
                state if state.is_terminal() => WorkbenchRecoveryAction::ReplayTerminal,
                _ => unreachable!("all workbench states are covered"),
            };
            items.push(WorkbenchRecoveryItem { record, action });
        }
        items.sort_by(|left, right| left.record.invocation_id.cmp(&right.record.invocation_id));
        Ok(items)
    }

    pub fn has_inflight(&self) -> anyhow::Result<bool> {
        let read = self.db.begin_read()?;
        let table = read.open_table(INVOCATIONS)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            if matches!(
                decode_record(value.value())?.state,
                WorkbenchInvocationState::Reserved | WorkbenchInvocationState::Executing
            ) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn transition(
        &self,
        invocation_id: &str,
        request_digest: &str,
        _now_ms: u64,
        update: impl FnOnce(&mut WorkbenchInvocationRecord) -> anyhow::Result<()>,
    ) -> anyhow::Result<WorkbenchInvocationRecord> {
        self.transition_guarded(invocation_id, request_digest, &|| Ok(()), update)
    }

    fn transition_guarded(
        &self,
        invocation_id: &str,
        request_digest: &str,
        revalidate: &dyn Fn() -> anyhow::Result<()>,
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
            // Recheck the effect's original World capability after any artifact
            // validation and immediately before staging the durable transition.
            revalidate()?;
            let bytes = encode_record(&current)?;
            table.insert(invocation_id, bytes.as_slice())?;
            record = current;
        }
        write.commit()?;
        Ok(record)
    }
}

fn validate_message_binding(
    schema_version: u16,
    invocation_id: &str,
    expected_invocation_id: &str,
) -> anyhow::Result<()> {
    if schema_version != WORKBENCH_SCHEMA_VERSION || invocation_id != expected_invocation_id {
        bail!("workbench runtime message mismatched its invocation");
    }
    Ok(())
}

fn validate_bound_outputs(
    record: &WorkbenchInvocationRecord,
    outcome: WorkbenchOutcome,
    artifacts: &[WorkbenchArtifactRef],
    error: Option<&WorkbenchErrorInfo>,
    artifact_roots: &HashMap<AgentId, PathBuf>,
) -> anyhow::Result<()> {
    if matches!(outcome, WorkbenchOutcome::Succeeded) == error.is_some() {
        return Err(WorkbenchStoreError::OutputRejected.into());
    }
    if !matches!(outcome, WorkbenchOutcome::Succeeded) && !artifacts.is_empty() {
        return Err(WorkbenchStoreError::OutputRejected.into());
    }
    match (
        record.package_artifact_kind.as_deref(),
        record.package_media_type.as_deref(),
    ) {
        (None, None) if !artifacts.is_empty() => {
            return Err(WorkbenchStoreError::OutputRejected.into());
        }
        (Some(_), Some(_))
            if matches!(outcome, WorkbenchOutcome::Succeeded) && artifacts.len() != 1 =>
        {
            return Err(WorkbenchStoreError::OutputRejected.into());
        }
        (None, None) | (Some(_), Some(_)) => {}
        _ => return Err(WorkbenchStoreError::OutputRejected.into()),
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
            && manifest_path.components().count() == 1
            && matches!(
                manifest_path.components().next(),
                Some(std::path::Component::Normal(_))
            )
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
        if record.package_artifact_kind.as_deref() != Some(artifact.artifact_kind.as_str())
            || record.package_media_type.as_deref() != Some(artifact.media_type.as_str())
        {
            return Err(WorkbenchStoreError::OutputRejected.into());
        }
        validate_concrete_artifact_manifest(record, artifact, artifact_roots)?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifiedArtifactManifest {
    schema_version: u16,
    invocation_id: String,
    input_digest: String,
    project_id: String,
    work_item_id: String,
    workspace_id: String,
    agent_id: u16,
    artifact_kind: String,
    media_type: String,
    runtime_key: String,
    tool_profile: String,
    tool_profile_digest: String,
    policy_digest: String,
    entries: Vec<VerifiedArtifactEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifiedArtifactEntry {
    path: String,
    blob_id: String,
    sha256: String,
    size_bytes: u64,
}

fn validate_concrete_artifact_manifest(
    record: &WorkbenchInvocationRecord,
    artifact: &WorkbenchArtifactRef,
    artifact_roots: &HashMap<AgentId, PathBuf>,
) -> anyhow::Result<()> {
    let root = artifact_roots
        .get(&record.agent_id)
        .ok_or(WorkbenchStoreError::OutputRejected)?;
    let scoped_root = bind_daemon_artifact_scope(
        root,
        record.agent_id,
        &record.project_id,
        &record.work_item_id,
    )?;
    let manifest_path = scoped_root.join(&artifact.manifest_path);
    let metadata =
        fs::symlink_metadata(&manifest_path).map_err(|_| WorkbenchStoreError::OutputRejected)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_ARTIFACT_MANIFEST_BYTES
    {
        return Err(WorkbenchStoreError::OutputRejected.into());
    }
    let manifest_path =
        fs::canonicalize(&manifest_path).map_err(|_| WorkbenchStoreError::OutputRejected)?;
    if !manifest_path.starts_with(&scoped_root) {
        return Err(WorkbenchStoreError::OutputRejected.into());
    }
    let manifest_name = manifest_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(WorkbenchStoreError::OutputRejected)?;
    let scope = open_pinned_daemon_artifact_directory(&scoped_root)?;
    let mut manifest_file = open_scoped_daemon_artifact_file(
        &scope,
        manifest_name,
        MAX_ARTIFACT_MANIFEST_BYTES,
        None,
    )?;
    let mut bytes = Vec::new();
    manifest_file
        .by_ref()
        .take(MAX_ARTIFACT_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| WorkbenchStoreError::OutputRejected)?;
    revalidate_scoped_daemon_artifact_file(&scope, manifest_name, &manifest_file)?;
    if hex_sha256(&bytes) != artifact.sha256 {
        return Err(WorkbenchStoreError::OutputRejected.into());
    }
    let manifest: VerifiedArtifactManifest =
        serde_json::from_slice(&bytes).map_err(|_| WorkbenchStoreError::OutputRejected)?;
    if manifest.schema_version != WORKBENCH_SCHEMA_VERSION
        || manifest.invocation_id != record.invocation_id
        || manifest.input_digest != record.request_digest
        || manifest.project_id != record.project_id
        || manifest.work_item_id != record.work_item_id
        || manifest.workspace_id != record.workspace_id
        || manifest.agent_id != record.agent_id.0
        || manifest.artifact_kind != artifact.artifact_kind
        || manifest.media_type != artifact.media_type
        || manifest.runtime_key != record.runtime_key
        || manifest.tool_profile != record.tool_profile
        || manifest.tool_profile_digest != record.tool_profile_digest
        || manifest.policy_digest != record.policy_digest
        || manifest.entries.is_empty()
    {
        return Err(WorkbenchStoreError::OutputRejected.into());
    }
    let blobs_root = canonical_daemon_child_directory(&scoped_root, "blobs")?;
    let blobs = open_pinned_daemon_artifact_directory(&blobs_root)?;
    let mut total_size = 0_u64;
    for entry in manifest.entries {
        let path = Path::new(&entry.path);
        if path.as_os_str().is_empty()
            || path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
            || !valid_lower_sha256(&entry.sha256)
            || entry.blob_id != format!("sha256:{}", entry.sha256)
        {
            return Err(WorkbenchStoreError::OutputRejected.into());
        }
        total_size = total_size
            .checked_add(entry.size_bytes)
            .ok_or(WorkbenchStoreError::OutputRejected)?;
        let mut blob = open_scoped_daemon_artifact_file(
            &blobs,
            &entry.sha256,
            entry.size_bytes,
            Some(entry.size_bytes),
        )?;
        let mut blob_bytes = Vec::new();
        blob.by_ref()
            .take(entry.size_bytes.saturating_add(1))
            .read_to_end(&mut blob_bytes)
            .map_err(|_| WorkbenchStoreError::OutputRejected)?;
        revalidate_scoped_daemon_artifact_file(&blobs, &entry.sha256, &blob)?;
        if blob_bytes.len() as u64 != entry.size_bytes
            || hex_sha256(&blob_bytes) != entry.sha256
        {
            return Err(WorkbenchStoreError::OutputRejected.into());
        }
    }
    if total_size != artifact.size_bytes {
        return Err(WorkbenchStoreError::OutputRejected.into());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ArtifactFileIdentity {
    device: u64,
    inode: u64,
    size: u64,
}

impl ArtifactFileIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
        }
    }
}

fn open_pinned_daemon_artifact_directory(path: &Path) -> anyhow::Result<File> {
    let before = fs::symlink_metadata(path).map_err(|_| WorkbenchStoreError::OutputRejected)?;
    if before.file_type().is_symlink() || !before.is_dir() {
        return Err(WorkbenchStoreError::OutputRejected.into());
    }
    let directory = File::open(path).map_err(|_| WorkbenchStoreError::OutputRejected)?;
    let opened = directory
        .metadata()
        .map_err(|_| WorkbenchStoreError::OutputRejected)?;
    if before.dev() != opened.dev() || before.ino() != opened.ino() {
        return Err(WorkbenchStoreError::OutputRejected.into());
    }
    Ok(directory)
}

fn open_scoped_daemon_artifact_file(
    directory: &File,
    name: &str,
    max_bytes: u64,
    exact_size: Option<u64>,
) -> anyhow::Result<File> {
    open_scoped_daemon_artifact_file_with(directory, name, max_bytes, exact_size, |path| {
        OpenOptions::new()
            .read(true)
            .custom_flags(LINUX_O_NOFOLLOW | LINUX_O_CLOEXEC)
            .open(path)
    })
}

fn open_scoped_daemon_artifact_file_with<Open>(
    directory: &File,
    name: &str,
    max_bytes: u64,
    exact_size: Option<u64>,
    open: Open,
) -> anyhow::Result<File>
where
    Open: FnOnce(&Path) -> std::io::Result<File>,
{
    daemon_scope_component(name)?;
    let path = PathBuf::from(format!("/proc/self/fd/{}/{}", directory.as_raw_fd(), name));
    let before = fs::symlink_metadata(&path).map_err(|_| WorkbenchStoreError::OutputRejected)?;
    validate_daemon_artifact_file_metadata(&before, max_bytes, exact_size)?;
    let expected = ArtifactFileIdentity::from_metadata(&before);
    let file = open(&path).map_err(|_| WorkbenchStoreError::OutputRejected)?;
    let opened = file
        .metadata()
        .map_err(|_| WorkbenchStoreError::OutputRejected)?;
    validate_daemon_artifact_file_metadata(&opened, max_bytes, exact_size)?;
    if ArtifactFileIdentity::from_metadata(&opened) != expected {
        return Err(WorkbenchStoreError::OutputRejected.into());
    }
    Ok(file)
}

fn validate_daemon_artifact_file_metadata(
    metadata: &fs::Metadata,
    max_bytes: u64,
    exact_size: Option<u64>,
) -> anyhow::Result<()> {
    let mode = metadata.mode() & 0o777;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.nlink() != 1
        || metadata.uid() != current_daemon_uid()?
        || mode & 0o022 != 0
        || metadata.len() > max_bytes
        || exact_size.is_some_and(|size| metadata.len() != size)
    {
        return Err(WorkbenchStoreError::OutputRejected.into());
    }
    Ok(())
}

fn revalidate_scoped_daemon_artifact_file(
    directory: &File,
    name: &str,
    file: &File,
) -> anyhow::Result<()> {
    let path = PathBuf::from(format!("/proc/self/fd/{}/{}", directory.as_raw_fd(), name));
    let opened = file
        .metadata()
        .map_err(|_| WorkbenchStoreError::OutputRejected)?;
    let path_metadata =
        fs::symlink_metadata(&path).map_err(|_| WorkbenchStoreError::OutputRejected)?;
    validate_daemon_artifact_file_metadata(&opened, opened.len(), Some(opened.len()))?;
    validate_daemon_artifact_file_metadata(&path_metadata, opened.len(), Some(opened.len()))?;
    if ArtifactFileIdentity::from_metadata(&path_metadata)
            != ArtifactFileIdentity::from_metadata(&opened)
    {
        return Err(WorkbenchStoreError::OutputRejected.into());
    }
    Ok(())
}

fn bind_daemon_artifact_scope(
    artifact_base: &Path,
    agent_id: AgentId,
    project_id: &str,
    work_item_id: &str,
) -> anyhow::Result<PathBuf> {
    use std::os::unix::fs::MetadataExt;

    let artifact_base = canonical_daemon_real_directory(artifact_base)?;
    let agent_root = artifact_base
        .parent()
        .ok_or(WorkbenchStoreError::OutputRejected)?;
    let marker = agent_root.join(".nano-runtime");
    let metadata = fs::symlink_metadata(&marker).map_err(|_| WorkbenchStoreError::OutputRejected)?;
    let expected_workload = format!("AGENT-{:02}", agent_id.0);
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.nlink() != 1
        || fs::read_to_string(&marker).ok().as_deref() != Some(expected_workload.as_str())
    {
        return Err(WorkbenchStoreError::OutputRejected.into());
    }
    let project = canonical_daemon_child_directory(
        &artifact_base,
        daemon_scope_component(project_id)?,
    )?;
    canonical_daemon_child_directory(&project, daemon_scope_component(work_item_id)?)
}

fn daemon_scope_component(value: &str) -> anyhow::Result<&str> {
    let mut components = Path::new(value).components();
    if value.is_empty()
        || !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(WorkbenchStoreError::OutputRejected.into());
    }
    Ok(value)
}

fn canonical_daemon_child_directory(parent: &Path, child: &str) -> anyhow::Result<PathBuf> {
    let path = parent.join(child);
    let metadata = fs::symlink_metadata(&path).map_err(|_| WorkbenchStoreError::OutputRejected)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(WorkbenchStoreError::OutputRejected.into());
    }
    let canonical = fs::canonicalize(&path).map_err(|_| WorkbenchStoreError::OutputRejected)?;
    if canonical.parent() != Some(parent) {
        return Err(WorkbenchStoreError::OutputRejected.into());
    }
    Ok(canonical)
}

fn canonical_daemon_real_directory(path: &Path) -> anyhow::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(WorkbenchStoreError::OutputRejected.into());
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::RootDir => current.push(std::path::MAIN_SEPARATOR_STR),
            std::path::Component::Normal(component) => current.push(component),
            _ => return Err(WorkbenchStoreError::OutputRejected.into()),
        }
        let metadata =
            fs::symlink_metadata(&current).map_err(|_| WorkbenchStoreError::OutputRejected)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(WorkbenchStoreError::OutputRejected.into());
        }
    }
    let canonical = fs::canonicalize(&current).map_err(|_| WorkbenchStoreError::OutputRejected)?;
    if canonical != current {
        return Err(WorkbenchStoreError::OutputRejected.into());
    }
    Ok(canonical)
}

fn valid_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn sanitize_runtime_error(error: &WorkbenchErrorInfo) -> anyhow::Result<WorkbenchErrorInfo> {
    let code_valid = !error.code.is_empty()
        && error.code.len() <= 128
        && error
            .code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
    if !code_valid {
        return Err(WorkbenchStoreError::OutputRejected.into());
    }
    Ok(WorkbenchErrorInfo {
        class: error.class,
        code: error.code.clone(),
        safe_message: "the isolated workbench reported a bounded failure".to_string(),
        retryable: error.retryable,
    })
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

pub trait WorkbenchAuthoritySource: Send + Sync {
    fn current_for_request(
        &self,
        request: &WorkbenchRequest,
    ) -> anyhow::Result<WorkbenchAuthoritySnapshot>;

    fn current_for_record(
        &self,
        record: &WorkbenchInvocationRecord,
    ) -> anyhow::Result<WorkbenchAuthoritySnapshot>;
}

#[cfg(test)]
impl WorkbenchAuthoritySource for WorkbenchAuthoritySnapshot {
    fn current_for_request(
        &self,
        _request: &WorkbenchRequest,
    ) -> anyhow::Result<WorkbenchAuthoritySnapshot> {
        Ok(self.clone())
    }

    fn current_for_record(
        &self,
        _record: &WorkbenchInvocationRecord,
    ) -> anyhow::Result<WorkbenchAuthoritySnapshot> {
        Ok(self.clone())
    }
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

pub fn authorize_workbench_record(
    record: &WorkbenchInvocationRecord,
    authority: &WorkbenchAuthoritySnapshot,
) -> anyhow::Result<BTreeSet<String>> {
    if !authority.assignment_active {
        bail!("workbench assignment is not active");
    }
    if record.agent_id != authority.agent_id
        || record.caller_id != authority.caller_id
        || record.caller_role != authority.caller_role
        || record.project_id != authority.project_id
        || record.work_item_id != authority.work_item_id
        || record.assignment_version != authority.assignment_version
        || record.credential_generation != authority.credential_generation
        || record.policy_digest != authority.policy_digest
        || record.tool_profile != authority.tool_profile
        || record.tool_profile_digest != authority.tool_profile_digest
        || record.runtime_key != authority.runtime_key
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
    if !record.capabilities.is_subset(&effective) {
        bail!("workbench capability was revoked before output acceptance");
    }
    Ok(effective)
}

pub struct WorkbenchRuntimeExchange<'a> {
    result: NanoExecResult,
    revalidate_world_authority: Box<dyn Fn() -> anyhow::Result<()> + 'a>,
}

impl<'a> WorkbenchRuntimeExchange<'a> {
    pub fn new(
        result: NanoExecResult,
        revalidate_world_authority: impl Fn() -> anyhow::Result<()> + 'a,
    ) -> Self {
        Self {
            result,
            revalidate_world_authority: Box::new(revalidate_world_authority),
        }
    }

    fn result(&self) -> &NanoExecResult {
        &self.result
    }

    fn revalidate(&self) -> anyhow::Result<()> {
        (self.revalidate_world_authority)()
    }
}

pub trait WorkbenchRuntimeClient {
    fn exchange(
        &mut self,
        agent_id: AgentId,
        request: NanoExecRequest,
    ) -> anyhow::Result<WorkbenchRuntimeExchange<'_>>;
}

pub enum WorkbenchDispatchCommand {
    Submit {
        request: Box<WorkbenchRequest>,
        authority: Arc<dyn WorkbenchAuthoritySource>,
        response: mpsc::SyncSender<anyhow::Result<WorkbenchCoordinatorUpdate>>,
    },
    Poll {
        invocation_id: String,
        authority: Arc<dyn WorkbenchAuthoritySource>,
        response: mpsc::SyncSender<anyhow::Result<WorkbenchCoordinatorUpdate>>,
    },
    Recover {
        invocation_id: String,
        authority: Arc<dyn WorkbenchAuthoritySource>,
        response: mpsc::SyncSender<anyhow::Result<WorkbenchCoordinatorUpdate>>,
    },
    Cancel {
        invocation_id: String,
        reason: String,
        authority: Arc<dyn WorkbenchAuthoritySource>,
        response: mpsc::SyncSender<anyhow::Result<WorkbenchCoordinatorUpdate>>,
    },
}

static WORKBENCH_DISPATCH: OnceLock<RwLock<Option<mpsc::SyncSender<WorkbenchDispatchCommand>>>> =
    OnceLock::new();
static WORKBENCH_SERVICE: OnceLock<Mutex<Option<WorkbenchService>>> = OnceLock::new();

pub(crate) struct WorkbenchService {
    pub(crate) store: WorkbenchInvocationStore,
    pub(crate) profile: WorkbenchProfile,
    pub(crate) profile_digest: String,
    pub(crate) receiver: mpsc::Receiver<WorkbenchDispatchCommand>,
}

pub(crate) fn install_workbench_service(
    data_dir: &Path,
    config_dir: &Path,
    artifact_roots: HashMap<AgentId, PathBuf>,
) -> anyhow::Result<()> {
    let service = WORKBENCH_SERVICE.get_or_init(|| Mutex::new(None));
    let mut service = service
        .lock()
        .map_err(|_| anyhow::anyhow!("workbench service lock was poisoned"))?;
    if service.is_some() {
        bail!("workbench service is already installed");
    }
    let (profile, profile_digest) =
        WorkbenchProfile::load(config_dir.join("workbench-profiles/web-authoring-v1.toml"))?;
    let store = WorkbenchInvocationStore::open_with_artifact_roots(
        data_dir.join("workbench.redb"),
        artifact_roots,
    )?;
    let (sender, receiver) = mpsc::sync_channel(128);
    install_workbench_dispatch(sender)?;
    *service = Some(WorkbenchService {
        store,
        profile,
        profile_digest,
        receiver,
    });
    Ok(())
}

pub(crate) fn take_workbench_service() -> anyhow::Result<Option<WorkbenchService>> {
    WORKBENCH_SERVICE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| anyhow::anyhow!("workbench service lock was poisoned"))
        .map(|mut service| service.take())
}

fn install_workbench_dispatch(
    sender: mpsc::SyncSender<WorkbenchDispatchCommand>,
) -> anyhow::Result<()> {
    let slot = WORKBENCH_DISPATCH.get_or_init(|| RwLock::new(None));
    let mut slot = slot
        .write()
        .map_err(|_| anyhow::anyhow!("workbench dispatch lock was poisoned"))?;
    if slot.is_some() {
        bail!("workbench dispatch is already installed");
    }
    *slot = Some(sender);
    Ok(())
}

pub fn dispatch_workbench(command: WorkbenchDispatchCommand) -> anyhow::Result<()> {
    let sender = WORKBENCH_DISPATCH
        .get()
        .ok_or_else(|| anyhow::anyhow!("workbench dispatch is not installed"))?
        .read()
        .map_err(|_| anyhow::anyhow!("workbench dispatch lock was poisoned"))?
        .clone()
        .ok_or_else(|| anyhow::anyhow!("workbench dispatch is not installed"))?;
    sender.try_send(command).map_err(|error| match error {
        mpsc::TrySendError::Full(_) => anyhow::anyhow!("workbench dispatch queue is full"),
        mpsc::TrySendError::Disconnected(_) => {
            anyhow::anyhow!("workbench dispatch is unavailable")
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkbenchCoordinatorUpdate {
    pub records: Vec<WorkbenchInvocationRecord>,
    pub runtime_state: Option<String>,
    pub replayed: bool,
}

pub struct WorkbenchCoordinator<'a> {
    store: &'a WorkbenchInvocationStore,
    profile: &'a WorkbenchProfile,
    profile_digest: &'a str,
}

impl<'a> WorkbenchCoordinator<'a> {
    pub fn new(
        store: &'a WorkbenchInvocationStore,
        profile: &'a WorkbenchProfile,
        profile_digest: &'a str,
    ) -> Self {
        Self {
            store,
            profile,
            profile_digest,
        }
    }

    pub fn submit(
        &self,
        runtime: &mut dyn WorkbenchRuntimeClient,
        request: &WorkbenchRequest,
        authority: &dyn WorkbenchAuthoritySource,
        now_ms: u64,
    ) -> anyhow::Result<WorkbenchCoordinatorUpdate> {
        self.profile
            .authorize_request(self.profile_digest, request)?;
        let current_authority = authority.current_for_request(request)?;
        authorize_workbench_request(request, &current_authority)?;
        let reservation = self.store.reserve(request, now_ms)?;
        let (record, replayed) = match reservation {
            ReservationOutcome::Reserved(record) => (record, false),
            ReservationOutcome::Replay(record) => (record, true),
        };
        if record.state.is_terminal() || record.state == WorkbenchInvocationState::Executing {
            return Ok(WorkbenchCoordinatorUpdate {
                records: vec![record],
                runtime_state: None,
                replayed: true,
            });
        }

        let mut records = if replayed { Vec::new() } else { vec![record] };
        let executing =
            self.store
                .mark_executing(&request.invocation_id, &request.input_digest, now_ms)?;
        records.push(executing);
        self.apply_exchange(
            runtime,
            request.agent_id,
            &request.invocation_id,
            &request.input_digest,
            WorkbenchRuntimeEnvelope::start(request)?,
            now_ms,
            records,
            replayed,
            authority,
        )
    }

    pub fn poll(
        &self,
        runtime: &mut dyn WorkbenchRuntimeClient,
        invocation_id: &str,
        authority: &dyn WorkbenchAuthoritySource,
        now_ms: u64,
    ) -> anyhow::Result<WorkbenchCoordinatorUpdate> {
        let record = self
            .store
            .load(invocation_id)?
            .ok_or(WorkbenchStoreError::NotReserved)?;
        let current_authority = authority.current_for_record(&record)?;
        authorize_workbench_record(&record, &current_authority)?;
        if record.state.is_terminal() {
            return Ok(WorkbenchCoordinatorUpdate {
                records: vec![record],
                runtime_state: None,
                replayed: true,
            });
        }
        if record.state != WorkbenchInvocationState::Executing {
            bail!("only an executing workbench invocation can be polled");
        }
        self.apply_exchange(
            runtime,
            record.agent_id,
            &record.invocation_id,
            &record.request_digest,
            WorkbenchRuntimeEnvelope::poll(&record.invocation_id)?,
            now_ms,
            Vec::new(),
            false,
            authority,
        )
    }

    pub fn recover_executing(
        &self,
        runtime: &mut dyn WorkbenchRuntimeClient,
        invocation_id: &str,
        authority: &dyn WorkbenchAuthoritySource,
        now_ms: u64,
    ) -> anyhow::Result<WorkbenchCoordinatorUpdate> {
        let record = self
            .store
            .load(invocation_id)?
            .ok_or(WorkbenchStoreError::NotReserved)?;
        let current_authority = authority.current_for_record(&record)?;
        authorize_workbench_record(&record, &current_authority)?;
        if record.state.is_terminal() {
            return Ok(WorkbenchCoordinatorUpdate {
                records: vec![record],
                runtime_state: None,
                replayed: true,
            });
        }
        if record.state != WorkbenchInvocationState::Executing {
            bail!("reserved workbench invocations require an authorized request replay");
        }
        self.apply_exchange(
            runtime,
            record.agent_id,
            &record.invocation_id,
            &record.request_digest,
            WorkbenchRuntimeEnvelope::recover(&record.invocation_id, &record.request_digest)?,
            now_ms,
            Vec::new(),
            false,
            authority,
        )
    }

    pub fn cancel(
        &self,
        runtime: &mut dyn WorkbenchRuntimeClient,
        invocation_id: &str,
        _reason: &str,
        authority: &dyn WorkbenchAuthoritySource,
        now_ms: u64,
    ) -> anyhow::Result<WorkbenchCoordinatorUpdate> {
        let record = self
            .store
            .load(invocation_id)?
            .ok_or(WorkbenchStoreError::NotReserved)?;
        let current_authority = authority.current_for_record(&record)?;
        authorize_workbench_record(&record, &current_authority)?;
        if record.state.is_terminal() {
            return Ok(WorkbenchCoordinatorUpdate {
                records: vec![record],
                runtime_state: None,
                replayed: true,
            });
        }
        if record.state == WorkbenchInvocationState::Reserved {
            let cancelled =
                self.store
                    .mark_cancelled(&record.invocation_id, &record.request_digest, now_ms)?;
            return Ok(WorkbenchCoordinatorUpdate {
                records: vec![cancelled],
                runtime_state: Some("completed".to_string()),
                replayed: false,
            });
        }
        self.apply_cancel_exchange(runtime, &record, authority, now_ms)
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_exchange(
        &self,
        runtime: &mut dyn WorkbenchRuntimeClient,
        agent_id: AgentId,
        invocation_id: &str,
        request_digest: &str,
        request: NanoExecRequest,
        now_ms: u64,
        mut records: Vec<WorkbenchInvocationRecord>,
        replayed: bool,
        authority: &dyn WorkbenchAuthoritySource,
    ) -> anyhow::Result<WorkbenchCoordinatorUpdate> {
        let current = self
            .store
            .load(invocation_id)?
            .ok_or(WorkbenchStoreError::NotReserved)?;
        let current_authority = authority.current_for_record(&current)?;
        authorize_workbench_record(&current, &current_authority)?;
        // A transport failure is not evidence that the isolated workload did
        // not complete. Keep the durable invocation executing so recovery can
        // probe the runtime receipt without dispatching the external effect a
        // second time.
        let exchange = runtime.exchange(agent_id, request)?;
        exchange.revalidate()?;
        let current = self
            .store
            .load(invocation_id)?
            .ok_or(WorkbenchStoreError::NotReserved)?;
        let current_authority = authority.current_for_record(&current)?;
        authorize_workbench_record(&current, &current_authority)?;
        let expected_workload_id = format!("AGENT-{:02}", agent_id.0);
        let envelope =
            match WorkbenchRuntimeEnvelope::decode(
                invocation_id,
                &expected_workload_id,
                exchange.result(),
            ) {
                Ok(envelope) => envelope,
                Err(_) => {
                    exchange.revalidate()?;
                    let unresolved = self.store.mark_unknown_outcome_guarded(
                        invocation_id,
                        request_digest,
                        now_ms,
                        safe_recovery_failure("runtime_response_rejected"),
                        &|| exchange.revalidate(),
                    )?;
                    records.push(unresolved);
                    return Ok(WorkbenchCoordinatorUpdate {
                        records,
                        runtime_state: Some("completed".to_string()),
                        replayed,
                    });
                }
        };
        let runtime_state = Some(envelope.state.clone());
        exchange.revalidate()?;
        if let Some(terminal) = self.store.accept_runtime_envelope_guarded(
            &envelope,
            now_ms,
            &|| exchange.revalidate(),
        )? {
            records.push(terminal);
        }
        Ok(WorkbenchCoordinatorUpdate {
            records,
            runtime_state,
            replayed,
        })
    }

    fn apply_cancel_exchange(
        &self,
        runtime: &mut dyn WorkbenchRuntimeClient,
        record: &WorkbenchInvocationRecord,
        authority: &dyn WorkbenchAuthoritySource,
        now_ms: u64,
    ) -> anyhow::Result<WorkbenchCoordinatorUpdate> {
        let current_authority = authority.current_for_record(record)?;
        authorize_workbench_record(record, &current_authority)?;
        let exchange = runtime.exchange(
            record.agent_id,
            WorkbenchRuntimeEnvelope::cancel(&record.invocation_id)?,
        )?;
        exchange.revalidate()?;
        let expected_workload_id = format!("AGENT-{:02}", record.agent_id.0);
        let envelope = WorkbenchRuntimeEnvelope::decode(
            &record.invocation_id,
            &expected_workload_id,
            exchange.result(),
        )?;
        let current = self
            .store
            .load(&record.invocation_id)?
            .ok_or(WorkbenchStoreError::NotReserved)?;
        let current_authority = authority.current_for_record(&current)?;
        authorize_workbench_record(&current, &current_authority)?;
        let mut records = Vec::new();
        exchange.revalidate()?;
        if let Some(terminal) = self.store.accept_runtime_envelope_guarded(
            &envelope,
            now_ms,
            &|| exchange.revalidate(),
        )? {
            records.push(terminal);
        }
        Ok(WorkbenchCoordinatorUpdate {
            records,
            runtime_state: Some(envelope.state),
            replayed: false,
        })
    }
}

fn safe_recovery_failure(code: &str) -> WorkbenchErrorInfo {
    WorkbenchErrorInfo {
        class: sentinel_common::WorkbenchErrorClass::Recovery,
        code: code.to_string(),
        safe_message: "the selected workbench runtime failed closed".to_string(),
        retryable: false,
    }
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
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    use std::sync::Mutex;

    use sentinel_common::{
        WorkbenchErrorClass, WorkbenchProgressStage, WorkbenchResourceLimits, WorkbenchTool,
        WORKBENCH_RUNTIME_BWRAP,
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

    fn authority(
        request: &WorkbenchRequest,
        profile: &WorkbenchProfile,
    ) -> WorkbenchAuthoritySnapshot {
        let granted = profile.capabilities.clone();
        WorkbenchAuthoritySnapshot {
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
            profile_capabilities: granted,
        }
    }

    struct FakeRuntime {
        calls: usize,
        responses: VecDeque<anyhow::Result<NanoExecResult>>,
    }

    impl WorkbenchRuntimeClient for FakeRuntime {
        fn exchange(
            &mut self,
            _agent_id: AgentId,
            _request: NanoExecRequest,
        ) -> anyhow::Result<WorkbenchRuntimeExchange<'_>> {
            self.calls += 1;
            let result = self
                .responses
                .pop_front()
                .unwrap_or_else(|| Err(anyhow::anyhow!("unexpected runtime exchange")))?;
            Ok(WorkbenchRuntimeExchange::new(result, || Ok(())))
        }
    }

    struct SequencedAuthority {
        snapshots: Mutex<VecDeque<WorkbenchAuthoritySnapshot>>,
    }

    struct GuardedRuntime {
        calls: Arc<std::sync::atomic::AtomicUsize>,
        validations: Arc<std::sync::atomic::AtomicUsize>,
        operations: Arc<Mutex<Vec<String>>>,
        fail_validation: usize,
        response: NanoExecResult,
    }

    impl WorkbenchRuntimeClient for GuardedRuntime {
        fn exchange(
            &mut self,
            _agent_id: AgentId,
            request: NanoExecRequest,
        ) -> anyhow::Result<WorkbenchRuntimeExchange<'_>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.operations.lock().unwrap().push(request.operation);
            let validations = Arc::clone(&self.validations);
            let fail_validation = self.fail_validation;
            Ok(WorkbenchRuntimeExchange::new(
                self.response.clone(),
                move || {
                    let current = validations.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    if current == fail_validation {
                        bail!("workbench World authority became stale");
                    }
                    Ok(())
                },
            ))
        }
    }

    impl WorkbenchAuthoritySource for SequencedAuthority {
        fn current_for_request(
            &self,
            _request: &WorkbenchRequest,
        ) -> anyhow::Result<WorkbenchAuthoritySnapshot> {
            self.next()
        }

        fn current_for_record(
            &self,
            _record: &WorkbenchInvocationRecord,
        ) -> anyhow::Result<WorkbenchAuthoritySnapshot> {
            self.next()
        }
    }

    impl SequencedAuthority {
        fn next(&self) -> anyhow::Result<WorkbenchAuthoritySnapshot> {
            self.snapshots
                .lock()
                .map_err(|_| anyhow::anyhow!("test authority lock poisoned"))?
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("test authority sequence exhausted"))
        }
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
        assert!(!store.has_inflight().unwrap());
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
        assert_eq!(
            items[0].action,
            WorkbenchRecoveryAction::AwaitAuthorizedReplay
        );
        assert_eq!(items[1].action, WorkbenchRecoveryAction::ProbeExecuting);
        assert!(store.has_inflight().unwrap());
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
    fn profile_authority_rejects_symlink_hardlink_mode_and_identity_replacement() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let directory = tempfile::tempdir().unwrap();
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/workbench-profiles/web-authoring-v1.toml");
        let safe = directory.path().join("profile.toml");
        fs::copy(&source, &safe).unwrap();
        fs::set_permissions(&safe, fs::Permissions::from_mode(0o600)).unwrap();
        WorkbenchProfile::load(&safe).unwrap();

        let symlink_path = directory.path().join("profile-link.toml");
        symlink(&safe, &symlink_path).unwrap();
        assert!(WorkbenchProfile::load(&symlink_path).is_err());

        let hardlink_path = directory.path().join("profile-hardlink.toml");
        fs::hard_link(&safe, &hardlink_path).unwrap();
        assert!(WorkbenchProfile::load(&safe).is_err());
        fs::remove_file(&hardlink_path).unwrap();

        fs::set_permissions(&safe, fs::Permissions::from_mode(0o622)).unwrap();
        assert!(WorkbenchProfile::load(&safe).is_err());
        fs::set_permissions(&safe, fs::Permissions::from_mode(0o600)).unwrap();

        let metadata = fs::symlink_metadata(&safe).unwrap();
        assert!(!authority_file_metadata_is_safe(
            &metadata,
            current_daemon_uid().unwrap().wrapping_add(1),
            MAX_PROFILE_BYTES,
            None,
        ));

        let identity = FileIdentity::from_metadata(&metadata);
        let replacement = directory.path().join("replacement.toml");
        fs::copy(&source, &replacement).unwrap();
        fs::rename(&replacement, &safe).unwrap();
        assert!(ensure_path_identity(&safe, identity).is_err());
    }

    #[test]
    fn invocation_store_is_mode_bound_and_reopens_the_same_safe_file() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("workbench.redb");
        drop(WorkbenchInvocationStore::open(&path).unwrap());
        let metadata = fs::symlink_metadata(&path).unwrap();
        assert_eq!(metadata.mode() & 0o777, 0o600);
        assert_eq!(metadata.nlink(), 1);
        drop(WorkbenchInvocationStore::open(&path).unwrap());

        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(WorkbenchInvocationStore::open(&path).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let linked = directory.path().join("workbench-hardlink.redb");
        fs::hard_link(&path, &linked).unwrap();
        assert!(WorkbenchInvocationStore::open(&path).is_err());
        fs::remove_file(linked).unwrap();

        let target = directory.path().join("target.redb");
        fs::rename(&path, &target).unwrap();
        symlink(&target, &path).unwrap();
        assert!(WorkbenchInvocationStore::open(&path).is_err());
    }

    #[test]
    fn invocation_store_rejects_identity_replacement_between_inspect_and_open() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("workbench.redb");
        let replacement = directory.path().join("replacement.redb");
        fs::write(&path, b"original").unwrap();
        fs::write(&replacement, b"replacement").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(&replacement, fs::Permissions::from_mode(0o600)).unwrap();

        assert!(open_secure_store_file_at(directory.path(), &path, |opened_path| {
            fs::rename(&replacement, &path)?;
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(opened_path)
        })
        .is_err());
    }

    #[test]
    fn poll_frame_matches_the_exact_v1_registry_contract() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b280a";
        let poll = WorkbenchRuntimeEnvelope::poll(invocation_id).unwrap();
        assert_eq!(poll.operation, "workbench_poll");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&poll.input).unwrap(),
            serde_json::json!({
                "kind": "poll",
                "schema_version": WORKBENCH_SCHEMA_VERSION,
                "invocation_id": invocation_id,
            })
        );
        let cancel = WorkbenchRuntimeEnvelope::cancel(invocation_id).unwrap();
        assert_eq!(cancel.operation, "workbench_cancel");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&cancel.input).unwrap(),
            serde_json::json!({
                "kind": "cancel",
                "schema_version": WORKBENCH_SCHEMA_VERSION,
                "invocation_id": invocation_id,
                "reason": "explicit_cancel",
            })
        );
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
        let artifact_root = directory.path().join("artifacts");
        let mut request = request("018f3f32-4f01-7f2c-a6c1-f6f4a81b2807");
        request.tool = WorkbenchTool::PackageArtifact {
            artifact_kind: "source_tree".to_string(),
            media_type: "application/json".to_string(),
            paths: vec!["src".to_string()],
        };
        request.capabilities = BTreeSet::from(["artifact.commit".to_string()]);
        request.output_artifact_kinds = BTreeSet::from(["source_tree".to_string()]);
        request.input_digest = request.canonical_digest().unwrap();
        fs::write(directory.path().join(".nano-runtime"), "AGENT-07").unwrap();
        let store = WorkbenchInvocationStore::open_with_artifact_roots(
            directory.path().join("workbench.redb"),
            HashMap::from([(request.agent_id, artifact_root.clone())]),
        )
        .unwrap();
        store.reserve(&request, 1_900_000_000_000).unwrap();
        store
            .mark_executing(
                &request.invocation_id,
                &request.input_digest,
                1_900_000_000_001,
            )
            .unwrap();
        let scoped = artifact_root
            .join(&request.project_id)
            .join(&request.work_item_id);
        fs::create_dir_all(scoped.join("blobs")).unwrap();
        let blob = b"bound artifact";
        let blob_digest = hex_sha256(blob);
        fs::write(scoped.join("blobs").join(&blob_digest), blob).unwrap();
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schema_version": WORKBENCH_SCHEMA_VERSION,
            "invocation_id": request.invocation_id.clone(),
            "input_digest": request.input_digest.clone(),
            "project_id": request.project_id.clone(),
            "work_item_id": request.work_item_id.clone(),
            "workspace_id": request.workspace_id.clone(),
            "agent_id": request.agent_id.0,
            "artifact_kind": "source_tree",
            "media_type": "application/json",
            "runtime_key": request.runtime_key.clone(),
            "tool_profile": request.tool_profile.clone(),
            "tool_profile_digest": request.tool_profile_digest.clone(),
            "policy_digest": request.policy_digest.clone(),
            "entries": [{
                "path": "src/index.html",
                "blob_id": format!("sha256:{blob_digest}"),
                "sha256": blob_digest.clone(),
                "size_bytes": blob.len(),
            }],
        }))
        .unwrap();
        let manifest_digest = hex_sha256(&manifest);
        let manifest_name = format!("{manifest_digest}.manifest.json");
        fs::write(scoped.join(&manifest_name), manifest).unwrap();
        let artifact = WorkbenchArtifactRef {
            artifact_id: format!("sha256:{manifest_digest}"),
            sha256: manifest_digest,
            artifact_kind: "source_tree".to_string(),
            media_type: "application/json".to_string(),
            size_bytes: blob.len() as u64,
            manifest_path: manifest_name,
        };
        let result = WorkbenchMessage::Result {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: request.invocation_id.clone(),
            input_digest: request.input_digest.clone(),
            outcome: WorkbenchOutcome::Succeeded,
            resources: WorkbenchResourceUsage {
                duration_ms: 7,
                artifact_bytes: blob.len() as u64,
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

    #[test]
    fn concrete_manifest_rejects_missing_tampered_and_foreign_authority() {
        let directory = tempfile::tempdir().unwrap();
        let artifact_root = directory.path().join("artifacts");
        let mut request = request("018f3f32-4f01-7f2c-a6c1-f6f4a81b2899");
        request.tool = WorkbenchTool::PackageArtifact {
            artifact_kind: "source_tree".to_string(),
            media_type: "application/json".to_string(),
            paths: vec!["src".to_string()],
        };
        request.capabilities = BTreeSet::from(["artifact.commit".to_string()]);
        request.output_artifact_kinds = BTreeSet::from(["source_tree".to_string()]);
        request.input_digest = request.canonical_digest().unwrap();
        let record = WorkbenchInvocationRecord::reserved(&request, 1_900_000_000_000);
        fs::write(directory.path().join(".nano-runtime"), "AGENT-07").unwrap();
        let roots = HashMap::from([(request.agent_id, artifact_root.clone())]);
        let scoped = artifact_root
            .join(&request.project_id)
            .join(&request.work_item_id);
        fs::create_dir_all(scoped.join("blobs")).unwrap();
        let blob = b"verified";
        let blob_digest = hex_sha256(blob);
        fs::write(scoped.join("blobs").join(&blob_digest), blob).unwrap();
        let manifest_bytes = |project_id: &str| {
            serde_json::to_vec(&serde_json::json!({
                "schema_version": WORKBENCH_SCHEMA_VERSION,
                "invocation_id": request.invocation_id.clone(),
                "input_digest": request.input_digest.clone(),
                "project_id": project_id,
                "work_item_id": request.work_item_id.clone(),
                "workspace_id": request.workspace_id.clone(),
                "agent_id": request.agent_id.0,
                "artifact_kind": "source_tree",
                "media_type": "application/json",
                "runtime_key": request.runtime_key.clone(),
                "tool_profile": request.tool_profile.clone(),
                "tool_profile_digest": request.tool_profile_digest.clone(),
                "policy_digest": request.policy_digest.clone(),
                "entries": [{
                    "path": "src/index.html",
                    "blob_id": format!("sha256:{blob_digest}"),
                    "sha256": blob_digest.clone(),
                    "size_bytes": blob.len(),
                }],
            }))
            .unwrap()
        };
        let valid = manifest_bytes(&request.project_id);
        let digest = hex_sha256(&valid);
        let mut artifact = WorkbenchArtifactRef {
            artifact_id: format!("sha256:{digest}"),
            sha256: digest.clone(),
            artifact_kind: "source_tree".to_string(),
            media_type: "application/json".to_string(),
            size_bytes: blob.len() as u64,
            manifest_path: format!("{digest}.manifest.json"),
        };
        assert!(validate_concrete_artifact_manifest(&record, &artifact, &roots).is_err());

        fs::write(scoped.join(&artifact.manifest_path), &valid).unwrap();
        assert!(validate_concrete_artifact_manifest(&record, &artifact, &roots).is_ok());
        fs::write(scoped.join(&artifact.manifest_path), b"tampered").unwrap();
        assert!(validate_concrete_artifact_manifest(&record, &artifact, &roots).is_err());

        let foreign = manifest_bytes("foreign-project");
        let foreign_digest = hex_sha256(&foreign);
        artifact.artifact_id = format!("sha256:{foreign_digest}");
        artifact.sha256 = foreign_digest.clone();
        artifact.manifest_path = format!("{foreign_digest}.manifest.json");
        fs::write(scoped.join(&artifact.manifest_path), foreign).unwrap();
        assert!(validate_concrete_artifact_manifest(&record, &artifact, &roots).is_err());
    }

    #[test]
    fn daemon_artifact_scope_rejects_symlink_components_and_foreign_agent_base() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let request = request("018f3f32-4f01-7f2c-a6c1-f6f4a81b2898");
        let agent_root = directory.path().join("agent-a");
        let artifact_root = agent_root.join("artifacts");
        fs::create_dir_all(&artifact_root).unwrap();
        fs::write(agent_root.join(".nano-runtime"), "AGENT-07").unwrap();

        let foreign_scope = directory.path().join("foreign-scope");
        fs::create_dir_all(foreign_scope.join(&request.work_item_id)).unwrap();
        symlink(&foreign_scope, artifact_root.join(&request.project_id)).unwrap();
        assert!(bind_daemon_artifact_scope(
            &artifact_root,
            request.agent_id,
            &request.project_id,
            &request.work_item_id,
        )
        .is_err());
        fs::remove_file(artifact_root.join(&request.project_id)).unwrap();

        let project = artifact_root.join(&request.project_id);
        fs::create_dir(&project).unwrap();
        symlink(
            foreign_scope.join(&request.work_item_id),
            project.join(&request.work_item_id),
        )
        .unwrap();
        assert!(bind_daemon_artifact_scope(
            &artifact_root,
            request.agent_id,
            &request.project_id,
            &request.work_item_id,
        )
        .is_err());

        let foreign_agent_root = directory.path().join("agent-b");
        let foreign_artifacts = foreign_agent_root.join("artifacts");
        fs::create_dir_all(
            foreign_artifacts
                .join(&request.project_id)
                .join(&request.work_item_id),
        )
        .unwrap();
        fs::write(foreign_agent_root.join(".nano-runtime"), "AGENT-08").unwrap();
        assert!(bind_daemon_artifact_scope(
            &foreign_artifacts,
            request.agent_id,
            &request.project_id,
            &request.work_item_id,
        )
        .is_err());

        let safe_scope = directory.path().join("safe-files");
        fs::create_dir(&safe_scope).unwrap();
        let manifest = safe_scope.join("manifest.json");
        fs::write(&manifest, b"{}").unwrap();
        let pinned = open_pinned_daemon_artifact_directory(&safe_scope).unwrap();
        open_scoped_daemon_artifact_file(&pinned, "manifest.json", 16, None).unwrap();

        let hardlink = safe_scope.join("manifest-hardlink.json");
        fs::hard_link(&manifest, &hardlink).unwrap();
        assert!(open_scoped_daemon_artifact_file(&pinned, "manifest.json", 16, None).is_err());
        fs::remove_file(hardlink).unwrap();

        let replacement = safe_scope.join("replacement.json");
        fs::write(&replacement, b"[]").unwrap();
        assert!(open_scoped_daemon_artifact_file_with(
            &pinned,
            "manifest.json",
            16,
            None,
            |path| {
                fs::rename(&replacement, &manifest)?;
                OpenOptions::new()
                    .read(true)
                    .custom_flags(LINUX_O_NOFOLLOW | LINUX_O_CLOEXEC)
                    .open(path)
            },
        )
        .is_err());
    }

    #[test]
    fn runtime_envelope_is_bound_committed_and_recoverable_without_private_output() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        let request = request("018f3f32-4f01-7f2c-a6c1-f6f4a81b2810");
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
            resources: WorkbenchResourceUsage {
                duration_ms: 12,
                ..WorkbenchResourceUsage::default()
            },
            artifacts: Vec::new(),
            output: BTreeMap::from([("content".to_string(), "PRIVATE".to_string())]),
            error: None,
        };
        let wire = NanoExecResult {
            runtime_key: WORKBENCH_RUNTIME_BWRAP.to_string(),
            workload_id: "AGENT-07".to_string(),
            success: true,
            output: serde_json::to_string(&serde_json::json!({
                "schema_version": WORKBENCH_SCHEMA_VERSION,
                "invocation_id": request.invocation_id,
                "state": "pending",
                "messages": [result],
            }))
            .unwrap(),
        };
        let mut wrong_workload = wire.clone();
        wrong_workload.workload_id = "AGENT-08".to_string();
        assert!(WorkbenchRuntimeEnvelope::decode(
            &request.invocation_id,
            "AGENT-07",
            &wrong_workload,
        )
        .is_err());
        let envelope =
            WorkbenchRuntimeEnvelope::decode(&request.invocation_id, "AGENT-07", &wire).unwrap();
        assert!(store
            .accept_runtime_envelope(&envelope, 1_900_000_000_012)
            .unwrap()
            .is_none());
        assert_eq!(
            store.load(&request.invocation_id).unwrap().unwrap().state,
            WorkbenchInvocationState::Executing,
            "a result is provisional until the adapter confirms completed cleanup"
        );

        let WorkbenchRuntimeEnvelope {
            messages: mut completed_messages,
            ..
        } = envelope;
        completed_messages.push(WorkbenchMessage::Progress {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: request.invocation_id.clone(),
            stage: WorkbenchProgressStage::Completed,
            elapsed_ms: 13,
        });

        let completion = WorkbenchRuntimeEnvelope {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: request.invocation_id.clone(),
            state: "completed".to_string(),
            messages: completed_messages,
        };
        let completed = store
            .accept_runtime_envelope(&completion, 1_900_000_000_013)
            .unwrap()
            .unwrap();
        assert_eq!(completed.state, WorkbenchInvocationState::Succeeded);
        assert!(!serde_json::to_string(&completed)
            .unwrap()
            .contains("PRIVATE"));
        let recovery =
            WorkbenchRuntimeEnvelope::recover(&request.invocation_id, &request.input_digest)
                .unwrap();
        assert_eq!(recovery.operation, "workbench_recover");
        assert!(recovery.input.contains(&request.input_digest));
    }

    #[test]
    fn foreign_or_duplicate_runtime_messages_cannot_mutate_the_record() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        let request = request("018f3f32-4f01-7f2c-a6c1-f6f4a81b2811");
        store.reserve(&request, 1_900_000_000_000).unwrap();
        store
            .mark_executing(
                &request.invocation_id,
                &request.input_digest,
                1_900_000_000_001,
            )
            .unwrap();
        let foreign = WorkbenchRuntimeEnvelope {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: request.invocation_id.clone(),
            state: "pending".to_string(),
            messages: vec![WorkbenchMessage::Progress {
                schema_version: WORKBENCH_SCHEMA_VERSION,
                invocation_id: "018f3f32-4f01-7f2c-a6c1-f6f4a81b2999".to_string(),
                stage: WorkbenchProgressStage::Executing,
                elapsed_ms: 1,
            }],
        };
        assert!(store.accept_runtime_envelope(&foreign, 2).is_err());
        assert_eq!(
            store.load(&request.invocation_id).unwrap().unwrap().state,
            WorkbenchInvocationState::Executing
        );

        let error = WorkbenchMessage::Error {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: Some(request.invocation_id.clone()),
            error: WorkbenchErrorInfo {
                class: WorkbenchErrorClass::Protocol,
                code: "protocol_failed".to_string(),
                safe_message: "failed".to_string(),
                retryable: false,
            },
        };
        let duplicate = WorkbenchRuntimeEnvelope {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: request.invocation_id.clone(),
            state: "completed".to_string(),
            messages: vec![error.clone(), error],
        };
        assert!(store.accept_runtime_envelope(&duplicate, 3).is_err());
        assert_eq!(
            store.load(&request.invocation_id).unwrap().unwrap().state,
            WorkbenchInvocationState::Executing
        );
    }

    #[test]
    fn acknowledged_cancel_is_durable_replayable_and_error_text_is_redacted() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        let request = request("018f3f32-4f01-7f2c-a6c1-f6f4a81b2816");
        store.reserve(&request, 1_900_000_000_000).unwrap();
        store
            .mark_executing(
                &request.invocation_id,
                &request.input_digest,
                1_900_000_000_001,
            )
            .unwrap();
        let cancelled = WorkbenchRuntimeEnvelope {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: request.invocation_id.clone(),
            state: "completed".to_string(),
            messages: vec![WorkbenchMessage::Cancelled {
                schema_version: WORKBENCH_SCHEMA_VERSION,
                invocation_id: request.invocation_id.clone(),
            }],
        };

        for now_ms in [1_900_000_000_002, 1_900_000_000_003] {
            let record = store
                .accept_runtime_envelope(&cancelled, now_ms)
                .unwrap()
                .unwrap();
            assert_eq!(record.state, WorkbenchInvocationState::Cancelled);
            assert_eq!(record.error.as_ref().unwrap().code, "runtime_cancelled");
            assert!(!serde_json::to_string(&record).unwrap().contains("PRIVATE"));
        }
    }

    #[test]
    fn missing_receipt_is_an_unknown_outcome_without_untrusted_error_text() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        let request = request("018f3f32-4f01-7f2c-a6c1-f6f4a81b2817");
        store.reserve(&request, 1_900_000_000_000).unwrap();
        store
            .mark_executing(
                &request.invocation_id,
                &request.input_digest,
                1_900_000_000_001,
            )
            .unwrap();
        let failed = WorkbenchRuntimeEnvelope {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: request.invocation_id.clone(),
            state: "completed".to_string(),
            messages: vec![WorkbenchMessage::Error {
                schema_version: WORKBENCH_SCHEMA_VERSION,
                invocation_id: Some(request.invocation_id.clone()),
                error: WorkbenchErrorInfo {
                    class: WorkbenchErrorClass::Recovery,
                    code: "completion_receipt_not_found".to_string(),
                    safe_message: "PRIVATE-RUNTIME-PAYLOAD".to_string(),
                    retryable: false,
                },
            }],
        };

        let record = store
            .accept_runtime_envelope(&failed, 1_900_000_000_002)
            .unwrap()
            .unwrap();
        let durable = serde_json::to_string(&record).unwrap();
        assert_eq!(record.state, WorkbenchInvocationState::UnknownOutcome);
        assert_eq!(
            record.error.as_ref().unwrap().code,
            "completion_receipt_not_found"
        );
        assert!(!durable.contains("PRIVATE-RUNTIME-PAYLOAD"));
        assert!(durable.contains("bounded failure"));
    }

    #[test]
    fn coordinator_replays_without_duplicate_dispatch_and_recovers_a_receipt() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        let profile_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/workbench-profiles/web-authoring-v1.toml");
        let (profile, profile_digest) = WorkbenchProfile::load(profile_path).unwrap();
        let mut request = request("018f3f32-4f01-7f2c-a6c1-f6f4a81b2812");
        request.tool_profile_digest = profile_digest.clone();
        request.input_digest = request.canonical_digest().unwrap();
        let authority = authority(&request, &profile);
        let accepted = NanoExecResult {
            runtime_key: WORKBENCH_RUNTIME_BWRAP.to_string(),
            workload_id: "AGENT-07".to_string(),
            success: true,
            output: serde_json::to_string(&serde_json::json!({
                "schema_version": WORKBENCH_SCHEMA_VERSION,
                "invocation_id": request.invocation_id,
                "state": "accepted",
                "messages": [],
            }))
            .unwrap(),
        };
        let recovered_result = WorkbenchMessage::Result {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: request.invocation_id.clone(),
            input_digest: request.input_digest.clone(),
            outcome: WorkbenchOutcome::Succeeded,
            resources: WorkbenchResourceUsage {
                bytes_written: 2,
                ..WorkbenchResourceUsage::default()
            },
            artifacts: Vec::new(),
            output: BTreeMap::new(),
            error: None,
        };
        let recovered = NanoExecResult {
            runtime_key: WORKBENCH_RUNTIME_BWRAP.to_string(),
            workload_id: "AGENT-07".to_string(),
            success: true,
            output: serde_json::to_string(&serde_json::json!({
                "schema_version": WORKBENCH_SCHEMA_VERSION,
                "invocation_id": request.invocation_id,
                "state": "completed",
                "messages": [
                    recovered_result,
                    WorkbenchMessage::Progress {
                        schema_version: WORKBENCH_SCHEMA_VERSION,
                        invocation_id: request.invocation_id.clone(),
                        stage: WorkbenchProgressStage::Completed,
                        elapsed_ms: 2,
                    }
                ],
            }))
            .unwrap(),
        };
        let mut runtime = FakeRuntime {
            calls: 0,
            responses: VecDeque::from([Ok(accepted), Ok(recovered)]),
        };
        let coordinator = WorkbenchCoordinator::new(&store, &profile, &profile_digest);

        let started = coordinator
            .submit(&mut runtime, &request, &authority, 1_900_000_000_000)
            .unwrap();
        assert_eq!(runtime.calls, 1);
        assert_eq!(started.records.len(), 2);
        assert_eq!(
            started.records.last().unwrap().state,
            WorkbenchInvocationState::Executing
        );
        let replay = coordinator
            .submit(&mut runtime, &request, &authority, 1_900_000_000_001)
            .unwrap();
        assert!(replay.replayed);
        assert_eq!(runtime.calls, 1, "executing replay must not redispatch");

        let completed = coordinator
            .recover_executing(
                &mut runtime,
                &request.invocation_id,
                &authority,
                1_900_000_000_002,
            )
            .unwrap();
        assert_eq!(runtime.calls, 2);
        assert_eq!(completed.runtime_state.as_deref(), Some("completed"));
        assert_eq!(
            completed.records.last().unwrap().state,
            WorkbenchInvocationState::Succeeded
        );
    }

    #[test]
    fn output_acceptance_rechecks_current_authority_and_events_are_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        let profile_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/workbench-profiles/web-authoring-v1.toml");
        let (profile, profile_digest) = WorkbenchProfile::load(profile_path).unwrap();
        let mut request = request("018f3f32-4f01-7f2c-a6c1-f6f4a81b2813");
        request.tool_profile_digest = profile_digest.clone();
        request.input_digest = request.canonical_digest().unwrap();
        let mut authority = authority(&request, &profile);
        let accepted = NanoExecResult {
            runtime_key: WORKBENCH_RUNTIME_BWRAP.to_string(),
            workload_id: "AGENT-07".to_string(),
            success: true,
            output: serde_json::to_string(&serde_json::json!({
                "schema_version": WORKBENCH_SCHEMA_VERSION,
                "invocation_id": request.invocation_id,
                "state": "accepted",
                "messages": [],
            }))
            .unwrap(),
        };
        let mut runtime = FakeRuntime {
            calls: 0,
            responses: VecDeque::from([Ok(accepted)]),
        };
        let coordinator = WorkbenchCoordinator::new(&store, &profile, &profile_digest);
        let started = coordinator
            .submit(&mut runtime, &request, &authority, 1_900_000_000_000)
            .unwrap();

        authority.assignment_active = false;
        assert!(coordinator
            .poll(
                &mut runtime,
                &request.invocation_id,
                &authority,
                1_900_000_000_001,
            )
            .unwrap_err()
            .to_string()
            .contains("assignment is not active"));
        assert_eq!(
            runtime.calls, 1,
            "revocation must reject before runtime I/O"
        );

        let first = started.records[0].safe_event(42).unwrap();
        let replay = started.records[0].safe_event(42).unwrap();
        assert_eq!(first.operation_id, replay.operation_id);
        assert_eq!(first.payload, replay.payload);
        assert!(!first.payload.contains("PRIVATE"));
        let executing = started.records[1].safe_event(42).unwrap();
        assert_ne!(first.operation_id, executing.operation_id);
    }

    #[test]
    fn authority_revoked_during_adapter_io_rejects_output_and_retains_fence() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        let profile_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/workbench-profiles/web-authoring-v1.toml");
        let (profile, profile_digest) = WorkbenchProfile::load(profile_path).unwrap();
        let mut request = request("018f3f32-4f01-7f2c-a6c1-f6f4a81b2816");
        request.tool_profile_digest = profile_digest.clone();
        request.input_digest = request.canonical_digest().unwrap();
        let active = authority(&request, &profile);
        let mut revoked = active.clone();
        revoked.assignment_active = false;
        let authority = SequencedAuthority {
            snapshots: Mutex::new(VecDeque::from([active.clone(), active, revoked])),
        };
        let accepted = NanoExecResult {
            runtime_key: WORKBENCH_RUNTIME_BWRAP.to_string(),
            workload_id: "AGENT-07".to_string(),
            success: true,
            output: serde_json::to_string(&serde_json::json!({
                "schema_version": WORKBENCH_SCHEMA_VERSION,
                "invocation_id": request.invocation_id,
                "state": "accepted",
                "messages": [],
            }))
            .unwrap(),
        };
        let mut runtime = FakeRuntime {
            calls: 0,
            responses: VecDeque::from([Ok(accepted)]),
        };
        let coordinator = WorkbenchCoordinator::new(&store, &profile, &profile_digest);

        let error = coordinator
            .submit(&mut runtime, &request, &authority, 1_900_000_000_000)
            .unwrap_err();
        assert!(error.to_string().contains("assignment is not active"));
        assert_eq!(runtime.calls, 1);
        assert_eq!(
            store.load(&request.invocation_id).unwrap().unwrap().state,
            WorkbenchInvocationState::Executing
        );
        assert!(store.has_inflight().unwrap());
    }

    #[test]
    fn stale_world_guard_during_store_adoption_retains_recoverable_state_without_event() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        let profile_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/workbench-profiles/web-authoring-v1.toml");
        let (profile, profile_digest) = WorkbenchProfile::load(profile_path).unwrap();
        let mut request = request("018f3f32-4f01-7f2c-a6c1-f6f4a81b2817");
        request.tool_profile_digest = profile_digest.clone();
        request.input_digest = request.canonical_digest().unwrap();
        let authority = authority(&request, &profile);
        let result = WorkbenchMessage::Result {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: request.invocation_id.clone(),
            input_digest: request.input_digest.clone(),
            outcome: WorkbenchOutcome::Succeeded,
            resources: WorkbenchResourceUsage::default(),
            artifacts: Vec::new(),
            output: BTreeMap::new(),
            error: None,
        };
        let response = NanoExecResult {
            runtime_key: WORKBENCH_RUNTIME_BWRAP.to_string(),
            workload_id: "AGENT-07".to_string(),
            success: true,
            output: serde_json::to_string(&serde_json::json!({
                "schema_version": WORKBENCH_SCHEMA_VERSION,
                "invocation_id": request.invocation_id,
                "state": "completed",
                "messages": [result],
            }))
            .unwrap(),
        };
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let validations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let operations = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = GuardedRuntime {
            calls: Arc::clone(&calls),
            validations: Arc::clone(&validations),
            operations: Arc::clone(&operations),
            fail_validation: 3,
            response: response.clone(),
        };
        let coordinator = WorkbenchCoordinator::new(&store, &profile, &profile_digest);

        let error = coordinator
            .submit(&mut runtime, &request, &authority, 1_900_000_000_000)
            .unwrap_err();
        assert!(error.to_string().contains("World authority became stale"));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            store.load(&request.invocation_id).unwrap().unwrap().state,
            WorkbenchInvocationState::Executing
        );
        assert!(store.has_inflight().unwrap());
        let event_store = sentinel_limbo::EventStore::open(
            directory.path().join("events.db").to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(event_store.event_count().unwrap(), 0);

        let recovery_operations = Arc::new(Mutex::new(Vec::new()));
        let mut recovery_runtime = GuardedRuntime {
            calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            validations: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            operations: Arc::clone(&recovery_operations),
            fail_validation: usize::MAX,
            response,
        };
        let recovered = coordinator
            .recover_executing(
                &mut recovery_runtime,
                &request.invocation_id,
                &authority,
                1_900_000_000_001,
            )
            .unwrap();
        assert_eq!(recovered.records.last().unwrap().state, WorkbenchInvocationState::Succeeded);
        assert_eq!(operations.lock().unwrap().as_slice(), ["workbench_start"]);
        assert_eq!(
            recovery_operations.lock().unwrap().as_slice(),
            ["workbench_recover"]
        );
    }

    #[test]
    fn terminal_poll_and_recovery_replay_still_require_current_authority() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        let profile_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/workbench-profiles/web-authoring-v1.toml");
        let (profile, profile_digest) = WorkbenchProfile::load(profile_path).unwrap();
        let mut request = request("018f3f32-4f01-7f2c-a6c1-f6f4a81b2818");
        request.tool_profile_digest = profile_digest.clone();
        request.input_digest = request.canonical_digest().unwrap();
        store.reserve(&request, 1_900_000_000_000).unwrap();
        store
            .mark_executing(
                &request.invocation_id,
                &request.input_digest,
                1_900_000_000_001,
            )
            .unwrap();
        store
            .accept_result(
                &WorkbenchMessage::Result {
                    schema_version: WORKBENCH_SCHEMA_VERSION,
                    invocation_id: request.invocation_id.clone(),
                    input_digest: request.input_digest.clone(),
                    outcome: WorkbenchOutcome::Succeeded,
                    resources: WorkbenchResourceUsage::default(),
                    artifacts: Vec::new(),
                    output: BTreeMap::new(),
                    error: None,
                },
                1_900_000_000_002,
            )
            .unwrap();
        let mut authority = authority(&request, &profile);
        authority.assignment_active = false;
        let mut runtime = FakeRuntime {
            calls: 0,
            responses: VecDeque::new(),
        };
        let coordinator = WorkbenchCoordinator::new(&store, &profile, &profile_digest);

        for result in [
            coordinator.poll(
                &mut runtime,
                &request.invocation_id,
                &authority,
                1_900_000_000_003,
            ),
            coordinator.recover_executing(
                &mut runtime,
                &request.invocation_id,
                &authority,
                1_900_000_000_004,
            ),
        ] {
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("assignment is not active"));
        }
        assert_eq!(runtime.calls, 0);
    }

    #[test]
    fn cancellation_requires_current_authority_before_runtime_io() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        let profile_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/workbench-profiles/web-authoring-v1.toml");
        let (profile, profile_digest) = WorkbenchProfile::load(profile_path).unwrap();
        let mut request = request("018f3f32-4f01-7f2c-a6c1-f6f4a81b2814");
        request.tool_profile_digest = profile_digest.clone();
        request.input_digest = request.canonical_digest().unwrap();
        store.reserve(&request, 1_900_000_000_000).unwrap();
        let mut authority = authority(&request, &profile);
        authority.assignment_active = false;
        let mut runtime = FakeRuntime {
            calls: 0,
            responses: VecDeque::new(),
        };
        let coordinator = WorkbenchCoordinator::new(&store, &profile, &profile_digest);

        assert!(coordinator
            .cancel(
                &mut runtime,
                &request.invocation_id,
                "operator_cancelled",
                &authority,
                1_900_000_000_001,
            )
            .unwrap_err()
            .to_string()
            .contains("assignment is not active"));
        assert_eq!(runtime.calls, 0);
        assert_eq!(
            store.load(&request.invocation_id).unwrap().unwrap().state,
            WorkbenchInvocationState::Reserved
        );

        authority.assignment_active = true;
        let cancelled = coordinator
            .cancel(
                &mut runtime,
                &request.invocation_id,
                "operator_cancelled",
                &authority,
                1_900_000_000_002,
            )
            .unwrap();
        assert_eq!(runtime.calls, 0);
        assert_eq!(
            cancelled.records.last().unwrap().state,
            WorkbenchInvocationState::Cancelled
        );
    }

    #[test]
    fn transport_failure_keeps_execution_recoverable_without_redispatch() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        let profile_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/workbench-profiles/web-authoring-v1.toml");
        let (profile, profile_digest) = WorkbenchProfile::load(profile_path).unwrap();
        let mut request = request("018f3f32-4f01-7f2c-a6c1-f6f4a81b2815");
        request.tool_profile_digest = profile_digest.clone();
        request.input_digest = request.canonical_digest().unwrap();
        let authority = authority(&request, &profile);
        let recovered_result = WorkbenchMessage::Result {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: request.invocation_id.clone(),
            input_digest: request.input_digest.clone(),
            outcome: WorkbenchOutcome::Succeeded,
            resources: WorkbenchResourceUsage::default(),
            artifacts: Vec::new(),
            output: BTreeMap::new(),
            error: None,
        };
        let recovered = NanoExecResult {
            runtime_key: WORKBENCH_RUNTIME_BWRAP.to_string(),
            workload_id: "AGENT-07".to_string(),
            success: true,
            output: serde_json::to_string(&serde_json::json!({
                "schema_version": WORKBENCH_SCHEMA_VERSION,
                "invocation_id": request.invocation_id,
                "state": "completed",
                "messages": [recovered_result],
            }))
            .unwrap(),
        };
        let mut runtime = FakeRuntime {
            calls: 0,
            responses: VecDeque::from([
                Err(anyhow::anyhow!("runtime transport unavailable")),
                Ok(recovered),
            ]),
        };
        let coordinator = WorkbenchCoordinator::new(&store, &profile, &profile_digest);

        assert!(coordinator
            .submit(&mut runtime, &request, &authority, 1_900_000_000_000)
            .is_err());
        assert_eq!(runtime.calls, 1);
        assert_eq!(
            store.load(&request.invocation_id).unwrap().unwrap().state,
            WorkbenchInvocationState::Executing
        );
        let replay = coordinator
            .submit(&mut runtime, &request, &authority, 1_900_000_000_001)
            .unwrap();
        assert!(replay.replayed);
        assert_eq!(runtime.calls, 1, "replay must not dispatch a second start");
        let recovered = coordinator
            .recover_executing(
                &mut runtime,
                &request.invocation_id,
                &authority,
                1_900_000_000_002,
            )
            .unwrap();
        assert_eq!(runtime.calls, 2);
        assert_eq!(
            recovered.records.last().unwrap().state,
            WorkbenchInvocationState::Succeeded
        );
    }
}
