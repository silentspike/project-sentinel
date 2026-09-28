//! Landlock wrapper — runs INSIDE bwrap, applies Landlock, then exec's the agent command.
//!
//! Usage: `landlock-wrapper <agent-name> -- <command> [args...]`
//!
//! This binary is injected by SandboxEnforcer::start_agent_process() between
//! bwrap and the actual agent command. It applies irreversible Landlock FS
//! restrictions then replaces itself with the agent command via exec.

use std::env;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{self, Child, Command, Stdio};
use std::time::{Duration, Instant};

const ATTESTATION_NONCE_ENV: &str = "SENTINEL_WORKBENCH_ATTESTATION_NONCE";
const ATTESTATION_WRAPPER_VERSION_ENV: &str = "SENTINEL_WORKBENCH_WRAPPER_VERSION";
const ATTESTATION_LANDLOCK_ABI_ENV: &str = "SENTINEL_WORKBENCH_LANDLOCK_ABI";
const COMMAND_BROKER_SOCKET_ENV: &str = "SENTINEL_COMMAND_BROKER_SOCKET";

struct CommandBroker {
    child: Child,
    directory: PathBuf,
    socket: PathBuf,
}

impl Drop for CommandBroker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.socket);
        let _ = fs::remove_dir(&self.directory);
    }
}

fn start_command_broker(nonce: &str, command: &[String]) -> io::Result<CommandBroker> {
    if command.first().map(String::as_str) != Some("/usr/bin/agent-runtime") {
        return Err(io::Error::other(
            "native command broker requires the canonical runtime",
        ));
    }
    let metadata = fs::symlink_metadata(&command[0])?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o7022 != 0
        || metadata.mode() & 0o111 == 0
    {
        return Err(io::Error::other(
            "native command broker executable is not immutable",
        ));
    }
    let mut runtime_membership = OpenOptions::new()
        .write(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open("/run/sentinel-runtime-cgroup.procs")?;
    if !runtime_membership.metadata()?.is_file()
        || nix::sys::statfs::fstatfs(&runtime_membership)
            .map_err(io::Error::from)?
            .filesystem_type()
            != nix::sys::statfs::CGROUP2_SUPER_MAGIC
    {
        return Err(io::Error::other(
            "native command broker runtime cgroup is unavailable",
        ));
    }
    runtime_membership.write_all(b"0")?;

    let root = Path::new("/run/sentinel-command-broker");
    match DirBuilder::new().mode(0o700).create(root) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o777 != 0o700 {
        return Err(io::Error::other(
            "native command broker directory is not private",
        ));
    }
    let directory = root.join(nonce);
    DirBuilder::new().mode(0o700).create(&directory)?;
    let socket = directory.join("socket");
    let cgroup = env::var_os("SENTINEL_COMMAND_CGROUP_ROOT")
        .ok_or_else(|| io::Error::other("native command broker cgroup is unavailable"))?;
    let child = match Command::new(&command[0])
        .args(["--workbench-command-broker-v1"])
        .arg(&socket)
        .arg(env::var_os("SENTINEL_WORKSPACE_ROOT").unwrap_or_else(|| "/workspace".into()))
        .arg(env::var_os("SENTINEL_INPUT_ROOT").unwrap_or_else(|| "/workspace/.inputs".into()))
        .arg(cgroup)
        .arg(process::id().to_string())
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            let _ = fs::remove_dir(&directory);
            return Err(error);
        }
    };
    let mut broker = CommandBroker {
        child,
        directory,
        socket,
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if broker.child.try_wait()?.is_some() {
            return Err(io::Error::other(
                "native command broker exited before readiness",
            ));
        }
        if fs::symlink_metadata(&broker.socket)
            .is_ok_and(|metadata| metadata.file_type().is_socket())
        {
            return Ok(broker);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other("native command broker startup timed out"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args
        .get(1)
        .is_some_and(|arg| arg == "--prepare-workbench-cgroup-namespace-v1")
    {
        if args.len() != 3 {
            eprintln!("Invalid cgroup namespace helper arguments");
            process::exit(2);
        }
        if let Err(error) =
            sentinel_sandbox::cgroups::run_workbench_namespace_helper(Path::new(&args[2]))
        {
            eprintln!("[landlock-wrapper] Cgroup namespace bootstrap failed: {error}");
            process::exit(126);
        }
        return;
    }

    // Parse either the general-agent form or the workbench attestation form:
    // landlock-wrapper <agent-name> -- <command> [args...]
    // landlock-wrapper --attest-v1 <nonce> <abi> <agent-name> -- <command> [args...]
    let separator = args.iter().position(|a| a == "--");
    if args.len() < 4 || separator.is_none() {
        eprintln!("Usage: landlock-wrapper <agent-name> -- <command> [args...]");
        process::exit(2);
    }

    let sep_idx = separator.unwrap();
    let (attestation, agent_name) = if args.get(1).is_some_and(|arg| arg == "--attest-v1") {
        if sep_idx != 5 {
            eprintln!("Invalid workbench attestation arguments");
            process::exit(2);
        }
        let nonce = args.get(2).expect("validated attestation nonce");
        if uuid::Uuid::parse_str(nonce).is_err() {
            eprintln!("Invalid workbench attestation nonce");
            process::exit(2);
        }
        let abi = args
            .get(3)
            .and_then(|value| value.parse::<u8>().ok())
            .filter(|abi| *abi > 0);
        let Some(abi) = abi else {
            eprintln!("Invalid workbench Landlock ABI");
            process::exit(2);
        };
        (Some((nonce.as_str(), abi)), &args[4])
    } else {
        if sep_idx != 2 {
            eprintln!("Invalid Landlock wrapper arguments");
            process::exit(2);
        }
        (None, &args[1])
    };
    let command = &args[sep_idx + 1..];

    if command.is_empty() {
        eprintln!("No command specified after --");
        process::exit(2);
    }

    // Namespace construction must occur in a trusted sibling established
    // before irreversible filesystem Landlock. Candidate processes still get
    // their mandatory rules only after their own mount setup.
    let broker = if let Some((nonce, _)) =
        attestation.filter(|_| env::var_os("SENTINEL_COMMAND_CGROUP_ROOT").is_some())
    {
        match start_command_broker(nonce, command) {
            Ok(broker) => Some(broker),
            Err(_) => {
                eprintln!("[landlock-wrapper] Native command broker startup failed");
                process::exit(126);
            }
        }
    } else {
        None
    };

    // Apply Landlock (irreversible)
    let rules =
        if attestation.is_some() && std::env::var_os("SENTINEL_COMMAND_CGROUP_ROOT").is_some() {
            sentinel_sandbox::LandlockRuleset::for_workbench(agent_name)
        } else {
            sentinel_sandbox::LandlockRuleset::for_agent(agent_name)
        }
        .with_entrypoint_exec(&command[0]);
    let enforcement = match if attestation.is_some() {
        rules.apply_required_status()
    } else {
        rules.apply_status()
    } {
        Ok(enforcement) => enforcement,
        Err(e) => {
            eprintln!("[landlock-wrapper] Landlock apply failed: {e}");
            drop(broker);
            process::exit(126);
        }
    };
    let attested_abi = match attestation {
        Some((_, expected_abi)) => {
            let Some(abi) =
                sentinel_sandbox::landlock::workbench_fully_enforced_abi(enforcement, expected_abi)
            else {
                eprintln!("[landlock-wrapper] Workbench Landlock contract was not fully enforced");
                drop(broker);
                process::exit(126);
            };
            Some(abi)
        }
        None => match enforcement {
            sentinel_sandbox::landlock::LandlockEnforcement::FullyEnforced { .. }
            | sentinel_sandbox::landlock::LandlockEnforcement::PartiallyEnforced => None,
            sentinel_sandbox::landlock::LandlockEnforcement::NotEnforced => {
                eprintln!("[landlock-wrapper] Landlock not enforced");
                drop(broker);
                process::exit(126);
            }
        },
    };
    eprintln!("[landlock-wrapper] Landlock enforced for {agent_name}");

    // Exec the actual command (replaces this process)
    let mut child = Command::new(&command[0]);
    child.args(&command[1..]);
    if let (Some((nonce, _)), Some(abi)) = (attestation, attested_abi) {
        child
            .env(ATTESTATION_NONCE_ENV, nonce)
            .env(ATTESTATION_WRAPPER_VERSION_ENV, env!("CARGO_PKG_VERSION"))
            .env(ATTESTATION_LANDLOCK_ABI_ENV, abi.to_string());
    }
    if let Some(broker) = &broker {
        child.env(COMMAND_BROKER_SOCKET_ENV, &broker.socket);
    }
    let err = child.exec();
    drop(broker);

    // exec() only returns on error
    eprintln!("[landlock-wrapper] exec failed: {err}");
    process::exit(1);
}
