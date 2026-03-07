//! agent-runtime: Lightweight sandbox process for sentinel agents.
//!
//! Runs inside bwrap namespace. Reads stdin (for future command dispatch),
//! writes periodic heartbeats to generate VFS I/O (tracked by eBPF).
//! Exits on EOF (stdin closed) or when `--die-with-parent` triggers.
//!
//! Zero external dependencies — only `std`.

use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Heartbeat interval in seconds.
const HEARTBEAT_INTERVAL_SECS: u64 = 30;

/// Main loop sleep granularity in milliseconds.
const SLEEP_MS: u64 = 500;

fn main() {
    eprintln!("agent-runtime: started (pid={})", std::process::id());

    let running = Arc::new(AtomicBool::new(true));

    // Stdin reader thread — blocks on read, sets running=false on EOF.
    let r = running.clone();
    std::thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(cmd) if cmd.trim() == "shutdown" => {
                    eprintln!("agent-runtime: shutdown command received");
                    break;
                }
                Ok(_) => {}      // Consume silently (future: JSON command dispatch)
                Err(_) => break, // EOF — daemon closed stdin
            }
        }
        r.store(false, Ordering::Relaxed);
    });

    // Initial heartbeat (immediate I/O for eBPF tracking).
    write_heartbeat();

    let mut last_heartbeat = Instant::now();

    while running.load(Ordering::Relaxed) {
        if last_heartbeat.elapsed() >= Duration::from_secs(HEARTBEAT_INTERVAL_SECS) {
            write_heartbeat();
            last_heartbeat = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(SLEEP_MS));
    }

    eprintln!("agent-runtime: shutting down");
}

/// Write a timestamp to `/tmp/heartbeat` — generates VFS I/O visible
/// in `/proc/{pid}/io` (wchar), tracked by sentinel-ebpf collector.
fn write_heartbeat() {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let result = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("/tmp/heartbeat")
        .and_then(|mut f| writeln!(f, "{ts}"));

    if let Err(e) = result {
        eprintln!("agent-runtime: heartbeat write failed: {e}");
    }
}
