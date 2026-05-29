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

fn main() {
    let args: Vec<String> = env::args().collect();

    // Parse: landlock-wrapper <agent-name> -- <command> [args...]
    let separator = args.iter().position(|a| a == "--");
    if args.len() < 4 || separator.is_none() {
        eprintln!("Usage: landlock-wrapper <agent-name> -- <command> [args...]");
        process::exit(2);
    }

    let agent_name = &args[1];
    let sep_idx = separator.unwrap();
    let command = &args[sep_idx + 1..];

    if command.is_empty() {
        eprintln!("No command specified after --");
        process::exit(2);
    }

    // Apply Landlock (irreversible)
    let rules =
        sentinel_sandbox::LandlockRuleset::for_agent(agent_name).with_entrypoint_exec(&command[0]);
    match rules.apply() {
        Ok(true) => eprintln!("[landlock-wrapper] Landlock enforced for {agent_name}"),
        Ok(false) => eprintln!("[landlock-wrapper] Landlock not enforced (kernel too old)"),
        Err(e) => {
            eprintln!("[landlock-wrapper] Landlock apply failed: {e}");
            // Non-fatal: continue with bwrap-only isolation
        }
    }

    // Exec the actual command (replaces this process)
    let err = Command::new(&command[0]).args(&command[1..]).exec();

    // exec() only returns on error
    eprintln!("[landlock-wrapper] exec failed: {err}");
    process::exit(1);
}
