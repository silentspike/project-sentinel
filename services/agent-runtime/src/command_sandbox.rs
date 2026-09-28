//! Initial-entry authorization, not a descendant executable allowlist.
//!
//! bwrap supplies a private mount/PID/network namespace; Landlock is defense in
//! depth. A hidden cgroup owns all descendants, including setsid/double-fork.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use landlock::{
    Access, AccessFs, AccessNet, BitFlags, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset,
    RulesetAttr, RulesetCreatedAttr, RulesetStatus, Scope, ABI,
};
use nix::fcntl::{fcntl, FcntlArg, FdFlag};
use nix::sys::resource::{setrlimit, Resource};
use sentinel_common::{WorkbenchResourceLimits, WorkbenchTool, WORKBENCH_MAX_COMMAND_ARGUMENTS};

use crate::command_membership::CommandMembership;

pub const CHILD_MODE: &str = "--workbench-command-v2";
pub const GATE_MODE: &str = "--workbench-command-gate-v2";
pub const QA_CHILD_MODE: &str = "--workbench-qa-command-v1";
pub const COMMAND_LANDLOCK_ABI: u8 = 6;
pub(crate) const READY: &[u8] = b"ready-v2\n";

/// Trusted outer-enforcer contract, never sourced from a WorkbenchRequest.
/// The writable bind is one FUSE workspace inode scope, with no cross-authority
/// links and atomic aggregate growth reservations capped at workspace_budget_bytes.
/// This is an outer-enforcer attestation of FUSE enforcement, not a host quota.
/// cgroup_root must be private, delegated cgroup v2, invisible to project code.
#[derive(Debug, Clone)]
pub struct CommandBoundary {
    pub cgroup_root: PathBuf,
    pub workspace_budget_bytes: u64,
}

pub struct ScopedCommand {
    pub command: Command,
    pub membership: CommandMembership,
    pub setup: SetupChannel,
}

pub struct SetupChannel {
    stream: UnixStream,
    received: Vec<u8>,
    eof: bool,
    broker_terminal: bool,
    terminal_status: Option<ExitStatus>,
}

impl SetupChannel {
    pub(crate) fn pair() -> io::Result<(Self, OwnedFd)> {
        let (parent, child) = UnixStream::pair()?;
        parent.set_nonblocking(true)?;
        Ok((
            Self {
                stream: parent,
                received: Vec::new(),
                eof: false,
                broker_terminal: false,
                terminal_status: None,
            },
            child.into(),
        ))
    }
    /// EOF after READY proves exec closed the CLOEXEC status descriptor.
    /// An exit code (including 126) is never used as an isolation sentinel.
    pub fn poll(&mut self) -> io::Result<bool> {
        let mut bytes = [0; 128];
        while !self.eof {
            match self.stream.read(&mut bytes) {
                Ok(0) => self.eof = true,
                Ok(count) => {
                    self.received.extend_from_slice(&bytes[..count]);
                    if self.received.len() > READY.len() + if self.broker_terminal { 4 } else { 0 }
                    {
                        return Err(denied());
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        if self.eof {
            if self.broker_terminal {
                if self.received.len() != READY.len() + 4 || !self.received.starts_with(READY) {
                    return Err(denied());
                }
                let raw = i32::from_be_bytes(
                    self.received[READY.len()..]
                        .try_into()
                        .map_err(|_| denied())?,
                );
                let status = ExitStatus::from_raw(raw);
                if !(raw >= 0 && ((raw & 255 == 0 && raw >> 8 <= 255) || (raw <= 64 && raw > 0))) {
                    return Err(denied());
                }
                self.terminal_status = Some(status);
            } else if self.received != READY {
                return Err(denied());
            }
        }
        Ok(self.eof)
    }

    pub fn terminal_status(&self) -> Option<ExitStatus> {
        self.terminal_status
    }
}

fn denied() -> io::Error {
    io::Error::other("scoped command isolation or exec setup is unavailable")
}

pub(crate) fn canonical_directory(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute()
        || path.as_os_str().len() > 4096
        || path.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::CurDir | Component::Prefix(_)
            )
        })
    {
        return Err(denied());
    }
    let mut current = PathBuf::from("/");
    for part in path.components() {
        if let Component::Normal(name) = part {
            current.push(name);
            let metadata = fs::symlink_metadata(&current)?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(denied());
            }
        }
    }
    let canonical = fs::canonicalize(path)?;
    if canonical != path || canonical == Path::new("/") {
        return Err(denied());
    }
    Ok(canonical)
}

pub(crate) fn executable(program: &OsStr) -> io::Result<PathBuf> {
    resolve_executable(program, true)
}

fn resolve_executable(program: &OsStr, check_owner: bool) -> io::Result<PathBuf> {
    let name = program.to_str().ok_or_else(denied)?;
    // Reuse the public canonical validator rather than a second name grammar.
    WorkbenchTool::RunCommand {
        program: name.to_owned(),
        args: Vec::new(),
    }
    .validate_shape()
    .map_err(|_| denied())?;
    let path = fs::canonicalize(Path::new("/usr/bin").join(program))?;
    let metadata = fs::metadata(&path)?;
    if !path.starts_with("/usr/bin")
        || !metadata.is_file()
        || (check_owner && metadata.uid() != 0)
        || metadata.mode() & 0o7022 != 0
        || metadata.mode() & 0o111 == 0
    {
        return Err(denied());
    }
    Ok(path)
}

#[cfg(test)]
fn bwrap_command(
    runner: &Path,
    workspace: &Path,
    inputs: &[PathBuf],
    program: &str,
    file_bytes: u64,
) -> io::Result<Command> {
    broker_bwrap_command(runner, workspace, inputs, program, file_bytes, None, None)
}

pub(crate) fn broker_bwrap_command(
    runner: &Path,
    workspace: &Path,
    inputs: &[PathBuf],
    program: &str,
    file_bytes: u64,
    evaluator: Option<(&Path, &str)>,
    readonly: Option<(&Path, &Path)>,
) -> io::Result<Command> {
    let workspace = canonical_directory(workspace)?;
    validate_input_paths(&workspace, inputs)?;
    executable(OsStr::new(program))?;
    let runner = fs::canonicalize(runner)?;
    let mut command = Command::new(executable(OsStr::new("bwrap"))?);
    command.args([
        "--unshare-user",
        "--unshare-pid",
        "--unshare-net",
        "--unshare-ipc",
        "--unshare-uts",
        "--disable-userns",
        "--die-with-parent",
        "--new-session",
        "--cap-drop",
        "ALL",
    ]);
    // No host-root bind: chmod/utime cannot reach trusted or foreign inodes.
    for path in ["/usr", "/lib", "/lib64", "/bin", "/sbin"] {
        if Path::new(path).exists() {
            command.arg("--ro-bind").arg(path).arg(path);
        }
    }
    command.args([
        "--proc", "/proc", "--dev", "/dev", "--tmpfs", "/dev/shm", "--dir", "/etc",
    ]);
    for path in [
        "/etc/ld.so.cache",
        "/etc/localtime",
        "/etc/passwd",
        "/etc/group",
        "/etc/hosts",
        "/etc/ssl/certs",
    ] {
        if Path::new(path).exists() {
            command.arg("--ro-bind").arg(path).arg(path);
        }
    }
    command
        .arg(if readonly.is_some() {
            "--ro-bind"
        } else {
            "--bind"
        })
        .arg(&workspace)
        .arg(&workspace);
    if let Some((scratch, _)) = readonly {
        let scratch = canonical_directory(scratch)?;
        if scratch.starts_with(&workspace) || workspace.starts_with(&scratch) {
            return Err(denied());
        }
        command.arg("--bind").arg(&scratch).arg(&scratch);
    }
    if let Some((socket, _)) = evaluator {
        if program != "sentinel-coding-qa" || readonly.is_some() {
            return Err(denied());
        }
        command
            .arg("--ro-bind")
            .arg(socket)
            .arg("/run/sentinel-qa-broker.sock");
    }
    // The retained input store is not mounted. Each declared file is its own
    // read-only bind; reject symlink ancestors and directories before bwrap.
    for input in inputs {
        command.arg("--ro-bind").arg(input).arg(input);
    }
    command
        .arg("--ro-bind")
        .arg(runner)
        .arg("/run/sentinel-command-runner");
    command
        .arg("--chdir")
        .arg(&workspace)
        .arg("--")
        .arg("/run/sentinel-command-runner")
        .arg(if readonly.is_some() {
            QA_CHILD_MODE
        } else {
            CHILD_MODE
        })
        .arg(&workspace)
        .arg(program)
        .arg(file_bytes.to_string());
    if let Some((scratch, input)) = readonly {
        command.arg(scratch).arg(input);
    }
    if let Some((_, token)) = evaluator {
        command.arg("--qa-broker-token").arg(token);
    }
    command.arg("--");
    Ok(command)
}

pub(crate) fn validate_input_paths(workspace: &Path, inputs: &[PathBuf]) -> io::Result<()> {
    for input in inputs {
        canonical_directory(input.parent().ok_or_else(denied)?)?;
        let metadata = fs::symlink_metadata(input)?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || input.starts_with(workspace)
            || workspace.starts_with(input)
        {
            return Err(denied());
        }
    }
    Ok(())
}

pub fn launch_command(
    runner: &Path,
    workspace: &Path,
    inputs: &[PathBuf],
    program: &str,
    limits: &WorkbenchResourceLimits,
    boundary: &CommandBoundary,
) -> io::Result<ScopedCommand> {
    // The aggregate retained namespace budget and per-file RLIMIT_FSIZE are
    // independent bounds. A small per-file limit must not disable all commands.
    if boundary.workspace_budget_bytes == 0 {
        return Err(denied());
    }
    canonical_directory(workspace)?;
    validate_input_paths(workspace, inputs)?;
    executable(OsStr::new(program))?;
    let membership = CommandMembership::create(&boundary.cgroup_root, limits)?;
    let (parent, child) = UnixStream::pair()?;
    parent.set_nonblocking(true)?;
    let mut command = Command::new(runner);
    let socket = crate::command_broker::configured_socket()?;
    let request = crate::command_broker::CommandRequest::new(
        workspace,
        inputs,
        program,
        limits,
        membership.path(),
    );
    command
        .arg(crate::command_broker::CLIENT_MODE)
        .arg(socket)
        .arg(serde_json::to_string(&request).map_err(|_| denied())?)
        .arg("--")
        .stdin(Stdio::from(OwnedFd::from(child)));
    Ok(ScopedCommand {
        command,
        membership,
        setup: SetupChannel {
            stream: parent,
            received: Vec::new(),
            eof: false,
            broker_terminal: true,
            terminal_status: None,
        },
    })
}

/// Probe the same irreversible child path before announcing configured native
/// command readiness. Call only from trusted startup, never from model input.
pub fn verify_readiness(
    runner: &Path,
    workspace: &Path,
    boundary: &CommandBoundary,
) -> io::Result<()> {
    use std::time::{Duration, Instant};
    let limits = WorkbenchResourceLimits {
        wall_time_ms: 2000,
        cpu_time_ms: 2000,
        memory_bytes: 128 * 1024 * 1024,
        process_count: 16,
        file_bytes: boundary.workspace_budget_bytes,
        stdout_bytes: 1024,
        stderr_bytes: 1024,
    };
    let ScopedCommand {
        mut command,
        membership,
        mut setup,
    } = launch_command(runner, workspace, &[], "true", &limits, boundary)?;
    command.stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = crate::OwnedCommandChild(command.spawn()?);
    drop(command);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        setup.poll()?;
        if let Some(status) = child.try_wait()? {
            membership.kill_and_wait()?;
            if !status.success()
                || !setup.poll()?
                || !setup.terminal_status().ok_or_else(denied)?.success()
            {
                return Err(denied());
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(denied());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Trusted single-threaded launch stage joins before any subprocess can fork.
/// bwrap then closes extra FDs and hides both cgroupfs and the parent PID tree.
pub fn run_gate(mut arguments: impl Iterator<Item = OsString>) -> io::Result<()> {
    let group = canonical_directory(Path::new(&arguments.next().ok_or_else(denied)?))?;
    if arguments.next().as_deref() != Some(OsStr::new("--")) {
        return Err(denied());
    }
    let program = arguments.next().ok_or_else(denied)?;
    if Path::new(&program) != executable(OsStr::new("bwrap"))? {
        return Err(denied());
    }
    fs::write(group.join("cgroup.procs"), b"0")?;
    close_inherited_descriptors(None)?;
    Err(Command::new(program)
        .args(arguments)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env("LC_ALL", "C.UTF-8")
        .exec())
}

fn close_inherited_descriptors(keep: Option<i32>) -> io::Result<()> {
    let descriptors = fs::read_dir("/proc/self/fd")?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<io::Result<Vec<_>>>()?;
    for name in descriptors {
        if let Some(fd) = name.to_str().and_then(|name| name.parse::<i32>().ok()) {
            if fd > 2 && Some(fd) != keep {
                match nix::unistd::close(fd) {
                    Ok(()) | Err(nix::errno::Errno::EBADF) => {}
                    Err(error) => return Err(io::Error::from(error)),
                }
            }
        }
    }
    Ok(())
}

fn add_path(
    ruleset: landlock::RulesetCreated,
    path: &Path,
    rights: BitFlags<AccessFs>,
) -> io::Result<landlock::RulesetCreated> {
    let rights = if fs::metadata(path)?.is_dir() {
        rights
    } else {
        rights & AccessFs::from_file(ABI::V6)
    };
    ruleset
        .add_rule(PathBeneath::new(
            PathFd::new(path).map_err(|_| denied())?,
            rights,
        ))
        .map_err(|_| denied())
}

fn restrict_command(workspace: &Path, readonly: bool) -> io::Result<()> {
    let all = AccessFs::from_all(ABI::V6);
    let read = AccessFs::ReadFile | AccessFs::ReadDir;
    let mut write = all;
    write.remove(AccessFs::MakeChar | AccessFs::MakeBlock);
    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(all)
        .map_err(|_| denied())?
        .handle_access(AccessNet::BindTcp | AccessNet::ConnectTcp)
        .map_err(|_| denied())?
        .scope(Scope::Signal | Scope::AbstractUnixSocket)
        .map_err(|_| denied())?
        .create()
        .map_err(|_| denied())?;
    ruleset = add_path(ruleset, workspace, write)?;
    // The mount namespace is the primary authority boundary. System tools and
    // loaders can execute, as can newly compiled project code in the workspace.
    for path in ["/usr", "/lib", "/lib64", "/bin", "/sbin"] {
        if Path::new(path).exists() {
            ruleset = add_path(ruleset, Path::new(path), read | AccessFs::Execute)?;
        }
    }
    ruleset = add_path(ruleset, Path::new("/"), read)?;
    for path in ["/dev/null", "/dev/zero", "/dev/urandom", "/dev/random"] {
        ruleset = add_path(
            ruleset,
            Path::new(path),
            AccessFs::ReadFile | AccessFs::WriteFile,
        )?;
    }
    if !readonly {
        ruleset = add_path(ruleset, Path::new("/dev/shm"), write)?;
    }
    if ruleset.restrict_self().map_err(|_| denied())?.ruleset != RulesetStatus::FullyEnforced {
        return Err(denied());
    }
    Ok(())
}

pub fn run_child(arguments: impl Iterator<Item = OsString>) -> io::Result<()> {
    run_child_mode(arguments, false)
}

pub fn run_qa_child(arguments: impl Iterator<Item = OsString>) -> io::Result<()> {
    run_child_mode(arguments, true)
}

fn run_child_mode(arguments: impl Iterator<Item = OsString>, readonly: bool) -> io::Result<()> {
    // Only this trusted setup stage can write the status channel. The tool sees
    // /dev/null on stdin and never inherits its CLOEXEC duplicate.
    let descriptor = nix::unistd::dup(io::stdin()).map_err(io::Error::from)?;
    fcntl(&descriptor, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).map_err(io::Error::from)?;
    let mut status = File::from(descriptor);
    nix::unistd::dup2_stdin(File::open("/dev/null")?).map_err(io::Error::from)?;
    close_inherited_descriptors(Some(status.as_raw_fd()))?;
    let result = child_setup(arguments, &mut status, readonly);
    if result.is_err() {
        let _ = status.write_all(b"setup-failed\n");
    }
    result
}

fn child_setup(
    mut arguments: impl Iterator<Item = OsString>,
    status: &mut File,
    readonly: bool,
) -> io::Result<()> {
    let workspace = canonical_directory(Path::new(&arguments.next().ok_or_else(denied)?))?;
    // The trusted launch already checked host ownership. A nested user namespace
    // can map host root to overflowuid; this view is mounted read-only by bwrap.
    let program = resolve_executable(&arguments.next().ok_or_else(denied)?, false)?;
    let file_bytes = arguments
        .next()
        .and_then(|value| value.to_str().and_then(|text| text.parse::<u64>().ok()))
        .filter(|bytes| *bytes > 0)
        .ok_or_else(denied)?;
    let scratch = if readonly {
        if !matches!(program.file_name().and_then(OsStr::to_str), Some("node"))
            && !program
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name.starts_with("python3"))
        {
            return Err(denied());
        }
        Some(canonical_directory(Path::new(
            &arguments.next().ok_or_else(denied)?,
        ))?)
    } else {
        None
    };
    let input = if let Some(scratch) = &scratch {
        use std::os::unix::fs::OpenOptionsExt;
        let path = PathBuf::from(arguments.next().ok_or_else(denied)?);
        let name = path
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(denied)?;
        if path.parent() != Some(scratch.as_path())
            || !name
                .strip_prefix(".broker-stdin-")
                .is_some_and(crate::command_broker::valid_token)
        {
            return Err(denied());
        }
        let input = fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits())
            .open(path)?;
        let metadata = input.metadata()?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.len() > 65_536
            || metadata.mode() & 0o7777 != 0o600
        {
            return Err(denied());
        }
        Some(input)
    } else {
        None
    };
    let mut separator = arguments.next().ok_or_else(denied)?;
    let token = if separator == "--qa-broker-token"
        && !readonly
        && program.file_name() == Some(OsStr::new("sentinel-coding-qa"))
    {
        let token = arguments.next().ok_or_else(denied)?;
        if !crate::command_broker::valid_token(token.to_str().ok_or_else(denied)?) {
            return Err(denied());
        }
        separator = arguments.next().ok_or_else(denied)?;
        Some(token)
    } else {
        None
    };
    if separator != "--" {
        return Err(denied());
    }
    let arguments = bounded_command_arguments(arguments)?;
    std::env::set_current_dir(&workspace)?;
    setrlimit(Resource::RLIMIT_FSIZE, file_bytes, file_bytes).map_err(io::Error::from)?;
    setrlimit(Resource::RLIMIT_CORE, 0, 0).map_err(io::Error::from)?;
    restrict_command(scratch.as_deref().unwrap_or(&workspace), readonly)?;
    if let Some(input) = input {
        nix::unistd::dup2_stdin(input).map_err(io::Error::from)?;
    }
    let temporary = scratch
        .clone()
        .unwrap_or_else(|| workspace.join(".command-tmp"));
    if !readonly {
        fs::create_dir_all(&temporary)?;
    }
    status.write_all(READY)?;
    let mut command = Command::new(program);
    command
        .args(arguments)
        .env_clear()
        .env("HOME", scratch.as_deref().unwrap_or(&workspace))
        .env("TMPDIR", temporary)
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env("LC_ALL", "C.UTF-8");
    if let Some(token) = token {
        command.env("SENTINEL_QA_BROKER_TOKEN", token);
    }
    if readonly {
        command.env("PYTHONDONTWRITEBYTECODE", "1");
    }
    Err(command.exec())
}

fn bounded_command_arguments(
    arguments: impl Iterator<Item = OsString>,
) -> io::Result<Vec<OsString>> {
    let arguments: Vec<_> = arguments
        .take(WORKBENCH_MAX_COMMAND_ARGUMENTS + 1)
        .collect();
    if arguments.len() > WORKBENCH_MAX_COMMAND_ARGUMENTS
        || arguments.iter().any(|argument| argument.len() > 4096)
    {
        return Err(denied());
    }
    Ok(arguments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_candidate_disables_userns_and_default_and_qa_mounts_hide_broker() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("candidate");
        let scratch = directory.path().join("scratch");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(&scratch).unwrap();
        let runner = directory.path().join("runner");
        fs::write(&runner, b"trusted runner fixture").unwrap();
        let input = scratch.join(format!(".broker-stdin-{}", "a".repeat(64)));
        for readonly in [None, Some((scratch.as_path(), input.as_path()))] {
            let command =
                broker_bwrap_command(&runner, &workspace, &[], "true", 1024, None, readonly)
                    .unwrap();
            let argv: Vec<_> = command.get_args().map(OsStr::to_owned).collect();
            assert!(argv.iter().any(|arg| arg == "--disable-userns"));
            assert!(argv.iter().any(|arg| arg == "--unshare-pid"));
            assert!(argv.iter().any(|arg| arg == "--unshare-net"));
            assert!(!argv
                .iter()
                .any(|arg| arg.to_string_lossy().contains("broker.sock")));
            assert!(!argv.iter().any(|arg| arg == "--qa-broker-token"));
            assert!(!argv
                .windows(3)
                .any(|args| args[0] == "--ro-bind" && args[1] == "/"));
            let mode = if readonly.is_some() {
                "--ro-bind"
            } else {
                "--bind"
            };
            assert!(argv.windows(3).any(|args| args[0] == mode
                && args[1] == workspace.as_os_str()
                && args[2] == workspace.as_os_str()));
            if readonly.is_some() {
                assert_eq!(argv.iter().filter(|arg| *arg == "--bind").count(), 1);
                assert!(argv.iter().any(|arg| arg == QA_CHILD_MODE));
            }
        }
        assert!(broker_bwrap_command(
            &runner,
            &workspace,
            &[],
            "true",
            1024,
            Some((Path::new("/private/socket"), &"a".repeat(64))),
            None
        )
        .is_err());
    }

    #[test]
    fn command_envelope_accepts_qa_family_and_sixty_four_inputs_but_not_more() {
        let mut args = vec![OsString::from("python-project-v1")];
        args.extend((0..64).map(|index| OsString::from(format!("/inputs/source/file-{index}.py"))));
        assert_eq!(
            bounded_command_arguments(args.clone().into_iter()).unwrap(),
            args
        );
        args.push(OsString::from("/inputs/source/extra.py"));
        assert!(bounded_command_arguments(args.into_iter()).is_err());
        assert!(
            bounded_command_arguments(vec![OsString::from("a".repeat(4097))].into_iter()).is_err()
        );
    }

    #[test]
    fn exec_status_requires_ready_and_eof_and_rejects_setup_error() {
        for (message, expected) in [
            (b"ready-v2\n".as_slice(), true),
            (b"".as_slice(), false),
            (b"ready-v2\nsetup-failed\n".as_slice(), false),
        ] {
            let (parent, mut child) = UnixStream::pair().unwrap();
            parent.set_nonblocking(true).unwrap();
            child.write_all(message).unwrap();
            let mut setup = SetupChannel {
                stream: parent,
                received: Vec::new(),
                eof: false,
                broker_terminal: false,
                terminal_status: None,
            };
            if expected {
                assert!(!setup.poll().unwrap());
            }
            drop(child);
            if expected {
                assert!(setup.poll().unwrap());
            } else {
                assert!(setup.poll().is_err());
            }
        }
    }

    #[test]
    fn broker_terminal_channel_retains_actual_exit_and_signal_without_self_signal() {
        for raw in [0, 7 << 8, 126 << 8, 255 << 8, 9, 11, 32, 64] {
            let (parent, mut child) = UnixStream::pair().unwrap();
            parent.set_nonblocking(true).unwrap();
            let mut setup = SetupChannel {
                stream: parent,
                received: Vec::new(),
                eof: false,
                broker_terminal: true,
                terminal_status: None,
            };
            child.write_all(READY).unwrap();
            assert!(!setup.poll().unwrap());
            child.write_all(&i32::to_be_bytes(raw)).unwrap();
            assert!(!setup.poll().unwrap());
            drop(child);
            assert!(setup.poll().unwrap());
            assert_eq!(setup.terminal_status().unwrap().into_raw(), raw);
        }
        for suffix in [
            Vec::new(),
            (-1_i32).to_be_bytes().to_vec(),
            65_i32.to_be_bytes().to_vec(),
            vec![0; 5],
        ] {
            let (parent, mut child) = UnixStream::pair().unwrap();
            child.write_all(READY).unwrap();
            child.write_all(&suffix).unwrap();
            drop(child);
            let mut setup = SetupChannel {
                stream: parent,
                received: Vec::new(),
                eof: false,
                broker_terminal: true,
                terminal_status: None,
            };
            assert!(setup.poll().is_err());
        }
    }

    #[test]
    fn dotted_program_names_use_the_canonical_rules() {
        assert!(WorkbenchTool::RunCommand {
            program: "python3.12".into(),
            args: Vec::new()
        }
        .validate_shape()
        .is_ok());
        assert!(executable(OsStr::new("../python3")).is_err());
    }

    #[test]
    fn command_envelope_rejects_symlink_and_overlapping_inputs() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&workspace, &alias).unwrap();
        assert!(bwrap_command(Path::new("runner"), &alias, &[], "ls", 1024).is_err());
        fs::write(workspace.join("input"), b"input").unwrap();
        assert!(validate_input_paths(&workspace, &[workspace.join("input")]).is_err());
        let inputs = root.path().join("inputs");
        fs::create_dir(&inputs).unwrap();
        fs::write(inputs.join("declared"), b"declared").unwrap();
        assert!(validate_input_paths(&workspace, &[inputs.join("declared")]).is_ok());
        std::os::unix::fs::symlink(inputs.join("declared"), inputs.join("alias")).unwrap();
        assert!(validate_input_paths(&workspace, &[inputs.join("alias")]).is_err());
    }
}
