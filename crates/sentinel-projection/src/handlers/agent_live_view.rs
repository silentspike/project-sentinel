//! Handler fuer die `agent_live_view` Projektion.
//!
//! Verarbeitet 8 Event-Varianten:
//! - AgentSpawned -> INSERT agent
//! - AgentDespawned -> status=despawned
//! - TransitStarted -> in_transit=1, rooms
//! - TransitCompleted -> in_transit=0, room
//! - AgentActionReceived -> last_action
//! - AgentStatusChanged -> status update
//! - ShiftTransitionCompleted -> bulk despawn
//! - BioStateUpdated -> hunger, energy, stress, bladder, social_need, caffeine_mg, mood

use sentinel_common::{DomainEvent, DomainEventPayload};
use tracing::debug;

use crate::store::{BioUpdate, ReadModelTransaction};

use super::ProjectionHandler;

pub struct AgentLiveViewHandler;

impl ProjectionHandler for AgentLiveViewHandler {
    fn handle(
        &self,
        row_id: i64,
        event: &DomainEvent,
        payload: &DomainEventPayload,
        txn: &ReadModelTransaction<'_>,
    ) -> anyhow::Result<()> {
        match payload {
            DomainEventPayload::AgentSpawned {
                agent_id,
                name,
                role,
                shift_set,
                room_id,
            } => {
                debug!(
                    agent_id = agent_id.0,
                    name,
                    room = room_id,
                    "Projecting agent_spawned"
                );
                txn.upsert_agent(agent_id.0, name, role, *shift_set, "active", row_id)?;
                txn.update_agent_room(agent_id.0, room_id, row_id)?;
            }

            DomainEventPayload::AgentDespawned { agent_id, .. } => {
                debug!(agent_id = agent_id.0, "Projecting agent_despawned");
                txn.update_agent_status(agent_id.0, "despawned", row_id)?;
            }

            DomainEventPayload::TransitStarted {
                agent_id,
                from_room,
                to_room,
                ..
            } => {
                debug!(
                    agent_id = agent_id.0,
                    from = from_room,
                    to = to_room,
                    "Projecting transit_started"
                );
                txn.update_agent_transit_start(agent_id.0, from_room, to_room, row_id)?;
            }

            DomainEventPayload::TransitCompleted { agent_id, room_id } => {
                debug!(
                    agent_id = agent_id.0,
                    room = room_id,
                    "Projecting transit_completed"
                );
                txn.update_agent_transit_complete(agent_id.0, room_id, row_id)?;
            }

            DomainEventPayload::AgentActionReceived {
                agent_id,
                action_type,
                content,
                ..
            } => {
                // Show natural-language content text, fall back to action_type
                let action_text = match content {
                    Some(c) if !c.is_empty() => c.as_str(),
                    _ => action_type.as_str(),
                };
                debug!(
                    agent_id = agent_id.0,
                    action_text = action_text,
                    "Projecting agent_action"
                );
                txn.update_agent_last_action(agent_id.0, action_text, event.tick, row_id)?;
            }

            DomainEventPayload::AgentStatusChanged {
                agent_id,
                new_status,
                ..
            } => {
                debug!(
                    agent_id = agent_id.0,
                    status = new_status,
                    "Projecting status_changed"
                );
                txn.update_agent_status(agent_id.0, new_status, row_id)?;
            }

            DomainEventPayload::ShiftTransitionCompleted { removed_agents, .. } => {
                debug!(
                    count = removed_agents.len(),
                    "Projecting shift_transition (agent despawn)"
                );
                for agent_id in removed_agents {
                    txn.update_agent_status(agent_id.0, "despawned", row_id)?;
                }
            }

            DomainEventPayload::BioStateUpdated {
                agent_id,
                hunger,
                energy,
                stress,
                bladder,
                social_need,
                caffeine_mg,
                room_id,
                mood,
                valence: _,
                arousal: _,
            } => {
                debug!(
                    agent_id = agent_id.0,
                    hunger, energy, stress, "Projecting bio_state_updated"
                );
                txn.update_agent_bio(
                    &BioUpdate {
                        agent_id: agent_id.0,
                        hunger: f64::from(*hunger),
                        energy: f64::from(*energy),
                        stress: f64::from(*stress),
                        bladder: f64::from(*bladder),
                        social_need: f64::from(*social_need),
                        caffeine_mg: f64::from(*caffeine_mg),
                        mood,
                        room_id,
                    },
                    row_id,
                )?;
            }

            // Andere Events sind nicht relevant fuer agent_live_view
            _ => {}
        }
        Ok(())
    }
}
