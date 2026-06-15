//! #25 exporter-format benchmark: measures `MetricsExporter::export_snapshot` formatting time and
//! the label-value escaping overhead.
//!
//! Standalone (`harness = false`, own `fn main`): pure in-memory, no daemon, safe to run on the
//! deploy VM next to the production daemon (no second daemon, Lesson #529).
//!
//! ```text
//! Build (remote): cargo remote -c -- build -p sentinel-ebpf --release --bench exporter_format
//! Run (deploy VM): scp target/release/deps/exporter_format-* ubuntu@10.0.0.240:/tmp/ && ./exporter_format
//!                  with sidecars: vmstat 1 / mpstat 1 / iostat -x 1
//! ```
//!
//! Honest scope: `export_snapshot` runs once per `:9090` scrape (infrequent), so absolute latency is
//! uncritical. The benchmark proves the escaping adds negligible overhead (the `Cow` fast path makes
//! clean names near-zero) and which escape variant wins for the real (clean-dominant) workload.

use std::borrow::Cow;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use sentinel_ebpf::collector::{
    IoSnapshot, MetricsSnapshot, NetworkSnapshot, PsiSnapshot, StalledAgent,
};
use sentinel_ebpf::exporter::MetricsExporter;
use sentinel_ebpf::loader::MonitoringMode;

/// Builds an N-row snapshot. `special` toggles between clean names (Borrowed fast path) and names
/// with inner quotes (Owned escape path), mirroring real roster names like `Tobias "Tobi" Lehmann`.
fn build_snapshot(n: usize, special: bool) -> MetricsSnapshot {
    let name = |i: usize| -> String {
        if special {
            format!("Agent \"Nick{i}\" Lastname")
        } else {
            format!("AGENT-{i:04}")
        }
    };
    let mut io_metrics = HashMap::new();
    let mut network_metrics = HashMap::new();
    let mut psi_metrics = HashMap::new();
    let stalled_agents = (0..n)
        .map(|i| StalledAgent {
            cgroup_id: i as u64,
            agent_name: name(i),
            seconds_since_write: 65,
        })
        .collect();
    for i in 0..n {
        io_metrics.insert(
            i as u64,
            IoSnapshot {
                cgroup_name: name(i),
                read_ops: 100,
                write_ops: 50,
                read_bytes: 409_600,
                write_bytes: 204_800,
            },
        );
        let dest = if special {
            format!("host\"{i}\":443")
        } else {
            format!("api{i}.example.com:443")
        };
        network_metrics.insert(
            dest.clone(),
            NetworkSnapshot {
                destination: dest,
                request_count: 10,
                avg_latency_us: 150_000,
                bytes_sent: 10_240,
                bytes_received: 40_960,
                error_count: 1,
            },
        );
        psi_metrics.insert(
            name(i),
            PsiSnapshot {
                cpu_avg10: 25.0,
                memory_avg10: 10.0,
                io_avg10: 5.0,
                combined_stress: 0.175,
            },
        );
    }
    MetricsSnapshot {
        stalled_agents,
        io_metrics,
        network_metrics,
        psi_metrics,
        cycle_duration: Duration::from_micros(500),
        mode: MonitoringMode::Userspace,
        ring_buffer_drops: 0,
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[(((sorted.len() - 1) as f64) * p).round() as usize]
}

fn bench_export(n: usize, special: bool, iters: usize) {
    let snapshot = build_snapshot(n, special);
    let mut size = 0;
    let mut durations = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t = Instant::now();
        let out = MetricsExporter::export_snapshot(&snapshot);
        durations.push(t.elapsed().as_secs_f64() * 1_000_000.0); // microseconds
        size = out.len();
        std::hint::black_box(&out);
    }
    durations.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let profile = if special {
        "special-char"
    } else {
        "clean      "
    };
    println!(
        "export_snapshot N={n:5} {profile}  p50={:8.2}us  p95={:8.2}us  | out={size} bytes",
        percentile(&durations, 0.50),
        percentile(&durations, 0.95),
    );
}

// ── Settings tuner: chosen Cow fast-path vs an always-allocate variant ──────────────────────────

fn escape_cow(value: &str) -> Cow<'_, str> {
    if !value
        .bytes()
        .any(|b| matches!(b, b'\\' | b'"' | b'\n' | b'\r'))
    {
        return Cow::Borrowed(value);
    }
    let mut out = String::with_capacity(value.len() + 8);
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            _ => out.push(c),
        }
    }
    Cow::Owned(out)
}

fn escape_always_alloc(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 8);
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            _ => out.push(c),
        }
    }
    out
}

fn bench_escape_variants() {
    // Real workload is clean-dominant: most agent/cgroup/destination names have no special chars.
    let mut labels: Vec<String> = (0..1000).map(|i| format!("AGENT-{i:04}")).collect();
    for i in 0..50 {
        labels.push(format!("Agent \"Nick{i}\" Lastname"));
    }
    let runs = 2000usize;

    let t = Instant::now();
    let mut sink = 0usize;
    for _ in 0..runs {
        for l in &labels {
            sink += escape_cow(l).len();
        }
    }
    let cow_ns = t.elapsed().as_nanos() as f64 / (runs * labels.len()) as f64;
    std::hint::black_box(sink);

    let t = Instant::now();
    let mut sink = 0usize;
    for _ in 0..runs {
        for l in &labels {
            sink += escape_always_alloc(l).len();
        }
    }
    let alloc_ns = t.elapsed().as_nanos() as f64 / (runs * labels.len()) as f64;
    std::hint::black_box(sink);

    println!(
        "escape variant (1050 labels, 95% clean): Cow-fast-path={cow_ns:.1}ns/label  always-alloc={alloc_ns:.1}ns/label  -> best={}",
        if cow_ns <= alloc_ns { "Cow-fast-path" } else { "always-alloc" }
    );
}

fn main() {
    println!("=== #25 eBPF exporter-format benchmark (MetricsExporter::export_snapshot) ===\n");
    for &n in &[26usize, 100, 1000] {
        let iters = if n >= 1000 { 300 } else { 1000 };
        bench_export(n, false, iters);
        bench_export(n, true, iters);
    }
    println!();
    bench_escape_variants();
}
