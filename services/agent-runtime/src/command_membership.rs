//! Kernel-owned invocation lifetime and cumulative accounting (cgroup v2).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::statfs::{statfs, CGROUP2_SUPER_MAGIC};
use sentinel_common::WorkbenchResourceLimits;

#[derive(Debug, Default)]
pub struct CommandUsage {
    pub cpu_time_ms: u64,
    pub peak_memory_bytes: u64,
    pub peak_process_count: u32,
    /// Kernel block-I/O bytes, not logical write() bytes or retained file size.
    pub storage_bytes_written: u64,
    pub oom_kills: u64,
    pub process_limit_hits: u64,
}

pub struct CommandMembership {
    path: PathBuf,
}

impl CommandMembership {
    pub fn create(root: &Path, limits: &WorkbenchResourceLimits) -> io::Result<Self> {
        let root = crate::command_sandbox::canonical_directory(root)?;
        if statfs(&root).map_err(io::Error::from)?.filesystem_type() != CGROUP2_SUPER_MAGIC {
            return Err(invalid());
        }
        let path = root.join(format!("command-{}", crate::random_receipt_suffix()?));
        fs::create_dir(&path)?;
        let membership = Self { path };
        for (name, value) in [
            ("memory.max", limits.memory_bytes.to_string()),
            ("memory.swap.max", "0".into()),
            ("memory.oom.group", "1".into()),
            ("pids.max", limits.process_count.to_string()),
        ] {
            fs::write(membership.path.join(name), value)?;
        }
        membership.sample()?;
        if !membership.path.join("cgroup.kill").is_file() {
            return Err(invalid());
        }
        Ok(membership)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn sample(&self) -> io::Result<CommandUsage> {
        let cpu = fs::read_to_string(self.path.join("cpu.stat"))?;
        let io = fs::read_to_string(self.path.join("io.stat"))?;
        let memory = fs::read_to_string(self.path.join("memory.events"))?;
        let pids = fs::read_to_string(self.path.join("pids.events"))?;
        Ok(CommandUsage {
            cpu_time_ms: field(&cpu, "usage_usec")?.div_ceil(1000),
            peak_memory_bytes: fs::read_to_string(self.path.join("memory.peak"))?
                .trim()
                .parse()
                .map_err(|_| invalid())?,
            peak_process_count: fs::read_to_string(self.path.join("pids.peak"))?
                .trim()
                .parse()
                .map_err(|_| invalid())?,
            storage_bytes_written: io_bytes(&io)?,
            oom_kills: field(&memory, "oom_kill")?,
            process_limit_hits: field(&pids, "max")?,
        })
    }

    pub fn populated(&self) -> io::Result<bool> {
        match field(
            &fs::read_to_string(self.path.join("cgroup.events"))?,
            "populated",
        )? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid()),
        }
    }

    pub fn kill_and_wait(&self) -> io::Result<()> {
        fs::write(self.path.join("cgroup.kill"), b"1")?;
        let deadline = Instant::now() + Duration::from_secs(1);
        while self.populated()? {
            if Instant::now() >= deadline {
                return Err(invalid());
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }
}

impl Drop for CommandMembership {
    fn drop(&mut self) {
        // Best effort on error unwinding, never a successful-receipt proof.
        if self.kill_and_wait().is_ok() {
            let _ = fs::remove_dir(&self.path);
        }
    }
}

fn invalid() -> io::Error {
    io::Error::other("command cgroup accounting or quiescence unavailable")
}

fn field(text: &str, name: &str) -> io::Result<u64> {
    let mut found = None;
    for line in text.lines() {
        let mut words = line.split_whitespace();
        if words.next() == Some(name) {
            if found.is_some() {
                return Err(invalid());
            }
            found = Some(
                words
                    .next()
                    .ok_or_else(invalid)?
                    .parse()
                    .map_err(|_| invalid())?,
            );
            if words.next().is_some() {
                return Err(invalid());
            }
        }
    }
    found.ok_or_else(invalid)
}

fn io_bytes(text: &str) -> io::Result<u64> {
    let mut total = 0_u64;
    for line in text.lines() {
        let mut bytes = None;
        for item in line.split_whitespace().skip(1) {
            if let Some(value) = item.strip_prefix("wbytes=") {
                if bytes.is_some() {
                    return Err(invalid());
                }
                bytes = Some(value.parse::<u64>().map_err(|_| invalid())?);
            }
        }
        total = total
            .checked_add(bytes.ok_or_else(invalid)?)
            .ok_or_else(invalid)?;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cumulative_accounting_is_strict_and_sums_devices() {
        assert_eq!(
            field("usage_usec 12000\nuser_usec 9000\n", "usage_usec").unwrap(),
            12000
        );
        assert!(field("usage_usec 1\nusage_usec 2\n", "usage_usec").is_err());
        assert!(field("user_usec 1\n", "usage_usec").is_err());
        assert_eq!(
            io_bytes("8:0 rbytes=1 wbytes=4096 rios=1\n8:1 wbytes=8192\n").unwrap(),
            12288
        );
        assert_eq!(io_bytes("").unwrap(), 0);
        assert!(io_bytes("8:0 rbytes=1").is_err());
        assert!(io_bytes("8:0 wbytes=1 wbytes=2").is_err());
    }
}
