//! Sentinel Daemon — ECS Orchestrator Binary.
//!
//! Composition Root das alle Library-Crates zu einem laufenden
//! Daemon zusammenfuegt. Dedicated `std::thread` fuer ECS Tick Loop,
//! `tokio::Runtime` fuer async I/O (Zenoh, Limbo).

pub mod config;
pub mod controlplane;
pub mod llm_bridge;
pub mod orchestrator;
pub mod shift;
pub mod signal;
