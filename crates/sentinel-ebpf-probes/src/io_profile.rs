//! eBPF probe: I/O profiling via tracepoint/block:block_rq_complete.
//!
//! Tracks read/write IOPS and throughput per cgroup at the block layer.
//! Uses Per-CPU Hash Map for lock-free I/O accounting.
//!
//! Hook: tracepoint/block/block_rq_complete
//! Map: Per-CPU Hash (cgroup_id -> IoStats { read_ops, write_ops, read_bytes, write_bytes })
//!
//! Build: cargo +nightly build -Z build-std=core --target bpfel-unknown-none

#![no_std]
#![no_main]

use aya_ebpf::{
    helpers::bpf_get_current_cgroup_id,
    macros::{map, tracepoint},
    maps::PerCpuHashMap,
    programs::TracePointContext,
};

/// I/O statistics per cgroup, stored in Per-CPU Hash Map.
#[repr(C)]
pub struct IoStats {
    pub read_ops: u64,
    pub write_ops: u64,
    pub read_bytes: u64,
    pub write_bytes: u64,
}

/// Per-CPU Hash Map: cgroup_id -> IoStats.
/// Max 128 entries covers 54 agents + headroom.
#[map]
static IO_STATS: PerCpuHashMap<u64, IoStats> = PerCpuHashMap::with_max_entries(128, 0);

/// Tracepoint hook on block:block_rq_complete.
/// Increments I/O counters for the calling cgroup.
#[tracepoint(category = "block", name = "block_rq_complete")]
pub fn io_profile_probe(ctx: TracePointContext) -> u32 {
    match try_io_profile(ctx) {
        Ok(ret) => ret,
        Err(_) => 0,
    }
}

#[inline(always)]
fn try_io_profile(ctx: TracePointContext) -> Result<u32, u32> {
    let cgroup_id = unsafe { bpf_get_current_cgroup_id() };

    // Read the request bytes and rwbs (read/write flag) from tracepoint args.
    // block_rq_complete format: dev, sector, nr_sector, errors, rwbs
    // rwbs offset in trace event struct is typically at offset 32.
    let nr_sector: u32 = unsafe { ctx.read_at(24).map_err(|_| 0u32)? };
    let rwbs: u8 = unsafe { ctx.read_at(32).map_err(|_| 0u32)? };
    let bytes = (nr_sector as u64) * 512;

    let is_write = rwbs == b'W' || rwbs == b'w';

    // Get or create entry for this cgroup.
    if let Some(stats) = unsafe { IO_STATS.get_ptr_mut(&cgroup_id) } {
        let stats = unsafe { &mut *stats };
        if is_write {
            stats.write_ops += 1;
            stats.write_bytes += bytes;
        } else {
            stats.read_ops += 1;
            stats.read_bytes += bytes;
        }
    } else {
        // First event for this cgroup — create new entry.
        let mut stats = IoStats {
            read_ops: 0,
            write_ops: 0,
            read_bytes: 0,
            write_bytes: 0,
        };
        if is_write {
            stats.write_ops = 1;
            stats.write_bytes = bytes;
        } else {
            stats.read_ops = 1;
            stats.read_bytes = bytes;
        }
        let _ = IO_STATS.insert(&cgroup_id, &stats, 0);
    }

    Ok(0)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
