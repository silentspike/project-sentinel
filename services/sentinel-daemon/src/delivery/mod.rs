//! Independent QA, release, delivery, and product-lineage core (#696).
//!
//! Productive workflow/workbench/event adapters are deliberately absent until the
//! #695/#694 dependencies are merged and explicitly wired. The versioned core can
//! be built and tested with deterministic ports without changing daemon startup.

mod digest;
mod error;
mod ports;
mod schema;
mod service;
mod state;
mod store;

pub use digest::ContentDigest;
pub use error::DeliveryError;
pub use ports::*;
pub use schema::*;
pub use service::*;
pub use state::*;
pub use store::*;
