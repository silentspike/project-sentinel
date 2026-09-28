//! Pre-Landlock namespace launcher. The controller retains policy authority;
//! this service accepts structured scopes, never caller-supplied bwrap flags.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use nix::fcntl::{fcntl, FcntlArg, FdFlag, OFlag};
use nix::sys::statfs::{statfs, CGROUP2_SUPER_MAGIC};
use sentinel_common::{WorkbenchResourceLimits, WorkbenchTool};
use serde::{Deserialize, Serialize};

use crate::command_sandbox::{self, canonical_directory, SetupChannel, GATE_MODE, READY};

pub const BROKER_MODE: &str = "--workbench-command-broker-v1";
pub const CLIENT_MODE: &str = "--workbench-command-broker-client-v1";
pub const SOCKET_ENV: &str = "SENTINEL_COMMAND_BROKER_SOCKET";
const MAX_DIRECTION_BYTES: usize = 512 * 1024;
const MAX_OUTPUT_BYTES: u64 = 65_536;
const MAX_WALL_MS: u64 = 300_000;
const CHUNK_BYTES: usize = 4096;
const TICK: Duration = Duration::from_millis(10);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandRequest {
    pub version: u16,
    pub program: String,
    pub args: Vec<String>,
    pub workspace: PathBuf,
    pub inputs: Vec<PathBuf>,
    pub file_bytes: u64,
    pub owned_cgroup: PathBuf,
    pub limits: WorkbenchResourceLimits,
}

impl CommandRequest {
    pub(crate) fn new(
        workspace: &Path,
        inputs: &[PathBuf],
        program: &str,
        limits: &WorkbenchResourceLimits,
        group: &Path,
    ) -> Self {
        Self {
            version: 1,
            program: program.into(),
            args: Vec::new(),
            workspace: workspace.into(),
            inputs: inputs.into(),
            file_bytes: limits.file_bytes,
            owned_cgroup: group.into(),
            limits: limits.clone(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QaRequest {
    pub version: u16,
    pub token: String,
    pub program: String,
    pub args: Vec<String>,
    pub workspace: PathBuf,
    pub scratch: PathBuf,
    pub input_bytes: Vec<u8>,
    pub wall_time_ms: u64,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Command(CommandRequest),
    Qa(QaRequest),
    Quiesce {
        version: u16,
        #[serde(rename = "ownedCgroup")]
        owned_cgroup: PathBuf,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    Ready {
        version: u16,
    },
    Stdout {
        version: u16,
        #[serde(rename = "dataHex")]
        data_hex: String,
    },
    Stderr {
        version: u16,
        #[serde(rename = "dataHex")]
        data_hex: String,
    },
    Exit {
        version: u16,
        code: Option<i32>,
        signal: Option<i32>,
    },
    Error {
        version: u16,
        code: String,
    },
    Quiesced {
        version: u16,
    },
}

fn denied() -> io::Error {
    io::Error::other("namespace broker isolation or protocol unavailable")
}

fn effective_uid() -> io::Result<u32> {
    let status = fs::read_to_string("/proc/self/status")?;
    let line = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .ok_or_else(denied)?;
    line.split_whitespace()
        .nth(1)
        .ok_or_else(denied)?
        .parse()
        .map_err(|_| denied())
}

pub fn valid_token(token: &str) -> bool {
    token.len() == 64
        && token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn token() -> io::Result<String> {
    let mut bytes = [0_u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(hex(&bytes))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 15) as usize] as char);
    }
    out
}

fn unhex(value: &str) -> io::Result<Vec<u8>> {
    if value.len() > CHUNK_BYTES * 2 || !value.len().is_multiple_of(2) {
        return Err(denied());
    }
    let digit = |b: u8| match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        _ => Err(denied()),
    };
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| Ok(digit(pair[0])? * 16 + digit(pair[1])?))
        .collect()
}

/// Each direction is at most 512 KiB, including frame headers. No recovery
/// after malformed/partial records; one request per connection, no fd passing.
struct Wire {
    stream: UnixStream,
    read_bytes: usize,
    write_bytes: usize,
}
impl Wire {
    fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            read_bytes: 0,
            write_bytes: 0,
        }
    }
    fn receive<T: serde::de::DeserializeOwned>(&mut self) -> io::Result<T> {
        let mut size = [0; 4];
        self.stream.read_exact(&mut size)?;
        let size = u32::from_be_bytes(size) as usize;
        self.read_bytes = self
            .read_bytes
            .checked_add(size.checked_add(4).ok_or_else(denied)?)
            .ok_or_else(denied)?;
        if size == 0 || self.read_bytes > MAX_DIRECTION_BYTES {
            return Err(denied());
        }
        let mut bytes = vec![0; size];
        self.stream.read_exact(&mut bytes)?;
        serde_json::from_slice(&bytes).map_err(|_| denied())
    }
    fn send<T: Serialize>(&mut self, value: &T) -> io::Result<()> {
        let bytes = serde_json::to_vec(value).map_err(|_| denied())?;
        self.write_bytes = self
            .write_bytes
            .checked_add(bytes.len() + 4)
            .ok_or_else(denied)?;
        if self.write_bytes > MAX_DIRECTION_BYTES {
            return Err(denied());
        }
        self.stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
        self.stream.write_all(&bytes)
    }
}

fn validate_socket_path(socket: &Path) -> io::Result<()> {
    let directory = socket.parent().ok_or_else(denied)?;
    if socket.file_name() != Some(OsStr::new("socket"))
        || directory.parent() != Some(Path::new("/run/sentinel-command-broker"))
    {
        return Err(denied());
    }
    let nonce = directory
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(denied)?;
    if nonce.len() != 36
        || !nonce.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
    {
        return Err(denied());
    }
    canonical_directory(directory)?;
    let metadata = fs::symlink_metadata(directory)?;
    if metadata.uid() != effective_uid()? || metadata.mode() & 0o7777 != 0o700 {
        return Err(denied());
    }
    Ok(())
}

fn validate_socket(socket: &Path) -> io::Result<()> {
    validate_socket_path(socket)?;
    let metadata = fs::symlink_metadata(socket)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != effective_uid()?
        || metadata.mode() & 0o7777 != 0o600
    {
        return Err(denied());
    }
    Ok(())
}

pub fn configured_socket() -> io::Result<PathBuf> {
    let socket = PathBuf::from(std::env::var_os(SOCKET_ENV).ok_or_else(denied)?);
    validate_socket(&socket)?;
    Ok(socket)
}

fn protect() -> io::Result<()> {
    nix::sys::prctl::set_dumpable(false).map_err(io::Error::from)?;
    if nix::sys::prctl::get_dumpable().map_err(io::Error::from)? {
        return Err(denied());
    }
    nix::sys::prctl::set_no_new_privs().map_err(io::Error::from)
}

fn shape(program: &str, args: &[String]) -> io::Result<()> {
    WorkbenchTool::RunCommand {
        program: program.into(),
        args: args.into(),
    }
    .validate_shape()
    .map_err(|_| denied())?;
    if args
        .iter()
        .any(|arg| arg.len() > 4096 || arg.contains('\0'))
    {
        return Err(denied());
    }
    command_sandbox::executable(OsStr::new(program))?;
    Ok(())
}

fn command_shape(request: &CommandRequest) -> io::Result<()> {
    // The controller already validated the model argv before replacing declared
    // input arguments with their absolute RO mount paths. Only exact declared
    // files may take this transport-only absolute form; all other grammar stays.
    let args = request
        .args
        .iter()
        .map(|arg| {
            let path = Path::new(arg);
            if path.is_absolute() && request.inputs.iter().any(|input| input == path) {
                path.strip_prefix("/")
                    .ok()
                    .and_then(Path::to_str)
                    .map(str::to_owned)
                    .ok_or_else(denied)
            } else {
                Ok(arg.clone())
            }
        })
        .collect::<io::Result<Vec<_>>>()?;
    shape(&request.program, &args)
}

fn qa_arguments(program: &str, args: &[String]) -> io::Result<()> {
    if !matches!(program, "python3" | "node")
        || args.len() > sentinel_common::WORKBENCH_MAX_COMMAND_ARGUMENTS
        || args
            .iter()
            .any(|arg| arg.len() > 4096 || arg.contains('\0'))
        || args.iter().map(String::len).sum::<usize>() > 128 * 1024
    {
        return Err(denied());
    }
    command_sandbox::executable(OsStr::new(program))?;
    Ok(())
}

fn limits_valid(limits: &WorkbenchResourceLimits) -> bool {
    limits.wall_time_ms > 0
        && limits.wall_time_ms <= MAX_WALL_MS
        && limits.cpu_time_ms > 0
        && limits.cpu_time_ms <= MAX_WALL_MS
        && limits.memory_bytes > 0
        && limits.process_count > 0
        && limits.file_bytes > 0
        && limits.stdout_bytes > 0
        && limits.stdout_bytes <= MAX_OUTPUT_BYTES
        && limits.stderr_bytes > 0
        && limits.stderr_bytes <= MAX_OUTPUT_BYTES
}

fn scoped_workspace(root: &Path, workspace: &Path, probe: bool) -> io::Result<()> {
    canonical_directory(workspace)?;
    if probe && root == workspace {
        return Ok(());
    }
    let relative = workspace.strip_prefix(root).map_err(|_| denied())?;
    if relative.components().count() != 2
        || relative.components().any(|part| {
            let name = part.as_os_str().to_str().unwrap_or("");
            name.is_empty()
                || name.len() > 128
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
        })
    {
        return Err(denied());
    }
    Ok(())
}

fn owned_group(root: &Path, group: &Path) -> io::Result<()> {
    canonical_directory(group)?;
    let name = group
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(denied)?;
    if group.parent() != Some(root)
        || !name.starts_with("command-")
        || !name[8..].bytes().all(|b| b.is_ascii_hexdigit())
        || name.len() != 40
        || statfs(group).map_err(io::Error::from)?.filesystem_type() != CGROUP2_SUPER_MAGIC
        || fs::metadata(group)?.uid() != effective_uid()?
    {
        return Err(denied());
    }
    for file in [
        "cgroup.kill",
        "cgroup.procs",
        "cgroup.events",
        "cpu.stat",
        "memory.max",
        "pids.max",
    ] {
        let metadata = fs::symlink_metadata(group.join(file))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(denied());
        }
    }
    Ok(())
}

struct QaLease {
    workspace: PathBuf,
    group: PathBuf,
    limits: WorkbenchResourceLimits,
    deadline: Instant,
    busy: AtomicBool,
    live: AtomicBool,
}
type Registry = Arc<Mutex<BTreeMap<String, Arc<QaLease>>>>;

struct Job {
    cancelled: AtomicBool,
    finished: AtomicBool,
    lease: Mutex<Option<Arc<QaLease>>>,
}
impl Job {
    fn new(closed: bool) -> Self {
        Self {
            cancelled: AtomicBool::new(closed),
            finished: AtomicBool::new(closed),
            lease: Mutex::new(None),
        }
    }
    fn cancel(&self) -> io::Result<()> {
        self.cancelled.store(true, Ordering::Release);
        if let Some(lease) = self.lease.lock().map_err(|_| denied())?.as_ref() {
            lease.live.store(false, Ordering::Release);
        }
        Ok(())
    }
    fn settled(&self) -> io::Result<bool> {
        Ok(self.finished.load(Ordering::Acquire)
            && !self
                .lease
                .lock()
                .map_err(|_| denied())?
                .as_ref()
                .is_some_and(|lease| lease.busy.load(Ordering::Acquire)))
    }
}
struct JobGuard(Arc<Job>);
impl Drop for JobGuard {
    fn drop(&mut self) {
        self.0.finished.store(true, Ordering::Release);
    }
}

struct Config {
    socket: PathBuf,
    workspace: PathBuf,
    inputs: PathBuf,
    cgroups: PathBuf,
    runner: PathBuf,
    registry: Registry,
    regular_busy: AtomicBool,
    qa_busy: AtomicBool,
    running: AtomicBool,
    jobs: Mutex<BTreeMap<PathBuf, Arc<Job>>>,
}

/// Called inside the outer bwrap before controller Landlock. The wrapper must
/// attach itself to the runtime cgroup before spawning us, not after spawn.
pub fn run_broker(mut args: impl Iterator<Item = OsString>) -> io::Result<()> {
    protect()?;
    let socket = PathBuf::from(args.next().ok_or_else(denied)?);
    validate_socket_path(&socket)?;
    if socket.exists() {
        return Err(denied());
    }
    let workspace = canonical_directory(Path::new(&args.next().ok_or_else(denied)?))?;
    let inputs = canonical_directory(Path::new(&args.next().ok_or_else(denied)?))?;
    let cgroups = canonical_directory(Path::new(&args.next().ok_or_else(denied)?))?;
    let socket_directory = socket.parent().ok_or_else(denied)?;
    if statfs(&cgroups).map_err(io::Error::from)?.filesystem_type() != CGROUP2_SUPER_MAGIC
        || fs::metadata(&cgroups)?.uid() != effective_uid()?
        || cgroups.starts_with(&workspace)
        || workspace.starts_with(&cgroups)
        || inputs == workspace
        || workspace.starts_with(&inputs)
        || [workspace.as_path(), inputs.as_path(), cgroups.as_path()]
            .iter()
            .any(|root| socket_directory.starts_with(root) || root.starts_with(socket_directory))
    {
        return Err(denied());
    }
    let pid: u32 = args
        .next()
        .and_then(|p| p.to_str().and_then(|s| s.parse().ok()))
        .filter(|p| *p > 0)
        .ok_or_else(denied)?;
    if args.next().is_some() {
        return Err(denied());
    }
    // A pinned proc file, never kill(pid), is the controller lifetime anchor.
    let controller = File::open(format!("/proc/{pid}/stat"))?;
    let identity = controller_identity(&controller)?;
    let runner = command_sandbox::executable(OsStr::new("agent-runtime"))?;
    if fs::canonicalize(std::env::current_exe()?)? != runner {
        return Err(denied());
    }
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, Permissions::from_mode(0o600))?;
    validate_socket(&socket)?;
    listener.set_nonblocking(true)?;
    let config = Arc::new(Config {
        socket: socket.clone(),
        workspace,
        inputs,
        cgroups,
        runner,
        registry: Arc::new(Mutex::new(BTreeMap::new())),
        regular_busy: AtomicBool::new(false),
        qa_busy: AtomicBool::new(false),
        running: AtomicBool::new(true),
        jobs: Mutex::new(BTreeMap::new()),
    });
    let mut handlers = Vec::new();
    while controller_identity(&controller).ok().as_ref() == Some(&identity) {
        handlers.retain(|handle: &thread::JoinHandle<()>| !handle.is_finished());
        match listener.accept() {
            Ok((stream, _)) => {
                // At most two active services and two bounded parsing slots.
                if handlers.len() >= 4 {
                    drop(stream);
                    continue;
                }
                let config = config.clone();
                handlers.push(thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let _ = stream.set_write_timeout(Some(Duration::from_millis(100)));
                    let mut wire = Wire::new(stream);
                    if serve(&config, &mut wire).is_err() {
                        let _ = wire.send(&Event::Error {
                            version: 1,
                            code: "broker_denied".into(),
                        });
                    }
                }));
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::sleep(TICK),
            Err(error) => {
                config.running.store(false, Ordering::Release);
                for handle in handlers {
                    let _ = handle.join();
                }
                return Err(error);
            }
        }
    }
    config.running.store(false, Ordering::Release);
    for handle in handlers {
        let _ = handle.join();
    }
    fs::remove_file(socket)?;
    Ok(())
}

fn controller_identity(file: &File) -> io::Result<String> {
    use std::os::unix::fs::FileExt;
    let mut buffer = [0; 4096];
    let count = file.read_at(&mut buffer, 0)?;
    let value = std::str::from_utf8(&buffer[..count]).map_err(|_| denied())?;
    let rest = value.rsplit_once(") ").ok_or_else(denied)?.1;
    let fields: Vec<_> = rest.split_whitespace().collect();
    if fields.len() < 20 || matches!(fields[0], "Z" | "X" | "x") {
        return Err(denied());
    }
    Ok(fields[19].to_owned())
}

struct Busy<'a>(&'a AtomicBool);
impl<'a> Busy<'a> {
    fn acquire(flag: &'a AtomicBool) -> io::Result<Self> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| denied())?;
        Ok(Self(flag))
    }
}
impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

struct Registration {
    registry: Registry,
    token: String,
    lease: Arc<QaLease>,
}
impl Drop for Registration {
    fn drop(&mut self) {
        self.lease.live.store(false, Ordering::Release);
        if let Ok(mut registry) = self.registry.lock() {
            registry.remove(&self.token);
        }
    }
}

fn validate_command(config: &Config, request: &CommandRequest) -> io::Result<()> {
    if request.version != 1
        || request.file_bytes != request.limits.file_bytes
        || !limits_valid(&request.limits)
        || request.inputs.len() > 64
    {
        return Err(denied());
    }
    command_shape(request)?;
    scoped_workspace(
        &config.workspace,
        &request.workspace,
        request.program == "true" && request.args.is_empty() && request.inputs.is_empty(),
    )?;
    if request.workspace.starts_with(&config.inputs) {
        return Err(denied());
    }
    command_sandbox::validate_input_paths(&request.workspace, &request.inputs)?;
    let relative_workspace = request
        .workspace
        .strip_prefix(&config.workspace)
        .map_err(|_| denied())?;
    let input_scope = config.inputs.join(relative_workspace);
    for input in &request.inputs {
        if !input.starts_with(&input_scope)
            || input == &input_scope
            || fs::metadata(input)?.nlink() != 1
        {
            return Err(denied());
        }
    }
    owned_group(&config.cgroups, &request.owned_cgroup)?;
    for (file, expected) in [
        ("memory.max", request.limits.memory_bytes),
        ("pids.max", u64::from(request.limits.process_count)),
        ("memory.swap.max", 0),
        ("memory.oom.group", 1),
    ] {
        if fs::read_to_string(request.owned_cgroup.join(file))?.trim() != expected.to_string() {
            return Err(denied());
        }
    }
    Ok(())
}

fn qa_paths(primary: &Path, candidate: &Path, scratch: &Path) -> io::Result<()> {
    canonical_directory(candidate)?;
    canonical_directory(scratch)?;
    let parent = candidate.parent().ok_or_else(denied)?;
    let name = parent
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(denied)?;
    if parent.parent() != Some(primary)
        || !name.starts_with(".coding-qa-")
        || name.len() <= ".coding-qa-".len()
        || candidate.file_name() != Some(OsStr::new("candidate"))
        || scratch != parent.join("scratch")
    {
        return Err(denied());
    }
    reject_links(candidate, &mut 0, &mut 0)?;
    // Scratch is trusted-created and may contain leftovers, but no authority
    // crossing hardlinks/symlinks; both trees are bounded before bind setup.
    reject_links(scratch, &mut 0, &mut 0)
}

fn reject_links(root: &Path, count: &mut usize, depth: &mut usize) -> io::Result<()> {
    if *depth >= 64 {
        return Err(denied());
    }
    *depth += 1;
    for entry in fs::read_dir(root)? {
        *count += 1;
        if *count > 4096 {
            return Err(denied());
        }
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.is_dir() {
            reject_links(&entry.path(), count, depth)?;
        } else if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(denied());
        }
    }
    *depth -= 1;
    Ok(())
}

fn serve(config: &Config, wire: &mut Wire) -> io::Result<()> {
    match wire.receive::<Request>()? {
        Request::Command(request) => {
            let _busy = Busy::acquire(&config.regular_busy)?;
            validate_command(config, &request)?;
            let job = claim_job(config, &request.owned_cgroup, false)?;
            let owned_job = JobGuard(job.clone());
            let deadline = Instant::now() + Duration::from_millis(request.limits.wall_time_ms);
            let registration = if request.program == "sentinel-coding-qa" {
                let token = token()?;
                let lease = Arc::new(QaLease {
                    workspace: request.workspace.clone(),
                    group: request.owned_cgroup.clone(),
                    limits: request.limits.clone(),
                    deadline,
                    busy: AtomicBool::new(false),
                    live: AtomicBool::new(true),
                });
                config
                    .registry
                    .lock()
                    .map_err(|_| denied())?
                    .insert(token.clone(), lease.clone());
                *job.lease.lock().map_err(|_| denied())? = Some(lease.clone());
                if job.cancelled.load(Ordering::Acquire) {
                    lease.live.store(false, Ordering::Release);
                }
                Some(Registration {
                    registry: config.registry.clone(),
                    token,
                    lease,
                })
            } else {
                None
            };
            let evaluator = registration
                .as_ref()
                .map(|r| (config.socket.as_path(), r.token.as_str()));
            let mut wrapped = command_sandbox::broker_bwrap_command(
                &config.runner,
                &request.workspace,
                &request.inputs,
                &request.program,
                request.file_bytes,
                evaluator,
                None,
            )?;
            wrapped.args(&request.args);
            let result = execute(
                config,
                wire,
                wrapped,
                &request.owned_cgroup,
                &request.owned_cgroup,
                &request.limits,
                deadline,
                registration.as_ref().map(|r| r.lease.as_ref()),
                true,
                Some(&job),
            );
            drop(registration);
            drop(owned_job);
            let status = result?;
            settle_job(&job, &request.owned_cgroup)?;
            emit_exit(wire, status)
        }
        Request::Qa(request) => {
            let _slot = Busy::acquire(&config.qa_busy)?;
            if request.version != 1
                || !valid_token(&request.token)
                || request.input_bytes.len() > 65_536
                || !matches!(request.program.as_str(), "node" | "python3")
            {
                return Err(denied());
            }
            let lease = config
                .registry
                .lock()
                .map_err(|_| denied())?
                .get(&request.token)
                .cloned()
                .ok_or_else(denied)?;
            let _busy = Busy::acquire(&lease.busy)?;
            if !lease.live.load(Ordering::Acquire)
                || Instant::now() >= lease.deadline
                || request.wall_time_ms == 0
                || request.wall_time_ms > lease.limits.wall_time_ms
                || request.stdout_bytes == 0
                || request.stdout_bytes > lease.limits.stdout_bytes
                || request.stderr_bytes == 0
                || request.stderr_bytes > lease.limits.stderr_bytes
            {
                return Err(denied());
            }
            qa_arguments(&request.program, &request.args)?;
            qa_paths(&lease.workspace, &request.workspace, &request.scratch)?;
            owned_group(&config.cgroups, &lease.group)?;
            let limits = WorkbenchResourceLimits {
                wall_time_ms: request.wall_time_ms,
                stdout_bytes: request.stdout_bytes,
                stderr_bytes: request.stderr_bytes,
                ..lease.limits.clone()
            };
            let mut input = InputFile::create(&request.scratch, &request.input_bytes)?;
            let mut wrapped = command_sandbox::broker_bwrap_command(
                &config.runner,
                &request.workspace,
                &[],
                &request.program,
                limits.file_bytes,
                None,
                Some((&request.scratch, &input.path)),
            )?;
            wrapped.args(&request.args);
            let subgroup = lease.group.join(format!("qa-{}", token()?));
            fs::create_dir(&subgroup)?;
            let mut group = GroupGuard {
                path: subgroup,
                remove: true,
            };
            input.quiesced = false;
            let result = execute(
                config,
                wire,
                wrapped,
                &group.path,
                &lease.group,
                &limits,
                lease
                    .deadline
                    .min(Instant::now() + Duration::from_millis(request.wall_time_ms)),
                Some(&lease),
                false,
                None,
            );
            group.cleanup()?;
            input.quiesced = true;
            input.cleanup()?;
            emit_exit(wire, result?)
        }
        Request::Quiesce {
            version: 1,
            owned_cgroup,
        } => {
            owned_group(&config.cgroups, &owned_cgroup)?;
            let job = claim_job(config, &owned_cgroup, true)?;
            job.cancel()?;
            settle_job(&job, &owned_cgroup)?;
            wire.send(&Event::Quiesced { version: 1 })
        }
        Request::Quiesce { .. } => Err(denied()),
    }
}

fn settle_job(job: &Job, group: &Path) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !job.settled()? {
        if Instant::now() >= deadline {
            return Err(denied());
        }
        thread::sleep(TICK);
    }
    GroupGuard {
        path: group.into(),
        remove: false,
    }
    .cleanup()
}

fn claim_job(config: &Config, group: &Path, close: bool) -> io::Result<Arc<Job>> {
    let mut jobs = config.jobs.lock().map_err(|_| denied())?;
    // Closed leases live until the controller removes its random-owned group.
    // This prevents a cancelled request from arriving late and reopening it.
    jobs.retain(|path, _| path.exists());
    if let Some(job) = jobs.get(group) {
        if close {
            return Ok(job.clone());
        }
        return Err(denied());
    }
    if jobs.len() >= 256 {
        return Err(denied());
    }
    let job = Arc::new(Job::new(close));
    jobs.insert(group.into(), job.clone());
    Ok(job)
}

/// Controller cancellation barrier. Call after stopping/reaping the facade,
/// before treating command cgroup emptiness as proof or issuing a receipt.
pub fn quiesce_command(socket: &Path, owned_cgroup: &Path) -> io::Result<()> {
    validate_socket(socket)?;
    let stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut wire = Wire::new(stream);
    wire.send(&Request::Quiesce {
        version: 1,
        owned_cgroup: owned_cgroup.into(),
    })?;
    match wire.receive::<Event>()? {
        Event::Quiesced { version: 1 } => Ok(()),
        _ => Err(denied()),
    }
}

fn emit_exit(wire: &mut Wire, status: std::process::ExitStatus) -> io::Result<()> {
    wire.send(&Event::Exit {
        version: 1,
        code: status.code(),
        signal: status.signal(),
    })
}

struct InputFile {
    path: PathBuf,
    identity: (u64, u64),
    removed: bool,
    quiesced: bool,
}
impl InputFile {
    fn create(scratch: &Path, bytes: &[u8]) -> io::Result<Self> {
        let path = scratch.join(format!(".broker-stdin-{}", token()?));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(OFlag::O_NOFOLLOW.bits())
            .open(&path)?;
        let metadata = file.metadata()?;
        let owned = Self {
            path,
            identity: (metadata.dev(), metadata.ino()),
            removed: false,
            quiesced: true,
        };
        file.write_all(bytes)?;
        Ok(owned)
    }
    fn cleanup(&mut self) -> io::Result<()> {
        if self.removed {
            return Ok(());
        }
        let metadata = fs::symlink_metadata(&self.path)?;
        if (metadata.dev(), metadata.ino()) != self.identity
            || !metadata.is_file()
            || metadata.nlink() != 1
        {
            return Err(denied());
        }
        fs::remove_file(&self.path)?;
        self.removed = true;
        Ok(())
    }
}
impl Drop for InputFile {
    fn drop(&mut self) {
        if self.quiesced {
            let _ = self.cleanup();
        }
    }
}

struct GroupGuard {
    path: PathBuf,
    remove: bool,
}
impl GroupGuard {
    fn cleanup(&mut self) -> io::Result<()> {
        fs::write(self.path.join("cgroup.kill"), b"1")?;
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let events = fs::read_to_string(self.path.join("cgroup.events"))?;
            let populated: Vec<_> = events
                .lines()
                .filter_map(|line| line.strip_prefix("populated "))
                .collect();
            if populated.as_slice() == ["0"] {
                break;
            }
            if populated.as_slice() != ["1"] || Instant::now() >= deadline {
                return Err(denied());
            }
            thread::sleep(TICK);
        }
        if self.remove {
            fs::remove_dir(&self.path)?;
            self.remove = false;
        }
        Ok(())
    }
}
impl Drop for GroupGuard {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        // Child retains the unreaped process identity; never kill a saved PID.
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

enum Output {
    Chunk(bool, Vec<u8>),
    End(bool),
    Error,
}
fn reader(
    mut source: impl Read + Send + 'static,
    stderr: bool,
    sender: mpsc::SyncSender<Output>,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut bytes = [0; CHUNK_BYTES];
        loop {
            if stop.load(Ordering::Acquire) {
                return;
            }
            let output = match source.read(&mut bytes) {
                Ok(0) => Output::End(stderr),
                Ok(count) => Output::Chunk(stderr, bytes[..count].into()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(TICK);
                    continue;
                }
                Err(_) => Output::Error,
            };
            let terminal = !matches!(output, Output::Chunk(..));
            if sender.send(output).is_err() || terminal {
                return;
            }
        }
    })
}

fn cpu_time_ms(group: &Path) -> io::Result<u64> {
    let stat = fs::read_to_string(group.join("cpu.stat"))?;
    let values: Vec<_> = stat
        .lines()
        .filter_map(|line| line.strip_prefix("usage_usec "))
        .collect();
    if values.len() != 1 {
        return Err(denied());
    }
    Ok(values[0]
        .parse::<u64>()
        .map_err(|_| denied())?
        .div_ceil(1000))
}

fn execute(
    config: &Config,
    wire: &mut Wire,
    wrapped: Command,
    group: &Path,
    accounting: &Path,
    limits: &WorkbenchResourceLimits,
    deadline: Instant,
    lease: Option<&QaLease>,
    primary: bool,
    job: Option<&Job>,
) -> io::Result<std::process::ExitStatus> {
    let mut ownership = GroupGuard {
        path: group.into(),
        remove: false,
    };
    let (mut setup, status) = SetupChannel::pair()?;
    let mut command = Command::new(&config.runner);
    command
        .arg(GATE_MODE)
        .arg(group)
        .arg("--")
        .arg(wrapped.get_program())
        .args(wrapped.get_args())
        .env_clear()
        .stdin(Stdio::from(status))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = ChildGuard(command.spawn()?);
    drop(command);
    let stdout = child.0.stdout.take().ok_or_else(denied)?;
    let stderr = child.0.stderr.take().ok_or_else(denied)?;
    fcntl(&stdout, FcntlArg::F_SETFL(OFlag::O_NONBLOCK)).map_err(io::Error::from)?;
    fcntl(&stderr, FcntlArg::F_SETFL(OFlag::O_NONBLOCK)).map_err(io::Error::from)?;
    let stop = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::sync_channel(4);
    let readers = [
        reader(stdout, false, sender.clone(), stop.clone()),
        reader(stderr, true, sender, stop.clone()),
    ];
    let result = (|| {
        let mut ready = false;
        let mut eof = [false; 2];
        let mut totals = [0_u64; 2];
        let mut pending = Vec::new();
        let mut status = None;
        fcntl(&wire.stream, FcntlArg::F_SETFL(OFlag::O_NONBLOCK)).map_err(io::Error::from)?;
        loop {
            if !config.running.load(Ordering::Acquire)
                || job.is_some_and(|job| job.cancelled.load(Ordering::Acquire))
                || Instant::now() >= deadline
                || (lease.is_some_and(|lease| !lease.live.load(Ordering::Acquire))
                    && (!primary || status.is_none()))
                || cpu_time_ms(accounting)? > limits.cpu_time_ms
            {
                return Err(denied());
            }
            let mut extra = [0; 1];
            match wire.stream.read(&mut extra) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                _ => return Err(denied()), // EOF or a second request cancels.
            }
            if !ready && setup.poll()? {
                ready = true;
                wire.send(&Event::Ready { version: 1 })?;
                for event in pending.drain(..) {
                    wire.send(&event)?;
                }
            }
            match receiver.recv_timeout(TICK) {
                Ok(Output::Chunk(stderr, bytes)) => {
                    let index = usize::from(stderr);
                    totals[index] = totals[index]
                        .checked_add(bytes.len() as u64)
                        .ok_or_else(denied)?;
                    if totals[index]
                        > if stderr {
                            limits.stderr_bytes
                        } else {
                            limits.stdout_bytes
                        }
                    {
                        return Err(denied());
                    }
                    let event = if stderr {
                        Event::Stderr {
                            version: 1,
                            data_hex: hex(&bytes),
                        }
                    } else {
                        Event::Stdout {
                            version: 1,
                            data_hex: hex(&bytes),
                        }
                    };
                    if ready {
                        wire.send(&event)?;
                    } else {
                        pending.push(event);
                    }
                }
                Ok(Output::End(stderr)) => eof[usize::from(stderr)] = true,
                Ok(Output::Error) => return Err(denied()),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) if eof == [true, true] => {}
                Err(_) => return Err(denied()),
            }
            if status.is_none() {
                status = child.0.try_wait()?;
            }
            // Quiesce descendants immediately when the owned initial child
            // exits, even if descendants kept stdout/stderr open.
            if status.is_some() {
                if primary {
                    if let Some(lease) = lease {
                        lease.live.store(false, Ordering::Release);
                    }
                }
                ownership.cleanup()?;
            }
            if let Some(status) = status.filter(|_| eof == [true, true]) {
                if !ready && !setup.poll()? {
                    return Err(denied());
                }
                if !ready {
                    wire.send(&Event::Ready { version: 1 })?;
                    for event in pending.drain(..) {
                        wire.send(&event)?;
                    }
                }
                return Ok(status);
            }
        }
    })();
    // Revoke before cleanup, so a waiting evaluator cannot register another job.
    if result.is_err() {
        if let Some(lease) = lease {
            lease.live.store(false, Ordering::Release);
        }
    }
    stop.store(true, Ordering::Release);
    drop(child);
    let cleanup = ownership.cleanup();
    drop(receiver);
    for reader in readers {
        let _ = reader.join();
    }
    cleanup?;
    result
}

/// Child-compatible facade: fd0 is the parent's setup socket, not candidate
/// stdin. READY is relayed only after the broker's actual exec proof.
pub fn run_client(mut args: impl Iterator<Item = OsString>) -> io::Result<()> {
    protect()?;
    let socket = PathBuf::from(args.next().ok_or_else(denied)?);
    validate_socket(&socket)?;
    let metadata = args.next().ok_or_else(denied)?;
    if metadata.len() > MAX_DIRECTION_BYTES / 2 {
        return Err(denied());
    }
    let mut request: CommandRequest =
        serde_json::from_slice(metadata.as_encoded_bytes()).map_err(|_| denied())?;
    if !request.args.is_empty() || args.next().as_deref() != Some(OsStr::new("--")) {
        return Err(denied());
    }
    for arg in args.take(sentinel_common::WORKBENCH_MAX_COMMAND_ARGUMENTS + 1) {
        request.args.push(arg.into_string().map_err(|_| denied())?);
    }
    command_shape(&request)?;
    if !limits_valid(&request.limits) {
        return Err(denied());
    }
    let descriptor = nix::unistd::dup(io::stdin()).map_err(io::Error::from)?;
    fcntl(&descriptor, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).map_err(io::Error::from)?;
    let mut status = File::from(descriptor);
    let mut ready = false;
    nix::unistd::dup2_stdin(File::open("/dev/null")?).map_err(io::Error::from)?;
    let stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_millis(
        request.limits.wall_time_ms + 2000,
    )))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut wire = Wire::new(stream);
    let output_limits = [request.limits.stdout_bytes, request.limits.stderr_bytes];
    wire.send(&Request::Command(request))?;
    let mut totals = [0_u64; 2];
    loop {
        match wire.receive::<Event>()? {
            Event::Ready { version: 1 } if !ready => {
                status.write_all(READY)?;
                ready = true;
            }
            Event::Stdout {
                version: 1,
                data_hex,
            } if ready => {
                write_output(
                    &data_hex,
                    &mut totals[0],
                    output_limits[0],
                    &mut io::stdout().lock(),
                )?;
            }
            Event::Stderr {
                version: 1,
                data_hex,
            } if ready => {
                write_output(
                    &data_hex,
                    &mut totals[1],
                    output_limits[1],
                    &mut io::stderr().lock(),
                )?;
            }
            Event::Exit {
                version: 1,
                code,
                signal,
            } if ready => {
                status.write_all(&terminal_wait_status(code, signal)?.to_be_bytes())?;
                return Ok(());
            }
            _ => return Err(denied()),
        }
    }
}

fn write_output(
    data: &str,
    total: &mut u64,
    limit: u64,
    target: &mut impl Write,
) -> io::Result<()> {
    let bytes = unhex(data)?;
    *total = total.checked_add(bytes.len() as u64).ok_or_else(denied)?;
    if *total > limit {
        return Err(denied());
    }
    target.write_all(&bytes)
}

fn terminal_wait_status(code: Option<i32>, signal: Option<i32>) -> io::Result<i32> {
    // The facade is not the candidate. Carry its broker-observed status over
    // the private setup FD instead of fabricating an equivalent self-signal.
    match (code, signal) {
        (Some(code), None) if (0..=255).contains(&code) => Ok(code << 8),
        (None, Some(signal)) if (1..=64).contains(&signal) => Ok(signal),
        _ => Err(denied()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn limits() -> WorkbenchResourceLimits {
        WorkbenchResourceLimits {
            wall_time_ms: 2000,
            cpu_time_ms: 2000,
            memory_bytes: 128 * 1024 * 1024,
            process_count: 16,
            file_bytes: 1024,
            stdout_bytes: 65_536,
            stderr_bytes: 65_536,
        }
    }

    fn config(root: &Path) -> Config {
        Config {
            socket: root.join("socket"),
            workspace: root.join("workspace"),
            inputs: root.join("inputs"),
            cgroups: root.join("groups"),
            runner: root.join("runner"),
            registry: Arc::new(Mutex::new(BTreeMap::new())),
            regular_busy: AtomicBool::new(false),
            qa_busy: AtomicBool::new(false),
            running: AtomicBool::new(true),
            jobs: Mutex::new(BTreeMap::new()),
        }
    }

    #[test]
    fn wire_rejects_eof_malformed_oversized_and_partial_frames() {
        for bytes in [
            Vec::new(),
            vec![0, 0, 0, 0],
            vec![0, 0, 0, 1, b'x'],
            vec![0, 0, 0, 4, b'{'],
            ((MAX_DIRECTION_BYTES + 1) as u32).to_be_bytes().to_vec(),
        ] {
            let (mut sender, receiver) = UnixStream::pair().unwrap();
            sender.write_all(&bytes).unwrap();
            drop(sender);
            assert!(Wire::new(receiver).receive::<Request>().is_err());
        }
    }

    #[test]
    fn protocol_is_typed_versioned_and_rejects_raw_flags_or_unknown_fields() {
        let request = json!({"version":1,"kind":"qa","token":"a".repeat(64),"program":"python3",
            "args":["-I","-c","print('x')"],"workspace":"/workspace/p/w/.coding-qa-x/candidate",
            "scratch":"/workspace/p/w/.coding-qa-x/scratch","inputBytes":[0,255],
            "wallTimeMs":1000,"stdoutBytes":1024,"stderrBytes":1024});
        let decoded: Request = serde_json::from_value(request.clone()).unwrap();
        assert!(
            matches!(decoded, Request::Qa(QaRequest { version: 1, input_bytes, .. }) if input_bytes == [0,255])
        );
        let mut invalid = request.clone();
        invalid["bwrapFlags"] = json!(["--bind", "/", "/"]);
        assert!(serde_json::from_value::<Request>(invalid).is_err());
        let mut invalid = request;
        invalid["inputBytes"] = json!([256]);
        assert!(serde_json::from_value::<Request>(invalid).is_err());
        let event = serde_json::to_value(Event::Stdout {
            version: 1,
            data_hex: "00ff".into(),
        })
        .unwrap();
        assert_eq!(event, json!({"kind":"stdout","version":1,"dataHex":"00ff"}));
    }

    #[test]
    fn output_and_connection_budgets_are_enforced_before_allocation_or_write() {
        assert_eq!(unhex("00ff").unwrap(), [0, 255]);
        assert!(unhex("FF").is_err());
        assert!(unhex("0").is_err());
        assert!(unhex(&"aa".repeat(CHUNK_BYTES + 1)).is_err());
        let mut total = 0;
        assert!(write_output("00ff", &mut total, 1, &mut Vec::new()).is_err());
        let (sender, _receiver) = UnixStream::pair().unwrap();
        let mut wire = Wire::new(sender);
        wire.write_bytes = MAX_DIRECTION_BYTES;
        assert!(wire.send(&Event::Ready { version: 1 }).is_err());
        let mut invalid = limits();
        invalid.stdout_bytes = MAX_OUTPUT_BYTES + 1;
        assert!(!limits_valid(&invalid));
        invalid = limits();
        invalid.wall_time_ms = MAX_WALL_MS + 1;
        assert!(!limits_valid(&invalid));
    }

    #[test]
    fn trusted_qa_argv_accepts_code_while_regular_grammar_and_declared_inputs_stay_scoped() {
        let args = vec![
            "-E".into(),
            "-s".into(),
            "-c".into(),
            "print('hello world')\nprint('next')".into(),
        ];
        assert!(qa_arguments("python3", &args).is_ok());
        assert!(shape("python3", &args).is_err());
        assert!(qa_arguments("sh", &args).is_err());
        assert!(qa_arguments("python3", &["bad\0arg".into()]).is_err());
        assert!(qa_arguments("python3", &["x".repeat(4097)]).is_err());
        assert!(qa_arguments(
            "python3",
            &vec!["x".into(); sentinel_common::WORKBENCH_MAX_COMMAND_ARGUMENTS + 1]
        )
        .is_err());
        let input = PathBuf::from("/inputs/project/work/.inputs/source/file.py");
        let mut request = CommandRequest::new(
            Path::new("/workspace/project/work"),
            std::slice::from_ref(&input),
            "true",
            &limits(),
            Path::new("/groups/command"),
        );
        request.args = vec![input.to_string_lossy().into_owned()];
        assert!(command_shape(&request).is_ok());
        request.args = vec!["/inputs/foreign/work/file.py".into()];
        assert!(command_shape(&request).is_err());
        request.args = vec!["../foreign".into()];
        assert!(command_shape(&request).is_err());
    }

    #[test]
    fn scoped_workspace_and_readonly_qa_reject_foreign_or_linked_trees() {
        let directory = tempfile::tempdir().unwrap();
        let primary = directory.path().join("project/work");
        let candidate = primary.join(".coding-qa-test/candidate");
        let scratch = primary.join(".coding-qa-test/scratch");
        fs::create_dir_all(&candidate).unwrap();
        fs::create_dir(&scratch).unwrap();
        assert!(scoped_workspace(directory.path(), &primary, false).is_ok());
        assert!(scoped_workspace(directory.path(), directory.path(), false).is_err());
        assert!(scoped_workspace(directory.path(), directory.path(), true).is_ok());
        assert!(scoped_workspace(directory.path(), &candidate, false).is_err());
        assert!(qa_paths(&primary, &candidate, &scratch).is_ok());
        assert!(qa_paths(&directory.path().join("other"), &candidate, &scratch).is_err());
        fs::write(candidate.join("file"), b"x").unwrap();
        fs::hard_link(candidate.join("file"), scratch.join("alias")).unwrap();
        assert!(qa_paths(&primary, &candidate, &scratch).is_err());
        fs::remove_file(scratch.join("alias")).unwrap();
        std::os::unix::fs::symlink(candidate.join("file"), candidate.join("link")).unwrap();
        assert!(qa_paths(&primary, &candidate, &scratch).is_err());
    }

    #[test]
    fn foreign_inputs_invalid_versions_and_missing_qa_leases_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let config = config(directory.path());
        let workspace = config.workspace.join("project/work");
        let foreign = config.inputs.join("foreign/work/file");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(foreign.parent().unwrap()).unwrap();
        fs::write(&foreign, b"x").unwrap();
        let mut request = CommandRequest::new(
            &workspace,
            &[foreign],
            "true",
            &limits(),
            &config.cgroups.join("command-bad"),
        );
        assert!(validate_command(&config, &request).is_err());
        request.version = 2;
        assert!(validate_command(&config, &request).is_err());
        let (client, server) = UnixStream::pair().unwrap();
        let mut client = Wire::new(client);
        let mut server = Wire::new(server);
        client
            .send(&Request::Qa(QaRequest {
                version: 1,
                token: "a".repeat(64),
                program: "python3".into(),
                args: Vec::new(),
                workspace,
                scratch: config.workspace.join("scratch"),
                input_bytes: Vec::new(),
                wall_time_ms: 1000,
                stdout_bytes: 1024,
                stderr_bytes: 1024,
            }))
            .unwrap();
        assert!(serve(&config, &mut server).is_err());
        assert!(!config.qa_busy.load(Ordering::Acquire));
    }

    #[test]
    fn bounded_slots_allow_one_primary_and_one_qa_without_token_reuse() {
        let regular = AtomicBool::new(false);
        let qa = AtomicBool::new(false);
        let _primary = Busy::acquire(&regular).unwrap();
        assert!(Busy::acquire(&regular).is_err());
        let nested = Busy::acquire(&qa).unwrap();
        assert!(Busy::acquire(&qa).is_err());
        drop(nested);
        assert!(Busy::acquire(&qa).is_ok());
        assert!(valid_token(&"a".repeat(64)));
        assert!(!valid_token(&"A".repeat(64)));
        assert!(!valid_token("../socket"));
        let registry = Arc::new(Mutex::new(BTreeMap::new()));
        let lease = Arc::new(QaLease {
            workspace: "/workspace/p/w".into(),
            group: "/groups/command".into(),
            limits: limits(),
            deadline: Instant::now() + Duration::from_secs(1),
            busy: AtomicBool::new(false),
            live: AtomicBool::new(true),
        });
        registry
            .lock()
            .unwrap()
            .insert("a".repeat(64), lease.clone());
        drop(Registration {
            registry: registry.clone(),
            token: "a".repeat(64),
            lease: lease.clone(),
        });
        assert!(!lease.live.load(Ordering::Acquire));
        assert!(registry.lock().unwrap().is_empty());
    }

    #[test]
    fn cancellation_tombstone_prevents_late_launch_and_waits_for_qa_handler() {
        let directory = tempfile::tempdir().unwrap();
        let config = config(directory.path());
        let group = directory.path().join("group");
        fs::create_dir(&group).unwrap();
        let closed = claim_job(&config, &group, true).unwrap();
        assert!(closed.settled().unwrap());
        assert!(claim_job(&config, &group, false).is_err());
        fs::remove_dir(&group).unwrap();
        let live_group = directory.path().join("live");
        fs::create_dir(&live_group).unwrap();
        let job = claim_job(&config, &live_group, false).unwrap();
        let owner = JobGuard(job.clone());
        let lease = Arc::new(QaLease {
            workspace: directory.path().into(),
            group: live_group,
            limits: limits(),
            deadline: Instant::now() + Duration::from_secs(1),
            busy: AtomicBool::new(true),
            live: AtomicBool::new(true),
        });
        *job.lease.lock().unwrap() = Some(lease.clone());
        job.cancel().unwrap();
        assert!(job.cancelled.load(Ordering::Acquire));
        assert!(!lease.live.load(Ordering::Acquire));
        assert!(!job.settled().unwrap());
        drop(owner);
        assert!(!job.settled().unwrap());
        lease.busy.store(false, Ordering::Release);
        assert!(job.settled().unwrap());
        assert_eq!(config.jobs.lock().unwrap().len(), 1);
    }

    #[test]
    fn input_file_is_private_and_cleanup_never_unlinks_replacement_or_live_input() {
        let directory = tempfile::tempdir().unwrap();
        let mut input = InputFile::create(directory.path(), &[0, 255]).unwrap();
        assert_eq!(fs::read(&input.path).unwrap(), [0, 255]);
        assert_eq!(fs::metadata(&input.path).unwrap().mode() & 0o7777, 0o600);
        let original = directory.path().join("pinned");
        fs::rename(&input.path, &original).unwrap();
        fs::write(&input.path, b"replacement").unwrap();
        assert!(input.cleanup().is_err());
        drop(input);
        assert_eq!(fs::read(directory.path().join("pinned")).unwrap(), [0, 255]);
        let mut live = InputFile::create(directory.path(), b"live").unwrap();
        let path = live.path.clone();
        live.quiesced = false;
        drop(live);
        assert_eq!(fs::read(path).unwrap(), b"live");
    }

    #[test]
    fn private_socket_requires_fixed_nonce_path_and_no_public_directory() {
        assert!(validate_socket_path(Path::new("/tmp/socket")).is_err());
        assert!(
            validate_socket_path(Path::new("/run/sentinel-command-broker/not-a-nonce/socket"))
                .is_err()
        );
        assert!(validate_socket_path(Path::new(
            "/run/sentinel-command-broker/00000000-0000-0000-0000-000000000000/../socket"
        ))
        .is_err());
    }

    // Future kernel gates owned by lead: inherited-Landlock controller launch;
    // READY after actual exec; disconnect/deadline descendant quiescence;
    // evaluator plus serial QA; RO candidate and scratch-only Landlock writes;
    // candidate denied broker socket/token, userns, network and parent PIDs.
}
