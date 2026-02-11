//! ECS Components - Re-Export aus sentinel-common.
//!
//! Die Component-Definitionen liegen in sentinel-common::components,
//! damit sentinel-bio und sentinel-physics sie ohne zirkulaere
//! Abhaengigkeit nutzen koennen.

pub use sentinel_common::components::*;
