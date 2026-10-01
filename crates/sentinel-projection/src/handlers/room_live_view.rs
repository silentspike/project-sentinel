//! Handler fuer die `room_live_view` Projektion.
//!
//! Presence counters are derived from active agent rows after the agent handler.
//! Other room fields retain their event-watermark guards:
//! - ChaosTriggered -> active_chaos auf target_room
//! - RoomPhysicsUpdated -> temperature, co2_ppm, noise_db

use sentinel_common::{DomainEvent, DomainEventPayload};
use tracing::debug;

use crate::store::ReadModelTransaction;

use super::ProjectionHandler;

fn is_chaos_expired(chaos_json: &str, current_tick: u64) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(chaos_json) else {
        return true;
    };
    let created_tick = value
        .get("created_tick")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let duration_ticks = value
        .get("duration_ticks")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    current_tick >= created_tick.saturating_add(duration_ticks)
}

pub struct RoomLiveViewHandler;

impl ProjectionHandler for RoomLiveViewHandler {
    fn handle(
        &self,
        row_id: i64,
        event: &DomainEvent,
        payload: &DomainEventPayload,
        txn: &ReadModelTransaction<'_>,
    ) -> anyhow::Result<()> {
        match payload {
            DomainEventPayload::AgentSpawned { .. }
            | DomainEventPayload::AgentDespawned { .. }
            | DomainEventPayload::AgentStatusChanged { .. }
            | DomainEventPayload::TransitStarted { .. }
            | DomainEventPayload::TransitCompleted { .. }
            | DomainEventPayload::ShiftTransitionCompleted { .. }
            | DomainEventPayload::BioStateUpdated { .. } => {
                txn.reconcile_room_presence()?;
            }

            DomainEventPayload::ChaosTriggered {
                event_type,
                target_room: Some(room),
                description,
                ..
            } => {
                let chaos_json = serde_json::json!({
                    "type": format!("{:?}", event_type),
                    "event_type": format!("{:?}", event_type),
                    "description": description,
                    "created_tick": event.tick,
                    "duration_ticks": sentinel_physics::default_chaos_duration_ticks(*event_type),
                })
                .to_string();
                debug!(room, "Projecting chaos_triggered (room)");
                txn.update_room_chaos(room, &chaos_json, event.tick, row_id)?;
            }

            DomainEventPayload::RoomPhysicsUpdated {
                room_id,
                temperature,
                co2_ppm,
                noise_db,
                ..
            } => {
                debug!(room = room_id, "Projecting room_physics_updated");
                let clear_active_chaos = txn
                    .get_room_active_chaos(room_id)?
                    .as_deref()
                    .map(|json| is_chaos_expired(json, event.tick))
                    .unwrap_or(false);
                txn.update_room_physics(
                    room_id,
                    *temperature as f64,
                    *co2_ppm as f64,
                    *noise_db as f64,
                    clear_active_chaos,
                    event.tick,
                    row_id,
                )?;
            }

            DomainEventPayload::SmellEventTriggered {
                room_id,
                smell_type,
                intensity,
                duration_ticks,
            } => {
                let smell_json = serde_json::json!({
                    "smell_type": smell_type,
                    "intensity": intensity,
                    "duration_ticks": duration_ticks,
                    "tick": event.tick,
                })
                .to_string();
                debug!(
                    room = room_id,
                    smell_type, "Projecting smell_event_triggered (room)"
                );
                txn.update_room_smells(room_id, &smell_json, event.tick, row_id)?;
            }

            // Andere Events sind nicht relevant fuer room_live_view
            _ => {}
        }
        Ok(())
    }
}
