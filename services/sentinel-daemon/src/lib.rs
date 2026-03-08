//! Sentinel Daemon — ECS Orchestrator Binary.
//!
//! Composition Root das alle Library-Crates zu einem laufenden
//! Daemon zusammenfuegt. Dedicated `std::thread` fuer ECS Tick Loop,
//! `tokio::Runtime` fuer async I/O (Zenoh, Limbo).

pub mod adaptive_tick;
pub mod config;
pub mod controlplane;
pub mod ebpf;
pub mod episode_producer;
pub mod fanout;
pub mod llm_bridge;
#[cfg(feature = "nats")]
pub mod nats_consumer;
pub mod orchestrator;
pub mod query_responder;
pub mod shift;
pub mod signal;
