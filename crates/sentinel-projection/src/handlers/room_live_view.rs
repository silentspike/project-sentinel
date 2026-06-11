//! Handler fuer die `room_live_view` Projektion.
//!
//! Verarbeitet 7 Event-Varianten:
//! - AgentSpawned -> room_id occupancy++
//! - TransitStarted -> from_room occupancy--, transit_count++
//! - TransitCompleted -> to_room occupancy++, transit_count--
//! - ChaosTriggered -> active_chaos auf target_room
//! - RoomPhysicsUpdated -> temperature, co2_ppm, noise_db
//! - AgentDespawned -> current_room occupancy--
//! - ShiftTransitionCompleted -> pro removed_agent: current_room occupancy--

use sentinel_common::{DomainEvent, DomainEventPayload};
use tracing::{debug, warn};

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
            DomainEventPayload::AgentSpawned {
                agent_id, room_id, ..
            } => {
                // Only increment occupancy if agent was not already active.
                // Without this guard, daemon restarts (re-spawn without prior despawn)
                // cause occupant_count to drift upward monotonically.
                let was_active = txn
                    .get_agent_status(agent_id.0)?
                    .map(|s| s != "despawned")
                    .unwrap_or(false);
                if was_active {
                    // Agent already counted — update room but don't increment
                    if let Some(old_room) = txn.get_agent_room(agent_id.0)? {
                        if old_room != *room_id {
                            txn.update_room_occupancy(&old_room, -1, event.tick, row_id)?;
                            txn.update_room_occupancy(room_id, 1, event.tick, row_id)?;
                        }
                    }
                    debug!(
                        agent_id = agent_id.0,
                        room = room_id,
                        "Re-spawn: agent already active, no occupancy increment"
                    );
                } else {
                    debug!(room = room_id, "Projecting agent_spawned (room occupancy)");
                    txn.update_room_occupancy(room_id, 1, event.tick, row_id)?;
                }
            }

            DomainEventPayload::TransitStarted {
                from_room, to_room, ..
            } => {
                debug!(
                    from = from_room,
                    to = to_room,
                    "Projecting transit_started (room)"
                );
                txn.update_room_occupancy(from_room, -1, event.tick, row_id)?;
                txn.update_room_transit(to_room, 1, row_id)?;
            }

            DomainEventPayload::TransitCompleted { room_id, .. } => {
                debug!(room = room_id, "Projecting transit_completed (room)");
                txn.update_room_occupancy(room_id, 1, event.tick, row_id)?;
                txn.update_room_transit(room_id, -1, row_id)?;
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

            DomainEventPayload::AgentDespawned { agent_id, .. } => {
                // Agent despawned: Raum-Belegung anpassen
                if let Some(room) = txn.get_agent_room(agent_id.0)? {
                    debug!(
                        agent_id = agent_id.0,
                        room, "Projecting agent_despawned (room occupancy)"
                    );
                    txn.update_room_occupancy(&room, -1, event.tick, row_id)?;
                }
            }

            DomainEventPayload::ShiftTransitionCompleted { removed_agents, .. } => {
                // Gruppiere Decrements pro Raum um den Idempotenz-Guard
                // nicht auszuhebeln (gleiche row_id, gleicher Raum).
                debug!(
                    count = removed_agents.len(),
                    "Projecting shift_transition (room occupancy)"
                );
                let mut room_decrements: std::collections::HashMap<String, i64> =
                    std::collections::HashMap::new();
                for agent_id in removed_agents {
                    match txn.get_agent_room(agent_id.0)? {
                        Some(room) => {
                            *room_decrements.entry(room).or_insert(0) -= 1;
                        }
                        None => {
                            warn!(
                                agent_id = agent_id.0,
                                "Agent has no current_room during shift transition"
                            );
                        }
                    }
                }
                for (room, delta) in &room_decrements {
                    txn.update_room_occupancy(room, *delta, event.tick, row_id)?;
                }
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
