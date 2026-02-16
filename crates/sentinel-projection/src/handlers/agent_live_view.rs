//! Handler fuer die `agent_live_view` Projektion.
//!
//! Verarbeitet 7 Event-Varianten:
//! - AgentSpawned -> INSERT agent
//! - AgentDespawned -> status=despawned
//! - TransitStarted -> in_transit=1, rooms
//! - TransitCompleted -> in_transit=0, room
//! - AgentActionReceived -> last_action
//! - AgentStatusChanged -> status update
//! - ShiftTransitionCompleted -> bulk despawn

use sentinel_common::{DomainEvent, DomainEventPayload};
use tracing::debug;

use crate::store::ReadModelTransaction;

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
            } => {
                debug!(agent_id = agent_id.0, name, "Projecting agent_spawned");
                txn.upsert_agent(agent_id.0, name, role, *shift_set, "active", row_id)?;
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
                ..
            } => {
                debug!(
                    agent_id = agent_id.0,
                    action = action_type,
                    "Projecting agent_action"
                );
                txn.update_agent_last_action(agent_id.0, action_type, event.tick, row_id)?;
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

            // Andere Events sind nicht relevant fuer agent_live_view
            _ => {}
        }
        Ok(())
    }
}
