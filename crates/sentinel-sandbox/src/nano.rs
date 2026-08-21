use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use sentinel_common::nano_runtime::{
    ensure_handle_instance, ensure_handle_runtime, NanoExecError, NanoExecErrorCode,
    NanoExecRequest, NanoExecResult, NanoHandle, NanoHealth, NanoHealthState, NanoIsolationPolicy,
    NanoIsolationReport, NanoRecoveryResult, NanoRuntime, NanoRuntimeControlAction,
    NanoRuntimeControlResult, NanoRuntimeResources, NanoSnapshot, NanoSnapshotSemantics,
    NanoStopResult, NanoWorkloadSpec, RUNTIME_BWRAP_LANDLOCK,
};
use sentinel_common::{
    WorkbenchArtifactRef, WorkbenchMessage, WorkbenchOutcome, WorkbenchRequest, WorkbenchTool,
};
use sentinel_fs::artifact::ArtifactPlane;
use sentinel_fs::home_manifest::{self, HomeManifest, RestorePolicy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::enforcer::{
    ProtocolCancelOwner, ProtocolSupervisionFailure, ProtocolSupervisionSnapshot,
};
use crate::{cgroups, AgentProcess, CgroupLimits, SandboxEnforcer, SandboxHandle};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BwrapSnapshotPayload {
    workload: NanoWorkloadSpec,
    command: Vec<String>,
    home_manifest: HomeManifest,
    cgroup_created: bool,
    io_available: bool,
    bwrap_available: bool,
    landlock_available: bool,
    semantics_note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BwrapRecreateSnapshotPayload {
    workload: NanoWorkloadSpec,
    command: Vec<String>,
    semantics_note: String,
}

#[derive(Debug, Clone)]
struct BwrapWorkloadState {
    instance_id: uuid::Uuid,
    workload: NanoWorkloadSpec,
    command: Vec<String>,
    /// `ArtifactPlane` object ids pinning the chunks of this workload's last home
    /// snapshot (released on re-snapshot/teardown to avoid chunk leaks, N1').
    owned_object_ids: Vec<u64>,
    suspended: bool,
}

struct BwrapSpawnTransaction {
    state: BwrapWorkloadState,
    marker_written: bool,
    setup_started: bool,
    handle: Option<SandboxHandle>,
    process: Option<AgentProcess>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BwrapSpawnStage {
    MarkerWritten,
    SetupComplete,
    ProcessStarted,
}

#[derive(Debug)]
struct WorkbenchExchange {
    instance_id: uuid::Uuid,
    invocation_id: String,
    request_digest: String,
    input_digest: String,
    artifact_authority: Option<WorkbenchArtifactAuthority>,
    cancel_requested_at_ms: Option<u64>,
    cancel_digest: Option<String>,
    deadline_cancel_digest: Option<String>,
    cancel_origin: Option<WorkbenchCancelOrigin>,
    messages: Vec<serde_json::Value>,
    retained_bytes: usize,
    result_seen: bool,
    terminal: Option<WorkbenchTerminal>,
    terminal_error: Option<NanoExecError>,
    finalized: bool,
    cleanup_pending: bool,
}

#[derive(Debug, Clone)]
struct WorkbenchArtifactAuthority {
    workload_id: String,
    invocation_id: String,
    input_digest: String,
    project_id: String,
    work_item_id: String,
    workspace_id: String,
    agent_id: u16,
    runtime_key: String,
    tool_profile: String,
    tool_profile_digest: String,
    policy_digest: String,
    expected_package: Option<WorkbenchPackageExpectation>,
}

#[derive(Debug, Clone)]
struct WorkbenchPackageExpectation {
    artifact_kind: String,
    media_type: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundArtifactManifest {
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
    entries: Vec<BoundArtifactEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundArtifactEntry {
    path: String,
    blob_id: String,
    sha256: String,
    size_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkbenchCancelOrigin {
    Explicit,
    Deadline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkbenchTerminal {
    Succeeded,
    Failed,
    Cancelled,
}

/// Default home content-addressed store location (chunks live under `/ram`).
const DEFAULT_HOME_CAS_DIR: &str = "/ram/agents/.sentinel-home-cas";
const DEFAULT_AGENT_HOME_ROOT: &str = "/ram/agents";
const MAX_WORKBENCH_FRAME_BYTES: usize = 1024 * 1024;
const MAX_WORKBENCH_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_WORKBENCH_ARTIFACT_MANIFEST_BYTES: u64 = 1024 * 1024;
const WORKBENCH_RECOVERY_DEADLINE_MS: u64 = 5_000;

fn exec_error(
    code: NanoExecErrorCode,
    retryable: bool,
    safe_message: &'static str,
) -> anyhow::Error {
    NanoExecError::new(code, retryable, safe_message).into()
}

fn frame_digest(input: &str) -> String {
    hex_sha256_bytes(input.as_bytes())
}

fn hex_sha256_bytes(input: &[u8]) -> String {
    let digest = Sha256::digest(input);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

impl WorkbenchArtifactAuthority {
    fn from_request(workload_id: &str, request: &WorkbenchRequest) -> Self {
        let expected_package = match &request.tool {
            WorkbenchTool::PackageArtifact {
                artifact_kind,
                media_type,
                ..
            } => Some(WorkbenchPackageExpectation {
                artifact_kind: artifact_kind.clone(),
                media_type: media_type.clone(),
            }),
            _ => None,
        };
        Self {
            workload_id: workload_id.to_string(),
            invocation_id: request.invocation_id.clone(),
            input_digest: request.input_digest.clone(),
            project_id: request.project_id.clone(),
            work_item_id: request.work_item_id.clone(),
            workspace_id: request.workspace_id.clone(),
            agent_id: request.agent_id.0,
            runtime_key: request.runtime_key.clone(),
            tool_profile: request.tool_profile.clone(),
            tool_profile_digest: request.tool_profile_digest.clone(),
            policy_digest: request.policy_digest.clone(),
            expected_package,
        }
    }
}

fn is_workbench_agent_runtime(command: &[String]) -> bool {
    command.first().is_some_and(|program| {
        std::path::Path::new(program)
            .file_name()
            .is_some_and(|name| name == "agent-runtime")
    })
}

fn parse_control_frame(input: &str) -> Result<serde_json::Value> {
    if input.len() > MAX_WORKBENCH_FRAME_BYTES {
        return Err(exec_error(
            NanoExecErrorCode::InvalidFrame,
            false,
            "workbench control frame exceeds the configured limit",
        ));
    }
    if input
        .as_bytes()
        .iter()
        .any(|byte| matches!(byte, b'\n' | b'\r'))
    {
        return Err(exec_error(
            NanoExecErrorCode::InvalidFrame,
            false,
            "workbench control input must contain exactly one JSONL record",
        ));
    }
    serde_json::from_str(input).map_err(|_| {
        exec_error(
            NanoExecErrorCode::InvalidFrame,
            false,
            "workbench control frame is not valid JSON",
        )
    })
}

fn validate_terminal_artifacts(
    host_agent_root: &std::path::Path,
    authority: &WorkbenchArtifactAuthority,
    message: &WorkbenchMessage,
) -> Result<()> {
    let WorkbenchMessage::Result {
        invocation_id,
        input_digest,
        outcome,
        artifacts,
        ..
    } = message
    else {
        return Ok(());
    };
    if invocation_id != &authority.invocation_id || input_digest != &authority.input_digest {
        return Err(exec_error(
            NanoExecErrorCode::InvocationConflict,
            false,
            "workbench result authority binding is invalid",
        ));
    }
    if *outcome != WorkbenchOutcome::Succeeded {
        if artifacts.is_empty() {
            return Ok(());
        }
        return Err(exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "failed workbench result cannot publish artifacts",
        ));
    }
    match authority.expected_package.as_ref() {
        None if artifacts.is_empty() => Ok(()),
        None => Err(exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "non-package workbench result cannot publish artifacts",
        )),
        Some(expected) if artifacts.len() == 1 => {
            validate_artifact_manifest(host_agent_root, authority, expected, &artifacts[0])
        }
        Some(_) => Err(exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "package workbench result must publish exactly one artifact",
        )),
    }
}

fn validate_artifact_manifest(
    host_agent_root: &std::path::Path,
    authority: &WorkbenchArtifactAuthority,
    expected: &WorkbenchPackageExpectation,
    artifact: &WorkbenchArtifactRef,
) -> Result<()> {
    if artifact.artifact_kind != expected.artifact_kind
        || artifact.media_type != expected.media_type
        || !valid_sha256(&artifact.sha256)
        || artifact.artifact_id != format!("sha256:{}", artifact.sha256)
    {
        return Err(exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact reference conflicts with its request binding",
        ));
    }
    let relative = safe_relative_path(&artifact.manifest_path)?;
    if relative.components().count() != 1
        || !matches!(
            relative.components().next(),
            Some(std::path::Component::Normal(_))
        )
    {
        return Err(exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact manifest path is not canonical",
        ));
    }
    let artifact_root = bind_workbench_artifact_scope(
        host_agent_root,
        &authority.workload_id,
        &authority.project_id,
        &authority.work_item_id,
    )?;
    let manifest_path = artifact_root.join(relative);
    let metadata = std::fs::symlink_metadata(&manifest_path).map_err(|_| {
        exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact manifest is missing",
        )
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_WORKBENCH_ARTIFACT_MANIFEST_BYTES
    {
        return Err(exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact manifest is outside its integrity boundary",
        ));
    }
    let manifest_path = std::fs::canonicalize(&manifest_path).map_err(|_| {
        exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact manifest cannot be resolved",
        )
    })?;
    if !manifest_path.starts_with(&artifact_root) {
        return Err(exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact manifest escaped its authority scope",
        ));
    }
    let manifest_name = manifest_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            exec_error(
                NanoExecErrorCode::ProtocolViolation,
                false,
                "workbench artifact manifest name is invalid",
            )
        })?;
    let scope = open_pinned_artifact_directory(&artifact_root)?;
    let mut manifest_file = open_scoped_artifact_file(
        &scope,
        manifest_name,
        MAX_WORKBENCH_ARTIFACT_MANIFEST_BYTES,
        None,
    )?;
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut manifest_file)
        .take(MAX_WORKBENCH_ARTIFACT_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            exec_error(
                NanoExecErrorCode::ProtocolViolation,
                false,
                "workbench artifact manifest cannot be read",
            )
        })?;
    revalidate_scoped_artifact_file(&scope, manifest_name, &manifest_file)?;
    if hex_sha256_bytes(&bytes) != artifact.sha256 {
        return Err(exec_error(
            NanoExecErrorCode::DigestConflict,
            false,
            "workbench artifact manifest digest conflicts with its reference",
        ));
    }
    let manifest: BoundArtifactManifest = serde_json::from_slice(&bytes).map_err(|_| {
        exec_error(
            NanoExecErrorCode::InvalidFrame,
            false,
            "workbench artifact manifest is invalid",
        )
    })?;
    if manifest.schema_version != 1
        || manifest.invocation_id != authority.invocation_id
        || manifest.input_digest != authority.input_digest
        || manifest.project_id != authority.project_id
        || manifest.work_item_id != authority.work_item_id
        || manifest.workspace_id != authority.workspace_id
        || manifest.agent_id != authority.agent_id
        || manifest.artifact_kind != expected.artifact_kind
        || manifest.media_type != expected.media_type
        || manifest.runtime_key != authority.runtime_key
        || manifest.tool_profile != authority.tool_profile
        || manifest.tool_profile_digest != authority.tool_profile_digest
        || manifest.policy_digest != authority.policy_digest
        || manifest.entries.is_empty()
    {
        return Err(exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact manifest authority binding is invalid",
        ));
    }
    let blobs_root = canonical_child_directory(&artifact_root, "blobs")?;
    let blobs = open_pinned_artifact_directory(&blobs_root)?;
    let mut total_size = 0_u64;
    for entry in manifest.entries {
        let _ = safe_relative_path(&entry.path)?;
        if !valid_sha256(&entry.sha256) || entry.blob_id != format!("sha256:{}", entry.sha256) {
            return Err(exec_error(
                NanoExecErrorCode::ProtocolViolation,
                false,
                "workbench artifact manifest contains an invalid blob binding",
            ));
        }
        total_size = total_size.checked_add(entry.size_bytes).ok_or_else(|| {
            exec_error(
                NanoExecErrorCode::OutputLimitExceeded,
                false,
                "workbench artifact size overflowed its boundary",
            )
        })?;
        let mut blob = open_scoped_artifact_file(
            &blobs,
            &entry.sha256,
            entry.size_bytes,
            Some(entry.size_bytes),
        )?;
        let mut blob_bytes = Vec::new();
        std::io::Read::by_ref(&mut blob)
            .take(entry.size_bytes.saturating_add(1))
            .read_to_end(&mut blob_bytes)
            .map_err(|_| {
                exec_error(
                    NanoExecErrorCode::ProtocolViolation,
                    false,
                    "workbench artifact blob cannot be read",
                )
            })?;
        revalidate_scoped_artifact_file(&blobs, &entry.sha256, &blob)?;
        let digest = hex_sha256_bytes(&blob_bytes);
        if digest != entry.sha256 {
            return Err(exec_error(
                NanoExecErrorCode::DigestConflict,
                false,
                "workbench artifact blob digest conflicts with its manifest",
            ));
        }
    }
    if total_size != artifact.size_bytes {
        return Err(exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact size conflicts with its manifest",
        ));
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
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
        }
    }
}

fn current_process_uid() -> Result<u32> {
    std::fs::metadata("/proc/self")
        .map(|metadata| metadata.uid())
        .map_err(|_| {
            exec_error(
                NanoExecErrorCode::ProtocolViolation,
                false,
                "workbench artifact owner cannot be verified",
            )
        })
}

fn open_pinned_artifact_directory(path: &std::path::Path) -> Result<File> {
    let before = std::fs::symlink_metadata(path).map_err(|_| {
        exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact directory cannot be inspected",
        )
    })?;
    if before.file_type().is_symlink() || !before.is_dir() {
        return Err(exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact directory identity is invalid",
        ));
    }
    let directory = File::open(path).map_err(|_| {
        exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact directory cannot be opened",
        )
    })?;
    let opened = directory.metadata().map_err(|_| {
        exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact directory cannot be verified",
        )
    })?;
    if before.dev() != opened.dev() || before.ino() != opened.ino() {
        return Err(exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact directory identity changed",
        ));
    }
    Ok(directory)
}

fn open_scoped_artifact_file(
    directory: &File,
    name: &str,
    max_bytes: u64,
    exact_size: Option<u64>,
) -> Result<File> {
    open_scoped_artifact_file_with(directory, name, max_bytes, exact_size, |path| {
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
    })
}

fn open_scoped_artifact_file_with<Open>(
    directory: &File,
    name: &str,
    max_bytes: u64,
    exact_size: Option<u64>,
    open: Open,
) -> Result<File>
where
    Open: FnOnce(&std::path::Path) -> std::io::Result<File>,
{
    safe_scope_component(name)?;
    let path = PathBuf::from(format!("/proc/self/fd/{}/{}", directory.as_raw_fd(), name));
    let before = std::fs::symlink_metadata(&path).map_err(|_| {
        exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact file cannot be inspected",
        )
    })?;
    validate_artifact_file_metadata(&before, max_bytes, exact_size)?;
    let expected = ArtifactFileIdentity::from_metadata(&before);
    let file = open(&path).map_err(|_| {
        exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact file cannot be opened",
        )
    })?;
    let opened = file.metadata().map_err(|_| {
        exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact file cannot be verified",
        )
    })?;
    validate_artifact_file_metadata(&opened, max_bytes, exact_size)?;
    if ArtifactFileIdentity::from_metadata(&opened) != expected {
        return Err(exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact file identity changed before open",
        ));
    }
    Ok(file)
}

fn validate_artifact_file_metadata(
    metadata: &std::fs::Metadata,
    max_bytes: u64,
    exact_size: Option<u64>,
) -> Result<()> {
    let mode = metadata.mode() & 0o7777;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.nlink() != 1
        || metadata.uid() != current_process_uid()?
        || mode & 0o7022 != 0
        || metadata.len() > max_bytes
        || exact_size.is_some_and(|size| metadata.len() != size)
    {
        return Err(exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact file ownership or mode is invalid",
        ));
    }
    Ok(())
}

fn revalidate_scoped_artifact_file(directory: &File, name: &str, file: &File) -> Result<()> {
    let path = PathBuf::from(format!("/proc/self/fd/{}/{}", directory.as_raw_fd(), name));
    let opened = file.metadata().map_err(|_| {
        exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact file cannot be revalidated",
        )
    })?;
    let path_metadata = std::fs::symlink_metadata(&path).map_err(|_| {
        exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact file disappeared during read",
        )
    })?;
    validate_artifact_file_metadata(&opened, opened.len(), Some(opened.len()))?;
    validate_artifact_file_metadata(&path_metadata, opened.len(), Some(opened.len()))?;
    if ArtifactFileIdentity::from_metadata(&path_metadata)
        != ArtifactFileIdentity::from_metadata(&opened)
    {
        return Err(exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact file identity changed during read",
        ));
    }
    Ok(())
}

fn bind_workbench_artifact_scope(
    host_agent_root: &std::path::Path,
    expected_workload_id: &str,
    project_id: &str,
    work_item_id: &str,
) -> Result<std::path::PathBuf> {
    use std::os::unix::fs::MetadataExt;

    let agent_root = canonical_real_directory(host_agent_root)?;
    let marker = agent_root.join(".nano-runtime");
    let marker_metadata = std::fs::symlink_metadata(&marker).map_err(|_| {
        exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench agent artifact authority marker is unavailable",
        )
    })?;
    if marker_metadata.file_type().is_symlink()
        || !marker_metadata.is_file()
        || marker_metadata.nlink() != 1
        || std::fs::read_to_string(&marker).ok().as_deref() != Some(expected_workload_id)
    {
        return Err(exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench agent artifact authority marker is invalid",
        ));
    }
    let artifacts = canonical_child_directory(&agent_root, "artifacts")?;
    let project = canonical_child_directory(&artifacts, safe_scope_component(project_id)?)?;
    canonical_child_directory(&project, safe_scope_component(work_item_id)?)
}

fn safe_scope_component(value: &str) -> Result<&str> {
    let mut components = std::path::Path::new(value).components();
    if value.is_empty()
        || !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact scope component is invalid",
        ));
    }
    Ok(value)
}

fn canonical_child_directory(parent: &std::path::Path, child: &str) -> Result<std::path::PathBuf> {
    let path = parent.join(child);
    let metadata = std::fs::symlink_metadata(&path).map_err(|_| {
        exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact scope directory is unavailable",
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact scope directory is invalid",
        ));
    }
    let canonical = std::fs::canonicalize(&path).map_err(|_| {
        exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact scope directory cannot be resolved",
        )
    })?;
    if canonical.parent() != Some(parent) {
        return Err(exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact scope escaped its authority base",
        ));
    }
    Ok(canonical)
}

fn canonical_real_directory(path: &std::path::Path) -> Result<std::path::PathBuf> {
    let mut current = std::path::PathBuf::new();
    if !path.is_absolute() {
        return Err(exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact authority base is not absolute",
        ));
    }
    for component in path.components() {
        match component {
            std::path::Component::RootDir => current.push(std::path::MAIN_SEPARATOR_STR),
            std::path::Component::Normal(component) => current.push(component),
            _ => {
                return Err(exec_error(
                    NanoExecErrorCode::ProtocolViolation,
                    false,
                    "workbench artifact authority base is not canonical",
                ))
            }
        }
        let metadata = std::fs::symlink_metadata(&current).map_err(|_| {
            exec_error(
                NanoExecErrorCode::ProtocolViolation,
                false,
                "workbench artifact authority base is unavailable",
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(exec_error(
                NanoExecErrorCode::ProtocolViolation,
                false,
                "workbench artifact authority base contains an invalid component",
            ));
        }
    }
    let canonical = std::fs::canonicalize(&current).map_err(|_| {
        exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact authority base cannot be resolved",
        )
    })?;
    if canonical != current {
        return Err(exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact authority base changed identity",
        ));
    }
    Ok(canonical)
}

fn safe_relative_path(path: &str) -> Result<&std::path::Path> {
    let path = std::path::Path::new(path);
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
    {
        return Err(exec_error(
            NanoExecErrorCode::ProtocolViolation,
            false,
            "workbench artifact path escaped its authority scope",
        ));
    }
    Ok(path)
}

fn require_schema_version(value: Option<&serde_json::Value>) -> Result<()> {
    match value.and_then(serde_json::Value::as_u64) {
        Some(1) => Ok(()),
        _ => Err(exec_error(
            NanoExecErrorCode::UnsupportedVersion,
            false,
            "workbench protocol version is not supported",
        )),
    }
}

pub struct BwrapNanoRuntime {
    enforcer: SandboxEnforcer,
    /// Directory holding the home-content `ArtifactPlane`, opened lazily so the
    /// constructor stays infallible and does no I/O (daemon/registry callers that
    /// never snapshot are unaffected).
    cas_dir: PathBuf,
    agent_home_root: PathBuf,
    fs_mount: Option<PathBuf>,
    workloads: HashMap<String, BwrapWorkloadState>,
    handles: HashMap<String, SandboxHandle>,
    processes: HashMap<String, AgentProcess>,
    pending_spawns: HashMap<String, BwrapSpawnTransaction>,
    cas_manifest_enabled: bool,
    exchanges: HashMap<String, WorkbenchExchange>,
}

impl BwrapNanoRuntime {
    pub fn detect() -> Self {
        Self::with_cas_dir(DEFAULT_HOME_CAS_DIR)
    }

    /// Construct with an explicit home-content CAS directory (used in tests).
    pub fn with_cas_dir(cas_dir: impl Into<PathBuf>) -> Self {
        let (enforcer, _warnings) = SandboxEnforcer::detect();
        Self {
            enforcer,
            cas_dir: cas_dir.into(),
            agent_home_root: PathBuf::from(DEFAULT_AGENT_HOME_ROOT),
            fs_mount: None,
            workloads: HashMap::new(),
            handles: HashMap::new(),
            processes: HashMap::new(),
            pending_spawns: HashMap::new(),
            cas_manifest_enabled: false,
            exchanges: HashMap::new(),
        }
    }

    /// #548 feature boundary. Disabled production instances retain the safe
    /// workload-spec recreate semantics and never walk, pin, or rehydrate CAS
    /// home manifests.
    pub fn set_cas_manifest_enabled(&mut self, enabled: bool) {
        self.cas_manifest_enabled = enabled;
    }

    /// Keep daemon FUSE routing identical when bwrap lifecycle ownership moves
    /// behind the NanoRuntime adapter.
    pub fn set_fs_mount(&mut self, mount: impl Into<String>) {
        let mount = mount.into();
        self.fs_mount = Some(PathBuf::from(&mount));
        self.enforcer.set_fs_mount(mount);
    }

    #[cfg(test)]
    fn with_test_dirs(cas_dir: impl Into<PathBuf>, agent_home_root: impl Into<PathBuf>) -> Self {
        let mut runtime = Self::with_cas_dir(cas_dir);
        runtime.agent_home_root = agent_home_root.into();
        runtime.cas_manifest_enabled = true;
        runtime
    }

    /// Open (or create) the home-content `ArtifactPlane`. Called only on the
    /// snapshot/restore paths, so the constructor stays I/O-free.
    fn open_plane(&self) -> Result<ArtifactPlane> {
        std::fs::create_dir_all(&self.cas_dir)
            .with_context(|| format!("create home CAS dir {}", self.cas_dir.display()))?;
        ArtifactPlane::open(self.cas_dir.join("home.redb"))
    }

    fn command_for(workload: &NanoWorkloadSpec) -> Vec<String> {
        if workload.command.is_empty() {
            vec!["/usr/bin/sleep".to_string(), "30".to_string()]
        } else {
            workload.command.clone()
        }
    }

    fn home_dir(&self, agent_name: &str) -> PathBuf {
        self.agent_home_root.join(agent_name)
    }

    fn workbench_host_root(&self, workload_id: &str) -> Result<PathBuf> {
        let state = self
            .workloads
            .get(workload_id)
            .ok_or_else(|| anyhow!("workbench workload state is unavailable"))?;
        Ok(self.agent_home_root.join(&state.workload.agent_name))
    }

    fn write_marker(&self, agent_name: &str, workload_id: &str) -> Result<()> {
        let home = self.home_dir(agent_name);
        std::fs::create_dir_all(&home)?;
        let marker = home.join(".nano-runtime");
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
        {
            Ok(mut file) => {
                if let Err(error) = (|| -> std::io::Result<()> {
                    file.write_all(workload_id.as_bytes())?;
                    file.sync_all()
                })() {
                    let _ = std::fs::remove_file(&marker);
                    return Err(error.into());
                }
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let recorded = std::fs::read_to_string(&marker)?;
                if recorded == workload_id {
                    Ok(())
                } else {
                    Err(anyhow!(
                        "bwrap marker for '{agent_name}' belongs to workload '{recorded}', not '{workload_id}'"
                    ))
                }
            }
            Err(error) => Err(error.into()),
        }
    }

    fn remove_marker(&self, agent_name: &str, workload_id: &str) -> Result<bool> {
        let marker = self.home_dir(agent_name).join(".nano-runtime");
        let recorded = match std::fs::read_to_string(&marker) {
            Ok(recorded) => recorded,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        if recorded != workload_id {
            return Err(anyhow!(
                "bwrap marker for '{agent_name}' belongs to workload '{recorded}', not '{workload_id}'"
            ));
        }
        std::fs::remove_file(marker)?;
        Ok(true)
    }

    fn teardown_workload(&mut self, workload_id: &str) -> Result<bool> {
        let stopped = self.teardown_runtime_resources(workload_id)?;
        self.exchanges.remove(workload_id);
        Ok(stopped)
    }

    fn teardown_runtime_resources(&mut self, workload_id: &str) -> Result<bool> {
        let stopped = self.processes.contains_key(workload_id)
            || self.handles.contains_key(workload_id)
            || self.workloads.contains_key(workload_id);
        let (owned_process_reaped, cgroup_quiesced) =
            if let Some(process) = self.processes.get_mut(workload_id) {
                process.terminate_checked()?;
                (
                    true,
                    process.protocol_supervision_snapshot().cgroup_quiesced,
                )
            } else {
                (false, false)
            };
        if let Some(handle) = self.handles.get(workload_id).cloned() {
            let handle = teardown_handle_after_owned_process_reap(
                handle,
                owned_process_reaped,
                cgroup_quiesced,
            );
            self.enforcer.teardown_agent(&handle)?;
        }
        if let Some(process) = self.processes.get_mut(workload_id) {
            process.join_protocol_reader();
        }
        if let Some((agent_name, owned_object_ids)) = self.workloads.get(workload_id).map(|state| {
            (
                state.workload.agent_name.clone(),
                state.owned_object_ids.clone(),
            )
        }) {
            if !owned_object_ids.is_empty() {
                let plane = self.open_plane()?;
                home_manifest::release_manifest(&plane, &owned_object_ids)?;
            }
            self.remove_marker(&agent_name, workload_id)?;
        }
        self.processes.remove(workload_id);
        self.handles.remove(workload_id);
        self.workloads.remove(workload_id);
        Ok(stopped)
    }

    fn rollback_pending_spawn(&mut self, workload_id: &str) -> Result<bool> {
        if !self.pending_spawns.contains_key(workload_id) {
            return Ok(false);
        }
        let owned_process_reaped = if let Some(process) = self
            .pending_spawns
            .get_mut(workload_id)
            .and_then(|transaction| transaction.process.as_mut())
        {
            process
                .terminate_checked()
                .with_context(|| format!("rollback process for {workload_id}"))?;
            true
        } else {
            false
        };
        let handle = self
            .pending_spawns
            .get(workload_id)
            .and_then(|transaction| transaction.handle.clone());
        let (setup_started, marker_written, agent_name, marker_workload_id) = {
            let transaction = self
                .pending_spawns
                .get(workload_id)
                .expect("pending spawn checked above");
            (
                transaction.setup_started,
                transaction.marker_written,
                transaction.state.workload.agent_name.clone(),
                transaction.state.workload.workload_id.clone(),
            )
        };
        if let Some(handle) = handle {
            let handle =
                teardown_handle_after_owned_process_reap(handle, owned_process_reaped, false);
            self.enforcer
                .teardown_agent(&handle)
                .with_context(|| format!("rollback sandbox for {workload_id}"))?;
        } else if setup_started {
            self.enforcer
                .recover_partial_agent_setup(&agent_name)
                .with_context(|| format!("rollback partial setup for {workload_id}"))?;
        }
        if let Some(process) = self
            .pending_spawns
            .get_mut(workload_id)
            .and_then(|transaction| transaction.process.as_mut())
        {
            process.join_protocol_reader();
        }
        if marker_written {
            self.remove_marker(&agent_name, &marker_workload_id)
                .with_context(|| format!("rollback marker for {workload_id}"))?;
        }
        self.pending_spawns.remove(workload_id);
        Ok(true)
    }

    fn reconcile_durable_spawn_marker(&self, agent_name: &str, workload_id: &str) -> Result<bool> {
        let marker = self.home_dir(agent_name).join(".nano-runtime");
        let recorded = match std::fs::read_to_string(&marker) {
            Ok(recorded) => recorded,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        if recorded != workload_id {
            return Err(anyhow!(
                "bwrap agent home '{agent_name}' has durable ownership by workload '{recorded}'"
            ));
        }
        self.enforcer
            .recover_partial_agent_setup(agent_name)
            .with_context(|| format!("recover durable bwrap spawn for {workload_id}"))?;
        self.remove_marker(agent_name, workload_id)?;
        Ok(true)
    }

    fn ensure_workload_available(&self, workload: &NanoWorkloadSpec) -> Result<()> {
        if self.workloads.contains_key(&workload.workload_id)
            || self.handles.contains_key(&workload.workload_id)
            || self.processes.contains_key(&workload.workload_id)
            || self.pending_spawns.contains_key(&workload.workload_id)
            || self.exchanges.contains_key(&workload.workload_id)
        {
            return Err(anyhow!(
                "bwrap workload '{}' is already active",
                workload.workload_id
            ));
        }
        if let Some(existing) = self
            .workloads
            .values()
            .find(|state| state.workload.agent_name == workload.agent_name)
        {
            return Err(anyhow!(
                "bwrap agent home '{}' is already owned by workload '{}'",
                workload.agent_name,
                existing.workload.workload_id
            ));
        }
        if let Some(existing) = self
            .pending_spawns
            .values()
            .find(|transaction| transaction.state.workload.agent_name == workload.agent_name)
        {
            return Err(anyhow!(
                "bwrap agent home '{}' has pending ownership by workload '{}'",
                workload.agent_name,
                existing.state.workload.workload_id
            ));
        }
        Ok(())
    }

    fn ensure_restore_target_available(
        &self,
        snapshot_workload_id: &str,
        workload: &NanoWorkloadSpec,
    ) -> Result<()> {
        if workload.workload_id != snapshot_workload_id {
            return Err(anyhow!(
                "bwrap snapshot workload '{}' does not match envelope '{}'",
                workload.workload_id,
                snapshot_workload_id
            ));
        }
        if let Some(existing) = self.workloads.values().find(|state| {
            state.workload.workload_id != snapshot_workload_id
                && state.workload.agent_name == workload.agent_name
        }) {
            return Err(anyhow!(
                "bwrap agent home '{}' is already owned by workload '{}'",
                workload.agent_name,
                existing.workload.workload_id
            ));
        }
        if let Some(existing) = self.pending_spawns.values().find(|transaction| {
            transaction.state.workload.workload_id != snapshot_workload_id
                && transaction.state.workload.agent_name == workload.agent_name
        }) {
            return Err(anyhow!(
                "bwrap agent home '{}' has pending ownership by workload '{}'",
                workload.agent_name,
                existing.state.workload.workload_id
            ));
        }
        Ok(())
    }

    fn workload_pids(&self, workload_id: &str) -> Result<Vec<u32>> {
        let handle = self
            .handles
            .get(workload_id)
            .ok_or_else(|| anyhow!("missing bwrap sandbox handle '{workload_id}'"))?;
        let mut pids = if handle.cgroup_created {
            cgroups::list_pids_in_cgroup(&handle.agent_name)
                .with_context(|| format!("list bwrap cgroup members for {}", handle.agent_name))?
        } else {
            Vec::new()
        };
        if let Some(process) = self.processes.get(workload_id) {
            if !process.owned_process_reaped() {
                pids.push(process.pid);
                if let Some(child_pid) = process.child_pid {
                    pids.push(child_pid);
                }
            }
        }
        pids.sort_unstable();
        pids.dedup();
        if pids.is_empty() {
            return Err(anyhow!(
                "bwrap workload '{workload_id}' has no live execution unit"
            ));
        }
        Ok(pids)
    }

    fn signal_workload(
        &self,
        workload_id: &str,
        signal: nix::sys::signal::Signal,
    ) -> Result<usize> {
        let pids = self.workload_pids(workload_id)?;
        for pid in &pids {
            let result = nix::sys::signal::kill(nix::unistd::Pid::from_raw(*pid as i32), signal);
            if let Err(error) = result {
                if std::path::Path::new(&format!("/proc/{pid}")).exists() {
                    return Err(error)
                        .with_context(|| format!("signal {signal:?} to bwrap PID {pid}"));
                }
            }
        }
        let expect_stopped = signal == nix::sys::signal::Signal::SIGSTOP;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let states = pids
                .iter()
                .filter_map(|pid| {
                    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
                    let state = status.lines().find_map(|line| {
                        line.strip_prefix("State:")
                            .and_then(|value| value.trim().chars().next())
                    })?;
                    (state != 'Z').then_some((*pid, state))
                })
                .collect::<Vec<_>>();
            let confirmed = if expect_stopped {
                !states.is_empty() && states.iter().all(|(_, state)| *state == 'T')
            } else {
                !states.is_empty() && states.iter().all(|(_, state)| *state != 'T')
            };
            if confirmed {
                break;
            }
            if std::time::Instant::now() >= deadline {
                return Err(anyhow!(
                    "bwrap workload '{workload_id}' did not confirm signal {signal}; states={states:?}"
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        Ok(pids.len())
    }

    fn spawn_state_with<Start, Checkpoint>(
        &mut self,
        state: BwrapWorkloadState,
        start_process: Start,
        mut checkpoint: Checkpoint,
    ) -> Result<NanoHandle>
    where
        Start: FnOnce(&SandboxEnforcer, &str, &str, &[String]) -> Result<AgentProcess>,
        Checkpoint: FnMut(BwrapSpawnStage) -> Result<()>,
    {
        let workload = state.workload.clone();
        let instance_id = state.instance_id;
        let agent_name = workload.agent_name.clone();
        let workload_id = workload.workload_id.clone();
        self.pending_spawns.insert(
            workload_id.clone(),
            BwrapSpawnTransaction {
                state,
                marker_written: false,
                setup_started: false,
                handle: None,
                process: None,
            },
        );

        let attempted = (|| -> Result<u32> {
            // Claim marker cleanup ownership before attempting the write. If
            // write/fsync fails and the immediate unlink also fails, rollback
            // must still probe and retain this exact transaction for retry.
            self.pending_spawns
                .get_mut(&workload_id)
                .expect("pending spawn inserted")
                .marker_written = true;
            self.write_marker(&agent_name, &workload_id)?;
            checkpoint(BwrapSpawnStage::MarkerWritten)?;

            self.pending_spawns
                .get_mut(&workload_id)
                .expect("pending spawn inserted")
                .setup_started = true;
            let handle = self
                .enforcer
                .setup_agent(&agent_name, &CgroupLimits::default())
                .with_context(|| format!("bwrap setup_agent failed for {agent_name}"))?;
            self.pending_spawns
                .get_mut(&workload_id)
                .expect("pending spawn inserted")
                .handle = Some(handle);
            checkpoint(BwrapSpawnStage::SetupComplete)?;

            let command = &self
                .pending_spawns
                .get(&workload_id)
                .expect("pending spawn inserted")
                .state
                .command;
            let workbench_process = is_workbench_agent_runtime(command);
            let process = start_process(&self.enforcer, &agent_name, &workload_id, command)
                .with_context(|| format!("bwrap start_agent_process failed for {agent_name}"))?;
            let pid = process.pid;
            let transaction = self
                .pending_spawns
                .get_mut(&workload_id)
                .expect("pending spawn inserted");
            let handle = transaction
                .handle
                .as_mut()
                .expect("setup completed before process start");
            if workbench_process {
                let attestation = process
                    .workbench_isolation_attestation()
                    .context("workbench process returned without isolation attestation")?;
                anyhow::ensure!(
                    handle.cgroup_created
                        && process.child_pid == Some(attestation.child_pid)
                        && attestation.landlock_abi > 0,
                    "workbench process isolation evidence is incomplete"
                );
                // These flags are published only after the exact child, cgroup,
                // netns and post-Landlock exec evidence have all succeeded.
                handle.landlock_applied = true;
                handle.network_isolated = true;
            }
            handle.bwrap_pid = Some(pid);
            transaction.process = Some(process);
            checkpoint(BwrapSpawnStage::ProcessStarted)?;
            Ok(pid)
        })();

        let pid = match attempted {
            Ok(pid) => pid,
            Err(error) => {
                let rollback_error = self.rollback_pending_spawn(&workload_id).err();
                return Err(match rollback_error {
                    Some(rollback_error) => anyhow!(
                        "bwrap spawn transaction failed: {error}; rollback retained for retry: {rollback_error}"
                    ),
                    None => error,
                });
            }
        };

        let transaction = self
            .pending_spawns
            .remove(&workload_id)
            .expect("successful spawn has pending transaction");
        self.processes.insert(
            workload_id.clone(),
            transaction.process.expect("process started before commit"),
        );
        self.handles.insert(
            workload_id.clone(),
            transaction.handle.expect("setup completed before commit"),
        );
        self.workloads
            .insert(workload_id.clone(), transaction.state);

        Ok(NanoHandle {
            instance_id,
            runtime_key: RUNTIME_BWRAP_LANDLOCK.to_string(),
            workload_id,
            agent_id: workload.agent_id,
            pid: Some(pid),
        })
    }

    fn spawn_state(&mut self, state: BwrapWorkloadState) -> Result<NanoHandle> {
        self.spawn_state_with(
            state,
            |enforcer, agent_name, workload_id, command| {
                if is_workbench_agent_runtime(command) {
                    enforcer.start_workbench_process(agent_name, Some(workload_id), command)
                } else {
                    enforcer.start_agent_process(agent_name, Some(workload_id), command)
                }
            },
            |_| Ok(()),
        )
    }

    fn fail_workbench_exchange(
        &mut self,
        workload_id: &str,
        code: NanoExecErrorCode,
        safe_message: &'static str,
    ) -> anyhow::Error {
        let failure = NanoExecError::new(code, false, safe_message);
        if let Some(exchange) = self.exchanges.get_mut(workload_id) {
            exchange.terminal_error = Some(failure.clone());
            exchange.cleanup_pending = true;
        }
        if self.teardown_runtime_resources(workload_id).is_err() {
            return exec_error(
                NanoExecErrorCode::ChannelDisconnected,
                true,
                "workbench cleanup remains pending",
            );
        }
        if let Some(exchange) = self.exchanges.get_mut(workload_id) {
            exchange.cleanup_pending = false;
        }
        failure.into()
    }

    fn synchronize_protocol_supervision(
        &mut self,
        workload_id: &str,
    ) -> Option<ProtocolSupervisionSnapshot> {
        let snapshot = self
            .processes
            .get(workload_id)
            .map(AgentProcess::protocol_supervision_snapshot)?;
        if snapshot.cancel_owner == Some(ProtocolCancelOwner::Deadline) {
            if let Some(exchange) = self.exchanges.get_mut(workload_id) {
                if exchange.cancel_origin != Some(WorkbenchCancelOrigin::Deadline) {
                    let cancel = deadline_cancel_frame(&exchange.invocation_id);
                    let digest = frame_digest(&cancel);
                    exchange.cancel_requested_at_ms = snapshot.cancel_requested_at_ms;
                    if let Some(existing) = exchange.deadline_cancel_digest.as_deref() {
                        debug_assert_eq!(existing, digest);
                    } else {
                        exchange.deadline_cancel_digest = Some(digest);
                    }
                    exchange.cancel_origin = Some(WorkbenchCancelOrigin::Deadline);
                }
            }
        }
        Some(snapshot)
    }

    fn retry_pending_exchange_cleanup(&mut self, workload_id: &str) -> Result<()> {
        if !self
            .exchanges
            .get(workload_id)
            .is_some_and(|exchange| exchange.cleanup_pending)
        {
            return Ok(());
        }
        let productive_workbench = self
            .workloads
            .get(workload_id)
            .is_some_and(|state| is_workbench_agent_runtime(&state.command));
        let cleanup = if productive_workbench {
            self.recycle_workbench_runtime(workload_id)
        } else {
            self.teardown_runtime_resources(workload_id).map(|_| ())
        };
        cleanup.map_err(|_| {
            exec_error(
                NanoExecErrorCode::ChannelDisconnected,
                true,
                "workbench runtime recycle remains pending",
            )
        })?;
        if let Some(exchange) = self.exchanges.get_mut(workload_id) {
            exchange.cleanup_pending = false;
        }
        Ok(())
    }

    fn recycle_workbench_runtime(&mut self, workload_id: &str) -> Result<()> {
        self.recycle_workbench_runtime_with(
            workload_id,
            |enforcer, previous, agent_name, workload_id, command| {
                enforcer.teardown_agent(&previous)?;
                let mut handle = enforcer
                    .setup_agent(agent_name, &CgroupLimits::default())
                    .with_context(|| format!("recreate workbench cgroup for {agent_name}"))?;
                let process = enforcer
                    .start_workbench_process(agent_name, Some(workload_id), command)
                    .with_context(|| format!("restart agent-runtime for {agent_name}"))?;
                handle.bwrap_pid = Some(process.pid);
                let attestation = process
                    .workbench_isolation_attestation()
                    .context("restarted workbench process lacks isolation attestation")?;
                anyhow::ensure!(
                    handle.cgroup_created
                        && process.child_pid == Some(attestation.child_pid)
                        && attestation.landlock_abi > 0,
                    "restarted workbench process isolation evidence is incomplete"
                );
                handle.landlock_applied = true;
                handle.network_isolated = true;
                Ok((handle, process))
            },
        )
    }

    fn recycle_workbench_runtime_with<Replace>(
        &mut self,
        workload_id: &str,
        replace: Replace,
    ) -> Result<()>
    where
        Replace: FnOnce(
            &SandboxEnforcer,
            SandboxHandle,
            &str,
            &str,
            &[String],
        ) -> Result<(SandboxHandle, AgentProcess)>,
    {
        let process = self.processes.get(workload_id).ok_or_else(|| {
            anyhow!("workbench runtime process is unavailable during terminal recycle")
        })?;
        let supervision = process.protocol_supervision_snapshot();
        anyhow::ensure!(
            supervision.terminal_finalized && process.owned_process_reaped(),
            "workbench runtime is not quiescent enough to recycle"
        );
        let (agent_name, command) = self
            .workloads
            .get(workload_id)
            .map(|state| (state.workload.agent_name.clone(), state.command.clone()))
            .ok_or_else(|| anyhow!("workbench workload state is unavailable during recycle"))?;
        anyhow::ensure!(
            is_workbench_agent_runtime(&command),
            "terminal protocol recycle is limited to agent-runtime workloads"
        );

        if let Some(process) = self.processes.get_mut(workload_id) {
            process.join_protocol_reader();
        }
        let previous = self
            .handles
            .get(workload_id)
            .cloned()
            .map(|previous| {
                teardown_handle_after_owned_process_reap(
                    previous,
                    true,
                    supervision.cgroup_quiesced,
                )
            })
            .ok_or_else(|| anyhow!("workbench sandbox handle is unavailable during recycle"))?;
        let (handle, process) =
            replace(&self.enforcer, previous, &agent_name, workload_id, &command)?;
        self.handles.insert(workload_id.to_string(), handle);
        self.processes.insert(workload_id.to_string(), process);
        Ok(())
    }

    fn validate_exchange_handle(&self, handle: &NanoHandle) -> Result<()> {
        if handle.runtime_key != RUNTIME_BWRAP_LANDLOCK {
            return Err(exec_error(
                NanoExecErrorCode::UnsupportedRuntime,
                false,
                "workbench execution requires the bwrap runtime",
            ));
        }
        let exchange = self.exchanges.get(&handle.workload_id).ok_or_else(|| {
            exec_error(
                NanoExecErrorCode::WorkloadUnavailable,
                false,
                "workbench exchange is unavailable",
            )
        })?;
        if exchange.instance_id != handle.instance_id {
            return Err(exec_error(
                NanoExecErrorCode::WorkloadUnavailable,
                false,
                "workbench handle does not own the retained exchange",
            ));
        }
        Ok(())
    }

    fn start_workbench_exchange(
        &mut self,
        handle: &NanoHandle,
        input: &str,
    ) -> Result<NanoExecResult> {
        if handle.runtime_key != RUNTIME_BWRAP_LANDLOCK {
            return Err(exec_error(
                NanoExecErrorCode::UnsupportedRuntime,
                false,
                "workbench execution requires the bwrap runtime",
            ));
        }
        let frame = parse_control_frame(input)?;
        if frame.get("kind").and_then(|value| value.as_str()) != Some("execute") {
            return Err(exec_error(
                NanoExecErrorCode::InvalidFrame,
                false,
                "workbench start requires an execute frame",
            ));
        }
        let request_value = frame.get("request").cloned().ok_or_else(|| {
            exec_error(
                NanoExecErrorCode::InvalidFrame,
                false,
                "workbench execute frame lacks a request object",
            )
        })?;
        let request = request_value.as_object().ok_or_else(|| {
            exec_error(
                NanoExecErrorCode::InvalidFrame,
                false,
                "workbench execute frame lacks a request object",
            )
        })?;
        require_schema_version(request.get("schema_version"))?;
        let invocation_id = request
            .get("invocation_id")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                exec_error(
                    NanoExecErrorCode::InvalidFrame,
                    false,
                    "workbench request lacks an invocation id",
                )
            })?
            .to_string();
        let deadline_unix_ms = request
            .get("deadline_unix_ms")
            .and_then(|value| value.as_u64())
            .ok_or_else(|| {
                exec_error(
                    NanoExecErrorCode::InvalidFrame,
                    false,
                    "workbench request lacks an absolute deadline",
                )
            })?;
        let request_digest = frame_digest(input);
        let input_digest = request
            .get("input_digest")
            .and_then(|value| value.as_str())
            .filter(|value| valid_sha256(value))
            .map(str::to_string)
            .unwrap_or_else(|| request_digest.clone());
        let artifact_authority = serde_json::from_value::<WorkbenchRequest>(request_value)
            .ok()
            .map(|request| WorkbenchArtifactAuthority::from_request(&handle.workload_id, &request));
        if self.exchanges.contains_key(&handle.workload_id) {
            self.validate_exchange_handle(handle)?;
            self.retry_pending_exchange_cleanup(&handle.workload_id)?;
            let supervision = self.synchronize_protocol_supervision(&handle.workload_id);
            if let Some(failure) = supervision.and_then(|snapshot| snapshot.failure) {
                let (code, message) = supervision_failure_contract(failure);
                return Err(self.fail_workbench_exchange(&handle.workload_id, code, message));
            }
            let exchange = self
                .exchanges
                .get(&handle.workload_id)
                .expect("exchange retained after cleanup retry");
            if exchange.invocation_id != invocation_id {
                if !exchange.finalized || exchange.cleanup_pending {
                    return Err(exec_error(
                        NanoExecErrorCode::InvocationConflict,
                        false,
                        "workbench workload already has another invocation",
                    ));
                }
                self.exchanges.remove(&handle.workload_id);
            } else {
                if exchange.request_digest != request_digest {
                    return Err(exec_error(
                        NanoExecErrorCode::DigestConflict,
                        false,
                        "workbench invocation request digest conflicts with retained state",
                    ));
                }
                if let Some(failure) = exchange.terminal_error.as_ref() {
                    return Err(failure.clone().into());
                }
                return workbench_exec_result(
                    handle,
                    true,
                    &invocation_id,
                    exchange_state_with_supervision(exchange, supervision),
                    exchange_messages_with_supervision(exchange, supervision),
                );
            }
        }
        if deadline_unix_ms <= unix_time_ms() {
            return Err(exec_error(
                NanoExecErrorCode::DeadlineExceeded,
                false,
                "workbench request deadline has expired",
            ));
        }
        let channel_available = self
            .processes
            .get(&handle.workload_id)
            .ok_or_else(|| {
                exec_error(
                    NanoExecErrorCode::WorkloadUnavailable,
                    false,
                    "workbench workload process is unavailable",
                )
            })?
            .protocol_channel_available();
        self.exchanges.insert(
            handle.workload_id.clone(),
            WorkbenchExchange {
                instance_id: handle.instance_id,
                invocation_id: invocation_id.clone(),
                request_digest,
                input_digest,
                artifact_authority,
                cancel_requested_at_ms: None,
                cancel_digest: None,
                deadline_cancel_digest: None,
                cancel_origin: None,
                messages: Vec::new(),
                retained_bytes: 0,
                result_seen: false,
                terminal: None,
                terminal_error: None,
                finalized: false,
                cleanup_pending: false,
            },
        );
        if !channel_available {
            return Err(self.fail_workbench_exchange(
                &handle.workload_id,
                NanoExecErrorCode::ChannelUnavailable,
                "workbench protocol channel is unavailable",
            ));
        }
        let supervision_started = self
            .processes
            .get_mut(&handle.workload_id)
            .expect("workload retained after channel validation")
            .start_protocol_supervision(&invocation_id, deadline_unix_ms, input);
        if let Err(failure) = supervision_started {
            let (code, message) = supervision_failure_contract(failure);
            return Err(self.fail_workbench_exchange(&handle.workload_id, code, message));
        }
        workbench_exec_result(handle, true, &invocation_id, "accepted", Vec::new())
    }

    fn recover_workbench_exchange(
        &mut self,
        handle: &NanoHandle,
        input: &str,
    ) -> Result<NanoExecResult> {
        if handle.runtime_key != RUNTIME_BWRAP_LANDLOCK {
            return Err(exec_error(
                NanoExecErrorCode::UnsupportedRuntime,
                false,
                "workbench recovery requires the bwrap runtime",
            ));
        }
        let frame = parse_control_frame(input)?;
        if frame.get("kind").and_then(|value| value.as_str()) != Some("recover") {
            return Err(exec_error(
                NanoExecErrorCode::InvalidFrame,
                false,
                "workbench recovery requires a recover frame",
            ));
        }
        require_schema_version(frame.get("schema_version"))?;
        let invocation_id = frame
            .get("invocation_id")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                exec_error(
                    NanoExecErrorCode::InvalidFrame,
                    false,
                    "workbench recovery frame lacks an invocation id",
                )
            })?
            .to_string();
        let input_digest = frame
            .get("input_digest")
            .and_then(|value| value.as_str())
            .filter(|value| valid_sha256(value))
            .ok_or_else(|| {
                exec_error(
                    NanoExecErrorCode::InvalidFrame,
                    false,
                    "workbench recovery frame lacks a canonical input digest",
                )
            })?
            .to_string();

        if self.exchanges.contains_key(&handle.workload_id) {
            self.validate_exchange_handle(handle)?;
            self.retry_pending_exchange_cleanup(&handle.workload_id)?;
            let supervision = self.synchronize_protocol_supervision(&handle.workload_id);
            if let Some(failure) = supervision.and_then(|snapshot| snapshot.failure) {
                let (code, message) = supervision_failure_contract(failure);
                return Err(self.fail_workbench_exchange(&handle.workload_id, code, message));
            }
            let exchange = self
                .exchanges
                .get(&handle.workload_id)
                .expect("exchange retained after recovery cleanup retry");
            if exchange.invocation_id != invocation_id {
                return Err(exec_error(
                    NanoExecErrorCode::InvocationConflict,
                    false,
                    "workbench recovery invocation conflicts with retained state",
                ));
            }
            if exchange.input_digest != input_digest {
                return Err(exec_error(
                    NanoExecErrorCode::DigestConflict,
                    false,
                    "workbench recovery digest conflicts with retained state",
                ));
            }
            if let Some(failure) = exchange.terminal_error.as_ref() {
                return Err(failure.clone().into());
            }
            return workbench_exec_result(
                handle,
                true,
                &invocation_id,
                exchange_state_with_supervision(exchange, supervision),
                exchange_messages_with_supervision(exchange, supervision),
            );
        }

        let channel_available = self
            .processes
            .get(&handle.workload_id)
            .ok_or_else(|| {
                exec_error(
                    NanoExecErrorCode::WorkloadUnavailable,
                    false,
                    "workbench workload process is unavailable",
                )
            })?
            .protocol_channel_available();
        self.exchanges.insert(
            handle.workload_id.clone(),
            WorkbenchExchange {
                instance_id: handle.instance_id,
                invocation_id: invocation_id.clone(),
                request_digest: input_digest.clone(),
                input_digest,
                artifact_authority: None,
                cancel_requested_at_ms: None,
                cancel_digest: None,
                deadline_cancel_digest: None,
                cancel_origin: None,
                messages: Vec::new(),
                retained_bytes: 0,
                result_seen: false,
                terminal: None,
                terminal_error: None,
                finalized: false,
                cleanup_pending: false,
            },
        );
        if !channel_available {
            return Err(self.fail_workbench_exchange(
                &handle.workload_id,
                NanoExecErrorCode::ChannelUnavailable,
                "workbench protocol channel is unavailable",
            ));
        }
        let deadline_unix_ms = unix_time_ms().saturating_add(WORKBENCH_RECOVERY_DEADLINE_MS);
        let supervision_started = self
            .processes
            .get_mut(&handle.workload_id)
            .expect("workload retained after recovery channel validation")
            .start_protocol_supervision(&invocation_id, deadline_unix_ms, input);
        if let Err(failure) = supervision_started {
            let (code, message) = supervision_failure_contract(failure);
            return Err(self.fail_workbench_exchange(&handle.workload_id, code, message));
        }
        workbench_exec_result(handle, true, &invocation_id, "accepted", Vec::new())
    }

    fn cancel_workbench_exchange(
        &mut self,
        handle: &NanoHandle,
        input: &str,
    ) -> Result<NanoExecResult> {
        let frame = parse_control_frame(input)?;
        if frame.get("kind").and_then(|value| value.as_str()) != Some("cancel") {
            return Err(exec_error(
                NanoExecErrorCode::InvalidFrame,
                false,
                "workbench cancel requires a cancel frame",
            ));
        }
        require_schema_version(frame.get("schema_version"))?;
        let invocation_id = frame
            .get("invocation_id")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                exec_error(
                    NanoExecErrorCode::InvalidFrame,
                    false,
                    "workbench cancel frame lacks an invocation id",
                )
            })?;
        let cancel_digest = frame_digest(input);
        self.validate_exchange_handle(handle)?;
        self.retry_pending_exchange_cleanup(&handle.workload_id)?;
        let supervision = self.synchronize_protocol_supervision(&handle.workload_id);
        if let Some(failure) = supervision.and_then(|snapshot| snapshot.failure) {
            let (code, message) = supervision_failure_contract(failure);
            return Err(self.fail_workbench_exchange(&handle.workload_id, code, message));
        }
        let exchange = self.exchanges.get(&handle.workload_id).ok_or_else(|| {
            exec_error(
                NanoExecErrorCode::WorkloadUnavailable,
                false,
                "workbench exchange is unavailable",
            )
        })?;
        if exchange.invocation_id != invocation_id {
            return Err(exec_error(
                NanoExecErrorCode::InvocationConflict,
                false,
                "workbench cancel invocation conflicts with retained state",
            ));
        }
        if let Some(existing_digest) = exchange.cancel_digest.as_deref() {
            if existing_digest != cancel_digest {
                return Err(exec_error(
                    NanoExecErrorCode::DigestConflict,
                    false,
                    "workbench cancel digest conflicts with retained state",
                ));
            }
            if let Some(failure) = exchange.terminal_error.as_ref() {
                return Err(failure.clone().into());
            }
            return workbench_exec_result(
                handle,
                true,
                invocation_id,
                exchange_state_with_supervision(exchange, supervision),
                exchange_messages_with_supervision(exchange, supervision),
            );
        }
        if let Some(failure) = exchange.terminal_error.clone() {
            return Err(failure.into());
        }
        if exchange.terminal.is_some() {
            return workbench_exec_result(
                handle,
                true,
                invocation_id,
                exchange_state_with_supervision(exchange, supervision),
                exchange_messages_with_supervision(exchange, supervision),
            );
        }
        let channel_available = self
            .processes
            .get(&handle.workload_id)
            .is_some_and(AgentProcess::protocol_channel_available);
        if !channel_available {
            return Err(self.fail_workbench_exchange(
                &handle.workload_id,
                NanoExecErrorCode::ChannelUnavailable,
                "workbench protocol channel is unavailable",
            ));
        }
        let requested_at_ms = unix_time_ms();
        let cancel_owner = self
            .processes
            .get(&handle.workload_id)
            .expect("workload retained after channel validation")
            .begin_explicit_protocol_cancel(requested_at_ms);
        let snapshot = self
            .synchronize_protocol_supervision(&handle.workload_id)
            .expect("workload retained after cancellation claim");
        let Some(cancel_owner) = cancel_owner else {
            if let Some(failure) = snapshot.failure {
                let (code, message) = supervision_failure_contract(failure);
                return Err(self.fail_workbench_exchange(&handle.workload_id, code, message));
            }
            let exchange = self
                .exchanges
                .get(&handle.workload_id)
                .expect("terminalizing exchange retained after rejected cancel claim");
            return workbench_exec_result(
                handle,
                true,
                invocation_id,
                exchange_state_with_supervision(exchange, Some(snapshot)),
                exchange_messages_with_supervision(exchange, Some(snapshot)),
            );
        };
        {
            let exchange = self
                .exchanges
                .get_mut(&handle.workload_id)
                .expect("exchange retained after cancellation claim");
            exchange.cancel_digest = Some(cancel_digest);
            exchange.cancel_requested_at_ms = snapshot.cancel_requested_at_ms;
            exchange.cancel_origin = Some(match cancel_owner {
                ProtocolCancelOwner::Explicit => WorkbenchCancelOrigin::Explicit,
                ProtocolCancelOwner::Deadline => WorkbenchCancelOrigin::Deadline,
            });
        }
        if cancel_owner == ProtocolCancelOwner::Deadline {
            // The autonomous deadline path owns the one child cancellation
            // frame, including the interval between claim and completed send.
            let exchange = self
                .exchanges
                .get(&handle.workload_id)
                .expect("deadline-owned cancellation retained");
            return workbench_exec_result(
                handle,
                true,
                invocation_id,
                exchange_state_with_supervision(exchange, Some(snapshot)),
                exchange_messages_with_supervision(exchange, Some(snapshot)),
            );
        }
        let send_result = self
            .processes
            .get_mut(&handle.workload_id)
            .expect("workload retained while cancellation is sent")
            .send_protocol_line(input);
        if send_result.is_err() {
            self.processes
                .get(&handle.workload_id)
                .expect("workload retained after cancellation send failure")
                .mark_protocol_channel_disconnected();
            return Err(self.fail_workbench_exchange(
                &handle.workload_id,
                NanoExecErrorCode::ChannelDisconnected,
                "workbench cancellation channel disconnected",
            ));
        }
        #[cfg(test)]
        {
            self.processes
                .get(&handle.workload_id)
                .expect("workload retained after cancellation send")
                .mark_protocol_cancel_sent();
        }
        let exchange = self
            .exchanges
            .get(&handle.workload_id)
            .expect("exchange retained while cancellation is sent");
        workbench_exec_result(
            handle,
            true,
            invocation_id,
            "cancelling",
            exchange_messages_with_supervision(exchange, Some(snapshot)),
        )
    }

    fn poll_workbench_exchange(
        &mut self,
        handle: &NanoHandle,
        input: &str,
    ) -> Result<NanoExecResult> {
        let frame = parse_control_frame(input)?;
        if frame.get("kind").and_then(|value| value.as_str()) != Some("poll") {
            return Err(exec_error(
                NanoExecErrorCode::InvalidFrame,
                false,
                "workbench poll requires a poll frame",
            ));
        }
        require_schema_version(frame.get("schema_version"))?;
        let invocation_id = frame
            .get("invocation_id")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                exec_error(
                    NanoExecErrorCode::InvalidFrame,
                    false,
                    "workbench poll frame lacks an invocation id",
                )
            })?;
        self.validate_exchange_handle(handle)?;
        self.retry_pending_exchange_cleanup(&handle.workload_id)?;
        let exchange = self.exchanges.get(&handle.workload_id).ok_or_else(|| {
            exec_error(
                NanoExecErrorCode::WorkloadUnavailable,
                false,
                "workbench exchange is unavailable",
            )
        })?;
        if exchange.invocation_id != invocation_id {
            return Err(exec_error(
                NanoExecErrorCode::InvocationConflict,
                false,
                "workbench poll invocation conflicts with retained state",
            ));
        }
        if let Some(failure) = exchange.terminal_error.as_ref() {
            return Err(failure.clone().into());
        }
        let terminal_messages = exchange
            .terminal
            .is_some()
            .then(|| exchange.messages.clone());
        let channel_status = self
            .processes
            .get(&handle.workload_id)
            .map(AgentProcess::protocol_channel_available);
        match channel_status {
            None if terminal_messages.is_some() => {
                return workbench_exec_result(
                    handle,
                    true,
                    invocation_id,
                    "completed",
                    terminal_messages.expect("terminal messages were present"),
                );
            }
            None => {
                return Err(self.fail_workbench_exchange(
                    &handle.workload_id,
                    NanoExecErrorCode::ChannelDisconnected,
                    "workbench workload process disappeared before a terminal frame",
                ));
            }
            Some(false) | Some(true) => {}
        }
        let supervision = self.synchronize_protocol_supervision(&handle.workload_id);
        if let Some(failure) = supervision.and_then(|snapshot| snapshot.failure) {
            let (code, message) = supervision_failure_contract(failure);
            return Err(self.fail_workbench_exchange(&handle.workload_id, code, message));
        }
        let drain = match self
            .processes
            .get_mut(&handle.workload_id)
            .expect("workload retained after channel validation")
            .drain_protocol_lines()
        {
            Ok(drain) => drain,
            Err(_) => {
                let supervision = self.synchronize_protocol_supervision(&handle.workload_id);
                if let Some(failure) = supervision.and_then(|snapshot| snapshot.failure) {
                    let (code, message) = supervision_failure_contract(failure);
                    return Err(self.fail_workbench_exchange(&handle.workload_id, code, message));
                }
                let exchange = self
                    .exchanges
                    .get(&handle.workload_id)
                    .expect("exchange retained while autonomous cleanup completes");
                return workbench_exec_result(
                    handle,
                    true,
                    invocation_id,
                    exchange_state_with_supervision(exchange, supervision),
                    exchange_messages_with_supervision(exchange, supervision),
                );
            }
        };
        // A reader only publishes a line after committing its shared
        // supervision state. Synchronizing after the drain therefore imports
        // autonomous deadline ownership before adapter-level `cancelled`
        // validation, while a winning supervision failure retains priority.
        let supervision = self.synchronize_protocol_supervision(&handle.workload_id);
        if let Some(failure) = supervision.and_then(|snapshot| snapshot.failure) {
            let (code, message) = supervision_failure_contract(failure);
            return Err(self.fail_workbench_exchange(&handle.workload_id, code, message));
        }
        if drain.queue_overflowed {
            let exchange = self
                .exchanges
                .get(&handle.workload_id)
                .expect("exchange retained while output cleanup completes");
            return workbench_exec_result(
                handle,
                true,
                invocation_id,
                exchange_state_with_supervision(exchange, supervision),
                exchange_messages_with_supervision(exchange, supervision),
            );
        }
        let mut violation = None;
        let host_agent_root = self.workbench_host_root(&handle.workload_id)?;
        let artifact_authority = self
            .exchanges
            .get(&handle.workload_id)
            .expect("exchange validated before output binding")
            .artifact_authority
            .clone();
        {
            let exchange = self
                .exchanges
                .get_mut(&handle.workload_id)
                .expect("exchange validated before draining protocol output");
            for line in drain.lines {
                if exchange.terminal.is_some() {
                    violation = Some(NanoExecErrorCode::ProtocolViolation);
                    break;
                }
                exchange.retained_bytes = exchange.retained_bytes.saturating_add(line.len());
                if exchange.retained_bytes > MAX_WORKBENCH_OUTPUT_BYTES {
                    violation = Some(NanoExecErrorCode::OutputLimitExceeded);
                    break;
                }
                let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
                    violation = Some(NanoExecErrorCode::InvalidFrame);
                    break;
                };
                let Ok(bound_message) = serde_json::from_value::<WorkbenchMessage>(message.clone())
                else {
                    violation = Some(NanoExecErrorCode::InvalidFrame);
                    break;
                };
                if message
                    .get("schema_version")
                    .and_then(|value| value.as_u64())
                    != Some(1)
                {
                    violation = Some(NanoExecErrorCode::UnsupportedVersion);
                    break;
                }
                let message_invocation = message
                    .get("invocation_id")
                    .and_then(|value| value.as_str());
                if message_invocation != Some(exchange.invocation_id.as_str()) {
                    violation = Some(NanoExecErrorCode::InvocationConflict);
                    break;
                }
                match message.get("kind").and_then(|value| value.as_str()) {
                    Some("result") => {
                        if exchange.result_seen || exchange.terminal.is_some() {
                            violation = Some(NanoExecErrorCode::ProtocolViolation);
                            break;
                        }
                        if artifact_authority.as_ref().is_some_and(|authority| {
                            validate_terminal_artifacts(&host_agent_root, authority, &bound_message)
                                .is_err()
                        }) {
                            violation = Some(NanoExecErrorCode::ProtocolViolation);
                            break;
                        }
                        exchange.result_seen = true;
                    }
                    Some("progress")
                        if message.get("stage").and_then(|value| value.as_str())
                            == Some("completed") =>
                    {
                        if !exchange.result_seen || exchange.terminal.is_some() {
                            violation = Some(NanoExecErrorCode::ProtocolViolation);
                            break;
                        }
                        exchange.terminal = Some(WorkbenchTerminal::Succeeded);
                    }
                    Some("progress") => {
                        if exchange.result_seen {
                            violation = Some(NanoExecErrorCode::ProtocolViolation);
                            break;
                        }
                    }
                    Some("error") => {
                        if exchange.result_seen || exchange.terminal.is_some() {
                            violation = Some(NanoExecErrorCode::ProtocolViolation);
                            break;
                        }
                        exchange.terminal = Some(WorkbenchTerminal::Failed);
                    }
                    Some("cancelled") => {
                        if exchange.cancel_requested_at_ms.is_none()
                            || exchange.result_seen
                            || exchange.terminal.is_some()
                        {
                            violation = Some(NanoExecErrorCode::ProtocolViolation);
                            break;
                        }
                        exchange.terminal = Some(WorkbenchTerminal::Cancelled);
                    }
                    _ => {
                        violation = Some(NanoExecErrorCode::ProtocolViolation);
                        break;
                    }
                }
                exchange.messages.push(message);
            }
        }
        if let Some(violation) = violation {
            let supervision = self.synchronize_protocol_supervision(&handle.workload_id);
            if let Some(failure) = supervision.and_then(|snapshot| snapshot.failure) {
                let (code, message) = supervision_failure_contract(failure);
                return Err(self.fail_workbench_exchange(&handle.workload_id, code, message));
            }
            return Err(self.fail_workbench_exchange(
                &handle.workload_id,
                violation,
                "workbench output failed its protocol or artifact binding",
            ));
        }
        let supervision = self.synchronize_protocol_supervision(&handle.workload_id);
        let exchange_is_terminal = self
            .exchanges
            .get(&handle.workload_id)
            .is_some_and(|exchange| exchange.terminal.is_some());
        if let Some(failure) = supervision.and_then(|snapshot| snapshot.failure) {
            let (code, message) = supervision_failure_contract(failure);
            return Err(self.fail_workbench_exchange(&handle.workload_id, code, message));
        }
        let terminal_finalized = supervision.is_some_and(|snapshot| snapshot.terminal_finalized);
        if exchange_is_terminal && terminal_finalized {
            let exchange = self
                .exchanges
                .get_mut(&handle.workload_id)
                .expect("terminal exchange retained before cleanup");
            exchange.finalized = true;
            exchange.cleanup_pending = true;
            self.retry_pending_exchange_cleanup(&handle.workload_id)?;
        }
        let exchange = self
            .exchanges
            .get(&handle.workload_id)
            .expect("exchange retained after a valid poll");
        workbench_exec_result(
            handle,
            true,
            invocation_id,
            exchange_state_with_supervision(exchange, supervision),
            exchange_messages_with_supervision(exchange, supervision),
        )
    }
}

fn teardown_handle_after_owned_process_reap(
    mut handle: SandboxHandle,
    owned_process_reaped: bool,
    cgroup_quiesced: bool,
) -> SandboxHandle {
    if owned_process_reaped {
        // The owned Child has already been waited. Never signal its numeric PID
        // again: it may have been reused before cgroup cleanup begins.
        handle.bwrap_pid = None;
    }
    if cgroup_quiesced {
        handle.cgroup_created = false;
    }
    handle
}

fn exchange_state_with_supervision(
    exchange: &WorkbenchExchange,
    supervision: Option<ProtocolSupervisionSnapshot>,
) -> &'static str {
    if exchange.terminal.is_some() {
        if exchange.finalized || supervision.is_some_and(|snapshot| snapshot.terminal_finalized) {
            "completed"
        } else if exchange.cancel_requested_at_ms.is_some() {
            "cancelling"
        } else {
            "pending"
        }
    } else if exchange.cancel_requested_at_ms.is_some() {
        "cancelling"
    } else {
        "pending"
    }
}

fn exchange_messages_with_supervision(
    exchange: &WorkbenchExchange,
    supervision: Option<ProtocolSupervisionSnapshot>,
) -> Vec<serde_json::Value> {
    if exchange.terminal.is_some()
        && !exchange.finalized
        && !supervision.is_some_and(|snapshot| snapshot.terminal_finalized)
    {
        // A terminal child frame is provisional until the autonomous owner has
        // closed the reader and quiesced the process tree. Do not expose the
        // result/completed pair before the post-terminal validation window.
        Vec::new()
    } else {
        exchange.messages.clone()
    }
}

fn deadline_cancel_frame(invocation_id: &str) -> String {
    serde_json::json!({
        "kind": "cancel",
        "schema_version": 1,
        "invocation_id": invocation_id,
        "reason": "deadline_expired"
    })
    .to_string()
}

fn supervision_failure_contract(
    failure: ProtocolSupervisionFailure,
) -> (NanoExecErrorCode, &'static str) {
    match failure {
        ProtocolSupervisionFailure::InvalidFrame => (
            NanoExecErrorCode::InvalidFrame,
            "workbench child emitted an invalid JSON frame",
        ),
        ProtocolSupervisionFailure::ProtocolViolation => (
            NanoExecErrorCode::ProtocolViolation,
            "workbench child violated the protocol state machine",
        ),
        ProtocolSupervisionFailure::UnsupportedVersion => (
            NanoExecErrorCode::UnsupportedVersion,
            "workbench child emitted an unsupported protocol version",
        ),
        ProtocolSupervisionFailure::InvocationConflict => (
            NanoExecErrorCode::InvocationConflict,
            "workbench child emitted a foreign invocation id",
        ),
        ProtocolSupervisionFailure::OutputLimitExceeded => (
            NanoExecErrorCode::OutputLimitExceeded,
            "workbench output exceeded the configured limit",
        ),
        ProtocolSupervisionFailure::ChannelDisconnected => (
            NanoExecErrorCode::ChannelDisconnected,
            "workbench output channel disconnected before a terminal frame",
        ),
        ProtocolSupervisionFailure::DeadlineExceeded => (
            NanoExecErrorCode::DeadlineExceeded,
            "workbench deadline expired without a terminal acknowledgement",
        ),
        ProtocolSupervisionFailure::Cancelled => (
            NanoExecErrorCode::Cancelled,
            "workbench cancellation was not acknowledged within its grace period",
        ),
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn workbench_exec_result(
    handle: &NanoHandle,
    success: bool,
    invocation_id: &str,
    state: &str,
    messages: Vec<serde_json::Value>,
) -> Result<NanoExecResult> {
    Ok(NanoExecResult {
        runtime_key: handle.runtime_key.clone(),
        workload_id: handle.workload_id.clone(),
        success,
        output: serde_json::to_string(&serde_json::json!({
            "schema_version": 1,
            "invocation_id": invocation_id,
            "state": state,
            "messages": messages,
        }))?,
    })
}

impl Default for BwrapNanoRuntime {
    fn default() -> Self {
        Self::detect()
    }
}

impl Drop for BwrapNanoRuntime {
    fn drop(&mut self) {
        let mut ids: Vec<String> = self
            .processes
            .keys()
            .chain(self.handles.keys())
            .chain(self.workloads.keys())
            .chain(self.pending_spawns.keys())
            .cloned()
            .collect();
        ids.sort();
        ids.dedup();
        for id in ids {
            let _ = self.rollback_pending_spawn(&id);
            let _ = self.teardown_workload(&id);
        }
    }
}

impl NanoRuntime for BwrapNanoRuntime {
    fn runtime_key(&self) -> &'static str {
        RUNTIME_BWRAP_LANDLOCK
    }

    fn spawn(&mut self, workload: NanoWorkloadSpec) -> Result<NanoHandle> {
        if workload.agent_name.is_empty() {
            return Err(anyhow!("bwrap workload requires agent_name"));
        }
        if self.pending_spawns.contains_key(&workload.workload_id) {
            self.rollback_pending_spawn(&workload.workload_id)
                .with_context(|| {
                    format!(
                        "recover previous bwrap spawn for '{}'",
                        workload.workload_id
                    )
                })?;
        }
        self.ensure_workload_available(&workload)?;
        self.reconcile_durable_spawn_marker(&workload.agent_name, &workload.workload_id)?;
        let state = BwrapWorkloadState {
            instance_id: uuid::Uuid::new_v4(),
            command: Self::command_for(&workload),
            workload,
            owned_object_ids: Vec::new(),
            suspended: false,
        };
        self.spawn_state(state)
    }

    fn reconcile_abandoned(&mut self, workload: &NanoWorkloadSpec) -> Result<NanoRecoveryResult> {
        let selected = workload
            .runtime_key
            .as_deref()
            .unwrap_or(RUNTIME_BWRAP_LANDLOCK);
        anyhow::ensure!(
            selected == RUNTIME_BWRAP_LANDLOCK,
            "bwrap cannot reconcile workload '{}' for runtime '{}'",
            workload.workload_id,
            selected
        );
        anyhow::ensure!(
            !self.workloads.contains_key(&workload.workload_id)
                && !self.handles.contains_key(&workload.workload_id)
                && !self.processes.contains_key(&workload.workload_id),
            "bwrap workload '{}' is active in this adapter instance",
            workload.workload_id
        );
        let rolled_back = self.rollback_pending_spawn(&workload.workload_id)?;
        let marker_reconciled =
            self.reconcile_durable_spawn_marker(&workload.agent_name, &workload.workload_id)?;
        Ok(NanoRecoveryResult {
            runtime_key: self.runtime_key().to_string(),
            workload_id: workload.workload_id.clone(),
            cleaned: rolled_back || marker_reconciled,
            detail: "durable bwrap marker and partial sandbox state reconciled".to_string(),
        })
    }

    fn stop(&mut self, handle: &NanoHandle) -> Result<NanoStopResult> {
        ensure_handle_runtime(handle, self.runtime_key())?;
        if let Some(state) = self.workloads.get(&handle.workload_id) {
            ensure_handle_instance(handle, state.instance_id)?;
        } else if let Some(exchange) = self.exchanges.get(&handle.workload_id) {
            ensure_handle_instance(handle, exchange.instance_id)?;
        }
        Ok(NanoStopResult::new(
            self.runtime_key(),
            &handle.workload_id,
            self.teardown_workload(&handle.workload_id)?,
        ))
    }

    fn resources(&self, handle: &NanoHandle) -> Result<NanoRuntimeResources> {
        ensure_handle_runtime(handle, self.runtime_key())?;
        let state = self
            .workloads
            .get(&handle.workload_id)
            .ok_or_else(|| anyhow!("missing bwrap workload '{}'", handle.workload_id))?;
        ensure_handle_instance(handle, state.instance_id)?;
        let sandbox = self
            .handles
            .get(&handle.workload_id)
            .ok_or_else(|| anyhow!("missing bwrap sandbox handle '{}'", handle.workload_id))?;
        let process = self
            .processes
            .get(&handle.workload_id)
            .ok_or_else(|| anyhow!("missing bwrap process '{}'", handle.workload_id))?;
        Ok(NanoRuntimeResources {
            instance_id: Some(state.instance_id),
            pid: Some(process.pid),
            child_pid: process.child_pid,
            cgroup_created: sandbox.cgroup_created,
            cgroup_id: sandbox.cgroup_id,
            io_available: sandbox.io_available,
            landlock_applied: sandbox.landlock_applied,
            network_isolated: sandbox.network_isolated,
        })
    }

    fn exec(&mut self, handle: &NanoHandle, request: NanoExecRequest) -> Result<NanoExecResult> {
        match request.operation.as_str() {
            "health" => {
                let health = self.health(handle)?;
                Ok(NanoExecResult {
                    runtime_key: self.runtime_key().to_string(),
                    workload_id: handle.workload_id.clone(),
                    success: true,
                    output: format!("{:?}", health.state),
                })
            }
            "workbench_start" => {
                if !self.exchanges.contains_key(&handle.workload_id)
                    && self.health(handle)?.state != NanoHealthState::Healthy
                {
                    return Err(exec_error(
                        NanoExecErrorCode::WorkloadUnavailable,
                        false,
                        "workbench workload is not healthy",
                    ));
                }
                self.start_workbench_exchange(handle, &request.input)
            }
            "workbench_poll" => self.poll_workbench_exchange(handle, &request.input),
            "workbench_cancel" => self.cancel_workbench_exchange(handle, &request.input),
            "workbench_recover" => {
                if !self.exchanges.contains_key(&handle.workload_id)
                    && self.health(handle)?.state != NanoHealthState::Healthy
                {
                    return Err(exec_error(
                        NanoExecErrorCode::WorkloadUnavailable,
                        false,
                        "workbench workload is not healthy",
                    ));
                }
                self.recover_workbench_exchange(handle, &request.input)
            }
            _ => Err(exec_error(
                NanoExecErrorCode::UnsupportedOperation,
                false,
                "bwrap exec operation is not supported",
            )),
        }
    }

    fn snapshot(&mut self, handle: &NanoHandle) -> Result<NanoSnapshot> {
        ensure_handle_runtime(handle, self.runtime_key())?;
        // Clone the bits we need so `self` can be re-borrowed mutably below.
        let (workload, command, prev_owned) = {
            let state = self
                .workloads
                .get(&handle.workload_id)
                .ok_or_else(|| anyhow!("unknown bwrap workload '{}'", handle.workload_id))?;
            ensure_handle_instance(handle, state.instance_id)?;
            (
                state.workload.clone(),
                state.command.clone(),
                state.owned_object_ids.clone(),
            )
        };

        if !self.cas_manifest_enabled {
            return Ok(NanoSnapshot {
                runtime_key: self.runtime_key().to_string(),
                workload_id: handle.workload_id.clone(),
                agent_id: handle.agent_id,
                semantics: NanoSnapshotSemantics::BwrapRecreate,
                payload: serde_json::to_value(BwrapRecreateSnapshotPayload {
                    workload,
                    command,
                    semantics_note: "bwrap compatibility snapshot recreates a fresh runtime from the bound workload specification; it contains no process RAM, CRIU state, or filesystem manifest".to_string(),
                })?,
            });
        }

        let (cgroup_created, io_available) = {
            let sandbox_handle = self
                .handles
                .get(&handle.workload_id)
                .ok_or_else(|| anyhow!("missing bwrap sandbox handle '{}'", handle.workload_id))?;
            (sandbox_handle.cgroup_created, sandbox_handle.io_available)
        };

        // Walk the agent home into a metadata-aware CAS manifest (no file bytes).
        let home = self.home_dir(&workload.agent_name);
        let plane = self.open_plane()?;
        // Release the previous snapshot's pinned objects before re-walking.
        home_manifest::release_manifest(&plane, &prev_owned)?;
        let walked = home_manifest::walk_home(&home, &plane)?;
        if let Some(state) = self.workloads.get_mut(&handle.workload_id) {
            state.owned_object_ids = walked.owned_object_ids;
        }

        let payload = BwrapSnapshotPayload {
            workload,
            command,
            home_manifest: walked.manifest,
            cgroup_created,
            io_available,
            bwrap_available: self.enforcer.has_bwrap(),
            landlock_available: self.enforcer.has_landlock(),
            semantics_note: "bwrap snapshot is a metadata-aware CAS manifest of the agent-home filesystem; no process RAM or CRIU checkpoint".to_string(),
        };

        Ok(NanoSnapshot {
            runtime_key: self.runtime_key().to_string(),
            workload_id: handle.workload_id.clone(),
            agent_id: handle.agent_id,
            semantics: NanoSnapshotSemantics::BwrapConfigFs,
            payload: serde_json::to_value(payload)?,
        })
    }

    fn restore(&mut self, snapshot: NanoSnapshot) -> Result<NanoHandle> {
        if snapshot.runtime_key != self.runtime_key() {
            return Err(anyhow!(
                "cannot restore {} snapshot into {} runtime",
                snapshot.runtime_key,
                self.runtime_key()
            ));
        }
        match snapshot.semantics {
            NanoSnapshotSemantics::BwrapRecreate => {
                let payload: BwrapRecreateSnapshotPayload =
                    serde_json::from_value(snapshot.payload)?;
                self.rollback_pending_spawn(&snapshot.workload_id)?;
                self.ensure_restore_target_available(&snapshot.workload_id, &payload.workload)?;
                if !self.workloads.contains_key(&snapshot.workload_id)
                    && !self.handles.contains_key(&snapshot.workload_id)
                    && !self.processes.contains_key(&snapshot.workload_id)
                {
                    self.reconcile_durable_spawn_marker(
                        &payload.workload.agent_name,
                        &snapshot.workload_id,
                    )?;
                }
                self.teardown_workload(&snapshot.workload_id)?;
                self.spawn_state(BwrapWorkloadState {
                    instance_id: uuid::Uuid::new_v4(),
                    workload: payload.workload,
                    command: payload.command,
                    owned_object_ids: Vec::new(),
                    suspended: false,
                })
            }
            NanoSnapshotSemantics::BwrapConfigFs => {
                anyhow::ensure!(
                    self.cas_manifest_enabled,
                    "bwrap CAS-manifest restore is disabled until #548 is enabled"
                );
                let payload: BwrapSnapshotPayload = serde_json::from_value(snapshot.payload)?;
                self.rollback_pending_spawn(&snapshot.workload_id)?;
                self.ensure_restore_target_available(&snapshot.workload_id, &payload.workload)?;
                if !self.workloads.contains_key(&snapshot.workload_id)
                    && !self.handles.contains_key(&snapshot.workload_id)
                    && !self.processes.contains_key(&snapshot.workload_id)
                {
                    self.reconcile_durable_spawn_marker(
                        &payload.workload.agent_name,
                        &snapshot.workload_id,
                    )?;
                }
                self.teardown_workload(&snapshot.workload_id)?;

                // #548 path: rehydrate the metadata-aware home manifest. The
                // feature remains default-off until durable retained ownership
                // and GC-safe pin transfer are complete.
                let home = self.home_dir(&payload.workload.agent_name);
                if home.exists() {
                    std::fs::remove_dir_all(&home)
                        .with_context(|| format!("reset agent home dir {}", home.display()))?;
                }
                let plane = self.open_plane()?;
                home_manifest::rehydrate(
                    &payload.home_manifest,
                    &home,
                    &plane,
                    &RestorePolicy::default(),
                )?;

                self.spawn_state(BwrapWorkloadState {
                    instance_id: uuid::Uuid::new_v4(),
                    workload: payload.workload,
                    command: payload.command,
                    owned_object_ids: Vec::new(),
                    suspended: false,
                })
            }
            semantics => Err(anyhow!(
                "bwrap restore requires BwrapRecreate or BwrapConfigFs snapshot, got {semantics:?}"
            )),
        }
    }

    fn health(&mut self, handle: &NanoHandle) -> Result<NanoHealth> {
        ensure_handle_runtime(handle, self.runtime_key())?;
        if let Some(state) = self.workloads.get(&handle.workload_id) {
            ensure_handle_instance(handle, state.instance_id)?;
        }
        let state = if self
            .workloads
            .get(&handle.workload_id)
            .is_some_and(|state| state.suspended)
        {
            NanoHealthState::Degraded
        } else if let Some(process) = self.processes.get_mut(&handle.workload_id) {
            if process.is_running() {
                NanoHealthState::Healthy
            } else {
                NanoHealthState::Stopped
            }
        } else if let Some(pid) = handle.pid {
            let cgroup_name = self
                .workloads
                .get(&handle.workload_id)
                .map(|state| state.workload.agent_name.as_str())
                .unwrap_or(handle.workload_id.as_str());
            let cgroup = cgroups::list_pids_in_cgroup(cgroup_name).unwrap_or_default();
            if cgroup.contains(&pid) {
                NanoHealthState::Degraded
            } else {
                NanoHealthState::Stopped
            }
        } else {
            NanoHealthState::Stopped
        };
        Ok(NanoHealth {
            runtime_key: self.runtime_key().to_string(),
            workload_id: handle.workload_id.clone(),
            state,
            detail: "bwrap process plus cgroup/Landlock sandbox state".to_string(),
        })
    }

    fn isolate(
        &mut self,
        handle: &NanoHandle,
        policy: NanoIsolationPolicy,
    ) -> Result<NanoIsolationReport> {
        ensure_handle_runtime(handle, self.runtime_key())?;
        if let Some(state) = self.workloads.get(&handle.workload_id) {
            ensure_handle_instance(handle, state.instance_id)?;
        }
        let applied = self.handles.contains_key(&handle.workload_id);
        Ok(NanoIsolationReport {
            runtime_key: self.runtime_key().to_string(),
            workload_id: handle.workload_id.clone(),
            applied,
            detail: format!(
                "bwrap={} cgroups={} landlock={} network={}",
                self.enforcer.has_bwrap(),
                self.enforcer.has_cgroups() && policy.cgroups,
                self.enforcer.has_landlock() && policy.landlock,
                // #75: network isolation = full cage from bwrap --unshare-all.
                self.enforcer.has_bwrap() && policy.network
            ),
        })
    }

    fn control(
        &mut self,
        handle: &NanoHandle,
        action: NanoRuntimeControlAction,
    ) -> Result<NanoRuntimeControlResult> {
        self.resources(handle)?;
        let suspended = self
            .workloads
            .get(&handle.workload_id)
            .map(|state| state.suspended)
            .ok_or_else(|| anyhow!("unknown bwrap workload '{}'", handle.workload_id))?;
        let should_apply = match action {
            NanoRuntimeControlAction::Suspend => !suspended,
            NanoRuntimeControlAction::Resume => suspended,
        };
        let affected_units = if should_apply {
            let signal = match action {
                NanoRuntimeControlAction::Suspend => nix::sys::signal::Signal::SIGSTOP,
                NanoRuntimeControlAction::Resume => nix::sys::signal::Signal::SIGCONT,
            };
            let affected = self.signal_workload(&handle.workload_id, signal)?;
            if let Some(state) = self.workloads.get_mut(&handle.workload_id) {
                state.suspended = matches!(action, NanoRuntimeControlAction::Suspend);
            }
            affected
        } else {
            0
        };
        Ok(NanoRuntimeControlResult::new(
            self.runtime_key(),
            &handle.workload_id,
            action,
            should_apply,
            affected_units,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_workload(workload_id: &str, agent_name: &str) -> NanoWorkloadSpec {
        NanoWorkloadSpec {
            workload_id: workload_id.to_string(),
            runtime_key: Some(RUNTIME_BWRAP_LANDLOCK.to_string()),
            agent_id: None,
            agent_name: agent_name.to_string(),
            role: "Tester".to_string(),
            room_id: "empfang".to_string(),
            shift_set: 1,
            command: Vec::new(),
            capabilities: Vec::new(),
            metadata: Default::default(),
            ecs_snapshot: None,
        }
    }

    #[test]
    fn workbench_artifacts_use_writable_agent_backing_with_active_fs_mount() {
        let temp = tempfile::tempdir().unwrap();
        let homes = temp.path().join("homes");
        let mut runtime = BwrapNanoRuntime::with_test_dirs(temp.path().join("cas"), &homes);
        runtime.set_fs_mount(temp.path().join("sentinel-fs").to_string_lossy());
        let workload = fixture_workload("AGENT-01", "alice");
        runtime.workloads.insert(
            workload.workload_id.clone(),
            BwrapWorkloadState {
                instance_id: uuid::Uuid::new_v4(),
                command: workload.command.clone(),
                workload,
                owned_object_ids: Vec::new(),
                suspended: false,
            },
        );

        assert_eq!(
            runtime.workbench_host_root("AGENT-01").unwrap(),
            homes.join("alice")
        );
    }

    fn insert_fixture(
        runtime: &mut BwrapNanoRuntime,
        workload: NanoWorkloadSpec,
        owned_object_ids: Vec<u64>,
    ) -> NanoHandle {
        let workload_id = workload.workload_id.clone();
        let agent_name = workload.agent_name.clone();
        let command = workload.command.clone();
        let process = AgentProcess::launch_fixture().unwrap();
        let pid = process.pid;
        runtime.processes.insert(workload_id.clone(), process);
        runtime.handles.insert(
            workload_id.clone(),
            SandboxHandle {
                agent_name,
                cgroup_created: false,
                cgroup_id: None,
                io_available: false,
                bwrap_pid: Some(pid),
                landlock_applied: false,
                network_isolated: false,
            },
        );
        runtime.workloads.insert(
            workload_id.clone(),
            BwrapWorkloadState {
                instance_id: uuid::Uuid::new_v4(),
                workload,
                command,
                owned_object_ids,
                suspended: false,
            },
        );
        NanoHandle {
            instance_id: runtime.workloads[&workload_id].instance_id,
            runtime_key: RUNTIME_BWRAP_LANDLOCK.to_string(),
            workload_id,
            agent_id: None,
            pid: Some(pid),
        }
    }

    fn transactional_fixture_state(workload_id: &str, agent_name: &str) -> BwrapWorkloadState {
        let workload = fixture_workload(workload_id, agent_name);
        BwrapWorkloadState {
            instance_id: uuid::Uuid::new_v4(),
            command: vec!["/usr/bin/sleep".to_string(), "30".to_string()],
            workload,
            owned_object_ids: Vec::new(),
            suspended: false,
        }
    }

    #[test]
    fn spawn_failure_after_each_side_effect_rolls_back_transactionally() {
        for stage in [
            BwrapSpawnStage::MarkerWritten,
            BwrapSpawnStage::SetupComplete,
            BwrapSpawnStage::ProcessStarted,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let mut runtime = BwrapNanoRuntime::with_test_dirs(
                temp.path().join("cas"),
                temp.path().join("homes"),
            );
            let workload_id = format!("failure-{stage:?}");
            let agent_name = format!("failure-agent-{stage:?}-{}", std::process::id());
            let state = transactional_fixture_state(&workload_id, &agent_name);
            let started_pid = std::sync::Arc::new(std::sync::Mutex::new(None));
            let pid_observer = std::sync::Arc::clone(&started_pid);

            let error = runtime
                .spawn_state_with(
                    state,
                    move |_, _, _, _| {
                        let process = AgentProcess::launch_fixture()?;
                        *pid_observer.lock().unwrap() = Some(process.pid);
                        Ok(process)
                    },
                    |reached| {
                        if reached == stage {
                            Err(anyhow!("injected failure after {stage:?}"))
                        } else {
                            Ok(())
                        }
                    },
                )
                .unwrap_err();

            assert!(error.to_string().contains("injected failure"));
            assert!(!runtime.pending_spawns.contains_key(&workload_id));
            assert!(!runtime.workloads.contains_key(&workload_id));
            assert!(!runtime.handles.contains_key(&workload_id));
            assert!(!runtime.processes.contains_key(&workload_id));
            assert!(!runtime.home_dir(&agent_name).join(".nano-runtime").exists());
            let started_pid = *started_pid.lock().unwrap();
            if let Some(pid) = started_pid {
                assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
            }
        }
    }

    #[test]
    fn failed_spawn_rollback_retains_exact_transaction_for_retry() {
        let temp = tempfile::tempdir().unwrap();
        let mut runtime =
            BwrapNanoRuntime::with_test_dirs(temp.path().join("cas"), temp.path().join("homes"));
        let workload_id = "retry-spawn";
        let agent_name = format!("retry-spawn-agent-{}", std::process::id());
        let state = transactional_fixture_state(workload_id, &agent_name);
        let marker = runtime.home_dir(&agent_name).join(".nano-runtime");

        let error = runtime
            .spawn_state_with(
                state,
                |_, _, _, _| AgentProcess::launch_fixture(),
                |stage| {
                    if stage == BwrapSpawnStage::ProcessStarted {
                        std::fs::write(&marker, b"foreign-owner")?;
                        Err(anyhow!("injected post-process failure"))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("rollback retained for retry"));
        assert!(runtime.pending_spawns.contains_key(workload_id));

        std::fs::write(&marker, workload_id.as_bytes()).unwrap();
        assert!(runtime.rollback_pending_spawn(workload_id).unwrap());
        assert!(!runtime.pending_spawns.contains_key(workload_id));
        assert!(!marker.exists());

        let handle = runtime
            .spawn_state_with(
                transactional_fixture_state(workload_id, &agent_name),
                |_, _, _, _| AgentProcess::launch_fixture(),
                |_| Ok(()),
            )
            .unwrap();
        assert_eq!(
            runtime.stop(&handle).unwrap().outcome,
            sentinel_common::nano_runtime::NanoStopOutcome::Stopped
        );
    }

    #[test]
    fn durable_spawn_marker_recovery_is_exact_and_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let cas = temp.path().join("cas");
        let homes = temp.path().join("homes");
        let first = BwrapNanoRuntime::with_test_dirs(cas.clone(), homes.clone());
        first
            .write_marker("durable-agent", "durable-workload")
            .unwrap();
        drop(first);

        let recovered = BwrapNanoRuntime::with_test_dirs(cas.clone(), homes.clone());
        assert!(recovered
            .reconcile_durable_spawn_marker("durable-agent", "durable-workload")
            .unwrap());
        assert!(!homes.join("durable-agent/.nano-runtime").exists());

        recovered
            .write_marker("durable-agent", "foreign-workload")
            .unwrap();
        let error = recovered
            .reconcile_durable_spawn_marker("durable-agent", "durable-workload")
            .unwrap_err();
        assert!(error.to_string().contains("durable ownership"));
        assert_eq!(
            std::fs::read_to_string(homes.join("durable-agent/.nano-runtime")).unwrap(),
            "foreign-workload"
        );
    }

    #[test]
    fn workbench_spawn_without_exact_isolation_attestation_rolls_back_all_ownership() {
        let temp = tempfile::tempdir().unwrap();
        let mut runtime =
            BwrapNanoRuntime::with_test_dirs(temp.path().join("cas"), temp.path().join("homes"));
        let workload_id = "AGENT-42";
        let mut state = transactional_fixture_state(workload_id, "workbench-agent");
        state.command = vec!["/usr/bin/agent-runtime".to_string()];
        let error = runtime
            .spawn_state_with(
                state,
                |_, _, _, _| AgentProcess::launch_fixture(),
                |_| Ok(()),
            )
            .unwrap_err();
        assert!(error.to_string().contains("isolation attestation"));
        assert!(!runtime.pending_spawns.contains_key(workload_id));
        assert!(!runtime.workloads.contains_key(workload_id));
        assert!(!runtime.handles.contains_key(workload_id));
        assert!(!runtime.processes.contains_key(workload_id));
        assert!(!temp
            .path()
            .join("homes/workbench-agent/.nano-runtime")
            .exists());
    }

    #[test]
    fn sandbox_artifact_scope_rejects_symlink_components_and_foreign_agent_base() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let directory = tempfile::tempdir().unwrap();
        let agent_root = directory.path().join("AGENT-07");
        let artifacts = agent_root.join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::write(agent_root.join(".nano-runtime"), "AGENT-07").unwrap();
        let foreign_scope = directory.path().join("foreign-scope");
        std::fs::create_dir_all(foreign_scope.join("work-04")).unwrap();

        symlink(&foreign_scope, artifacts.join("project-01")).unwrap();
        assert!(
            bind_workbench_artifact_scope(&agent_root, "AGENT-07", "project-01", "work-04",)
                .is_err()
        );
        std::fs::remove_file(artifacts.join("project-01")).unwrap();

        let project = artifacts.join("project-01");
        std::fs::create_dir(&project).unwrap();
        symlink(foreign_scope.join("work-04"), project.join("work-04")).unwrap();
        assert!(
            bind_workbench_artifact_scope(&agent_root, "AGENT-07", "project-01", "work-04",)
                .is_err()
        );

        let foreign_agent = directory.path().join("AGENT-08");
        std::fs::create_dir_all(foreign_agent.join("artifacts/project-01").join("work-04"))
            .unwrap();
        std::fs::write(foreign_agent.join(".nano-runtime"), "AGENT-08").unwrap();
        assert!(
            bind_workbench_artifact_scope(&foreign_agent, "AGENT-07", "project-01", "work-04",)
                .is_err()
        );

        let safe_scope = directory.path().join("safe-files");
        std::fs::create_dir(&safe_scope).unwrap();
        let manifest = safe_scope.join("manifest.json");
        std::fs::write(&manifest, b"{}").unwrap();
        let pinned = open_pinned_artifact_directory(&safe_scope).unwrap();
        open_scoped_artifact_file(&pinned, "manifest.json", 16, None).unwrap();

        let hardlink = safe_scope.join("manifest-hardlink.json");
        std::fs::hard_link(&manifest, &hardlink).unwrap();
        assert!(open_scoped_artifact_file(&pinned, "manifest.json", 16, None).is_err());
        std::fs::remove_file(hardlink).unwrap();

        for mode in [0o4644, 0o2644, 0o1644] {
            std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(mode)).unwrap();
            assert!(open_scoped_artifact_file(&pinned, "manifest.json", 16, None).is_err());
        }
        std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o644)).unwrap();

        let replacement = safe_scope.join("replacement.json");
        std::fs::write(&replacement, b"[]").unwrap();
        assert!(
            open_scoped_artifact_file_with(&pinned, "manifest.json", 16, None, |path| {
                std::fs::rename(&replacement, &manifest)?;
                OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(path)
            },)
            .is_err()
        );
    }

    fn insert_protocol_fixture(
        runtime: &mut BwrapNanoRuntime,
        workload_id: &str,
        lines: &[&str],
    ) -> NanoHandle {
        insert_protocol_process(
            runtime,
            workload_id,
            AgentProcess::launch_protocol_fixture(lines).unwrap(),
        )
    }

    fn insert_protocol_process(
        runtime: &mut BwrapNanoRuntime,
        workload_id: &str,
        process: AgentProcess,
    ) -> NanoHandle {
        let workload = fixture_workload(workload_id, "agent-protocol-fixture");
        let pid = process.pid;
        let instance_id = uuid::Uuid::new_v4();
        runtime.processes.insert(workload_id.to_string(), process);
        runtime.handles.insert(
            workload_id.to_string(),
            SandboxHandle {
                agent_name: workload.agent_name.clone(),
                cgroup_created: false,
                cgroup_id: None,
                io_available: false,
                bwrap_pid: Some(pid),
                landlock_applied: false,
                network_isolated: true,
            },
        );
        runtime.workloads.insert(
            workload_id.to_string(),
            BwrapWorkloadState {
                instance_id,
                workload,
                command: Vec::new(),
                owned_object_ids: Vec::new(),
                suspended: false,
            },
        );
        NanoHandle {
            instance_id,
            runtime_key: RUNTIME_BWRAP_LANDLOCK.to_string(),
            workload_id: workload_id.to_string(),
            agent_id: None,
            pid: Some(pid),
        }
    }

    #[test]
    fn attested_workbench_protocol_reports_exact_instance_and_all_isolation_barriers() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2998";
        let mut runtime = BwrapNanoRuntime::with_cas_dir(tempfile::tempdir().unwrap().path());
        let mut process = AgentProcess::launch_protocol_fixture(&[]).unwrap();
        let child_pid = process.pid;
        process.install_workbench_isolation_attestation(child_pid, 4);
        let workload_id = "AGENT-42";
        let handle = insert_protocol_process(&mut runtime, workload_id, process);
        let sandbox = runtime.handles.get_mut(workload_id).unwrap();
        sandbox.cgroup_created = true;
        sandbox.landlock_applied = true;
        sandbox.network_isolated = true;
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start_frame(invocation_id, unix_time_ms() + 10_000),
                },
            )
            .unwrap();
        let resources = runtime.resources(&handle).unwrap();
        assert_eq!(resources.instance_id, Some(handle.instance_id));
        assert_eq!(resources.child_pid, Some(child_pid));
        assert!(resources.cgroup_created);
        assert!(resources.landlock_applied);
        assert!(resources.network_isolated);
    }

    fn start_frame(invocation_id: &str, deadline_unix_ms: u64) -> String {
        serde_json::to_string(&serde_json::json!({
            "kind": "execute",
            "request": {
                "schema_version": 1,
                "invocation_id": invocation_id,
                "deadline_unix_ms": deadline_unix_ms
            }
        }))
        .unwrap()
    }

    fn successful_result_frame(
        invocation_id: &str,
        input_digest: &str,
        output: serde_json::Value,
    ) -> String {
        serde_json::json!({
            "kind": "result",
            "schema_version": 1,
            "invocation_id": invocation_id,
            "input_digest": input_digest,
            "outcome": "succeeded",
            "resources": {
                "duration_ms": 1,
                "cpu_time_ms": 0,
                "peak_memory_bytes": 0,
                "peak_process_count": 0,
                "bytes_read": 0,
                "bytes_written": 0,
                "artifact_bytes": 0
            },
            "artifacts": [],
            "output": output,
            "error": null
        })
        .to_string()
    }

    fn completed_progress_frame(invocation_id: &str) -> String {
        serde_json::json!({
            "kind": "progress",
            "schema_version": 1,
            "invocation_id": invocation_id,
            "stage": "completed",
            "elapsed_ms": 1
        })
        .to_string()
    }

    #[test]
    fn control_input_rejects_embedded_jsonl_record_boundaries() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2900";
        for (workload_id, suffix) in [("lf-control", "\n"), ("crlf-control", "\r\n")] {
            let mut runtime = BwrapNanoRuntime::with_cas_dir(tempfile::tempdir().unwrap().path());
            let handle = insert_protocol_fixture(&mut runtime, workload_id, &[]);
            let input = format!(
                "{}{}",
                start_frame(invocation_id, unix_time_ms() + 10_000),
                suffix
            );
            let error = runtime
                .exec(
                    &handle,
                    NanoExecRequest {
                        operation: "workbench_start".to_string(),
                        input,
                    },
                )
                .unwrap_err();
            assert_exec_error(&error, NanoExecErrorCode::InvalidFrame);
            assert!(!runtime.exchanges.contains_key(workload_id));
        }
    }

    fn poll_frame(invocation_id: &str) -> String {
        serde_json::to_string(&serde_json::json!({
            "kind": "poll",
            "schema_version": 1,
            "invocation_id": invocation_id
        }))
        .unwrap()
    }

    fn recover_frame(invocation_id: &str, input_digest: &str) -> String {
        serde_json::to_string(&serde_json::json!({
            "kind": "recover",
            "schema_version": 1,
            "invocation_id": invocation_id,
            "input_digest": input_digest
        }))
        .unwrap()
    }

    fn wait_for_autonomous_quiescence(
        runtime: &BwrapNanoRuntime,
        workload_id: &str,
    ) -> ProtocolSupervisionSnapshot {
        for _ in 0..600 {
            let snapshot = runtime.processes[workload_id].protocol_supervision_snapshot();
            if (snapshot.failure.is_some() || snapshot.terminal_finalized)
                && snapshot.process_reaped
                && snapshot.reader_closed
                && snapshot.stdin_closed
                && snapshot.cgroup_quiesced
            {
                return snapshot;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("autonomous protocol supervisor did not quiesce {workload_id}");
    }

    fn assert_exec_error(error: &anyhow::Error, expected: NanoExecErrorCode) {
        let typed = error
            .downcast_ref::<NanoExecError>()
            .expect("exec failure must retain its typed public-safe classification");
        assert_eq!(typed.code, expected);
        assert!(!typed.retryable);
    }

    fn poll_until_error(
        runtime: &mut BwrapNanoRuntime,
        handle: &NanoHandle,
        invocation_id: &str,
    ) -> anyhow::Error {
        for _ in 0..100 {
            match runtime.exec(
                handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: poll_frame(invocation_id),
                },
            ) {
                Ok(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
                Err(error) => return error,
            }
        }
        panic!("protocol fixture did not produce the expected error");
    }

    fn wait_until_pid_exits(pid: u32) {
        for _ in 0..100 {
            if !PathBuf::from(format!("/proc/{pid}")).exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn wait_for_recorded_lines(path: &std::path::Path, expected: usize) -> String {
        for _ in 0..100 {
            if let Ok(recorded) = std::fs::read_to_string(path) {
                if recorded.lines().count() >= expected {
                    return recorded;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("protocol fixture did not record {expected} frame(s)");
    }

    fn wait_for_protocol_start_failure(
        runtime: &BwrapNanoRuntime,
        workload_id: &str,
    ) -> ProtocolSupervisionFailure {
        for _ in 0..1_000 {
            if let Some(failure) = runtime.processes[workload_id].protocol_start_failure() {
                return failure;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("protocol fixture did not publish its pre-execute failure");
    }

    fn poll_until_terminal(
        registry: &mut sentinel_common::nano_runtime::NanoRuntimeRegistry,
        handle: &NanoHandle,
        invocation_id: &str,
    ) -> NanoExecResult {
        for _ in 0..100 {
            let result = registry
                .exec(
                    handle,
                    NanoExecRequest {
                        operation: "workbench_poll".to_string(),
                        input: poll_frame(invocation_id),
                    },
                )
                .unwrap();
            let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
            if output["state"] == "completed" {
                return result;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("protocol fixture did not reach a terminal state");
    }

    #[test]
    fn registry_exec_channel_returns_only_matching_bounded_terminal_exchange() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2901";
        let progress = format!(
            r#"{{"kind":"progress","schema_version":1,"invocation_id":"{invocation_id}","stage":"validated","elapsed_ms":0}}"#
        );
        let result = format!(
            r#"{{"kind":"result","schema_version":1,"invocation_id":"{invocation_id}","input_digest":"{}","outcome":"succeeded","resources":{{"duration_ms":1,"cpu_time_ms":0,"peak_memory_bytes":0,"peak_process_count":0,"bytes_read":0,"bytes_written":0,"artifact_bytes":0}},"artifacts":[],"output":{{}},"error":null}}"#,
            "a".repeat(64)
        );
        let completed = format!(
            r#"{{"kind":"progress","schema_version":1,"invocation_id":"{invocation_id}","stage":"completed","elapsed_ms":1}}"#
        );
        let temp = tempfile::tempdir().unwrap();
        let record_path = temp.path().join("protocol-input.jsonl");
        let descendant_pid_path = temp.path().join("descendant.pid");
        let process = AgentProcess::launch_recording_protocol_fixture(
            &[&progress, &result, &completed],
            &record_path,
            &descendant_pid_path,
        )
        .unwrap();
        let mut runtime = BwrapNanoRuntime::with_cas_dir(temp.path().join("cas"));
        let handle = insert_protocol_process(&mut runtime, "protocol-success", process);
        let mut registry = sentinel_common::nano_runtime::NanoRuntimeRegistry::new(None);
        registry.register(runtime).unwrap();
        let start = start_frame(invocation_id, unix_time_ms() + 10_000);
        let accepted = registry
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start.clone(),
                },
            )
            .unwrap();
        assert!(accepted.success);
        let start_replay = registry
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start,
                },
            )
            .unwrap();
        assert!(start_replay.output.contains("pending"));
        let terminal = poll_until_terminal(&mut registry, &handle, invocation_id);
        let output: serde_json::Value = serde_json::from_str(&terminal.output).unwrap();
        assert_eq!(output["invocation_id"], invocation_id);
        assert_eq!(output["state"], "completed");
        assert_eq!(output["messages"].as_array().unwrap().len(), 3);
        let terminal_replay = registry
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: poll_frame(invocation_id),
                },
            )
            .unwrap();
        assert_eq!(terminal_replay.output, terminal.output);
        let recorded = wait_for_recorded_lines(&record_path, 1);
        assert_eq!(recorded.lines().count(), 1, "start replay wrote to child");

        let descendant_pid: u32 = std::fs::read_to_string(&descendant_pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        registry.stop(&handle).unwrap();
        wait_until_pid_exits(descendant_pid);
        assert!(
            !PathBuf::from(format!("/proc/{descendant_pid}")).exists(),
            "descendant survived runtime stop"
        );
    }

    #[test]
    fn terminal_invocation_recycles_agent_runtime_for_a_second_serial_invocation() {
        let first_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2981";
        let second_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2982";
        let temp = tempfile::tempdir().unwrap();
        let first_record = temp.path().join("first-input.jsonl");
        let first_descendant = temp.path().join("first-descendant.pid");
        let first_result = format!(
            r#"{{"kind":"result","schema_version":1,"invocation_id":"{first_id}","input_digest":"{}","outcome":"succeeded","resources":{{"duration_ms":1,"cpu_time_ms":0,"peak_memory_bytes":0,"peak_process_count":0,"bytes_read":0,"bytes_written":0,"artifact_bytes":0}},"artifacts":[],"output":{{}},"error":null}}"#,
            "a".repeat(64)
        );
        let first_completed = format!(
            r#"{{"kind":"progress","schema_version":1,"invocation_id":"{first_id}","stage":"completed","elapsed_ms":1}}"#
        );
        let first_process = AgentProcess::launch_recording_protocol_fixture(
            &[&first_result, &first_completed],
            &first_record,
            &first_descendant,
        )
        .unwrap();
        let mut runtime = BwrapNanoRuntime::with_cas_dir(temp.path().join("cas"));
        let handle = insert_protocol_process(&mut runtime, "serial-workbench", first_process);
        runtime
            .workloads
            .get_mut(&handle.workload_id)
            .unwrap()
            .command = vec!["/usr/bin/agent-runtime".to_string()];
        let first_start = start_frame(first_id, unix_time_ms() + 10_000);
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: first_start.clone(),
                },
            )
            .unwrap();
        for _ in 0..200 {
            if runtime.processes[&handle.workload_id]
                .protocol_supervision_snapshot()
                .terminal_finalized
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            runtime.processes[&handle.workload_id]
                .protocol_supervision_snapshot()
                .terminal_finalized
        );
        {
            let exchange = runtime.exchanges.get_mut(&handle.workload_id).unwrap();
            exchange.messages = vec![
                serde_json::from_str(&first_result).unwrap(),
                serde_json::from_str(&first_completed).unwrap(),
            ];
            exchange.result_seen = true;
            exchange.terminal = Some(WorkbenchTerminal::Succeeded);
            exchange.finalized = true;
            exchange.cleanup_pending = true;
        }

        let second_record = temp.path().join("second-input.jsonl");
        let second_descendant = temp.path().join("second-descendant.pid");
        let second_result = format!(
            r#"{{"kind":"result","schema_version":1,"invocation_id":"{second_id}","input_digest":"{}","outcome":"succeeded","resources":{{"duration_ms":1,"cpu_time_ms":0,"peak_memory_bytes":0,"peak_process_count":0,"bytes_read":0,"bytes_written":0,"artifact_bytes":0}},"artifacts":[],"output":{{}},"error":null}}"#,
            "b".repeat(64)
        );
        let second_completed = format!(
            r#"{{"kind":"progress","schema_version":1,"invocation_id":"{second_id}","stage":"completed","elapsed_ms":1}}"#
        );
        runtime
            .recycle_workbench_runtime_with(&handle.workload_id, |_, mut previous, _, _, _| {
                let process = AgentProcess::launch_recording_protocol_fixture(
                    &[&second_result, &second_completed],
                    &second_record,
                    &second_descendant,
                )?;
                previous.bwrap_pid = Some(process.pid);
                Ok((previous, process))
            })
            .unwrap();
        runtime
            .exchanges
            .get_mut(&handle.workload_id)
            .unwrap()
            .cleanup_pending = false;

        let replay = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: first_start,
                },
            )
            .unwrap();
        assert!(replay.output.contains("completed"));
        assert!(
            !second_record.exists(),
            "terminal replay reached the replacement runtime"
        );

        let second_start = start_frame(second_id, unix_time_ms() + 10_000);
        let accepted = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: second_start.clone(),
                },
            )
            .unwrap();
        assert!(accepted.output.contains("accepted"));
        assert_eq!(
            wait_for_recorded_lines(&second_record, 1).trim(),
            second_start,
            "second invocation did not use the same exact NanoHandle once"
        );
        let duplicate = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: second_start,
                },
            )
            .unwrap();
        assert!(duplicate.output.contains("pending") || duplicate.output.contains("completed"));
        assert_eq!(
            wait_for_recorded_lines(&second_record, 1).lines().count(),
            1
        );
        runtime
            .workloads
            .get_mut(&handle.workload_id)
            .unwrap()
            .command
            .clear();
        let terminal = (0..100).find_map(|_| {
            let result = runtime
                .exec(
                    &handle,
                    NanoExecRequest {
                        operation: "workbench_poll".to_string(),
                        input: poll_frame(second_id),
                    },
                )
                .unwrap();
            (serde_json::from_str::<serde_json::Value>(&result.output).unwrap()["state"].as_str()
                == Some("completed"))
            .then_some(result)
            .or_else(|| {
                std::thread::sleep(std::time::Duration::from_millis(10));
                None
            })
        });
        assert!(terminal.is_some());
    }

    #[test]
    fn recovery_is_effect_free_digest_bound_and_replayable_after_cleanup() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2902";
        let input_digest = "a".repeat(64);
        let result = format!(
            r#"{{"kind":"result","schema_version":1,"invocation_id":"{invocation_id}","input_digest":"{input_digest}","outcome":"succeeded","resources":{{"duration_ms":1,"cpu_time_ms":0,"peak_memory_bytes":0,"peak_process_count":0,"bytes_read":0,"bytes_written":0,"artifact_bytes":0}},"artifacts":[],"output":{{}},"error":null}}"#
        );
        let completed = format!(
            r#"{{"kind":"progress","schema_version":1,"invocation_id":"{invocation_id}","stage":"completed","elapsed_ms":1}}"#
        );
        let temp = tempfile::tempdir().unwrap();
        let record_path = temp.path().join("recovery-input.jsonl");
        let descendant_pid_path = temp.path().join("recovery-descendant.pid");
        let process = AgentProcess::launch_recording_protocol_fixture(
            &[&result, &completed],
            &record_path,
            &descendant_pid_path,
        )
        .unwrap();
        let mut runtime = BwrapNanoRuntime::with_cas_dir(temp.path().join("cas"));
        let handle = insert_protocol_process(&mut runtime, "protocol-recovery", process);
        let recover = recover_frame(invocation_id, &input_digest);

        let accepted = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_recover".to_string(),
                    input: recover.clone(),
                },
            )
            .unwrap();
        assert!(accepted.output.contains("accepted"));
        let replay = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_recover".to_string(),
                    input: recover,
                },
            )
            .unwrap();
        assert!(replay.output.contains("pending") || replay.output.contains("completed"));

        let terminal = (0..100)
            .find_map(|_| {
                let result = runtime
                    .exec(
                        &handle,
                        NanoExecRequest {
                            operation: "workbench_poll".to_string(),
                            input: poll_frame(invocation_id),
                        },
                    )
                    .unwrap();
                let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
                if output["state"] == "completed" {
                    Some(result)
                } else {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    None
                }
            })
            .expect("recovery receipt did not reach its retained terminal state");
        assert!(!runtime.processes.contains_key(&handle.workload_id));
        let terminal_replay = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: poll_frame(invocation_id),
                },
            )
            .unwrap();
        assert_eq!(terminal_replay.output, terminal.output);

        let digest_error = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_recover".to_string(),
                    input: recover_frame(invocation_id, &"b".repeat(64)),
                },
            )
            .unwrap_err();
        assert_exec_error(&digest_error, NanoExecErrorCode::DigestConflict);
        let mut stale = handle.clone();
        stale.instance_id = uuid::Uuid::new_v4();
        let stale_error = runtime
            .exec(
                &stale,
                NanoExecRequest {
                    operation: "workbench_recover".to_string(),
                    input: recover_frame(invocation_id, &input_digest),
                },
            )
            .unwrap_err();
        assert_exec_error(&stale_error, NanoExecErrorCode::WorkloadUnavailable);

        let recorded = wait_for_recorded_lines(&record_path, 1);
        assert_eq!(recorded.lines().count(), 1);
        let frame: serde_json::Value =
            serde_json::from_str(recorded.lines().next().unwrap()).unwrap();
        assert_eq!(frame["kind"], "recover");
        assert_eq!(frame["input_digest"], input_digest);
    }

    #[test]
    fn foreign_poll_is_rejected_before_matching_output_is_consumed() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2910";
        let start = start_frame(invocation_id, unix_time_ms() + 10_000);
        let result =
            successful_result_frame(invocation_id, &frame_digest(&start), serde_json::json!({}));
        let completed = completed_progress_frame(invocation_id);
        let mut runtime = BwrapNanoRuntime::with_cas_dir(tempfile::tempdir().unwrap().path());
        let handle = insert_protocol_fixture(
            &mut runtime,
            "protocol-foreign-poll",
            &[&result, &completed],
        );
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start,
                },
            )
            .unwrap();

        let error = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: poll_frame("foreign-invocation"),
                },
            )
            .unwrap_err();
        assert_exec_error(&error, NanoExecErrorCode::InvocationConflict);

        for _ in 0..100 {
            let output = runtime
                .exec(
                    &handle,
                    NanoExecRequest {
                        operation: "workbench_poll".to_string(),
                        input: poll_frame(invocation_id),
                    },
                )
                .unwrap();
            if output.output.contains("completed") {
                let envelope: serde_json::Value = serde_json::from_str(&output.output).unwrap();
                assert_eq!(envelope["messages"].as_array().unwrap().len(), 2);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("matching poll did not receive retained child output");
    }

    #[test]
    fn concurrent_workloads_cannot_cross_wire_protocol_frames() {
        let invocation_a = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2918";
        let invocation_b = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2919";
        let start_a = start_frame(invocation_a, unix_time_ms() + 10_000);
        let start_b = start_frame(invocation_b, unix_time_ms() + 10_000);
        let result_a = successful_result_frame(
            invocation_a,
            &frame_digest(&start_a),
            serde_json::json!({"marker": "A"}),
        );
        let completed_a = completed_progress_frame(invocation_a);
        let result_b = successful_result_frame(
            invocation_b,
            &frame_digest(&start_b),
            serde_json::json!({"marker": "B"}),
        );
        let completed_b = completed_progress_frame(invocation_b);
        let mut runtime = BwrapNanoRuntime::with_cas_dir(tempfile::tempdir().unwrap().path());
        let handle_a = insert_protocol_fixture(
            &mut runtime,
            "protocol-concurrent-a",
            &[&result_a, &completed_a],
        );
        let handle_b = insert_protocol_fixture(
            &mut runtime,
            "protocol-concurrent-b",
            &[&result_b, &completed_b],
        );
        let mut registry = sentinel_common::nano_runtime::NanoRuntimeRegistry::new(None);
        registry.register(runtime).unwrap();

        for (handle, start) in [(&handle_a, start_a), (&handle_b, start_b)] {
            registry
                .exec(
                    handle,
                    NanoExecRequest {
                        operation: "workbench_start".to_string(),
                        input: start,
                    },
                )
                .unwrap();
        }

        let terminal_b = poll_until_terminal(&mut registry, &handle_b, invocation_b);
        let terminal_a = poll_until_terminal(&mut registry, &handle_a, invocation_a);
        for (terminal, invocation_id, marker) in [
            (terminal_a, invocation_a, "A"),
            (terminal_b, invocation_b, "B"),
        ] {
            let envelope: serde_json::Value = serde_json::from_str(&terminal.output).unwrap();
            assert_eq!(envelope["invocation_id"], invocation_id);
            let messages = envelope["messages"].as_array().unwrap();
            assert!(messages
                .iter()
                .all(|message| message["invocation_id"] == invocation_id));
            assert_eq!(messages[0]["output"]["marker"], marker);
        }
        registry.stop(&handle_a).unwrap();
        registry.stop(&handle_b).unwrap();
    }

    #[test]
    fn start_replay_is_digest_bound_and_control_frames_are_versioned() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2911";
        let temp = tempfile::tempdir().unwrap();
        let record_path = temp.path().join("protocol-input.jsonl");
        let descendant_pid_path = temp.path().join("descendant.pid");
        let process = AgentProcess::launch_recording_protocol_fixture(
            &[],
            &record_path,
            &descendant_pid_path,
        )
        .unwrap();
        let mut runtime = BwrapNanoRuntime::with_cas_dir(temp.path().join("cas"));
        let handle = insert_protocol_process(&mut runtime, "protocol-digest", process);
        let start = start_frame(invocation_id, unix_time_ms() + 10_000);
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start.clone(),
                },
            )
            .unwrap();

        let mut conflicting: serde_json::Value = serde_json::from_str(&start).unwrap();
        conflicting["request"]["opaque"] = serde_json::json!("different bytes");
        let error = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: serde_json::to_string(&conflicting).unwrap(),
                },
            )
            .unwrap_err();
        assert_exec_error(&error, NanoExecErrorCode::DigestConflict);

        let error = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: invocation_id.to_string(),
                },
            )
            .unwrap_err();
        assert_exec_error(&error, NanoExecErrorCode::InvalidFrame);

        let mut version: serde_json::Value =
            serde_json::from_str(&poll_frame(invocation_id)).unwrap();
        version["schema_version"] = serde_json::json!(2);
        let error = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: serde_json::to_string(&version).unwrap(),
                },
            )
            .unwrap_err();
        assert_exec_error(&error, NanoExecErrorCode::UnsupportedVersion);

        let cancel = serde_json::json!({
            "kind": "cancel",
            "schema_version": 1,
            "invocation_id": invocation_id,
            "reason": "operator_cancelled"
        });
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_cancel".to_string(),
                    input: serde_json::to_string(&cancel).unwrap(),
                },
            )
            .unwrap();
        let mut conflicting_cancel = cancel;
        conflicting_cancel["reason"] = serde_json::json!("different_reason");
        let error = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_cancel".to_string(),
                    input: serde_json::to_string(&conflicting_cancel).unwrap(),
                },
            )
            .unwrap_err();
        assert_exec_error(&error, NanoExecErrorCode::DigestConflict);
        let recorded = wait_for_recorded_lines(&record_path, 2);
        runtime.stop(&handle).unwrap();
        assert_eq!(
            recorded.lines().count(),
            2,
            "conflicting start or cancel replay wrote an extra child frame"
        );
    }

    #[test]
    fn retained_exchange_rejects_stale_incarnation_without_consuming_or_tearing_down() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2930";
        let start = start_frame(invocation_id, unix_time_ms() + 10_000);
        let result =
            successful_result_frame(invocation_id, &frame_digest(&start), serde_json::json!({}));
        let completed = completed_progress_frame(invocation_id);
        let mut runtime = BwrapNanoRuntime::with_cas_dir(tempfile::tempdir().unwrap().path());
        let handle = insert_protocol_fixture(
            &mut runtime,
            "protocol-stale-incarnation",
            &[&result, &completed],
        );
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start,
                },
            )
            .unwrap();

        let mut stale = handle.clone();
        stale.instance_id = uuid::Uuid::new_v4();
        let error = runtime
            .exec(
                &stale,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: poll_frame(invocation_id),
                },
            )
            .unwrap_err();
        assert_exec_error(&error, NanoExecErrorCode::WorkloadUnavailable);
        assert!(runtime.stop(&stale).is_err());
        assert!(runtime.exchanges.contains_key(&handle.workload_id));

        for _ in 0..100 {
            let output = runtime
                .exec(
                    &handle,
                    NanoExecRequest {
                        operation: "workbench_poll".to_string(),
                        input: poll_frame(invocation_id),
                    },
                )
                .unwrap();
            if output.output.contains("completed") {
                runtime.stop(&handle).unwrap();
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("matching incarnation did not consume its retained exchange");
    }

    #[test]
    fn byte_identical_start_replay_remains_stable_after_request_deadline() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2931";
        let temp = tempfile::tempdir().unwrap();
        let record_path = temp.path().join("protocol-input.jsonl");
        let descendant_pid_path = temp.path().join("descendant.pid");
        let claimed = std::sync::Arc::new(std::sync::Barrier::new(2));
        let release = std::sync::Arc::new(std::sync::Barrier::new(2));
        let process = AgentProcess::launch_recording_protocol_fixture(
            &[],
            &record_path,
            &descendant_pid_path,
        )
        .unwrap();
        process.install_deadline_send_barrier(
            std::sync::Arc::clone(&claimed),
            std::sync::Arc::clone(&release),
        );
        let mut runtime = BwrapNanoRuntime::with_cas_dir(temp.path().join("cas"));
        let handle = insert_protocol_process(&mut runtime, "protocol-expired-replay", process);
        let start = start_frame(invocation_id, unix_time_ms() + 20);
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start.clone(),
                },
            )
            .unwrap();
        claimed.wait();
        let replay = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start,
                },
            )
            .unwrap();
        let replay: serde_json::Value = serde_json::from_str(&replay.output).unwrap();
        assert_eq!(replay["state"], "cancelling");
        assert_ne!(replay["state"], "finalizing");
        assert_eq!(wait_for_recorded_lines(&record_path, 1).lines().count(), 1);
        release.wait();

        let quiesced = wait_for_autonomous_quiescence(&runtime, "protocol-expired-replay");
        assert_eq!(
            quiesced.failure,
            Some(ProtocolSupervisionFailure::DeadlineExceeded)
        );
        let recorded = wait_for_recorded_lines(&record_path, 2);
        assert_eq!(
            recorded.lines().count(),
            2,
            "byte-identical start replay must not emit another execute or deadline cancel frame"
        );
        let terminal = poll_until_error(&mut runtime, &handle, invocation_id);
        assert_exec_error(&terminal, NanoExecErrorCode::DeadlineExceeded);
        let terminal_replay = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: poll_frame(invocation_id),
                },
            )
            .unwrap_err();
        assert_eq!(terminal_replay.to_string(), terminal.to_string());
    }

    #[test]
    fn bwrap_control_suspends_and_resumes_adapter_owned_process() {
        let temp = tempfile::tempdir().unwrap();
        let mut runtime =
            BwrapNanoRuntime::with_test_dirs(temp.path().join("cas"), temp.path().join("homes"));
        let handle = insert_fixture(
            &mut runtime,
            fixture_workload("control-fixture", "control-agent"),
            Vec::new(),
        );
        let pid = handle.pid.unwrap();

        let suspended = runtime
            .control(&handle, NanoRuntimeControlAction::Suspend)
            .unwrap();
        assert_eq!(suspended.affected_units, 1);
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
        assert!(status.lines().any(|line| line.starts_with("State:\tT")));
        assert_eq!(
            runtime.health(&handle).unwrap().state,
            NanoHealthState::Degraded
        );

        runtime
            .control(&handle, NanoRuntimeControlAction::Resume)
            .unwrap();
        assert_eq!(
            runtime.health(&handle).unwrap().state,
            NanoHealthState::Healthy
        );
        runtime.stop(&handle).unwrap();
    }

    #[test]
    fn default_bwrap_snapshot_is_reproducible_recreate_without_cas_manifest_claim() {
        let temp = tempfile::tempdir().unwrap();
        let mut runtime =
            BwrapNanoRuntime::with_test_dirs(temp.path().join("cas"), temp.path().join("homes"));
        runtime.set_cas_manifest_enabled(false);
        let agent_name = "compatibility-agent";
        let home = runtime.home_dir(agent_name);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("existing.txt"),
            b"must remain outside compatibility snapshot",
        )
        .unwrap();
        let mut workload = fixture_workload("compatibility-workload", agent_name);
        workload.command = vec!["/usr/bin/true".to_string()];
        let handle = insert_fixture(&mut runtime, workload, Vec::new());

        let snapshot = runtime.snapshot(&handle).unwrap();
        assert_eq!(snapshot.semantics, NanoSnapshotSemantics::BwrapRecreate);
        assert_eq!(
            snapshot.payload["workload"]["workload_id"],
            "compatibility-workload"
        );
        assert_eq!(snapshot.payload["command"][0], "/usr/bin/true");
        assert!(snapshot.payload.get("home_manifest").is_none());
        assert_eq!(
            std::fs::read(home.join("existing.txt")).unwrap(),
            b"must remain outside compatibility snapshot"
        );
        runtime.stop(&handle).unwrap();
    }

    #[test]
    fn missing_protocol_channel_fails_closed_with_a_typed_error() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2917";
        let mut runtime = BwrapNanoRuntime::with_cas_dir(tempfile::tempdir().unwrap().path());
        let handle = insert_fixture(
            &mut runtime,
            fixture_workload("protocol-missing", "agent-missing-protocol"),
            Vec::new(),
        );

        let error = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_unknown".to_string(),
                    input: "SECRET=must-not-be-reflected".to_string(),
                },
            )
            .unwrap_err();
        assert_exec_error(&error, NanoExecErrorCode::UnsupportedOperation);
        assert!(!error.to_string().contains("SECRET"));

        let start = start_frame(invocation_id, unix_time_ms() + 10_000);
        let error = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start.clone(),
                },
            )
            .unwrap_err();
        assert_exec_error(&error, NanoExecErrorCode::ChannelUnavailable);
        assert_eq!(
            runtime.health(&handle).unwrap().state,
            NanoHealthState::Stopped
        );
        let error = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start,
                },
            )
            .unwrap_err();
        assert_exec_error(&error, NanoExecErrorCode::ChannelUnavailable);
    }

    #[test]
    fn foreign_invocation_output_kills_the_selected_workload() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2902";
        let foreign = r#"{"kind":"error","schema_version":1,"invocation_id":"foreign","error":{"class":"protocol","code":"bad","safe_message":"bad","retryable":false}}"#;
        let mut runtime = BwrapNanoRuntime::with_cas_dir(tempfile::tempdir().unwrap().path());
        let handle = insert_protocol_fixture(&mut runtime, "protocol-foreign", &[foreign]);
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start_frame(invocation_id, unix_time_ms() + 10_000),
                },
            )
            .unwrap();
        let mut rejected = false;
        for _ in 0..100 {
            match runtime.exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: poll_frame(invocation_id),
                },
            ) {
                Ok(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
                Err(error) => {
                    assert_exec_error(&error, NanoExecErrorCode::InvocationConflict);
                    rejected = true;
                    break;
                }
            }
        }
        assert!(rejected);
        assert_eq!(
            runtime.health(&handle).unwrap().state,
            NanoHealthState::Stopped
        );
    }

    #[test]
    fn malformed_output_fails_closed_without_reflecting_private_content() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2906";
        let private_line = "not-json SECRET=must-not-be-reflected";
        let mut runtime = BwrapNanoRuntime::with_cas_dir(tempfile::tempdir().unwrap().path());
        let handle = insert_protocol_fixture(&mut runtime, "protocol-malformed", &[private_line]);
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start_frame(invocation_id, unix_time_ms() + 10_000),
                },
            )
            .unwrap();
        let mut failure = None;
        for _ in 0..100 {
            match runtime.exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: poll_frame(invocation_id),
                },
            ) {
                Ok(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        let error = failure.expect("malformed output must fail closed");
        assert_exec_error(&error, NanoExecErrorCode::InvalidFrame);
        assert!(!error.to_string().contains("SECRET"));
    }

    #[test]
    fn failed_protocol_cleanup_retains_exact_ownership_for_retry() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2932";
        let temp = tempfile::tempdir().unwrap();
        let mut runtime =
            BwrapNanoRuntime::with_test_dirs(temp.path().join("cas"), temp.path().join("homes"));
        let workload_id = "protocol-cleanup-retry";
        let handle = insert_protocol_fixture(&mut runtime, workload_id, &["not-json"]);
        let marker = runtime
            .home_dir("agent-protocol-fixture")
            .join(".nano-runtime");
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, b"foreign-owner").unwrap();
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start_frame(invocation_id, unix_time_ms() + 10_000),
                },
            )
            .unwrap();

        let cleanup_error = poll_until_error(&mut runtime, &handle, invocation_id);
        let typed = cleanup_error
            .downcast_ref::<NanoExecError>()
            .expect("cleanup failure must stay typed");
        assert_eq!(typed.code, NanoExecErrorCode::ChannelDisconnected);
        assert!(typed.retryable);
        assert!(runtime.exchanges[workload_id].cleanup_pending);
        assert!(runtime.workloads.contains_key(workload_id));
        assert!(runtime.processes.contains_key(workload_id));
        assert!(runtime.handles.contains_key(workload_id));
        assert_eq!(
            runtime.processes[workload_id].termination_signal_attempts(),
            1,
            "the owned process tree must be signaled exactly once before retry"
        );
        let signal_attempts = runtime.processes[workload_id].termination_signal_counter();
        assert!(runtime
            .signal_workload(workload_id, nix::sys::signal::Signal::SIGCONT)
            .is_err());
        assert_eq!(
            signal_attempts.load(std::sync::atomic::Ordering::Acquire),
            1,
            "a retained post-reap handle must not expose numeric PIDs to lifecycle signals"
        );

        std::fs::write(&marker, workload_id.as_bytes()).unwrap();
        let terminal = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: poll_frame(invocation_id),
                },
            )
            .unwrap_err();
        assert_exec_error(&terminal, NanoExecErrorCode::InvalidFrame);
        assert!(!runtime.exchanges[workload_id].cleanup_pending);
        assert!(!runtime.workloads.contains_key(workload_id));
        assert_eq!(
            signal_attempts.load(std::sync::atomic::Ordering::Acquire),
            1,
            "cleanup retry must not signal a reused numeric process target"
        );
        let replay = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: poll_frame(invocation_id),
                },
            )
            .unwrap_err();
        assert_eq!(replay.to_string(), terminal.to_string());
    }

    #[test]
    fn health_reap_publishes_no_signal_state_before_cgroup_only_cleanup() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2935";
        let workload_id = "health-before-cleanup";
        let temp = tempfile::tempdir().unwrap();
        let descendant_pid_path = temp.path().join("health-reap-descendant.pid");
        let observed = std::sync::Arc::new(std::sync::Barrier::new(2));
        let release = std::sync::Arc::new(std::sync::Barrier::new(2));
        let script = format!(
            "IFS= read -r frame; sleep 30 & descendant=$!; printf '%s\\n' \"$descendant\" > '{}'; exit 0",
            descendant_pid_path.display()
        );
        let process = AgentProcess::launch_raw_protocol_fixture(&script).unwrap();
        process.install_pre_quiescence_barrier(
            std::sync::Arc::clone(&observed),
            std::sync::Arc::clone(&release),
        );
        let mut runtime = BwrapNanoRuntime::with_cas_dir(temp.path().join("cas"));
        let handle = insert_protocol_process(&mut runtime, workload_id, process);
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start_frame(invocation_id, unix_time_ms() + 10_000),
                },
            )
            .unwrap();

        let descendant_pid: u32 = wait_for_recorded_lines(&descendant_pid_path, 1)
            .trim()
            .parse()
            .unwrap();
        let mut stopped = false;
        for _ in 0..100 {
            if runtime.health(&handle).unwrap().state == NanoHealthState::Stopped {
                stopped = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(stopped, "health did not observe the exited exact Child");
        let signal_attempts = runtime.processes[workload_id].termination_signal_counter();
        assert_eq!(runtime.processes[workload_id].reap_publications(), 1);
        assert_eq!(
            signal_attempts.load(std::sync::atomic::Ordering::Acquire),
            0
        );
        let reaped = runtime.processes[workload_id].protocol_supervision_snapshot();
        assert!(reaped.process_reaped);
        assert!(reaped.stdin_closed);
        assert!(
            PathBuf::from(format!("/proc/{descendant_pid}")).exists(),
            "cgroup-owned descendant exited before cleanup retry"
        );

        // Health advanced the supervision state; the autonomous owner now
        // reaches its bounded cgroup-only quiescence boundary without polling.
        observed.wait();

        let first_cleanup = runtime.processes[workload_id].retry_supervised_cgroup_cleanup_with(
            "health-before-cleanup-cgroup",
            |_| Ok(vec![4242]),
            |_| Err(anyhow!("injected first cgroup kill failure")),
            |_| Ok(()),
        );
        assert!(first_cleanup.is_err());
        assert!(runtime.processes.contains_key(workload_id));
        assert!(
            !runtime.processes[workload_id]
                .protocol_supervision_snapshot()
                .cgroup_quiesced
        );
        assert_eq!(
            signal_attempts.load(std::sync::atomic::Ordering::Acquire),
            0
        );
        runtime.processes[workload_id]
            .retry_supervised_cgroup_cleanup_with(
                "health-before-cleanup-cgroup",
                |_| Ok(vec![4242]),
                |_| {
                    // This closure represents the authoritative cgroup.kill
                    // participant; it does not address the reaped supervisor.
                    let pid = i32::try_from(descendant_pid)
                        .context("fixture descendant PID exceeds pid_t")?;
                    nix::sys::signal::kill(
                        nix::unistd::Pid::from_raw(pid),
                        nix::sys::signal::Signal::SIGKILL,
                    )
                    .context("kill fixture cgroup descendant")?;
                    Ok(1)
                },
                |_| Ok(()),
            )
            .unwrap();
        assert_eq!(
            signal_attempts.load(std::sync::atomic::Ordering::Acquire),
            0
        );
        wait_until_pid_exits(descendant_pid);
        assert!(!PathBuf::from(format!("/proc/{descendant_pid}")).exists());

        release.wait();
        let quiesced = wait_for_autonomous_quiescence(&runtime, workload_id);
        assert_eq!(
            quiesced.failure,
            Some(ProtocolSupervisionFailure::ChannelDisconnected)
        );
        let terminal = poll_until_error(&mut runtime, &handle, invocation_id);
        assert_exec_error(&terminal, NanoExecErrorCode::ChannelDisconnected);
        let replay = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: poll_frame(invocation_id),
                },
            )
            .unwrap_err();
        assert_eq!(replay.to_string(), terminal.to_string());
        assert_eq!(
            signal_attempts.load(std::sync::atomic::Ordering::Acquire),
            0
        );
        assert!(!runtime.processes.contains_key(workload_id));
        assert!(!runtime.handles.contains_key(workload_id));
        assert!(!runtime.workloads.contains_key(workload_id));
    }

    #[test]
    fn confirmed_owned_reap_removes_only_the_cloned_numeric_signal_target() {
        let stored = SandboxHandle {
            agent_name: "post-reap-signal-target".to_string(),
            cgroup_created: true,
            cgroup_id: Some(17),
            io_available: true,
            bwrap_pid: Some(42),
            landlock_applied: true,
            network_isolated: true,
        };

        let post_reap = teardown_handle_after_owned_process_reap(stored.clone(), true, false);
        assert_eq!(post_reap.bwrap_pid, None);
        assert_eq!(stored.bwrap_pid, Some(42), "stored ownership was mutated");

        let pre_reap = teardown_handle_after_owned_process_reap(stored, false, false);
        assert_eq!(pre_reap.bwrap_pid, Some(42));
    }

    #[test]
    fn unacknowledged_cancel_kills_the_process_tree_without_a_later_poll() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2903";
        let temp = tempfile::tempdir().unwrap();
        let record_path = temp.path().join("protocol-input.jsonl");
        let descendant_pid_path = temp.path().join("descendant.pid");
        let process = AgentProcess::launch_recording_protocol_fixture(
            &[],
            &record_path,
            &descendant_pid_path,
        )
        .unwrap();
        let mut runtime = BwrapNanoRuntime::with_cas_dir(temp.path().join("cas"));
        let handle = insert_protocol_process(&mut runtime, "protocol-cancel", process);
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start_frame(invocation_id, unix_time_ms() + 10_000),
                },
            )
            .unwrap();
        let cancel = serde_json::json!({
            "kind": "cancel",
            "schema_version": 1,
            "invocation_id": invocation_id,
            "reason": "operator_cancelled"
        });
        let cancel_input = serde_json::to_string(&cancel).unwrap();
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_cancel".to_string(),
                    input: cancel_input.clone(),
                },
            )
            .unwrap();
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_cancel".to_string(),
                    input: cancel_input,
                },
            )
            .unwrap();
        let quiesced = wait_for_autonomous_quiescence(&runtime, "protocol-cancel");
        assert_eq!(
            quiesced.failure,
            Some(ProtocolSupervisionFailure::Cancelled)
        );
        let error = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: poll_frame(invocation_id),
                },
            )
            .unwrap_err();
        assert_exec_error(&error, NanoExecErrorCode::Cancelled);
        let replay = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: poll_frame(invocation_id),
                },
            )
            .unwrap_err();
        assert_exec_error(&replay, NanoExecErrorCode::Cancelled);
        assert_eq!(replay.to_string(), error.to_string());
        let recorded = wait_for_recorded_lines(&record_path, 2);
        assert_eq!(recorded.lines().count(), 2, "cancel replay wrote to child");
        let descendant_pid: u32 = std::fs::read_to_string(&descendant_pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        wait_until_pid_exits(descendant_pid);
        assert!(
            !PathBuf::from(format!("/proc/{descendant_pid}")).exists(),
            "descendant survived forced cancellation"
        );
        assert_eq!(
            runtime.health(&handle).unwrap().state,
            NanoHealthState::Stopped
        );
    }

    #[test]
    fn acknowledged_cancel_is_terminal_replayable_and_releases_the_process() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2912";
        let cancelled = format!(
            r#"{{"kind":"cancelled","schema_version":1,"invocation_id":"{invocation_id}"}}"#
        );
        let mut runtime = BwrapNanoRuntime::with_cas_dir(tempfile::tempdir().unwrap().path());
        let script = format!(
            "IFS= read -r execute; IFS= read -r cancel; printf '%s\\n' '{}'; sleep 5",
            cancelled
        );
        let process = AgentProcess::launch_raw_protocol_fixture(&script).unwrap();
        let handle = insert_protocol_process(&mut runtime, "protocol-cancelled", process);
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start_frame(invocation_id, unix_time_ms() + 10_000),
                },
            )
            .unwrap();
        let cancel = serde_json::json!({
            "kind": "cancel",
            "schema_version": 1,
            "invocation_id": invocation_id,
            "reason": "operator_cancelled"
        });
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_cancel".to_string(),
                    input: serde_json::to_string(&cancel).unwrap(),
                },
            )
            .unwrap();

        let mut terminal = None;
        for _ in 0..100 {
            let output = runtime
                .exec(
                    &handle,
                    NanoExecRequest {
                        operation: "workbench_poll".to_string(),
                        input: poll_frame(invocation_id),
                    },
                )
                .unwrap();
            if output.output.contains("completed") {
                terminal = Some(output);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let terminal = terminal.expect("cancelled exchange did not become terminal");
        assert_eq!(
            runtime.health(&handle).unwrap().state,
            NanoHealthState::Stopped
        );
        let replay = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: poll_frame(invocation_id),
                },
            )
            .unwrap();
        assert_eq!(replay.output, terminal.output);
    }

    #[test]
    fn completed_frame_is_not_published_before_post_terminal_validation() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2933";
        let start = start_frame(invocation_id, unix_time_ms() + 10_000);
        let result =
            successful_result_frame(invocation_id, &frame_digest(&start), serde_json::json!({}));
        let completed = completed_progress_frame(invocation_id);
        let extra = serde_json::json!({
            "kind": "progress",
            "schema_version": 1,
            "invocation_id": invocation_id,
            "stage": "after-completed"
        })
        .to_string();
        let observed = std::sync::Arc::new(std::sync::Barrier::new(2));
        let release = std::sync::Arc::new(std::sync::Barrier::new(2));
        let process =
            AgentProcess::launch_protocol_fixture(&[&result, &completed, &extra]).unwrap();
        process.install_post_terminal_barrier(
            std::sync::Arc::clone(&observed),
            std::sync::Arc::clone(&release),
        );
        let mut runtime = BwrapNanoRuntime::with_cas_dir(tempfile::tempdir().unwrap().path());
        let handle = insert_protocol_process(&mut runtime, "completed-extra", process);
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start,
                },
            )
            .unwrap();

        observed.wait();
        let pending = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: poll_frame(invocation_id),
                },
            )
            .unwrap();
        let pending: serde_json::Value = serde_json::from_str(&pending.output).unwrap();
        assert_eq!(pending["state"], "pending");
        assert_ne!(pending["state"], "finalizing");
        assert!(matches!(
            pending["state"].as_str(),
            Some("accepted" | "pending" | "cancelling" | "completed")
        ));
        assert_eq!(pending["messages"], serde_json::json!([]));
        release.wait();

        let quiesced = wait_for_autonomous_quiescence(&runtime, "completed-extra");
        assert_eq!(
            quiesced.failure,
            Some(ProtocolSupervisionFailure::ProtocolViolation)
        );
        let terminal = poll_until_error(&mut runtime, &handle, invocation_id);
        assert_exec_error(&terminal, NanoExecErrorCode::ProtocolViolation);
        let replay = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: poll_frame(invocation_id),
                },
            )
            .unwrap_err();
        assert_eq!(replay.to_string(), terminal.to_string());
    }

    #[test]
    fn duplicate_terminal_and_output_overflow_fail_with_typed_safe_errors() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2913";
        let terminal =
            format!(r#"{{"kind":"error","schema_version":1,"invocation_id":"{invocation_id}"}}"#);
        let mut runtime = BwrapNanoRuntime::with_cas_dir(tempfile::tempdir().unwrap().path());
        let handle = insert_protocol_fixture(
            &mut runtime,
            "protocol-duplicate-terminal",
            &[&terminal, &terminal],
        );
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start_frame(invocation_id, unix_time_ms() + 10_000),
                },
            )
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let error = poll_until_error(&mut runtime, &handle, invocation_id);
        assert_exec_error(&error, NanoExecErrorCode::ProtocolViolation);

        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2914";
        let output_chunk = serde_json::json!({
            "kind": "progress",
            "schema_version": 1,
            "invocation_id": invocation_id,
            "stage": "running",
            "padding": "x".repeat(MAX_WORKBENCH_OUTPUT_BYTES / 3)
        })
        .to_string();
        let mut runtime = BwrapNanoRuntime::with_cas_dir(tempfile::tempdir().unwrap().path());
        let handle = insert_protocol_fixture(
            &mut runtime,
            "protocol-output-overflow",
            &[&output_chunk, &output_chunk, &output_chunk, &output_chunk],
        );
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start_frame(invocation_id, unix_time_ms() + 10_000),
                },
            )
            .unwrap();
        let error = poll_until_error(&mut runtime, &handle, invocation_id);
        assert_exec_error(&error, NanoExecErrorCode::OutputLimitExceeded);
        assert!(!error.to_string().contains(&"x".repeat(32)));
    }

    #[test]
    fn invalid_child_protocol_variants_fail_closed() {
        let cases = [
            (
                "protocol-child-v2",
                "018f3f32-4f01-7f2c-a6c1-f6f4a81b2920",
                "result",
                2,
                None,
                NanoExecErrorCode::UnsupportedVersion,
            ),
            (
                "protocol-unknown-kind",
                "018f3f32-4f01-7f2c-a6c1-f6f4a81b2921",
                "unknown",
                1,
                None,
                NanoExecErrorCode::ProtocolViolation,
            ),
            (
                "protocol-premature-completed",
                "018f3f32-4f01-7f2c-a6c1-f6f4a81b2922",
                "progress",
                1,
                Some("completed"),
                NanoExecErrorCode::ProtocolViolation,
            ),
            (
                "protocol-unsolicited-cancelled",
                "018f3f32-4f01-7f2c-a6c1-f6f4a81b2923",
                "cancelled",
                1,
                None,
                NanoExecErrorCode::ProtocolViolation,
            ),
        ];

        for (workload_id, invocation_id, kind, version, stage, expected) in cases {
            let mut frame = serde_json::json!({
                "kind": kind,
                "schema_version": version,
                "invocation_id": invocation_id
            });
            if let Some(stage) = stage {
                frame["stage"] = serde_json::json!(stage);
            }
            let frame = serde_json::to_string(&frame).unwrap();
            let mut runtime = BwrapNanoRuntime::with_cas_dir(tempfile::tempdir().unwrap().path());
            let handle = insert_protocol_fixture(&mut runtime, workload_id, &[&frame]);
            runtime
                .exec(
                    &handle,
                    NanoExecRequest {
                        operation: "workbench_start".to_string(),
                        input: start_frame(invocation_id, unix_time_ms() + 10_000),
                    },
                )
                .unwrap();
            wait_for_autonomous_quiescence(&runtime, workload_id);
            let error = poll_until_error(&mut runtime, &handle, invocation_id);
            assert_exec_error(&error, expected);
            let replay = runtime
                .exec(
                    &handle,
                    NanoExecRequest {
                        operation: "workbench_poll".to_string(),
                        input: poll_frame(invocation_id),
                    },
                )
                .unwrap_err();
            assert_eq!(replay.to_string(), error.to_string());
            assert_eq!(
                runtime.health(&handle).unwrap().state,
                NanoHealthState::Stopped
            );
        }
    }

    #[test]
    fn pre_execute_reader_failure_rejects_without_a_child_write() {
        let cases = [
            (
                "protocol-preclosed",
                "exec 1>&-; IFS= read -r frame && printf '%s\\n' \"$frame\" > '{record}'; sleep 5",
                ProtocolSupervisionFailure::ChannelDisconnected,
                NanoExecErrorCode::ChannelDisconnected,
            ),
            (
                "protocol-preinvalid",
                "printf 'not-json\\n'; IFS= read -r frame && printf '%s\\n' \"$frame\" > '{record}'; sleep 5",
                ProtocolSupervisionFailure::InvalidFrame,
                NanoExecErrorCode::InvalidFrame,
            ),
        ];

        for (workload_id, script, expected_supervision, expected_error) in cases {
            let temp = tempfile::tempdir().unwrap();
            let record_path = temp.path().join("unexpected-input.jsonl");
            let script = script.replace("{record}", &record_path.display().to_string());
            let process = AgentProcess::launch_raw_protocol_fixture(&script).unwrap();
            let mut runtime = BwrapNanoRuntime::with_cas_dir(temp.path().join("cas"));
            let handle = insert_protocol_process(&mut runtime, workload_id, process);
            assert_eq!(
                wait_for_protocol_start_failure(&runtime, workload_id),
                expected_supervision
            );

            let error = runtime
                .exec(
                    &handle,
                    NanoExecRequest {
                        operation: "workbench_start".to_string(),
                        input: start_frame(
                            "018f3f32-4f01-7f2c-a6c1-f6f4a81b2950",
                            unix_time_ms() + 10_000,
                        ),
                    },
                )
                .unwrap_err();
            assert_exec_error(&error, expected_error);
            assert!(
                !record_path.exists(),
                "pre-execute reader failure still allowed a child write"
            );
        }
    }

    #[test]
    fn invalid_utf8_and_endless_no_newline_overflow_fail_closed_without_poll() {
        let temp = tempfile::tempdir().unwrap();
        let overflow_descendant = temp.path().join("overflow-descendant.pid");
        let overflow_script = format!(
            "IFS= read -r frame; sleep 30 & descendant=$!; printf '%s\\n' \"$descendant\" > '{}'; head -c 1048577 /dev/zero | tr '\\000' x; wait \"$descendant\"",
            overflow_descendant.display()
        );
        let cases = [
            (
                "protocol-invalid-utf8",
                "018f3f32-4f01-7f2c-a6c1-f6f4a81b2924",
                r#"IFS= read -r frame; printf '\377\n'; sleep 5"#.to_string(),
                NanoExecErrorCode::InvalidFrame,
                None,
            ),
            (
                "protocol-frame-overflow",
                "018f3f32-4f01-7f2c-a6c1-f6f4a81b2925",
                overflow_script,
                NanoExecErrorCode::OutputLimitExceeded,
                Some(overflow_descendant),
            ),
        ];

        for (workload_id, invocation_id, script, expected, descendant_path) in cases {
            let process = AgentProcess::launch_raw_protocol_fixture(&script).unwrap();
            let mut runtime = BwrapNanoRuntime::with_cas_dir(tempfile::tempdir().unwrap().path());
            let handle = insert_protocol_process(&mut runtime, workload_id, process);
            runtime
                .exec(
                    &handle,
                    NanoExecRequest {
                        operation: "workbench_start".to_string(),
                        input: start_frame(invocation_id, unix_time_ms() + 10_000),
                    },
                )
                .unwrap();
            wait_for_autonomous_quiescence(&runtime, workload_id);
            if let Some(descendant_path) = descendant_path {
                let descendant_pid: u32 = std::fs::read_to_string(descendant_path)
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap();
                wait_until_pid_exits(descendant_pid);
                assert!(!PathBuf::from(format!("/proc/{descendant_pid}")).exists());
            }
            let error = poll_until_error(&mut runtime, &handle, invocation_id);
            assert_exec_error(&error, expected);
            let replay = runtime
                .exec(
                    &handle,
                    NanoExecRequest {
                        operation: "workbench_poll".to_string(),
                        input: poll_frame(invocation_id),
                    },
                )
                .unwrap_err();
            assert_eq!(replay.to_string(), error.to_string());
            assert_eq!(
                runtime.health(&handle).unwrap().state,
                NanoHealthState::Stopped
            );
        }
    }

    #[test]
    fn reader_queue_overflow_reaps_without_a_later_poll() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2915";
        let mut frames: Vec<String> = (0..=64)
            .map(|sequence| {
                serde_json::json!({
                    "kind": "progress",
                    "schema_version": 1,
                    "invocation_id": invocation_id,
                    "stage": "running",
                    "sequence": sequence
                })
                .to_string()
            })
            .collect();
        frames.push(
            serde_json::json!({
                "kind": "result",
                "schema_version": 1,
                "invocation_id": invocation_id
            })
            .to_string(),
        );
        frames.push(
            serde_json::json!({
                "kind": "progress",
                "schema_version": 1,
                "invocation_id": invocation_id,
                "stage": "completed"
            })
            .to_string(),
        );
        let frame_refs: Vec<&str> = frames.iter().map(String::as_str).collect();
        let temp = tempfile::tempdir().unwrap();
        let record_path = temp.path().join("queue-input.jsonl");
        let descendant_pid_path = temp.path().join("queue-descendant.pid");
        let process = AgentProcess::launch_recording_protocol_fixture(
            &frame_refs,
            &record_path,
            &descendant_pid_path,
        )
        .unwrap();
        let mut runtime = BwrapNanoRuntime::with_cas_dir(temp.path().join("cas"));
        let handle = insert_protocol_process(&mut runtime, "protocol-backpressure", process);
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start_frame(invocation_id, unix_time_ms() + 10_000),
                },
            )
            .unwrap();

        for _ in 0..1_000 {
            if runtime.processes["protocol-backpressure"].protocol_queue_overflowed() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(runtime.processes["protocol-backpressure"].protocol_queue_overflowed());
        wait_for_autonomous_quiescence(&runtime, "protocol-backpressure");
        let descendant_pid: u32 = std::fs::read_to_string(&descendant_pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        wait_until_pid_exits(descendant_pid);
        assert!(!PathBuf::from(format!("/proc/{descendant_pid}")).exists());
        let error = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: poll_frame(invocation_id),
                },
            )
            .unwrap_err();
        assert_exec_error(&error, NanoExecErrorCode::OutputLimitExceeded);
        let replay = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: poll_frame(invocation_id),
                },
            )
            .unwrap_err();
        assert_eq!(replay.to_string(), error.to_string());
        assert_eq!(
            runtime.health(&handle).unwrap().state,
            NanoHealthState::Stopped
        );
    }

    #[test]
    fn protocol_metacharacters_remain_stdin_data_and_are_never_reparsed() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2916";
        let temp = tempfile::tempdir().unwrap();
        let record_path = temp.path().join("protocol-input.jsonl");
        let descendant_pid_path = temp.path().join("descendant.pid");
        let injected_path = temp.path().join("must-not-exist");
        let process = AgentProcess::launch_recording_protocol_fixture(
            &[],
            &record_path,
            &descendant_pid_path,
        )
        .unwrap();
        let mut runtime = BwrapNanoRuntime::with_cas_dir(temp.path().join("cas"));
        let handle = insert_protocol_process(&mut runtime, "protocol-data-only", process);
        let mut start: serde_json::Value =
            serde_json::from_str(&start_frame(invocation_id, unix_time_ms() + 10_000)).unwrap();
        start["request"]["opaque"] = serde_json::json!(format!(
            "$(touch {}) ; `touch {}`",
            injected_path.display(),
            injected_path.display()
        ));
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: serde_json::to_string(&start).unwrap(),
                },
            )
            .unwrap();
        for _ in 0..100 {
            if record_path.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            record_path.exists(),
            "fixture did not receive execute frame"
        );
        assert!(
            !injected_path.exists(),
            "protocol content reached a shell parser"
        );
        runtime.stop(&handle).unwrap();
    }

    #[test]
    fn deadline_expiry_sends_one_cancel_and_reaps_without_a_later_poll() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2904";
        let temp = tempfile::tempdir().unwrap();
        let record_path = temp.path().join("protocol-input.jsonl");
        let descendant_pid_path = temp.path().join("descendant.pid");
        let process = AgentProcess::launch_recording_protocol_fixture(
            &[],
            &record_path,
            &descendant_pid_path,
        )
        .unwrap();
        let mut runtime = BwrapNanoRuntime::with_cas_dir(temp.path().join("cas"));
        let handle = insert_protocol_process(&mut runtime, "protocol-deadline", process);
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start_frame(invocation_id, unix_time_ms() + 20),
                },
            )
            .unwrap();
        let quiesced = wait_for_autonomous_quiescence(&runtime, "protocol-deadline");
        assert_eq!(
            quiesced.failure,
            Some(ProtocolSupervisionFailure::DeadlineExceeded)
        );
        let error = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: poll_frame(invocation_id),
                },
            )
            .unwrap_err();
        assert_exec_error(&error, NanoExecErrorCode::DeadlineExceeded);
        let replay = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: poll_frame(invocation_id),
                },
            )
            .unwrap_err();
        assert_exec_error(&replay, NanoExecErrorCode::DeadlineExceeded);
        assert_eq!(replay.to_string(), error.to_string());
        let recorded = wait_for_recorded_lines(&record_path, 2);
        assert_eq!(
            recorded.lines().count(),
            2,
            "deadline retry wrote more than one cancel frame"
        );
        let descendant_pid: u32 = std::fs::read_to_string(&descendant_pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        wait_until_pid_exits(descendant_pid);
        assert!(!PathBuf::from(format!("/proc/{descendant_pid}")).exists());
    }

    #[test]
    fn deadline_claim_is_stable_across_send_barrier_and_explicit_replay() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2934";
        let temp = tempfile::tempdir().unwrap();
        let record_path = temp.path().join("deadline-race-input.jsonl");
        let descendant_pid_path = temp.path().join("deadline-race-descendant.pid");
        let claimed = std::sync::Arc::new(std::sync::Barrier::new(2));
        let release = std::sync::Arc::new(std::sync::Barrier::new(2));
        let process = AgentProcess::launch_recording_protocol_fixture(
            &[],
            &record_path,
            &descendant_pid_path,
        )
        .unwrap();
        process.install_deadline_send_barrier(
            std::sync::Arc::clone(&claimed),
            std::sync::Arc::clone(&release),
        );
        let mut runtime = BwrapNanoRuntime::with_cas_dir(temp.path().join("cas"));
        let handle = insert_protocol_process(&mut runtime, "deadline-send-race", process);
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start_frame(invocation_id, unix_time_ms() + 20),
                },
            )
            .unwrap();

        claimed.wait();
        let before_cancel = wait_for_recorded_lines(&record_path, 1);
        assert_eq!(
            before_cancel.lines().count(),
            1,
            "deadline action preceded or duplicated the initial execute frame"
        );
        let first: serde_json::Value =
            serde_json::from_str(before_cancel.lines().next().unwrap()).unwrap();
        assert_eq!(first["kind"], "execute");
        let claimed_snapshot =
            runtime.processes["deadline-send-race"].protocol_supervision_snapshot();
        assert_eq!(
            claimed_snapshot.cancel_owner,
            Some(ProtocolCancelOwner::Deadline)
        );
        assert!(!claimed_snapshot.cancel_sent);
        let cancel = serde_json::json!({
            "kind": "cancel",
            "schema_version": 1,
            "invocation_id": invocation_id,
            "reason": "operator_cancelled"
        })
        .to_string();
        let first = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_cancel".to_string(),
                    input: cancel.clone(),
                },
            )
            .unwrap();
        let replay = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_cancel".to_string(),
                    input: cancel,
                },
            )
            .unwrap();
        assert_eq!(replay.output, first.output);

        release.wait();
        let quiesced = wait_for_autonomous_quiescence(&runtime, "deadline-send-race");
        assert_eq!(
            quiesced.failure,
            Some(ProtocolSupervisionFailure::DeadlineExceeded)
        );
        let recorded = wait_for_recorded_lines(&record_path, 2);
        assert_eq!(
            recorded.lines().count(),
            2,
            "deadline ownership must emit exactly one child cancel frame"
        );
        let second: serde_json::Value =
            serde_json::from_str(recorded.lines().nth(1).unwrap()).unwrap();
        assert_eq!(second["kind"], "cancel");
        let terminal = poll_until_error(&mut runtime, &handle, invocation_id);
        assert_exec_error(&terminal, NanoExecErrorCode::DeadlineExceeded);
        let terminal_replay = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: poll_frame(invocation_id),
                },
            )
            .unwrap_err();
        assert_eq!(terminal_replay.to_string(), terminal.to_string());
    }

    #[test]
    fn acknowledged_deadline_cancel_is_retained_replayable_and_cleans_once() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2951";
        let cancelled = serde_json::json!({
            "kind": "cancelled",
            "schema_version": 1,
            "invocation_id": invocation_id
        })
        .to_string();
        let temp = tempfile::tempdir().unwrap();
        let record_path = temp.path().join("deadline-ack-input.jsonl");
        let script = format!(
            "record='{}'; IFS= read -r execute; printf '%s\\n' \"$execute\" >> \"$record\"; IFS= read -r cancel; printf '%s\\n' \"$cancel\" >> \"$record\"; printf '%s\\n' '{}'; sleep 5",
            record_path.display(),
            cancelled
        );
        let process = AgentProcess::launch_raw_protocol_fixture(&script).unwrap();
        let mut runtime = BwrapNanoRuntime::with_cas_dir(temp.path().join("cas"));
        let handle = insert_protocol_process(&mut runtime, "deadline-ack", process);
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start_frame(invocation_id, unix_time_ms() + 20),
                },
            )
            .unwrap();

        let mut terminal = None;
        for _ in 0..400 {
            let output = runtime
                .exec(
                    &handle,
                    NanoExecRequest {
                        operation: "workbench_poll".to_string(),
                        input: poll_frame(invocation_id),
                    },
                )
                .unwrap();
            if output.output.contains("completed") {
                terminal = Some(output);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let terminal = terminal.expect("deadline acknowledgement did not become terminal");
        let envelope: serde_json::Value = serde_json::from_str(&terminal.output).unwrap();
        assert_eq!(envelope["state"], "completed");
        assert_eq!(envelope["messages"][0]["kind"], "cancelled");
        let replay = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: poll_frame(invocation_id),
                },
            )
            .unwrap();
        assert_eq!(replay.output, terminal.output);
        let recorded = wait_for_recorded_lines(&record_path, 2);
        assert_eq!(recorded.lines().count(), 2);
        let kinds: Vec<_> = recorded
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()["kind"].clone())
            .collect();
        assert_eq!(
            kinds,
            vec![serde_json::json!("execute"), serde_json::json!("cancel")]
        );
        assert_eq!(
            runtime.health(&handle).unwrap().state,
            NanoHealthState::Stopped
        );
    }

    #[test]
    fn protocol_eof_reaps_without_poll_and_retains_a_stable_failure() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2905";
        let temp = tempfile::tempdir().unwrap();
        let descendant_pid_path = temp.path().join("eof-descendant.pid");
        let script = format!(
            "IFS= read -r frame; sleep 30 </dev/null >/dev/null 2>&1 & descendant=$!; printf '%s\\n' \"$descendant\" > '{}'; exec 1>&-; wait \"$descendant\"",
            descendant_pid_path.display()
        );
        let mut runtime = BwrapNanoRuntime::with_cas_dir(temp.path().join("cas"));
        let handle = insert_protocol_process(
            &mut runtime,
            "protocol-eof",
            AgentProcess::launch_raw_protocol_fixture(&script).unwrap(),
        );
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start_frame(invocation_id, unix_time_ms() + 10_000),
                },
            )
            .unwrap();
        wait_for_autonomous_quiescence(&runtime, "protocol-eof");
        let descendant_pid: u32 = std::fs::read_to_string(&descendant_pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        wait_until_pid_exits(descendant_pid);
        assert!(!PathBuf::from(format!("/proc/{descendant_pid}")).exists());
        let failure = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: poll_frame(invocation_id),
                },
            )
            .unwrap_err();
        assert_exec_error(&failure, NanoExecErrorCode::ChannelDisconnected);
        assert_eq!(
            runtime.health(&handle).unwrap().state,
            NanoHealthState::Stopped
        );
        let replay = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: poll_frame(invocation_id),
                },
            )
            .unwrap_err();
        assert_exec_error(&replay, NanoExecErrorCode::ChannelDisconnected);
        assert_eq!(replay.to_string(), failure.to_string());
    }

    #[test]
    fn valid_terminal_without_jsonl_newline_is_rejected_at_eof() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2952";
        let terminal = serde_json::json!({
            "kind": "error",
            "schema_version": 1,
            "invocation_id": invocation_id
        })
        .to_string();
        let script = format!("IFS= read -r execute; printf '%s' '{}'", terminal);
        let mut runtime = BwrapNanoRuntime::with_cas_dir(tempfile::tempdir().unwrap().path());
        let handle = insert_protocol_process(
            &mut runtime,
            "protocol-unterminated-terminal",
            AgentProcess::launch_raw_protocol_fixture(&script).unwrap(),
        );
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start_frame(invocation_id, unix_time_ms() + 10_000),
                },
            )
            .unwrap();
        let quiesced = wait_for_autonomous_quiescence(&runtime, "protocol-unterminated-terminal");
        assert_eq!(
            quiesced.failure,
            Some(ProtocolSupervisionFailure::InvalidFrame)
        );
        let error = poll_until_error(&mut runtime, &handle, invocation_id);
        assert_exec_error(&error, NanoExecErrorCode::InvalidFrame);
        let replay = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: poll_frame(invocation_id),
                },
            )
            .unwrap_err();
        assert_eq!(replay.to_string(), error.to_string());
    }

    #[test]
    fn stop_fixture_reaps_process_releases_cas_and_preserves_other_workload() {
        let temp = tempfile::tempdir().unwrap();
        let cas_dir = temp.path().join("cas");
        let mut runtime = BwrapNanoRuntime::with_cas_dir(&cas_dir);
        let home_a = temp.path().join("home-a");
        let home_b = temp.path().join("home-b");
        std::fs::create_dir_all(&home_a).unwrap();
        std::fs::create_dir_all(&home_b).unwrap();
        std::fs::write(home_a.join("owned.txt"), b"workload a content").unwrap();
        std::fs::write(home_b.join("owned.txt"), b"workload b content").unwrap();
        let plane = runtime.open_plane().unwrap();
        let owned_a = home_manifest::walk_home(&home_a, &plane)
            .unwrap()
            .owned_object_ids;
        let owned_b = home_manifest::walk_home(&home_b, &plane)
            .unwrap()
            .owned_object_ids;
        assert!(!owned_a.is_empty());
        assert!(!owned_b.is_empty());
        drop(plane);

        let handle_a = insert_fixture(
            &mut runtime,
            fixture_workload("fixture-a", "agent-fixture-a"),
            owned_a.clone(),
        );
        let handle_b = insert_fixture(
            &mut runtime,
            fixture_workload("fixture-b", "agent-fixture-b"),
            owned_b.clone(),
        );

        let stale_for_b = NanoHandle {
            instance_id: handle_a.instance_id,
            ..handle_b.clone()
        };
        assert!(runtime.stop(&stale_for_b).is_err());

        let stopped = runtime.stop(&handle_a).unwrap();
        assert_eq!(
            stopped.outcome,
            sentinel_common::nano_runtime::NanoStopOutcome::Stopped
        );
        assert!(!PathBuf::from(format!("/proc/{}", handle_a.pid.unwrap())).exists());
        assert_eq!(
            runtime.health(&handle_a).unwrap().state,
            NanoHealthState::Stopped
        );
        assert!(matches!(
            runtime.health(&handle_b).unwrap().state,
            NanoHealthState::Healthy | NanoHealthState::Degraded
        ));
        let plane = runtime.open_plane().unwrap();
        for object_id in owned_a {
            assert!(plane.get_object(object_id).unwrap().is_none());
        }
        for object_id in &owned_b {
            assert!(plane.get_object(*object_id).unwrap().is_some());
        }
        drop(plane);

        let replay = runtime.stop(&handle_a).unwrap();
        assert_eq!(
            replay.outcome,
            sentinel_common::nano_runtime::NanoStopOutcome::AlreadyStopped
        );
        assert_eq!(
            runtime.stop(&handle_b).unwrap().outcome,
            sentinel_common::nano_runtime::NanoStopOutcome::Stopped
        );
        let plane = runtime.open_plane().unwrap();
        for object_id in owned_b {
            assert!(plane.get_object(object_id).unwrap().is_none());
        }
    }

    #[test]
    fn failed_cleanup_retains_ownership_for_retry() {
        let temp = tempfile::tempdir().unwrap();
        let mut runtime = BwrapNanoRuntime::with_test_dirs(
            temp.path().join("cas"),
            temp.path().join("agent-homes"),
        );
        let source_home = temp.path().join("retry-home");
        std::fs::create_dir_all(&source_home).unwrap();
        std::fs::write(source_home.join("owned.txt"), b"retry-owned content").unwrap();
        let plane = runtime.open_plane().unwrap();
        let owned_object_ids = home_manifest::walk_home(&source_home, &plane)
            .unwrap()
            .owned_object_ids;
        assert!(!owned_object_ids.is_empty());
        drop(plane);
        let handle = insert_fixture(
            &mut runtime,
            fixture_workload("fixture-retry", "agent-fixture-retry"),
            owned_object_ids.clone(),
        );
        runtime
            .write_marker("agent-fixture-retry", "different-workload")
            .unwrap();

        assert!(runtime.stop(&handle).is_err());
        assert!(runtime.processes.contains_key(&handle.workload_id));
        assert!(runtime.handles.contains_key(&handle.workload_id));
        assert!(runtime.workloads.contains_key(&handle.workload_id));
        let plane = runtime.open_plane().unwrap();
        for object_id in &owned_object_ids {
            assert!(plane.get_object(*object_id).unwrap().is_none());
        }
        drop(plane);

        std::fs::write(
            runtime
                .home_dir("agent-fixture-retry")
                .join(".nano-runtime"),
            handle.workload_id.as_bytes(),
        )
        .unwrap();
        assert_eq!(
            runtime.stop(&handle).unwrap().outcome,
            sentinel_common::nano_runtime::NanoStopOutcome::Stopped
        );
        assert!(!runtime.processes.contains_key(&handle.workload_id));
        assert!(!runtime.handles.contains_key(&handle.workload_id));
        assert!(!runtime.workloads.contains_key(&handle.workload_id));
    }

    #[test]
    fn duplicate_agent_home_is_rejected_without_touching_owner() {
        let temp = tempfile::tempdir().unwrap();
        let mut runtime = BwrapNanoRuntime::with_cas_dir(temp.path().join("cas"));
        let owner = insert_fixture(
            &mut runtime,
            fixture_workload("fixture-owner", "shared-agent-home"),
            Vec::new(),
        );
        let alias = fixture_workload("fixture-alias", "shared-agent-home");

        assert!(runtime.ensure_workload_available(&alias).is_err());
        assert!(runtime
            .ensure_restore_target_available(&alias.workload_id, &alias)
            .is_err());
        assert!(runtime.processes.contains_key(&owner.workload_id));
        assert!(runtime.workloads.contains_key(&owner.workload_id));
        assert_eq!(
            runtime.stop(&owner).unwrap().outcome,
            sentinel_common::nano_runtime::NanoStopOutcome::Stopped
        );
    }

    #[test]
    fn restore_rejects_mismatched_workload_identity_without_touching_owner() {
        let temp = tempfile::tempdir().unwrap();
        let mut runtime = BwrapNanoRuntime::with_cas_dir(temp.path().join("cas"));
        let owner = insert_fixture(
            &mut runtime,
            fixture_workload("fixture-owner", "fixture-owner-home"),
            Vec::new(),
        );
        let payload = fixture_workload("payload-workload", "payload-home");

        assert!(runtime
            .ensure_restore_target_available("envelope-workload", &payload)
            .is_err());
        assert!(runtime.processes.contains_key(&owner.workload_id));
        assert!(runtime.workloads.contains_key(&owner.workload_id));
        assert_eq!(
            runtime.stop(&owner).unwrap().outcome,
            sentinel_common::nano_runtime::NanoStopOutcome::Stopped
        );
    }

    /// N5 + AC-1 at the adapter level: the bwrap snapshot representation is a
    /// metadata-aware CAS manifest (not file bytes), and it is deterministic — a
    /// re-walk of an identical home yields a serde-equal manifest, so the
    /// conformance harness's `after.payload == before.payload` holds. This
    /// exercises the rewired snapshot/restore data path without needing a real
    /// bwrap spawn (which is the `#[ignore]`d host conformance test).
    #[test]
    fn home_manifest_is_deterministic_and_block_ref_based() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = BwrapNanoRuntime::with_cas_dir(tmp.path().join("cas"));
        let home = tmp.path().join("home");
        std::fs::create_dir_all(home.join("d")).unwrap();
        std::fs::write(home.join("d/f.txt"), b"deterministic agent-home content").unwrap();
        std::os::unix::fs::symlink("d/f.txt", home.join("link")).unwrap();

        let plane = rt.open_plane().unwrap();
        let m1 = home_manifest::walk_home(&home, &plane).unwrap().manifest;
        let m2 = home_manifest::walk_home(&home, &plane).unwrap().manifest;
        assert_eq!(
            serde_json::to_value(&m1).unwrap(),
            serde_json::to_value(&m2).unwrap(),
            "bwrap home manifest must be deterministic (N5 payload stability)"
        );

        // AC-1: the file entry carries BLAKE3-128 chunk refs, not bytes.
        let file = m1
            .entries
            .iter()
            .find(|e| e.rel_path_bytes == b"d/f.txt")
            .expect("file entry present");
        assert_eq!(file.kind, home_manifest::EntryKind::File);
        assert!(!file.content.is_empty());
        assert!(!file.content[0].chunk_refs.is_empty());

        // The snapshot payload embeds the manifest, never a raw byte map: the
        // type system guarantees this (BwrapSnapshotPayload.home_manifest), and
        // the serialized form shows the manifest field and no `home_files`.
        let workload: NanoWorkloadSpec = serde_json::from_value(serde_json::json!({
            "workload_id": "w-test",
            "agent_name": "agent-test",
        }))
        .unwrap();
        let payload = BwrapSnapshotPayload {
            workload,
            command: vec![],
            home_manifest: m1,
            cgroup_created: false,
            io_available: false,
            bwrap_available: false,
            landlock_available: false,
            semantics_note: String::new(),
        };
        let value = serde_json::to_value(&payload).unwrap();
        assert!(value.get("home_manifest").is_some());
        assert!(value.get("home_files").is_none(), "no raw byte map remains");
    }
}
