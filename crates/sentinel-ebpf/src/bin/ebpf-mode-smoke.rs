//! Short runtime smoke for kernel mode and userspace fallback.
//!
//! This intentionally exits successfully in both modes. It uses the same loader
//! and collector path as the daemon, then prints per-sample collection timing.

#[cfg(feature = "ebpf")]
fn main() -> anyhow::Result<()> {
    use std::thread;
    use std::time::Duration;

    use sentinel_ebpf::{loader, EbpfCollector};

    tracing_subscriber::fmt::init();

    let samples = std::env::args()
        .nth(1)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(5)
        .max(1);
    let interval_ms = std::env::args()
        .nth(2)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1000);

    let init = loader::init();
    let mode = init.mode;
    let mut collector = match init.probes {
        Some(probes) => EbpfCollector::with_probes(mode, probes),
        None => EbpfCollector::new(mode),
    };

    println!("mode={mode}");
    for sample in 1..=samples {
        let snapshot = collector.collect()?;
        println!(
            "sample={} mode={} cycle_us={} drops={} io_entries={} network_entries={} psi_entries={}",
            sample,
            snapshot.mode,
            snapshot.cycle_duration.as_micros(),
            snapshot.ring_buffer_drops,
            snapshot.io_metrics.len(),
            snapshot.network_metrics.len(),
            snapshot.psi_metrics.len()
        );
        if sample < samples {
            thread::sleep(Duration::from_millis(interval_ms));
        }
    }

    Ok(())
}

#[cfg(not(feature = "ebpf"))]
fn main() {
    eprintln!("This binary requires the 'ebpf' feature.");
    eprintln!("Build with: cargo build --features ebpf --bin ebpf-mode-smoke");
    std::process::exit(1);
}
