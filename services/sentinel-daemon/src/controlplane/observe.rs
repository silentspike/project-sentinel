//! Observe-Phase: Sammelt Systemzustand aus der ECS World.
//!
//! Liest BioState, Position, Mood direkt aus bevy_ecs Components.
//! Erkennt Schwellenwert-Verletzungen als Incidents.

use bevy_ecs::prelude::*;
use tracing::debug;

use sentinel_common::components::{AgentIdentity, BioState, Mood, Position};

use super::config::ControlplaneConfig;
use super::types::{AgentObservation, Incident, IncidentType, Observation, Severity};

/// Sammelt eine Observation aus dem aktuellen ECS World-Zustand.
pub fn observe(world: &mut World, tick: u64, timestamp_ms: u64) -> Observation {
    let mut agents = Vec::new();

    // Query alle Agents mit Bio/Position/Mood Components
    let mut query = world.query::<(&AgentIdentity, &BioState, &Position, &Mood)>();
    for (identity, bio, position, mood) in query.iter(world) {
        agents.push(AgentObservation {
            agent_id: identity.agent_id.0,
            hunger: bio.hunger,
            energy: bio.energy,
            stress: bio.stress,
            bladder: bio.bladder,
            social_need: bio.social_need,
            caffeine: bio.caffeine_mg,
            room_id: position.room_id.clone(),
            in_transit: position.in_transit,
            valence: mood.valence,
            arousal: mood.arousal,
        });
    }

    debug!(tick, agent_count = agents.len(), "Observation gesammelt");
    Observation {
        tick,
        timestamp_ms,
        agents,
    }
}

/// Erkennt Incidents aus einer Observation basierend auf Config-Schwellenwerten.
pub fn detect_incidents(observation: &Observation, config: &ControlplaneConfig) -> Vec<Incident> {
    let mut incidents = Vec::new();
    let tick = observation.tick;
    let ts = observation.timestamp_ms;

    for agent in &observation.agents {
        let aid = agent.agent_id;

        // Hunger kritisch
        if agent.hunger >= config.thresholds.hunger_critical {
            incidents.push(Incident {
                id: format!("inc-{tick}-hunger-{aid}"),
                tick,
                timestamp_ms: ts,
                incident_type: IncidentType::HungerCritical,
                severity: Severity::High,
                agent_id: Some(aid),
                description: format!(
                    "AGENT-{aid:02} hunger at {:.2} (threshold: {:.2})",
                    agent.hunger, config.thresholds.hunger_critical
                ),
            });
        }

        // Energy kritisch niedrig
        if agent.energy <= config.thresholds.energy_critical {
            incidents.push(Incident {
                id: format!("inc-{tick}-energy-{aid}"),
                tick,
                timestamp_ms: ts,
                incident_type: IncidentType::EnergyDepleted,
                severity: Severity::High,
                agent_id: Some(aid),
                description: format!(
                    "AGENT-{aid:02} energy at {:.2} (threshold: {:.2})",
                    agent.energy, config.thresholds.energy_critical
                ),
            });
        }

        // Stress kritisch
        if agent.stress >= config.thresholds.stress_critical {
            incidents.push(Incident {
                id: format!("inc-{tick}-stress-{aid}"),
                tick,
                timestamp_ms: ts,
                incident_type: IncidentType::StressCritical,
                severity: Severity::High,
                agent_id: Some(aid),
                description: format!(
                    "AGENT-{aid:02} stress at {:.2} (threshold: {:.2})",
                    agent.stress, config.thresholds.stress_critical
                ),
            });
        }

        // Bladder kritisch
        if agent.bladder >= config.thresholds.bladder_critical {
            incidents.push(Incident {
                id: format!("inc-{tick}-bladder-{aid}"),
                tick,
                timestamp_ms: ts,
                incident_type: IncidentType::BladderCritical,
                severity: Severity::Medium,
                agent_id: Some(aid),
                description: format!(
                    "AGENT-{aid:02} bladder at {:.2} (threshold: {:.2})",
                    agent.bladder, config.thresholds.bladder_critical
                ),
            });
        }
    }

    // Stress-Cluster: N+ Agents mit hohem Stress im selben Raum
    detect_stress_clusters(observation, config, &mut incidents);

    debug!(tick, incident_count = incidents.len(), "Incidents erkannt");
    incidents
}

/// Erkennt Stress-Cluster (mehrere gestresste Agents im selben Raum).
fn detect_stress_clusters(
    observation: &Observation,
    config: &ControlplaneConfig,
    incidents: &mut Vec<Incident>,
) {
    use std::collections::HashMap;

    let threshold = config.thresholds.stress_critical * 0.8; // Etwas unter kritisch
    let mut room_stress: HashMap<&str, Vec<u16>> = HashMap::new();

    for agent in &observation.agents {
        if agent.stress >= threshold && !agent.in_transit {
            room_stress
                .entry(&agent.room_id)
                .or_default()
                .push(agent.agent_id);
        }
    }

    for (room, agents) in &room_stress {
        if agents.len() >= 3 {
            incidents.push(Incident {
                id: format!("inc-{}-cluster-{room}", observation.tick),
                tick: observation.tick,
                timestamp_ms: observation.timestamp_ms,
                incident_type: IncidentType::HighStressCluster,
                severity: Severity::Critical,
                agent_id: None,
                description: format!(
                    "Stress-Cluster in {room}: {} Agents ueber Schwellenwert",
                    agents.len()
                ),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controlplane::config::{ControlplaneConfig, ThresholdConfig};

    fn test_config() -> ControlplaneConfig {
        ControlplaneConfig {
            cycle_interval_ticks: 10,
            guarded_mode: false,
            thresholds: ThresholdConfig {
                hunger_critical: 0.9,
                energy_critical: 0.15,
                stress_critical: 0.85,
                bladder_critical: 0.9,
            },
            default_ttl_ticks: 30,
            cooldown_ticks: 60,
        }
    }

    fn make_observation(agents: Vec<AgentObservation>) -> Observation {
        Observation {
            tick: 100,
            timestamp_ms: 100_000,
            agents,
        }
    }

    fn default_agent(id: u16) -> AgentObservation {
        AgentObservation {
            agent_id: id,
            hunger: 0.3,
            energy: 0.7,
            stress: 0.2,
            bladder: 0.2,
            social_need: 0.3,
            caffeine: 0.0,
            room_id: "buero-dev-1".into(),
            in_transit: false,
            valence: 0.5,
            arousal: 0.3,
        }
    }

    #[test]
    fn test_no_incidents_healthy_agents() {
        let obs = make_observation(vec![default_agent(1), default_agent(2)]);
        let incidents = detect_incidents(&obs, &test_config());
        assert!(incidents.is_empty());
    }

    #[test]
    fn test_hunger_critical() {
        let mut agent = default_agent(1);
        agent.hunger = 0.95;
        let obs = make_observation(vec![agent]);
        let incidents = detect_incidents(&obs, &test_config());
        assert_eq!(incidents.len(), 1);
        assert_eq!(incidents[0].incident_type, IncidentType::HungerCritical);
        assert_eq!(incidents[0].agent_id, Some(1));
    }

    #[test]
    fn test_energy_depleted() {
        let mut agent = default_agent(1);
        agent.energy = 0.10;
        let obs = make_observation(vec![agent]);
        let incidents = detect_incidents(&obs, &test_config());
        assert_eq!(incidents.len(), 1);
        assert_eq!(incidents[0].incident_type, IncidentType::EnergyDepleted);
    }

    #[test]
    fn test_stress_critical() {
        let mut agent = default_agent(1);
        agent.stress = 0.90;
        let obs = make_observation(vec![agent]);
        let incidents = detect_incidents(&obs, &test_config());
        assert_eq!(incidents.len(), 1);
        assert_eq!(incidents[0].incident_type, IncidentType::StressCritical);
    }

    #[test]
    fn test_multiple_incidents_same_agent() {
        let mut agent = default_agent(1);
        agent.hunger = 0.95;
        agent.stress = 0.90;
        let obs = make_observation(vec![agent]);
        let incidents = detect_incidents(&obs, &test_config());
        assert_eq!(incidents.len(), 2);
    }

    #[test]
    fn test_stress_cluster_detection() {
        let mut agents: Vec<_> = (1..=4)
            .map(|i| {
                let mut a = default_agent(i);
                a.stress = 0.80; // Ueber 0.85 * 0.8 = 0.68
                a.room_id = "konferenz-1".into();
                a
            })
            .collect();
        // Ein Agent in anderem Raum
        agents[3].room_id = "buero-dev-1".into();

        let obs = make_observation(agents);
        let incidents = detect_incidents(&obs, &test_config());
        let clusters: Vec<_> = incidents
            .iter()
            .filter(|i| i.incident_type == IncidentType::HighStressCluster)
            .collect();
        assert_eq!(clusters.len(), 1);
        assert!(clusters[0].description.contains("konferenz-1"));
    }
}
