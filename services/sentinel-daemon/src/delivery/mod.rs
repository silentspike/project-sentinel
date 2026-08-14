//! Independent QA, release, delivery, and product-lineage core (#696).
//!
//! The local aggregate, idempotency, journal, and outbox have a configured redb
//! composition. Productive workflow/workbench/effect/publication authorities are
//! explicit fail-closed ports until their versioned adapters are wired.

mod digest;
mod error;
mod lineage;
mod ports;
mod schema;
mod service;
mod state;
mod store;

pub use digest::ContentDigest;
pub use error::DeliveryError;
pub use lineage::*;
pub use ports::*;
pub use schema::*;
pub use service::*;
pub use state::*;
pub use store::*;
