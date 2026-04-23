//! Breakout helper binary — runs INSIDE the bwrap sandbox.
//!
//! Attempts various sandbox escape scenarios and reports results via exit code:
//! - 0 = breakout blocked (expected, security holds)
//! - 1 = breakout succeeded (security bug!)
//! - 2 = setup error (test infrastructure problem)
//!
//! Usage: `breakout-helper [--landlock AGENT_NAME] SCENARIO`

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::{self, Command};
use std::time::{Duration, Instant};

const EXIT_BLOCKED: i32 = 0;
const EXIT_BREAKOUT: i32 = 1;
const EXIT_SETUP_ERROR: i32 = 2;

fn main() {
    let args: Vec<String> = env::args().collect();

    let (landlock_agent, scenario) = parse_args(&args);

    // Apply Landlock if requested (irreversible!)
    if let Some(ref agent_name) = landlock_agent {
        let rules = sentinel_sandbox::LandlockRuleset::for_agent(agent_name);
        match rules.apply() {
            Ok(true) => eprintln!("Landlock applied for {agent_name}"),
            Ok(false) => eprintln!("Landlock not enforced (kernel support missing?)"),
            Err(e) => {
                eprintln!("Landlock apply failed: {e}");
                process::exit(EXIT_SETUP_ERROR);
            }
        }
    }

    let exit_code = match scenario.as_str() {
        "write-etc" => scenario_write_etc(),
        "read-other-home" => scenario_read_other_home(),
        "exec-from-tmp" => scenario_exec_from_tmp(),
        "exec-from-home" => scenario_exec_from_home(),
        "exec-bin-sh" => scenario_exec_bin_sh(),
        "exec-python3" => scenario_exec_python3(),
        "symlink-escape" => scenario_symlink_escape(),
        "memory-bomb" => scenario_memory_bomb(),
        "fork-bomb" => scenario_fork_bomb(),
        "cpu-burn" => scenario_cpu_burn(),
        "pid-count" => scenario_pid_count(),
        "hostname" => scenario_hostname(),
        _ => {
            eprintln!("Unknown scenario: {scenario}");
            eprintln!("Available: write-etc, read-other-home, exec-from-tmp, exec-from-home,");
            eprintln!("          exec-bin-sh, exec-python3, symlink-escape,");
            eprintln!("          memory-bomb, fork-bomb, cpu-burn, pid-count, hostname");
            EXIT_SETUP_ERROR
        }
    };

    process::exit(exit_code);
}

fn parse_args(args: &[String]) -> (Option<String>, String) {
    let mut landlock_agent = None;
    let mut scenario = None;
    let mut i = 1;

    while i < args.len() {
        if args[i] == "--landlock" {
            if i + 1 < args.len() {
                landlock_agent = Some(args[i + 1].clone());
                i += 2;
                continue;
            } else {
                eprintln!("--landlock requires agent name");
                process::exit(EXIT_SETUP_ERROR);
            }
        }
        scenario = Some(args[i].clone());
        i += 1;
    }

    match scenario {
        Some(s) => (landlock_agent, s),
        None => {
            eprintln!("Usage: breakout-helper [--landlock <agent-name>] <scenario>");
            process::exit(EXIT_SETUP_ERROR);
        }
    }
}

/// FS-001: Attempt to write to /etc/passwd.
/// Expected: ENOENT (bwrap) or EACCES (Landlock).
fn scenario_write_etc() -> i32 {
    match fs::write("/etc/passwd", "breakout-test") {
        Ok(()) => {
            eprintln!("SECURITY BUG: wrote to /etc/passwd!");
            EXIT_BREAKOUT
        }
        Err(e) => {
            eprintln!("Write to /etc/passwd blocked: {e}");
            EXIT_BLOCKED
        }
    }
}

/// FS-002: Attempt to read another agent's home directory.
/// Expected: ENOENT (not visible in mount namespace).
fn scenario_read_other_home() -> i32 {
    match fs::read_dir("/home/other-agent") {
        Ok(entries) => {
            let count = entries.count();
            eprintln!("SECURITY BUG: read /home/other-agent ({count} entries)!");
            EXIT_BREAKOUT
        }
        Err(e) => {
            eprintln!("Read /home/other-agent blocked: {e}");
            EXIT_BLOCKED
        }
    }
}

/// FS-003: Write a script to /tmp and attempt to execute it.
/// NOTE: Landlock bug gives all_access (incl. Execute) to write_paths.
/// Defense relies on bwrap mount namespace (no /bin/sh in production config).
fn scenario_exec_from_tmp() -> i32 {
    let script_path = "/tmp/evil.sh";

    // Write a script
    let write_result = fs::write(script_path, "#!/bin/sh\necho pwned\n");
    if let Err(e) = write_result {
        eprintln!("Could not write script to /tmp: {e}");
        return EXIT_BLOCKED;
    }

    // Make executable
    if let Err(e) = fs::set_permissions(script_path, fs::Permissions::from_mode(0o755)) {
        eprintln!("Could not chmod /tmp/evil.sh: {e}");
        // Cleanup
        let _ = fs::remove_file(script_path);
        return EXIT_BLOCKED;
    }

    // Try to execute
    match Command::new(script_path).output() {
        Ok(output) => {
            let _ = fs::remove_file(script_path);
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                eprintln!("SECURITY FINDING: exec from /tmp succeeded: {stdout}");
                EXIT_BREAKOUT
            } else {
                eprintln!("Exec from /tmp failed with status: {}", output.status);
                EXIT_BLOCKED
            }
        }
        Err(e) => {
            let _ = fs::remove_file(script_path);
            eprintln!("Exec from /tmp blocked: {e}");
            EXIT_BLOCKED
        }
    }
}

/// FS-003b: Write a script into the agent home and attempt to execute it.
/// Expected: blocked once Landlock no longer grants execute to write paths.
fn scenario_exec_from_home() -> i32 {
    let home = env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let script_path = format!("{home}/.issue264-evil.sh");
    scenario_exec_script(&script_path, "Exec from agent home")
}

/// FS-003c: Attempt to execute the shell from the sandbox.
/// Expected: blocked in the hardened config because no executable should leak through.
fn scenario_exec_bin_sh() -> i32 {
    match Command::new("/bin/sh")
        .arg("-c")
        .arg("echo shell-ok")
        .output()
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!(
                "SECURITY FINDING: /bin/sh executed (status={}, stdout={stdout:?}, stderr={stderr:?})",
                output.status
            );
            EXIT_BREAKOUT
        }
        Err(e) => {
            eprintln!("Exec /bin/sh blocked: {e}");
            EXIT_BLOCKED
        }
    }
}

/// FS-003d: Attempt to execute the Python interpreter from the sandbox.
/// Expected: blocked once the execute whitelist is tightened.
fn scenario_exec_python3() -> i32 {
    match Command::new("/usr/bin/python3")
        .arg("-c")
        .arg("print('python-ok')")
        .output()
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!(
                "SECURITY FINDING: /usr/bin/python3 executed (status={}, stdout={stdout:?}, stderr={stderr:?})",
                output.status
            );
            EXIT_BREAKOUT
        }
        Err(e) => {
            eprintln!("Exec /usr/bin/python3 blocked: {e}");
            EXIT_BLOCKED
        }
    }
}

fn scenario_exec_script(script_path: &str, label: &str) -> i32 {
    let path = std::path::Path::new(script_path);
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("{label} blocked while creating parent directory: {e}");
            return EXIT_BLOCKED;
        }
    }

    if let Err(e) = fs::write(path, "#!/bin/sh\necho pwned\n") {
        eprintln!("{label} blocked while writing script: {e}");
        return EXIT_BLOCKED;
    }

    if let Err(e) = fs::set_permissions(path, fs::Permissions::from_mode(0o755)) {
        let _ = fs::remove_file(path);
        eprintln!("{label} blocked while chmodding script: {e}");
        return EXIT_BLOCKED;
    }

    match Command::new(path).output() {
        Ok(output) => {
            let _ = fs::remove_file(path);
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                eprintln!("{label} succeeded: {stdout}");
                EXIT_BREAKOUT
            } else {
                eprintln!("{label} failed with status: {}", output.status);
                EXIT_BLOCKED
            }
        }
        Err(e) => {
            let _ = fs::remove_file(path);
            eprintln!("{label} blocked: {e}");
            EXIT_BLOCKED
        }
    }
}

/// FS-004: Create a symlink from /tmp to /etc/shadow, then read it.
/// Expected: ENOENT (target not in mount namespace) or EACCES.
fn scenario_symlink_escape() -> i32 {
    let link_path = "/tmp/escape-link";

    // Cleanup any previous attempt
    let _ = fs::remove_file(link_path);

    // Create symlink pointing outside sandbox
    if let Err(e) = std::os::unix::fs::symlink("/etc/shadow", link_path) {
        eprintln!("Could not create symlink: {e}");
        return EXIT_BLOCKED;
    }

    // Try to read through the symlink
    match fs::read_to_string(link_path) {
        Ok(content) => {
            let _ = fs::remove_file(link_path);
            eprintln!(
                "SECURITY BUG: read /etc/shadow via symlink ({} bytes)!",
                content.len()
            );
            EXIT_BREAKOUT
        }
        Err(e) => {
            let _ = fs::remove_file(link_path);
            eprintln!("Symlink escape blocked: {e}");
            EXIT_BLOCKED
        }
    }
}

/// RES-001: Allocate memory until OOM-killed.
/// Expected: SIGKILL from cgroup OOM killer (exit code 137).
fn scenario_memory_bomb() -> i32 {
    let mut buffers: Vec<Vec<u8>> = Vec::new();
    let chunk_size = 1_048_576; // 1 MB

    eprintln!("Starting memory bomb (1MB chunks)...");
    loop {
        let chunk = vec![0xFFu8; chunk_size];
        buffers.push(chunk);
        if buffers.len().is_multiple_of(50) {
            eprintln!("Allocated {} MB", buffers.len());
        }
    }
    // If we reach here, OOM-killer didn't intervene
    // (unreachable in practice — compiler needs this for type checking)
}

/// RES-002: Fork bomb via spawning child processes.
/// Expected: EAGAIN after hitting pids.max cgroup limit.
fn scenario_fork_bomb() -> i32 {
    let mut children = Vec::new();
    let mut spawn_failures = 0;

    eprintln!("Starting fork bomb (spawning sleep processes)...");
    for i in 0..1000 {
        match Command::new("sleep").arg("9999").spawn() {
            Ok(child) => {
                children.push(child);
                if i % 10 == 0 {
                    eprintln!("Spawned {i} children");
                }
            }
            Err(e) => {
                eprintln!("Spawn failed at child {i}: {e}");
                spawn_failures += 1;
                if spawn_failures >= 3 {
                    break;
                }
            }
        }
    }

    // Cleanup: kill all children
    for mut child in children {
        let _ = child.kill();
        let _ = child.wait();
    }

    if spawn_failures > 0 {
        eprintln!("Fork bomb contained: {spawn_failures} spawn failures");
        EXIT_BLOCKED
    } else {
        eprintln!("SECURITY BUG: spawned 1000 children without limit!");
        EXIT_BREAKOUT
    }
}

/// RES-003: CPU burn for 10 seconds, measure actual wall time.
/// Under cgroup CPU throttling, wall time will exceed expected duration.
fn scenario_cpu_burn() -> i32 {
    let burn_duration = Duration::from_secs(10);
    let start = Instant::now();

    eprintln!("Starting CPU burn for {burn_duration:?}...");

    // Tight loop consuming CPU
    let mut counter: u64 = 0;
    while start.elapsed() < burn_duration {
        counter = counter.wrapping_add(1);
        // Prevent optimization
        if counter.is_multiple_of(100_000_000) {
            std::hint::black_box(counter);
        }
    }

    let elapsed = start.elapsed();
    eprintln!("CPU burn completed: {elapsed:?} wall time, {counter} iterations");

    // Report timing — the test harness checks cpu.stat for throttling
    println!("elapsed_ms={}", elapsed.as_millis());
    println!("iterations={counter}");
    EXIT_BLOCKED // Always "blocked" — test checks cpu.stat externally
}

/// NS-001: Count PIDs visible in /proc.
/// In a PID namespace, should see only sandbox-internal processes (<=5).
fn scenario_pid_count() -> i32 {
    let proc_path = "/proc";

    let entries = match fs::read_dir(proc_path) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Cannot read /proc: {e}");
            // /proc not mounted is expected without proc_mount
            println!("pid_count=0");
            return EXIT_BLOCKED;
        }
    };

    let pid_count = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|n| n.chars().all(|c| c.is_ascii_digit()))
                .unwrap_or(false)
        })
        .count();

    eprintln!("Visible PIDs in /proc: {pid_count}");
    println!("pid_count={pid_count}");

    // Caller decides threshold — we just report
    EXIT_BLOCKED
}

/// NS-002: Read hostname from /proc.
/// In a UTS namespace, should be "sentinel-{name}", not the host hostname.
fn scenario_hostname() -> i32 {
    // Try /proc/sys/kernel/hostname first
    match fs::read_to_string("/proc/sys/kernel/hostname") {
        Ok(hostname) => {
            let hostname = hostname.trim();
            eprintln!("Hostname: {hostname}");
            println!("{hostname}");
            EXIT_BLOCKED
        }
        Err(e) => {
            eprintln!("Cannot read hostname from /proc: {e}");
            // Fallback: /etc/hostname
            match fs::read_to_string("/etc/hostname") {
                Ok(h) => {
                    let h = h.trim();
                    eprintln!("Hostname (from /etc/hostname): {h}");
                    println!("{h}");
                    EXIT_BLOCKED
                }
                Err(_) => {
                    eprintln!("Cannot determine hostname");
                    EXIT_SETUP_ERROR
                }
            }
        }
    }
}
