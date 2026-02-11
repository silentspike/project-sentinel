//! eBPF monitoring probes using aya-rs.
//!
//! Provides near-zero-overhead kernel-level monitoring for:
//! - Agent health (stalled process detection via write() syscall tracking)
//! - I/O profiling (IOPS per cgroup at block layer)
//! - Network monitoring (LLM API latency via TCP state tracking)
//! - PSI (Pressure Stall Information) for Bio-Engine stress input
//!
//! Real eBPF probe loading requires the `ebpf` feature and `CAP_BPF` capability.
//! Without the feature, only userspace logic (PSI reader, metric export) is available.

pub mod exporter;
pub mod probes;
pub mod psi;

pub use exporter::MetricsExporter;
pub use probes::{AgentHealthChecker, IoMetrics, IoProfiler, NetworkMetrics, NetworkMonitor};
pub use psi::PsiReader;
