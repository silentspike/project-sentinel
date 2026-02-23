//! eBPF probe: Agent health via fentry/vfs_write.
//!
//! Tracks the last write() syscall timestamp per cgroup to detect stalled agents.
//! Uses Per-CPU Hash Map for lock-free updates across all CPUs.
//!
//! Hook: fentry/vfs_write
//! Map: Per-CPU Hash (cgroup_id -> last_write_timestamp_ns)
//!
//! Build: cargo +nightly build -Z build-std=core --target bpfel-unknown-none
//!
//! NOTE: This file requires the bpfel-unknown-none target and aya-ebpf toolchain.
//! It is NOT compiled as part of the normal workspace build.
//! See sentinel-ebpf loader.rs for the userspace counterpart.

#![no_std]
#![no_main]

use aya_ebpf::{
    helpers::bpf_get_current_cgroup_id,
    helpers::bpf_ktime_get_ns,
    macros::{fentry, map},
    maps::PerCpuHashMap,
    programs::FEntryContext,
};

/// Per-CPU Hash Map: cgroup_id (u64) -> last_write_timestamp_ns (u64).
/// Max 128 entries covers 54 agents + headroom.
#[map]
static AGENT_HEALTH: PerCpuHashMap<u64, u64> = PerCpuHashMap::with_max_entries(128, 0);

/// fentry hook on vfs_write.
/// Records current kernel timestamp for the calling cgroup.
#[fentry(function = "vfs_write")]
pub fn agent_health_probe(_ctx: FEntryContext) -> u32 {
    match try_agent_health() {
        Ok(ret) => ret,
        Err(_) => 0,
    }
}

#[inline(always)]
fn try_agent_health() -> Result<u32, u32> {
    let cgroup_id = unsafe { bpf_get_current_cgroup_id() };
    let ts = unsafe { bpf_ktime_get_ns() };

    // Update the per-CPU map with current timestamp.
    // BPF_ANY (0) = create or update.
    let _ = AGENT_HEALTH.insert(&cgroup_id, &ts, 0);

    Ok(0)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
