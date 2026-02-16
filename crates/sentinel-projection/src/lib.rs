//! CQRS-lite Projection Worker fuer Project Sentinel (Issue #53).
//!
//! Konsumiert append-only Events aus dem EventStore (sentinel-limbo)
//! und pflegt drei materialisierte Read Models:
//! - `agent_live_view`: Aktueller Zustand jedes Agenten
//! - `room_live_view`: Aktuelle Belegung und Chaos-Events pro Raum
//! - `kpi_1m`: Minutenbasierte operative KPIs
//!
//! Restart-safe via `projection_offsets` Bookmark im EventStore.
//! Unterstuetzt Full-Rebuild aus dem Event-Log.

pub mod config;
pub mod handlers;
pub mod store;
pub mod worker;

pub use config::ProjectionConfig;
pub use store::ReadModelStore;
pub use worker::ProjectionWorker;
