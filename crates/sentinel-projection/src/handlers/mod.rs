//! Projection Handler: Dispatching von DomainEvents auf Read Models.
//!
//! Jeder Handler verarbeitet Events fuer eine spezifische View.
//! Der Worker deserialisiert das Payload einmal und reicht es durch.

pub mod agent_live_view;
pub mod cost;
pub mod kpi;
pub mod room_live_view;
pub mod task_kanban_view;

use sentinel_common::{DomainEvent, DomainEventPayload};

use crate::store::ReadModelTransaction;

/// Trait fuer Projection Handler.
///
/// Implementierungen verarbeiten ein bereits deserialisiertes Event
/// innerhalb einer bestehenden Transaktion. Idempotenz wird ueber
/// `last_event_id` in den jeweiligen Tabellen sichergestellt.
pub trait ProjectionHandler {
    /// Verarbeitet ein Event. Gibt `Ok(())` zurueck wenn das Event
    /// nicht relevant ist oder erfolgreich verarbeitet wurde.
    fn handle(
        &self,
        row_id: i64,
        event: &DomainEvent,
        payload: &DomainEventPayload,
        txn: &ReadModelTransaction<'_>,
    ) -> anyhow::Result<()>;
}
