//! Runtime Config-Apply Diff-Logik (#425).
//!
//! Pure + unit-testbar: berechnet das Delta zwischen der aktuell angewandten Firmen-Config
//! und einer neuen Config (per `identity.id`) und fuehrt das ECS-Live-Update fuer geaenderte
//! Agents aus. Der VOLLE Spawn/Despawn (Runtime- + Sandbox-Handles) und der Fresh-Load-Reset
//! passieren im Orchestrator, der den `AgentDiff` konsumiert (Handles leben dort).

use bevy_ecs::prelude::{Entity, World};
use sentinel_common::agent_config::{AgentConfig, AgentConfigValidation};
use sentinel_common::components::AgentIdentity;
use sentinel_common::room::BuildingConfig;
use sentinel_common::{AgentId, OperatorConfigApplyCommand};
use std::collections::HashSet;

/// Delta zwischen aktuell angewandter und neuer Agent-Config (per `identity.id`).
#[derive(Debug, Default, PartialEq)]
pub struct AgentDiff {
    /// Neu (in `new`, nicht in `old`) → voller Spawn (Runtime+Sandbox+ECS).
    pub spawn: Vec<AgentConfig>,
    /// Geaendert (in beiden, Config differ) → Live-Update OHNE Despawn.
    pub update: Vec<AgentConfig>,
    /// Entfernt (in `old`, nicht in `new`) → Despawn + Konsolidierung.
    pub despawn: Vec<AgentId>,
}

impl AgentDiff {
    pub fn is_empty(&self) -> bool {
        self.spawn.is_empty() && self.update.is_empty() && self.despawn.is_empty()
    }
}

/// Berechnet den Agent-Diff zwischen alter (aktuell angewandter) und neuer Config.
///
/// **Unveraenderte Agents** (Config identisch via `PartialEq`) erscheinen NICHT im Diff →
/// ihre laufende, evolvierte Personality/ihr Zustand bleibt unangetastet (TOGAF §6 L2:
/// Evolution nicht ueberschreiben). Nur explizit geaenderte Agents werden live aktualisiert.
pub fn compute_agent_diff(old: &[AgentConfig], new: &[AgentConfig]) -> AgentDiff {
    let mut diff = AgentDiff::default();
    for n in new {
        match old.iter().find(|o| o.identity.id == n.identity.id) {
            None => diff.spawn.push(n.clone()),
            Some(o) if *o != *n => diff.update.push(n.clone()),
            Some(_) => {} // unveraendert → nicht anfassen
        }
    }
    for o in old {
        if !new.iter().any(|n| n.identity.id == o.identity.id) {
            diff.despawn.push(AgentId(o.identity.id));
        }
    }
    diff
}

/// True, wenn sich die Building-/Raum-Config geaendert hat (per `PartialEq`).
pub fn building_changed(old: &BuildingConfig, new: &BuildingConfig) -> bool {
    old != new
}

/// Validiert eine Apply-Anfrage **vor jeder Mutation**. Sammelt ALLE Fehler (keine
/// Early-Exit-Ueberraschung). Gilt fuer beide Modi: nach einem Apply enthaelt die Welt genau
/// `cmd.agents` → die Agent-Anzahl muss `max_agents` (#414) respektieren; das Gebaeude muss alle
/// Agents fassen (Kapazitaet) und konsistente Adjacency haben; jede Personality in [0,1]; jede
/// AgentId in den Daemon-Grenzen; keine doppelten IDs.
pub fn validate_config_apply(
    cmd: &OperatorConfigApplyCommand,
    max_agents: usize,
    validation: AgentConfigValidation,
) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();

    if cmd.agents.len() > max_agents {
        errors.push(format!(
            "agent count {} exceeds daemon.max_agents {}",
            cmd.agents.len(),
            max_agents
        ));
    }

    // Gebaeude muss alle Agents fassen (min_capacity = Agent-Anzahl) + Adjacency/Duplikate.
    let min_capacity = u16::try_from(cmd.agents.len()).unwrap_or(u16::MAX);
    if let Err(room_errors) = cmd.building.validate(min_capacity) {
        errors.extend(room_errors);
    }

    let mut seen = HashSet::new();
    for agent in &cmd.agents {
        if !seen.insert(agent.identity.id) {
            errors.push(format!("duplicate agent id {}", agent.identity.id));
        }
        if let Err(e) = agent.personality.validate() {
            errors.push(format!(
                "agent {} personality invalid: {e}",
                agent.identity.id
            ));
        }
        if let Err(e) = AgentId::new_with_bounds(agent.identity.id, validation.agent_id_bounds) {
            errors.push(format!("agent {} id out of bounds: {e}", agent.identity.id));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Findet die ECS-Entity eines Agents anhand seiner `AgentId`.
pub fn find_agent_entity(world: &mut World, agent_id: AgentId) -> Option<Entity> {
    let mut query = world.query::<(Entity, &AgentIdentity)>();
    query
        .iter(world)
        .find(|(_, identity)| identity.agent_id == agent_id)
        .map(|(entity, _)| entity)
}

/// Live-Update der config-abgeleiteten ECS-Components eines laufenden Agents (kein Despawn).
///
/// Aendert NUR Identity (name/role), Personality und Capabilities. Bio/Position/Mood/Perception/
/// Memory bleiben unangetastet → der Agent behaelt Zustand + Evolution (#425 AC-1). Ein
/// expliziter User-Edit der Base-Personality ist legitim + live (TOGAF §6 L2, korrigiert PR #450).
/// Gibt `true` zurueck, wenn der Agent in der Welt gefunden wurde.
pub fn apply_agent_update(world: &mut World, cfg: &AgentConfig) -> bool {
    let agent_id = AgentId(cfg.identity.id);
    let Some(entity) = find_agent_entity(world, agent_id) else {
        return false;
    };
    sentinel_ecs::apply_identity(world, entity, &cfg.identity);
    sentinel_ecs::apply_personality(world, entity, &cfg.personality);
    sentinel_ecs::apply_capabilities(world, entity, &cfg.capabilities);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_common::agent_config::{
        AgentConfig, BackgroundConfig, CapabilitiesConfig, IdentityConfig, PersonalityConfig,
        PreferencesConfig, RuntimeSelectionConfig,
    };
    use sentinel_common::components::Personality;

    fn agent(id: u16, name: &str, role: &str, openness: f32) -> AgentConfig {
        AgentConfig {
            identity: IdentityConfig {
                id,
                name: name.to_string(),
                role: role.to_string(),
                department: "Dev".to_string(),
                shift_set: 1,
                kpis: vec![],
                reports_to: None,
                direct_reports: vec![],
            },
            personality: PersonalityConfig {
                openness,
                conscientiousness: 0.5,
                extraversion: 0.5,
                agreeableness: 0.5,
                neuroticism: 0.5,
                caffeine_tolerance: 0.5,
                morning_person: true,
            },
            preferences: PreferencesConfig {
                favorite_room: "empfang".to_string(),
                coffee_preference: "espresso".to_string(),
                lunch_time: "12:30".to_string(),
            },
            background: BackgroundConfig {
                bio: "bio".to_string(),
                quirks: vec![],
            },
            runtime: RuntimeSelectionConfig::default(),
            capabilities: CapabilitiesConfig::default(),
        }
    }

    #[test]
    fn diff_classifies_spawn_update_despawn_and_ignores_unchanged() {
        let old = vec![
            agent(1, "Anna", "Dev", 0.5),
            agent(2, "Bob", "PM", 0.5),
            agent(3, "Cara", "QA", 0.5),
        ];
        let new = vec![
            agent(1, "Anna", "Dev", 0.9), // geaendert (Personality)
            agent(2, "Bob", "PM", 0.5),   // unveraendert
            agent(4, "Dora", "Sales", 0.5), // neu
        ];
        let diff = compute_agent_diff(&old, &new);
        assert_eq!(diff.spawn.len(), 1);
        assert_eq!(diff.spawn[0].identity.id, 4);
        assert_eq!(diff.update.len(), 1);
        assert_eq!(diff.update[0].identity.id, 1);
        assert_eq!(diff.despawn, vec![AgentId(3)]);
        // Agent 2 unveraendert → in keinem Bucket (Evolution bleibt unangetastet).
    }

    #[test]
    fn diff_empty_when_identical() {
        let cfg = vec![agent(1, "Anna", "Dev", 0.5)];
        assert!(compute_agent_diff(&cfg, &cfg).is_empty());
    }

    #[test]
    fn apply_agent_update_live_updates_target_only() {
        let (mut world, _) = sentinel_ecs::create_simulation_world();
        let e1 = sentinel_ecs::spawn_agent(&mut world, AgentId(1), "Anna", "Dev", 1, "empfang");
        let e2 = sentinel_ecs::spawn_agent(&mut world, AgentId(2), "Bob", "PM", 1, "empfang");
        // Bekannte Personality setzen
        sentinel_ecs::apply_personality(&mut world, e1, &agent(1, "Anna", "Dev", 0.3).personality);
        sentinel_ecs::apply_personality(&mut world, e2, &agent(2, "Bob", "PM", 0.7).personality);

        // Agent 1 live aktualisieren (neue Rolle + Personality)
        let ok = apply_agent_update(&mut world, &agent(1, "Anna", "Lead Dev", 0.9));
        assert!(ok);

        let id1 = world.get::<AgentIdentity>(e1).unwrap();
        assert_eq!(id1.role, "Lead Dev");
        let p1 = world.get::<Personality>(e1).unwrap();
        assert_eq!(p1.openness, 0.9);
        // Agent 2 UNVERAENDERT (kein Reset unbeteiligter Agents, AC-1)
        let p2 = world.get::<Personality>(e2).unwrap();
        assert_eq!(p2.openness, 0.7);
    }

    #[test]
    fn apply_agent_update_returns_false_for_absent_agent() {
        let (mut world, _) = sentinel_ecs::create_simulation_world();
        assert!(!apply_agent_update(&mut world, &agent(99, "Ghost", "None", 0.5)));
    }

    use sentinel_common::room::{BuildingConfig, BuildingMeta, RoomConfig, RoomType};
    use sentinel_common::ApplyMode;

    fn building(capacity: u16) -> BuildingConfig {
        BuildingConfig {
            building: BuildingMeta {
                name: "Test".to_string(),
                address: "Teststr. 1".to_string(),
                floors: 1,
            },
            rooms: vec![RoomConfig {
                id: "empfang".to_string(),
                name: "Empfang".to_string(),
                floor: 0,
                capacity,
                room_type: RoomType::Common,
                adjacent: vec![],
                department: None,
                has_coffee_machine: false,
                has_printer: false,
            }],
        }
    }

    fn cmd(agents: Vec<AgentConfig>, capacity: u16) -> OperatorConfigApplyCommand {
        OperatorConfigApplyCommand {
            mode: ApplyMode::Live,
            agents,
            building: building(capacity),
        }
    }

    fn validation(max_id: u16) -> AgentConfigValidation {
        AgentConfigValidation::with_max_agent_id(max_id)
    }

    #[test]
    fn validate_accepts_valid_config() {
        let c = cmd(vec![agent(1, "Anna", "Dev", 0.5), agent(2, "Bob", "PM", 0.5)], 10);
        assert!(validate_config_apply(&c, 10, validation(60)).is_ok());
    }

    #[test]
    fn validate_rejects_too_many_agents() {
        let c = cmd(
            vec![agent(1, "A", "x", 0.5), agent(2, "B", "y", 0.5), agent(3, "C", "z", 0.5)],
            10,
        );
        let errs = validate_config_apply(&c, 2, validation(60)).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("exceeds daemon.max_agents")));
    }

    #[test]
    fn validate_rejects_out_of_range_personality() {
        let c = cmd(vec![agent(1, "Anna", "Dev", 1.5)], 10); // openness 1.5 > 1.0
        let errs = validate_config_apply(&c, 10, validation(60)).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("personality invalid")));
    }

    #[test]
    fn validate_rejects_duplicate_ids() {
        let c = cmd(vec![agent(1, "Anna", "Dev", 0.5), agent(1, "Clone", "Dev", 0.5)], 10);
        let errs = validate_config_apply(&c, 10, validation(60)).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("duplicate agent id 1")));
    }

    #[test]
    fn validate_rejects_insufficient_room_capacity() {
        // 3 Agents, Gebaeude fasst nur 1 → Kapazitaetsfehler.
        let c = cmd(
            vec![agent(1, "A", "x", 0.5), agent(2, "B", "y", 0.5), agent(3, "C", "z", 0.5)],
            1,
        );
        let errs = validate_config_apply(&c, 60, validation(60)).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("capacity")));
    }

    #[test]
    fn validate_rejects_agent_id_over_bound() {
        let c = cmd(vec![agent(99, "Over", "x", 0.5)], 10);
        let errs = validate_config_apply(&c, 10, validation(60)).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("id out of bounds")));
    }
}
