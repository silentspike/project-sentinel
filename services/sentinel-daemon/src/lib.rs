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
pub mod operator_api;
pub mod orchestrator;
pub mod platform_controlplane;
pub mod query_responder;
pub mod resource_manager;
pub mod runtime_control;
pub mod runtime_health;
pub mod service_health;
pub mod shift;
pub mod signal;
pub mod snapshot;
