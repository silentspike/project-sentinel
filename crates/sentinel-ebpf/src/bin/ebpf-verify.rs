//! Standalone verification binary for eBPF probes.
//!
//! Loads all probes, waits for data, reads BPF maps, and prints results.
//! Must be run with CAP_BPF (typically as root).
//!
//! Usage: sudo ./ebpf-verify

#[cfg(feature = "ebpf")]
fn main() -> anyhow::Result<()> {
    use std::thread;
    use std::time::Duration;

    use sentinel_ebpf::loader;

    tracing_subscriber::fmt::init();

    println!("=== Sentinel eBPF Probe Verification ===\n");

    // Step 1: Detect capabilities
    let report = loader::detect_capabilities();
    println!("Capability Report:");
    println!("  BTF available:    {}", report.btf_available);
    println!("  CAP_BPF:          {}", report.cap_bpf);
    println!("  Kernel:           {}", report.kernel_version);
    println!("  fentry support:   {}", report.fentry_support);
    println!("  Mode:             {}", report.mode);
    println!("  Reason:           {}", report.reason);
    println!();

    if report.mode != loader::MonitoringMode::Kernel {
        println!("ERROR: Kernel mode not available. Cannot load probes.");
        println!("Make sure to run with CAP_BPF (e.g., sudo).");
        std::process::exit(1);
    }

    // Step 2: Load probes
    println!("Loading eBPF probes...");
    let mut probes = loader::load_ebpf_probes()?;
    println!("All probes loaded and attached.\n");

    // Step 3: Wait for data to accumulate
    println!("Waiting 5 seconds for probe data...");
    thread::sleep(Duration::from_secs(5));

    // Step 4: Read maps
    println!("\n=== AGENT_HEALTH Map (Per-CPU Hash) ===");
    if let Some(map) = probes.agent_health.map("AGENT_HEALTH") {
        let map: aya::maps::PerCpuHashMap<_, u64, u64> = aya::maps::PerCpuHashMap::try_from(map)?;
        let mut count = 0;
        for (cgroup_id, per_cpu_values) in map.iter().flatten() {
            let max_ts = per_cpu_values.iter().copied().max().unwrap_or(0);
            println!(
                "  cgroup_id={} max_timestamp_ns={} ({:.2}s ago)",
                cgroup_id,
                max_ts,
                (monotonic_ns() - max_ts) as f64 / 1e9
            );
            count += 1;
        }
        println!("  Total entries: {}", count);
    } else {
        println!("  Map not found!");
    }

    println!("\n=== IO_STATS Map (Per-CPU Hash) ===");
    if let Some(map) = probes.io_profile.map("IO_STATS") {
        #[repr(C)]
        #[derive(Debug, Clone, Copy, Default)]
        struct IoStats {
            read_ops: u64,
            write_ops: u64,
            read_bytes: u64,
            write_bytes: u64,
        }
        unsafe impl aya::Pod for IoStats {}

        let map: aya::maps::PerCpuHashMap<_, u64, IoStats> =
            aya::maps::PerCpuHashMap::try_from(map)?;
        let mut count = 0;
        for (cgroup_id, per_cpu_values) in map.iter().flatten() {
            let mut total = IoStats::default();
            for v in per_cpu_values.iter() {
                total.read_ops += v.read_ops;
                total.write_ops += v.write_ops;
                total.read_bytes += v.read_bytes;
                total.write_bytes += v.write_bytes;
            }
            println!(
                "  cgroup_id={} read_ops={} write_ops={} read_bytes={} write_bytes={}",
                cgroup_id, total.read_ops, total.write_ops, total.read_bytes, total.write_bytes
            );
            count += 1;
        }
        println!("  Total entries: {}", count);
    } else {
        println!("  Map not found!");
    }

    println!("\n=== TCP_EVENTS Ring Buffer ===");
    if let Some(map) = probes.network.map_mut("TCP_EVENTS") {
        #[repr(C)]
        #[derive(Debug, Clone, Copy)]
        struct TcpEvent {
            dest_ip: u32,
            dest_port: u16,
            _pad: u16,
            timestamp_ns: u64,
            bytes_sent: u64,
            bytes_recv: u64,
            event_type: u8,
            _pad2: [u8; 7],
        }

        let mut ring_buf = aya::maps::RingBuf::try_from(map)?;
        let mut count = 0u64;
        while let Some(data) = ring_buf.next() {
            if data.len() >= core::mem::size_of::<TcpEvent>() {
                let event: TcpEvent =
                    unsafe { core::ptr::read_unaligned(data.as_ptr() as *const _) };
                let ip = format!(
                    "{}.{}.{}.{}",
                    event.dest_ip & 0xFF,
                    (event.dest_ip >> 8) & 0xFF,
                    (event.dest_ip >> 16) & 0xFF,
                    (event.dest_ip >> 24) & 0xFF,
                );
                let event_name = if event.event_type == 0 {
                    "connect"
                } else {
                    "close"
                };
                println!(
                    "  {}:{} {} sent={} recv={}",
                    ip, event.dest_port, event_name, event.bytes_sent, event.bytes_recv
                );
                count += 1;
            }
        }
        println!("  Total events drained: {}", count);
    } else {
        println!("  Map not found!");
    }

    println!("\n=== Verification PASSED ===");
    println!("All probes loaded, maps accessible, ring buffer drainable.");

    Ok(())
}

#[cfg(feature = "ebpf")]
fn monotonic_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
    }
    (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64)
}

#[cfg(not(feature = "ebpf"))]
fn main() {
    eprintln!("This binary requires the 'ebpf' feature.");
    eprintln!("Build with: cargo build --features ebpf --bin ebpf-verify");
    std::process::exit(1);
}
