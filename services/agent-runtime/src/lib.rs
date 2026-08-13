//! Capability-scoped tool executor used inside the bwrap agent sandbox.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nix::errno::Errno;
use nix::sys::signal::{killpg, Signal};
use nix::unistd::{sysconf, Pid, SysconfVar};
use sentinel_common::{
    WorkbenchArtifactRef, WorkbenchErrorClass, WorkbenchErrorInfo, WorkbenchMessage,
    WorkbenchOutcome, WorkbenchRequest, WorkbenchResourceUsage, WorkbenchTool,
    WORKBENCH_MAX_CALLER_RESULT_BYTES, WORKBENCH_MAX_INSPECT_BYTES, WORKBENCH_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(10);
const COMMAND_CLEANUP_GRACE: Duration = Duration::from_secs(1);
const MAX_COMPLETION_RECEIPT_BYTES: u64 = 1024 * 1024;
const MAX_ARTIFACT_MANIFEST_BYTES: u64 = 1024 * 1024;
const COMPLETION_RECEIPT_DIRECTORY: &str = ".workbench-receipts";
const COMPLETION_RECEIPT_DIRECTORY_MODE: u32 = 0o700;
const COMPLETION_RECEIPT_FILE_MODE: u32 = 0o600;
const COMPLETION_RECEIPT_SCHEMA_VERSION: u16 = 1;
const SAFE_ENVIRONMENT: [(&str, &str); 4] = [
    ("HOME", "/workspace"),
    ("LANG", "C.UTF-8"),
    ("LC_ALL", "C.UTF-8"),
    ("PATH", "/usr/bin:/bin"),
];

#[derive(Debug)]
struct PinnedDirectory {
    file: File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReceiptFileIdentity {
    device: u64,
    inode: u64,
    size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BoundedFileIdentity {
    device: u64,
    inode: u64,
    size: u64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SealedCompletionReceipt {
    schema_version: u16,
    invocation_id: String,
    input_digest: String,
    result_digest: String,
    result: WorkbenchMessage,
}

impl PinnedDirectory {
    fn open_chain(path: &Path, create: bool) -> std::io::Result<Self> {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        let mut current = open_directory(Path::new("/"))?;
        for component in absolute.components() {
            let name = match component {
                Component::RootDir | Component::CurDir => continue,
                Component::Normal(name) => name,
                Component::ParentDir | Component::Prefix(_) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "receipt directory contains an unsupported component",
                    ));
                }
            };
            let parent = Self { file: current };
            let child = parent.child_path(name);
            current = match open_directory(&child) {
                Ok(directory) => directory,
                Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => {
                    let mut builder = DirBuilder::new();
                    builder.mode(COMPLETION_RECEIPT_DIRECTORY_MODE);
                    match builder.create(&child) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(error) => return Err(error),
                    }
                    open_directory(&child)?
                }
                Err(error) => return Err(error),
            };
        }
        Ok(Self { file: current })
    }

    fn child_path(&self, name: impl AsRef<OsStr>) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.file.as_raw_fd()))
            .join(Path::new(name.as_ref()))
    }

    fn open_child_directory(&self, name: &str, create: bool) -> std::io::Result<Option<Self>> {
        let path = self.child_path(name);
        let file = match open_directory(&path) {
            Ok(file) => file,
            Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => {
                let mut builder = DirBuilder::new();
                builder.mode(COMPLETION_RECEIPT_DIRECTORY_MODE);
                match builder.create(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
                open_directory(&path)?
            }
            Err(error) => return Err(error),
        };
        Ok(Some(Self { file }))
    }

    fn validate_owned_safe_directory(&self, exact_mode: bool) -> std::io::Result<()> {
        let metadata = self.file.metadata()?;
        if !metadata.is_dir() || metadata.uid() != current_euid()? {
            return Err(invalid_receipt_data());
        }
        let mode = metadata.mode() & 0o7777;
        if exact_mode {
            if mode != COMPLETION_RECEIPT_DIRECTORY_MODE {
                self.file.set_permissions(fs::Permissions::from_mode(
                    COMPLETION_RECEIPT_DIRECTORY_MODE,
                ))?;
                let updated = self.file.metadata()?;
                if updated.uid() != current_euid()?
                    || updated.mode() & 0o7777 != COMPLETION_RECEIPT_DIRECTORY_MODE
                {
                    return Err(invalid_receipt_data());
                }
            }
        } else if mode & (0o7000 | 0o022) != 0 {
            return Err(invalid_receipt_data());
        }
        Ok(())
    }

    fn create_private_file(&self, name: &OsStr) -> std::io::Result<File> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(COMPLETION_RECEIPT_FILE_MODE)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
            .open(self.child_path(name))?;
        file.set_permissions(fs::Permissions::from_mode(COMPLETION_RECEIPT_FILE_MODE))?;
        Ok(file)
    }

    fn open_private_file(&self, name: &OsStr) -> std::io::Result<File> {
        OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
            .open(self.child_path(name))
    }

    fn remove_entry_if_identity(
        &self,
        name: &OsStr,
        expected: ReceiptFileIdentity,
        expected_links: u64,
    ) -> std::io::Result<()> {
        let file = self.open_private_file(name)?;
        let metadata = file.metadata()?;
        validate_receipt_metadata(&metadata, expected_links, true)?;
        if receipt_file_identity(&metadata) != expected {
            return Err(invalid_receipt_data());
        }
        fs::remove_file(self.child_path(name))
    }

    fn sync(&self) -> std::io::Result<()> {
        self.file.sync_all()
    }
}

fn open_directory(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(path)
}

fn current_euid() -> std::io::Result<u32> {
    fs::read_to_string("/proc/self/status")?
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|value| value.split_whitespace().nth(1))
        .and_then(|value| value.parse().ok())
        .ok_or_else(invalid_receipt_data)
}

fn invalid_receipt_data() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "completion receipt failed its integrity boundary",
    )
}

fn receipt_file_identity(metadata: &fs::Metadata) -> ReceiptFileIdentity {
    ReceiptFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        size: metadata.len(),
    }
}

fn validate_receipt_metadata(
    metadata: &fs::Metadata,
    expected_links: u64,
    allow_empty: bool,
) -> std::io::Result<()> {
    if !metadata.is_file()
        || metadata.nlink() != expected_links
        || metadata.uid() != current_euid()?
        || metadata.mode() & 0o7777 != COMPLETION_RECEIPT_FILE_MODE
        || metadata.len() > MAX_COMPLETION_RECEIPT_BYTES
        || (!allow_empty && metadata.len() == 0)
    {
        return Err(invalid_receipt_data());
    }
    Ok(())
}

fn validate_receipt_file(
    metadata: &fs::Metadata,
    allow_empty: bool,
) -> std::io::Result<ReceiptFileIdentity> {
    validate_receipt_metadata(metadata, 1, allow_empty)?;
    Ok(receipt_file_identity(metadata))
}

fn read_validated_receipt(
    directory: &PinnedDirectory,
    name: &OsStr,
    after_open: impl FnOnce(),
) -> std::io::Result<Vec<u8>> {
    read_validated_receipt_with_identity(directory, name, 1, None, after_open)
}

fn read_validated_receipt_for_reconciliation(
    directory: &PinnedDirectory,
    name: &OsStr,
    expected_links: u64,
    expected_identity: ReceiptFileIdentity,
) -> std::io::Result<Vec<u8>> {
    read_validated_receipt_with_identity(
        directory,
        name,
        expected_links,
        Some(expected_identity),
        || {},
    )
}

fn read_validated_receipt_with_identity(
    directory: &PinnedDirectory,
    name: &OsStr,
    expected_links: u64,
    expected_identity: Option<ReceiptFileIdentity>,
    after_open: impl FnOnce(),
) -> std::io::Result<Vec<u8>> {
    let mut file = directory.open_private_file(name)?;
    let before = file.metadata()?;
    validate_receipt_metadata(&before, expected_links, false)?;
    let identity = receipt_file_identity(&before);
    if expected_identity.is_some_and(|expected| expected != identity) {
        return Err(invalid_receipt_data());
    }
    after_open();
    let mut bytes = Vec::with_capacity(identity.size.try_into().unwrap_or(0));
    (&mut file)
        .take(MAX_COMPLETION_RECEIPT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != identity.size {
        return Err(invalid_receipt_data());
    }
    let after = file.metadata()?;
    validate_receipt_metadata(&after, expected_links, false)?;
    if receipt_file_identity(&after) != identity {
        return Err(invalid_receipt_data());
    }
    let installed = directory.open_private_file(name)?;
    let installed_metadata = installed.metadata()?;
    validate_receipt_metadata(&installed_metadata, expected_links, false)?;
    if receipt_file_identity(&installed_metadata) != identity {
        return Err(invalid_receipt_data());
    }
    Ok(bytes)
}

fn random_receipt_suffix() -> std::io::Result<String> {
    let mut random = [0_u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut random)?;
    let mut encoded = String::with_capacity(random.len() * 2);
    for byte in random {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    Ok(encoded)
}

#[derive(Debug, Clone)]
pub struct WorkbenchExecutor {
    workspace_root: PathBuf,
    artifact_root: PathBuf,
    input_root: PathBuf,
    #[cfg(test)]
    fail_next_patch_reservation: Arc<AtomicBool>,
}

impl WorkbenchExecutor {
    pub fn new(workspace_root: impl Into<PathBuf>, artifact_root: impl Into<PathBuf>) -> Self {
        let workspace_root = workspace_root.into();
        Self {
            input_root: workspace_root.join(".inputs"),
            workspace_root,
            artifact_root: artifact_root.into(),
            #[cfg(test)]
            fail_next_patch_reservation: Arc::new(AtomicBool::new(false)),
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
            #[cfg(test)]
            fail_next_patch_reservation: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Reconcile the unscoped runtime receipt authority before serving.
    ///
    /// This must run exactly once on the root executor before startup
    /// attestation, readiness, or protocol input. Project-scoped artifact
    /// reconciliation deliberately does not own completion receipts.
    pub fn reconcile_root_completion_receipts_before_serving(
        &self,
    ) -> Result<(), WorkbenchErrorInfo> {
        self.reconcile_completion_receipts().map_err(|_| {
            recovery_error(
                "completion_receipt_invalid",
                "the completion receipt store failed its integrity boundary",
            )
        })
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
        let message = match result {
            Ok(success) => WorkbenchMessage::Result {
                schema_version: WORKBENCH_SCHEMA_VERSION,
                invocation_id: request.invocation_id.clone(),
                input_digest: request.input_digest.clone(),
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
        };
        if serialized_caller_result_size(&message)
            .is_some_and(|size| size <= WORKBENCH_MAX_CALLER_RESULT_BYTES)
        {
            message
        } else {
            failure_message(
                &request,
                started,
                WorkbenchOutcome::Failed,
                ExecutionError::new(
                    WorkbenchErrorClass::Resource,
                    "caller_result_too_large",
                    "the transient caller result exceeded its size boundary",
                    false,
                ),
            )
        }
    }

    /// Persist a durable-safe terminal projection before transient output is emitted.
    ///
    /// The immutable receipt closes the daemon-crash window between a completed
    /// tool effect and its durable orchestration transition. Transient output
    /// and inspected file content are removed; retry receives only the same
    /// outcome, resources, artifacts, and safe error without repeating effect.
    /// The M0 command child does not attest this receipt: receipt authority
    /// belongs to this runtime process after its own result validation. The M0
    /// profile is limited to direct allowlisted argv; this local receipt is not
    /// evidence that an external side effect has an idempotency or provider
    /// receipt.
    pub fn persist_completion_receipt(
        &self,
        message: &WorkbenchMessage,
    ) -> Result<(), WorkbenchErrorInfo> {
        let sealed = seal_terminal_result(message)?;
        let WorkbenchMessage::Result {
            invocation_id,
            input_digest,
            ..
        } = &sealed.result
        else {
            unreachable!("seal_terminal_result accepts only results");
        };
        let bytes = serde_json::to_vec(&sealed).map_err(|_| {
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
        let directory = self
            .open_completion_receipt_directory(true)?
            .ok_or_else(|| {
                recovery_error(
                    "completion_receipt_path_rejected",
                    "the completion receipt directory is unavailable",
                )
            })?;
        let destination = OsString::from(format!("{invocation_id}.json"));
        let temporary = OsString::from(format!(
            ".{invocation_id}.{}.tmp",
            random_receipt_suffix().map_err(receipt_error)?
        ));
        let mut file = directory
            .create_private_file(&temporary)
            .map_err(receipt_error)?;
        file.write_all(&bytes).map_err(receipt_error)?;
        file.sync_all().map_err(receipt_error)?;
        let temporary_identity =
            validate_receipt_file(&file.metadata().map_err(receipt_error)?, true)
                .map_err(receipt_error)?;
        match fs::hard_link(
            directory.child_path(&temporary),
            directory.child_path(&destination),
        ) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                directory
                    .remove_entry_if_identity(&temporary, temporary_identity, 1)
                    .map_err(receipt_error)?;
                let existing = self.recover_completion(invocation_id, input_digest)?;
                if existing != sealed.result {
                    return Err(recovery_error(
                        "completion_receipt_conflict",
                        "the immutable completion receipt conflicts with this result",
                    ));
                }
                directory.sync().map_err(receipt_error)?;
                return Ok(());
            }
            Err(error) => {
                let _ = directory.remove_entry_if_identity(&temporary, temporary_identity, 1);
                return Err(receipt_error(error));
            }
        }
        let installed = directory
            .open_private_file(&destination)
            .map_err(receipt_error)?;
        let installed_metadata = installed.metadata().map_err(receipt_error)?;
        let installed_identity = receipt_file_identity(&installed_metadata);
        if installed_identity != temporary_identity || installed_metadata.nlink() != 2 {
            return Err(recovery_error(
                "completion_receipt_invalid",
                "the completion receipt changed during atomic installation",
            ));
        }
        directory.sync().map_err(receipt_error)?;
        directory
            .remove_entry_if_identity(&temporary, temporary_identity, 2)
            .map_err(receipt_error)?;
        let installed = directory
            .open_private_file(&destination)
            .map_err(receipt_error)?;
        validate_receipt_file(&installed.metadata().map_err(receipt_error)?, false)
            .map_err(receipt_error)?;
        directory.sync().map_err(receipt_error)?;
        Ok(())
    }

    pub fn recover_completion(
        &self,
        invocation_id: &str,
        input_digest: &str,
    ) -> Result<WorkbenchMessage, WorkbenchErrorInfo> {
        validate_receipt_key(invocation_id, input_digest)?;
        let directory = self
            .open_completion_receipt_directory(false)?
            .ok_or_else(|| {
                recovery_error(
                    "completion_receipt_not_found",
                    "no durable completion receipt exists for this invocation",
                )
            })?;
        let name = OsString::from(format!("{invocation_id}.json"));
        let bytes = read_validated_receipt(&directory, &name, || {}).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                recovery_error(
                    "completion_receipt_not_found",
                    "no durable completion receipt exists for this invocation",
                )
            } else {
                receipt_error(error)
            }
        })?;
        let sealed: SealedCompletionReceipt = serde_json::from_slice(&bytes).map_err(|_| {
            recovery_error(
                "completion_receipt_invalid",
                "the completion receipt could not be decoded",
            )
        })?;
        let result = validate_sealed_completion_receipt(sealed)?;
        let WorkbenchMessage::Result {
            invocation_id: stored_invocation,
            input_digest: stored_digest,
            ..
        } = &result
        else {
            unreachable!("validated sealed receipt contains only results");
        };
        if stored_invocation != invocation_id || stored_digest != input_digest {
            return Err(recovery_error(
                "completion_receipt_binding_mismatch",
                "the completion receipt does not match the requested invocation",
            ));
        }
        Ok(result)
    }

    fn open_completion_receipt_directory(
        &self,
        create: bool,
    ) -> Result<Option<PinnedDirectory>, WorkbenchErrorInfo> {
        let artifact_root = match PinnedDirectory::open_chain(&self.artifact_root, create) {
            Ok(directory) => directory,
            Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(receipt_error(error)),
        };
        artifact_root
            .validate_owned_safe_directory(false)
            .map_err(receipt_error)?;
        match artifact_root.open_child_directory(COMPLETION_RECEIPT_DIRECTORY, create) {
            Ok(Some(directory)) => {
                directory
                    .validate_owned_safe_directory(true)
                    .map_err(receipt_error)?;
                Ok(Some(directory))
            }
            Ok(None) => Ok(None),
            Err(error) => Err(receipt_error(error)),
        }
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
                let limit = (*max_bytes)
                    .min(request.resource_limits.file_bytes)
                    .min(WORKBENCH_MAX_INSPECT_BYTES)
                    .try_into()
                    .unwrap_or(usize::MAX);
                let bytes = read_bounded_file(&path, limit)?;
                ensure_invocation_active(request, cancelled, started)?;
                let size = bytes.len();
                let digest = hex_sha256(&bytes);
                let content = String::from_utf8(bytes).map_err(|_| {
                    ExecutionError::tool(
                        "non_utf8_input",
                        "M0 file inspection accepts UTF-8 text only",
                    )
                })?;
                let mut output = BTreeMap::new();
                output.insert("sha256".to_string(), digest);
                output.insert("size_bytes".to_string(), size.to_string());
                output.insert("content".to_string(), content);
                Ok(ExecutionSuccess {
                    output,
                    bytes_read: size as u64,
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
                let expected_identity = verify_expected_digest(
                    &destination,
                    expected_sha256.as_deref(),
                    request.resource_limits.file_bytes,
                )?;
                ensure_invocation_active(request, cancelled, started)?;
                atomic_write_bound(
                    &destination,
                    bytes,
                    &request.invocation_id,
                    request.attempt,
                    expected_identity,
                )?;
                Ok(file_write_success(bytes))
            }
            WorkbenchTool::ApplyPatch {
                path,
                expected_sha256,
                replacements,
            } => {
                reject_input_mutation(request, path)?;
                let destination = self.resolve_existing(path)?;
                let read_limit = request
                    .resource_limits
                    .file_bytes
                    .try_into()
                    .unwrap_or(usize::MAX);
                let (original, original_identity) =
                    read_bounded_file_with_identity(&destination, read_limit)?;
                if hex_sha256(&original) != expected_sha256.as_str() {
                    return Err(ExecutionError::new(
                        WorkbenchErrorClass::Recovery,
                        "digest_conflict",
                        "the current file digest differs from the bound precondition",
                        false,
                    ));
                }
                let original_size = original.len();
                let mut updated = String::from_utf8(original).map_err(|_| {
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
                    let count = updated.matches(&replacement.old).count();
                    if u32::try_from(count).ok() != Some(replacement.expected_occurrences) {
                        return Err(ExecutionError::tool(
                            "patch_context_conflict",
                            "patch source text did not match the expected occurrence count",
                        ));
                    }
                    let removed = replacement
                        .old
                        .len()
                        .checked_mul(count)
                        .ok_or_else(|| patch_expansion_error())?;
                    let added = replacement
                        .new
                        .len()
                        .checked_mul(count)
                        .ok_or_else(|| patch_expansion_error())?;
                    let projected = updated
                        .len()
                        .checked_sub(removed)
                        .and_then(|size| size.checked_add(added))
                        .ok_or_else(patch_expansion_error)?;
                    if u64::try_from(projected).unwrap_or(u64::MAX)
                        > request.resource_limits.file_bytes
                    {
                        return Err(patch_expansion_error());
                    }
                    let mut replaced = String::new();
                    self.reserve_patch_output(&mut replaced, projected)?;
                    let mut cursor = 0;
                    for (offset, _) in updated.match_indices(&replacement.old) {
                        replaced.push_str(&updated[cursor..offset]);
                        replaced.push_str(&replacement.new);
                        cursor = offset + replacement.old.len();
                    }
                    replaced.push_str(&updated[cursor..]);
                    if replaced.len() != projected {
                        return Err(patch_expansion_error());
                    }
                    updated = replaced;
                }
                ensure_file_budget(updated.len() as u64, request.resource_limits.file_bytes)?;
                ensure_invocation_active(request, cancelled, started)?;
                atomic_write_bound(
                    &destination,
                    updated.as_bytes(),
                    &request.invocation_id,
                    request.attempt,
                    Some(original_identity),
                )?;
                let mut success = file_write_success(updated.as_bytes());
                success.bytes_read = original_size as u64;
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

    fn reserve_patch_output(
        &self,
        output: &mut String,
        projected: usize,
    ) -> Result<(), ExecutionError> {
        #[cfg(test)]
        if self
            .fail_next_patch_reservation
            .swap(false, Ordering::AcqRel)
        {
            return Err(patch_allocation_error());
        }
        output
            .try_reserve_exact(projected)
            .map_err(|_| patch_allocation_error())
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
        let scoped = Self::with_input_root(workspace_root, artifact_root, input_root);
        #[cfg(test)]
        let scoped = Self {
            fail_next_patch_reservation: self.fail_next_patch_reservation.clone(),
            ..scoped
        };
        Ok(scoped)
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
        let relative_deadline = started
            .checked_add(Duration::from_millis(request.resource_limits.wall_time_ms))
            .unwrap_or_else(Instant::now);
        let mut observed = ProcessGroupUsage::default();

        let (status, forced_error) = loop {
            if cancelled.load(Ordering::Acquire) {
                terminate_process_group(pid, &mut child)?;
                break (
                    None,
                    Some(ExecutionError::runtime(
                        "cancelled",
                        "invocation was cancelled",
                        false,
                    )),
                );
            }
            if command_deadline_expired(
                absolute_deadline,
                relative_deadline,
                SystemTime::now(),
                Instant::now(),
            ) {
                terminate_process_group(pid, &mut child)?;
                break (
                    None,
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
                terminate_process_group(pid, &mut child)?;
                break (
                    None,
                    Some(ExecutionError::new(
                        WorkbenchErrorClass::Resource,
                        code,
                        message,
                        false,
                    )),
                );
            }
            match owned_process_group_leader_exited(pid) {
                Ok(true) => {
                    let status = quiesce_process_group_after_leader_exit(pid, &mut child)?;
                    break (Some(status), None);
                }
                Ok(false) => thread::sleep(COMMAND_POLL_INTERVAL),
                Err(_) => {
                    terminate_process_group(pid, &mut child)?;
                    break (
                        None,
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
            if entries
                .windows(2)
                .any(|entries| entries[0].path == entries[1].path)
            {
                return Err(ExecutionError::tool(
                    "duplicate_artifact_path",
                    "artifact packaging resolved duplicate manifest paths",
                ));
            }
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

    fn reconcile_completion_receipts(&self) -> Result<(), ExecutionError> {
        let Some(directory) = self
            .open_completion_receipt_directory(false)
            .map_err(|_| completion_receipt_reconcile_error())?
        else {
            return Ok(());
        };
        let mut names = fs::read_dir(directory.child_path(OsStr::new(".")))
            .map_err(|_| completion_receipt_reconcile_error())?
            .map(|entry| {
                entry
                    .map(|entry| entry.file_name())
                    .map_err(|_| completion_receipt_reconcile_error())
            })
            .collect::<Result<Vec<_>, _>>()?;
        names.sort();
        let mut temporary_cleanup = Vec::new();
        let mut linked_destinations = BTreeMap::new();
        for name in names
            .iter()
            .filter(|name| name.to_str().is_some_and(is_completion_receipt_temporary))
        {
            let Some(name_text) = name.to_str() else {
                return Err(completion_receipt_reconcile_error());
            };
            let invocation_id = completion_receipt_temp_invocation(name_text)
                .ok_or_else(completion_receipt_reconcile_error)?;
            let file = directory
                .open_private_file(name)
                .map_err(|_| completion_receipt_reconcile_error())?;
            let metadata = file
                .metadata()
                .map_err(|_| completion_receipt_reconcile_error())?;
            let identity = receipt_file_identity(&metadata);
            match metadata.nlink() {
                1 => {
                    validate_receipt_metadata(&metadata, 1, true)
                        .map_err(|_| completion_receipt_reconcile_error())?;
                }
                2 => {
                    validate_receipt_metadata(&metadata, 2, true)
                        .map_err(|_| completion_receipt_reconcile_error())?;
                    let destination = OsString::from(format!("{invocation_id}.json"));
                    let installed = directory
                        .open_private_file(&destination)
                        .map_err(|_| completion_receipt_reconcile_error())?;
                    let installed_metadata = installed
                        .metadata()
                        .map_err(|_| completion_receipt_reconcile_error())?;
                    validate_receipt_metadata(&installed_metadata, 2, false)
                        .map_err(|_| completion_receipt_reconcile_error())?;
                    if receipt_file_identity(&installed_metadata) != identity {
                        return Err(completion_receipt_reconcile_error());
                    }
                    if names.binary_search(&destination).is_err()
                        || linked_destinations.insert(destination, identity).is_some()
                    {
                        return Err(completion_receipt_reconcile_error());
                    }
                }
                _ => return Err(completion_receipt_reconcile_error()),
            }
            temporary_cleanup.push((name.clone(), identity, metadata.nlink()));
        }
        let mut validated_receipts = BTreeMap::new();
        for name in names
            .iter()
            .filter(|name| !name.to_str().is_some_and(is_completion_receipt_temporary))
        {
            let Some(name_text) = name.to_str() else {
                return Err(completion_receipt_reconcile_error());
            };
            let Some(invocation_id) = name_text.strip_suffix(".json") else {
                return Err(completion_receipt_reconcile_error());
            };
            if !valid_receipt_invocation_id(invocation_id) {
                return Err(completion_receipt_reconcile_error());
            }
            let bytes = if let Some(identity) = linked_destinations.get(name) {
                read_validated_receipt_for_reconciliation(&directory, name, 2, *identity)
            } else {
                read_validated_receipt(&directory, name, || {})
            }
            .map_err(|_| completion_receipt_reconcile_error())?;
            let sealed: SealedCompletionReceipt =
                serde_json::from_slice(&bytes).map_err(|_| completion_receipt_reconcile_error())?;
            let result = validate_sealed_completion_receipt(sealed)
                .map_err(|_| completion_receipt_reconcile_error())?;
            let WorkbenchMessage::Result {
                invocation_id: stored_invocation,
                ..
            } = result
            else {
                unreachable!("validated sealed receipt contains only results");
            };
            if stored_invocation != invocation_id {
                return Err(completion_receipt_reconcile_error());
            }
            validated_receipts.insert(name.clone(), bytes);
        }
        for (name, identity, links) in &temporary_cleanup {
            directory
                .remove_entry_if_identity(name, *identity, *links)
                .map_err(|_| completion_receipt_reconcile_error())?;
        }
        directory
            .sync()
            .map_err(|_| completion_receipt_reconcile_error())?;
        for (name, identity) in linked_destinations {
            let bytes = read_validated_receipt_for_reconciliation(&directory, &name, 1, identity)
                .map_err(|_| completion_receipt_reconcile_error())?;
            if validated_receipts.get(&name) != Some(&bytes) {
                return Err(completion_receipt_reconcile_error());
            }
        }
        directory
            .sync()
            .map_err(|_| completion_receipt_reconcile_error())?;
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
    if path
        .components()
        .next()
        .is_some_and(|component| component.as_os_str() == std::ffi::OsStr::new(".inputs"))
        || request
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

fn seal_terminal_result(
    message: &WorkbenchMessage,
) -> Result<SealedCompletionReceipt, WorkbenchErrorInfo> {
    let WorkbenchMessage::Result {
        invocation_id,
        input_digest,
        error,
        ..
    } = message
    else {
        return Err(recovery_error(
            "completion_receipt_not_terminal",
            "only a terminal workbench result can be persisted",
        ));
    };
    if error
        .as_ref()
        .is_some_and(|error| error.code == "command_cleanup_failed")
    {
        return Err(WorkbenchErrorInfo {
            class: WorkbenchErrorClass::Runtime,
            code: "command_cleanup_failed".to_string(),
            safe_message: "the command process tree is not proven quiescent".to_string(),
            retryable: false,
        });
    }
    validate_receipt_key(invocation_id, input_digest)?;
    let mut durable_result = message.clone();
    let WorkbenchMessage::Result { output, .. } = &mut durable_result else {
        unreachable!("terminal receipt validation already rejected non-results");
    };
    output.clear();
    let result_bytes = serde_json::to_vec(&durable_result).map_err(|_| {
        recovery_error(
            "caller_result_encode_failed",
            "the durable-safe caller result could not be encoded",
        )
    })?;
    if result_bytes.len() > WORKBENCH_MAX_CALLER_RESULT_BYTES {
        return Err(recovery_error(
            "caller_result_too_large",
            "the durable-safe caller result exceeded its size boundary",
        ));
    }
    Ok(SealedCompletionReceipt {
        schema_version: COMPLETION_RECEIPT_SCHEMA_VERSION,
        invocation_id: invocation_id.clone(),
        input_digest: input_digest.clone(),
        result_digest: hex_sha256(&result_bytes),
        result: durable_result,
    })
}

fn serialized_caller_result_size(message: &WorkbenchMessage) -> Option<usize> {
    serde_json::to_vec(message).ok().map(|bytes| bytes.len())
}

fn validate_sealed_completion_receipt(
    sealed: SealedCompletionReceipt,
) -> Result<WorkbenchMessage, WorkbenchErrorInfo> {
    if sealed.schema_version != COMPLETION_RECEIPT_SCHEMA_VERSION {
        return Err(recovery_error(
            "completion_receipt_version_unsupported",
            "the completion receipt version is unsupported",
        ));
    }
    let validated = seal_terminal_result(&sealed.result)?;
    if sealed.invocation_id != validated.invocation_id
        || sealed.input_digest != validated.input_digest
        || sealed.result_digest != validated.result_digest
        || sealed.result != validated.result
    {
        return Err(recovery_error(
            "completion_receipt_binding_mismatch",
            "the completion receipt result binding is invalid",
        ));
    }
    Ok(validated.result)
}

fn validate_receipt_key(invocation_id: &str, input_digest: &str) -> Result<(), WorkbenchErrorInfo> {
    let digest_valid = input_digest.len() == 64
        && input_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
    if !valid_receipt_invocation_id(invocation_id) || !digest_valid {
        return Err(recovery_error(
            "completion_receipt_key_invalid",
            "the completion receipt key is invalid",
        ));
    }
    Ok(())
}

fn valid_receipt_invocation_id(invocation_id: &str) -> bool {
    invocation_id.len() == 36
        && invocation_id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()
            }
        })
}

fn is_completion_receipt_temporary(name: &str) -> bool {
    completion_receipt_temp_invocation(name).is_some()
}

fn completion_receipt_temp_invocation(name: &str) -> Option<&str> {
    let value = name
        .strip_prefix('.')
        .and_then(|name| name.strip_suffix(".tmp"))?;
    let (invocation_id, token) = value.split_once('.')?;
    (valid_receipt_invocation_id(invocation_id)
        && !token.is_empty()
        && token.len() <= 64
        && token.bytes().all(|byte| byte.is_ascii_hexdigit()))
    .then_some(invocation_id)
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

fn completion_receipt_reconcile_error() -> ExecutionError {
    ExecutionError::new(
        WorkbenchErrorClass::Recovery,
        "completion_receipt_invalid",
        "the completion receipt store failed its integrity boundary",
        false,
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
    let page_size = sysconf(SysconfVar::PAGE_SIZE).ok().flatten().unwrap_or(0);
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
        let mut entry_paths = BTreeSet::new();
        for entry in self.entries {
            if checked_relative(&entry.path).is_err()
                || !is_lower_hex_digest(&entry.sha256)
                || entry.blob_id != format!("sha256:{}", entry.sha256)
                || !entry_paths.insert(entry.path.clone())
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

fn checked_relative(value: &str) -> Result<&Path, ExecutionError> {
    let path = Path::new(value);
    let mut canonical = String::new();
    for component in path.components() {
        let Component::Normal(component) = component else {
            return Err(ExecutionError::workspace(
                "invalid_path",
                "tool path must use its canonical workspace-relative representation",
            ));
        };
        let Some(component) = component.to_str() else {
            return Err(ExecutionError::workspace(
                "invalid_path",
                "tool path must use its canonical workspace-relative representation",
            ));
        };
        if !canonical.is_empty() {
            canonical.push('/');
        }
        canonical.push_str(component);
    }
    if canonical.is_empty() || canonical != value {
        return Err(ExecutionError::workspace(
            "invalid_path",
            "tool path must use its canonical workspace-relative representation",
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

fn verify_expected_digest(
    path: &Path,
    expected: Option<&str>,
    file_limit: u64,
) -> Result<Option<BoundedFileIdentity>, ExecutionError> {
    let Some(expected) = expected else {
        return Ok(None);
    };
    let (bytes, identity) =
        read_bounded_file_with_identity(path, file_limit.try_into().unwrap_or(usize::MAX))?;
    if hex_sha256(&bytes) != expected {
        return Err(ExecutionError::new(
            WorkbenchErrorClass::Recovery,
            "digest_conflict",
            "the current file digest differs from the bound precondition",
            false,
        ));
    }
    Ok(Some(identity))
}

fn atomic_write(
    destination: &Path,
    bytes: &[u8],
    invocation_id: &str,
    attempt: u32,
) -> Result<(), ExecutionError> {
    atomic_write_bound(destination, bytes, invocation_id, attempt, None)
}

fn atomic_write_bound(
    destination: &Path,
    bytes: &[u8],
    invocation_id: &str,
    attempt: u32,
    expected_identity: Option<BoundedFileIdentity>,
) -> Result<(), ExecutionError> {
    if let Some(expected_identity) = expected_identity {
        verify_bounded_file_path_identity(destination, expected_identity)?;
    }
    if destination.exists() {
        if existing_file_equals(destination, bytes)? {
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
        if let Some(expected_identity) = expected_identity {
            verify_bounded_file_path_identity(destination, expected_identity)?;
        }
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
        if existing_file_equals(destination, bytes)? {
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
    read_bounded_file_with_identity(path, limit).map(|(bytes, _)| bytes)
}

fn read_bounded_file_with_identity(
    path: &Path,
    limit: usize,
) -> Result<(Vec<u8>, BoundedFileIdentity), ExecutionError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(path)
        .map_err(workspace_io_error)?;
    let before = file.metadata().map_err(workspace_io_error)?;
    if !before.is_file() || before.len() > u64::try_from(limit).unwrap_or(u64::MAX) {
        return Err(ExecutionError::new(
            WorkbenchErrorClass::Resource,
            "file_limit_exceeded",
            "the requested file exceeds the invocation read limit",
            false,
        ));
    }
    let expected_size = usize::try_from(before.len()).map_err(|_| file_read_allocation_error())?;
    let read_boundary = before.len().saturating_add(1);
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(expected_size.saturating_add(1))
        .map_err(|_| file_read_allocation_error())?;
    (&mut file)
        .take(read_boundary)
        .read_to_end(&mut bytes)
        .map_err(workspace_io_error)?;
    let after = file.metadata().map_err(workspace_io_error)?;
    let identity = BoundedFileIdentity {
        device: after.dev(),
        inode: after.ino(),
        size: after.len(),
    };
    if bytes.len() > limit
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
        || after.len() != bytes.len() as u64
    {
        return Err(ExecutionError::new(
            WorkbenchErrorClass::Resource,
            "file_limit_exceeded",
            "the requested file exceeded or changed within the invocation read limit",
            false,
        ));
    }
    verify_bounded_file_path_identity(path, identity)?;
    Ok((bytes, identity))
}

fn verify_bounded_file_path_identity(
    path: &Path,
    expected: BoundedFileIdentity,
) -> Result<(), ExecutionError> {
    let metadata = fs::symlink_metadata(path).map_err(workspace_io_error)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.dev() != expected.device
        || metadata.ino() != expected.inode
        || metadata.len() != expected.size
    {
        return Err(ExecutionError::new(
            WorkbenchErrorClass::Recovery,
            "file_identity_changed",
            "the bound file identity changed before the effect committed",
            false,
        ));
    }
    Ok(())
}

fn existing_file_equals(path: &Path, expected: &[u8]) -> Result<bool, ExecutionError> {
    let metadata = fs::symlink_metadata(path).map_err(workspace_io_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(workspace_io_error(std::io::Error::other(
            "existing destination is not a regular file",
        )));
    }
    if metadata.len() != expected.len() as u64 {
        return Ok(false);
    }
    Ok(read_bounded_file(path, expected.len())? == expected)
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

fn patch_expansion_error() -> ExecutionError {
    ExecutionError::new(
        WorkbenchErrorClass::Resource,
        "file_limit_exceeded",
        "patch expansion exceeds the invocation file limit",
        false,
    )
}

fn patch_allocation_error() -> ExecutionError {
    ExecutionError::new(
        WorkbenchErrorClass::Resource,
        "patch_allocation_failed",
        "patch output memory could not be reserved within the invocation boundary",
        false,
    )
}

fn file_read_allocation_error() -> ExecutionError {
    ExecutionError::new(
        WorkbenchErrorClass::Resource,
        "file_read_allocation_failed",
        "file input memory could not be reserved within the invocation boundary",
        false,
    )
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

fn command_deadline_expired(
    absolute_deadline: SystemTime,
    relative_deadline: Instant,
    wall_now: SystemTime,
    monotonic_now: Instant,
) -> bool {
    wall_now >= absolute_deadline || monotonic_now >= relative_deadline
}

fn terminate_process_group(
    process_group: u32,
    child: &mut std::process::Child,
) -> Result<(), ExecutionError> {
    validate_owned_process_group_leader(process_group)?;
    let process_group_i32 = i32::try_from(process_group).map_err(|_| command_cleanup_error())?;
    match killpg(Pid::from_raw(process_group_i32), Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => {}
        Err(_) => return Err(command_cleanup_error()),
    }
    wait_for_owned_process_group_leader_and_descendants(process_group)?;
    child.wait().map_err(|_| command_cleanup_error())?;
    verify_reaped_process_group_quiescence(process_group)
}

fn quiesce_process_group_after_leader_exit(
    process_group: u32,
    child: &mut std::process::Child,
) -> Result<std::process::ExitStatus, ExecutionError> {
    if !owned_process_group_leader_exited(process_group)? {
        return Err(command_cleanup_error());
    }
    let process_group_i32 = i32::try_from(process_group).map_err(|_| command_cleanup_error())?;
    match killpg(Pid::from_raw(process_group_i32), Signal::SIGKILL) {
        Ok(()) => {}
        Err(Errno::ESRCH) if process_group_member_count(process_group)? <= 1 => {}
        Err(_) => return Err(command_cleanup_error()),
    }
    wait_for_owned_process_group_leader_and_descendants(process_group)?;
    let status = child.wait().map_err(|_| command_cleanup_error())?;
    verify_reaped_process_group_quiescence(process_group)?;
    Ok(status)
}

fn wait_for_owned_process_group_leader_and_descendants(
    process_group: u32,
) -> Result<(), ExecutionError> {
    let deadline = Instant::now() + COMMAND_CLEANUP_GRACE;
    loop {
        if owned_process_group_leader_exited(process_group)?
            && process_group_member_count(process_group)? <= 1
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(command_cleanup_error());
        }
        thread::sleep(COMMAND_POLL_INTERVAL);
    }
}

fn verify_reaped_process_group_quiescence(process_group: u32) -> Result<(), ExecutionError> {
    let deadline = Instant::now() + COMMAND_CLEANUP_GRACE;
    loop {
        if process_group_member_count(process_group)? == 0 {
            return Ok(());
        }
        if Instant::now() >= deadline {
            // The exact child has already been reaped. Never signal this numeric
            // process-group identifier again because it is now reusable.
            return Err(command_cleanup_error());
        }
        thread::sleep(COMMAND_POLL_INTERVAL);
    }
}

fn owned_process_group_leader_exited(process_group: u32) -> Result<bool, ExecutionError> {
    let (state, actual_group) = read_process_state_and_group(process_group)?;
    if actual_group != process_group {
        return Err(command_cleanup_error());
    }
    Ok(matches!(state, 'Z' | 'X'))
}

fn validate_owned_process_group_leader(process_group: u32) -> Result<(), ExecutionError> {
    let (_, actual_group) = read_process_state_and_group(process_group)?;
    if actual_group != process_group {
        return Err(command_cleanup_error());
    }
    Ok(())
}

fn read_process_state_and_group(process: u32) -> Result<(char, u32), ExecutionError> {
    let stat =
        fs::read_to_string(format!("/proc/{process}/stat")).map_err(|_| command_cleanup_error())?;
    let after_name = stat
        .rsplit_once(')')
        .map(|(_, rest)| rest.trim())
        .ok_or_else(command_cleanup_error)?;
    let mut fields = after_name.split_whitespace();
    let state = fields
        .next()
        .and_then(|value| value.chars().next())
        .ok_or_else(command_cleanup_error)?;
    let _parent = fields.next().ok_or_else(command_cleanup_error)?;
    let group = fields
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(command_cleanup_error)?;
    Ok((state, group))
}

fn process_group_member_count(process_group: u32) -> Result<u32, ExecutionError> {
    let entries = fs::read_dir("/proc").map_err(|_| command_cleanup_error())?;
    let mut count = 0_u32;
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
        let group = after_name
            .split_whitespace()
            .nth(2)
            .and_then(|value| value.parse::<u32>().ok());
        if group == Some(process_group) {
            count = count.saturating_add(1);
        }
    }
    Ok(count)
}

fn command_cleanup_error() -> ExecutionError {
    ExecutionError::runtime(
        "command_cleanup_failed",
        "the command process group could not be proven quiescent",
        false,
    )
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

    fn completion_result() -> (WorkbenchMessage, String, String) {
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
            outcome: WorkbenchOutcome::Failed,
            resources: WorkbenchResourceUsage {
                duration_ms: 17,
                bytes_read: 23,
                ..WorkbenchResourceUsage::default()
            },
            artifacts: vec![WorkbenchArtifactRef {
                artifact_id: format!("sha256:{}", "d".repeat(64)),
                sha256: "d".repeat(64),
                artifact_kind: "source_tree".to_string(),
                media_type: "application/json".to_string(),
                size_bytes: 31,
                manifest_path: "receipt-safe.manifest.json".to_string(),
            }],
            output: BTreeMap::from([("content".to_string(), "PRIVATE".to_string())]),
            error: Some(WorkbenchErrorInfo {
                class: WorkbenchErrorClass::Tool,
                code: "safe_failure".to_string(),
                safe_message: "the tool failed safely".to_string(),
                retryable: false,
            }),
        };
        (result, request.invocation_id, request.input_digest)
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
    fn inspect_result_fits_transient_budget_under_worst_case_json_escaping() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let artifacts = directory.path().join("artifacts");
        fs::create_dir_all(scoped(&workspace).join("src")).unwrap();
        fs::write(
            scoped(&workspace).join("src/control.txt"),
            vec![0_u8; WORKBENCH_MAX_INSPECT_BYTES as usize],
        )
        .unwrap();
        let executor = WorkbenchExecutor::new(&workspace, &artifacts);
        let result = executor.execute(
            request(
                WorkbenchTool::InspectFile {
                    path: "src/control.txt".to_string(),
                    max_bytes: WORKBENCH_MAX_INSPECT_BYTES,
                },
                "file.inspect",
            ),
            Arc::new(AtomicBool::new(false)),
        );
        assert_eq!(outcome(&result), WorkbenchOutcome::Succeeded);
        assert!(
            serialized_caller_result_size(&result).unwrap() <= WORKBENCH_MAX_CALLER_RESULT_BYTES
        );

        fs::write(scoped(&workspace).join("src/control.txt"), [0xff]).unwrap();
        let non_utf8 = executor.execute(
            request(
                WorkbenchTool::InspectFile {
                    path: "src/control.txt".to_string(),
                    max_bytes: WORKBENCH_MAX_INSPECT_BYTES,
                },
                "file.inspect",
            ),
            Arc::new(AtomicBool::new(false)),
        );
        let WorkbenchMessage::Result {
            error: Some(error), ..
        } = non_utf8
        else {
            panic!("non-UTF-8 inspection did not fail closed");
        };
        assert_eq!(error.code, "non_utf8_input");

        let transient_content = "INSPECT-TRANSIENT-CONTENT-ONLY";
        fs::write(
            scoped(&workspace).join("src/control.txt"),
            transient_content,
        )
        .unwrap();
        let inspect_request = request(
            WorkbenchTool::InspectFile {
                path: "src/control.txt".to_string(),
                max_bytes: WORKBENCH_MAX_INSPECT_BYTES,
            },
            "file.inspect",
        );
        let immediate = executor.execute(inspect_request.clone(), Arc::new(AtomicBool::new(false)));
        let WorkbenchMessage::Result { output, .. } = &immediate else {
            panic!("inspection did not return a result");
        };
        assert_eq!(
            output.get("content").map(String::as_str),
            Some(transient_content)
        );
        executor.persist_completion_receipt(&immediate).unwrap();
        let replay = executor
            .recover_completion(
                &inspect_request.invocation_id,
                &inspect_request.input_digest,
            )
            .unwrap();
        let WorkbenchMessage::Result { output, .. } = replay else {
            panic!("inspection replay did not return a durable-safe result");
        };
        assert!(output.is_empty());
        let receipt = artifacts
            .join(COMPLETION_RECEIPT_DIRECTORY)
            .join(format!("{}.json", inspect_request.invocation_id));
        assert!(!fs::read(receipt)
            .unwrap()
            .windows(transient_content.len())
            .any(|bytes| bytes == transient_content.as_bytes()));
    }

    #[test]
    fn patch_reads_and_allocations_are_bounded_without_mutation_and_executor_remains_usable() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let artifacts = directory.path().join("artifacts");
        fs::create_dir_all(scoped(&workspace).join("src")).unwrap();
        let original = "a".repeat(1024 * 1024);
        let path = scoped(&workspace).join("src/index.txt");
        fs::write(&path, &original).unwrap();
        let executor = WorkbenchExecutor::new(&workspace, &artifacts);
        let result = executor.execute(
            request(
                WorkbenchTool::ApplyPatch {
                    path: "src/index.txt".to_string(),
                    expected_sha256: hex_sha256(original.as_bytes()),
                    replacements: vec![sentinel_common::TextReplacement {
                        old: "a".to_string(),
                        new: "b".repeat(64 * 1024),
                        expected_occurrences: 1024 * 1024,
                    }],
                },
                "patch.apply",
            ),
            Arc::new(AtomicBool::new(false)),
        );
        let WorkbenchMessage::Result {
            error: Some(error), ..
        } = result
        else {
            panic!("expansion bomb did not return a typed terminal error");
        };
        assert_eq!(error.code, "file_limit_exceeded");
        assert_eq!(fs::read_to_string(&path).unwrap(), original);

        let oversized = "x".repeat(1025);
        fs::write(&path, &oversized).unwrap();
        let mut oversized_request = request(
            WorkbenchTool::ApplyPatch {
                path: "src/index.txt".to_string(),
                expected_sha256: hex_sha256(oversized.as_bytes()),
                replacements: vec![sentinel_common::TextReplacement {
                    old: "x".to_string(),
                    new: "y".to_string(),
                    expected_occurrences: 1025,
                }],
            },
            "patch.apply",
        );
        oversized_request.resource_limits.file_bytes = 1024;
        oversized_request.input_digest = oversized_request.canonical_digest().unwrap();
        let oversized_result =
            executor.execute(oversized_request, Arc::new(AtomicBool::new(false)));
        let WorkbenchMessage::Result {
            error: Some(error), ..
        } = oversized_result
        else {
            panic!("oversized existing patch input did not fail closed");
        };
        assert_eq!(error.code, "file_limit_exceeded");
        assert_eq!(fs::read_to_string(&path).unwrap(), oversized);

        let allocation_input = "allocation-bound";
        fs::write(&path, allocation_input).unwrap();
        executor
            .fail_next_patch_reservation
            .store(true, Ordering::Release);
        let allocation_result = executor.execute(
            request(
                WorkbenchTool::ApplyPatch {
                    path: "src/index.txt".to_string(),
                    expected_sha256: hex_sha256(allocation_input.as_bytes()),
                    replacements: vec![sentinel_common::TextReplacement {
                        old: "bound".to_string(),
                        new: "safe".to_string(),
                        expected_occurrences: 1,
                    }],
                },
                "patch.apply",
            ),
            Arc::new(AtomicBool::new(false)),
        );
        let WorkbenchMessage::Result {
            error: Some(error), ..
        } = allocation_result
        else {
            panic!("injected patch allocation failure did not fail closed");
        };
        assert_eq!(error.code, "patch_allocation_failed");
        assert_eq!(fs::read_to_string(&path).unwrap(), allocation_input);

        let (_, original_identity) = read_bounded_file_with_identity(&path, 1024).unwrap();
        let displaced = scoped(&workspace).join("src/displaced.txt");
        fs::rename(&path, &displaced).unwrap();
        fs::write(&path, allocation_input).unwrap();
        let identity_error = atomic_write_bound(
            &path,
            b"replacement",
            "018f3f32-4f01-7f2c-a6c1-f6f4a81b2801",
            1,
            Some(original_identity),
        )
        .unwrap_err();
        assert_eq!(identity_error.code, "file_identity_changed");
        assert_eq!(fs::read_to_string(&path).unwrap(), allocation_input);

        let next_effect = executor.execute(
            request(
                WorkbenchTool::WriteFile {
                    path: "src/next.txt".to_string(),
                    content: "healthy".to_string(),
                    expected_sha256: None,
                },
                "file.write",
            ),
            Arc::new(AtomicBool::new(false)),
        );
        assert_eq!(outcome(&next_effect), WorkbenchOutcome::Succeeded);
    }

    #[test]
    fn relative_command_deadline_is_monotonic_across_wall_clock_rollback() {
        let started = Instant::now();
        let relative_deadline = started + Duration::from_millis(10);
        let absolute_deadline = UNIX_EPOCH + Duration::from_secs(10_000);
        assert!(command_deadline_expired(
            absolute_deadline,
            relative_deadline,
            UNIX_EPOCH,
            relative_deadline + Duration::from_millis(1),
        ));
    }

    #[test]
    fn command_terminalization_quiesces_descendants_on_normal_cancel_and_deadline_paths() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("workspace")).unwrap();
        let executor = WorkbenchExecutor::new(
            directory.path().join("workspace"),
            directory.path().join("artifacts"),
        );
        let normal = request(
            WorkbenchTool::RunCommand {
                program: "sh".to_string(),
                args: Vec::new(),
            },
            "command.run_allowlisted",
        );
        let normal_result = executor.run_command(
            &normal,
            "sh",
            &[
                "-c".to_string(),
                "sleep 30 </dev/null >/dev/null 2>&1 &".to_string(),
            ],
            None,
            &AtomicBool::new(false),
            Instant::now(),
        );
        assert!(normal_result.is_ok());

        let cancelled = AtomicBool::new(true);
        let cancelled_result = executor.run_command(
            &normal,
            "sh",
            &["-c".to_string(), "sleep 30 & wait".to_string()],
            None,
            &cancelled,
            Instant::now(),
        );
        let cancelled_error = match cancelled_result {
            Ok(_) => panic!("cancelled command unexpectedly succeeded"),
            Err(error) => error,
        };
        assert_eq!(cancelled_error.code, "cancelled");

        let mut expired = normal;
        expired.deadline_unix_ms = unix_time_ms().saturating_add(20);
        expired.resource_limits.wall_time_ms = 20;
        let deadline_result = executor.run_command(
            &expired,
            "sh",
            &["-c".to_string(), "sleep 30 & wait".to_string()],
            None,
            &AtomicBool::new(false),
            Instant::now(),
        );
        let deadline_error = match deadline_result {
            Ok(_) => panic!("expired command unexpectedly succeeded"),
            Err(error) => error,
        };
        assert_eq!(deadline_error.code, "deadline_expired");
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

        let transient_command_output = "TRANSIENT-COMMAND-OUTPUT-ONLY";
        let mut command = request(
            WorkbenchTool::RunCommand {
                program: "printf".to_string(),
                args: vec![transient_command_output.to_string()],
            },
            "command.run_allowlisted",
        );
        command.command_policy = vec![CommandRule {
            program: "printf".to_string(),
            required_arg_prefix: Vec::new(),
            max_args: 1,
        }];
        command.input_digest = command.canonical_digest().unwrap();
        let immediate = executor.execute(command.clone(), Arc::new(AtomicBool::new(false)));
        let WorkbenchMessage::Result {
            outcome: immediate_outcome,
            output,
            ..
        } = &immediate
        else {
            panic!("command execution must return a result");
        };
        assert_eq!(*immediate_outcome, WorkbenchOutcome::Succeeded);
        assert_eq!(
            output.get("stdout").map(String::as_str),
            Some(transient_command_output)
        );
        executor.persist_completion_receipt(&immediate).unwrap();
        let replay = executor
            .recover_completion(&command.invocation_id, &command.input_digest)
            .unwrap();
        let WorkbenchMessage::Result { output, .. } = replay else {
            panic!("command replay did not return a durable-safe result");
        };
        assert!(output.is_empty());
        let receipt = directory
            .path()
            .join("artifacts")
            .join(COMPLETION_RECEIPT_DIRECTORY)
            .join(format!("{}.json", command.invocation_id));
        assert!(!fs::read(receipt)
            .unwrap()
            .windows(transient_command_output.len())
            .any(|bytes| bytes == transient_command_output.as_bytes()));

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
        let excessive_output = executor.execute(excessive_output, Arc::new(AtomicBool::new(false)));
        let WorkbenchMessage::Result {
            outcome: excessive_outcome,
            error,
            ..
        } = excessive_output
        else {
            panic!("command execution must return a result")
        };
        assert_eq!(excessive_outcome, WorkbenchOutcome::Failed);
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
    fn executor_paths_and_recovered_manifest_entries_are_canonical_and_unique() {
        assert!(checked_relative("src/index.html").is_ok());
        for alias in [
            ".",
            "./src/index.html",
            "src/./index.html",
            "src//index.html",
        ] {
            assert!(checked_relative(alias).is_err(), "accepted alias {alias}");
        }

        let digest = "d".repeat(64);
        let manifest = StoredArtifactManifest {
            schema_version: 1,
            invocation_id: "018f3f32-4f01-7f2c-a6c1-f6f4a81b2801".to_string(),
            input_digest: "a".repeat(64),
            project_id: "project-01".to_string(),
            work_item_id: "work-04".to_string(),
            workspace_id: "project-01:work-04".to_string(),
            agent_id: 7,
            artifact_kind: "source_tree".to_string(),
            media_type: "application/json".to_string(),
            runtime_key: sentinel_common::WORKBENCH_RUNTIME_BWRAP.to_string(),
            tool_profile: "web-authoring-v1".to_string(),
            tool_profile_digest: "b".repeat(64),
            policy_digest: "c".repeat(64),
            entries: vec![
                StoredArtifactManifestEntry {
                    path: "src/index.html".to_string(),
                    blob_id: format!("sha256:{digest}"),
                    sha256: digest.clone(),
                    size_bytes: 8,
                },
                StoredArtifactManifestEntry {
                    path: "src/index.html".to_string(),
                    blob_id: format!("sha256:{digest}"),
                    sha256: digest,
                    size_bytes: 8,
                },
            ],
        };
        assert!(manifest.referenced_blobs().is_err());
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
        fs::set_permissions(assigned.join("brief.md"), fs::Permissions::from_mode(0o444)).unwrap();
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
        assert_eq!(
            fs::read_to_string(assigned.join("brief.md")).unwrap(),
            "assigned"
        );

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
    fn completion_receipt_is_immutable_digest_bound_and_durable_safe() {
        let directory = tempfile::tempdir().unwrap();
        let executor = WorkbenchExecutor::new(
            directory.path().join("workspace"),
            directory.path().join("artifacts"),
        );
        let (result, invocation_id, input_digest) = completion_result();

        executor.persist_completion_receipt(&result).unwrap();
        executor.persist_completion_receipt(&result).unwrap();
        let recovered = executor
            .recover_completion(&invocation_id, &input_digest)
            .unwrap();
        let encoded = serde_json::to_string(&recovered).unwrap();
        assert!(!encoded.contains("PRIVATE"));
        assert!(encoded.contains(&input_digest));
        let receipt = directory
            .path()
            .join("artifacts")
            .join(COMPLETION_RECEIPT_DIRECTORY)
            .join(format!("{invocation_id}.json"));
        assert!(!fs::read(&receipt)
            .unwrap()
            .windows("PRIVATE".len())
            .any(|bytes| bytes == b"PRIVATE"));
        let WorkbenchMessage::Result {
            outcome,
            resources,
            artifacts,
            output,
            error,
            ..
        } = &recovered
        else {
            panic!("receipt replay did not return a terminal result");
        };
        assert_eq!(*outcome, WorkbenchOutcome::Failed);
        assert_eq!(resources.duration_ms, 17);
        assert_eq!(resources.bytes_read, 23);
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].size_bytes, 31);
        assert_eq!(
            error.as_ref().map(|error| error.code.as_str()),
            Some("safe_failure")
        );
        assert!(output.is_empty());
        assert!(executor
            .recover_completion(&invocation_id, &"c".repeat(64))
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

    #[test]
    fn unresolved_command_cleanup_cannot_publish_a_terminal_receipt() {
        let directory = tempfile::tempdir().unwrap();
        let executor = WorkbenchExecutor::new(
            directory.path().join("workspace"),
            directory.path().join("artifacts"),
        );
        let (mut result, invocation_id, input_digest) = completion_result();
        let WorkbenchMessage::Result { outcome, error, .. } = &mut result else {
            unreachable!();
        };
        *outcome = WorkbenchOutcome::Failed;
        *error = Some(command_cleanup_error().info());

        assert_eq!(
            executor
                .persist_completion_receipt(&result)
                .unwrap_err()
                .code,
            "command_cleanup_failed"
        );
        assert_eq!(
            executor
                .recover_completion(&invocation_id, &input_digest)
                .unwrap_err()
                .code,
            "completion_receipt_not_found"
        );
    }

    #[test]
    fn completion_receipt_rejects_symlinked_directory_component_and_hardlink() {
        let directory = tempfile::tempdir().unwrap();
        let artifact_root = directory.path().join("artifacts");
        let foreign = directory.path().join("foreign");
        fs::create_dir(&artifact_root).unwrap();
        fs::create_dir(&foreign).unwrap();
        symlink(&foreign, artifact_root.join(COMPLETION_RECEIPT_DIRECTORY)).unwrap();
        let executor = WorkbenchExecutor::new(directory.path().join("workspace"), &artifact_root);
        let (result, invocation_id, input_digest) = completion_result();
        assert!(executor.persist_completion_receipt(&result).is_err());
        assert!(executor
            .recover_completion(&invocation_id, &input_digest)
            .is_err());

        fs::remove_file(artifact_root.join(COMPLETION_RECEIPT_DIRECTORY)).unwrap();
        executor.persist_completion_receipt(&result).unwrap();
        let receipt = artifact_root
            .join(COMPLETION_RECEIPT_DIRECTORY)
            .join(format!("{invocation_id}.json"));
        fs::hard_link(&receipt, artifact_root.join("receipt-hardlink")).unwrap();
        assert!(executor
            .recover_completion(&invocation_id, &input_digest)
            .is_err());
    }

    #[test]
    fn completion_receipt_rejects_tampered_or_reintroduced_transient_output() {
        let directory = tempfile::tempdir().unwrap();
        let artifact_root = directory.path().join("artifacts");
        let executor = WorkbenchExecutor::new(directory.path().join("workspace"), &artifact_root);
        let (result, invocation_id, input_digest) = completion_result();
        executor.persist_completion_receipt(&result).unwrap();
        let receipt = artifact_root
            .join(COMPLETION_RECEIPT_DIRECTORY)
            .join(format!("{invocation_id}.json"));
        let mut sealed: serde_json::Value =
            serde_json::from_slice(&fs::read(&receipt).unwrap()).unwrap();
        sealed["result"]["output"]["content"] = serde_json::json!("TAMPERED");
        fs::write(&receipt, serde_json::to_vec(&sealed).unwrap()).unwrap();
        assert_eq!(
            executor
                .recover_completion(&invocation_id, &input_digest)
                .unwrap_err()
                .code,
            "completion_receipt_binding_mismatch"
        );
    }

    #[test]
    fn completion_receipt_rejects_name_replacement_after_descriptor_open() {
        let directory = tempfile::tempdir().unwrap();
        let artifact_root = directory.path().join("artifacts");
        let executor = WorkbenchExecutor::new(directory.path().join("workspace"), &artifact_root);
        let (result, invocation_id, _) = completion_result();
        executor.persist_completion_receipt(&result).unwrap();
        let receipt_directory = executor
            .open_completion_receipt_directory(false)
            .unwrap()
            .unwrap();
        let name = OsString::from(format!("{invocation_id}.json"));
        let installed = receipt_directory.child_path(&name);
        let displaced = receipt_directory.child_path(OsStr::new("displaced.json"));
        let replacement = installed.clone();

        let error = read_validated_receipt(&receipt_directory, &name, || {
            fs::rename(&installed, &displaced).unwrap();
            fs::write(&replacement, b"{}").unwrap();
            fs::set_permissions(
                &replacement,
                fs::Permissions::from_mode(COMPLETION_RECEIPT_FILE_MODE),
            )
            .unwrap();
        })
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn receipt_reconcile_rejects_manipulated_temporary_cleanup_entry() {
        let directory = tempfile::tempdir().unwrap();
        let artifact_root = directory.path().join("artifacts");
        let executor = WorkbenchExecutor::new(directory.path().join("workspace"), &artifact_root);
        let (result, invocation_id, _) = completion_result();
        executor.persist_completion_receipt(&result).unwrap();
        let temporary = artifact_root
            .join(COMPLETION_RECEIPT_DIRECTORY)
            .join(format!(".{invocation_id}.deadbeef.tmp"));
        fs::write(&temporary, b"manipulated").unwrap();
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o644)).unwrap();

        assert!(executor.reconcile_completion_receipts().is_err());
        assert!(temporary.exists());
    }

    #[test]
    fn receipt_reconcile_finishes_crash_after_no_overwrite_install() {
        let directory = tempfile::tempdir().unwrap();
        let artifact_root = directory.path().join("artifacts");
        let executor = WorkbenchExecutor::new(directory.path().join("workspace"), &artifact_root);
        let (result, invocation_id, input_digest) = completion_result();
        let durable_result = seal_terminal_result(&result).unwrap().result;
        executor.persist_completion_receipt(&result).unwrap();
        let receipt_directory = artifact_root.join(COMPLETION_RECEIPT_DIRECTORY);
        let receipt = receipt_directory.join(format!("{invocation_id}.json"));
        let temporary = receipt_directory.join(format!(".{invocation_id}.deadbeef.tmp"));
        fs::hard_link(&receipt, &temporary).unwrap();

        executor.reconcile_completion_receipts().unwrap();
        assert!(!temporary.exists());
        assert_eq!(
            executor
                .recover_completion(&invocation_id, &input_digest)
                .unwrap(),
            durable_result
        );
    }
}
