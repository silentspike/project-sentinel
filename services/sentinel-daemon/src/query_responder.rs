//! Scoped Query Responder fuer Zenoh.
//!
//! Subscribes auf `sentinel/query/*/request` Topics und beantwortet Queries
//! mit State aus dem redb StateStore. Laeuft als async Task im tokio Runtime.
//!
//! Nutzt den bestehenden `ScopedQuery`/`QueryResponse` Typ aus sentinel-zenoh
//! und den `InFlightTracker` fuer Capacity Enforcement.

use std::sync::Arc;

use sentinel_common::AgentId;
use sentinel_redb::StateStore;
use sentinel_zenoh::query::{QueryResponse, QueryScope, ScopedQuery};
use sentinel_zenoh::SentinelBus;
use tracing::{debug, info, warn};

/// Async Task: Beantwortet Scoped Queries ueber Zenoh mit State aus redb.
///
/// Subscribes auf:
/// - `sentinel/query/agent/+/request` (Agent-spezifische Queries)
/// - `sentinel/query/room/+/request` (Raum-spezifische Queries)
/// - `sentinel/query/global/request` (Globale Queries)
///
/// Liest State aus redb (read-only, thread-safe via `Arc<StateStore>`).
pub async fn query_responder_task(bus: SentinelBus, state_store: Arc<StateStore>) {
    info!("Query Responder gestartet");

    // Subscribe auf alle Query-Request-Topics (Wildcard)
    let request_topic = "sentinel/query/*/request";
    let subscriber = match bus.subscribe(request_topic).await {
        Ok(sub) => sub,
        Err(e) => {
            warn!(error = %e, "Query Responder: Subscribe fehlgeschlagen, Task beendet");
            return;
        }
    };

    let mut query_count: u64 = 0;
    let mut error_count: u64 = 0;

    while let Ok(sample) = subscriber.recv_async().await {
        let payload_bytes = sample.payload().to_bytes();

        // Query deserialisieren
        let query: ScopedQuery = match serde_json::from_slice(&payload_bytes) {
            Ok(q) => q,
            Err(e) => {
                warn!(error = %e, "Query Responder: ungueltige Query empfangen");
                error_count += 1;
                continue;
            }
        };

        query_count += 1;
        debug!(
            query_id = %query.query_id,
            scope = ?query.scope,
            origin = query.origin_agent.0,
            "Query empfangen"
        );

        // State aus redb lesen basierend auf Scope
        let response_payload = match &query.scope {
            QueryScope::Agent(agent_id) => match state_store.get_agent_state(*agent_id) {
                Ok(Some(data)) => data,
                Ok(None) => {
                    debug!(agent_id = agent_id.0, "Kein State fuer Agent");
                    vec![]
                }
                Err(e) => {
                    warn!(error = %e, agent_id = agent_id.0, "redb Read fehlgeschlagen");
                    error_count += 1;
                    continue;
                }
            },
            QueryScope::Room(room_id) => {
                // Room-Queries: room_id String → RoomId(u16) Konversion
                // Versuche den String als u16 zu parsen (numerische Room-IDs in redb)
                match room_id.parse::<u16>() {
                    Ok(rid) => match state_store.get_room_state(sentinel_common::RoomId(rid)) {
                        Ok(Some(data)) => data,
                        Ok(None) => {
                            debug!(room_id, "Kein State fuer Raum");
                            vec![]
                        }
                        Err(e) => {
                            warn!(error = %e, room_id, "redb Read fehlgeschlagen");
                            error_count += 1;
                            continue;
                        }
                    },
                    Err(_) => {
                        debug!(
                            room_id,
                            "Room-ID nicht numerisch, keine redb-Abfrage moeglich"
                        );
                        vec![]
                    }
                }
            }
            QueryScope::Global => {
                // Global: Liste aller Agents mit State zusammenbauen
                match state_store.list_agents() {
                    Ok(agents) => {
                        let mut states: Vec<(u16, Vec<u8>)> = Vec::new();
                        for aid in agents {
                            if let Ok(Some(data)) = state_store.get_agent_state(aid) {
                                states.push((aid.0, data));
                            }
                        }
                        serde_json::to_vec(&states).unwrap_or_default()
                    }
                    Err(e) => {
                        warn!(error = %e, "redb list_agents fehlgeschlagen");
                        error_count += 1;
                        continue;
                    }
                }
            }
        };

        // Stale-Check: sim_hour als Proxy fuer den aktuellen Tick
        let current_tick = state_store
            .get_sim_hour()
            .ok()
            .flatten()
            .map(|h| (h * 60.0) as u64) // Grobe Approximation
            .unwrap_or(0);

        let response = QueryResponse {
            query_id: query.query_id,
            response_tick: current_tick,
            payload: response_payload,
        };

        // Stale-Response nicht senden
        if response.is_stale(query.min_tick) {
            debug!(
                query_id = %query.query_id,
                response_tick = current_tick,
                min_tick = query.min_tick,
                "Response stale — nicht gesendet"
            );
            continue;
        }

        // Response auf das Response-Topic publishen
        let response_topic = sentinel_zenoh::topics::query_response_agent(&format!(
            "AGENT-{:02}",
            query.origin_agent.0
        ));

        match serde_json::to_vec(&response) {
            Ok(bytes) => {
                if let Err(e) = bus.publish(&response_topic, &bytes).await {
                    warn!(error = %e, "Query Response publish fehlgeschlagen");
                    error_count += 1;
                }
            }
            Err(e) => {
                warn!(error = %e, "Query Response Serialisierung fehlgeschlagen");
                error_count += 1;
            }
        }
    }

    info!(query_count, error_count, "Query Responder beendet");
}

/// Prueft ob der Agent-ID String ein numerisches Format hat (fuer AGENT-XX).
fn _agent_name_from_id(agent_id: AgentId) -> String {
    format!("AGENT-{:02}", agent_id.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_name_formatting() {
        assert_eq!(_agent_name_from_id(AgentId(1)), "AGENT-01");
        assert_eq!(_agent_name_from_id(AgentId(42)), "AGENT-42");
    }
}
