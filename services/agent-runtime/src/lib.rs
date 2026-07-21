//! Capability-scoped tool executor used inside the bwrap agent sandbox.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sentinel_common::{
    WorkbenchArtifactRef, WorkbenchErrorClass, WorkbenchErrorInfo, WorkbenchMessage,
    WorkbenchOutcome, WorkbenchRequest, WorkbenchResourceUsage, WorkbenchTool,
    WORKBENCH_SCHEMA_VERSION,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(10);
const SAFE_ENVIRONMENT: [(&str, &str); 4] = [
    ("HOME", "/workspace"),
    ("LANG", "C.UTF-8"),
    ("LC_ALL", "C.UTF-8"),
    ("PATH", "/usr/bin:/bin"),
];

#[derive(Debug, Clone)]
pub struct WorkbenchExecutor {
    workspace_root: PathBuf,
    artifact_root: PathBuf,
}

impl WorkbenchExecutor {
    pub fn new(workspace_root: impl Into<PathBuf>, artifact_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            artifact_root: artifact_root.into(),
        }
    }

    pub fn execute(
        &self,
        request: WorkbenchRequest,
        cancelled: Arc<AtomicBool>,
    ) -> WorkbenchMessage {
        let started = Instant::now();
        let now_ms = unix_time_ms();
        if let Err(error) = request.validate_at(now_ms) {
            return failure_message(
                &request,
                started,
                WorkbenchOutcome::Failed,
                ExecutionError::new(
                    WorkbenchErrorClass::Authorization,
                    "request_rejected",
                    &error.to_string(),
                    false,
                ),
            );
        }
        if request.runtime_key != sentinel_common::WORKBENCH_RUNTIME_BWRAP {
            return failure_message(
                &request,
                started,
                WorkbenchOutcome::Failed,
                ExecutionError::new(
                    WorkbenchErrorClass::Runtime,
                    "secure_runtime_required",
                    "the M0 workbench requires the bwrap runtime",
                    false,
                ),
            );
        }
        if cancelled.load(Ordering::Acquire) {
            return failure_message(
                &request,
                started,
                WorkbenchOutcome::Cancelled,
                ExecutionError::new(
                    WorkbenchErrorClass::Runtime,
                    "cancelled",
                    "invocation was cancelled before execution",
                    false,
                ),
            );
        }

        let result = self.execute_validated(&request, &cancelled);
        match result {
            Ok(success) => WorkbenchMessage::Result {
                schema_version: WORKBENCH_SCHEMA_VERSION,
                invocation_id: request.invocation_id,
                input_digest: request.input_digest,
                outcome: WorkbenchOutcome::Succeeded,
                resources: WorkbenchResourceUsage {
                    duration_ms: elapsed_ms(started),
                    bytes_read: success.bytes_read,
                    bytes_written: success.bytes_written,
                    artifact_bytes: success.artifact_bytes,
                    ..WorkbenchResourceUsage::default()
                },
                artifacts: success.artifacts,
                output: success.output,
                error: None,
            },
            Err(error) => {
                let outcome = match error.code {
                    "cancelled" => WorkbenchOutcome::Cancelled,
                    "deadline_expired" => WorkbenchOutcome::TimedOut,
                    "digest_conflict" => WorkbenchOutcome::DigestConflict,
                    _ => WorkbenchOutcome::Failed,
                };
                failure_message(&request, started, outcome, error)
            }
        }
    }

    fn execute_validated(
        &self,
        request: &WorkbenchRequest,
        cancelled: &AtomicBool,
    ) -> Result<ExecutionSuccess, ExecutionError> {
        fs::create_dir_all(&self.workspace_root).map_err(workspace_io_error)?;
        fs::create_dir_all(&self.artifact_root).map_err(workspace_io_error)?;
        reject_symlink(&self.workspace_root)?;
        reject_symlink(&self.artifact_root)?;

        match &request.tool {
            WorkbenchTool::InspectFile { path, max_bytes } => {
                let path = self.resolve_existing(path)?;
                let metadata = fs::metadata(&path).map_err(workspace_io_error)?;
                if !metadata.is_file() {
                    return Err(ExecutionError::workspace(
                        "not_a_file",
                        "the requested workspace path is not a regular file",
                    ));
                }
                let limit = (*max_bytes)
                    .min(request.resource_limits.file_bytes)
                    .try_into()
                    .unwrap_or(usize::MAX);
                let bytes = read_bounded_file(&path, limit)?;
                let mut output = BTreeMap::new();
                output.insert("sha256".to_string(), hex_sha256(&bytes));
                output.insert("size_bytes".to_string(), metadata.len().to_string());
                output.insert(
                    "content".to_string(),
                    String::from_utf8(bytes.clone()).map_err(|_| {
                        ExecutionError::tool(
                            "non_utf8_input",
                            "M0 file inspection accepts UTF-8 text only",
                        )
                    })?,
                );
                Ok(ExecutionSuccess {
                    output,
                    bytes_read: bytes.len() as u64,
                    ..ExecutionSuccess::default()
                })
            }
            WorkbenchTool::WriteFile {
                path,
                content,
                expected_sha256,
            } => {
                let bytes = content.as_bytes();
                ensure_file_budget(bytes.len() as u64, request.resource_limits.file_bytes)?;
                let destination = self.resolve_for_write(path)?;
                verify_expected_digest(&destination, expected_sha256.as_deref())?;
                atomic_write(&destination, bytes, &request.invocation_id, request.attempt)?;
                Ok(file_write_success(bytes))
            }
            WorkbenchTool::ApplyPatch {
                path,
                expected_sha256,
                replacements,
            } => {
                let destination = self.resolve_existing(path)?;
                verify_expected_digest(&destination, Some(expected_sha256))?;
                let original = fs::read(&destination).map_err(workspace_io_error)?;
                let mut updated = String::from_utf8(original.clone()).map_err(|_| {
                    ExecutionError::tool(
                        "non_utf8_input",
                        "M0 patch application accepts UTF-8 text only",
                    )
                })?;
                for replacement in replacements {
                    if replacement.old.is_empty() || replacement.expected_occurrences == 0 {
                        return Err(ExecutionError::tool(
                            "invalid_patch",
                            "patch replacements must bind non-empty source text and occurrence count",
                        ));
                    }
                    let count = updated.matches(&replacement.old).count() as u32;
                    if count != replacement.expected_occurrences {
                        return Err(ExecutionError::tool(
                            "patch_context_conflict",
                            "patch source text did not match the expected occurrence count",
                        ));
                    }
                    updated = updated.replace(&replacement.old, &replacement.new);
                }
                ensure_file_budget(updated.len() as u64, request.resource_limits.file_bytes)?;
                atomic_write(
                    &destination,
                    updated.as_bytes(),
                    &request.invocation_id,
                    request.attempt,
                )?;
                let mut success = file_write_success(updated.as_bytes());
                success.bytes_read = original.len() as u64;
                Ok(success)
            }
            WorkbenchTool::RunCommand { program, args } => {
                self.run_command(request, program, args, None, cancelled)
            }
            WorkbenchTool::RunTests {
                suite_id,
                program,
                args,
            } => self.run_command(request, program, args, Some(suite_id), cancelled),
            WorkbenchTool::PackageArtifact {
                artifact_kind,
                media_type,
                paths,
            } => self.package_artifact(request, artifact_kind, media_type, paths),
        }
    }

    fn run_command(
        &self,
        request: &WorkbenchRequest,
        program: &str,
        args: &[String],
        suite_id: Option<&str>,
        cancelled: &AtomicBool,
    ) -> Result<ExecutionSuccess, ExecutionError> {
        let workspace = fs::canonicalize(&self.workspace_root).map_err(workspace_io_error)?;
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(workspace)
            .env_clear()
            .envs(SAFE_ENVIRONMENT)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = command.spawn().map_err(|_| {
            ExecutionError::runtime(
                "command_spawn_failed",
                "the allowlisted command could not be started",
                false,
            )
        })?;
        let pid = child.id();
        let stdout = child.stdout.take().ok_or_else(|| {
            ExecutionError::runtime(
                "stdout_pipe_missing",
                "command output capture could not be initialized",
                false,
            )
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            ExecutionError::runtime(
                "stderr_pipe_missing",
                "command error capture could not be initialized",
                false,
            )
        })?;
        let stdout_reader = drain_bounded(stdout, request.resource_limits.stdout_bytes);
        let stderr_reader = drain_bounded(stderr, request.resource_limits.stderr_bytes);
        let absolute_deadline = UNIX_EPOCH + Duration::from_millis(request.deadline_unix_ms);
        let wall_deadline = SystemTime::now()
            .checked_add(Duration::from_millis(request.resource_limits.wall_time_ms))
            .unwrap_or(absolute_deadline);
        let deadline = absolute_deadline.min(wall_deadline);

        let (status, forced_error) = loop {
            if cancelled.load(Ordering::Acquire) {
                terminate_process_group(pid, &mut child);
                break (
                    child.wait().ok(),
                    Some(ExecutionError::runtime(
                        "cancelled",
                        "invocation was cancelled",
                        false,
                    )),
                );
            }
            if SystemTime::now() >= deadline {
                terminate_process_group(pid, &mut child);
                break (
                    child.wait().ok(),
                    Some(ExecutionError::new(
                        WorkbenchErrorClass::Resource,
                        "deadline_expired",
                        "invocation deadline expired",
                        false,
                    )),
                );
            }
            match child.try_wait() {
                Ok(Some(status)) => break (Some(status), None),
                Ok(None) => thread::sleep(COMMAND_POLL_INTERVAL),
                Err(_) => {
                    terminate_process_group(pid, &mut child);
                    break (
                        child.wait().ok(),
                        Some(ExecutionError::runtime(
                            "command_wait_failed",
                            "command state could not be observed",
                            false,
                        )),
                    );
                }
            }
        };

        let stdout = stdout_reader.join().unwrap_or_default();
        let stderr = stderr_reader.join().unwrap_or_default();
        if let Some(error) = forced_error {
            return Err(error);
        }
        let status = status.ok_or_else(|| {
            ExecutionError::runtime(
                "command_status_missing",
                "command completion status was unavailable",
                false,
            )
        })?;
        let mut output = BTreeMap::new();
        output.insert(
            "exit_code".to_string(),
            status.code().unwrap_or(-1).to_string(),
        );
        output.insert("stdout".to_string(), redact_output(&stdout.retained));
        output.insert("stderr".to_string(), redact_output(&stderr.retained));
        output.insert("stdout_bytes".to_string(), stdout.total.to_string());
        output.insert("stderr_bytes".to_string(), stderr.total.to_string());
        if let Some(suite_id) = suite_id {
            output.insert("suite_id".to_string(), suite_id.to_string());
        }
        if !status.success() {
            return Err(ExecutionError::tool(
                "command_failed",
                "the allowlisted command returned a non-zero status",
            ));
        }
        Ok(ExecutionSuccess {
            output,
            bytes_read: stdout.total + stderr.total,
            ..ExecutionSuccess::default()
        })
    }

    fn package_artifact(
        &self,
        request: &WorkbenchRequest,
        artifact_kind: &str,
        media_type: &str,
        paths: &[String],
    ) -> Result<ExecutionSuccess, ExecutionError> {
        let mut entries = Vec::new();
        for path in paths {
            let absolute = self.resolve_existing(path)?;
            collect_artifact_entries(&self.workspace_root, &absolute, &mut entries)?;
        }
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        entries.dedup_by(|left, right| left.path == right.path);
        if entries.is_empty() {
            return Err(ExecutionError::tool(
                "empty_artifact",
                "artifact packaging requires at least one regular file",
            ));
        }
        let total_size = entries.iter().map(|entry| entry.size_bytes).sum::<u64>();
        ensure_file_budget(total_size, request.resource_limits.file_bytes)?;
        let manifest = ArtifactManifest {
            schema_version: 1,
            invocation_id: &request.invocation_id,
            input_digest: &request.input_digest,
            project_id: &request.project_id,
            work_item_id: &request.work_item_id,
            workspace_id: &request.workspace_id,
            agent_id: request.agent_id.0,
            artifact_kind,
            media_type,
            runtime_key: &request.runtime_key,
            tool_profile: &request.tool_profile,
            tool_profile_digest: &request.tool_profile_digest,
            policy_digest: &request.policy_digest,
            entries: &entries,
        };
        let bytes = serde_json::to_vec(&manifest).map_err(|_| {
            ExecutionError::tool(
                "manifest_serialization_failed",
                "artifact manifest could not be serialized",
            )
        })?;
        let digest = hex_sha256(&bytes);
        let file_name = format!("{}.manifest.json", request.invocation_id);
        let destination = self.artifact_root.join(&file_name);
        immutable_write(
            &destination,
            &bytes,
            &request.invocation_id,
            request.attempt,
        )?;
        Ok(ExecutionSuccess {
            artifacts: vec![WorkbenchArtifactRef {
                artifact_id: format!("sha256:{digest}"),
                sha256: digest,
                artifact_kind: artifact_kind.to_string(),
                media_type: media_type.to_string(),
                size_bytes: total_size,
                manifest_path: file_name,
            }],
            bytes_read: total_size,
            bytes_written: bytes.len() as u64,
            artifact_bytes: total_size,
            ..ExecutionSuccess::default()
        })
    }

    fn resolve_existing(&self, relative: &str) -> Result<PathBuf, ExecutionError> {
        let root = fs::canonicalize(&self.workspace_root).map_err(workspace_io_error)?;
        let relative = checked_relative(relative)?;
        let mut current = root.clone();
        for component in relative.components() {
            current.push(component.as_os_str());
            reject_symlink(&current)?;
        }
        let canonical = fs::canonicalize(&current).map_err(workspace_io_error)?;
        if !canonical.starts_with(&root) {
            return Err(ExecutionError::workspace(
                "workspace_escape",
                "workspace path escaped its assigned root",
            ));
        }
        Ok(canonical)
    }

    fn resolve_for_write(&self, relative: &str) -> Result<PathBuf, ExecutionError> {
        let root = fs::canonicalize(&self.workspace_root).map_err(workspace_io_error)?;
        let relative = checked_relative(relative)?;
        let parent = relative.parent().unwrap_or_else(|| Path::new(""));
        let mut current = root.clone();
        for component in parent.components() {
            current.push(component.as_os_str());
            if current.exists() {
                reject_symlink(&current)?;
            } else {
                fs::create_dir(&current).map_err(workspace_io_error)?;
            }
        }
        let destination = root.join(relative);
        if destination.exists() {
            reject_symlink(&destination)?;
        }
        if !destination.starts_with(root) {
            return Err(ExecutionError::workspace(
                "workspace_escape",
                "workspace path escaped its assigned root",
            ));
        }
        Ok(destination)
    }
}

#[derive(Default)]
struct ExecutionSuccess {
    output: BTreeMap<String, String>,
    artifacts: Vec<WorkbenchArtifactRef>,
    bytes_read: u64,
    bytes_written: u64,
    artifact_bytes: u64,
}

#[derive(Debug)]
struct ExecutionError {
    class: WorkbenchErrorClass,
    code: &'static str,
    safe_message: String,
    retryable: bool,
}

impl ExecutionError {
    fn new(
        class: WorkbenchErrorClass,
        code: &'static str,
        safe_message: &str,
        retryable: bool,
    ) -> Self {
        Self {
            class,
            code,
            safe_message: safe_message.to_string(),
            retryable,
        }
    }

    fn workspace(code: &'static str, safe_message: &str) -> Self {
        Self::new(WorkbenchErrorClass::Workspace, code, safe_message, false)
    }

    fn runtime(code: &'static str, safe_message: &str, retryable: bool) -> Self {
        Self::new(WorkbenchErrorClass::Runtime, code, safe_message, retryable)
    }

    fn tool(code: &'static str, safe_message: &str) -> Self {
        Self::new(WorkbenchErrorClass::Tool, code, safe_message, false)
    }

    fn info(&self) -> WorkbenchErrorInfo {
        WorkbenchErrorInfo {
            class: self.class,
            code: self.code.to_string(),
            safe_message: self.safe_message.clone(),
            retryable: self.retryable,
        }
    }
}

#[derive(Serialize)]
struct ArtifactManifest<'a> {
    schema_version: u16,
    invocation_id: &'a str,
    input_digest: &'a str,
    project_id: &'a str,
    work_item_id: &'a str,
    workspace_id: &'a str,
    agent_id: u16,
    artifact_kind: &'a str,
    media_type: &'a str,
    runtime_key: &'a str,
    tool_profile: &'a str,
    tool_profile_digest: &'a str,
    policy_digest: &'a str,
    entries: &'a [ArtifactManifestEntry],
}

#[derive(Debug, Serialize)]
struct ArtifactManifestEntry {
    path: String,
    sha256: String,
    size_bytes: u64,
}

fn collect_artifact_entries(
    root: &Path,
    path: &Path,
    entries: &mut Vec<ArtifactManifestEntry>,
) -> Result<(), ExecutionError> {
    reject_symlink(path)?;
    let metadata = fs::metadata(path).map_err(workspace_io_error)?;
    if metadata.is_file() {
        let bytes = fs::read(path).map_err(workspace_io_error)?;
        let relative = path.strip_prefix(root).map_err(|_| {
            ExecutionError::workspace(
                "workspace_escape",
                "artifact path escaped its assigned workspace",
            )
        })?;
        entries.push(ArtifactManifestEntry {
            path: relative.to_string_lossy().replace('\\', "/"),
            sha256: hex_sha256(&bytes),
            size_bytes: bytes.len() as u64,
        });
    } else if metadata.is_dir() {
        let mut children = fs::read_dir(path)
            .map_err(workspace_io_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(workspace_io_error)?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            collect_artifact_entries(root, &child.path(), entries)?;
        }
    } else {
        return Err(ExecutionError::workspace(
            "unsupported_file_type",
            "artifact paths may contain regular files and directories only",
        ));
    }
    Ok(())
}

fn checked_relative(path: &str) -> Result<&Path, ExecutionError> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(ExecutionError::workspace(
            "invalid_path",
            "tool path must be workspace-relative",
        ));
    }
    Ok(path)
}

fn reject_symlink(path: &Path) -> Result<(), ExecutionError> {
    let metadata = fs::symlink_metadata(path).map_err(workspace_io_error)?;
    if metadata.file_type().is_symlink() {
        return Err(ExecutionError::workspace(
            "symlink_denied",
            "symbolic links are not accepted at workbench effect boundaries",
        ));
    }
    Ok(())
}

fn verify_expected_digest(path: &Path, expected: Option<&str>) -> Result<(), ExecutionError> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let bytes = fs::read(path).map_err(workspace_io_error)?;
    if hex_sha256(&bytes) != expected {
        return Err(ExecutionError::new(
            WorkbenchErrorClass::Recovery,
            "digest_conflict",
            "the current file digest differs from the bound precondition",
            false,
        ));
    }
    Ok(())
}

fn atomic_write(
    destination: &Path,
    bytes: &[u8],
    invocation_id: &str,
    attempt: u32,
) -> Result<(), ExecutionError> {
    if destination.exists() {
        let current = fs::read(destination).map_err(workspace_io_error)?;
        if current == bytes {
            return Ok(());
        }
    }
    let parent = destination.parent().ok_or_else(|| {
        ExecutionError::workspace("invalid_path", "destination has no workspace parent")
    })?;
    let temporary = parent.join(format!(".sentinel-{invocation_id}-{attempt}.tmp"));
    if temporary.exists() {
        reject_symlink(&temporary)?;
        fs::remove_file(&temporary).map_err(workspace_io_error)?;
    }
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(workspace_io_error)?;
        file.write_all(bytes).map_err(workspace_io_error)?;
        file.sync_all().map_err(workspace_io_error)?;
        fs::rename(&temporary, destination).map_err(workspace_io_error)?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(workspace_io_error)
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

fn immutable_write(
    destination: &Path,
    bytes: &[u8],
    invocation_id: &str,
    attempt: u32,
) -> Result<(), ExecutionError> {
    if destination.exists() {
        let current = fs::read(destination).map_err(workspace_io_error)?;
        if current == bytes {
            return Ok(());
        }
        return Err(ExecutionError::new(
            WorkbenchErrorClass::Recovery,
            "digest_conflict",
            "an immutable artifact already exists for this invocation",
            false,
        ));
    }
    atomic_write(destination, bytes, invocation_id, attempt)
}

fn read_bounded_file(path: &Path, limit: usize) -> Result<Vec<u8>, ExecutionError> {
    let metadata = fs::metadata(path).map_err(workspace_io_error)?;
    if metadata.len() > limit as u64 {
        return Err(ExecutionError::new(
            WorkbenchErrorClass::Resource,
            "file_limit_exceeded",
            "the requested file exceeds the invocation read limit",
            false,
        ));
    }
    fs::read(path).map_err(workspace_io_error)
}

fn ensure_file_budget(actual: u64, limit: u64) -> Result<(), ExecutionError> {
    if actual > limit {
        return Err(ExecutionError::new(
            WorkbenchErrorClass::Resource,
            "file_limit_exceeded",
            "tool output exceeds the invocation file limit",
            false,
        ));
    }
    Ok(())
}

fn file_write_success(bytes: &[u8]) -> ExecutionSuccess {
    ExecutionSuccess {
        output: BTreeMap::from([
            ("sha256".to_string(), hex_sha256(bytes)),
            ("size_bytes".to_string(), bytes.len().to_string()),
        ]),
        bytes_written: bytes.len() as u64,
        ..ExecutionSuccess::default()
    }
}

fn terminate_process_group(pid: u32, child: &mut std::process::Child) {
    if let Ok(pid) = i32::try_from(pid) {
        // SAFETY: a negative PID targets only the process group created for this child.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

#[derive(Default)]
struct BoundedOutput {
    retained: Vec<u8>,
    total: u64,
}

fn drain_bounded<R>(mut reader: R, limit: u64) -> thread::JoinHandle<BoundedOutput>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut output = BoundedOutput::default();
        let mut chunk = [0_u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    output.total = output.total.saturating_add(count as u64);
                    let remaining = limit.saturating_sub(output.retained.len() as u64) as usize;
                    output
                        .retained
                        .extend_from_slice(&chunk[..count.min(remaining)]);
                }
            }
        }
        output
    })
}

fn redact_output(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.lines()
        .map(|line| {
            let lower = line.to_ascii_lowercase();
            if [
                "api_key",
                "authorization:",
                "bearer ",
                "password=",
                "secret=",
                "token=",
            ]
            .iter()
            .any(|marker| lower.contains(marker))
            {
                "[REDACTED]"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn workspace_io_error(_: std::io::Error) -> ExecutionError {
    ExecutionError::workspace(
        "workspace_io_failed",
        "the workspace operation could not be completed",
    )
}

fn failure_message(
    request: &WorkbenchRequest,
    started: Instant,
    outcome: WorkbenchOutcome,
    error: ExecutionError,
) -> WorkbenchMessage {
    WorkbenchMessage::Result {
        schema_version: WORKBENCH_SCHEMA_VERSION,
        invocation_id: request.invocation_id.clone(),
        input_digest: request.input_digest.clone(),
        outcome,
        resources: WorkbenchResourceUsage {
            duration_ms: elapsed_ms(started),
            ..WorkbenchResourceUsage::default()
        },
        artifacts: Vec::new(),
        output: BTreeMap::new(),
        error: Some(error.info()),
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::os::unix::fs::symlink;

    use sentinel_common::{CommandRule, WorkbenchResourceLimits};

    use super::*;

    fn request(tool: WorkbenchTool, capability: &str) -> WorkbenchRequest {
        WorkbenchRequest {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: "018f3f32-4f01-7f2c-a6c1-f6f4a81b2801".to_string(),
            agent_id: sentinel_common::AgentId(7),
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
            runtime_key: sentinel_common::WORKBENCH_RUNTIME_BWRAP.to_string(),
            capabilities: BTreeSet::from([capability.to_string()]),
            output_artifact_kinds: BTreeSet::from(["source_tree".to_string()]),
            inputs: Vec::new(),
            command_policy: Vec::new(),
            resource_limits: WorkbenchResourceLimits {
                wall_time_ms: 30_000,
                cpu_time_ms: 10_000,
                memory_bytes: 134_217_728,
                process_count: 16,
                file_bytes: 8_388_608,
                stdout_bytes: 65_536,
                stderr_bytes: 65_536,
            },
            deadline_unix_ms: unix_time_ms() + 60_000,
            attempt: 1,
            tool,
            input_digest: String::new(),
        }
        .bind_digest()
        .unwrap()
    }

    fn outcome(message: &WorkbenchMessage) -> WorkbenchOutcome {
        match message {
            WorkbenchMessage::Result { outcome, .. } => *outcome,
            other => panic!("expected result, got {other:?}"),
        }
    }

    #[test]
    fn writes_patches_inspects_and_packages_digest_bound_files() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let artifacts = directory.path().join("artifacts");
        let executor = WorkbenchExecutor::new(&workspace, &artifacts);
        let cancelled = Arc::new(AtomicBool::new(false));

        let write = request(
            WorkbenchTool::WriteFile {
                path: "src/index.html".to_string(),
                content: "<h1>draft</h1>".to_string(),
                expected_sha256: None,
            },
            "file.write",
        );
        assert_eq!(
            outcome(&executor.execute(write, cancelled.clone())),
            WorkbenchOutcome::Succeeded
        );

        let original_digest = hex_sha256(b"<h1>draft</h1>");
        let patch = request(
            WorkbenchTool::ApplyPatch {
                path: "src/index.html".to_string(),
                expected_sha256: original_digest,
                replacements: vec![sentinel_common::TextReplacement {
                    old: "draft".to_string(),
                    new: "ready".to_string(),
                    expected_occurrences: 1,
                }],
            },
            "patch.apply",
        );
        assert_eq!(
            outcome(&executor.execute(patch, cancelled.clone())),
            WorkbenchOutcome::Succeeded
        );

        let inspect = request(
            WorkbenchTool::InspectFile {
                path: "src/index.html".to_string(),
                max_bytes: 1024,
            },
            "file.inspect",
        );
        let inspected = executor.execute(inspect, cancelled.clone());
        match inspected {
            WorkbenchMessage::Result {
                outcome, output, ..
            } => {
                assert_eq!(outcome, WorkbenchOutcome::Succeeded);
                assert_eq!(output.get("content").unwrap(), "<h1>ready</h1>");
            }
            other => panic!("expected result, got {other:?}"),
        }

        let package = request(
            WorkbenchTool::PackageArtifact {
                artifact_kind: "source_tree".to_string(),
                media_type: "application/vnd.sentinel.source-tree+json".to_string(),
                paths: vec!["src".to_string()],
            },
            "artifact.commit",
        );
        let packaged = executor.execute(package, cancelled);
        match packaged {
            WorkbenchMessage::Result {
                outcome,
                artifacts: refs,
                ..
            } => {
                assert_eq!(outcome, WorkbenchOutcome::Succeeded);
                assert_eq!(refs.len(), 1);
                let manifest = fs::read(artifacts.join(&refs[0].manifest_path)).unwrap();
                assert_eq!(hex_sha256(&manifest), refs[0].sha256);
                assert!(!String::from_utf8(manifest).unwrap().contains("<h1>ready"));
            }
            other => panic!("expected result, got {other:?}"),
        }
    }

    #[test]
    fn symlink_escape_and_stale_digest_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let artifacts = directory.path().join("artifacts");
        fs::create_dir_all(&workspace).unwrap();
        let foreign = directory.path().join("foreign");
        fs::create_dir_all(&foreign).unwrap();
        fs::write(foreign.join("secret"), "no").unwrap();
        symlink(&foreign, workspace.join("escape")).unwrap();
        let executor = WorkbenchExecutor::new(&workspace, &artifacts);

        let inspect = request(
            WorkbenchTool::InspectFile {
                path: "escape/secret".to_string(),
                max_bytes: 1024,
            },
            "file.inspect",
        );
        assert_eq!(
            outcome(&executor.execute(inspect, Arc::new(AtomicBool::new(false)))),
            WorkbenchOutcome::Failed
        );

        fs::write(workspace.join("file.txt"), "current").unwrap();
        let write = request(
            WorkbenchTool::WriteFile {
                path: "file.txt".to_string(),
                content: "next".to_string(),
                expected_sha256: Some("0".repeat(64)),
            },
            "file.write",
        );
        assert_eq!(
            outcome(&executor.execute(write, Arc::new(AtomicBool::new(false)))),
            WorkbenchOutcome::DigestConflict
        );
        assert_eq!(
            fs::read_to_string(workspace.join("file.txt")).unwrap(),
            "current"
        );
    }

    #[test]
    fn command_policy_and_cancellation_are_enforced() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let executor = WorkbenchExecutor::new(&workspace, directory.path().join("artifacts"));

        let mut command = request(
            WorkbenchTool::RunCommand {
                program: "printf".to_string(),
                args: vec!["ok".to_string()],
            },
            "command.run_allowlisted",
        );
        command.command_policy = vec![CommandRule {
            program: "printf".to_string(),
            required_arg_prefix: Vec::new(),
            max_args: 1,
        }];
        command.input_digest = command.canonical_digest().unwrap();
        assert_eq!(
            outcome(&executor.execute(command, Arc::new(AtomicBool::new(false)))),
            WorkbenchOutcome::Succeeded
        );

        let cancelled = Arc::new(AtomicBool::new(true));
        let write = request(
            WorkbenchTool::WriteFile {
                path: "never.txt".to_string(),
                content: "no".to_string(),
                expected_sha256: None,
            },
            "file.write",
        );
        assert_eq!(
            outcome(&executor.execute(write, cancelled)),
            WorkbenchOutcome::Cancelled
        );
        assert!(!workspace.join("never.txt").exists());
    }

    #[test]
    fn command_output_redacts_common_secret_shapes() {
        assert_eq!(
            redact_output(b"visible\nAuthorization: Bearer abc\ntoken=abc"),
            "visible\n[REDACTED]\n[REDACTED]"
        );
    }
}
