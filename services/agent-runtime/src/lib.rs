//! Capability-scoped tool executor used inside the bwrap agent sandbox.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nix::sys::signal::{killpg, Signal};
use nix::unistd::{sysconf, Pid, SysconfVar};
use sentinel_common::{
    WorkbenchArtifactRef, WorkbenchErrorClass, WorkbenchErrorInfo, WorkbenchMessage,
    WorkbenchOutcome, WorkbenchRequest, WorkbenchResourceUsage, WorkbenchTool,
    WORKBENCH_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_COMPLETION_RECEIPT_BYTES: u64 = 1024 * 1024;
const MAX_ARTIFACT_MANIFEST_BYTES: u64 = 1024 * 1024;
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
    input_root: PathBuf,
}

impl WorkbenchExecutor {
    pub fn new(workspace_root: impl Into<PathBuf>, artifact_root: impl Into<PathBuf>) -> Self {
        let workspace_root = workspace_root.into();
        Self {
            input_root: workspace_root.join(".inputs"),
            workspace_root,
            artifact_root: artifact_root.into(),
        }
    }

    pub fn with_input_root(
        workspace_root: impl Into<PathBuf>,
        artifact_root: impl Into<PathBuf>,
        input_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            artifact_root: artifact_root.into(),
            input_root: input_root.into(),
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

        let result = self
            .scoped_for(&request)
            .and_then(|executor| executor.execute_validated(&request, &cancelled, started));
        match result {
            Ok(success) => WorkbenchMessage::Result {
                schema_version: WORKBENCH_SCHEMA_VERSION,
                invocation_id: request.invocation_id,
                input_digest: request.input_digest,
                outcome: WorkbenchOutcome::Succeeded,
                resources: WorkbenchResourceUsage {
                    duration_ms: elapsed_ms(started),
                    cpu_time_ms: success.cpu_time_ms,
                    peak_memory_bytes: success.peak_memory_bytes,
                    peak_process_count: success.peak_process_count,
                    bytes_read: success.bytes_read,
                    bytes_written: success.bytes_written,
                    artifact_bytes: success.artifact_bytes,
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

    /// Persist the redacted terminal result before it is emitted to the daemon.
    ///
    /// The immutable receipt closes the daemon-crash window between a completed
    /// tool effect and its durable orchestration transition. Transient tool
    /// output is intentionally removed; recovery returns only the auditable
    /// outcome, resources, artifacts, and safe error classification.
    pub fn persist_completion_receipt(
        &self,
        message: &WorkbenchMessage,
    ) -> Result<(), WorkbenchErrorInfo> {
        let safe = safe_terminal_receipt(message)?;
        let WorkbenchMessage::Result {
            invocation_id,
            input_digest,
            ..
        } = &safe
        else {
            unreachable!("safe_terminal_receipt accepts only results");
        };
        let directory = self.artifact_root.join(".workbench-receipts");
        fs::create_dir_all(&directory).map_err(receipt_error)?;
        reject_symlink(&directory).map_err(|_| {
            recovery_error(
                "completion_receipt_path_rejected",
                "the completion receipt path failed containment validation",
            )
        })?;
        let destination = directory.join(format!("{invocation_id}.json"));
        if destination.exists() {
            let existing = self.recover_completion(invocation_id, input_digest)?;
            if existing == safe {
                return Ok(());
            }
            return Err(recovery_error(
                "completion_receipt_conflict",
                "the immutable completion receipt conflicts with this result",
            ));
        }

        let bytes = serde_json::to_vec(&safe).map_err(|_| {
            recovery_error(
                "completion_receipt_encode_failed",
                "the completion receipt could not be encoded",
            )
        })?;
        if bytes.len() as u64 > MAX_COMPLETION_RECEIPT_BYTES {
            return Err(recovery_error(
                "completion_receipt_too_large",
                "the completion receipt exceeded its size boundary",
            ));
        }
        let temporary = directory.join(format!(".{invocation_id}.{}.tmp", std::process::id()));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(receipt_error)?;
        file.write_all(&bytes).map_err(receipt_error)?;
        file.sync_all().map_err(receipt_error)?;
        match fs::hard_link(&temporary, &destination) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing = self.recover_completion(invocation_id, input_digest)?;
                if existing != safe {
                    let _ = fs::remove_file(&temporary);
                    return Err(recovery_error(
                        "completion_receipt_conflict",
                        "the immutable completion receipt conflicts with this result",
                    ));
                }
            }
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                return Err(receipt_error(error));
            }
        }
        fs::remove_file(&temporary).map_err(receipt_error)?;
        sync_directory(&directory).map_err(receipt_error)?;
        Ok(())
    }

    pub fn recover_completion(
        &self,
        invocation_id: &str,
        input_digest: &str,
    ) -> Result<WorkbenchMessage, WorkbenchErrorInfo> {
        validate_receipt_key(invocation_id, input_digest)?;
        let path = self
            .artifact_root
            .join(".workbench-receipts")
            .join(format!("{invocation_id}.json"));
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(recovery_error(
                    "completion_receipt_path_rejected",
                    "the completion receipt path failed containment validation",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(recovery_error(
                    "completion_receipt_not_found",
                    "no durable completion receipt exists for this invocation",
                ));
            }
            Err(error) => return Err(receipt_error(error)),
        }
        let metadata = fs::metadata(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                recovery_error(
                    "completion_receipt_not_found",
                    "no durable completion receipt exists for this invocation",
                )
            } else {
                receipt_error(error)
            }
        })?;
        if !metadata.is_file() || metadata.len() > MAX_COMPLETION_RECEIPT_BYTES {
            return Err(recovery_error(
                "completion_receipt_invalid",
                "the completion receipt is outside its integrity boundary",
            ));
        }
        let bytes = fs::read(&path).map_err(receipt_error)?;
        let message: WorkbenchMessage = serde_json::from_slice(&bytes).map_err(|_| {
            recovery_error(
                "completion_receipt_invalid",
                "the completion receipt could not be decoded",
            )
        })?;
        let safe = safe_terminal_receipt(&message)?;
        let WorkbenchMessage::Result {
            invocation_id: stored_invocation,
            input_digest: stored_digest,
            ..
        } = &safe
        else {
            unreachable!("safe_terminal_receipt accepts only results");
        };
        if stored_invocation != invocation_id || stored_digest != input_digest {
            return Err(recovery_error(
                "completion_receipt_binding_mismatch",
                "the completion receipt does not match the requested invocation",
            ));
        }
        Ok(safe)
    }

    fn execute_validated(
        &self,
        request: &WorkbenchRequest,
        cancelled: &AtomicBool,
        started: Instant,
    ) -> Result<ExecutionSuccess, ExecutionError> {
        ensure_invocation_active(request, cancelled, started)?;
        fs::create_dir_all(&self.workspace_root).map_err(workspace_io_error)?;
        fs::create_dir_all(&self.artifact_root).map_err(workspace_io_error)?;
        reject_symlink(&self.workspace_root)?;
        reject_symlink(&self.artifact_root)?;
        self.reconcile_artifact_store()?;
        self.validate_declared_inputs(request, cancelled, started)?;

        match &request.tool {
            WorkbenchTool::InspectFile { path, max_bytes } => {
                let path = self.resolve_read_path(request, path)?;
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
                ensure_invocation_active(request, cancelled, started)?;
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
                reject_input_mutation(request, path)?;
                let bytes = content.as_bytes();
                ensure_file_budget(bytes.len() as u64, request.resource_limits.file_bytes)?;
                let destination = self.resolve_for_write(path)?;
                verify_expected_digest(&destination, expected_sha256.as_deref())?;
                ensure_invocation_active(request, cancelled, started)?;
                atomic_write(&destination, bytes, &request.invocation_id, request.attempt)?;
                Ok(file_write_success(bytes))
            }
            WorkbenchTool::ApplyPatch {
                path,
                expected_sha256,
                replacements,
            } => {
                reject_input_mutation(request, path)?;
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
                ensure_invocation_active(request, cancelled, started)?;
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
                self.run_command(request, program, args, None, cancelled, started)
            }
            WorkbenchTool::RunTests {
                suite_id,
                program,
                args,
            } => self.run_command(request, program, args, Some(suite_id), cancelled, started),
            WorkbenchTool::PackageArtifact {
                artifact_kind,
                media_type,
                paths,
            } => self.package_artifact(
                request,
                artifact_kind,
                media_type,
                paths,
                cancelled,
                started,
            ),
        }
    }

    fn scoped_for(&self, request: &WorkbenchRequest) -> Result<Self, ExecutionError> {
        let workspace_root = create_contained_directory(
            &self.workspace_root,
            &[request.project_id.as_str(), request.work_item_id.as_str()],
        )?;
        let artifact_root = create_contained_directory(
            &self.artifact_root,
            &[request.project_id.as_str(), request.work_item_id.as_str()],
        )?;
        let input_root = if request.inputs.is_empty() {
            self.input_root
                .join(&request.project_id)
                .join(&request.work_item_id)
        } else {
            open_contained_directory(
                &self.input_root,
                &[request.project_id.as_str(), request.work_item_id.as_str()],
            )?
        };
        Ok(Self::with_input_root(
            workspace_root,
            artifact_root,
            input_root,
        ))
    }

    fn validate_declared_inputs(
        &self,
        request: &WorkbenchRequest,
        cancelled: &AtomicBool,
        started: Instant,
    ) -> Result<(), ExecutionError> {
        for input in &request.inputs {
            ensure_invocation_active(request, cancelled, started)?;
            let path = self.resolve_input(&input.mount_path)?;
            let metadata = fs::metadata(&path).map_err(workspace_io_error)?;
            if !metadata.is_file() || metadata.permissions().mode() & 0o222 != 0 {
                return Err(ExecutionError::workspace(
                    "input_mount_not_read_only",
                    "declared workbench inputs must be read-only regular files",
                ));
            }
            let limit = request
                .resource_limits
                .file_bytes
                .try_into()
                .unwrap_or(usize::MAX);
            let bytes = read_bounded_file(&path, limit)?;
            if hex_sha256(&bytes) != input.sha256
                || input.artifact_id != format!("sha256:{}", input.sha256)
            {
                return Err(ExecutionError::new(
                    WorkbenchErrorClass::Recovery,
                    "input_digest_conflict",
                    "a declared input does not match its content-addressed binding",
                    false,
                ));
            }
        }
        Ok(())
    }

    fn run_command(
        &self,
        request: &WorkbenchRequest,
        program: &str,
        args: &[String],
        suite_id: Option<&str>,
        cancelled: &AtomicBool,
        started: Instant,
    ) -> Result<ExecutionSuccess, ExecutionError> {
        let workspace = fs::canonicalize(&self.workspace_root).map_err(workspace_io_error)?;
        let input_root = (!request.inputs.is_empty())
            .then(|| fs::canonicalize(&self.input_root).map_err(workspace_io_error))
            .transpose()?;
        let args = args
            .iter()
            .map(|argument| {
                if let Some(input) = request
                    .inputs
                    .iter()
                    .find(|input| input.mount_path == *argument)
                {
                    let input_root = input_root.as_ref().expect("declared input root resolved");
                    return Ok(input_root.join(checked_relative(&input.mount_path)?));
                }
                let path = checked_relative(argument)?;
                if path.components().next().is_some_and(|component| {
                    component.as_os_str() == std::ffi::OsStr::new(".inputs")
                }) {
                    return Err(ExecutionError::workspace(
                        "foreign_input_scope_denied",
                        "command arguments cannot address undeclared input scopes",
                    ));
                }
                Ok(PathBuf::from(argument))
            })
            .collect::<Result<Vec<_>, ExecutionError>>()?;
        let mut command = Command::new(program);
        command
            .args(&args)
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
        let wall_deadline = UNIX_EPOCH
            .checked_add(Duration::from_millis(
                unix_time_ms().saturating_add(
                    request
                        .resource_limits
                        .wall_time_ms
                        .saturating_sub(elapsed_ms(started)),
                ),
            ))
            .unwrap_or(absolute_deadline);
        let deadline = absolute_deadline.min(wall_deadline);
        let mut observed = ProcessGroupUsage::default();

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
            let sample = sample_process_group(pid);
            observed.observe(sample);
            let limit_error = if observed.cpu_time_ms > request.resource_limits.cpu_time_ms {
                Some((
                    "cpu_limit_exceeded",
                    "the invocation exceeded its CPU time limit",
                ))
            } else if observed.peak_memory_bytes > request.resource_limits.memory_bytes {
                Some((
                    "memory_limit_exceeded",
                    "the invocation exceeded its memory limit",
                ))
            } else if observed.peak_process_count > request.resource_limits.process_count {
                Some((
                    "process_limit_exceeded",
                    "the invocation exceeded its process limit",
                ))
            } else {
                None
            };
            if let Some((code, message)) = limit_error {
                terminate_process_group(pid, &mut child);
                break (
                    child.wait().ok(),
                    Some(ExecutionError::new(
                        WorkbenchErrorClass::Resource,
                        code,
                        message,
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
        if stdout.total > request.resource_limits.stdout_bytes
            || stderr.total > request.resource_limits.stderr_bytes
        {
            return Err(ExecutionError::new(
                WorkbenchErrorClass::Resource,
                "command_output_limit_exceeded",
                "the invocation exceeded its command output limit",
                false,
            ));
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
            cpu_time_ms: observed.cpu_time_ms,
            peak_memory_bytes: observed.peak_memory_bytes,
            peak_process_count: observed.peak_process_count,
            ..ExecutionSuccess::default()
        })
    }

    fn package_artifact(
        &self,
        request: &WorkbenchRequest,
        artifact_kind: &str,
        media_type: &str,
        paths: &[String],
        cancelled: &AtomicBool,
        started: Instant,
    ) -> Result<ExecutionSuccess, ExecutionError> {
        let result = (|| {
            let mut entries = Vec::new();
            let mut remaining_bytes = request.resource_limits.file_bytes;
            for path in paths {
                ensure_invocation_active(request, cancelled, started)?;
                let absolute = self.resolve_existing(path)?;
                collect_artifact_entries(
                    &self.workspace_root,
                    &self.artifact_root,
                    &absolute,
                    &request.invocation_id,
                    request.attempt,
                    &mut entries,
                    &mut remaining_bytes,
                    request,
                    cancelled,
                    started,
                )?;
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
            let file_name = format!("{digest}.manifest.json");
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
        })();
        if result.is_err() {
            self.reconcile_artifact_store()?;
        }
        result
    }

    /// Remove incomplete temporary files and unreferenced content-addressed
    /// blobs. The runtime admits only one invocation at a time, so this runs at
    /// startup/before execution without racing an artifact commit.
    fn reconcile_artifact_store(&self) -> Result<(), ExecutionError> {
        fs::create_dir_all(&self.artifact_root).map_err(workspace_io_error)?;
        reject_symlink(&self.artifact_root)?;
        let mut referenced = BTreeMap::new();
        for entry in fs::read_dir(&self.artifact_root).map_err(workspace_io_error)? {
            let entry = entry.map_err(workspace_io_error)?;
            let path = entry.path();
            reject_symlink(&path)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(".sentinel-") && name.ends_with(".tmp") {
                fs::remove_file(path).map_err(workspace_io_error)?;
                continue;
            }
            if !name.ends_with(".manifest.json") {
                continue;
            }
            let bytes = read_bounded_file(&path, MAX_ARTIFACT_MANIFEST_BYTES as usize)?;
            let Some(expected_manifest_digest) = name.strip_suffix(".manifest.json") else {
                unreachable!("manifest suffix was checked above");
            };
            if !is_lower_hex_digest(expected_manifest_digest)
                || hex_sha256(&bytes) != expected_manifest_digest
            {
                return Err(ExecutionError::new(
                    WorkbenchErrorClass::Recovery,
                    "artifact_manifest_digest_conflict",
                    "a committed artifact manifest failed its integrity check",
                    false,
                ));
            }
            let manifest: StoredArtifactManifest =
                serde_json::from_slice(&bytes).map_err(|_| {
                    ExecutionError::new(
                        WorkbenchErrorClass::Recovery,
                        "artifact_manifest_invalid",
                        "an artifact manifest could not be reconciled",
                        false,
                    )
                })?;
            for (digest, size_bytes) in manifest.referenced_blobs()? {
                if let Some(previous) = referenced.insert(digest, size_bytes) {
                    if previous != size_bytes {
                        return Err(ExecutionError::new(
                            WorkbenchErrorClass::Recovery,
                            "artifact_manifest_binding_invalid",
                            "artifact manifests disagree about a blob binding",
                            false,
                        ));
                    }
                }
            }
        }

        let receipt_directory = self.artifact_root.join(".workbench-receipts");
        if receipt_directory.exists() {
            reject_symlink(&receipt_directory)?;
            for entry in fs::read_dir(&receipt_directory).map_err(workspace_io_error)? {
                let entry = entry.map_err(workspace_io_error)?;
                let path = entry.path();
                reject_symlink(&path)?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') && name.ends_with(".tmp") {
                    fs::remove_file(path).map_err(workspace_io_error)?;
                }
            }
            sync_directory(&receipt_directory).map_err(workspace_io_error)?;
        }

        let blob_directory = self.artifact_root.join("blobs");
        fs::create_dir_all(&blob_directory).map_err(workspace_io_error)?;
        reject_symlink(&blob_directory)?;
        for entry in fs::read_dir(&blob_directory).map_err(workspace_io_error)? {
            let entry = entry.map_err(workspace_io_error)?;
            let path = entry.path();
            reject_symlink(&path)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_lower_hex_digest(&name)
                || !entry.metadata().map_err(workspace_io_error)?.is_file()
            {
                return Err(ExecutionError::new(
                    WorkbenchErrorClass::Recovery,
                    "artifact_blob_invalid",
                    "the artifact blob store contains an invalid entry",
                    false,
                ));
            }
            if let Some(expected_size) = referenced.remove(&name) {
                let metadata = entry.metadata().map_err(workspace_io_error)?;
                if metadata.len() != expected_size
                    || hex_sha256_file(&path).map_err(workspace_io_error)? != name
                {
                    return Err(ExecutionError::new(
                        WorkbenchErrorClass::Recovery,
                        "artifact_blob_digest_conflict",
                        "a committed artifact blob failed its integrity check",
                        false,
                    ));
                }
            } else {
                fs::remove_file(path).map_err(workspace_io_error)?;
            }
        }
        if !referenced.is_empty() {
            return Err(ExecutionError::new(
                WorkbenchErrorClass::Recovery,
                "artifact_blob_missing",
                "an artifact manifest references a missing blob",
                false,
            ));
        }
        sync_directory(&blob_directory).map_err(workspace_io_error)?;
        sync_directory(&self.artifact_root).map_err(workspace_io_error)?;
        Ok(())
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

    fn resolve_input(&self, relative: &str) -> Result<PathBuf, ExecutionError> {
        resolve_existing_beneath(&self.input_root, relative, "input")
    }

    fn resolve_read_path(
        &self,
        request: &WorkbenchRequest,
        relative: &str,
    ) -> Result<PathBuf, ExecutionError> {
        if request
            .inputs
            .iter()
            .any(|input| input.mount_path == relative)
        {
            self.resolve_input(relative)
        } else {
            self.resolve_existing(relative)
        }
    }
}

fn create_contained_directory(base: &Path, components: &[&str]) -> Result<PathBuf, ExecutionError> {
    fs::create_dir_all(base).map_err(workspace_io_error)?;
    reject_symlink(base)?;
    let root = fs::canonicalize(base).map_err(workspace_io_error)?;
    let mut current = root.clone();
    for component in components {
        if component.is_empty()
            || !component
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(ExecutionError::workspace(
                "invalid_workspace_binding",
                "workspace scope contains an invalid identifier",
            ));
        }
        current.push(component);
        if current.exists() {
            reject_symlink(&current)?;
            if !fs::metadata(&current).map_err(workspace_io_error)?.is_dir() {
                return Err(ExecutionError::workspace(
                    "workspace_scope_conflict",
                    "workspace scope is not a directory",
                ));
            }
        } else {
            fs::create_dir(&current).map_err(workspace_io_error)?;
        }
    }
    let canonical = fs::canonicalize(&current).map_err(workspace_io_error)?;
    if !canonical.starts_with(&root) {
        return Err(ExecutionError::workspace(
            "workspace_escape",
            "workspace scope escaped its assigned root",
        ));
    }
    Ok(canonical)
}

fn open_contained_directory(base: &Path, components: &[&str]) -> Result<PathBuf, ExecutionError> {
    reject_symlink(base)?;
    let root = fs::canonicalize(base).map_err(workspace_io_error)?;
    let mut current = root.clone();
    for component in components {
        if component.is_empty()
            || !component
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(ExecutionError::workspace(
                "invalid_input_binding",
                "input scope contains an invalid identifier",
            ));
        }
        current.push(component);
        reject_symlink(&current)?;
        if !fs::metadata(&current).map_err(workspace_io_error)?.is_dir() {
            return Err(ExecutionError::workspace(
                "input_scope_conflict",
                "input scope is not a directory",
            ));
        }
    }
    let canonical = fs::canonicalize(&current).map_err(workspace_io_error)?;
    if !canonical.starts_with(&root) {
        return Err(ExecutionError::workspace(
            "input_scope_escape",
            "input scope escaped its assigned root",
        ));
    }
    Ok(canonical)
}

fn resolve_existing_beneath(
    root: &Path,
    relative: &str,
    boundary: &str,
) -> Result<PathBuf, ExecutionError> {
    let root = fs::canonicalize(root).map_err(workspace_io_error)?;
    let relative = checked_relative(relative)?;
    let mut current = root.clone();
    for component in relative.components() {
        current.push(component.as_os_str());
        reject_symlink(&current)?;
    }
    let resolved = fs::canonicalize(&current).map_err(workspace_io_error)?;
    if !resolved.starts_with(&root) {
        return Err(ExecutionError::workspace(
            "boundary_escape",
            &format!("{boundary} path escaped its assigned root"),
        ));
    }
    Ok(resolved)
}

fn reject_input_mutation(request: &WorkbenchRequest, relative: &str) -> Result<(), ExecutionError> {
    let path = checked_relative(relative)?;
    if path.components().next().is_some_and(|component| {
        component.as_os_str() == std::ffi::OsStr::new(".inputs")
    }) || request
        .inputs
        .iter()
        .any(|input| input.mount_path == relative)
    {
        return Err(ExecutionError::workspace(
            "input_mount_read_only",
            "declared workbench inputs cannot be modified",
        ));
    }
    Ok(())
}

fn safe_terminal_receipt(
    message: &WorkbenchMessage,
) -> Result<WorkbenchMessage, WorkbenchErrorInfo> {
    let WorkbenchMessage::Result {
        schema_version,
        invocation_id,
        input_digest,
        outcome,
        resources,
        artifacts,
        error,
        ..
    } = message
    else {
        return Err(recovery_error(
            "completion_receipt_not_terminal",
            "only a terminal workbench result can be persisted",
        ));
    };
    validate_receipt_key(invocation_id, input_digest)?;
    Ok(WorkbenchMessage::Result {
        schema_version: *schema_version,
        invocation_id: invocation_id.clone(),
        input_digest: input_digest.clone(),
        outcome: *outcome,
        resources: resources.clone(),
        artifacts: artifacts.clone(),
        output: BTreeMap::new(),
        error: error.clone(),
    })
}

fn validate_receipt_key(invocation_id: &str, input_digest: &str) -> Result<(), WorkbenchErrorInfo> {
    let invocation_valid = invocation_id.len() == 36
        && invocation_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-');
    let digest_valid = input_digest.len() == 64
        && input_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
    if !invocation_valid || !digest_valid {
        return Err(recovery_error(
            "completion_receipt_key_invalid",
            "the completion receipt key is invalid",
        ));
    }
    Ok(())
}

fn sync_directory(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

fn receipt_error(_error: std::io::Error) -> WorkbenchErrorInfo {
    recovery_error(
        "completion_receipt_io_failed",
        "the completion receipt could not be persisted or read",
    )
}

fn recovery_error(code: &str, safe_message: &str) -> WorkbenchErrorInfo {
    WorkbenchErrorInfo {
        class: WorkbenchErrorClass::Recovery,
        code: code.to_string(),
        safe_message: safe_message.to_string(),
        retryable: false,
    }
}

#[derive(Default)]
struct ExecutionSuccess {
    output: BTreeMap<String, String>,
    artifacts: Vec<WorkbenchArtifactRef>,
    bytes_read: u64,
    bytes_written: u64,
    artifact_bytes: u64,
    cpu_time_ms: u64,
    peak_memory_bytes: u64,
    peak_process_count: u32,
}

#[derive(Debug, Default, Clone, Copy)]
struct ProcessGroupUsage {
    cpu_time_ms: u64,
    peak_memory_bytes: u64,
    peak_process_count: u32,
}

impl ProcessGroupUsage {
    fn observe(&mut self, sample: Self) {
        self.cpu_time_ms = self.cpu_time_ms.max(sample.cpu_time_ms);
        self.peak_memory_bytes = self.peak_memory_bytes.max(sample.peak_memory_bytes);
        self.peak_process_count = self.peak_process_count.max(sample.peak_process_count);
    }
}

fn sample_process_group(process_group: u32) -> ProcessGroupUsage {
    let clock_ticks = sysconf(SysconfVar::CLK_TCK).ok().flatten().unwrap_or(0);
    let page_size = sysconf(SysconfVar::PAGE_SIZE)
        .ok()
        .flatten()
        .unwrap_or(0);
    if clock_ticks <= 0 || page_size <= 0 {
        return ProcessGroupUsage::default();
    }
    let mut usage = ProcessGroupUsage::default();
    let Ok(entries) = fs::read_dir("/proc") else {
        return usage;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some(after_name) = stat.rsplit_once(')').map(|(_, rest)| rest.trim()) else {
            continue;
        };
        let fields = after_name.split_whitespace().collect::<Vec<_>>();
        let Some(group) = fields.get(2).and_then(|value| value.parse::<u32>().ok()) else {
            continue;
        };
        if group != process_group {
            continue;
        }
        let user_ticks = fields
            .get(11)
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        let system_ticks = fields
            .get(12)
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        let resident_pages = fields
            .get(21)
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        usage.cpu_time_ms = usage.cpu_time_ms.saturating_add(
            user_ticks.saturating_add(system_ticks).saturating_mul(1000) / clock_ticks as u64,
        );
        usage.peak_memory_bytes = usage
            .peak_memory_bytes
            .saturating_add(resident_pages.saturating_mul(page_size as u64));
        usage.peak_process_count = usage.peak_process_count.saturating_add(1);
    }
    usage
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
    blob_id: String,
    sha256: String,
    size_bytes: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredArtifactManifest {
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
    entries: Vec<StoredArtifactManifestEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredArtifactManifestEntry {
    path: String,
    blob_id: String,
    sha256: String,
    size_bytes: u64,
}

impl StoredArtifactManifest {
    fn referenced_blobs(self) -> Result<BTreeMap<String, u64>, ExecutionError> {
        if self.schema_version != 1 {
            return Err(ExecutionError::new(
                WorkbenchErrorClass::Recovery,
                "artifact_manifest_version_unsupported",
                "an artifact manifest uses an unsupported version",
                false,
            ));
        }
        if self.invocation_id.is_empty()
            || !is_lower_hex_digest(&self.input_digest)
            || self.project_id.is_empty()
            || self.work_item_id.is_empty()
            || self.workspace_id.is_empty()
            || self.agent_id == 0
            || self.artifact_kind.is_empty()
            || self.media_type.is_empty()
            || self.runtime_key.is_empty()
            || self.tool_profile.is_empty()
            || !is_lower_hex_digest(&self.tool_profile_digest)
            || !is_lower_hex_digest(&self.policy_digest)
            || self.entries.is_empty()
        {
            return Err(ExecutionError::new(
                WorkbenchErrorClass::Recovery,
                "artifact_manifest_binding_invalid",
                "an artifact manifest has an invalid authority binding",
                false,
            ));
        }
        let mut referenced = BTreeMap::new();
        for entry in self.entries {
            if checked_relative(&entry.path).is_err()
                || !is_lower_hex_digest(&entry.sha256)
                || entry.blob_id != format!("sha256:{}", entry.sha256)
            {
                return Err(ExecutionError::new(
                    WorkbenchErrorClass::Recovery,
                    "artifact_manifest_binding_invalid",
                    "an artifact manifest has an invalid blob binding",
                    false,
                ));
            }
            if let Some(previous) = referenced.insert(entry.sha256, entry.size_bytes) {
                if previous != entry.size_bytes {
                    return Err(ExecutionError::new(
                        WorkbenchErrorClass::Recovery,
                        "artifact_manifest_binding_invalid",
                        "an artifact manifest has conflicting blob sizes",
                        false,
                    ));
                }
            }
        }
        Ok(referenced)
    }
}

fn is_lower_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn collect_artifact_entries(
    root: &Path,
    artifact_root: &Path,
    path: &Path,
    invocation_id: &str,
    attempt: u32,
    entries: &mut Vec<ArtifactManifestEntry>,
    remaining_bytes: &mut u64,
    request: &WorkbenchRequest,
    cancelled: &AtomicBool,
    started: Instant,
) -> Result<(), ExecutionError> {
    ensure_invocation_active(request, cancelled, started)?;
    reject_symlink(path)?;
    let metadata = fs::metadata(path).map_err(workspace_io_error)?;
    if metadata.is_file() {
        if metadata.len() > *remaining_bytes {
            return Err(ExecutionError::new(
                WorkbenchErrorClass::Resource,
                "file_limit_exceeded",
                "artifact inputs exceed the invocation file limit",
                false,
            ));
        }
        let bytes = read_bounded_file(path, (*remaining_bytes).try_into().unwrap_or(usize::MAX))?;
        *remaining_bytes = (*remaining_bytes).saturating_sub(bytes.len() as u64);
        ensure_invocation_active(request, cancelled, started)?;
        let relative = path.strip_prefix(root).map_err(|_| {
            ExecutionError::workspace(
                "workspace_escape",
                "artifact path escaped its assigned workspace",
            )
        })?;
        let digest = hex_sha256(&bytes);
        let blob_directory = artifact_root.join("blobs");
        fs::create_dir_all(&blob_directory).map_err(workspace_io_error)?;
        reject_symlink(&blob_directory)?;
        immutable_write(
            &blob_directory.join(&digest),
            &bytes,
            invocation_id,
            attempt,
        )?;
        entries.push(ArtifactManifestEntry {
            path: relative.to_string_lossy().replace('\\', "/"),
            blob_id: format!("sha256:{digest}"),
            sha256: digest,
            size_bytes: bytes.len() as u64,
        });
    } else if metadata.is_dir() {
        let mut children = fs::read_dir(path)
            .map_err(workspace_io_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(workspace_io_error)?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            collect_artifact_entries(
                root,
                artifact_root,
                &child.path(),
                invocation_id,
                attempt,
                entries,
                remaining_bytes,
                request,
                cancelled,
                started,
            )?;
        }
    } else {
        return Err(ExecutionError::workspace(
            "unsupported_file_type",
            "artifact paths may contain regular files and directories only",
        ));
    }
    Ok(())
}

fn ensure_invocation_active(
    request: &WorkbenchRequest,
    cancelled: &AtomicBool,
    started: Instant,
) -> Result<(), ExecutionError> {
    if cancelled.load(Ordering::Acquire) {
        return Err(ExecutionError::runtime(
            "cancelled",
            "invocation was cancelled",
            false,
        ));
    }
    if unix_time_ms() >= request.deadline_unix_ms
        || elapsed_ms(started) >= request.resource_limits.wall_time_ms
    {
        return Err(ExecutionError::new(
            WorkbenchErrorClass::Resource,
            "deadline_expired",
            "invocation deadline expired",
            false,
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
        let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
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

fn hex_sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let read = file.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        hasher.update(&chunk[..read]);
    }
    let mut output = String::with_capacity(64);
    for byte in hasher.finalize() {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    Ok(output)
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

    fn scoped(base: &Path) -> PathBuf {
        base.join("project-01").join("work-04")
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
                let manifest = fs::read(scoped(&artifacts).join(&refs[0].manifest_path)).unwrap();
                assert_eq!(hex_sha256(&manifest), refs[0].sha256);
                let manifest = String::from_utf8(manifest).unwrap();
                assert!(!manifest.contains("<h1>ready"));
                let blob_digest = hex_sha256(b"<h1>ready</h1>");
                assert!(manifest.contains(&format!("sha256:{blob_digest}")));
                assert_eq!(
                    fs::read(scoped(&artifacts).join("blobs").join(blob_digest)).unwrap(),
                    b"<h1>ready</h1>"
                );
            }
            other => panic!("expected result, got {other:?}"),
        }
    }

    #[test]
    fn restart_reconciles_orphaned_artifact_blobs_without_deleting_committed_content() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let artifacts = directory.path().join("artifacts");
        let executor = WorkbenchExecutor::new(&workspace, &artifacts);
        let active = Arc::new(AtomicBool::new(false));
        assert_eq!(
            outcome(&executor.execute(
                request(
                    WorkbenchTool::WriteFile {
                        path: "src/index.html".to_string(),
                        content: "committed".to_string(),
                        expected_sha256: None,
                    },
                    "file.write",
                ),
                active.clone(),
            )),
            WorkbenchOutcome::Succeeded
        );
        assert_eq!(
            outcome(&executor.execute(
                request(
                    WorkbenchTool::PackageArtifact {
                        artifact_kind: "source_tree".to_string(),
                        media_type: "application/vnd.sentinel.source-tree+json".to_string(),
                        paths: vec!["src".to_string()],
                    },
                    "artifact.commit",
                ),
                active.clone(),
            )),
            WorkbenchOutcome::Succeeded
        );

        let scoped_artifacts = scoped(&artifacts);
        let committed_digest = hex_sha256(b"committed");
        let orphan_digest = hex_sha256(b"orphan");
        fs::write(
            scoped_artifacts.join("blobs").join(&orphan_digest),
            "orphan",
        )
        .unwrap();
        fs::write(
            scoped_artifacts.join(".sentinel-crash-leftover.tmp"),
            "partial",
        )
        .unwrap();

        let restarted = WorkbenchExecutor::new(&workspace, &artifacts);
        assert_eq!(
            outcome(&restarted.execute(
                request(
                    WorkbenchTool::InspectFile {
                        path: "src/index.html".to_string(),
                        max_bytes: 1024,
                    },
                    "file.inspect",
                ),
                active,
            )),
            WorkbenchOutcome::Succeeded
        );
        assert!(scoped_artifacts
            .join("blobs")
            .join(committed_digest)
            .exists());
        assert!(!scoped_artifacts.join("blobs").join(orphan_digest).exists());
        assert!(!scoped_artifacts
            .join(".sentinel-crash-leftover.tmp")
            .exists());
    }

    #[test]
    fn restart_rejects_a_manifest_whose_content_no_longer_matches_its_name() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let artifacts = directory.path().join("artifacts");
        fs::create_dir_all(scoped(&workspace).join("src")).unwrap();
        fs::write(scoped(&workspace).join("src/index.html"), "committed").unwrap();
        let executor = WorkbenchExecutor::new(&workspace, &artifacts);
        let active = Arc::new(AtomicBool::new(false));
        let result = executor.execute(
            request(
                WorkbenchTool::PackageArtifact {
                    paths: vec!["src".to_string()],
                    artifact_kind: "source_tree".to_string(),
                    media_type: "application/x-tar".to_string(),
                },
                "artifact.commit",
            ),
            active.clone(),
        );
        let manifest_path = match result {
            WorkbenchMessage::Result {
                artifacts: references,
                ..
            } => scoped(&artifacts).join(&references[0].manifest_path),
            other => panic!("unexpected workbench result: {other:?}"),
        };
        let mut bytes = fs::read(&manifest_path).unwrap();
        bytes.push(b' ');
        fs::write(&manifest_path, bytes).unwrap();

        let restarted = WorkbenchExecutor::new(&workspace, &artifacts);
        let result = restarted.execute(
            request(
                WorkbenchTool::InspectFile {
                    path: "src/index.html".to_string(),
                    max_bytes: 1024,
                },
                "file.inspect",
            ),
            active,
        );
        assert_eq!(outcome(&result), WorkbenchOutcome::Failed);
        let WorkbenchMessage::Result {
            error: Some(error), ..
        } = result
        else {
            panic!("expected a safe recovery error");
        };
        assert_eq!(error.code, "artifact_manifest_digest_conflict");
    }

    #[test]
    fn symlink_escape_and_stale_digest_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let artifacts = directory.path().join("artifacts");
        fs::create_dir_all(scoped(&workspace)).unwrap();
        let foreign = directory.path().join("foreign");
        fs::create_dir_all(&foreign).unwrap();
        fs::write(foreign.join("secret"), "no").unwrap();
        symlink(&foreign, scoped(&workspace).join("escape")).unwrap();
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

        fs::write(scoped(&workspace).join("file.txt"), "current").unwrap();
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
            fs::read_to_string(scoped(&workspace).join("file.txt")).unwrap(),
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

        let mut excessive_output = request(
            WorkbenchTool::RunCommand {
                program: "printf".to_string(),
                args: vec!["1234".to_string()],
            },
            "command.run_allowlisted",
        );
        excessive_output.command_policy = vec![CommandRule {
            program: "printf".to_string(),
            required_arg_prefix: Vec::new(),
            max_args: 1,
        }];
        excessive_output.resource_limits.stdout_bytes = 2;
        excessive_output.input_digest = excessive_output.canonical_digest().unwrap();
        let excessive_output = executor.execute(
            excessive_output,
            Arc::new(AtomicBool::new(false)),
        );
        let WorkbenchMessage::Result { outcome, error, .. } = excessive_output else {
            panic!("command execution must return a result")
        };
        assert_eq!(outcome, WorkbenchOutcome::Failed);
        assert_eq!(
            error.expect("output-limit failure").code,
            "command_output_limit_exceeded"
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

    #[test]
    fn declared_inputs_are_digest_bound_read_only_and_scope_local() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let artifacts = directory.path().join("artifacts");
        let inputs = directory.path().join("inputs");
        let input_scope = scoped(&inputs);
        fs::create_dir_all(&input_scope).unwrap();
        let input_path = input_scope.join("brief.md");
        fs::write(&input_path, "bounded brief").unwrap();
        fs::set_permissions(&input_path, fs::Permissions::from_mode(0o444)).unwrap();
        let digest = hex_sha256(b"bounded brief");
        let input = sentinel_common::WorkbenchInputRef {
            artifact_id: format!("sha256:{digest}"),
            sha256: digest,
            mount_path: "brief.md".to_string(),
            media_type: "text/markdown".to_string(),
        };
        let executor = WorkbenchExecutor::with_input_root(&workspace, &artifacts, &inputs);

        let mut inspect = request(
            WorkbenchTool::InspectFile {
                path: "brief.md".to_string(),
                max_bytes: 1024,
            },
            "file.inspect",
        );
        inspect.inputs = vec![input.clone()];
        inspect.input_digest = inspect.canonical_digest().unwrap();
        assert_eq!(
            outcome(&executor.execute(inspect, Arc::new(AtomicBool::new(false)))),
            WorkbenchOutcome::Succeeded
        );

        let mut overwrite = request(
            WorkbenchTool::WriteFile {
                path: "brief.md".to_string(),
                content: "changed".to_string(),
                expected_sha256: None,
            },
            "file.write",
        );
        overwrite.inputs = vec![input];
        overwrite.input_digest = overwrite.canonical_digest().unwrap();
        assert_eq!(
            outcome(&executor.execute(overwrite, Arc::new(AtomicBool::new(false)))),
            WorkbenchOutcome::Failed
        );
        assert_eq!(fs::read_to_string(input_path).unwrap(), "bounded brief");
    }

    #[test]
    fn declared_input_parent_cannot_be_replaced_or_address_foreign_scope() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let artifacts = directory.path().join("artifacts");
        let inputs = directory.path().join("inputs");
        let assigned = scoped(&inputs);
        let foreign = inputs.join("other-project/WORK-1");
        fs::create_dir_all(&assigned).unwrap();
        fs::create_dir_all(&foreign).unwrap();
        fs::write(assigned.join("brief.md"), "assigned").unwrap();
        fs::write(foreign.join("brief.md"), "foreign").unwrap();
        fs::set_permissions(
            assigned.join("brief.md"),
            fs::Permissions::from_mode(0o444),
        )
        .unwrap();
        let executor = WorkbenchExecutor::with_input_root(&workspace, &artifacts, &inputs);
        let digest = hex_sha256(b"assigned");
        let mut replace_parent = request(
            WorkbenchTool::WriteFile {
                path: ".inputs".to_string(),
                content: "replacement".to_string(),
                expected_sha256: None,
            },
            "file.write",
        );
        replace_parent.inputs = vec![sentinel_common::WorkbenchInputRef {
            artifact_id: format!("sha256:{digest}"),
            sha256: digest,
            mount_path: "brief.md".to_string(),
            media_type: "text/markdown".to_string(),
        }];
        replace_parent.input_digest = replace_parent.canonical_digest().unwrap();
        assert_eq!(
            outcome(&executor.execute(replace_parent, Arc::new(AtomicBool::new(false)))),
            WorkbenchOutcome::Failed
        );
        assert_eq!(fs::read_to_string(assigned.join("brief.md")).unwrap(), "assigned");

        let mut foreign_command = request(
            WorkbenchTool::RunCommand {
                program: "cat".to_string(),
                args: vec![".inputs/other-project/WORK-1/brief.md".to_string()],
            },
            "command.run_allowlisted",
        );
        foreign_command.command_policy = vec![CommandRule {
            program: "cat".to_string(),
            required_arg_prefix: Vec::new(),
            max_args: 1,
        }];
        foreign_command.input_digest = foreign_command.canonical_digest().unwrap();
        assert_eq!(
            outcome(&executor.execute(foreign_command, Arc::new(AtomicBool::new(false)))),
            WorkbenchOutcome::Failed
        );
    }

    #[test]
    fn completion_receipt_is_immutable_digest_bound_and_redacted() {
        let directory = tempfile::tempdir().unwrap();
        let executor = WorkbenchExecutor::new(
            directory.path().join("workspace"),
            directory.path().join("artifacts"),
        );
        let request = request(
            WorkbenchTool::InspectFile {
                path: "index.html".to_string(),
                max_bytes: 1024,
            },
            "file.inspect",
        );
        let result = WorkbenchMessage::Result {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: request.invocation_id.clone(),
            input_digest: request.input_digest.clone(),
            outcome: WorkbenchOutcome::Succeeded,
            resources: WorkbenchResourceUsage::default(),
            artifacts: Vec::new(),
            output: BTreeMap::from([("content".to_string(), "PRIVATE".to_string())]),
            error: None,
        };

        executor.persist_completion_receipt(&result).unwrap();
        executor.persist_completion_receipt(&result).unwrap();
        let recovered = executor
            .recover_completion(&request.invocation_id, &request.input_digest)
            .unwrap();
        let encoded = serde_json::to_string(&recovered).unwrap();
        assert!(!encoded.contains("PRIVATE"));
        assert!(encoded.contains(&request.input_digest));
        assert!(executor
            .recover_completion(&request.invocation_id, &"c".repeat(64))
            .unwrap_err()
            .code
            .contains("mismatch"));

        let mut conflicting = result;
        let WorkbenchMessage::Result { resources, .. } = &mut conflicting else {
            unreachable!();
        };
        resources.duration_ms = 1;
        assert_eq!(
            executor
                .persist_completion_receipt(&conflicting)
                .unwrap_err()
                .code,
            "completion_receipt_conflict"
        );
    }
}
