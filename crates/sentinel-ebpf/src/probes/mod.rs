//! eBPF probe definitions for agent monitoring.
//!
//! Each probe module defines userspace data structures and logic.
//! Actual eBPF kernel programs require the `ebpf` feature gate.

pub mod agent_health;
pub mod io_profile;
pub mod network;

pub use agent_health::AgentHealthChecker;
pub use io_profile::{IoMetrics, IoProfiler};
pub use network::{NetworkMetrics, NetworkMonitor};
