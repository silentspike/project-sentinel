//! eBPF monitoring probes using aya-rs.
//!
//! Provides near-zero-overhead kernel-level monitoring for:
//! - Agent health (stalled process detection via write() syscall tracking)
//! - I/O profiling (IOPS per cgroup at block layer)
//! - Network monitoring (LLM API latency via TCP state tracking)
//! - PSI (Pressure Stall Information) for Bio-Engine stress input
//!
//! Real eBPF probe loading requires the `ebpf` feature and `CAP_BPF` capability.
//! Without the feature, only userspace fallback monitoring is available.
//!
//! ## Monitoring Modes
//!
//! - **Kernel mode** (`ebpf` feature + `CAP_BPF`): fentry probes, Per-CPU Hash Maps,
//!   Ring Buffer. Near-zero overhead (~50ns per probe hit).
//! - **Userspace mode** (fallback): /proc/{pid}/io polling, cgroup io.stat,
//!   Cortex Gateway metrics scraping. Higher overhead (~10ms per cycle).
//!
//! Mode is determined at startup and logged (never silent degradation).
//! CI verifies compilation with `--features ebpf` on every PR.

pub mod collector;
pub mod exporter;
pub mod loader;
pub mod probes;
pub mod psi;

pub use collector::{AgentCgroupMapping, EbpfCollector, MetricsSnapshot};
pub use exporter::MetricsExporter;
#[cfg(feature = "ebpf")]
pub use loader::LoadedProbes;
pub use loader::{CapabilityReport, InitResult, MonitoringMode};
pub use probes::{AgentHealthChecker, IoMetrics, IoProfiler, NetworkMetrics, NetworkMonitor};
pub use psi::PsiReader;
