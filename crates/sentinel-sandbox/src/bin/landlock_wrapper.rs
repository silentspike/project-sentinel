//! Landlock wrapper — runs INSIDE bwrap, applies Landlock, then exec's the agent command.
//!
//! Usage: `landlock-wrapper <agent-name> -- <command> [args...]`
//!
//! This binary is injected by SandboxEnforcer::start_agent_process() between
//! bwrap and the actual agent command. It applies irreversible Landlock FS
//! restrictions then replaces itself with the agent command via exec.

use std::env;
use std::os::unix::process::CommandExt;
use std::process::{self, Command};

const ATTESTATION_NONCE_ENV: &str = "SENTINEL_WORKBENCH_ATTESTATION_NONCE";
const ATTESTATION_WRAPPER_VERSION_ENV: &str = "SENTINEL_WORKBENCH_WRAPPER_VERSION";
const ATTESTATION_LANDLOCK_ABI_ENV: &str = "SENTINEL_WORKBENCH_LANDLOCK_ABI";

fn main() {
    let args: Vec<String> = env::args().collect();

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

    // Apply Landlock (irreversible)
    let rules =
        sentinel_sandbox::LandlockRuleset::for_agent(agent_name).with_entrypoint_exec(&command[0]);
    let enforcement = match rules.apply_status() {
        Ok(enforcement) => enforcement,
        Err(e) => {
            eprintln!("[landlock-wrapper] Landlock apply failed: {e}");
            process::exit(126);
        }
    };
    let attested_abi = match attestation {
        Some((_, expected_abi)) => {
            let Some(abi) =
                sentinel_sandbox::landlock::workbench_fully_enforced_abi(enforcement, expected_abi)
            else {
                eprintln!("[landlock-wrapper] Workbench Landlock contract was not fully enforced");
                process::exit(126);
            };
            Some(abi)
        }
        None => match enforcement {
            sentinel_sandbox::landlock::LandlockEnforcement::FullyEnforced { .. }
            | sentinel_sandbox::landlock::LandlockEnforcement::PartiallyEnforced => None,
            sentinel_sandbox::landlock::LandlockEnforcement::NotEnforced => {
                eprintln!("[landlock-wrapper] Landlock not enforced");
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
    let err = child.exec();

    // exec() only returns on error
    eprintln!("[landlock-wrapper] exec failed: {err}");
    process::exit(1);
}
